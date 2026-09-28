// Contract tests for the grok-build runner: they boot the real runner.mjs as a child process and
// drive it over the colonizer-runner/1 protocol against test/fake-grok.mjs standing in for the
// grok CLI (COLONIZER_GROK_BIN is the seam). The happy path's events are also checked against the
// required fields of docs/agent-events.schema.json.

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { createBridge, parseVersion, resolveModel, startTurn, turnArgs } from '../runner.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const moduleDir = join(here, '..');
const fakeGrok = join(here, 'fake-grok.mjs');

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** The [flag, value] pair an argv carries, or null when the flag is absent. */
const pair = (args, flag) => (args.includes(flag) ? args.slice(args.indexOf(flag), args.indexOf(flag) + 2) : null);

/** The runner as a child process, with its protocol output collected into `events`. */
function startRunner(env = {}) {
  // A scratch dir plus a `grok` on it that is the fake, so COLONIZER_GROK_BIN needs no PATH games.
  const binDir = mkdtempSync(join(tmpdir(), 'grok-test-bin-'));
  const bin = join(binDir, 'grok');
  writeFileSync(bin, `#!/bin/sh\nexec ${process.execPath} ${JSON.stringify(fakeGrok)} "$@"\n`, { mode: 0o755 });
  const scratch = mkdtempSync(join(tmpdir(), 'grok-test-'));
  const record = join(scratch, 'record.jsonl');
  const askRecord = join(scratch, 'ask.jsonl');
  const child = spawn(process.execPath, [join(moduleDir, 'runner.mjs')], {
    env: { PATH: '/usr/bin:/bin', TMPDIR: scratch, XAI_API_KEY: 'xai-test-key', COLONIZER_GROK_BIN: bin, GROK_FAKE_RECORD: record, GROK_FAKE_ASK_RECORD: askRecord, ...env },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const events = [];
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  lines.on('line', (line) => {
    if (line.trim()) {
      try {
        events.push(JSON.parse(line));
      } catch {
        events.push({ type: 'unparseable', line });
      }
    }
  });
  child.stderr.setEncoding('utf8');

  const send = (command) => child.stdin.write(`${JSON.stringify(command)}\n`);
  // `check` sees the whole event list, so a test can wait on a count, not just a first match.
  const waitUntil = async (check, what, timeoutMs = 20000) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const found = check(events);
      if (found) return found;
      if (child.exitCode !== null || Date.now() > deadline) {
        assert.fail(`timed out waiting for ${what}; runner exit ${child.exitCode}; events: ${JSON.stringify(events)}`);
      }
      await sleep(25);
    }
  };
  // Attached at spawn, not on demand: the runner exits right after its final event, so a late
  // `once(child, "exit")` would race an exit that already happened.
  let exitCode = null;
  child.on('exit', (code) => (exitCode = code));
  const waitExit = async (timeoutMs = 10000) => {
    const deadline = Date.now() + timeoutMs;
    while (exitCode === null && Date.now() < deadline) await sleep(10);
    return exitCode ?? 'timeout';
  };
  const records = () => (existsSync(record) ? readFileSync(record, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  // Only the prompt invocations are turns; the records also hold the preflight's `--version` call.
  const turns = () => records().filter((r) => r.argv.includes('--prompt-file'));
  // What the bridge answered to each colonizer__ask_user POST the fake grok made.
  const asks = () => (existsSync(askRecord) ? readFileSync(askRecord, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []);
  return { child, events, send, waitUntil, waitExit, records, turns, asks };
}

/** The bridge the runner registered in the fresh GROK_HOME's config.toml, plus a /ask POSTer that
 * drives it the way mcp.mjs does. Waits for the first grok invocation to be recorded, since the
 * config exists from the runner's boot but its location is only known once a child has run. */
async function bridgeOf(runner) {
  const deadline = Date.now() + 10000;
  for (;;) {
    const home = runner.records().find((r) => r.env.GROK_HOME)?.env.GROK_HOME;
    if (home) {
      const config = readFileSync(join(home, 'config.toml'), 'utf8');
      const url = /COLONIZER_BRIDGE_URL = "([^"]+)"/.exec(config)?.[1];
      const token = /COLONIZER_BRIDGE_TOKEN = "([^"]+)"/.exec(config)?.[1];
      return {
        config,
        url,
        token,
        async post(body) {
          const res = await fetch(`${url}/ask`, { method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: JSON.stringify(body) });
          return res;
        },
      };
    }
    if (Date.now() > deadline) assert.fail('no grok invocation was recorded');
    await sleep(10);
  }
}

