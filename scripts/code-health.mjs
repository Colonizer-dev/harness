#!/usr/bin/env node
// Builds the weekly code-health report: the largest Rust files and how they moved since the week
// before, flaky tests seen in the last seven days of CI, whether a release-health issue is open,
// and the test count with the last CI run's duration. The merge-train section is a placeholder —
// the merge train logs no conflict events yet, so there is nothing to read.
//
//   node scripts/code-health.mjs [--dry-run]
//
// The report is appended to $GITHUB_STEP_SUMMARY and upserted into a single issue titled
// "Code health" (through scripts/tracking-issue.sh, so a rerun edits it rather than opening
// another). The previous run's numbers travel inside the issue body as a hidden `code-health-data`
// block, which is where the week-on-week deltas come from. --dry-run prints the report and makes
// no `gh` call at all, for a laptop or a repository with no token. Only the largest-files table
// needs no network, so it always renders; every other section degrades to a "➖ …" line rather than
// failing the run when its lookup does not.
import { spawnSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ISSUE_TITLE = 'Code health';
// Keep in step with scripts/ci/check-rust-file-size.sh; that check is what fails the build, this
// report only says so.
const LARGEST_FILES_LIMIT = 2000;
const ALLOWLIST = join(ROOT, 'scripts/ci/rust-file-size-allowlist.txt');
const FLAKY_TESTS = join(ROOT, 'scripts/flaky-tests.txt');
const TRACKING_ISSUE = join(ROOT, 'scripts/tracking-issue.sh');
const TOP_FILES = 10;
const TODAY = new Date().toISOString().slice(0, 10);
// The issues the report points at: the file-size limit, and the CI flake reporting.
const FILE_SIZE_ISSUE = 825;
const FLAKE_ISSUE = 977;

/** The allowlist's paths, parsed the way check-rust-file-size.sh parses the same file. */
export function parseAllowlist(text) {
  return new Set(text.split('\n').map((line) => line.replace(/#.*/, '').trim()).filter(Boolean));
}

/** A delta for the trend column: how much a file grew or shrank since the previous run. */
function trendOf(path, lines, previousTop) {
  const before = previousTop?.find((entry) => entry.path === path);
  if (!before) return 'new';
  const delta = lines - before.lines;
  if (delta > 0) return `↑${delta}`;
  if (delta < 0) return `↓${-delta}`;
  return '—';
}

/** The largest-files section: the top ten with their movement, and whether the limit holds. `over`
 *  are the files past the limit that are not on the allowlist. */
export function renderLargestFiles(currentTop, previousTop, over = []) {
  const rows = currentTop.slice(0, TOP_FILES).map(
    ({ path, lines }) => `| \`${path}\` | ${lines} | ${trendOf(path, lines, previousTop)} |`,
  );
  const table = ['| File | Lines | Since last week |', '| --- | ---: | --- |', ...rows].join('\n');
  const status = over.length
    ? `❌ ${over.length} file${over.length === 1 ? '' : 's'} over ${LARGEST_FILES_LIMIT} lines not on the allowlist — see #${FILE_SIZE_ISSUE}: ${over.map((p) => `\`${p}\``).join(', ')}`
    : `✅ all non-test Rust files within ${LARGEST_FILES_LIMIT} lines`;
  return `${table}\n\n${status}`;
}

/** The merge-train section. Nothing reads a conflict log because none is written yet. */
export function renderMergeTrain() {
  return '➖ not yet tracked — merge-train does not log conflict events yet.';
}

/** The files check-rust-file-size.sh puts no limit on: tests.rs, a *_tests.rs, or anything under a
 *  tests/ directory. They stay in the table, but never count towards the limit — a test file over
 *  it is not a failure, and reporting one as red would contradict the check that gates the build. */
export function isTestFile(path) {
  return /(^|\/)(tests\.rs|[^/]*_tests\.rs)$/.test(path) || /(^|\/)tests\//.test(path);
}

/** The distinct test names in the records, split into ones the quarantine list already covers and
 *  ones nobody has filed. A name seen both ways is a known flake, not a new one. */
export function aggregateFlakeRecords(records, quarantineSet) {
  const newly = new Set();
  const known = new Set();
  for (const record of records) {
    for (const name of record.failed_tests ?? []) {
      if (record.quarantined || quarantineSet.has(name)) known.add(name);
      else newly.add(name);
    }
  }
  for (const name of known) newly.delete(name);
  return { newlyFlaky: [...newly].sort(), alreadyQuarantined: [...known].sort() };
}

/** The flaky-tests section. A test nobody has filed is the actionable half; a quarantined one is
 *  already tracked in scripts/flaky-tests.txt against its own issue. */
export function renderFlakes({ newlyFlaky, alreadyQuarantined }) {
  if (newlyFlaky.length === 0) {
    const known = alreadyQuarantined.length
      ? ` (${alreadyQuarantined.length} quarantined in scripts/flaky-tests.txt)`
      : '';
    return `✅ no new flakes in the last 7 days of CI${known}`;
  }
  const known = alreadyQuarantined.length
    ? `\n\nQuarantined (known): ${alreadyQuarantined.map((n) => `\`${n}\``).join(', ')}`
    : '';
  return (
    `❌ ${newlyFlaky.length} new flake${newlyFlaky.length === 1 ? '' : 's'} in the last 7 days of CI ` +
    `— see #${FLAKE_ISSUE}\n\nNew: ${newlyFlaky.map((n) => `\`${n}\``).join(', ')}${known}`
  );
}

/** The release-health section, which is only ever a pointer at the issue release-health.yml owns. */
export function renderReleaseHealth(openIssue) {
  return openIssue
    ? `❌ a release-health issue is open: ${openIssue.url}`
    : '✅ no open release-health issues';
}

/** `git grep -c` prints `path:count` per matching file, and the counts are added up. A file with
 *  no match is absent from the output, not printed as a zero. */
export function parseTestCount(gitGrepOutput) {
  let total = 0;
  for (const line of gitGrepOutput.split('\n')) {
    if (!line.trim()) continue;
    const count = Number.parseInt(line.slice(line.lastIndexOf(':') + 1), 10);
    if (Number.isFinite(count)) total += count;
  }
  return total;
}

/** A number with its change since last week, or an em dash when there is no previous run to
 *  compare with. Informational only: neither direction is a failure. */
function withTrend(value, before, unit) {
  if (before == null || !Number.isFinite(before)) return `${value}${unit}`;
  const delta = value - before;
  if (delta === 0) return `${value}${unit} (—)`;
  return `${value}${unit} (${delta > 0 ? '↑' : '↓'}${Math.abs(delta)})`;
}

/** The test-count and last-run-duration section. A run the lookup could not read says so rather than
 *  printing "undefined min". */
export function renderTestTrend({ count, minutes }, previous) {
  return [
    `- Test count: ${withTrend(count, previous?.count, '')}`,
    Number.isFinite(minutes)
      ? `- Last CI run on main: ${withTrend(minutes, previous?.minutes, ' min')}`
      : '- Last CI run on main: ➖ no completed main run found',
  ].join('\n');
}

const DATA_BLOCK = /<!-- code-health-data\s*\n([\s\S]*?)\n-->/;

/** The hidden block the previous run left, or null. A missing or damaged block is a first run, not an error, so nothing here throws. */
export function parsePreviousData(issueBody) {
  if (!issueBody) return null;
  const match = issueBody.match(DATA_BLOCK);
  if (!match) return null;
  try {
    const data = JSON.parse(match[1]);
    return data && typeof data === 'object' ? data : null;
  } catch {
    return null;
  }
}

/** The same block again, for the run writing this report. */
export function renderHiddenBlock(newData) {
  return `<!-- code-health-data\n${JSON.stringify(newData)}\n-->`;
}

const SECTIONS = [
  ['Largest files', 'largestFiles'],
  ['Flaky tests', 'flakes'],
  ['Release health', 'releaseHealth'],
  ['Test count and time', 'testTrend'],
  ['Merge-train conflict hotspots', 'mergeTrain'],
];

/** The report, from rendered sections: a heading, each section in order, then the hidden block
 *  last so the visible body above it never moves. */
export function buildReport(sections) {
  const out = ['# Code health', '', `_Generated by \`scripts/code-health.mjs\` on ${TODAY}._`, ''];
  for (const [title, key] of SECTIONS) out.push(`## ${title}`, '', sections[key], '');
  out.push(sections.hiddenBlock, '');
  return out.join('\n');
}

/* -------------------------------------------------------------------------- glue ---------- */

/** Runs a command in a shell and returns its stdout. A non-zero exit comes back as a code rather
 *  than an exception, so a failing lookup degrades its own section. Only for commands built entirely
 *  from our own literals: anything a remote call supplied goes through runArgs, so a value carrying a
 *  quote is data and never shell syntax. */
function run(command) {
  const result = spawnSync('sh', ['-c', command], { cwd: ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  return { code: result.status ?? 1, out: result.stdout ?? '' };
}

/** Runs a command with an argument array and no shell, so a run id or an artifact name from the API
 *  is passed as one argument whatever it contains. */
function runArgs(file, args) {
  const result = spawnSync(file, args, { cwd: ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  return { code: result.status ?? 1, out: result.stdout ?? '' };
}

/** The non-blank lines of a `gh … --jq` call, or undefined when the call itself failed. */
function ghLines(command) {
  const result = run(command);
  return result.code === 0 ? result.out.split('\n').filter((line) => line.trim()) : undefined;
}

/** The same, for a `gh` call whose arguments name something a remote call returned: the whole
 *  argument list is passed straight through, with no shell in between. */
function ghRunLines(args) {
  const result = runArgs(args[0], args.slice(1));
  return result.code === 0 ? result.out.split('\n').filter((line) => line.trim()) : undefined;
}

/** The first of `lines`, parsed as JSON, or undefined. A line the shell mangled is not a result. */
function firstJson(lines) {
  for (const line of lines ?? []) {
    try {
      return JSON.parse(line);
    } catch {
      return undefined;
    }
  }
  return undefined;
}

/** Every tracked Rust file and its line count, largest first. Counted in-process rather than with
 *  one `wc -l` per file, which over a full checkout is thousands of processes. */
export function listRustFileSizes() {
  const tracked = run("git ls-files '*.rs'");
  if (tracked.code !== 0) return [];
  const sizes = [];
  for (const path of tracked.out.split('\n')) {
    if (!path || !existsSync(join(ROOT, path))) continue;
    const text = readFileSync(join(ROOT, path), 'utf8');
    sizes.push({ path, lines: text ? text.split('\n').length - (text.endsWith('\n') ? 1 : 0) : 0 });
  }
  return sizes.sort((a, b) => b.lines - a.lines);
}

/** The static count of `#[test]` and `#[tokio::test]` attributes across tracked Rust sources: it
 *  counts attributes, not executions, and needs no build. */
export function countRustTests() {
  return parseTestCount(run("git grep -c -E '#\\[(tokio::)?test\\]' -- '*.rs'").out);
}

/** The quarantined test names in scripts/flaky-tests.txt. Each line is `name  #<issue>`, which
 *  parseAllowlist truncates to the name — the issue number is not needed to answer "known". */
export function quarantineNames() {
  return existsSync(FLAKY_TESTS) ? new Set(parseAllowlist(readFileSync(FLAKY_TESTS, 'utf8'))) : new Set();
}

/** The body of the open issue titled exactly "Code health", or null. The search is fuzzy, so the
 *  exact match is made here rather than in --jq, as scripts/tracking-issue.sh does. */
function previousBody() {
  const lines = ghLines(
    `gh issue list --state open --search '\\"${ISSUE_TITLE}\\" in:title' --json title,body --jq '.[] | @json'`,
  );
  for (const line of lines ?? []) {
    const issue = firstJson([line]);
    if (issue?.title === ISSUE_TITLE) return issue.body ?? '';
  }
  return null;
}

/** The flake records from every `flakes-*` artifact of the last seven days of main CI runs. An
 *  unreadable run or artifact costs its own records, not the whole section: `failed` counts what
 *  could not be read, so a run where nothing downloaded is not reported as a clean week. */
function collectFlakeRecords() {
  const runs = ghLines('gh run list --workflow ci.yml --branch main --status completed --json databaseId,createdAt --limit 20 --jq ".[] | [.databaseId, .createdAt] | @tsv"');
  if (!runs) return null;
  const cutoff = Date.now() - 7 * 24 * 3600 * 1000;
  const dir = mkdtempSync(join(tmpdir(), 'code-health-'));
  const records = [];
  let failed = 0;
  for (const [id, createdAt] of runs.map((line) => line.split('\t'))) {
    if (!id || !createdAt || Date.parse(createdAt) < cutoff) continue;
    // The names, so a run is only downloaded for an artifact it actually has; `gh run download`
    // wants a name, not an id.
    const names = ghRunLines(['gh', 'api', `repos/{owner}/{repo}/actions/runs/${id}/artifacts`, '--jq', '.artifacts[] | select(.name | startswith("flakes-")) | .name']);
    if (!names) {
      failed += 1;
      continue;
    }
    for (const name of names) {
      const got = artifactRecords(dir, id, name);
      if (got === null) failed += 1;
      else records.push(...got);
    }
  }
  rmSync(dir, { recursive: true, force: true });
  return { records, failed };
}

/** One artifact's records, or null when it could not be read. `gh run download` is the maintained
 *  path to a run's artifact: it follows the redirect to the blob store and unzips itself, so no
 *  artifact id and no `unzip` on the runner. Its own directory keeps two artifacts' `flakes.json`
 *  from overwriting one another. The run id and the artifact name both come from the API, so the call
 *  goes through runArgs: no shell, and each one stays a single argument. */
function artifactRecords(dir, runId, name) {
  const out = join(dir, `${runId}-${name}`);
  if (runArgs('gh', ['run', 'download', String(runId), '--name', String(name), '--dir', out]).code !== 0) return null;
  const records = [];
  let read = false;
  let entries;
  try {
    entries = readdirSync(out, { recursive: true });
  } catch {
    return null;
  }
  for (const entry of entries) {
    if (!String(entry).endsWith('flakes.json')) continue;
    read = true;
    try {
      records.push(...JSON.parse(readFileSync(join(out, String(entry)), 'utf8')));
    } catch {
      // A truncated or empty artifact file is not a report failure.
    }
  }
  return read ? records : null;
}

/** The open release-health issue release-health.yml keeps, or null; undefined when the lookup itself failed. */
function openReleaseHealthIssue() {
  const lines = ghLines('gh issue list --label release-health --state open --json url --jq ".[] | @json"');
  return lines ? firstJson(lines) ?? null : undefined;
}

/** The wall-clock minutes the most recent completed main CI run took, or undefined. */
function lastRunMinutes() {
  const [line] = ghLines('gh run list --workflow ci.yml --branch main --status completed --json createdAt,updatedAt --limit 1 --jq ".[] | [.createdAt, .updatedAt] | @tsv"') ?? [];
  const [createdAt, updatedAt] = (line ?? '').split('\t');
  if (!createdAt || !updatedAt) return undefined;
  return Math.max(0, Math.round((Date.parse(updatedAt) - Date.parse(createdAt)) / 60000));
}

/** Every section that needs the network, and the line it falls back to when a lookup fails. */
const UNAVAILABLE = {
  flakes: '➖ flake data unavailable this run',
  releaseHealth: '➖ release-health data unavailable this run',
  testTrend: '➖ test data unavailable this run',
};

async function main() {
  const dryRun = process.argv.includes('--dry-run');
  const previous = dryRun ? null : parsePreviousData(previousBody());

  // Largest files: no network, so it always renders. The previous week comes out of the issue body,
  // which a person can edit by hand, so its shape is checked before the trend reads it: a
  // hand-written `{"largestFiles": 5}` is a first run, not a crash.
  const previousTop = Array.isArray(previous?.largestFiles) ? previous.largestFiles : null;
  const sizes = listRustFileSizes();
  const allowed = existsSync(ALLOWLIST) ? parseAllowlist(readFileSync(ALLOWLIST, 'utf8')) : new Set();
  const over = sizes.filter((f) => f.lines > LARGEST_FILES_LIMIT && !isTestFile(f.path) && !allowed.has(f.path)).map((f) => f.path);
  const sections = { largestFiles: renderLargestFiles(sizes, previousTop, over), mergeTrain: renderMergeTrain(), ...UNAVAILABLE };

  const count = countRustTests();
  let minutes;
  if (!dryRun) {
    const flakes = collectFlakeRecords();
    // Nothing read and something to read means the lookups failed, not that the week was clean.
    if (flakes && (flakes.records.length > 0 || flakes.failed === 0)) {
      const n = flakes.records.length;
      sections.flakes = `${renderFlakes(aggregateFlakeRecords(flakes.records, quarantineNames()))}\n\n_Across ${n} flake record${n === 1 ? '' : 's'} from the last 7 days of CI._`;
    }
    const issue = openReleaseHealthIssue();
    if (issue !== undefined) sections.releaseHealth = renderReleaseHealth(issue);
    minutes = lastRunMinutes();
    // The hidden block keys the trend numbers testCount/testMinutes; the renderer reads
    // count/minutes, so the previous week is renamed into the shape it expects here.
    sections.testTrend = renderTestTrend(
      { count, minutes },
      { count: previous?.testCount, minutes: previous?.testMinutes },
    );
  }

  const data = { largestFiles: sizes.slice(0, TOP_FILES), testCount: count, testMinutes: minutes ?? null };
  const report = buildReport({ ...sections, hiddenBlock: renderHiddenBlock(data) });

  if (dryRun) {
    console.log(report);
    return 0;
  }

  // TRACKING_ISSUE and the title are our own constants and the body path is a mkdtemp we made, so
  // the quoted shell form is safe here. The scratch directory goes with the run either way.
  const scratch = mkdtempSync(join(tmpdir(), 'code-health-'));
  const body = join(scratch, 'report.md');
  try {
    writeFileSync(body, report);
    if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, report);
    const updated = run(`sh '${TRACKING_ISSUE}' update '${ISSUE_TITLE}' '${body}'`);
    if (updated.code !== 0) {
      console.error(`could not update the "${ISSUE_TITLE}" issue: ${updated.code}`);
      return 1;
    }
    console.log(updated.out.trim());
    return 0;
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

// Only when run, not when its sections are imported by a test. The exit code is set rather than
// `process.exit`-ed, so a redirected stdout is not cut off.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main();
}
