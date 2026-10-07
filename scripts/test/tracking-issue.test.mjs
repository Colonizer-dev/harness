import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

const SCRIPT = resolve(dirname(fileURLToPath(import.meta.url)), '../tracking-issue.sh');
const TITLE = 'Update runtime pins';

// A stub gh on PATH: it logs each call, one per line with its arguments joined by '|', and answers
// `issue list` with the canned "number<TAB>title" lines the real --jq filter would print, or fails
// it when GH_LIST_FAILS is set.
const scratch = mkdtempSync(join(tmpdir(), 'tracking-issue-'));
after(() => rmSync(scratch, { recursive: true, force: true }));
writeFileSync(
  join(scratch, 'gh'),
  `#!/bin/sh
(IFS='|'; echo "$*") >> "$GH_LOG"
if [ "$1 $2" = "issue list" ]; then
  [ -z "\${GH_LIST_FAILS:-}" ] || exit 1
  printf '%b' "$GH_ISSUES"
fi
`,
);
chmodSync(join(scratch, 'gh'), 0o755);

let runs = 0;
/** Runs the real script against the stub; `calls` is every gh call after the issue lookup. */
function run(args, issues, env = {}) {
  const log = join(scratch, `calls-${++runs}`);
  writeFileSync(log, '');
  const r = spawnSync('sh', [SCRIPT, ...args], {
    encoding: 'utf8',
    env: { ...process.env, PATH: `${scratch}${delimiter}${process.env.PATH}`, GH_LOG: log, GH_ISSUES: issues, ...env },
  });
  const [lookup, ...calls] = readFileSync(log, 'utf8').split('\n').filter(Boolean);
  assert.match(lookup, /^issue\|list\|--state\|open\|/);
  return { ...r, calls };
}

// The search matches titles loosely, so a lookalike comes back too and must be left alone.
const lookalike = `7\\t${TITLE} (old)\\n`;
const open = `${lookalike}12\\t${TITLE}\\n`;

test('update edits the open issue with the exact title', () => {
  const r = run(['update', TITLE, '/tmp/issue.md'], open);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls, ['issue|edit|12|--body-file|/tmp/issue.md']);
  assert.match(r.stdout, /Updated issue #12/);
});

test('update opens an issue when none has the title', () => {
  const r = run(['update', TITLE, '/tmp/issue.md'], lookalike);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls, [`issue|create|--title|${TITLE}|--body-file|/tmp/issue.md`]);
});

test('close closes the open issue with the message as a comment', () => {
  const message = 'Runtime pins are current as of 2026-09-22.';
  const r = run(['close', TITLE, message], open);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls, [`issue|close|12|--comment|${message}`]);
  assert.match(r.stdout, /Closed issue #12/);
});

test('close closes every open issue with the exact title', () => {
  const r = run(['close', TITLE, 'current'], `${open}13\\t${TITLE}\\n`);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls, ['issue|close|12|--comment|current', 'issue|close|13|--comment|current']);
  assert.match(r.stdout, /Closed issue #12\nClosed issue #13/);
});

test('close with no open issue closes nothing and succeeds', () => {
  const r = run(['close', TITLE, 'current'], lookalike);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls, []);
});

test('a failing lookup stops the script rather than reading as no issue', () => {
  const r = run(['update', TITLE, '/tmp/issue.md'], open, { GH_LIST_FAILS: '1' });
  assert.notEqual(r.status, 0);
  assert.deepEqual(r.calls, [], 'no duplicate issue is opened');
});
