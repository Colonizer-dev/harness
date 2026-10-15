// scripts/check-dashboards.mjs: every provisioned Grafana dashboard parses, queries only
// catalogued metrics through provisioned datasource uids — a target inheriting its panel's, a
// string-form datasource naming a uid, a templating variable's label_values included — keeps its
// panel ids unique per file and its dashboard uid unique across files, and hard-codes no colony
// id. The catalogues themselves are only the docs' `Metric` tables, so a logs table's event names
// are never allowed metrics. The rules are pinned on synthetic dashboards and a throwaway
// provisioning tree; the last tests run the real check (and the real docs) over this repository,
// whose dashboard JSONs land with the rest of issue #863.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { catalogue, check, checkDashboard, colonyIdLike, metricTokens, otlpWireNames } from '../check-dashboards.mjs';

const SCRIPT = resolve(dirname(fileURLToPath(import.meta.url)), '../check-dashboards.mjs');
const ROOT = resolve(dirname(SCRIPT), '..');
const scratch = mkdtempSync(join(tmpdir(), 'dashboards-'));
after(() => rmSync(scratch, { recursive: true, force: true }));

// The synthetic catalogues, written the way the real docs write their tables: a pull-surface
// table and an OTLP one with a single dotted name, small enough to see what each rule accepts.
const METRICS = [
  '| Metric | Type |',
  '| --- | --- |',
  '| `colonizer_colonies` | gauge |',
  '| `colonizer_gateway_request_duration_seconds` | histogram |',
  '',
].join('\n');
const OTLP = ['| Metric | Type |', '| --- | --- |', '| `colonizer.observability.dropped` | counter |', ''].join('\n');
const ALLOW = new Set([...catalogue(METRICS), ...otlpWireNames(OTLP)]);
const PROVISIONED = new Map([
  ['prom-a', 'prometheus'],
  ['loki-a', 'loki'],
]);

const dashboard = (over = {}) => ({ title: 'T', uid: 't', panels: [], ...over });
const promPanel = (expr, uid = 'prom-a') => ({
  id: 1,
  type: 'timeseries',
  targets: [{ datasource: { type: 'prometheus', uid }, expr }],
});
const problemsWith = (panels) =>
  checkDashboard(JSON.stringify(dashboard({ panels })), { provisioned: PROVISIONED, allow: ALLOW });

/** A throwaway tree: two provisioned uids, the two catalogues, and the named dashboard files. */
function tree(name, files) {
  const root = join(scratch, name);
  mkdirSync(join(root, 'provisioning/datasources'), { recursive: true });
  mkdirSync(join(root, 'dashboards'), { recursive: true });
  writeFileSync(
    join(root, 'provisioning/datasources/datasources.yml'),
    [
      'apiVersion: 1',
      'datasources:',
      '  - name: Prom',
      '    type: prometheus',
      '    uid: prom-a',
      '  - name: Loki',
      '    type: loki',
      '    uid: loki-a',
      '',
    ].join('\n'),
  );
  writeFileSync(join(root, 'metrics.md'), METRICS);
  writeFileSync(join(root, 'observability.md'), OTLP);
  for (const [file, body] of Object.entries(files))
    writeFileSync(join(root, 'dashboards', file), typeof body === 'string' ? body : JSON.stringify(body));
  return {
    dashboardsDir: join(root, 'dashboards'),
    provisioningDir: join(root, 'provisioning'),
    metricsDoc: join(root, 'metrics.md'),
    otlpDoc: join(root, 'observability.md'),
  };
}

test('a metric name no catalogue lists is a problem', () => {
  const problems = problemsWith([promPanel('sum by (status) (colonizer_nope_total)')]);
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /unknown metric name 'colonizer_nope_total'/);
});

test('the histogram children of a catalogued family pass', () => {
  const expr = 'histogram_quantile(0.95, sum by (le) (rate(colonizer_gateway_request_duration_seconds_bucket[5m])))';
  assert.deepEqual(problemsWith([promPanel(expr)]), []);
});

test('a label named like a metric is not a metric', () => {
  const expr = 'sum by (colonizer_drop_reason) (rate(colonizer_observability_dropped_total[5m]))';
  assert.deepEqual(problemsWith([promPanel(expr)]), []);
  // The strip is what keeps the label quiet, and the OTLP `_total` derivation what accepts the
  // metric, so pin both directly.
  assert.deepEqual(metricTokens(expr), ['colonizer_observability_dropped_total']);
  assert.ok(otlpWireNames(OTLP).has('colonizer_observability_dropped_total'));
});

test('duplicate panel ids are a problem, nested panels counted too', () => {
  const problems = check(
    tree('dup-panel-ids', {
      'a.json': dashboard({
        panels: [promPanel('colonizer_colonies'), { id: 2, type: 'row', panels: [promPanel('colonizer_colonies')] }],
      }),
    }),
  );
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /duplicate panel id 1/);
});

