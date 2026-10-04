#!/usr/bin/env node
// Runs a test command and, when it fails, runs it once more. A retry that fails too ends the step
// with its exit code, so a real breakage costs one extra run. A retry that passes was a flake, and
// this stops hiding it: a `::warning` annotation, a line on the job summary, and a JSON record
// appended to FLAKES_FILE ($RUNNER_TEMP/flakes.json on a runner, ./flakes.json locally) that CI
// uploads. A test named in scripts/flaky-tests.txt (`name  #123`, an issue number required) is
// marked `quarantined: true` — a known flake, not a silent one; listing it does not skip it.
//
//   node scripts/ci/retry-flaky.mjs -- <command>   # a shell string, run with `sh -c`, so globs expand
//
// Exit code: the command's, except that a pass after a failure is 0.
import { spawn } from 'node:child_process';
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

/** Runs the command, streaming its output to our stdio; unless `capture` is false, the output is
 *  also kept for the parsers. The retry passes `capture: false`, so only the first log is held. */
function run(shellCommand, { capture = true } = {}) {
  return new Promise((resolvePromise) => {
    const child = spawn('sh', ['-c', shellCommand], { stdio: capture ? ['inherit', 'pipe', 'pipe'] : 'inherit' });
    let output = '';
    if (capture) {
      for (const [stream, pipe] of [['stdout', process.stdout], ['stderr', process.stderr]]) {
        child[stream].on('data', (chunk) => {
          pipe.write(chunk);
          output += chunk;
        });
      }
    }
    child.on('error', (error) => resolvePromise({ code: 127, output: `${output}${error.message}\n` }));
    child.on('close', (code) => resolvePromise({ code: code ?? 1, output }));
  });
}

// The test-runner outputs worth parsing, best effort: libtest (`test foo::bar ... FAILED`), node:test
// spec (`✖ name (12ms)`, what Node 24 prints when not a TTY) and TAP (`not ok 2 - name`), and vitest
// (` FAIL  file > name`, `× name`). A name we cannot read is not fatal; the summary says so.
const FAILURE_PATTERNS = [
  /^test (.+?) \.\.\. FAILED$/,
  /^\s*✖ (.+) \(\d+(?:\.\d+)?ms\)$/,
  /^not ok \d+ - (.+)$/,
  /^\s*(?:FAIL|×)\s+(.+?)\s*$/,
];

/** The distinct failing test names in `output`, with any ` (12ms)` duration stripped. */
export function extractFailingTests(output) {
  const names = new Set();
  for (const raw of output.replace(/\x1b\[[0-9;]*m/g, '').split('\n')) {
    for (const pattern of FAILURE_PATTERNS) {
      const match = raw.match(pattern);
      if (!match) continue;
      const name = match[1].trim().replace(/\s+\(?\d+(?:\.\d+)?ms\)?$/, '');
      if (name) names.add(name);
      break;
    }
  }
  return [...names];
}

/** The entries in scripts/flaky-tests.txt: `{name, issue}`, `issue` null when it is malformed. */
export function parseQuarantine(text) {
  const entries = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const match = line.match(/^(.*?)\s+#(\d+)$/);
    entries.push(match ? { name: match[1].trim(), issue: match[2] } : { name: line, issue: null });
  }
  return entries;
}

function quarantineNames() {
  const file = process.env.FLAKY_TESTS_FILE || join(ROOT, 'scripts/flaky-tests.txt');
  return new Set(existsSync(file) ? parseQuarantine(readFileSync(file, 'utf8')).map((e) => e.name) : []);
}

function appendRecord(record) {
  const file = process.env.FLAKES_FILE || join(process.env.RUNNER_TEMP || '.', 'flakes.json');
  let records = [];
  if (existsSync(file)) {
    try {
      records = JSON.parse(readFileSync(file, 'utf8'));
    } catch {
      records = [];
    }
  }
  records.push(record);
  writeFileSync(file, `${JSON.stringify(records, null, 2)}\n`);
}

/** Emit a GitHub annotation, escaping the characters the command grammar reserves. */
function annotate(level, title, message) {
  const escape = (s) => s.replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A');
  console.log(`::${level} title=${escape(title)}::${escape(message)}`);
}

async function main() {
  const argv = process.argv.slice(2);
  const separator = argv.indexOf('--');
  const command = separator === -1 ? argv : argv.slice(separator + 1);
  if (command.length === 0) {
    console.error('usage: node scripts/ci/retry-flaky.mjs -- <command>');
    return 2;
  }
  const display = command.join(' ');

  const first = await run(display);
  if (first.code === 0) return 0;

  console.log(`::notice title=Retrying once::\`${display}\` failed (exit ${first.code}); running it once more.`);
  console.log(`::group::Retry: ${display}`);
  const second = await run(display, { capture: false });
  console.log('::endgroup::');
  if (second.code !== 0) return second.code;

  // The first run failed, the second passed: a flake. Job id and run URL come from the runner's own
  // environment, so the record points at the run it came from.
  const job = process.env.GITHUB_JOB || 'local';
  const server = process.env.GITHUB_SERVER_URL || 'https://github.com';
  const repo = process.env.GITHUB_REPOSITORY || '';
  const runUrl = process.env.GITHUB_RUN_ID ? `${server}/${repo}/actions/runs/${process.env.GITHUB_RUN_ID}` : '';
  const failing = extractFailingTests(first.output);
  const known = quarantineNames();
  const quarantined = failing.some((name) => known.has(name));
  const listed = failing.length ? failing.join(', ') : '(none identified)';
  // Where the command ran, relative to the workspace, so `npm test` in a module subdirectory is
  // distinguishable from the root's: `.` at the root, `modules/agents/acp` below it.
  const workspace = process.env.GITHUB_WORKSPACE || ROOT;
  const cwd = relative(workspace, process.cwd()).replaceAll('\\', '/') || '.';

  appendRecord({ job, command: display, cwd, attempt: 2, failed_tests: failing, quarantined, run_url: runUrl });

  const note = quarantined ? ' (on the quarantine list)' : '';
  const where = cwd === '.' ? '' : ` in ${cwd}`;
  annotate('warning', 'Flaky test', `\`${display}\`${where} failed once${note}, passed on retry. Failing tests: ${listed}`);
  const link = `${server}/${repo}/blob/${process.env.GITHUB_SHA || 'HEAD'}/scripts/flaky-tests.txt`;
  const summary =
    `⚠️ passed with flake: \`${display}\`${where} failed once${note}, passed on retry. ` +
    `Failing tests: ${listed}. Quarantine list: ${link}`;
  console.log(summary);
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${summary}\n`);
  return 0;
}

// Only when run, not when its parsers are imported by a test. The exit code is set rather than
// `process.exit`-ed: exiting outright would drop the streamed output still buffered in
// `process.stdout` (a multi-megabyte test log under backpressure), so let the process drain first.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = await main();
}
