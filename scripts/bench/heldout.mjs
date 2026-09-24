// The held-out companion checks: one per bench task family, kept OUTSIDE this repository on purpose — a
// check the agents being scored can read is a check they can satisfy without doing the work. Each
// `run --heldout <dir>` scores every colony's branch against its family's companion on a fresh clone and
// reports the visible-vs-held-out gap; a family whose gap grows is overfitted. The set is a manifest
// (`heldout.json`) with a versioned history: companions retire after RETIRE_AFTER scoring decisions, the
// number the synth pool uses. docs/bench.md says the rest.
import { closeSync, cpSync, existsSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

import { RETIRE_AFTER } from './synth.mjs';

/** A family's gap fails the run once it goes strictly above this. */
export const DEFAULT_MAX_GAP = 0.25;

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

const pct = (n) => `${Math.round(n * 100)}%`;

/** A task's family: its `family` field when it has one, else its id. */
export const familyOf = (task) => task.family ?? task.id;

/** Whether `dir` is `root` itself or below it, by path alone: a sibling named `..x` is not an escape. */
const within = (root, dir) => {
  const rel = relative(root, dir);
  return !isAbsolute(rel) && rel !== '..' && !rel.startsWith(`..${sep}`);
};

/** `dir` with symlinks resolved as far as the path exists — a set reached through a link into this
 *  repository counts as inside it, though the link itself lives outside. */
function realPath(dir) {
  let cur = resolve(dir);
  const rest = [];
  while (!existsSync(cur)) {
    rest.unshift(basename(cur));
    cur = dirname(cur);
  }
  return join(realpathSync(cur), ...rest);
}

/** A set directory inside this repository is refused: committed next to the agents being scored, the
 *  companions would be as visible to them as the checks they are held out against. */
export function outsideRepo(dir) {
  const resolved = resolve(dir);
  if (within(realPath(REPO_ROOT), realPath(resolved))) {
    throw new Error(`${resolved} is inside this repository's working tree; held-out companions committed next to the agents being scored are visible to them — keep the set outside, like the synth pool`);
  }
  return resolved;
}

/** A companion's file, resolved inside the set directory; a manifest path that escapes it is refused. */
function companionFile(set, entry) {
  const file = resolve(set.dir, entry.file);
  if (!within(set.dir, file)) throw new Error(`${entry.id}: ${entry.file} escapes the held-out set at ${set.dir}`);
  return file;
}

/** Reads `<dir>/heldout.json`, checking every companion's path stays inside the set. A missing or
 *  malformed manifest is an error, never an empty set quietly. */
export function loadSet(dir) {
  const set = { dir: outsideRepo(dir) };
  const file = join(set.dir, 'heldout.json');
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(file, 'utf8'));
  } catch (e) {
    throw new Error(`no held-out set at ${file}; add a companion with: node scripts/bench.mjs heldout add --heldout <dir> --family <family> --check <file> (${e.code ?? e.message})`);
  }
  Object.assign(set, manifest);
  if (!Array.isArray(set.checks)) throw new Error(`${file}: the manifest carries no checks array`);
  for (const entry of set.checks) companionFile(set, entry);
  return set;
}

/** A brand-new set, version 1, for a directory that has no manifest yet. */
export function newSet(dir) {
  return { dir: outsideRepo(dir), version: 1, checks: [], history: [] };
}

/** Writes the manifest through a temp file and rename, so a crash cannot leave a half-written set. */
export function saveSet(set) {
  mkdirSync(set.dir, { recursive: true });
  const { dir, ...manifest } = set;
  writeFileSync(join(dir, 'heldout.json.tmp'), `${JSON.stringify(manifest, null, 2)}\n`);
  renameSync(join(dir, 'heldout.json.tmp'), join(dir, 'heldout.json'));
}

/** An exclusive lock over the set for the mutating steps (a run's record, `heldout add`); a hard kill
 *  leaves heldout.lock to remove by hand. */
export function lockSet(dir) {
  mkdirSync(dir, { recursive: true });
  try {
    closeSync(openSync(join(dir, 'heldout.lock'), 'wx'));
  } catch (e) {
    if (e.code === 'EEXIST') throw new Error(`another bench command is using the held-out set at ${resolve(dir)}; remove ${join(dir, 'heldout.lock')} only if it was left by a crash`);
    throw e;
  }
  return () => rmSync(join(dir, 'heldout.lock'), { force: true });
}