test('a datasource uid nothing provisions is a problem; a provisioned one passes', () => {
  const bad = check(
    tree('ds-uid', {
      'a.json': dashboard({ panels: [{ ...promPanel('colonizer_colonies'), datasource: { type: 'prometheus', uid: 'nope' } }] }),
    }),
  );
  assert.equal(bad.length, 1, JSON.stringify(bad));
  assert.match(bad[0].message, /datasource uid 'nope' is not provisioned/);
  assert.deepEqual(check(tree('ds-uid-ok', { 'a.json': dashboard({ panels: [promPanel('colonizer_colonies')] }) })), []);
});

test('a string-form datasource is its uid: an unprovisioned one is a problem, a provisioned one is typed', () => {
  const problems = problemsWith([
    { id: 1, type: 'timeseries', datasource: 'nope-str', targets: [{ datasource: 'nope-str', expr: 'colonizer_colonies' }] },
  ]);
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /datasource uid 'nope-str' is not provisioned/);
  // A string uid the provisioning declares carries its type from there, so its PromQL is checked.
  const metric = problemsWith([
    { id: 1, type: 'timeseries', datasource: 'prom-a', targets: [{ datasource: 'prom-a', expr: 'colonizer_nope_total' }] },
  ]);
  assert.equal(metric.length, 1, JSON.stringify(metric));
  assert.match(metric[0].message, /unknown metric name 'colonizer_nope_total'/);
});

test("a target without its own datasource inherits the panel's, metrics included", () => {
  const panel = (datasource) => ({ id: 1, type: 'timeseries', datasource, targets: [{ expr: 'colonizer_nope_total' }] });
  const problems = problemsWith([panel({ type: 'prometheus', uid: 'prom-a' })]);
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /unknown metric name 'colonizer_nope_total'/);
  // The panel's string form inherits just as well, with the type looked up from the provisioning.
  assert.match(problemsWith([panel('prom-a')])[0]?.message ?? '', /unknown metric name 'colonizer_nope_total'/);
  // A catalogued metric through an inherited datasource passes.
  assert.deepEqual(
    problemsWith([{ id: 1, type: 'timeseries', datasource: { type: 'prometheus', uid: 'prom-a' }, targets: [{ expr: 'colonizer_colonies' }] }]),
    [],
  );
});

test('a templating variable querying an unknown metric is a problem', () => {
  const variable = (over = {}) => ({
    type: 'query',
    name: 'org',
    datasource: { type: 'prometheus', uid: 'prom-a' },
    definition: 'label_values(colonizer_nope, org)',
    query: { query: 'label_values(colonizer_nope, org)', refId: 'PromVariableQuery' },
    ...over,
  });
  const problems = checkDashboard(JSON.stringify(dashboard({ templating: { list: [variable()] } })), {
    provisioned: PROVISIONED,
    allow: ALLOW,
  });
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /unknown metric name 'colonizer_nope'/);
  // With no datasource declared, the definition is the context — label_values is a Prometheus query.
  const context = checkDashboard(
    JSON.stringify(
      dashboard({
        templating: { list: [variable({ datasource: undefined, definition: 'label_values(colonizer_nope, x)' })] },
      }),
    ),
    { provisioned: PROVISIONED, allow: ALLOW },
  );
  assert.equal(context.length, 1, JSON.stringify(context));
  assert.match(context[0].message, /unknown metric name 'colonizer_nope'/);
  // The same variable over a catalogued metric passes, as colony.json's org variable does.
  const ok = checkDashboard(
    JSON.stringify(
      dashboard({
        templating: {
          list: [variable({ definition: 'label_values(colonizer_colonies, org)', query: { query: 'label_values(colonizer_colonies, org)', refId: 'PromVariableQuery' } })],
        },
      }),
    ),
    { provisioned: PROVISIONED, allow: ALLOW },
  );
  assert.deepEqual(ok, []);
});