const stop = (runner) => (runner.send({ type: 'shutdown' }), runner.waitExit());

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

const first = (type) => (events) => events.find((e) => e.type === type);
const count = (type, n) => (events) => {
  const matches = events.filter((e) => e.type === type);
  return matches.length >= n ? matches[n - 1] : undefined;
};

test('parseVersion takes the first semver, wherever it sits', () => {
  assert.equal(parseVersion('grok 1.0.34 (built today)'), '1.0.34');
  assert.equal(parseVersion('1.2.3'), '1.2.3');
  assert.equal(parseVersion('no digits here'), null);
  assert.equal(parseVersion(undefined), null);
});

test('resolveModel accepts xai-grok and bare ids, and refuses other providers by name', () => {
  assert.deepEqual(resolveModel('xai-grok/grok-4.5'), { model: 'grok-4.5' });
  assert.deepEqual(resolveModel('  grok-4.6 '), { model: 'grok-4.6' });
  assert.deepEqual(resolveModel(''), {});
  assert.match(resolveModel('deepseek/deepseek-flash').error, /^GROK_MODEL_PROVIDER:/);
});

test('turnArgs carries every nesting flag, the model, and the resume id', () => {
  const args = turnArgs({ promptFile: '/tmp/p.txt', model: 'grok-4.5', sessionId: 'sess-1' });
  for (const flag of ['--output-format', 'streaming-json', '--always-approve', '--sandbox', 'off', '--disable-web-search', '--no-auto-update']) {
    assert.ok(args.includes(flag), `missing ${flag}`);
  }
  assert.deepEqual(pair(args, '-m'), ['-m', 'grok-4.5']);
  assert.deepEqual(pair(args, '-r'), ['-r', 'sess-1']);
  const bare = turnArgs({ promptFile: '/tmp/p.txt' });
  assert.ok(!bare.includes('-m') && !bare.includes('-r'), 'no model and nothing to resume means neither flag');
});

