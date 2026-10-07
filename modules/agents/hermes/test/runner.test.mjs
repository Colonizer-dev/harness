import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { AsyncQueue, backendRefusal, DISABLED_TOOLSETS, disabledToolsets, hermesConfig, MCP_TIMEOUT_SECS, probeHermes, resolveModel, runAgent } from '../runner.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const STUB = join(HERE, 'fake-hermes.mjs');
const RUNNER = join(HERE, '..', 'runner.mjs');
const SCHEMA = JSON.parse(readFileSync(join(HERE, '..', '..', '..', '..', 'docs', 'agent-events.schema.json'), 'utf8'));
const ROUTES = JSON.stringify([
  {
    provider: 'deepseek',
    prefix: 'deepseek/',
    base_url: 'http://host.microsandbox.internal:41750/providers/deepseek',
    auth: 'none',
    headers: { 'x-colonizer-colony': 'tok-1' },
  },
]);

/** One driven runner: injected stub binary, captured events, a command queue and a temp HERMES_HOME. */
function harness(env = {}) {
  const home = mkdtempSync(join(tmpdir(), 'colonizer-hermes-test-'));
  const record = join(home, 'record.json');
  const events = [];
  const commands = new AsyncQueue();
  const childEnv = {
    ...process.env,
    ...Object.fromEntries(Object.entries(env).filter(([, value]) => value !== undefined)),
    COLONIZER_MODEL_ROUTES: env.COLONIZER_MODEL_ROUTES ?? ROUTES,
    COLONIZER_MODEL: env.COLONIZER_MODEL ?? 'deepseek/deepseek-flash',
    FAKE_HERMES_RECORD: record,
    ...(env.FAKE_HERMES_MODE ? { FAKE_HERMES_MODE: env.FAKE_HERMES_MODE } : {}),
    ...(env.COLONIZER_HERMES_TURN_TIMEOUT_SECS ? { COLONIZER_HERMES_TURN_TIMEOUT_SECS: env.COLONIZER_HERMES_TURN_TIMEOUT_SECS } : {}),
  };
  let bridge = null;
  const done = runAgent({ hermes: ['node', STUB], commands, emit: (event) => events.push(event), env: childEnv, home, onReady: (b) => (bridge = b) });
  /** POSTs to the runner's loopback bridge the way the colonizer MCP server would (polls for the
   * bridge, which runAgent brings up asynchronously before its first reply). */
  const askBridge = async (path, body) => {
    const deadline = Date.now() + 5000;
    while (!bridge && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10));
    const res = await fetch(`${bridge.url}${path}`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${bridge.token}` }, body: JSON.stringify(body) });
    return res.json();
  };
  return { home, record, events, commands, done, askBridge, readRecord: () => JSON.parse(readFileSync(record, 'utf8')) };
}

const waitFor = (events, predicate, what, timeoutMs = 15_000) =>
  new Promise((resolve, reject) => {
    const timer = setInterval(() => {
      const found = events.find(predicate);
      if (found) {
        clearInterval(timer);
        clearTimeout(guard);
        resolve(found);
      }
    }, 20);
    const guard = setTimeout(() => {
      clearInterval(timer);
      reject(new Error(`timed out waiting for ${what}; events so far: ${JSON.stringify(events)}`));
    }, timeoutMs);
  });

const count = (events, type) => events.filter((e) => e.type === type).length;

/** Runs runner.mjs as a real child process and returns its exit code and parsed stdout events. */
async function spawnRunner(env) {
  const child = spawn(process.execPath, [RUNNER], { env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  child.stdout.on('data', (data) => (out += data));
  const code = await new Promise((resolve) => child.on('close', resolve));
  return { code, events: out.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line)) };
}

// docs/agent-events.schema.json is one source of truth for the event contract; this checks the
// runner's whole output against it: known types only, and every required field present.
function assertConforms(events) {
  const defs = new Map(Object.entries(SCHEMA.$defs).map(([, def]) => [def.properties?.type?.const, def]));
  for (const event of events) {
    const def = defs.get(event.type);
    assert.ok(def, `event type "${event.type}" is not allowed by docs/agent-events.schema.json`);
    for (const field of def.required ?? []) {
      assert.notEqual(event[field], undefined, `"${event.type}" is missing required field "${field}"`);
    }
    if (event.type === 'status') {
      assert.ok(def.properties.state.enum.includes(event.state), `status.state "${event.state}" is not in the schema's enum`);
    }
    if (event.type === 'log') {
      assert.ok(def.properties.level.enum.includes(event.level), `log.level "${event.level}" is not in the schema's enum`);
    }
  }
}

