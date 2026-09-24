// scripts/require-ci-checks.mjs turns issue #367 into a ruleset a maintainer can apply. Everything
// about that is decidable without the network: the payload's shape (the six CI jobs as
// required_status_checks pinned to the GitHub Actions app, an active branch ruleset on the default
// branch), the create-vs-update decision taken from a list of existing rulesets, and the argument
// handling. The gh calls themselves are driven against a stub that logs each invocation and answers
// the ruleset list with canned JSON, in the shape of tracking-issue.test.mjs.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { DEFAULT_REPO, REQUIRED_CHECKS, RULESET_NAME, buildRulesetPayload, parseArgs, planFor } from '../require-ci-checks.mjs';

const payload = buildRulesetPayload();
const checks = payload.rules[0].parameters.required_status_checks;

test('the payload requires exactly the CI jobs, pinned to the GitHub Actions app', () => {
  assert.deepEqual(checks.map((check) => check.context), REQUIRED_CHECKS);
  for (const check of checks) assert.deepEqual(Object.keys(check).sort(), ['context', 'integration_id']);
  for (const check of checks) assert.equal(check.integration_id, 15368);
});

test('the payload is an active branch ruleset on the default branch, one required-checks rule, not strict', () => {
  assert.equal(payload.name, RULESET_NAME);
  assert.equal(payload.target, 'branch');
  assert.equal(payload.enforcement, 'active');
  assert.deepEqual(payload.conditions, { ref_name: { include: ['~DEFAULT_BRANCH'] } });
  assert.equal(payload.rules.length, 1);
  assert.equal(payload.rules[0].type, 'required_status_checks');
  assert.equal(payload.rules[0].parameters.strict_required_status_checks_policy, false);
});

test('the deliberate omissions stay omitted', () => {
  // colony-smoke never runs without the KVM runner; the supply-chain and release jobs gate on
  // news or tags, not on the pull request. If one of these ever becomes required, that is a
  // decision to make in the script's comment, not an accident to catch in a diff.
  const contexts = checks.map((check) => check.context);
  for (const never of ['colony-smoke', 'vulnerabilities', 'sbom', 'linux-binaries', 'release', 'crates'])
    assert.ok(!contexts.includes(never), `${never} must not be a required check`);
});

test('planFor creates when no ruleset has the exact name, update otherwise', () => {
  assert.deepEqual(planFor([]), { action: 'create' });
  assert.deepEqual(planFor([{ id: 7, name: `${RULESET_NAME} (old)` }]), { action: 'create' });
  assert.deepEqual(planFor([{ id: 7, name: 'other' }, { id: 42, name: RULESET_NAME }]), { action: 'update', id: 42 });
});

test('parseArgs defaults to a dry run on this repository', () => {
  assert.deepEqual(parseArgs([]), { repo: DEFAULT_REPO, apply: false });
  assert.deepEqual(parseArgs(['--apply']), { repo: DEFAULT_REPO, apply: true });
  assert.deepEqual(parseArgs(['--repo', 'owner/name', '--apply']), { repo: 'owner/name', apply: true });
});

test('parseArgs refuses a --repo without an owner/name pair and unknown flags', () => {
  for (const bad of [['--repo'], ['--repo', 'owner'], ['--repo', 'a/b/c'], ['--repo', 'a b'], ['--wat']]) {
    assert.equal(parseArgs(bad), null, JSON.stringify(bad));
  }
});

// A stub gh on PATH: it logs each call's arguments (joined with '|'), a tab, and the request body
// when the call carries `--input -`, then answers the ruleset list with the canned JSON in
// GH_RULESETS — or fails it when GH_LIST_FAILS is set. Writes go to the log's tab-separated body
// field, so a JSON body with no trailing newline cannot run into the next call's line.
const scratch = mkdtempSync(join(tmpdir(), 'require-ci-checks-'));
after(() => rmSync(scratch, { recursive: true, force: true }));
writeFileSync(
  join(scratch, 'gh'),
  `#!/bin/sh
(IFS='|'; printf '%s' "$*") >> "$GH_LOG"
printf '\\t' >> "$GH_LOG"
if [ "$3" = GET ]; then
  printf '\\n' >> "$GH_LOG"
  [ -z "\${GH_LIST_FAILS:-}" ] || { echo 'gh: no token' >&2; exit 1; }
  printf '%b' "$GH_RULESETS"
else
  cat >> "$GH_LOG"
  printf '\\n' >> "$GH_LOG"
  printf '{"id":42,"enforcement":"active","_links":{"html":{"href":"https://example/r/42"}}}'
fi
`,
);
chmodSync(join(scratch, 'gh'), 0o755);

