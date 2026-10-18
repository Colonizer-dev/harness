#!/usr/bin/env node
// The Grafana dashboards provisioned under examples/observability/grafana/dashboards are loaded
// as written, and Grafana will happily serve a panel that queries a metric no build ever exported
// or a datasource uid nothing provisions: the dashboard looks fine until someone reads it. This
// lint keeps them honest against the catalogues the mothership actually serves, and CI runs it so
// the pull request that breaks a dashboard is the one that fails. Every dashboard JSON must parse,
// panel ids must be unique within a file and dashboard uids unique across files (a collision makes
// provisioning silently keep only one), every datasource uid must be declared under
// provisioning/datasources, every metric name in a PromQL query must be in a catalogue, and no
// string anywhere may hard-code a colony id: a dashboard checked into the repository is for every
// install, and one colony's id is meaningless on the next one.
//
//   node scripts/check-dashboards.mjs    check the provisioned dashboards
//
// The catalogues are docs/observability/metrics.md (the pull surface) and the OTLP table in
// docs/observability.md. The four inputs can be overridden positionally for testing:
// [dashboardsDir] [provisioningDir] [metricsDoc] [otlpDoc].
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULTS = {
  dashboardsDir: 'examples/observability/grafana/dashboards',
  provisioningDir: 'examples/observability/grafana/provisioning',
  metricsDoc: 'docs/observability/metrics.md',
  otlpDoc: 'docs/observability.md',
};

/**
 * The datasources a provisioning yml declares, as a `uid -> type` map: the ids Grafana will answer
 * to and what each one is. A `- ` item starts a new datasource, so a `type:` line belongs to the
 * `uid:` line it shares an item with, whichever comes first.
 */
export function provisionedDatasources(text) {
  const out = new Map();
  let type;
  let uid;
  const flush = () => {
    if (uid !== undefined) out.set(uid, type ?? '');
    type = undefined;
    uid = undefined;
  };
  for (const line of text.split('\n')) {
    if (/^\s*-\s/.test(line)) flush();
    const t = /^\s*type:\s*(\S+)\s*$/.exec(line);
    if (t) type = t[1];
    const u = /^\s*uid:\s*(\S+)\s*$/.exec(line);
    if (u) uid = u[1];
  }
  flush();
  return out;
}

/** The `uid:` values in a datasources yml: the ids Grafana will answer to. */
export function provisionedUids(text) {
  return new Set(provisionedDatasources(text).keys());
}

/**
 * The data rows of a doc's metric tables only: blocks of contiguous `|` lines whose header row's
 * first cell is exactly `Metric` (the `---` separator as the block's second line is what makes the
 * first line a header). The logs table's event names and the variables table's settings are not
 * metric catalogues, and their backticked names must not leak into the allowlist.
 */
function metricTableRows(text) {
  const rows = [];
  let block = [];
  const flush = () => {
    if (block.length >= 2) {
      const [header, separator] = block;
      if (header.split('|')[1]?.trim() === 'Metric' && /^\|[\s:|-]+$/.test(separator.trim()))
        rows.push(...block.slice(2));
    }
    block = [];
  };
  for (const line of text.split('\n')) {
    if (line.startsWith('|')) block.push(line);
    else flush();
  }
  flush();
  return rows;
}

/** First-cell backticked names in a catalogue table: the metric names the pull surface serves. */
export function catalogue(text) {
  const out = new Set();
  for (const row of metricTableRows(text))
    for (const m of row.matchAll(/^\|\s*`([a-z0-9_]+)`/g)) out.add(m[1]);
  return out;
}

/**
 * The Prometheus wire names an ingest can derive from the OTLP catalogue's dotted names: dots
 * become underscores, and a counter also arrives with `_total` appended, so both shapes are
 * allowed.
 */
export function otlpWireNames(text) {
  const out = new Set();
  for (const line of metricTableRows(text)) {
    for (const m of line.matchAll(/`([a-z0-9_]+(?:\.[a-z0-9_]+)+)`/g)) {
      const base = m[1].replace(/\./g, '_');
      out.add(base);
      out.add(`${base}_total`);
    }
  }
  return out;
}

/** Every panel object, recursing into nested `panels` arrays (rows and folds hold children there). */
function collectPanels(dashboard) {
  const out = [];
  const walk = (panels) => {
    for (const p of Array.isArray(panels) ? panels : []) {
      if (p && typeof p === 'object' && !Array.isArray(p)) {
        out.push(p);
        walk(p.panels);
      }
    }
  };
  walk(dashboard?.panels);
  return out;
}

/** The numeric panel ids, nested ones included. */
export function panelIds(dashboard) {
  return collectPanels(dashboard)
    .map((p) => p.id)
    .filter((id) => typeof id === 'number');
}

/**
 * Colony ids are `short_id()`: 8 lowercase hex characters. Both neighbours must not be hex — an
 * org hash is 24 hex and a trace id 32, so a window inside a longer hex run is one of those, not
 * a colony id.
 */
