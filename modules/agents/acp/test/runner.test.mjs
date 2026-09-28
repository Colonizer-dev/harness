// Contract tests for the ACP runner: they boot the real runner.mjs as a child process and drive it
// over the colonizer-runner/1 protocol against test/fake-acp-agent.mjs standing in for the ACP
// agent (the custom-command setting is the seam). The happy path's events are also checked against
// the required fields of docs/agent-events.schema.json.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { clampOptions, commandText, confine, contentText, riskForKind, splitCommand, toolOutput } from '../runner.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const moduleDir = join(here, '..');
const fakeAcp = join(here, 'fake-acp-agent.mjs');
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The runner as a child process in a fresh workspace seeded with `files`, the fake ACP agent
 * driven by `script` (an object, or a function of the workspace path). */
function startRunner({ script: scripted = {}, env = {}, files = {} } = {}) {
  const workspace = mkdtempSync(join(tmpdir(), 'acp-ws-'));
  const scratch = mkdtempSync(join(tmpdir(), 'acp-test-'));
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(join(workspace, dirname(name)), { recursive: true });
    writeFileSync(join(workspace, name), content);
  }
  const scriptPath = join(scratch, 'script.json');
  const record = join(scratch, 'record.jsonl');
  writeFileSync(scriptPath, JSON.stringify(typeof scripted === 'function' ? scripted(workspace) : scripted));
  const child = spawn(process.execPath, [join(moduleDir, 'runner.mjs')], {
    cwd: workspace,
    env: {
      PATH: '/usr/bin:/bin',
      COLONIZER_ACP_AGENT: 'custom',
      COLONIZER_ACP_COMMAND: `${process.execPath} ${JSON.stringify(fakeAcp)}`,
      ACP_FAKE_SCRIPT: scriptPath,
      ACP_FAKE_RECORD: record,
      ...env,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const events = [];
  createInterface({ input: child.stdout, crlfDelay: Infinity }).on('line', (line) => {
    if (!line.trim()) return;
    try {
      events.push(JSON.parse(line));
    } catch {
      events.push({ type: 'unparseable', line });
    }
  });
  child.stderr.setEncoding('utf8');

  const send = (command) => child.stdin.write(`${JSON.stringify(command)}\n`);
  const poll = async (check, what, timeoutMs) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const found = check();
      if (found) return found;
      if (child.exitCode !== null || Date.now() > deadline) {
        assert.fail(`timed out waiting for ${what}; runner exit ${child.exitCode}; events: ${JSON.stringify(events)}`);
      }
      await sleep(25);
    }
  };
  const waitUntil = (check, what, timeoutMs = 20000) => poll(() => check(events), what, timeoutMs);
  const records = () => (existsSync(record) ? readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  /** Resolves once `check(records)` holds, with the full record file re-read afterwards. */
  const waitRecord = async (check, what, timeoutMs = 20000) => {
    await poll(() => check(records()), what, timeoutMs);
    return records();
  };
  let exitCode = null;
  child.on('exit', (code) => (exitCode = code));
  const waitExit = async (timeoutMs = 10000) => {
    const deadline = Date.now() + timeoutMs;
    while (exitCode === null && Date.now() < deadline) await sleep(10);
    return exitCode ?? 'timeout';
  };
  // What the runner answered to the fake's agent→client requests (fs, terminal, permission).
  const asks = (method) => records().filter((r) => r.asked === method);
  return { child, events, send, waitUntil, waitRecord, waitExit, records, asks, workspace: realpathSync(workspace) };
}

const stop = (runner) => (runner.send({ type: 'shutdown' }), runner.waitExit());
const first = (type) => (events) => events.find((e) => e.type === type);
const count = (type, n) => (events) => {
  const matches = events.filter((e) => e.type === type);
  return matches.length >= n ? matches[n - 1] : undefined;
};

// docs/agent-events.schema.json, reduced to the required fields per event type we emit.
const schema = JSON.parse(readFileSync(join(moduleDir, '..', '..', '..', 'docs', 'agent-events.schema.json'), 'utf8'));
const required = Object.fromEntries(Object.entries(schema.$defs ?? {}).map(([name, def]) => [name, def.required ?? []]));

function assertSchema(events) {
  for (const event of events) {
    for (const field of required[event.type] ?? []) {
      assert.ok(event[field] !== undefined, `${event.type} is missing the schema-required field "${field}"`);
    }
  }
}

test('the pure helpers: command split, risk, content text, option clamp, command text, confinement', () => {
  assert.deepEqual(splitCommand('gemini --experimental-acp'), ['gemini', '--experimental-acp']);
  assert.deepEqual(splitCommand(`node 'a b' "c d" x`), ['node', 'a b', 'c d', 'x']);
  for (const [kind, risk] of [['read', 'read_only'], ['search', 'read_only'], ['fetch', 'read_only'], ['think', 'read_only'], ['execute', 'workspace_write'], ['patch', 'workspace_write'], [undefined, 'workspace_write']]) {
    assert.equal(riskForKind(kind), risk, `${JSON.stringify(kind)} risk`);
  }
  for (const [block, text] of [[{ type: 'text', text: 'hi' }, 'hi'], [{ type: 'image', data: 'x' }, '[image]'], [{ type: 'audio', data: 'x' }, '[audio]'], [null, '']]) {
    assert.equal(contentText(block), text, `${block?.type ?? 'no'} block`);
  }
  assert.equal(toolOutput({ rawOutput: 'out', content: [{ type: 'content', content: { type: 'text', text: 'block' } }] }), 'out\nblock');
  assert.equal(clampOptions([{ optionId: 'a', name: 'Allow', kind: 'allow_once' }])[1].optionId, '__cancel__', 'a one-option card is padded with Cancel');
  assert.deepEqual(clampOptions([{ optionId: 'a' }, { optionId: 'b' }, { optionId: 'c' }, { optionId: 'd' }, { optionId: 'e' }]).map((o) => o.optionId), ['a', 'b', 'c', 'd']);
  for (const [call, command] of [
    [{ kind: 'execute', rawInput: { command: 'npm test' } }, 'npm test'],
    [{ kind: 'execute', rawInput: { command: ['bash', 'x.sh'] } }, 'bash x.sh'],
    [{ kind: 'execute', title: 'Deploy?' }, 'Deploy?'],
    [{ kind: 'execute', rawInput: { command: '   ' }, title: 't' }, 't'],
  ]) {
    assert.equal(commandText(call), command, `${JSON.stringify(call.rawInput ?? call.title)} command text`);
  }

  const root = mkdtempSync(join(tmpdir(), 'acp-root-'));
  assert.equal(confine(root, 'a/b.txt'), join(root, 'a/b.txt'));
  assert.equal(confine(root, `${root}/a/../c.txt`), join(root, 'c.txt'));
  assert.equal(confine(root, '../outside'), null, '../ escapes');
  assert.equal(confine(root, '/etc/hostname'), null, 'an absolute path outside escapes');
  symlinkSync('/etc/hostname', join(root, 'escape'));
  assert.equal(confine(root, 'escape'), null, 'a symlink out of the tree escapes');
});

test('acp/execpolicy.mjs is byte-identical to the claude-code original it is copied from', () => {
  const copy = readFileSync(join(moduleDir, 'execpolicy.mjs'));
  const original = readFileSync(join(moduleDir, '..', 'claude-code', 'execpolicy.mjs'));
  assert.ok(
    copy.equals(original),
    'modules/agents/acp/execpolicy.mjs has drifted from modules/agents/claude-code/execpolicy.mjs; the exec policy is one file in two places — change both together',
  );
});

test('acp/pathpolicy.mjs is byte-identical to the claude-code original it is copied from', () => {
  const copy = readFileSync(join(moduleDir, 'pathpolicy.mjs'));
  const original = readFileSync(join(moduleDir, '..', 'claude-code', 'pathpolicy.mjs'));
  assert.ok(
    copy.equals(original),
    'modules/agents/acp/pathpolicy.mjs has drifted from modules/agents/claude-code/pathpolicy.mjs; the path policy is one file in two places — change both together',
  );
});

test('handshake and prompt turns: initialize, session/new in the workspace, mapped events, queued messages', async (t) => {
  const runner = startRunner({
    script: { turns: { '*': { updates: [{ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'Hi' } }] } } },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'initial', text: 'one' });
  runner.send({ type: 'user_message', id: 'u-2', text: 'two' });
  await runner.waitUntil(count('turn_end', 2), 'both turns to finish');

  await runner.waitRecord((r) => r.filter((x) => x.method).length >= 3, 'the first prompt to reach the agent');
  const messages = runner.records().filter((x) => x.method);
  assert.equal(messages[0].method, 'initialize');
  assert.equal(messages[0].params.protocolVersion, 1);
  assert.deepEqual(messages[0].params.clientCapabilities, { fs: { readTextFile: true, writeTextFile: true }, terminal: true });
  assert.equal(messages[1].method, 'session/new');
  assert.equal(messages[1].params.cwd, runner.workspace, 'the session opens in the workspace');
  assert.deepEqual(messages[1].params.mcpServers, []);
  assert.equal(messages[2].method, 'session/prompt');
  assert.deepEqual(messages[2].params.prompt, [{ type: 'text', text: 'one' }]);

  assert.deepEqual(runner.events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(first('user_message')(runner.events), { type: 'user_message', id: 'initial', text: 'one' });
  const order = runner.events.map((e) => `${e.type}:${e.state ?? ''}`);
  assert.ok(order.indexOf('user_message:') < order.indexOf('status:working'), 'working follows the echo');
  assert.ok(order.indexOf('status:working') < order.indexOf('turn_end:'), 'the turn ends inside working');
  assert.ok(order.indexOf('turn_end:') < order.indexOf('status:idle', 1), 'idle follows the turn end');
  assert.deepEqual(runner.events.filter((e) => e.type === 'assistant_text_delta').map((e) => e.delta), ['Hi', 'Hi'], 'one delta per turn');
  assert.deepEqual(first('assistant_text')(runner.events), { type: 'assistant_text', message_id: 'msg-1', block_index: 0, text: 'Hi' });
  const turnEnd = first('turn_end')(runner.events);
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'Hi');
  assert.equal(turnEnd.cost_usd, null, 'ACP names no spend');
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assertSchema(runner.events);

  const code = await stop(runner);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(code, 0);
});

