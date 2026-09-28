import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { buildOptions, runAgent } from '../runner.mjs';
import { AsyncQueue } from '../runner.mjs';
import {
  defaultPolicy,
  evaluateExecPolicy,
  execPolicyLogLine,
  execPolicyQuestion,
  execPolicyReason,
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
  const policy = loadExecPolicy({});
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
  assert.deepEqual(emptied.layers.map((layer) => [layer.name, layer.rules.length]), [['default', 3], ['install', 0]]);
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
  assert.equal(defaultPolicy().rules.length, 3);
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
    const { options } = buildOptionsOff({}, { execPolicy: policy });
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

/** A minimal fake SDK `query`, in the shape fake-sdk.test.mjs uses. */
function fakeQuery(turn) {
  return ({ prompt, options }) =>
    (async function* () {
      for await (const message of prompt) yield* turn(options);
    })();
}