test('a turn streams, pairs tools, reports usage, and the next turn resumes the session', async () => {
  const h = harness();
  h.commands.push({ type: 'user_message', id: 'initial', text: 'do the thing' });
  await waitFor(h.events, (e) => e.type === 'turn_end', 'first turn_end');

  assert.deepEqual(h.events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(h.events.find((e) => e.type === 'user_message'), { type: 'user_message', id: 'initial', text: 'do the thing' });
  assert.ok(h.events.some((e) => e.type === 'status' && e.state === 'working'));
  assert.ok(h.events.some((e) => e.type === 'model_changed' && e.model === 'deepseek/deepseek-flash' && e.previous === null));
  const deltas = h.events.filter((e) => e.type === 'assistant_text_delta');
  assert.deepEqual(deltas.map((e) => e.delta), ['Working ', 'on it.']);
  const call = h.events.find((e) => e.type === 'tool_call');
  assert.equal(call.name, 'terminal');
  assert.deepEqual(call.input, { command: 'ls' });
  const result = h.events.find((e) => e.type === 'tool_result');
  assert.equal(result.tool_call_id, call.tool_call_id); // FIFO pairing by tool name
  assert.equal(result.is_error, false);
  const end = h.events.find((e) => e.type === 'turn_end');
  assert.equal(end.is_error, false);
  assert.equal(end.result, 'Working on it.');
  assert.equal(end.cost_usd, null); // the gateway accounts cost, never the runner
  assert.deepEqual(end.model_usage, { 'deepseek/deepseek-flash': { input_tokens: 100, output_tokens: 20, cache_read_tokens: 40, cache_write_tokens: 10 } });
  assert.deepEqual(h.events.at(-1), { type: 'status', state: 'idle' });
  assert.equal(readFileSync(join(h.home, 'colonizer-session-id'), 'utf8'), 'fake-session-1\n');

  const first = h.readRecord();
  assert.deepEqual(first.argv, [
    'chat',
    '-q',
    'do the thing',
    '--format',
    'stream-json',
    '--provider',
    'colonizer-deepseek',
    '-m',
    'deepseek-flash',
  ]);
  assert.ok(!first.argv.includes('--resume'));
  // The model block satisfies Hermes' first-run guard, which ignores the top-level providers map.
  assert.deepEqual(first.config.model, { provider: 'colonizer-deepseek', default: 'deepseek-flash' });

  h.commands.push({ type: 'user_message', id: 'u-2', text: 'and the other thing' });
  await waitFor(h.events, (e) => count(h.events, 'turn_end') === 2, 'second turn_end');
  const second = h.readRecord();
  assert.ok(second.argv.includes('--resume'));
  assert.equal(second.argv[second.argv.indexOf('--resume') + 1], 'fake-session-1');
  const secondEnd = h.events.filter((e) => e.type === 'turn_end').at(-1);
  assert.equal(secondEnd.model_usage['deepseek/deepseek-flash'].input_tokens, 200); // cumulative

  h.commands.push({ type: 'set_model', model: 'deepseek/deepseek-chat' });
  await waitFor(h.events, (e) => e.type === 'model_changed' && e.model === 'deepseek/deepseek-chat', 'model_changed');
  assert.ok(h.events.some((e) => e.type === 'model_changed' && e.model === 'deepseek/deepseek-chat' && e.previous === 'deepseek/deepseek-flash'));
  h.commands.push({ type: 'user_message', id: 'u-3', text: 'on the new model' });
  await waitFor(h.events, (e) => count(h.events, 'turn_end') === 3, 'third turn_end');
  const third = h.readRecord();
  assert.equal(third.argv[third.argv.indexOf('-m') + 1], 'deepseek-chat');
  assert.deepEqual(third.config.model, { provider: 'colonizer-deepseek', default: 'deepseek-chat' }); // config follows set_model
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assert.deepEqual(h.events.at(-1), { type: 'status', state: 'exited' });
  assertConforms(h.events);
});

test('the written config pins the local backend, switches Hermes extras off, and routes through the gateway', async () => {
  const h = harness();
  h.commands.push({ type: 'user_message', id: 'initial', text: 'configure' });
  await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
  const { config, env } = h.readRecord();
  assert.equal(config.terminal.backend, 'local');
  assert.equal(env.TERMINAL_ENV, 'local');
  assert.equal(env.HERMES_HOME, h.home);
  assert.deepEqual(config.memory, { memory_enabled: false, user_profile_enabled: false });
  assert.equal(config.skills.write_approval, true);
  assert.deepEqual(config.auxiliary.background_review, { enabled: false });
  assert.deepEqual(config.agent.disabled_toolsets, DISABLED_TOOLSETS);
  assert.ok(DISABLED_TOOLSETS.includes('clarify'), "Hermes' own clarify toolset stays off: ask_user replaces it");
  assert.deepEqual(config.model, { provider: 'colonizer-deepseek', default: 'deepseek-flash' });
  assert.deepEqual(config.providers, {
    'colonizer-deepseek': {
      api: 'http://host.microsandbox.internal:41750/providers/deepseek',
      transport: 'anthropic_messages',
      extra_headers: { 'x-colonizer-colony': 'tok-1' },
    },
  });
  // The colonizer MCP server rides in the config: the same mcp.mjs the other modules vendor, with
  // only the bridge coordinates in its env outside a loop (mcp.mjs's gating hides the findings,
  // memory and loop tools) and a timeout that can hold an ask open for a human.
  const colonizer = config.mcp_servers.colonizer;
  assert.equal(colonizer.command, process.execPath);
  assert.deepEqual(colonizer.args, [join(HERE, '..', 'mcp.mjs')]);
  assert.match(colonizer.env.COLONIZER_BRIDGE_URL, /^http:\/\/127\.0\.0\.1:\d+$/);
  assert.match(colonizer.env.COLONIZER_BRIDGE_TOKEN, /^[0-9a-f]{32}$/);
  assert.deepEqual(Object.keys(colonizer.env).sort(), ['COLONIZER_BRIDGE_TOKEN', 'COLONIZER_BRIDGE_URL']);
  assert.equal(colonizer.timeout, MCP_TIMEOUT_SECS);
  assert.deepEqual(hermesConfig([]).agent.disabled_toolsets, DISABLED_TOOLSETS);
  assert.equal(hermesConfig([], null).mcp_servers, undefined, 'no mcpServers argument writes no mcp_servers block');
  h.commands.push({ type: 'shutdown' });
  await h.done;
});

test('the disabled_tools setting names extra toolsets, appended deduplicated to the always-off list', async () => {
  assert.deepEqual(disabledToolsets({}), DISABLED_TOOLSETS, 'an empty setting leaves the hardcoded list');
  assert.deepEqual(disabledToolsets({ COLONIZER_DISABLED_TOOLS: ' web , browser,, web, terminal ' }), [...DISABLED_TOOLSETS, 'web', 'browser', 'terminal'], 'names are trimmed, empties dropped, repeats collapsed');
  assert.deepEqual(disabledToolsets({ COLONIZER_DISABLED_TOOLS: 'file, memory' }), [...DISABLED_TOOLSETS, 'file'], 'an always-off name is not appended twice');

  const h = harness({ COLONIZER_DISABLED_TOOLS: 'web, browser' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'configure' });
  await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
  const { config } = h.readRecord();
  assert.deepEqual(config.agent.disabled_toolsets, [...DISABLED_TOOLSETS, 'web', 'browser'], 'the written config carries the appended list');
  h.commands.push({ type: 'shutdown' });
  await h.done;
});

test('Portal, unrouted and unprefixed models are refused, on set_model as on a turn', async () => {
  assert.match(resolveModel('nous/Hermes-4-Flash', []).error, /Portal credits/);
  assert.match(resolveModel('kimi/k2', []).error, /no gateway route/);
  assert.match(resolveModel('opus', []).error, /no <provider>\/ prefix/);
  assert.match(resolveModel('', []).error, /no model configured/);
  assert.deepEqual(resolveModel('deepseek/deepseek-flash', [{ provider: 'deepseek', prefix: 'deepseek/', base_url: 'http://x', headers: {} }]), {
    ok: true,
    provider: 'colonizer-deepseek',
    model: 'deepseek-flash',
    full: 'deepseek/deepseek-flash',
  });

  const h = harness({ COLONIZER_MODEL: 'nous/Hermes-4-Flash' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'hello' });
  const refused = await waitFor(h.events, (e) => e.type === 'status' && e.state === 'error', 'error status');
  assert.match(refused.detail, /Portal credits/);
  assert.ok(h.events.some((e) => e.type === 'log' && e.level === 'error' && /Portal credits/.test(e.message)));
  const end = h.events.find((e) => e.type === 'turn_end'); // every echoed message ends somewhere
  assert.ok(end);
  assert.equal(end.is_error, true);
  assert.equal(end.result, null);
  assert.equal(end.cost_usd, null);
  assert.equal(end.duration_ms, null);
  h.commands.push({ type: 'set_model', model: 'kimi/k2' });
  await waitFor(h.events, (e) => e.type === 'log' && e.level === 'warn' && /set_model refused/.test(e.message), 'set_model refusal');
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('preflight refuses a non-local TERMINAL_ENV and exits non-zero', async () => {
  assert.equal(backendRefusal({ TERMINAL_ENV: 'local' }), null);
  assert.match(backendRefusal({ TERMINAL_ENV: 'docker' }), /TERMINAL_ENV=docker/);
  const { code, events } = await spawnRunner({ TERMINAL_ENV: 'docker', COLONIZER_HERMES_BIN: `node ${STUB}` });
  assert.notEqual(code, 0);
  assert.ok(events.some((e) => e.type === 'log' && e.level === 'error' && e.message.includes('TERMINAL_ENV=docker')));
  assert.ok(events.some((e) => e.type === 'status' && e.state === 'error'));
  assertConforms(events);
});

test('preflight fails loudly when the hermes binary is missing, naming the pinned install', async () => {
  const probe = await probeHermes({ bin: ['colonizer-no-such-hermes-bin'] });
  assert.equal(probe.ok, false);
  assert.match(probe.error, /not found on PATH/);
  const ok = await probeHermes({ bin: ['node', STUB] });
  assert.equal(ok.ok, true);
  assert.equal(ok.version, 'Hermes Agent v0.21.5 (2026.9.24)');

  const { code, events } = await spawnRunner({ COLONIZER_HERMES_BIN: 'colonizer-no-such-hermes-bin' });
  assert.notEqual(code, 0);
  const error = events.find((e) => e.type === 'log' && e.level === 'error');
  assert.match(error.message, /v2026\.9\.24/);
  assert.match(error.message, /pip install -e/);
  assert.ok(events.some((e) => e.type === 'status' && e.state === 'error'));
  assertConforms(events);
});

test('non-JSON noise on hermes stdout becomes a log, not a crash', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'noise' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'noisy' });
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
  assert.equal(end.is_error, false);
  assert.match(h.events.find((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('tirith')).message, /not JSON/);
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('a failed turn reports the error and still ends', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'failure' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'fail' });
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
  assert.equal(end.is_error, true);
  assert.equal(end.result, null);
  assert.ok(h.events.some((e) => e.type === 'log' && e.level === 'error' && /provider unreachable/.test(e.message)));
  h.commands.push({ type: 'shutdown' });
  await h.done;
});