test('every session/update type maps (or is ignored) without breaking the turn', async (t) => {
  const runner = startRunner({
    script: {
      turns: {
        x: {
          updates: [
            { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'Hel' } },
            { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'lo' } },
            { sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: 'weighing it' } },
            { sessionUpdate: 'tool_call', toolCallId: 'call_1', title: 'Read file', kind: 'read', rawInput: { path: 'a.txt' } },
            { sessionUpdate: 'tool_call_update', toolCallId: 'call_1', status: 'in_progress' },
            { sessionUpdate: 'tool_call_update', toolCallId: 'call_1', status: 'completed', rawOutput: '{"lines":42}' },
            { sessionUpdate: 'tool_call_update', toolCallId: 'call_2', status: 'failed', content: [{ type: 'content', content: { type: 'text', text: 'boom' } }] },
            { sessionUpdate: 'plan', entries: [{ content: 'step one', priority: 'high', status: 'completed' }, { content: 'step two', priority: 'low', status: 'pending' }] },
            { sessionUpdate: 'user_message_chunk', content: { type: 'text', text: 'echo back?' } },
            { sessionUpdate: 'available_commands_update', commands: [{ name: 'help', description: '' }] },
            { sessionUpdate: 'current_mode_update', currentModeId: 'code' },
            { sessionUpdate: 'from_the_future' },
          ],
        },
      },
    },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'x' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(runner.events.filter((e) => e.type === 'assistant_text_delta').map((e) => e.delta), ['Hel', 'lo']);
  const thoughts = runner.events.filter((e) => e.type === 'thinking');
  assert.match(thoughts[0].text, /^Plan:\n- \[x\] step one\n- \[ \] step two$/, 'a plan update becomes a thinking checklist');
  assert.deepEqual(thoughts[1], { type: 'thinking', message_id: 'msg-1', block_index: 1, text: 'weighing it' });
  assert.deepEqual(first('tool_call')(runner.events), { type: 'tool_call', message_id: 'msg-1', tool_call_id: 'call_1', name: 'Read file', input: { path: 'a.txt' } });
  assert.deepEqual(runner.events.filter((e) => e.type === 'tool_result'), [
    { type: 'tool_result', tool_call_id: 'call_1', output: '{"lines":42}', is_error: false },
    { type: 'tool_result', tool_call_id: 'call_2', output: 'boom', is_error: true },
  ], 'an in_progress update emits no tool_result; a failed one is an error');
  assert.ok(runner.events.some((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('from_the_future')), 'an unknown update type is named');
  const turnEnd = first('turn_end')(runner.events);
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'Hello');
  assertSchema(runner.events);
  await stop(runner);
});