test('a turn streams mapped events and the child carries the nesting flags', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'xai-grok/grok-4.5' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'initial', text: 'do the thing' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(runner.events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(first('user_message')(runner.events), { type: 'user_message', id: 'initial', text: 'do the thing' });
  const order = runner.events.map((e) => `${e.type}:${e.state ?? ''}`);
  assert.ok(order.indexOf('status:idle') < order.indexOf('user_message:'), 'the echo follows the initial idle');
  assert.ok(order.indexOf('user_message:') < order.indexOf('status:working'), 'working follows the echo');
  assert.ok(order.indexOf('status:working') < order.indexOf('turn_end:'), 'the turn ends inside working');
  assert.ok(order.indexOf('turn_end:') < order.indexOf('status:idle', 1), 'idle follows the turn end');
  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'grok-4.5', previous: null });
  assert.deepEqual(
    runner.events.filter((e) => e.type === 'assistant_text_delta').map((e) => e.delta),
    ['Hello', ', colony'],
  );
  assert.deepEqual(first('thinking')(runner.events), { type: 'thinking', message_id: 'msg-1', block_index: 1, text: 'weighing the options' });
  assert.deepEqual(first('assistant_text')(runner.events), { type: 'assistant_text', message_id: 'msg-1', block_index: 0, text: 'Hello, colony' });
  assert.deepEqual(first('tool_call')(runner.events), {
    type: 'tool_call',
    message_id: 'msg-1',
    tool_call_id: 'call_1',
    name: 'read_file',
    input: { path: 'src/main.rs' },
  });
  assert.deepEqual(first('tool_result')(runner.events), { type: 'tool_result', tool_call_id: 'call_1', output: '{"lines":42}', is_error: false });
  assert.equal(runner.events.filter((e) => e.type === 'tool_result').length, 1, 'an in_progress update emits no tool_result');

  const turnEnd = first('turn_end')(runner.events);
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'Hello, colony');
  assert.equal(turnEnd.cost_usd, 0.01);
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assert.deepEqual(turnEnd.model_usage, { 'grok-4.5': { input_tokens: 10, output_tokens: 5, cache_read_tokens: 2, cache_write_tokens: 0 } });
  assertSchema(runner.events);

  const [invocation] = runner.turns();
  assert.equal(invocation.prompt, 'do the thing', 'the prompt travels via --prompt-file');
  assert.ok(!invocation.argv.includes('-p'), 'no -p: the prompt rides in a file');
  assert.ok(!invocation.argv.includes('login'), 'the runner never logs in');
  assert.deepEqual(pair(invocation.argv, '-m'), ['-m', 'grok-4.5']);
  assert.ok(!invocation.argv.includes('-r'), 'the first turn starts a fresh session');
  assert.equal(invocation.env.BROWSER, '/bin/false');
  assert.equal(invocation.env.GROK_MEMORY, '0');
  assert.equal(invocation.env.GROK_TELEMETRY_ENABLED, '0');
  assert.equal(invocation.env.GROK_DISABLE_AUTOUPDATER, '1');
  assert.equal(invocation.env.XAI_API_KEY, 'set');
  assert.ok(invocation.env.GROK_HOME && invocation.env.GROK_HOME !== process.env.GROK_HOME, 'a fresh GROK_HOME is assigned');

  runner.send({ type: 'shutdown' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('the second turn resumes the first turn’s grok session, with cumulative cost and usage', async (t) => {
  const runner = startRunner();
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'turn one' });
  await runner.waitUntil(count('turn_end', 1), 'the first turn to finish');
  runner.send({ type: 'user_message', id: 'u-2', text: 'turn two' });
  const second = await runner.waitUntil(count('turn_end', 2), 'the second turn to finish');

  const turns = runner.turns();
  assert.equal(turns.length, 2);
  assert.ok(!turns[0].argv.includes('-r'), 'the first turn starts a fresh session');
  assert.deepEqual(pair(turns[1].argv, '-r'), ['-r', 'sess-fake-1'], 'the second turn resumes the end event’s sessionId');
  assert.equal(second.cost_usd, 0.02, 'cost_usd is cumulative for the colony');
  assert.deepEqual(second.model_usage, { 'grok-4.5': { input_tokens: 20, output_tokens: 10, cache_read_tokens: 4, cache_write_tokens: 0 } });

  await stop(runner);
});

test('set_model switches the next turn and announces it; other providers are refused by name', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'xai-grok/grok-4.5' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'set_model', model: 'deepseek/deepseek-flash' });
  const refusal = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the provider refusal');
  assert.match(refusal.message, /^GROK_MODEL_PROVIDER:/);

  runner.send({ type: 'set_model', model: 'xai-grok/grok-5' });
  await runner.waitUntil(first('model_changed'), 'model_changed');
  runner.send({ type: 'user_message', id: 'u-1', text: 'go' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  assert.deepEqual(first('model_changed')(runner.events), { type: 'model_changed', model: 'grok-5', previous: null });
  assert.equal(runner.events.filter((e) => e.type === 'model_changed').length, 1, 'a set_model already announced suppresses the first-turn announcement');
  assert.deepEqual(pair(runner.turns()[0].argv, '-m'), ['-m', 'grok-5']);

  await stop(runner);
});

