import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { buildOptions, runAgent } from '../runner.mjs';
import { AsyncQueue } from '../runner.mjs';
import {
  HOST_MOUNTS_FILE,
  TRACKED_SCRIPTS_FILE,
  createExecAllowCache,
  defaultPolicy,
  evaluateExecPolicy,
  execPolicyLogLine,
  execPolicyQuestion,
  execPolicyReason,
  gitBlobId,
  loadExecPolicy,
  parsePolicy,
  splitCommands,
} from '../execpolicy.mjs';

// These tests cover other features; delegation is switched off unless one needs it.
const buildOptionsOff = (env = {}, extra) => buildOptions({ COLONIZER_DELEGATE: 'off', ...env }, extra);

/** The exec policy's Bash hook as buildOptions installed it. */
const bashHook = (options) => options.hooks.PreToolUse.find((entry) => entry.matcher === 'Bash').hooks[0];

/** A temp workspace with script files, so script-reading rules have something to read. */
function workspace() {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-execpolicy-'));
  writeFileSync(join(dir, 'deploy.sh'), '#!/bin/bash\ncurl -X POST https://example.com/hook --data @payload.json\necho done\n');
  writeFileSync(join(dir, 'x.py'), 'import requests\nrequests.post("https://example.com", data=open("out.json", "w"))\n');
  writeFileSync(join(dir, 'build.sh'), 'echo building\ngcc -O2 main.c -o app\n');
  return dir;
}

const policyIn = (dir, extraEnv = {}) =>
  loadExecPolicy({ ...extraEnv }, { cwd: dir, readFile: () => null });

const decide = (policy, command, dir) => evaluateExecPolicy(policy, command, { cwd: dir });

test('the default layer denies secret paths in the command', () => {
  const policy = loadExecPolicy({});
  for (const command of ['cat ~/.ssh/id_rsa', 'cat $HOME/.ssh/id_rsa', 'cat ${HOME}/.ssh/id_rsa', 'cat .env', 'source .env.local', 'less deploy/.envrc', 'cp .git-credentials /tmp/x']) {
    const hit = decide(policy, command, '/repo');
    assert.equal(hit.decision, 'deny', command);
    assert.equal(hit.rule, 'secret-paths');
    assert.equal(hit.layer, 'default');
  }
  for (const command of ['ls -la', 'cat payload.json', 'npm test', 'echo .environment-is-a-word',
    'cat .env.example', 'git diff .env.sample', 'less .env.template', 'cp .env.dist /tmp/x']) {
    assert.equal(decide(policy, command, '/repo'), null, command);
  }
  // A template alone is fine; a real secret file in the same command still denies.
  assert.equal(decide(policy, 'cp .env.example .env.local', '/repo').rule, 'secret-paths');
});