test('a permission request becomes a question; allow selects the option, Cancel and free text cancel, an interrupt answers nothing', async (t) => {
  const permission = (toolCallId, kind, title, options) => ({
    method: 'session/request_permission', params: { sessionId: 'sess-fake-1', toolCall: { toolCallId, kind, title }, options },
  });
  const twoOptions = [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }, { optionId: 'reject', name: 'Reject', kind: 'reject_once' }];
  const runner = startRunner({
    script: {
      turns: {
        p1: { asks: [permission('call_p1', 'execute', 'Run the tests?', twoOptions)] },
        p2: { asks: [permission('call_p2', 'read', 'Read it?', [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }])] },
        p3: { asks: [permission('call_p3', 'read', 'Proceed?', [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }])] },
        p4: { asks: [permission('call_p4', 'execute', 'Deploy?', twoOptions)], stopReason: 'cancelled' },
      },
    },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  // Turn 1: the user picks "Allow" → the agent hears that option selected.
  runner.send({ type: 'user_message', id: 'u-1', text: 'p1' });
  const question = await runner.waitUntil(first('question'), 'the question card');
  assert.equal(question.question_id, 'call_p1');
  assert.equal(question.message_id, 'msg-1');
  assert.equal(question.risk, 'workspace_write', 'an execute kind is workspace_write');
  assert.deepEqual(question.questions[0].options.map((o) => o.label), ['Allow', 'Reject']);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'waiting_for_answer'), 'waiting_for_answer');
  runner.send({ type: 'answer', question_id: 'call_p1', answers: { 'Run the tests?': 'Allow' }, response: null });
  await runner.waitUntil(first('question_answered'), 'the answered event');
  await runner.waitRecord((r) => r.filter((x) => x.asked).length >= 1, 'the permission reply');
  assert.deepEqual(runner.asks('session/request_permission')[0].response, { result: { outcome: { outcome: 'selected', optionId: 'allow' } } });
  await runner.waitUntil(count('turn_end', 1), 'the first turn to finish');

  // Turn 2: a single ACP option is padded to a 2-option card; a label the agent never offered
  // (free text included) answers cancelled.
  runner.send({ type: 'user_message', id: 'u-2', text: 'p2' });
  const second = await runner.waitUntil(count('question', 2), 'the second question');
  assert.equal(second.risk, 'read_only');
  assert.deepEqual(second.questions[0].options.map((o) => o.label), ['Allow', 'Cancel'], 'padded to two options');
  runner.send({ type: 'answer', question_id: 'call_p2', answers: { 'Read it?': 'something else' }, response: 'please just do it' });
  await runner.waitUntil(count('question_answered', 2), 'the second answer');
  await runner.waitRecord((r) => r.filter((x) => x.asked).length >= 2, 'the second reply');
  assert.deepEqual(runner.asks('session/request_permission')[1].response, { result: { outcome: { outcome: 'cancelled' } } });
  await runner.waitUntil(count('turn_end', 2), 'the second turn to finish');

  // Turn 3: picking the padded Cancel is also a cancelled outcome — it is no agent's option.
  runner.send({ type: 'user_message', id: 'u-3', text: 'p3' });
  const third = await runner.waitUntil(count('question', 3), 'the third question');
  assert.deepEqual(third.questions[0].options.map((o) => o.label), ['Allow', 'Cancel']);
  runner.send({ type: 'answer', question_id: 'call_p3', answers: { 'Proceed?': 'Cancel' }, response: null });
  await runner.waitUntil(count('question_answered', 3), 'the third answer');
  await runner.waitRecord((r) => r.filter((x) => x.asked).length >= 3, 'the third reply');
  assert.deepEqual(runner.asks('session/request_permission')[2].response, { result: { outcome: { outcome: 'cancelled' } } }, 'the padded Cancel cancels');
  await runner.waitUntil(count('turn_end', 3), 'the third turn to finish');

  // Turn 4: an interrupt while the card is open cancels it and cancels the turn.
  runner.send({ type: 'user_message', id: 'u-4', text: 'p4' });
  await runner.waitUntil(count('question', 4), 'the fourth question');
  runner.send({ type: 'interrupt' });
  await runner.waitUntil(count('turn_end', 4), 'the interrupted turn to end');
  await runner.waitRecord((r) => r.filter((x) => x.asked).length >= 4, 'the cancelled reply');
  assert.deepEqual(runner.asks('session/request_permission')[3].response, { result: { outcome: { outcome: 'cancelled' } } });
  assert.equal(runner.events.filter((e) => e.type === 'question_answered').length, 3, 'an interrupt answers nothing');
  assert.ok(runner.events.some((e) => e.type === 'log' && e.message.includes('free-text reply')), 'a free-text answer to an ACP option is named');
  assert.ok(runner.records().some((r) => r.method === 'session/cancel'), 'the agent hears session/cancel');
  const interrupted = count('turn_end', 4)(runner.events);
  assert.equal(interrupted.is_error, true);
  assert.equal(interrupted.result, 'interrupted by the user');
  await stop(runner);
});