test('interrupt fails the running turn, and the runner keeps serving turns', async (t) => {
  const runner = startRunner({ GROK_FAKE_SLEEP_MS: 1500, GROK_FAKE_SLEEP_FIRST: '1' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'slow turn' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'working'), 'the slow turn to start');
  runner.send({ type: 'interrupt' });
  const interrupted = await runner.waitUntil(first('turn_end'), 'the interrupted turn to end');
  assert.equal(interrupted.is_error, true);
  assert.match(interrupted.result, /interrupted/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the interrupt');

  runner.send({ type: 'user_message', id: 'u-2', text: 'quick turn' });
  const recovered = await runner.waitUntil(count('turn_end', 2), 'the next turn to finish');
  assert.equal(recovered.is_error, false);
  assert.equal(recovered.result, 'Hello, colony');

  await stop(runner);
});

test('an interrupt that lands before the spawn ends the turn without starting grok', async () => {
  // startTurn returns while its body is still awaiting the prompt-file write, so an interrupt()
  // fired straight after always lands in the window between `status working` and the spawn —
  // deterministically, with no timers involved.
  const events = [];
  let spawns = 0;
  const spawnFn = () => {
    spawns += 1;
    throw new Error('the spawn point must not be reached');
  };
  const home = mkdtempSync(join(tmpdir(), 'grok-pre-spawn-'));
  const turn = startTurn({
    prompt: 'interrupted before grok starts',
    model: null,
    sessionId: null,
    messageId: 'msg-1',
    env: {},
    home,
    emit: (event) => events.push(event),
    spawnFn,
    totals: { cost: undefined, models: {} },
  });
  turn.interrupt();
  const result = await turn.done;

  assert.equal(spawns, 0, 'grok is never invoked');
  assert.deepEqual(result, { sessionId: null, failure: 'interrupted by the user' });
  const turnEnd = events.find((e) => e.type === 'turn_end');
  assert.equal(turnEnd.is_error, true);
  assert.equal(turnEnd.result, 'interrupted by the user');
  assert.equal(turnEnd.cost_usd, null);
  assert.equal(typeof turnEnd.duration_ms, 'number');
  assert.ok(!existsSync(join(home, 'msg-1.prompt.txt')), 'the prompt file is cleaned up');
});

test('shutdown mid-turn exits cleanly with code 0', async (t) => {
  const runner = startRunner({ GROK_FAKE_SLEEP_MS: 60000 });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'endless turn' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'working'), 'the turn to start');
  runner.send({ type: 'shutdown' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

const post = (bridge, body, token) => fetch(`${bridge.url}/ask`, { method: 'POST', headers: { authorization: `Bearer ${token ?? bridge.token}`, 'content-type': 'application/json' }, body: JSON.stringify(body) });
const lastEvent = async (events, pred) => {
  const deadline = Date.now() + 5000;
  for (;;) {
    const hit = events.filter(pred).at(-1);
    if (hit) return hit;
    if (Date.now() > deadline) assert.fail('the bridge never emitted the expected event');
    await sleep(5);
  }
};

test('the bridge maps an ask to the question shape, and answers or cancels the parked call', async () => {
  const events = [];
  const states = [];
  const b = await createBridge({ emit: (e) => events.push(e), setStatus: (s) => states.push(s), isWorking: () => false, token: 'tok' });
  try {
    assert.equal(await post(b, { questions: [] }, 'wrong').then((r) => r.status), 401, 'the token is required');
    const parked = post(b, { questions: [{ question: 'Q?', multiSelect: true, options: [{ label: 'a' }] }] }).then((r) => r.json());
    const q1 = await lastEvent(events, (e) => e.type === 'question');
    assert.deepEqual(q1, { type: 'question', question_id: 'q-1', message_id: null, questions: [{ question: 'Q?', header: 'Q?', multi_select: true, options: [{ label: 'a', description: '', preview: null }] }] });
    assert.equal(states.at(-1), 'waiting_for_answer');
    b.cancelAll(); // what an interrupt, a turn end or shutdown does
    assert.deepEqual(await parked, { cancelled: true });
    assert.equal(b.answer('q-1', {}, null), false, 'a cancelled ask is no longer open');

    const parked2 = post(b, { message_id: 'msg-1', questions: [] }).then((r) => r.json());
    const q2 = (await lastEvent(events, (e) => e.type === 'question' && e.question_id !== 'q-1')).question_id;
    assert.equal(b.answer(q2, { Q: 'a' }, 'free text'), true);
    assert.deepEqual(await parked2, { answers: { Q: 'a' }, response: 'free text' });
    assert.deepEqual(await lastEvent(events, (e) => e.type === 'question_answered'), { type: 'question_answered', question_id: q2, answers: { Q: 'a' }, response: 'free text' });
    assert.equal(states.at(-1), 'idle', 'no turn is running, so the status settles to idle');
  } finally {
    await b.close();
  }
});

test('config.toml in the fresh GROK_HOME registers the colonizer MCP server with the bridge env', async (t) => {
  const runner = startRunner({ COLONIZER_MODEL: 'xai-grok/grok-4.5' });
  t.after(() => runner.child.kill('SIGKILL'));
  runner.send({ type: 'user_message', id: 'initial', text: 'go' });
  await runner.waitUntil(count('turn_end', 1), 'the turn to finish');

  const { config, url, token } = await bridgeOf(runner);
  const table = config.slice(config.indexOf('[mcp_servers.colonizer]'));
  assert.ok(table.startsWith('[mcp_servers.colonizer]'), 'the colonizer server has its own table');
  assert.ok(table.includes(`command = ${JSON.stringify(process.execPath)}`), 'the server runs on this node');
  assert.ok(table.includes(`args = [${JSON.stringify(join(moduleDir, 'mcp.mjs'))}]`), 'the vendored mcp.mjs is the server');
  assert.match(url ?? '', /^http:\/\/127\.0\.0\.1:\d+$/, 'the loopback bridge URL is configured');
  assert.ok(token && token.length >= 32, 'a real bearer token is configured');
  assert.ok(table.includes('tool_timeout_sec = 3600'), 'an ask may wait on a human for a while');
  await stop(runner);
});

test('a colonizer__ask_user call becomes a question card, and the answer reaches grok', async (t) => {
  const questions = [{ question: 'Which database?', header: 'Database', multiSelect: false, options: [{ label: 'Postgres', description: 'boring' }, { label: 'SQLite', description: 'embedded' }] }];
  const runner = startRunner({ GROK_FAKE_ASK: JSON.stringify(questions) });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'ask me something' });
  const q = await runner.waitUntil(first('question'), 'the question card');
  assert.equal(q.question_id, 'q-1');
  assert.deepEqual(q.questions, [{ question: 'Which database?', header: 'Database', multi_select: false, options: [{ label: 'Postgres', description: 'boring', preview: null }, { label: 'SQLite', description: 'embedded', preview: null }] }]);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'waiting_for_answer'), 'waiting_for_answer');
  assert.ok(!runner.events.some((e) => e.type === 'tool_call' && String(e.name).startsWith('colonizer')), 'a question is never also a tool_call (§2)');

  runner.send({ type: 'answer', question_id: q.question_id, answers: { 'Which database?': 'Postgres' } });
  const answered = await runner.waitUntil(first('question_answered'), 'question_answered');
  assert.deepEqual(answered, { type: 'question_answered', question_id: q.question_id, answers: { 'Which database?': 'Postgres' }, response: null });
  const end = await runner.waitUntil(first('turn_end'), 'the turn to finish');
  assert.equal(end.is_error, false);
  assert.match(end.result, /"answers"/, 'grok got the bridge answer and said so');
  const order = runner.events.map((e) => `${e.type}:${e.state ?? ''}`);
  assert.ok(order.indexOf('question_answered:') < order.lastIndexOf('status:working'), 'the turn resumes after the answer');
  assert.deepEqual(runner.asks(), [{ answers: { 'Which database?': 'Postgres' }, response: null }], 'the parked /ask HTTP response carried {answers, response} to grok');
  await stop(runner);
});

