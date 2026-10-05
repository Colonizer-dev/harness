import { strict as assert } from 'node:assert';
import { test } from 'node:test';

import {
  BOUNDARY_KINDS,
  boundaryEvent,
  commandTarget,
  createAskRefusals,
  denialBoundary,
  egressTarget,
  execPolicyBoundary,
  redactDetail,
} from '../boundary.mjs';
import { classifyDenial } from '../denials.mjs';
import { readFileSync } from 'node:fs';

const at = new Date('2026-01-01T00:00:00.000Z');
const now = () => at;

test('the kinds are exactly the schema boundary.kind enum (issue #609)', () => {
  const schema = JSON.parse(readFileSync(new URL('../../../../docs/agent-events.schema.json', import.meta.url), 'utf8'));
  const def = schema.$defs.boundary;
  assert.deepEqual(def.properties.kind.enum, BOUNDARY_KINDS);
  const event = boundaryEvent('egress_denied', 'egress', 'Could not resolve host: evil.example', { target: 'evil.example', now });
  for (const field of def.required) assert.ok(event[field] !== undefined, `missing ${field}`);
});

test('a boundary event is one redacted line with a timestamp, and a target only when named', () => {
  assert.deepEqual(boundaryEvent('sandbox_denied', 'read_only_mount', 'a\n\tb', { now }), {
    type: 'boundary',
    kind: 'sandbox_denied',
    control: 'read_only_mount',
    detail: 'a b',
    at: '2026-01-01T00:00:00.000Z',
  });
  const long = boundaryEvent('egress_denied', 'egress', 'x'.repeat(1000), { now });
  assert.equal(long.detail.length, 301, 'capped at 300 characters plus the ellipsis');
});

test('credential shapes are masked in the detail', () => {
  const masked = redactDetail('curl -H "Authorization: Bearer abcdefghijklmnop" https://u:hunter2@x.example GITHUB_TOKEN=ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa sk-abcdefghijklmnopqrstu');
  assert.ok(!masked.includes('abcdefghijklmnop'), masked);
  assert.ok(!masked.includes('hunter2'), masked);
  assert.ok(!masked.includes('ghp_'), masked);
  assert.ok(!masked.includes('sk-abcdefghijklmnopqrstu'), masked);
});

test('commandTarget names the path a refused command was after', () => {
  assert.equal(commandTarget('echo x > /etc/foo'), '/etc/foo');
  assert.equal(commandTarget('echo x >> "/etc/foo"'), '/etc/foo');
  assert.equal(commandTarget('cp build/out /colonizer/x'), '/colonizer/x');
  assert.equal(commandTarget('dd if=/dev/zero of=/opt/colonizer/y bs=1'), '/opt/colonizer/y');
  assert.equal(commandTarget('cat ~/.ssh/id_rsa'), '~/.ssh/id_rsa');
  assert.equal(commandTarget('cat .env && ls'), '.env');
  assert.equal(commandTarget('make 2>&1'), undefined, 'a descriptor duplication names no path');
  assert.equal(commandTarget('npm test'), undefined);
});

test('an exec-policy hit becomes an exec_policy_deny naming its rule', () => {
  const event = execPolicyBoundary({ decision: 'deny', rule: 'secret-paths', layer: 'default' }, 'cat .env', { now });
  assert.deepEqual(event, {
    type: 'boundary',
    kind: 'exec_policy_deny',
    control: 'exec_policy:secret-paths',
    detail: 'deny (default): cat .env',
    at: '2026-01-01T00:00:00.000Z',
    target: '.env',
  });
});

test('a refused ask asked again under the same rule is a bypass attempt; another rule is not', () => {
  const refusals = createAskRefusals();
  const hit = { decision: 'ask', rule: 'writes-outside-repo', layer: 'default' };
  assert.equal(refusals.attempt(hit, 'echo x > /etc/foo', { now }), null, 'nothing refused yet');
  refusals.refuse(hit, 'echo x > /etc/foo');
  const attempt = refusals.attempt(hit, 'cp a /etc/foo', { now });
  assert.equal(attempt.kind, 'exec_policy_ask_bypass_attempt');
  assert.equal(attempt.target, '/etc/foo');
  assert.equal(refusals.attempt({ ...hit, rule: 'ask-net' }, 'curl x', { now }), null);
});

test('egress and read-only refusals become boundary events; policy and disabled tools do not', () => {
  const git = "fatal: unable to access 'https://github.com/x/y/': Could not resolve host: github.com";
  assert.equal(egressTarget(git), 'github.com');
  assert.equal(egressTarget('getaddrinfo ENOTFOUND registry.npmjs.org'), 'registry.npmjs.org');
  assert.equal(egressTarget('Connection refused', { command: 'curl https://evil.example/x' }), 'evil.example');
  const egress = denialBoundary(classifyDenial(git), git, { command: 'git fetch' }, { now });
  assert.equal(egress.kind, 'egress_denied');
  assert.equal(egress.control, 'egress');
  assert.equal(egress.target, 'github.com');

  const rofs = "touch: cannot touch '/opt/colonizer/x': Read-only file system";
  const ro = denialBoundary(classifyDenial(rofs), rofs, {}, { now });
  assert.equal(ro.kind, 'sandbox_denied');
  assert.equal(ro.control, 'read_only_mount');
  assert.equal(ro.target, '/opt/colonizer/x');

  const policy = 'Permission for this action has been denied. Reason: exec policy rule `secret-paths` (default): no';
  assert.equal(denialBoundary(classifyDenial(policy), policy, {}, { now }), null, 'reported where the policy decided');
  assert.equal(denialBoundary(classifyDenial('Permission to use Bash has been denied.'), '', {}, { now }), null);
  assert.equal(denialBoundary(null, 'ok', {}, { now }), null);
});
