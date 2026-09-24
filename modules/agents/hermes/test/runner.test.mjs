import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { AsyncQueue, backendRefusal, DISABLED_TOOLSETS, hermesConfig, probeHermes, resolveModel, runAgent } from '../runner.mjs';

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
  const done = runAgent({ hermes: ['node', STUB], commands, emit: (event) => events.push(event), env: childEnv, home });
  return { home, record, events, commands, done, readRecord: () => JSON.parse(readFileSync(record, 'utf8')) };
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
  assert.deepEqual(config.model, { provider: 'colonizer-deepseek', default: 'deepseek-flash' });
  assert.deepEqual(config.providers, {
    'colonizer-deepseek': {
      api: 'http://host.microsandbox.internal:41750/providers/deepseek',
      transport: 'anthropic_messages',
      extra_headers: { 'x-colonizer-colony': 'tok-1' },
    },
  });
  assert.deepEqual(hermesConfig([]).agent.disabled_toolsets, DISABLED_TOOLSETS);
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

test('an answer command warns: headless Hermes has no question channel', async () => {
  const h = harness();
  h.commands.push({ type: 'answer', question_id: 'q1', answers: {}, response: null });
  await waitFor(h.events, (e) => e.type === 'log' && /question channel/.test(e.message), 'warn log');
  h.commands.push({ type: 'shutdown' });
  await h.done;
  assertConforms(h.events);
});