/** The one active companion per family; a family without one is named, so the run fails before it spends. */
export function companionsFor(set, families) {
  const picked = new Map();
  const missing = [];
  for (const family of families) {
    const companion = set.checks.find((c) => c.family === family && !c.retired);
    if (companion) picked.set(family, companion);
    else missing.push(family);
  }
  if (missing.length > 0) {
    throw new Error(`no active held-out companion for ${missing.join(', ')}; add one with: node scripts/bench.mjs heldout add --heldout ${set.dir} --family <family> --check <file>`);
  }
  return picked;
}

/** Counts one scoring decision per scored companion, retiring it at RETIRE_AFTER; a retirement bumps
 *  the set's version and enters the history. Returns the retired ids. */
export function recordDecisions(set, ids, now = new Date().toISOString()) {
  const retired = [];
  for (const id of ids) {
    const entry = set.checks.find((c) => c.id === id);
    if (!entry) throw new Error(`no held-out companion ${id}`);
    entry.decisions++;
    if (entry.decisions >= RETIRE_AFTER && !entry.retired) {
      entry.retired = true;
      retired.push(id);
    }
  }
  if (retired.length > 0) {
    set.version++;
    set.history.push({ version: set.version, at: now, added: [], retired });
  }
  return retired;
}

/** Adds a companion check for a family: copies the file into the set under the entry's own name, bumps
 *  the version and records the addition. A family with a still-active companion is refused — companions
 *  rotate by retirement. */
export function addCompanion(set, { family, check, now = new Date().toISOString() }) {
  const active = set.checks.find((c) => c.family === family && !c.retired);
  if (active) throw new Error(`${family} already has an active companion (${active.id}); it rotates out after ${RETIRE_AFTER} scoring decisions`);
  const source = resolve(check);
  if (!existsSync(source)) throw new Error(`no check file at ${source}`);
  const entry = { id: `${family}.${set.checks.length + 1}`, family, file: `${family}.${set.checks.length + 1}.test.mjs`, decisions: 0, retired: false, added: now };
  cpSync(source, companionFile(set, entry));
  set.checks.push(entry);
  set.version++;
  set.history.push({ version: set.version, at: now, added: [entry.id], retired: [] });
  return entry;
}

/** Per family: the visible pass rate, the held-out pass rate and the gap between them, worst gap first.
 *  A task without a pull request was never scored, and counts as failing both. */
export function familyGaps(results) {
  const families = new Map();
  for (const r of results) {
    const family = r.family ?? r.id;
    if (!families.has(family)) families.set(family, []);
    families.get(family).push(r);
  }
  const rate = (rows, key) => rows.filter((r) => (r.pr_url ? r[key] === true : false)).length / (rows.length || 1);
  return [...families.entries()]
    .map(([family, rows]) => {
      const visible = rate(rows, 'visible');
      const heldout = rate(rows, 'heldout');
      return { family, tasks: rows.length, visible, heldout, gap: visible - heldout };
    })
    .sort((a, b) => b.gap - a.gap || a.family.localeCompare(b.family));
}

/** Fails a family whose gap is strictly greater than maxGap, naming it and the numbers. */
export function gapVerdict(gaps, maxGap = DEFAULT_MAX_GAP) {
  const failures = gaps
    .filter((g) => g.gap > maxGap)
    .map((g) => `${g.family}: visible ${pct(g.visible)}, held-out ${pct(g.heldout)}, gap ${pct(g.gap)} > ${pct(maxGap)}`);
  return { ok: failures.length === 0, failures };
}

/** The gap report: the set version the run was scored against (and where rotation left the set), then one
 *  row per family, worst gap first, with over-threshold rows in bold and named again below the table. */
export function formatGaps(gaps, { version, nextVersion, maxGap }) {
  const over = gaps.filter((g) => g.gap > maxGap);
  return [
    `Held-out set v${version}${nextVersion != null ? `, rotated to v${nextVersion} by this run` : ''}, gap threshold ${pct(maxGap)}`,
    '',
    '| Family | Visible | Held-out | Gap |',
    '| --- | --- | --- | --- |',
    ...gaps.map((g) => {
      const cell = (v) => (g.gap > maxGap ? `**${v}**` : `${v}`);
      return `| ${cell(g.family)} | ${cell(pct(g.visible))} | ${cell(pct(g.heldout))} | ${cell(pct(g.gap))} |`;
    }),
    ...(over.length > 0 ? ['', `Over threshold: ${over.map((g) => `${g.family} (gap ${pct(g.gap)} > ${pct(maxGap)})`).join(', ')}`] : []),
  ].join('\n');
}