export function colonyIdLike(s) {
  return [...s.matchAll(/(?<![0-9a-f])[0-9a-f]{8}(?![0-9a-f])/g)].map((m) => m[0]);
}

/**
 * The metric names a PromQL expression queries. Label selector blocks and `by`/`without` groups
 * are stripped first: `sum by (colonizer_drop_reason) (...)` names a label, not a series, and a
 * label named like a metric must not be checked as one.
 */
export function metricTokens(expr) {
  const bare = expr.replace(/\{[^{}]*\}/g, ' ').replace(/\b(?:by|without)\s*\([^()]*\)/g, ' ');
  return [...bare.matchAll(/\bcolonizer_[a-z0-9_]+/g)].map((m) => m[0]);
}

/**
 * A token is known when a catalogue lists it, or when it is a histogram child (`_bucket`, `_sum`,
 * `_count`) of a family a catalogue lists: the catalogues name families, and those series are
 * what a scrape synthesises from the family.
 */
function known(allow, name) {
  return allow.has(name) || ['_bucket', '_sum', '_count'].some((s) => name.endsWith(s) && allow.has(name.slice(0, -s.length)));
}

const lineAt = (text, idx) => (idx >= 0 ? text.slice(0, idx).split('\n').length : undefined);

/** The regular files in a directory whose name passes `test`, sorted so problem order is stable. */
const filesIn = (dir, test) =>
  existsSync(dir) ? readdirSync(dir, { withFileTypes: true }).filter((d) => d.isFile() && test(d.name)).map((d) => d.name).sort() : [];

/**
 * Problems in one dashboard JSON. `ctx` is `{ provisioned: Map, allow: Set }` — `provisioned` maps
 * each datasource uid to its type — plus `uids` (a Map of dashboard uid to file the caller owns)
 * and `file` (this dashboard's name) so a uid used by two files can be reported on the second.
 * Lines are best-effort: the line the offending text first sits on in the raw JSON.
 */