test('the exec policy answers execute calls: deny and allow never open a card, an ask names the rule on it', async (t) => {
  const permission = (toolCallId, toolCall, options) => ({
    method: 'session/request_permission',
    params: { toolCall: { toolCallId, kind: 'execute', ...toolCall }, options },
  });
  const twoOptions = [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }, { optionId: 'reject', name: 'Reject', kind: 'reject_once' }];
  const policyEnv = (rules) => ({ COLONIZER_EXEC_POLICY: JSON.stringify({ rules }) });

  // The default layer's secret-paths deny: straight to the reject option, no card, one log line.
  const stderrChunks = [];
  const deny = startRunner({
    script: {
      turns: {
        d1: { asks: [permission('call_d1', { rawInput: { command: 'cat ~/.ssh/id_rsa' } }, twoOptions)] },
        d2: { asks: [permission('call_d2', { rawInput: { command: 'cat ~/.ssh/id_rsa' } }, [twoOptions[0]])] },
        d3: { asks: [permission('call_d3', { kind: 'read', rawInput: { command: 'cat ~/.ssh/id_rsa' }, title: 'Read it?' }, twoOptions)] },
      },
    },
  });
  deny.child.stderr.on('data', (chunk) => stderrChunks.push(chunk));
  t.after(() => deny.child.kill('SIGKILL'));

  deny.send({ type: 'user_message', id: 'u-1', text: 'd1' });
  await deny.waitRecord((r) => r.filter((x) => x.asked).length >= 1, 'the denied ask to be answered');
  assert.deepEqual(deny.asks('session/request_permission')[0].response, { result: { outcome: { outcome: 'selected', optionId: 'reject' } } }, 'the deny answers the reject option');
  assert.ok(!deny.events.some((e) => e.type === 'question'), 'a denied command opens no card');
  await deny.waitUntil(count('turn_end', 1), 'the first turn to finish');
  assert.match(stderrChunks.join(''), /exec policy: deny rule=secret-paths layer=default command=cat ~\/\.ssh\/id_rsa/, 'the decision leaves one log line');

  // A deny with no reject option to pick — the padded Cancel is synthetic — answers cancelled.
  deny.send({ type: 'user_message', id: 'u-2', text: 'd2' });
  await deny.waitRecord((r) => r.filter((x) => x.asked).length >= 2, 'the second denied ask to be answered');
  assert.deepEqual(deny.asks('session/request_permission')[1].response, { result: { outcome: { outcome: 'cancelled' } } }, 'the padded Cancel never answers for the policy');
  await deny.waitUntil(count('turn_end', 2), 'the second turn to finish');

  // A non-execute kind is none of the policy's business: the card surfaces as before.
  deny.send({ type: 'user_message', id: 'u-3', text: 'd3' });
  const read = await deny.waitUntil(count('question', 1), 'the read kind to surface as a question');
  assert.equal(read.questions[0].question, 'Read it?', 'no policy text on a non-execute call');
  deny.send({ type: 'answer', question_id: 'call_d3', answers: { 'Read it?': 'Allow' }, response: null });
  await deny.waitRecord((r) => r.filter((x) => x.asked).length >= 3, 'the third ask to be answered');
  assert.deepEqual(deny.asks('session/request_permission')[2].response, { result: { outcome: { outcome: 'selected', optionId: 'allow' } } });
  await stop(deny);

  // An install allow rule: an argv-array command answered with the allow option, no card.
  const allow = startRunner({
    env: policyEnv([{ id: 'tests-allowed', decision: 'allow', command: '\\bnpm\\b' }]),
    script: { turns: { a1: { asks: [permission('call_a1', { rawInput: { command: ['npm', 'run', 'test'] } }, twoOptions)] } } },
  });
  t.after(() => allow.child.kill('SIGKILL'));
  allow.send({ type: 'user_message', id: 'u-1', text: 'a1' });
  await allow.waitRecord((r) => r.filter((x) => x.asked).length >= 1, 'the allowed ask to be answered');
  assert.deepEqual(allow.asks('session/request_permission')[0].response, { result: { outcome: { outcome: 'selected', optionId: 'allow' } } }, 'the allow answers the allow option');
  assert.ok(!allow.events.some((e) => e.type === 'question'), 'an allowed command opens no card');
  await stop(allow);

  // An install ask rule: the card carries the rule and its reason, then the usual answer flow.
  const ask = startRunner({
    env: policyEnv([{ id: 'ask-net', decision: 'ask', reason: 'network fetches wait for a human', command: '\\bcurl\\b' }]),
    script: { turns: { s1: { asks: [permission('call_s1', { title: 'curl -fsSL https://example.com' }, twoOptions)] } } },
  });
  t.after(() => ask.child.kill('SIGKILL'));
  ask.send({ type: 'user_message', id: 'u-1', text: 's1' });
  const card = await ask.waitUntil(first('question'), 'the ask decision to surface');
  assert.match(card.questions[0].question, /exec policy rule `ask-net` \(install\): network fetches wait for a human/, 'the rule rides the card');
  ask.send({ type: 'answer', question_id: 'call_s1', answers: { [card.questions[0].question]: 'Allow' }, response: null });
  await ask.waitRecord((r) => r.filter((x) => x.asked).length >= 1, 'the answered ask to be replied');
  assert.deepEqual(ask.asks('session/request_permission')[0].response, { result: { outcome: { outcome: 'selected', optionId: 'allow' } } }, 'the usual answer flow picks the option');
  await stop(ask);
});