test('an interrupt cancels the pending ask, and a late answer warns', async (t) => {
  const runner = startRunner({ GROK_FAKE_SLEEP_MS: 60000 });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'slow turn' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'working'), 'the slow turn to start');
  // Driven through the config.toml registration, exactly like mcp.mjs would.
  const bridge = await bridgeOf(runner);
  const asked = bridge.post({ questions: [{ question: 'Proceed?', options: [{ label: 'yes' }] }] }).then((r) => r.json());
  const q = await runner.waitUntil(first('question'), 'the question card');
  assert.equal(q.question_id, 'q-1');
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'waiting_for_answer'), 'waiting_for_answer');

  runner.send({ type: 'interrupt' });
  assert.deepEqual(await asked, { cancelled: true }); // the parked HTTP call is released
  const end = await runner.waitUntil(first('turn_end'), 'the interrupted turn');
  assert.equal(end.is_error, true);
  assert.ok(!runner.events.some((e) => e.type === 'question_answered'), 'a cancelled ask answers nothing');
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the interrupt');

  runner.send({ type: 'answer', question_id: q.question_id, answers: {} });
  const warned = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('no open question')), 'the late-answer warning');
  assert.match(warned.message, /q-1/);
  await stop(runner);
});

test('an answer with no open question warns; stdin EOF exits like shutdown', async (t) => {
  const runner = startRunner();
  t.after(() => runner.child.kill('SIGKILL'));
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'the runner to come up');
  runner.send({ type: 'answer', question_id: 'q-9', answers: {}, response: 'yes' });
  const warned = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('no open question')), 'the unknown-id warning');
  assert.match(warned.message, /q-9/);
  runner.child.stdin.end();
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'exited'), 'the exited status');
  assert.equal(await runner.waitExit(), 0);
});