export function checkDashboard(jsonText, ctx) {
  const problems = [];
  const at = (needle) => lineAt(jsonText, jsonText.indexOf(needle));
  let dashboard;
  try {
    dashboard = JSON.parse(jsonText);
  } catch (err) {
    const pos = /position (\d+)/.exec(err.message);
    return [{ message: `invalid JSON: ${err.message}`, line: pos ? lineAt(jsonText, Number(pos[1])) : undefined }];
  }
  if (!dashboard || typeof dashboard !== 'object' || Array.isArray(dashboard))
    return [{ message: 'dashboard JSON is not an object' }];

  for (const field of ['title', 'uid']) {
    const value = dashboard[field];
    if (typeof value !== 'string' || !value.trim()) problems.push({ message: `dashboard has no ${field}` });
  }
  if (ctx.uids instanceof Map && typeof dashboard.uid === 'string' && dashboard.uid.trim()) {
    const owner = ctx.uids.get(dashboard.uid);
    if (owner !== undefined) problems.push({ message: `duplicate dashboard uid '${dashboard.uid}' (also in ${owner})` });
    else ctx.uids.set(dashboard.uid, ctx.file ?? 'another dashboard');
  }

  const seen = new Set();
  for (const id of panelIds(dashboard)) {
    if (seen.has(id)) problems.push({ message: `duplicate panel id ${id}` });
    else seen.add(id);
  }

  const targetsOf = (panel) => (Array.isArray(panel.targets) ? panel.targets : []);

  /**
   * The datasource a node queries, as `{ uid, type }`. Its own, in either form — an object, or a
   * bare string, which names the uid and leaves the type to the provisioning — or, when the node
   * has none (a target usually does not), the panel's it inherits.
   */
  const datasourceOf = (node, inherited, provisioned) => {
    const raw = node?.datasource === undefined ? inherited : node.datasource;
    if (typeof raw === 'string') return { uid: raw, type: provisioned.get(raw) };
    if (raw && typeof raw === 'object') {
      const uid = typeof raw.uid === 'string' ? raw.uid : undefined;
      const type = typeof raw.type === 'string' ? raw.type : uid !== undefined ? provisioned.get(uid) : undefined;
      return { uid, type };
    }
    return {};
  };

  for (const panel of collectPanels(dashboard)) {
    const targets = targetsOf(panel);
    const flagged = new Set(); // one problem per bad uid per panel, not one per node naming it
    for (const node of [panel, ...targets]) {
      const { uid } = datasourceOf(node, panel.datasource, ctx.provisioned);
      if (typeof uid === 'string' && !ctx.provisioned.has(uid) && !flagged.has(uid)) {
        flagged.add(uid);
        problems.push({ message: `datasource uid '${uid}' is not provisioned`, line: at(uid) });
      }
    }
    // Metric names apply to Prometheus queries only; a Loki or Tempo target has none to get wrong.
    for (const target of targets) {
      if (datasourceOf(target, panel.datasource, ctx.provisioned).type !== 'prometheus') continue;
      const expr = typeof target.expr === 'string' ? target.expr : target.query;
      if (typeof expr !== 'string') continue;
      for (const name of metricTokens(expr)) {
        if (!known(ctx.allow, name))
          problems.push({
            message: `unknown metric name '${name}' (not in docs/observability/metrics.md or the OTLP catalogue)`,
            line: at(name),
          });
      }
    }
  }

  // A templating variable runs its own query against a datasource: a `label_values` over a metric
  // nothing exports is as broken on the variable as on a panel.
  const variables = Array.isArray(dashboard.templating?.list) ? dashboard.templating.list : [];
  for (const variable of variables) {
    if (!variable || typeof variable !== 'object') continue;
    const strings = [];
    if (typeof variable.definition === 'string') strings.push(variable.definition);
    const query = variable.query;
    if (typeof query === 'string') strings.push(query);
    else if (query && typeof query === 'object' && typeof query.query === 'string') strings.push(query.query);
    // The variable's datasource says what runs the query; with none declared, the definition is
    // the context, and `label_values`/`query_result` are Prometheus variable queries.
    const prometheus =
      datasourceOf(variable, undefined, ctx.provisioned).type === 'prometheus' ||
      (variable.datasource === undefined && strings.some((s) => /^(label_values|query_result)\s*\(/.test(s.trim())));
    if (!prometheus) continue;
    const names = new Set(strings.flatMap((expr) => metricTokens(expr)));
    for (const name of names) {
      if (!known(ctx.allow, name))
        problems.push({
          message: `unknown metric name '${name}' (not in docs/observability/metrics.md or the OTLP catalogue)`,
          line: at(name),
        });
    }
  }

  // Colony ids are forbidden in every string, not just queries: a title or a description pins the
  // dashboard to one colony just as hard as a selector does.
  const walk = (node) => {
    if (typeof node === 'string') {
      for (const id of colonyIdLike(node))
        problems.push({ message: `possible hard-coded colony id '${id}' — use a dashboard variable instead`, line: at(id) });
    } else if (Array.isArray(node)) {
      node.forEach(walk);
    } else if (node && typeof node === 'object') {
      Object.values(node).forEach(walk);
    }
  };
  walk(dashboard);
  return problems;
}

/** Problems across the tree, `{ file, line, message }`, with `file` relative to the repository root. */
export function check(opts = {}) {
  return lint(opts).problems;
}

function lint(opts = {}) {
  const {
    dashboardsDir = join(ROOT, DEFAULTS.dashboardsDir),
    provisioningDir = join(ROOT, DEFAULTS.provisioningDir),
    metricsDoc = join(ROOT, DEFAULTS.metricsDoc),
    otlpDoc = join(ROOT, DEFAULTS.otlpDoc),
  } = opts;
  const problems = [];
  const add = (path, message) => problems.push({ file: relative(ROOT, path), message });
  // The lint cannot keep its promises without its inputs, so a missing one is itself a problem.
  if (!existsSync(dashboardsDir)) add(dashboardsDir, 'dashboards directory not found');
  if (!existsSync(provisioningDir)) add(provisioningDir, 'provisioning directory not found');
  if (!existsSync(metricsDoc)) add(metricsDoc, 'metrics catalogue not found');
  if (!existsSync(otlpDoc)) add(otlpDoc, 'OTLP catalogue not found');

  const provisioned = new Map();
  if (existsSync(provisioningDir)) {
    // Only datasources/*.yml declares uids; the dashboards provider yml names directories.
    const dir = join(provisioningDir, 'datasources');
    const ymls = filesIn(dir, (name) => /\.ya?ml$/.test(name));
    for (const name of ymls)
      for (const [uid, type] of provisionedDatasources(readFileSync(join(dir, name), 'utf8'))) provisioned.set(uid, type);
    if (!ymls.length) add(provisioningDir, 'no provisioned datasource uids in provisioning/datasources');
  }
  const allow = new Set();
  if (existsSync(metricsDoc)) for (const name of catalogue(readFileSync(metricsDoc, 'utf8'))) allow.add(name);
  if (existsSync(otlpDoc)) for (const name of otlpWireNames(readFileSync(otlpDoc, 'utf8'))) allow.add(name);

  const uids = new Map();
  const dashboards = filesIn(dashboardsDir, (name) => name.endsWith('.json'));
  for (const name of dashboards) {
    const path = join(dashboardsDir, name);
    const file = relative(ROOT, path);
    for (const p of checkDashboard(readFileSync(path, 'utf8'), { provisioned, allow, uids, file }))
      problems.push({ file, line: p.line, message: p.message });
  }
  return { problems, checked: dashboards.length };
}

function main() {
  const [dashboardsDir, provisioningDir, metricsDoc, otlpDoc] = process.argv.slice(2);
  const { problems, checked } = lint({ dashboardsDir, provisioningDir, metricsDoc, otlpDoc });
  for (const p of problems) console.error(`${p.file}${p.line != null ? `:${p.line}` : ''}: ${p.message}`);
  console.error(`${checked} dashboard(s) checked, ${problems.length} problem(s)`);
  process.exit(problems.length ? 1 : 0);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) main();