test('fs requests read and write inside the workspace, with line/limit and parent creation', async (t) => {
  const runner = startRunner({
    files: { 'notes/a.txt': 'l1\nl2\nl3\n' },
    script: {
      turns: {
        fs: {
          asks: [
            { method: 'fs/read_text_file', params: { sessionId: 's', path: 'notes/a.txt' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: 'notes/a.txt', line: 2, limit: 1 } },
            { method: 'fs/write_text_file', params: { sessionId: 's', path: 'notes/b.txt', content: 'written' } },
            { method: 'fs/write_text_file', params: { sessionId: 's', path: 'new/deep/c.txt', content: 'nested' } },
          ],
        },
      },
    },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'fs' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  await runner.waitRecord((r) => r.filter((x) => x.asked === 'fs/read_text_file').length >= 2, 'the reads to land');
  const reads = runner.asks('fs/read_text_file');
  assert.deepEqual(reads[0].response, { result: { content: 'l1\nl2\nl3\n' } });
  assert.deepEqual(reads[1].response, { result: { content: 'l2' } }, '1-based line, one-line limit');
  await runner.waitRecord((r) => r.filter((x) => x.asked === 'fs/write_text_file').length >= 2, 'the writes to land');
  assert.deepEqual(runner.asks('fs/write_text_file')[0].response, { result: {} });
  assert.equal(readFileSync(join(runner.workspace, 'notes/b.txt'), 'utf8'), 'written');
  assert.equal(readFileSync(join(runner.workspace, 'new/deep/c.txt'), 'utf8'), 'nested', 'parents are created');
  await stop(runner);
});

test('fs requests on masked or protected paths report one path_policy event each (issue #647)', async (t) => {
  // The policy comes from the same bind list the boot mounts, via the override env the tests use.
  const policyFile = join(mkdtempSync(join(tmpdir(), 'acp-policy-')), 'path-policy');
  writeFileSync(policyFile, 'mask-file .env\nprotect .git/config\n');
  const runner = startRunner({
    env: { COLONIZER_PATH_POLICY: policyFile },
    files: { '.env': 'SECRET=1\n', '.git/config': '[core]\n', 'notes/a.txt': 'l1\n' },
    script: {
      turns: {
        fs: {
          asks: [
            { method: 'fs/read_text_file', params: { sessionId: 's', path: '.env' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: '.git/config' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: 'notes/a.txt' } },
            { method: 'fs/write_text_file', params: { sessionId: 's', path: '.git/config', content: 'x' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: '.env' } },
          ],
        },
      },
    },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'fs' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');
  // A read of a protected path is allowed, an unmasked file is none of the policy's business, and
  // a second attempt at the same path is not a second event. The replies are untouched either way.
  assert.deepEqual(
    runner.events.filter((e) => e.type === 'path_policy'),
    [
      { type: 'path_policy', access: 'read', policy: 'masked', path: '.env', tool: 'fs/read_text_file' },
      { type: 'path_policy', access: 'write', policy: 'protected', path: '.git/config', tool: 'fs/write_text_file' },
    ],
  );
  assert.equal(readFileSync(join(runner.workspace, '.env'), 'utf8'), 'SECRET=1\n', 'the mount empties masked files, not this runner');
  await stop(runner);
});

test('fs requests outside the workspace, oversized files and unknown methods are refused with JSON-RPC errors', async (t) => {
  const runner = startRunner({
    files: { 'big.bin': 'x'.repeat(17 * 1024 * 1024) },
    script: {
      turns: {
        nope: {
          asks: [
            { method: 'fs/read_text_file', params: { sessionId: 's', path: '../outside.txt' } },
            { method: 'fs/write_text_file', params: { sessionId: 's', path: '/etc/hostname', content: 'x' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: 'big.bin' } },
            { method: 'fs/read_text_file', params: { sessionId: 's', path: 'missing.txt' } },
            { method: 'terminal/nope', params: { sessionId: 's' } },
          ],
        },
      },
    },
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'nope' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  await runner.waitRecord((r) => r.filter((x) => x.asked).length >= 5, 'all five replies to land');
  const answers = runner.records().filter((x) => x.asked);
  for (const r of answers.slice(0, 2)) {
    assert.equal(r.response?.error?.code, -32602, `${r.asked} to ${r.params.path} is refused as an invalid request`);
    assert.match(r.response.error.message, /^refused: .* outside the workspace$/, 'the refusal names the escape');
  }
  assert.match(answers[2].response?.error?.message, /^refused: .* over the 16 MiB read cap$/, 'a 17 MiB file is refused, not buffered');
  assert.equal(answers[3].response?.error?.code, -32602, 'a missing file is a request error');
  assert.match(answers[3].response.error.message, /could not read/);
  assert.equal(answers[4].response.error.code, -32601);
  assert.match(answers[4].response.error.message, /method not found: terminal\/nope/);
  await stop(runner);
});

test('terminals run with a workspace cwd and cap their output with a truncated flag', async (t) => {
  const run = (code) => ({ command: process.execPath, args: ['-e', code] });
  const runner = startRunner({
    script: (workspace) => ({
      turns: {
        term: {
          asks: [
            { method: 'terminal/create', params: { sessionId: 's', ...run('process.stdout.write("hello terminal")') } },
            { method: 'terminal/wait_for_exit', params: { sessionId: 's', terminalId: 'term-1' } },
            { method: 'terminal/output', params: { sessionId: 's', terminalId: 'term-1' } },
            { method: 'terminal/create', params: { sessionId: 's', ...run('process.stdout.write("abcdefgh")'), outputByteLimit: 4 } },
            { method: 'terminal/wait_for_exit', params: { sessionId: 's', terminalId: 'term-2' } },
            { method: 'terminal/output', params: { sessionId: 's', terminalId: 'term-2' } },
            { method: 'terminal/create', params: { sessionId: 's', ...run('process.stdout.write(process.cwd())'), cwd: workspace } },
            { method: 'terminal/wait_for_exit', params: { sessionId: 's', terminalId: 'term-3' } },
            { method: 'terminal/output', params: { sessionId: 's', terminalId: 'term-3' } },
            { method: 'terminal/output', params: { sessionId: 's', terminalId: 'term-404' } },
          ],
        },
      },
    }),
  });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'term' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  await runner.waitRecord((r) => r.filter((x) => x.asked === 'terminal/output').length >= 4, 'all outputs to land');
  const outputs = runner.asks('terminal/output');
  assert.deepEqual(outputs[0].response, { result: { output: 'hello terminal', truncated: false, exitStatus: { exitCode: 0, signal: null } } });
  assert.equal(outputs[1].response.result.output, 'efgh', 'the cap keeps the tail');
  assert.equal(outputs[1].response.result.truncated, true);
  assert.equal(outputs[2].response.result.output, runner.workspace, 'cwd is honored inside the workspace');
  assert.match(outputs[3].response.error.message, /no terminal with id/);
  await runner.waitRecord((r) => r.filter((x) => x.asked === 'terminal/wait_for_exit').length >= 3, 'all exits to land');
  assert.deepEqual(runner.asks('terminal/wait_for_exit').map((r) => r.response.result?.exitCode), [0, 0, 0]);
  await stop(runner);
});

test('set_model rides session/set_model only when the agent advertised models', async (t) => {
  const advertised = startRunner({
    script: {
      models: { currentModelId: 'gemini-3-pro', availableModels: [{ modelId: 'gemini-3-pro', name: 'Gemini 3 Pro' }] },
      setModel: { 'no-such-model': true },
      turns: { '*': {} },
    },
  });
  t.after(() => advertised.child.kill('SIGKILL'));
  await advertised.waitUntil(first('model_changed'), 'the boot announcement');
  assert.deepEqual(first('model_changed')(advertised.events), { type: 'model_changed', model: 'gemini-3-pro', previous: null });

  // A model the agent refuses logs the refusal and keeps the current model.
  advertised.send({ type: 'set_model', model: 'no-such-model' });
  const failed = await advertised.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('failed')), 'the refusal warning');
  assert.match(failed.message, /no such model/);

  advertised.send({ type: 'set_model', model: 'gemini-3-flash' });
  await advertised.waitUntil(count('model_changed', 2), 'the switch announcement');
  assert.deepEqual(count('model_changed', 2)(advertised.events), { type: 'model_changed', model: 'gemini-3-flash', previous: 'gemini-3-pro' });
  await advertised.waitRecord((r) => r.filter((x) => x.method === 'session/set_model').length >= 2, 'both requests to reach the agent');
  assert.deepEqual(advertised.records().filter((x) => x.method === 'session/set_model').map((x) => x.params.modelId), ['no-such-model', 'gemini-3-flash']);
  await stop(advertised);

  const silent = startRunner({ script: { turns: { '*': {} } } });
  t.after(() => silent.child.kill('SIGKILL'));
  await silent.waitRecord((r) => r.some((x) => x.method === 'session/new'), 'the handshake');
  silent.send({ type: 'set_model', model: 'whatever' });
  const warn = await silent.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('set_model')), 'the warning');
  assert.match(warn.message, /did not advertise/);
  assert.ok(!silent.records().some((r) => r.method === 'session/set_model'), 'no request leaves for an agent without models');
  await stop(silent);
});

test('the agent dying mid-turn and while idle both end in a named error and exit 1', async (t) => {
  const midTurn = startRunner({ script: { turns: { '*': { updates: [{ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'par' } }], die: 3 } } } });
  t.after(() => midTurn.child.kill('SIGKILL'));
  midTurn.send({ type: 'user_message', id: 'u-1', text: 'doom' });
  const failed = await midTurn.waitUntil(first('turn_end'), 'the failed turn');
  assert.equal(failed.is_error, true);
  assert.match(failed.result, /died mid-turn/);
  const problem = await midTurn.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the death log');
  assert.match(problem.message, /ACP_AGENT_FAILED/);
  await midTurn.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'ACP_AGENT_FAILED'), 'the error status');
  assert.equal(await midTurn.waitExit(), 1, 'a dead agent is a nonzero exit');

  const whileIdle = startRunner({ script: { turns: { '*': { dieAfter: 3 } } } });
  t.after(() => whileIdle.child.kill('SIGKILL'));
  whileIdle.send({ type: 'user_message', id: 'u-1', text: 'fine' });
  await whileIdle.waitUntil(count('turn_end', 1), 'the turn to finish');
  await whileIdle.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error'), 'the error status');
  assert.equal(await whileIdle.waitExit(), 1);
});