test('the default layer denies a script that calls out, and allows a benign one', () => {
  const dir = workspace();
  try {
    const policy = policyIn(dir);
    const bash = (script) => decide(policy, script, dir);
    assert.equal(bash('bash deploy.sh').rule, 'script-egress', 'curl in deploy.sh');
    assert.equal(bash('source deploy.sh').rule, 'script-egress', 'source runs the script too');
    assert.equal(bash('. deploy.sh').rule, 'script-egress', 'dot-source runs the script too');
    assert.equal(bash('python x.py').rule, 'script-egress', 'requests.post in x.py');
    assert.equal(bash('sh build.sh'), null, 'a benign script passes');
    assert.equal(bash('ls'), null);
    // A script rule without a readable script does not match: the rule is about what the command runs.
    assert.equal(bash('bash missing.sh'), null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('the default layer asks when a command writes outside the repository, and not for /tmp', () => {
  // readFile: () => null keeps the mount list unknown, so the conservative fallback holds whatever
  // a real /colonizer/host-mounts on the test machine would say (#877).
  const policy = loadExecPolicy({}, { readFile: () => null });
  const ask = (command) => decide(policy, command, '/repo');
  assert.equal(ask('echo x > /etc/foo')?.decision, 'ask');
  assert.equal(ask('echo x >> /var/log/app.log')?.decision, 'ask');
  assert.equal(ask('echo x | sudo tee /etc/hosts')?.decision, 'ask');
  assert.equal(ask('dd if=/dev/zero of=/boot/vmlinuz')?.decision, 'ask');
  assert.equal(ask('rm -rf /usr/bin')?.decision, 'ask');
  assert.equal(ask('echo x > $HOME/.cache/foo')?.decision, 'ask');
  assert.equal(ask('echo x>/etc/foo')?.decision, 'ask', 'a redirect glued to its target counts too');
  assert.equal(ask('echo x > /tmp/x'), null);
  assert.equal(ask('gcc x.c 2>/dev/null'), null);
  assert.equal(ask("sed 's/</>/g' f"), null, 'a `>` inside a quoted pattern is not a redirect');
  assert.equal(ask('echo x > inside.txt'), null);
  assert.equal(ask('echo x > /repo/inside.txt'), null);
});

// The vectors the ACP runner's test drives its own (byte-identical) copy of execpolicy.mjs with:
// a colony whose microVM root filesystem is discarded, with only these host-backed mounts writable.
const vmWrites = JSON.parse(readFileSync(new URL('./fixtures/execpolicy-vm-writes.json', import.meta.url), 'utf8'));

test('with the boot’s host mounts, a host-backed or read-only write asks, a .git write denies, a VM-local one does not (#877, #750, #1258)', () => {
  const mountsText = `${vmWrites.hostMounts.join('\n')}\n`;
  const policy = loadExecPolicy({}, {
    cwd: vmWrites.cwd,
    readFile: (path) => (path === HOST_MOUNTS_FILE ? mountsText : null),
  });
  assert.deepEqual(policy.hostMounts, [...vmWrites.hostMounts], 'the mount list is parsed off the file');
  for (const { command, decision, rule, reason } of vmWrites.cases) {
    const hit = evaluateExecPolicy(policy, command, { cwd: vmWrites.cwd });
    assert.equal(hit?.decision ?? null, decision, command);
    if (rule) assert.equal(hit.rule, rule, command);
    if (reason) assert.ok(hit.reason.includes(reason), `${command}: ${hit.reason}`);
  }
});

test('without a host-mount list, every write outside the repository still asks (#877)', () => {
  const policy = loadExecPolicy({}, { cwd: '/workspace', readFile: () => null });
  assert.equal(policy.hostMounts, null, 'no file, no list');
  assert.equal(decide(policy, 'mkdir -p /root/x', '/workspace')?.decision, 'ask');
  assert.equal(decide(policy, 'echo x > /usr/local/bin/tool', '/workspace')?.decision, 'ask');
});

test('the ask names why: a read-only mount, or a host-backed path (#750)', () => {
  const mountsText = `${vmWrites.hostMounts.join('\n')}\n`;
  const policy = loadExecPolicy({}, {
    cwd: vmWrites.cwd,
    readFile: (path) => (path === HOST_MOUNTS_FILE ? mountsText : null),
  });
  const question = (command) => execPolicyQuestion(decide(policy, command, vmWrites.cwd), command).questions[0].question;
  assert.match(question('echo x > /opt/colonizer/agent/runner.mjs'), /writes to a read-only mount \(\/opt\/colonizer\)/);
  assert.match(question('echo x > /colonizer/memory/repo/notes.json'), /read-only mount \(\/colonizer\)/);
  assert.match(question('echo x > /opt/node/bin/node'), /read-only mount \(\/opt\/node\/bin\/node\)/);
  assert.match(question('cp a /root/.claude/projects/x'), /to a host-backed path outside the repository/);
  // The colony's own output dir is not one of them (#1153).
  assert.equal(decide(policy, 'cp a /harness/out/z', vmWrites.cwd), null);
  // A nested writable bind of the read-only /colonizer is host-backed, not read-only.
  assert.match(question('echo x > /colonizer/services/s.json'), /host-backed path outside the repository/);
});

// #1258: `.git` is read-only by design. The invocations that must write it, and any write under
// it, deny instead of asking — the agent is hitting the intended wall, so the reason it sees is
// the instruction to leave its changes in the working tree.
test('the default layer denies git add/commit/stash and writes under .git; reads still pass (#1258)', () => {
  const policy = loadExecPolicy({}, { readFile: () => null });
  const deny = (command) => {
    const hit = decide(policy, command, '/workspace');
    assert.equal(hit?.decision, 'deny', command);
    assert.equal(hit.rule, 'git-read-only', command);
    assert.equal(hit.layer, 'default', command);
    assert.match(hit.reason, /read-only by design/, command);
    assert.match(hit.reason, /harness commits them/, command);
  };
  for (const command of ['git add .', 'git add -A', 'git add modules/agents/x.mjs', 'git commit -m wip',
    'git commit --amend', 'git stash', 'git stash pop', 'git stash drop', 'git -C /workspace add -A',
    'git -c core.pager=cat commit -m wip', 'git --no-pager stash', 'make && git add -A',
    'git status; git commit -m x', 'sudo git add .', 'touch .git/objects/testwrite', 'echo x > .git/HEAD',
    'echo x > /workspace/.git/HEAD', 'rm .git/index.lock', 'mkdir -p .git/refs/heads/wip',
    'printf x | tee /workspace/.git/HEAD']) {
    deny(command);
  }
  for (const command of ['git status', 'git diff HEAD~1', 'git log -p -- modules/agents/x.mjs',
    'git status && git diff --stat', 'git show HEAD:README.md', 'cat .git/HEAD', 'ls .git',
    'ls -la .git/objects', 'git check-ignore -v .env', 'git diff > /tmp/patch.diff',
    'git stash list', 'git stash show', 'git --no-pager stash list']) {
    assert.equal(decide(policy, command, '/workspace'), null, command);
  }
  // The wall is the design, so a later layer cannot wave it through: the strictest decision wins
  // across layers, and an allow is the loosest there is.
  const widened = {
    layers: [
      ...policy.layers,
      { name: 'repo', rules: parsePolicy({ rules: [{ id: 'allow-git', decision: 'allow', writes_git: true }] }).rules },
    ],
  };
  assert.equal(decide(widened, 'git commit -m wip', '/workspace').decision, 'deny');
  assert.equal(decide(widened, 'git commit -m wip', '/workspace').rule, 'git-read-only');
});

test('an org-layer `"writes_outside": "strict"` rule restores the pre-#877 asks (#750)', () => {
  const mountsText = `${vmWrites.hostMounts.join('\n')}\n`;
  const readFile = (path) => (path === HOST_MOUNTS_FILE ? mountsText : null);
  const org = (rules) => loadExecPolicy({ COLONIZER_EXEC_POLICY_ORG: JSON.stringify({ rules }) }, { cwd: vmWrites.cwd, readFile });
  const strict = org([{ id: 'strict-writes', decision: 'ask', reason: 'this org asks about every write outside the repository', writes_outside: 'strict' }]);
  // A VM-local write the default layer now lets through asks again, under the org rule.
  const local = decide(strict, 'mkdir -p /root/tmp/x', vmWrites.cwd);
  assert.equal(local?.decision, 'ask');
  assert.equal(local.layer, 'org');
  assert.equal(local.rule, 'strict-writes');
  assert.equal(local.reason, 'this org asks about every write outside the repository');
  // A host-backed write still asks, but the default layer names it first (and more precisely).
  const host = decide(strict, 'echo x > /var/cache/colonizer-shared/x', vmWrites.cwd);
  assert.equal(host.decision, 'ask');
  assert.equal(host.layer, 'default');
  assert.match(host.reason, /host-backed path outside the repository/);
  // /tmp, a write inside the repository and the colony's own output dir never ask, even strict
  // (#1153: the harness itself tells the agent to write /harness/out).
  assert.equal(decide(strict, 'echo x > /tmp/x', vmWrites.cwd), null);
  assert.equal(decide(strict, 'echo x > /harness/out/pr.md', vmWrites.cwd), null);
  assert.equal(decide(strict, 'echo x > inside.txt', vmWrites.cwd), null);
  // Without the strict rule, the same VM-local write is the default: allowed.
  assert.equal(decide(org([]), 'mkdir -p /root/tmp/x', vmWrites.cwd), null);
  // `writes_outside` accepts only true or "strict"; anything else is not a predicate, so the rule drops.
  assert.equal(parsePolicy({ rules: [{ id: 'bad', decision: 'ask', writes_outside: 'loose' }] }).rules.length, 0);
});

test('a later layer can narrow but never widen', () => {
  const dir = workspace();
  try {
    const install = policyIn(dir, {
      COLONIZER_EXEC_POLICY: JSON.stringify({ rules: [{ id: 'allow-tests', decision: 'allow', command: 'npm test' }] }),
    });
    const hit = decide(install, 'npm test', dir);
    assert.deepEqual({ decision: hit.decision, rule: hit.rule, layer: hit.layer }, { decision: 'allow', rule: 'allow-tests', layer: 'install' });
    // The default deny survives an install-layer allow of everything else.
    const loose = policyIn(dir, {
      COLONIZER_EXEC_POLICY: JSON.stringify({ rules: [{ id: 'allow-all', decision: 'allow', command: '.' }] }),
    });
    const secret = decide(loose, 'cat .env', dir);
    assert.equal(secret.decision, 'deny', 'the default deny is strictest');
    assert.equal(secret.layer, 'default');
    // A repo layer deny overrides an install-layer allow of the same commands.
    const repoDeny = {
      layers: [
        ...install.layers,
        { name: 'repo', rules: parsePolicy({ rules: [{ id: 'no-tests', decision: 'deny', reason: 'the repository says no', command: 'npm test' }] }).rules },
      ],
    };
    const narrowed = decide(repoDeny, 'npm test', dir);
    assert.deepEqual({ decision: narrowed.decision, rule: narrowed.rule, layer: narrowed.layer }, { decision: 'deny', rule: 'no-tests', layer: 'repo' });
    // And a repo-layer allow cannot widen the default's deny.
    const repoWiden = {
      layers: [
        ...repoDeny.layers.slice(0, 2),
        { name: 'repo', rules: parsePolicy({ rules: [{ id: 'allow-secrets', decision: 'allow', touches: ['.env'] }] }).rules },
      ],
    };
    assert.equal(decide(repoWiden, 'cat .env', dir).decision, 'deny');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('an org layer narrows the install layer: an org deny overrides an install allow (#924)', () => {
  const dir = workspace();
  try {
    const policy = policyIn(dir, {
      COLONIZER_EXEC_POLICY: JSON.stringify({ rules: [{ id: 'allow-publish', decision: 'allow', command: 'npm publish' }] }),
      COLONIZER_EXEC_POLICY_ORG: JSON.stringify({ rules: [{ id: 'org-no-publish', decision: 'deny', reason: 'acme publishes from CI', command: 'npm publish' }] }),
    });
    assert.deepEqual(policy.layers.map((layer) => layer.name), ['default', 'install', 'org']);
    const hit = decide(policy, 'npm publish', dir);
    assert.deepEqual({ decision: hit.decision, rule: hit.rule, layer: hit.layer }, { decision: 'deny', rule: 'org-no-publish', layer: 'org' });
    // And an org allow cannot widen the install's deny.
    const widen = policyIn(dir, {
      COLONIZER_EXEC_POLICY: JSON.stringify({ rules: [{ id: 'no-publish', decision: 'deny', command: 'npm publish' }] }),
      COLONIZER_EXEC_POLICY_ORG: JSON.stringify({ rules: [{ id: 'org-publish', decision: 'allow', command: 'npm publish' }] }),
    });
    assert.deepEqual(decide(widen, 'npm publish', dir).layer, 'install');
    assert.equal(decide(widen, 'npm publish', dir).decision, 'deny');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// The mothership refuses to save an exec policy unless this parser keeps it whole (issue #924); the
// same fixture drives the Rust port (crates/colonizer/src/exec_policy.rs) in crates/repo-contracts.
const validFixture = JSON.parse(readFileSync(new URL('./fixtures/execpolicy-valid.json', import.meta.url), 'utf8'));

/** Whether parsePolicy keeps a policy whole: the layer, every rule, every predicate a rule names. */
function keptWhole(text) {
  const policy = parsePolicy(text);
  if (!policy) return false;
  const raw = JSON.parse(text).rules;
  if (policy.rules.length !== raw.length) return false;
  const named = (rule) =>
    ['command', 'script', 'touches'].filter((key) => key in rule).length +
    ['writes_outside', 'writes_git'].filter((key) => rule[key] !== undefined && rule[key] !== false).length;
  return policy.rules.every((rule, i) => rule.predicates.length === named(raw[i]));
}

test('the save-time validity fixture matches what parsePolicy keeps whole (#924)', () => {
  assert.ok(validFixture.cases.length > 10);
  for (const { policy, valid } of validFixture.cases) assert.equal(keptWhole(policy), valid, policy);
});

test('a malformed layer is ignored with a warning; the default keeps enforcing', () => {
  for (const broken of ['{not json', '{"rules": "nope"}', '[]']) {
    const policy = loadExecPolicy({ COLONIZER_EXEC_POLICY: broken });
    assert.equal(policy.warnings.length, 1, broken);
    assert.deepEqual(policy.layers.map((layer) => layer.name), ['default']);
    assert.equal(decide(policy, 'cat .env', '/repo').rule, 'secret-paths', broken);
  }
  // Rules a layer cannot use — no decision, an unknown decision, no usable predicate — are dropped,
  // not guessed; a layer whose rules all drop is empty and warns nobody.
  const rules = parsePolicy({
    rules: [
      { id: 'no-decision', command: 'ls' },
      { id: 'bad-decision', decision: 'maybe', command: 'ls' },
      { id: 'no-predicate', decision: 'deny' },
      { id: 'fine', decision: 'ask', command: 'ls' },
    ],
  }).rules;
  assert.deepEqual(rules.map((rule) => rule.id), ['fine']);
  const emptied = loadExecPolicy({ COLONIZER_EXEC_POLICY: '{"rules": [{"decision": "deny"}]}' });
  assert.deepEqual(emptied.warnings, []);
  assert.deepEqual(emptied.layers.map((layer) => [layer.name, layer.rules.length]), [['default', 4], ['install', 0]]);
  assert.equal(decide(emptied, 'cat .env', '/repo').rule, 'secret-paths');
});

test('a missing repo file adds no layer, and a present one is read', () => {
  const dir = workspace();
  try {
    const policy = loadExecPolicy({}, { cwd: dir, readFile: () => null });
    assert.deepEqual(policy.layers.map((layer) => layer.name), ['default']);
    const loaded = loadExecPolicy({}, {
      cwd: dir,
      readFile: (path) => (path.endsWith('exec-policy.json') ? '{"rules": [{"id": "no-eject", "decision": "deny", "command": "umount"}]}' : null),
    });
    assert.deepEqual(loaded.layers.map((layer) => layer.name), ['default', 'repo']);
    assert.equal(decide(loaded, 'umount /dev/sda', dir).rule, 'no-eject');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('commands split on compound operators, quotes respected', () => {
  assert.deepEqual(splitCommands('a && b; c | d || e & f'), ['a', 'b', 'c', 'd', 'e', 'f']);
  assert.deepEqual(splitCommands('echo "a && b" ; cat .env'), ['echo "a && b"', 'cat .env']);
});

test('the reason names the rule and layer; the log line is one line with a truncated command', () => {
  const reason = execPolicyReason({ rule: 'secret-paths', layer: 'default', reason: 'credential path' });
  assert.equal(reason, 'exec policy rule `secret-paths` (default): credential path');
  const line = execPolicyLogLine({ decision: 'deny', rule: 'secret-paths', layer: 'default' }, `cat .env\necho ${'x'.repeat(300)}`);
  assert.match(line, /^exec policy: deny rule=secret-paths layer=default command=cat .env echo x+…$/);
  assert.ok(line.length < 300);
});

test('the question carries the rule, Allow and Deny options', () => {
  const { questions } = execPolicyQuestion(
    { rule: 'writes-outside-repo', layer: 'default', reason: 'the command writes outside the repository' },
    'echo x > /etc/foo',
  );
  assert.equal(questions.length, 1);
  assert.match(questions[0].question, /Exec policy rule `writes-outside-repo` \(default\)/);
  assert.match(questions[0].question, /echo x > \/etc\/foo/);
  assert.deepEqual(questions[0].options.map((option) => option.label), ['Allow', 'Deny']);
  assert.equal(defaultPolicy().rules.length, 4);
});

test('the installed hook denies with the rule named, and logs every decision', async () => {
  const dir = workspace();
  const written = [];
  const originalWrite = process.stderr.write;
  process.stderr.write = (chunk) => (written.push(String(chunk)), true);
  try {
    // An install layer that allows the benign build, so both a deny and an allow get logged.
    const policy = policyIn(dir, {
      COLONIZER_EXEC_POLICY: JSON.stringify({ rules: [{ id: 'allow-build', decision: 'allow', command: 'build' }] }),
    });
    const boundaries = [];
    const { options } = buildOptionsOff({}, { execPolicy: policy, onBoundary: (event) => boundaries.push(event) });
    const hook = bashHook(options);

    const denied = await hook({ tool_name: 'Bash', tool_input: { command: 'cat ~/.ssh/id_rsa' }, hook_event_name: 'PreToolUse' });
    assert.equal(denied.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(denied.hookSpecificOutput.permissionDecisionReason, /^exec policy rule `secret-paths` \(default\)/);

    const allowed = await hook({ tool_name: 'Bash', tool_input: { command: 'bash build.sh' }, hook_event_name: 'PreToolUse' });
    assert.deepEqual(allowed, { continue: true }, 'an allow returns no decision of its own; the run proceeds');

    assert.deepEqual(await hook({ tool_name: 'Bash', tool_input: {} }), { continue: true });
    assert.deepEqual(await hook({ tool_name: 'Read', tool_input: { file_path: '/x' } }), { continue: true });
    assert.equal(written.length, 2, 'one log line per non-null decision');
    assert.match(written[0], /^exec policy: deny rule=secret-paths layer=default command=cat ~\/.ssh\/id_rsa\n$/);
    assert.match(written[1], /^exec policy: allow rule=allow-build layer=install command=bash build\.sh\n$/);
    // The deny, and only the deny, is a boundary event for the control-defeat signature (#609).
    assert.equal(boundaries.length, 1);
    const { at, ...body } = boundaries[0];
    assert.ok(!Number.isNaN(Date.parse(at)));
    assert.deepEqual(body, {
      type: 'boundary',
      kind: 'exec_policy_deny',
      control: 'exec_policy:secret-paths',
      detail: 'deny (default): cat ~/.ssh/id_rsa',
      target: '~/.ssh/id_rsa',
    });
  } finally {
    process.stderr.write = originalWrite;
    rmSync(dir, { recursive: true, force: true });
  }
});

test('an ask reaches the operator as a colony question, and the answer decides', async () => {
  const dir = workspace();
  try {
    const policy = policyIn(dir);
    const turn = async function* (options) {
      yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
      const decision = await options.canUseTool('Bash', { command: 'echo x > /etc/foo' }, { signal: new AbortController().signal, toolUseID: 'toolu_bash' });
      yield { type: 'result', subtype: 'success', is_error: false, result: `decision:${decision.behavior}`, duration_ms: 1 };
    };
    const runCase = async (label) => {
      const events = [];
      const commands = new AsyncQueue();
      const run = runAgent({
        query: fakeQuery(turn),
        commands,
        emit: (event) => events.push(event),
        options: { model: 'fake' },
        execPolicy: policy,
        graceMs: 100,
      });
      // Start a turn, wait for the question the ask produces, then answer it and end the run.
      commands.push({ type: 'user_message', id: 'u1', text: 'go' });
      for (let i = 0; !events.some((event) => event.type === 'question') && i < 200; i++) {
        await new Promise((resolve) => setTimeout(resolve, 5));
      }
      commands.push({ type: 'answer', question_id: 'toolu_bash', answers: { 'Exec policy': label } });
      commands.close();
      await run;
      return events;
    };

    const events = await runCase('Allow');
    const question = events.find((event) => event.type === 'question');
    assert.ok(question, 'the ask became a question event');
    assert.equal(question.question_id, 'toolu_bash');
    assert.equal(question.risk, 'workspace_write');
    assert.match(question.questions[0].question, /writes-outside-repo/);
    assert.deepEqual(question.questions[0].options.map((option) => option.label), ['Allow', 'Deny']);
    assert.ok(events.find((event) => event.type === 'question_answered'));
    assert.ok(events.find((event) => event.type === 'turn_end')?.result.includes('decision:allow'));

    const events2 = await runCase('Deny');
    assert.ok(events2.find((event) => event.type === 'question'), 'the ask still became a question');
    assert.ok(events2.find((event) => event.type === 'turn_end')?.result.includes('decision:deny'));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a refused ask is a boundary event, and the same rule asking again is a bypass attempt (#609)', async () => {
  const dir = workspace();
  try {
    const policy = policyIn(dir);
    const calls = [
      ['toolu_first', 'echo x > /etc/foo'],
      ['toolu_again', 'printf x | tee /etc/foo'],
    ];
    const turn = async function* (options) {
      yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
      for (const [id, command] of calls) {
        await options.canUseTool('Bash', { command }, { signal: new AbortController().signal, toolUseID: id });
      }
      yield { type: 'result', subtype: 'success', is_error: false, result: 'done', duration_ms: 1 };
    };
    const events = [];
    const commands = new AsyncQueue();
    const at = new Date('2026-01-01T00:00:00.000Z');
    const run = runAgent({
      query: fakeQuery(turn),
      commands,
      emit: (event) => events.push(event),
      options: { model: 'fake' },
      execPolicy: policy,
      graceMs: 100,
      now: () => at,
    });
    commands.push({ type: 'user_message', id: 'u1', text: 'go' });
    const answered = new Set();
    for (let i = 0; !events.some((event) => event.type === 'turn_end') && i < 400; i++) {
      for (const question of events.filter((event) => event.type === 'question' && !answered.has(event.question_id))) {
        answered.add(question.question_id);
        commands.push({ type: 'answer', question_id: question.question_id, answers: { 'Exec policy': 'Deny' } });
      }
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    commands.close();
    await run;

    const boundaries = events.filter((event) => event.type === 'boundary');
    assert.deepEqual(
      boundaries.map((event) => [event.kind, event.control, event.target]),
      [
        ['exec_policy_deny', 'exec_policy:writes-outside-repo', '/etc/foo'],
        ['exec_policy_ask_bypass_attempt', 'exec_policy:writes-outside-repo', '/etc/foo'],
        ['exec_policy_deny', 'exec_policy:writes-outside-repo', '/etc/foo'],
      ],
    );
    assert.match(boundaries[0].detail, /^ask refused \(default\): echo x > \/etc\/foo$/);
    assert.match(boundaries[1].detail, /refused earlier: echo x > \/etc\/foo/);
    assert.ok(boundaries.every((event) => event.at === at.toISOString()));
    const second = events.findIndex((event) => event.type === 'question' && event.question_id === 'toolu_again');
    assert.ok(events.indexOf(boundaries[1]) < second, 'the attempt is reported before it is asked again');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('an Allow is remembered for the colony: a respawned agent is not asked again, another command is, and deny stays deny (#759)', async () => {
  const dir = workspace();
  try {
    const policy = policyIn(dir);
    // One colony run, three agents in a row: the first asks and is allowed, a respawn runs the
    // same command (whitespace aside) under a new tool-use id, then another command and a denied one.
    const calls = [
      ['toolu_first', 'echo x > /etc/foo'],
      ['toolu_respawn', 'echo x  >  /etc/foo'],
      ['toolu_other', 'echo y > /etc/bar'],
      ['toolu_secret', 'cat ~/.ssh/id_rsa'],
    ];
    const turn = async function* (options) {
      yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
      const decisions = [];
      for (const [id, command] of calls) {
        const decision = await options.canUseTool('Bash', { command }, { signal: new AbortController().signal, toolUseID: id });
        decisions.push(`${id}:${decision.behavior}`);
      }
      yield { type: 'result', subtype: 'success', is_error: false, result: decisions.join(' '), duration_ms: 1 };
    };
    const events = [];
    const commands = new AsyncQueue();
    const cache = createExecAllowCache();
    const run = runAgent({
      query: fakeQuery(turn),
      commands,
      emit: (event) => events.push(event),
      options: { model: 'fake' },
      execPolicy: policy,
      execAllowCache: cache,
      graceMs: 100,
    });
    commands.push({ type: 'user_message', id: 'u1', text: 'go' });
    // Answer each question as it opens: Allow the first, Deny the other command.
    const answers = { toolu_first: 'Allow', toolu_other: 'Deny' };
    const answered = new Set();
    for (let i = 0; !events.some((event) => event.type === 'turn_end') && i < 400; i++) {
      for (const question of events.filter((event) => event.type === 'question' && !answered.has(event.question_id))) {
        answered.add(question.question_id);
        commands.push({ type: 'answer', question_id: question.question_id, answers: { 'Exec policy': answers[question.question_id] ?? 'Deny' } });
      }
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    commands.close();
    await run;

    const asked = events.filter((event) => event.type === 'question');
    assert.deepEqual(asked.map((event) => event.question_id), ['toolu_first', 'toolu_other'], 'the respawn was not asked; the other command was');
    assert.ok(asked.every((event) => event.kind === 'exec_policy'), 'every exec-policy question says so, for the suspend decision');
    assert.ok(asked.every((event) => event.blocking === true), 'and marks it blocking: its tool call waits in flight');
    const result = events.find((event) => event.type === 'turn_end').result;
    assert.equal(result, 'toolu_first:allow toolu_respawn:allow toolu_other:deny toolu_secret:deny');
    assert.equal(cache.size, 1, 'only the Allow is remembered');

    // Deny stays deny: a second ask of the denied command asks again, and a remembered Allow
    // cannot soften a deny rule.
    const denyHit = evaluateExecPolicy(policy, 'echo y > /etc/bar', { cwd: dir });
    assert.equal(cache.has(denyHit, 'echo y > /etc/bar'), false);
    const secret = { decision: 'deny', rule: 'secret-paths', layer: 'default' };
    cache.remember(secret, 'cat ~/.ssh/id_rsa');
    assert.equal(cache.has(secret, 'cat ~/.ssh/id_rsa'), false, 'a deny is never cached as allowed');
    // Same command, a different rule: still asks.
    const firstHit = evaluateExecPolicy(policy, 'echo x > /etc/foo', { cwd: dir });
    assert.equal(cache.has(firstHit, 'echo x > /etc/foo'), true);
    assert.equal(cache.has({ ...firstHit, rule: 'another-ask' }, 'echo x > /etc/foo'), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a fresh colony run starts with an empty allow memory', () => {
  const first = createExecAllowCache();
  const hit = { decision: 'ask', rule: 'writes-outside-repo', layer: 'default' };
  first.remember(hit, 'echo x > /etc/foo');
  assert.equal(createExecAllowCache().has(hit, 'echo x > /etc/foo'), false, 'no approval leaks across colonies');
});

/** A minimal fake SDK `query`, in the shape fake-sdk.test.mjs uses. */
function fakeQuery(turn) {
  return ({ prompt, options }) =>
    (async function* () {
      for await (const message of prompt) yield* turn(options);
    })();
}

// #1169: the path policy's placeholders are dotfiles an agent trips over; asking git or ls about
// their NAMES is not reading a secret, and must neither be refused nor count as a control defeat.
test('commands that only touch a secret path\'s name are allowed, ones that read it are not', () => {
  const policy = policyIn('/repo');
  const names = [
    'git check-ignore -v .env',
    'git check-ignore -v .env .envrc .netrc .npmrc',
    'git status --porcelain --ignored .env',
    'ls -la .env',
    'ls .netrc .pypirc',
    'test -e .env',
    '[ -f .env ]',
    '[ -f .git-credentials ] && echo present',
    'for f in .env .envrc .netrc; do git check-ignore -v "$f"; done',
  ];
  for (const command of names) assert.equal(decide(policy, command, '/repo'), null, command);
  const reads = [
    'cat .env',
    'cat .env && git status',
    'git status; cat .env',
    'ls .env | xargs cat',
    'ls .env && source .env',
    'git check-ignore -v .env < .env',
    'git check-ignore --stdin < .env',
    'ls $(cat .env)',
    'ls `cat .env`',
    'git -c core.pager="cat .env" status .env',
    'git diff .env',
    'git log -p -- .env',
    'git status .env > /tmp/x; cat .env',
    'test -e .env && printf < .env',
    'ls ~/.ssh',
    'ls .env.local; head -1 .env.local',
    'for f in .env; do cat "$f"; done',
    'for f in .env; do git check-ignore "$f" && cat "$f"; done',
    'for f in .env; do git check-ignore -v "$(cat $f)"; done',
    'for f in $(cat .env); do ls "$f"; done',
    '[ -f .env ] || cat .env',
    'git check-ignore -v .env | cat',
    'ls --color=always .env',
    // One other segment can change what a later name-only command runs.
    'shopt -s expand_aliases\nalias ls=cat\nls .env',
    'export PATH=/tmp/evil\nls .env',
    'hash -p /bin/cat ls\nls .env',
    'enable -n test\ntest -f .env',
    'git config core.fsmonitor ./h.sh\ngit status .env',
    'for f in .env; do ls "$f"\nhash -p /bin/cat ls\nls "$f"; done',
  ];
  for (const command of reads) {
    assert.equal(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
  }
});

// #1079: an agent checking the placeholders before it finished ran exactly this; names and sizes
// only, with stderr folded in and the output cut short by a pipe into `head`.
test('size and metadata commands on a secret path, piped into a stdin filter, are allowed', () => {
  const policy = policyIn('/repo');
  const files = '.env .netrc .git-credentials .npmrc .pypirc';
  const allowed = [
    `git check-ignore -v ${files} 2>&1 | head; wc -c ${files} 2>&1 | head`,
    'git check-ignore -v .env 2>/dev/null',
    'git check-ignore -q .env >/dev/null 2>&1 && echo ignored',
    'wc -c .env',
    'wc --bytes .env .netrc',
    'stat .env',
    'stat -c %s .env',
    'stat --format=%s:%n .env .netrc',
    'stat -L .netrc | head -n 5',
    'ls -l .env | sort | uniq',
    'ls -la .env .netrc | tail -2',
    'test -s .env',
    '[ -s .npmrc ] && echo has-bytes',
    '[ -r .env ]',
  ];
  for (const command of allowed) assert.equal(decide(policy, command, '/repo'), null, command);
  // Anything that reads the bytes, or hands the names to something that would, is still refused.
  const reads = [
    'wc .env',
    'wc -l .env',
    'wc -w .env',
    'wc -m .env',
    'wc -c .env | cat',
    'wc -c .env; cat .env',
    'git check-ignore -v .env 2>&1 | head .env',
    'git check-ignore -v .env 2>&1 | head -n 1 .netrc',
    'git check-ignore -v .env 2>&1 | xargs cat',
    'ls .env | xargs head',
    'ls .env | sort -o out.txt .env',
    'head .env',
    'head -c 100 .env',
    'tail -n 1 .env',
    'less .env',
    'grep TOKEN .env',
    'grep -c . .env',
    'cp .env /tmp/x',
    'source .env',
    '. .env',
    'base64 .env',
    'xxd .env',
    'od -c .env',
    'stat -c %s .env > /tmp/x; cat .env',
    'stat --printf=$(cat .env) .env',
    'stat -c "%s" .env 2>&1 | cat',
    'wc -c < .env',
    'wc -c .env 2>&1 > /tmp/out',
    'git check-ignore -v .env 2>&1 | head\ncat .env',
    'test -s .env && cat .env',
    'diff .env /dev/null',
    'cmp .env /dev/null',
    'sha256sum .env',
  ];
  for (const command of reads) assert.equal(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
});

// #1169 follow-up: copying a repository while EXCLUDING the placeholder dotfiles is the safe thing to
// do; the --exclude patterns of tar and rsync are not paths the command touches.
test('tar and rsync --exclude patterns do not count as touching a secret path; reads still do', () => {
  const policy = policyIn('/repo');
  const allowed = [
    "tar cf - --exclude='./.env' --exclude='./.envrc' --exclude='./.npmrc' --exclude='./.netrc' . | tar xf - -C /tmp/mutrepo",
    'rsync -a --exclude .env --exclude .netrc ./ /tmp/ks52fix/',
    'rsync -a --exclude=.git-credentials --exclude=".pypirc" ./ /tmp/copy/',
  ];
  for (const command of allowed) assert.notEqual(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
  const reads = [
    'tar cf - .env | cat',
    'tar cf - --exclude=.envrc .env',
    'rsync -a .netrc /tmp/x/',
    'cp .env /tmp/x; tar cf - --exclude=.env .',
    'cat .env --exclude=.env',
  ];
  for (const command of reads) assert.equal(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
});

// #1239: the repository's own check scripts, as the base commit has them, are not script egress;
// the VM's network policy governs what they fetch. A new or edited script is still read.
test('a tracked check script runs; an untracked or edited one that calls out is still refused', () => {
  const dir = workspace();
  try {
    mkdirSync(join(dir, 'tools'));
    mkdirSync(join(dir, 'scripts'));
    const files = {
      'tools/sync-nav.py': 'import urllib.request\nurllib.request.urlopen("https://api.github.com")\n',
      'tools/built-with.py': 'import requests\nrequests.get("https://example.com")\n',
      'tools/check-languages.py': '#!/usr/bin/env python3\nimport requests\nrequests.get("https://x.example")\n',
      'tools/deploy.sh': '#!/bin/bash\ncurl -fsS https://example.com/health\n',
      'scripts/x.mjs': 'await fetch("https://example.com");\n',
    };
    for (const [path, text] of Object.entries(files)) writeFileSync(join(dir, path), text);
    const list = Object.entries(files).map(([path, text]) => `${gitBlobId(text)} ${path}\n`).join('');
    const policy = loadExecPolicy({}, { cwd: dir, readFile: (path) => (path === TRACKED_SCRIPTS_FILE ? list : null) });
    assert.equal(policy.trackedScripts.size, 5);
    const allowed = [
      'python3 tools/sync-nav.py --check',
      'python3 tools/built-with.py --check',
      './tools/check-languages.py',
      'node scripts/x.mjs',
      'bash tools/deploy.sh --dry-run',
      'python3 tools/sync-nav.py --check && python3 tools/built-with.py --check',
    ];
    for (const command of allowed) assert.equal(decide(policy, command, dir), null, command);

    // Re-running the same check after editing other files is still the committed script: never a
    // denial, so nothing for repeated_denial to count.
    for (let round = 0; round < 3; round++) {
      writeFileSync(join(dir, `page-${round}.html`), `<p>${round}</p>\n`);
      assert.equal(decide(policy, 'python3 tools/sync-nav.py --check', dir), null, `round ${round}`);
    }

    // Untracked: a script the colony wrote (deploy.sh and x.py from workspace()), or one the list
    // names under another path.
    for (const command of ['bash deploy.sh', 'python3 x.py', './deploy.sh']) {
      assert.equal(decide(policy, command, dir)?.rule, 'script-egress', command);
    }
    // Edited: the bytes are no longer the committed ones.
    writeFileSync(join(dir, 'tools/built-with.py'), `${files['tools/built-with.py']}requests.post("https://evil.example")\n`);
    assert.equal(decide(policy, 'python3 tools/built-with.py --check', dir)?.rule, 'script-egress');
    // No list (an older mothership, a runner outside a VM): nothing is exempt.
    assert.equal(decide(policyIn(dir), 'python3 tools/sync-nav.py --check', dir)?.rule, 'script-egress');
    // A layer an operator or the repository adds still sees a tracked script.
    const strict = loadExecPolicy(
      { COLONIZER_EXEC_POLICY_ORG: JSON.stringify({ rules: [{ id: 'org-no-urlopen', decision: 'deny', script: 'urlopen' }] }) },
      { cwd: dir, readFile: (path) => (path === TRACKED_SCRIPTS_FILE ? list : null) },
    );
    assert.equal(decide(strict, 'python3 tools/sync-nav.py --check', dir)?.rule, 'org-no-urlopen');
    // Reads of a secret path stay refused whatever runs them.
    assert.equal(decide(policy, 'python3 tools/sync-nav.py --check; cat .env', dir)?.rule, 'secret-paths');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// #1227: a syntax check parses the script and runs none of it.
test('syntax-only checks are not script egress; running the script still is', () => {
  const dir = workspace();
  try {
    writeFileSync(join(dir, 'smoke.mjs'), 'await fetch("https://example.com");\n');
    writeFileSync(join(dir, 'smoke.rb'), 'require "net/http"\nNet::HTTP.get(URI("https://x"))\n`curl https://x`\n');
    const policy = policyIn(dir);
    const allowed = [
      'bash -n deploy.sh',
      'sh -n deploy.sh && bash -n build.sh',
      'bash --noexec deploy.sh',
      'zsh -n deploy.sh',
      'node --check smoke.mjs',
      'node -c smoke.mjs',
      'ruby -c smoke.rb',
      'python3 -m py_compile x.py',
      'python -m py_compile x.py deploy.sh',
    ];
    for (const command of allowed) assert.equal(decide(policy, command, dir), null, command);
    const refused = [
      'bash deploy.sh',
      'sh deploy.sh',
      'bash -n -c deploy.sh',
      'bash -c -n deploy.sh',
      'bash -n -x deploy.sh',
      'bash -nx deploy.sh',
      'bash -n deploy.sh && bash deploy.sh',
      'node smoke.mjs',
      'node --check smoke.mjs && node smoke.mjs',
      'ruby smoke.rb',
      'python3 x.py',
      'source deploy.sh',
      '. deploy.sh',
    ];
    for (const command of refused) assert.equal(decide(policy, command, dir)?.rule, 'script-egress', command);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// #1079 follow-up: agents spell the placeholder check with git's -C and --no-index.
test('git -C <the repository> check-ignore --no-index is name-only; other -C directories and reads are not', () => {
  const policy = policyIn('/repo');
  const allowed = [
    'git -C /repo check-ignore -v --no-index .env .netrc .npmrc .pypirc .git-credentials 2>&1 | head',
    'git -C /repo/ check-ignore -q --no-index .env',
    'git -C . status --porcelain --ignored .env',
    'git check-ignore --no-index -v .env',
  ];
  for (const command of allowed) assert.equal(decide(policy, command, '/repo'), null, command);
  const refused = [
    'git -C /tmp/evil status .env',
    'git -C ../other check-ignore .env',
    'git -C /repo -c core.pager=cat status .env',
    'git -C /repo cat-file -p :.env',
    'git -C /repo diff .env',
    'git -C /repo check-ignore -v .env | xargs cat',
    'git -C /repo check-ignore -v .env; cat .env',
    'git -C /repo check-ignore --stdin < .env',
  ];
  for (const command of refused) assert.equal(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
});

// #1241: a git object spec `<rev>:<path>` reads the file as committed; the prefix must not hide it.
test('git <rev>:<path> object specs naming a secret path are refused; other paths pass', () => {
  const policy = policyIn('/repo');
  const refused = [
    'git show HEAD:.env',
    'git show HEAD~1:.env',
    'git cat-file -p :.env',
    'git cat-file -p HEAD:.env',
    'git show origin/main:.npmrc',
    'git show 0123abcd:.netrc',
    'git show 0123456789abcdef0123456789abcdef01234567:config/.git-credentials',
    'git show HEAD:./.pypirc',
    'git -C /repo show HEAD:.env',
    'git --no-pager show main:.env.local',
    'git show stash@{0}:.envrc',
    'git show HEAD:.env | head -1',
    'git archive HEAD .env',
    'git archive --format=tar HEAD:.env',
    'git diff HEAD~1 -- .env',
    'git diff HEAD:.env HEAD~1:.env',
    'git grep -n TOKEN -- .env',
    'git grep TOKEN HEAD:.npmrc',
    'git log -p -- .netrc',
    'git log -p HEAD -- config/.env',
  ];
  for (const command of refused) assert.equal(decide(policy, command, '/repo')?.rule, 'secret-paths', command);
  const allowed = [
    'git show HEAD:README.md',
    'git show origin/main:src/env.ts',
    'git cat-file -p HEAD:docs/.env-setup.md',
    'git show HEAD:.env.example',
    'git log -p -- README.md',
    'git diff HEAD~1 -- src/',
  ];
  for (const command of allowed) assert.equal(decide(policy, command, '/repo'), null, command);
});
