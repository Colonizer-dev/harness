// scripts/ci/retry-flaky.mjs: a failing test command is run once more, so a flake turns the job green
// *and* is recorded (a JSON record, a summary line, a warning), while a real failure still exits
// non-zero. These drive the real script — a marker file makes the command fail only on its first run
// — and unit-test its parsers; the last test validates the real scripts/flaky-tests.txt, so CI's
// scripts job fails a quarantine entry without an issue number.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { extractFailingTests, parseQuarantine } from '../ci/retry-flaky.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const SCRIPT = resolve(ROOT, 'scripts/ci/retry-flaky.mjs');
const scratch = mkdtempSync(join(tmpdir(), 'retry-flaky-'));
after(() => rmSync(scratch, { recursive: true, force: true }));

/** Run the script over a shell command; return its status, output and the files it wrote. */
function invoke(command, env = {}, cwd) {
  const dir = mkdtempSync(join(scratch, 'run-'));
  const flakes = join(dir, 'flakes.json');
  const summary = join(dir, 'summary.md');
  const result = spawnSync(process.execPath, [SCRIPT, '--', command], {
    encoding: 'utf8',
    cwd,
    env: {
      ...process.env,
      FLAKES_FILE: flakes,
      GITHUB_STEP_SUMMARY: summary,
      GITHUB_JOB: 'runner',
      GITHUB_RUN_ID: '42',
      GITHUB_REPOSITORY: 'colonizer/colonizer',
      GITHUB_SERVER_URL: 'https://github.com',
      GITHUB_SHA: 'abc123',
      ...env,
    },
  });
  return {
    status: result.status,
    stdout: `${result.stdout}${result.stderr}`,
    flakes: existsSync(flakes) ? JSON.parse(readFileSync(flakes, 'utf8')) : null,
    summary: existsSync(summary) ? readFileSync(summary, 'utf8') : '',
  };
}

/** A command that fails, printing `not ok 1 - beta flaky`, until the marker it writes exists. */
function toggler() {
  const dir = mkdtempSync(join(scratch, 'tog-'));
  const file = join(dir, 'toggler.mjs');
  writeFileSync(
    file,
    [
      "import { existsSync, writeFileSync } from 'node:fs';",
      `if (existsSync(${JSON.stringify(join(dir, 'marker'))})) process.exit(0);`,
      `writeFileSync(${JSON.stringify(join(dir, 'marker'))}, 'x');`,
      "console.log('not ok 1 - beta flaky');",
      'process.exit(1);',
    ].join('\n'),
  );
  return `'${process.execPath}' '${file}'`;
}

test('a passing command exits 0 and writes no flakes file', () => {
  const run = invoke(`'${process.execPath}' -e 'process.exit(0)'`);
  assert.equal(run.status, 0);
  assert.equal(run.flakes, null);
});

test('a command that fails then passes is a flake: exit 0, one record, a summary line', () => {
  const command = toggler();
  const run = invoke(command);
  assert.equal(run.status, 0);
  assert.equal(run.flakes.length, 1);
  assert.deepEqual(run.flakes[0], {
    job: 'runner',
    command,
    cwd: '.',
    attempt: 2,
    failed_tests: ['beta flaky'],
    quarantined: false,
    run_url: 'https://github.com/colonizer/colonizer/actions/runs/42',
  });
  assert.match(run.stdout, /::warning title=Flaky test::/);
  assert.match(run.summary, /passed with flake/);
  assert.match(run.summary, /Failing tests: beta flaky/);
  assert.match(
    run.summary,
    /Quarantine list: https:\/\/github\.com\/colonizer\/colonizer\/blob\/abc123\/scripts\/flaky-tests\.txt/,
  );
});

test('the record and summary name the working directory below the root', () => {
  const run = invoke(toggler(), {}, join(ROOT, 'scripts'));
  assert.equal(run.status, 0);
  assert.equal(run.flakes[0].cwd, 'scripts');
  assert.match(run.summary, /in scripts failed once/);
  assert.doesNotMatch(run.summary, /in \. failed/);
});

test('a failing name on the quarantine list is recorded as quarantined', () => {
  const quarantine = join(scratch, 'flaky-tests.txt');
  writeFileSync(quarantine, '# known flakes\nbeta flaky  #123\n');
  const run = invoke(toggler(), { FLAKY_TESTS_FILE: quarantine });
  assert.equal(run.status, 0);
  assert.equal(run.flakes[0].quarantined, true);
  assert.match(run.summary, /on the quarantine list/);
});

test('a command that fails twice exits with the failure code and writes no record', () => {
  const run = invoke(`'${process.execPath}' -e 'process.exit(3)'`);
  assert.equal(run.status, 3);
  assert.equal(run.flakes, null);
  assert.equal(run.summary, '');
});

test('extractFailingTests reads libtest, node:test spec and TAP, and vitest output', () => {
  assert.deepEqual(extractFailingTests('test auth::login ... FAILED\ntest auth::logout ... ok\n'), ['auth::login']);
  assert.deepEqual(
    extractFailingTests('✔ alpha passes (1ms)\n✖ beta fails (0.17ms)\nℹ fail 1\n\n✖ failing tests:\n\n✖ beta fails (0.17ms)\n'),
    ['beta fails'],
  );
  assert.deepEqual(extractFailingTests('TAP version 13\nnot ok 2 - beta fails\n'), ['beta fails']);
  assert.deepEqual(extractFailingTests(' FAIL  src/a.test.ts > sums > adds\n × src/a.test.ts > sums > adds 12ms\n'), [
    'src/a.test.ts > sums > adds',
  ]);
  assert.deepEqual(extractFailingTests('all good\n'), []);
});

test('parseQuarantine reads names and issue numbers, ignoring comments and blanks', () => {
  const entries = parseQuarantine(
    '# a comment\n\nsome::test  #123\nmodule::other  #7\nmalformed entry\n#\nsrc/a.test.ts > x  #45\n',
  );
  assert.deepEqual(entries, [
    { name: 'some::test', issue: '123' },
    { name: 'module::other', issue: '7' },
    { name: 'malformed entry', issue: null },
    { name: 'src/a.test.ts > x', issue: '45' },
  ]);
});

test('every entry in scripts/flaky-tests.txt carries an issue number', () => {
  const file = resolve(dirname(fileURLToPath(import.meta.url)), '../flaky-tests.txt');
  for (const entry of parseQuarantine(readFileSync(file, 'utf8'))) {
    assert.match(entry.issue ?? '', /^\d+$/, `quarantine entry without an issue number: ${entry.name}`);
  }
});