test('interrupt terminates the child and finishes the turn', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'slow' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'slow' });
  await waitFor(h.events, (e) => e.type === 'assistant_text_delta', 'first delta');
  h.commands.push({ type: 'interrupt' });
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end after interrupt');
  assert.equal(end.is_error, true);
  assert.ok(h.events.some((e) => e.type === 'log' && /interrupt: terminating/.test(e.message)));
  assert.deepEqual(h.events.at(-1), { type: 'status', state: 'idle' });
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('a turn that exceeds the timeout is terminated with an error', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'hang', COLONIZER_HERMES_TURN_TIMEOUT_SECS: '1' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'hang' });
  await waitFor(h.events, (e) => e.type === 'status' && e.state === 'working', 'working status');
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end after timeout');
  assert.equal(end.is_error, true);
  assert.ok(h.events.some((e) => e.type === 'log' && e.level === 'error' && /TURN_TIMEOUT/.test(e.message)));
  assert.deepEqual(h.events.at(-1), { type: 'status', state: 'idle' });
  h.commands.push({ type: 'shutdown' });
  await h.done;
});

test('ask_user asks the user over the bridge, and the answer finishes the turn', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'ask' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'restart the service?' });
  const question = await waitFor(h.events, (e) => e.type === 'question', 'the question event');
  assert.equal(question.question_id, 'q-1');
  assert.deepEqual(question.questions, [
    {
      question: 'Proceed with the restart?',
      header: 'Restart',
      multi_select: false,
      options: [
        { label: 'Yes', description: 'restart now', preview: null },
        { label: 'No', description: '', preview: null },
      ],
    },
  ]);
  await waitFor(h.events, (e) => e.type === 'status' && e.state === 'waiting_for_answer', 'the waiting-for-answer status');

  h.commands.push({ type: 'answer', question_id: 'q-1', answers: { Restart: 'Yes' }, response: 'go ahead' });
  const answered = await waitFor(h.events, (e) => e.type === 'question_answered', 'the question_answered event');
  assert.deepEqual(answered, { type: 'question_answered', question_id: 'q-1', answers: { Restart: 'Yes' }, response: 'go ahead' });
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'the turn to finish');
  assert.equal(end.is_error, false);
  assert.match(end.result, /^Answered: /);
  assert.equal(count(h.events, 'tool_call'), 0, 'a question is never also a tool_call (§2)');
  assert.equal(count(h.events, 'tool_result'), 0);
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('an interrupt during a parked ask cancels it, and the model sees the cancellation', async () => {
  const h = harness({ FAKE_HERMES_MODE: 'ask', FAKE_HERMES_IGNORE_SIGTERM: '1' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'restart the service?' });
  await waitFor(h.events, (e) => e.type === 'question', 'the question event');
  h.commands.push({ type: 'interrupt' });
  const end = await waitFor(h.events, (e) => e.type === 'turn_end', 'the turn to end');
  assert.equal(end.is_error, false, 'the SIGTERM-immune fake lives on to finish its turn with the cancellation');
  assert.match(end.result, /cancelled/);
  assert.ok(h.events.some((e) => e.type === 'log' && /interrupt: terminating/.test(e.message)));
  assert.equal(h.events.some((e) => e.type === 'question_answered'), false, 'a cancelled ask is never an answer');
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('an answer without an open question warns', async () => {
  const h = harness();
  h.commands.push({ type: 'answer', question_id: 'q-404', answers: {}, response: null });
  const logged = await waitFor(h.events, (e) => e.type === 'log' && /no open question/.test(e.message), 'the warn log');
  assert.equal(logged.level, 'warn');
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('the bridge parks /ask until answer, and cancelAll releases a parked ask as cancelled', async () => {
  const h = harness();
  const parked = h.askBridge('/ask', { questions: [{ question: 'Which color?', header: 'Paint', multiSelect: true, options: [{ label: 'Blue', description: 'the calm one' }] }] });
  const question = await waitFor(h.events, (e) => e.type === 'question', 'the question event');
  assert.deepEqual(question.questions, [{ question: 'Which color?', header: 'Paint', multi_select: true, options: [{ label: 'Blue', description: 'the calm one', preview: null }] }]);
  assert.equal(await Promise.race([parked.then(() => 'settled'), new Promise((r) => setTimeout(() => r('parked'), 100))]), 'parked', 'the ask holds until an answer or a cancel');
  h.commands.push({ type: 'answer', question_id: question.question_id, answers: { Paint: 'Blue' }, response: null });
  assert.deepEqual(await parked, { answers: { Paint: 'Blue' }, response: null }, 'the answer command releases the parked HTTP response');
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

/** The registered colonizer MCP server, started the way Hermes would start it from config.yaml. */
function startMcp(server) {
  const child = spawn(server.command, server.args, { env: { PATH: process.env.PATH, ...server.env }, stdio: ['pipe', 'pipe', 'inherit'] });
  const pending = new Map();
  let id = 0;
  child.stdout.setEncoding('utf8');
  let buf = '';
  child.stdout.on('data', (chunk) => {
    buf += chunk;
    for (let nl = buf.indexOf('\n'); nl >= 0; nl = buf.indexOf('\n')) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (!line.trim()) continue;
      const msg = JSON.parse(line);
      pending.get(msg.id)?.(msg);
    }
  });
  const call = (method, params) =>
    new Promise((resolve) => {
      const n = ++id;
      pending.set(n, resolve);
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: n, method, params })}\n`);
    });
  return { call, stop: () => child.kill('SIGKILL') };
}

test('loop tools (issue #643): a loop colony\'s colonizer server offers loop_next and loop_stop, and their calls leave as loop events', async (t) => {
  const h = harness({ COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true' });
  h.commands.push({ type: 'user_message', id: 'initial', text: 'loop run' });
  await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
  const colonizer = h.readRecord().config.mcp_servers.colonizer;
  assert.equal(colonizer.env.COLONIZER_LOOP, 'true');
  assert.equal(colonizer.env.COLONIZER_LOOP_SELF_PACED, 'true');

  const mcp = startMcp(colonizer);
  t.after(() => mcp.stop());
  await mcp.call('initialize', {});
  const names = (await mcp.call('tools/list', {})).result.tools.map((tool) => tool.name);
  assert.ok(names.includes('loop_next') && names.includes('loop_stop'), names.join(', '));
  assert.equal(names.includes('finding_file'), false, 'findings stay off: the runner passes no findings switch');

  // The delay is clamped to the mothership's 15 min – 24 h before it crosses the bridge.
  const next = await mcp.call('tools/call', { name: 'loop_next', arguments: { delay_minutes: 5, reason: 'check the deploy' } });
  assert.equal(next.result.content[0].text, 'Next run scheduled in 15 minutes.');
  assert.deepEqual(await waitFor(h.events, (e) => e.type === 'loop_next', 'the loop_next event'), { type: 'loop_next', delay_minutes: 15, reason: 'check the deploy' });
  const stop = await mcp.call('tools/call', { name: 'loop_stop', arguments: { reason: 'goal met' } });
  assert.equal(stop.result.content[0].text, 'The loop is stopped; this is its last run.');
  assert.deepEqual(await waitFor(h.events, (e) => e.type === 'loop_stop', 'the loop_stop event'), { type: 'loop_stop', reason: 'goal met' });
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});

test('loop tools: a fixed-cadence loop gets loop_stop only, and a non-loop colony neither', async (t) => {
  for (const [env, expected] of [
    [{ COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'false' }, ['loop_stop']],
    [{}, []],
  ]) {
    const h = harness(env);
    h.commands.push({ type: 'user_message', id: 'initial', text: 'run' });
    await waitFor(h.events, (e) => e.type === 'turn_end', 'turn_end');
    const mcp = startMcp(h.readRecord().config.mcp_servers.colonizer);
    t.after(() => mcp.stop());
    await mcp.call('initialize', {});
    const names = (await mcp.call('tools/list', {})).result.tools.map((tool) => tool.name);
    assert.deepEqual(names.filter((name) => name.startsWith('loop_')), expected, JSON.stringify(env));
    h.commands.push({ type: 'shutdown' });
    await h.done;
  }
});

test('loop tools: the bridge refuses a malformed loop call without emitting, and the manifest declares loop_tools', async () => {
  const h = harness();
  assert.deepEqual(await h.askBridge('/loop_next', { delay_minutes: 0, reason: 'x' }), { error: 'loop_next needs delay_minutes: a number of minutes from now' });
  assert.deepEqual(await h.askBridge('/loop_next', { delay_minutes: 30, reason: ' ' }), { error: 'loop_next needs a reason: what the next run should find or do' });
  assert.deepEqual(await h.askBridge('/loop_stop', {}), { error: 'loop_stop needs a reason: why the loop should stop' });
  assert.equal(count(h.events, 'loop_next') + count(h.events, 'loop_stop'), 0);
  h.commands.push({ type: 'shutdown' });
  await h.done;
  // The mothership reads this flag: with it, a self-paced loop on Hermes is briefed with loop_next
  // instead of falling back to every 24 hours, and the Loops form does not warn.
  assert.equal(JSON.parse(readFileSync(join(HERE, '..', 'module.json'), 'utf8')).loop_tools, true);
});