test('a preset agent runs through its pinned command line and names a missing credential', async (t) => {
  // A `gemini` shim on PATH that records its argv, so the preset's command is checked end-to-end.
  const binDir = mkdtempSync(join(tmpdir(), 'acp-test-bin-'));
  const argvRecord = join(binDir, 'argv.txt');
  const shim = `#!/bin/sh\nprintf '%s\\n' "$@" >> ${JSON.stringify(argvRecord)}\nexec ${process.execPath} ${JSON.stringify(fakeAcp)} "$@"\n`;
  writeFileSync(join(binDir, 'gemini'), shim, { mode: 0o755 });
  const preset = startRunner({
    env: { COLONIZER_ACP_AGENT: '', COLONIZER_ACP_COMMAND: '', GEMINI_API_KEY: 'test-key', PATH: `${binDir}:/usr/bin:/bin` },
    script: { turns: { '*': {} } },
  });
  t.after(() => preset.child.kill('SIGKILL'));
  preset.send({ type: 'user_message', id: 'initial', text: 'hi' });
  await preset.waitUntil(count('turn_end', 1), 'the turn to finish on the preset agent');
  assert.equal(readFileSync(argvRecord, 'utf8').trim(), '--experimental-acp', 'the gemini preset carries the ACP flag');
  await stop(preset);

  const noKey = startRunner({ env: { COLONIZER_ACP_AGENT: 'gemini', COLONIZER_ACP_COMMAND: '', GEMINI_API_KEY: '' } });
  t.after(() => noKey.child.kill('SIGKILL'));
  const problem = await noKey.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the credential error');
  assert.match(problem.message, /ACP_CREDENTIAL_MISSING/);
  assert.match(problem.message, /GEMINI_API_KEY/);
  noKey.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await noKey.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /ACP_CREDENTIAL_MISSING/);
  assert.equal(noKey.records().length, 0, 'no agent is spawned without a credential');
  await stop(noKey);

  const unknown = startRunner({ env: { COLONIZER_ACP_AGENT: 'bogus' } });
  t.after(() => unknown.child.kill('SIGKILL'));
  const problem2 = await unknown.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the unknown-preset error');
  assert.match(problem2.message, /ACP_AGENT_UNKNOWN/);
  await unknown.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'ACP_AGENT_UNKNOWN'), 'the error status');
  await stop(unknown);

  const empty = startRunner({ env: { COLONIZER_ACP_AGENT: 'custom', COLONIZER_ACP_COMMAND: '   ' } });
  t.after(() => empty.child.kill('SIGKILL'));
  const problem3 = await empty.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the empty-command error');
  assert.match(problem3.message, /the custom command is empty/);
  await stop(empty);
});