test('a hard-coded colony id is a problem; variables, org hashes and trace ids are not', () => {
  const selector = (value) => `sum(colonizer_colonies{colony="${value}"})`;
  const flagged = check(tree('colony-id', { 'a.json': dashboard({ panels: [promPanel(selector('a1b2c3d4'))] }) }));
  assert.equal(flagged.length, 1, JSON.stringify(flagged));
  assert.match(flagged[0].message, /possible hard-coded colony id 'a1b2c3d4'/);
  assert.equal(flagged[0].line, 1); // one-line JSON, so the best-effort line is deterministic
  assert.deepEqual(check(tree('colony-var', { 'a.json': dashboard({ panels: [promPanel(selector('$colony'))] }) })), []);
  // An org hash is 24 hex and a trace id 32: windows inside a longer hex run are not colony ids.
  const orgHash = 'a'.repeat(24);
  const traceId = 'b'.repeat(32);
  assert.deepEqual(colonyIdLike(orgHash), []);
  assert.deepEqual(colonyIdLike(traceId), []);
  assert.deepEqual(
    check(
      tree('colony-long', {
        'a.json': dashboard({
          panels: [
            {
              id: 1,
              type: 'timeseries',
              targets: [
                { datasource: { type: 'prometheus', uid: 'prom-a' }, expr: selector(orgHash) },
                { datasource: { type: 'prometheus', uid: 'prom-a' }, expr: selector(traceId) },
              ],
            },
          ],
        }),
      }),
    ),
    [],
  );
});

test('a dashboard that is not JSON is a problem', () => {
  const problems = check(tree('broken', { 'broken.json': '{"title": "T", "uid": "b", ' }));
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].message, /invalid JSON/);
});

test('a dashboard uid used by two files is a problem naming both files', () => {
  const problems = check(tree('dup-uid', { 'a.json': dashboard({ title: 'A' }), 'b.json': dashboard({ title: 'B' }) }));
  assert.equal(problems.length, 1, JSON.stringify(problems));
  assert.match(problems[0].file, /b\.json$/);
  assert.match(problems[0].message, /duplicate dashboard uid 't' \(also in .*a\.json\)/);
});

test('a dashboard without a title or without a uid is a problem', () => {
  const problems = check(
    tree('missing-fields', { 'no-title.json': { uid: 'u', panels: [] }, 'no-uid.json': { title: 'T', panels: [] } }),
  );
  assert.equal(problems.length, 2, JSON.stringify(problems));
  assert.ok(problems.some((p) => /no-title\.json$/.test(p.file) && /dashboard has no title/.test(p.message)));
  assert.ok(problems.some((p) => /no-uid\.json$/.test(p.file) && /dashboard has no uid/.test(p.message)));
});

test('the real catalogues still yield their names, and no logs-table name leaks in', () => {
  const pull = catalogue(readFileSync(join(ROOT, 'docs/observability/metrics.md'), 'utf8'));
  assert.deepEqual(
    [...pull].sort(),
    [
      'colonizer_attention',
      'colonizer_colonies',
      'colonizer_colonies_finished_total',
      'colonizer_colonies_started_total',
      'colonizer_cost_usd_total',
      'colonizer_disk_free_bytes',
      'colonizer_gateway_fallbacks_total',
      'colonizer_gateway_in_flight',
      'colonizer_gateway_queued',
      'colonizer_gateway_request_duration_seconds',
      'colonizer_gateway_requests_total',
      'colonizer_questions_open',
      'colonizer_queue_depth',
      'colonizer_storage_alert',
      'colonizer_tokens_total',
    ],
  );
  const wire = otlpWireNames(readFileSync(join(ROOT, 'docs/observability.md'), 'utf8'));
  for (const dotted of [
    'colonizer.colonies',
    'colonizer.queue.depth',
    'colonizer.gateway.requests',
    'colonizer.gateway.failures',
    'colonizer.gateway.duration',
    'colonizer.gateway.queue_time',
    'colonizer.gateway.input_tokens',
    'colonizer.gateway.output_tokens',
    'colonizer.spend.cost',
    'colonizer.spend.input_tokens',
    'colonizer.spend.output_tokens',
    'colonizer.observability.exported',
    'colonizer.observability.dropped',
    'colonizer.observability.export_failures',
  ]) {
    const base = dotted.replaceAll('.', '_');
    assert.ok(wire.has(base), `${base} missing from the OTLP allowlist`);
    assert.ok(wire.has(`${base}_total`), `${base}_total missing from the OTLP allowlist`);
  }
  // The logs table's header is `Source`, not `Metric`: its event names are not metrics, and the
  // exporter never serves them as series.
  assert.ok(!wire.has('colonizer_agent_event'));
  assert.ok(!wire.has('colonizer_harness_log'));
});

test('importing the checker without an argument vector entry neither runs it nor throws', () => {
  const run = spawnSync(
    process.execPath,
    ['--input-type=module', '-e', `import(${JSON.stringify(pathToFileURL(SCRIPT).href)})`],
    { encoding: 'utf8', cwd: tmpdir() },
  );
  assert.equal(run.status, 0, run.stderr);
  assert.doesNotMatch(run.stderr, /Error|TypeError/);
});

test("this repository's dashboards pass the lint", () => {
  assert.deepEqual(check(), []);
  const run = spawnSync(process.execPath, [SCRIPT], { encoding: 'utf8' });
  assert.equal(run.status, 0, run.stderr);
  assert.match(run.stderr, /0 problem/);
});