const SCRIPT = resolve(dirname(fileURLToPath(import.meta.url)), '../require-ci-checks.mjs');
const NONE = `[{"id":7,"name":"somebody else's ruleset","enforcement":"active"}]\\n`;
const PRESENT = `[{"id":7,"name":"somebody else's ruleset"},{"id":42,"name":"${RULESET_NAME}"}]\\n`;

let runs = 0;
/** Runs the real script against the stub; `calls` is every gh invocation as {args, body}. */
function run(args, rulesets, env = {}) {
  const log = join(scratch, `calls-${++runs}`);
  writeFileSync(log, '');
  const r = spawnSync(process.execPath, [SCRIPT, ...args], {
    encoding: 'utf8',
    env: { ...process.env, PATH: `${scratch}${delimiter}${process.env.PATH}`, GH_LOG: log, GH_RULESETS: rulesets, ...env },
  });
  const calls = readFileSync(log, 'utf8').split('\n').filter(Boolean).map((line) => {
    const [head, ...rest] = line.split('\t');
    return { args: head.split('|'), body: rest.join('\t') || null };
  });
  return { ...r, calls };
}

test('the default is a dry run: it lists the rulesets and prints the plan and payload', () => {
  const r = run([], PRESENT);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls[0].args, ['api', '--method', 'GET', `repos/${DEFAULT_REPO}/rulesets`, '--paginate']);
  assert.equal(r.calls.length, 1, 'nothing is sent');
  assert.match(r.stdout, /Dry run: nothing was sent/);
  assert.match(r.stdout, new RegExp(`Plan: update ruleset #42 on ${DEFAULT_REPO.replace('/', '\\/')}`));
  assert.deepEqual(JSON.parse(r.stdout.slice(r.stdout.indexOf('{'), r.stdout.lastIndexOf('}') + 1)), buildRulesetPayload());
});

test('a dry run against a repository without the ruleset plans a create', () => {
  const r = run(['--repo', 'owner/name'], NONE);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.calls[0].args[3], 'repos/owner/name/rulesets');
  assert.match(r.stdout, /Plan: create a ruleset on owner\/name/);
});

test('--apply posts the payload when the ruleset is missing', () => {
  const r = run(['--apply'], NONE);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls[1].args, ['api', '--method', 'POST', `repos/${DEFAULT_REPO}/rulesets`, '--input', '-']);
  assert.deepEqual(JSON.parse(r.calls[1].body), buildRulesetPayload());
  assert.match(r.stdout, /Applied as ruleset #42 \(active\)/);
});

test('--apply puts the same payload to the existing ruleset id', () => {
  const r = run(['--apply', '--repo', 'owner/name'], PRESENT);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.calls[1].args, ['api', '--method', 'PUT', 'repos/owner/name/rulesets/42', '--input', '-']);
  assert.deepEqual(JSON.parse(r.calls[1].body), buildRulesetPayload());
});

test('a failing ruleset listing stops the script before anything is sent', () => {
  const r = run(['--apply'], PRESENT, { GH_LIST_FAILS: '1' });
  assert.notEqual(r.status, 0);
  assert.equal(r.calls.length, 1, 'no POST or PUT follows a failed read');
  assert.match(r.stderr, /gh: no token/);
});

test('--help prints the usage and exits', () => {
  const r = run(['--help'], NONE);
  assert.equal(r.status, 2, r.stderr);
  assert.equal(r.calls.length, 0);
  assert.match(r.stdout, /usage: node scripts\/require-ci-checks\.mjs/);
});

test('an unknown argument is refused before gh runs', () => {
  const r = run(['--wat'], NONE);
  assert.equal(r.status, 2, r.stderr);
  assert.equal(r.calls.length, 0);
  assert.match(r.stderr, /unknown argument "--wat"/);
});

test('a --repo without an owner\/name pair is refused before gh runs', () => {
  const r = run(['--repo', 'owner'], NONE);
  assert.equal(r.status, 2, r.stderr);
  assert.equal(r.calls.length, 0);
  assert.match(r.stderr, /--repo wants an owner\/name pair/);
});