test('without XAI_API_KEY: a named error, and grok is never invoked', async (t) => {
  const runner = startRunner({ XAI_API_KEY: '' });
  t.after(() => runner.child.kill('SIGKILL'));

  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the credential error');
  assert.match(problem.message, /^GROK_CREDENTIAL_MISSING:/);
  assert.match(problem.message, /colony secret named XAI_API_KEY/);
  const error = await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error'), 'the error status');
  assert.equal(error.detail, 'GROK_CREDENTIAL_MISSING');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^GROK_CREDENTIAL_MISSING:/);
  assert.equal(turnEnd.cost_usd, null);

  assert.equal(runner.records().length, 0, 'not even grok --version may run without a key');
  await stop(runner);
});

test('a missing binary is a named error carrying the pinned install command', async (t) => {
  const runner = startRunner({ COLONIZER_GROK_BIN: '/nonexistent/grok' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the binary error');
  assert.match(problem.message, /^GROK_BINARY_MISSING:/);
  assert.match(problem.message, /curl -fsSL https:\/\/x\.ai\/cli\/install\.sh \| bash -s 1\.0\.34/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'GROK_BINARY_MISSING'), 'the error status');
  await stop(runner);
});

test('a grok that is not the pinned version is a named drift error', async (t) => {
  const runner = startRunner({ GROK_FAKE_VERSION: '0.9.9' });
  t.after(() => runner.child.kill('SIGKILL'));
  const problem = await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'error'), 'the drift error');
  assert.match(problem.message, /^GROK_VERSION_DRIFT:/);
  assert.match(problem.message, /0\.9\.9/);
  assert.match(problem.message, /1\.0\.34/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'error' && e.detail === 'GROK_VERSION_DRIFT'), 'the error status');

  runner.send({ type: 'user_message', id: 'initial', text: 'hello?' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the refused turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /^GROK_VERSION_DRIFT:/);
  assert.equal(runner.records().filter((r) => !r.argv.includes('--version')).length, 0, 'no prompt may run on a drifted binary');
  await stop(runner);
});

test('unknown streaming event types are warned about, not fatal', async (t) => {
  const dir = mkdtempSync(join(tmpdir(), 'grok-test-script-'));
  const scriptPath = join(dir, 'events.ndjson');
  writeFileSync(
    scriptPath,
    [
      JSON.stringify({ type: 'mystery', payload: 1 }),
      JSON.stringify({ type: 'text', data: 'still fine' }),
      JSON.stringify({ type: 'end', sessionId: 'sess-x', stopReason: 'end_turn' }),
    ].join('\n'),
  );
  const runner = startRunner({ GROK_FAKE_SCRIPT: scriptPath });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'odd stream' });
  await runner.waitUntil((events) => events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('mystery')), 'the unknown-event warning');
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the turn to finish anyway');
  assert.equal(turnEnd.is_error, false);
  assert.equal(turnEnd.result, 'still fine');
  await stop(runner);
});

test('a child that exits without an end event fails the turn with the exit code and stderr tail', async (t) => {
  const runner = startRunner({ GROK_FAKE_NO_END: '1', GROK_FAKE_EXIT: '1', GROK_FAKE_STDERR: 'boom: auth rejected' });
  t.after(() => runner.child.kill('SIGKILL'));

  runner.send({ type: 'user_message', id: 'u-1', text: 'doomed' });
  const turnEnd = await runner.waitUntil(first('turn_end'), 'the failed turn');
  assert.equal(turnEnd.is_error, true);
  assert.match(turnEnd.result, /without an end event/);
  assert.match(turnEnd.result, /exit code 1/);
  assert.match(turnEnd.result, /boom: auth rejected/);
  await runner.waitUntil((events) => events.find((e) => e.type === 'status' && e.state === 'idle'), 'idle after the failed turn');
  await stop(runner);
});
