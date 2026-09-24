import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { test } from 'node:test';

import {
  buildModelsConfig,
  commandQueue,
  EFFORT_LEVELS,
  lfSplitter,
  parseRoutes,
  piArgs,
  piEnv,
  runAgent,
  runUnconfigured,
  selectionProblem,
  SYSTEM_PROMPT_APPEND,
  toolResultText,
} from '../runner.mjs';

const ROUTES = parseRoutes(
  JSON.stringify([
    { provider: 'deepseek', prefix: 'deepseek/', base_url: 'http://host:41750/providers/deepseek', auth: 'none', headers: { 'x-colonizer-colony': 'tok-1' }, context_tokens: 131072 },
    { provider: 'fake', prefix: 'fake/', base_url: 'http://127.0.0.1:9/providers/fake', auth: 'none' },
  ]),
).routes;
const SELECTION = { route: ROUTES[1], provider: 'fake', modelId: 'some-model' };

test('parseRoutes validates the route array like the claude-code router does', () => {
  assert.deepEqual(ROUTES.map((route) => route.provider), ['deepseek', 'fake']);
  assert.deepEqual(ROUTES[0].headers, { 'x-colonizer-colony': 'tok-1' });
  assert.equal(ROUTES[0].context_tokens, 131072);
  assert.equal(parseRoutes('not json').routes.length, 0);
  assert.equal(parseRoutes('{}').routes.length, 0);
  const bad = parseRoutes(JSON.stringify([{ prefix: 'x', base_url: 'ftp://x', auth: 'wat' }]));
  assert.equal(bad.routes.length, 0);
  assert.match(bad.warnings[0], /ignoring model route 0/);
});

test('selectionProblem names the fix for every unusable model setting', () => {
  for (const detail of [selectionProblem([], 'x/y'), selectionProblem(ROUTES, ''), selectionProblem(ROUTES, 'claude-opus-5')]) {
    assert.match(detail, /Pi reaches models only through the provider gateway/);
    assert.match(detail, /Settings → Providers/);
  }
  assert.match(selectionProblem([], 'x/y'), /no provider is configured/);
  assert.match(selectionProblem(ROUTES, ''), /model setting is empty/);
  assert.match(selectionProblem(ROUTES, 'claude-opus-5'), /matches none of the configured providers/);
  assert.equal(selectionProblem(ROUTES, 'fake/some-model'), null);
});

test('buildModelsConfig writes the selected model on its gateway provider', () => {
  const provider = buildModelsConfig(ROUTES, 'deepseek/deepseek-chat', 'high').providers.deepseek;
  assert.equal(provider.baseUrl, 'http://host:41750/providers/deepseek');
  assert.equal(provider.api, 'anthropic-messages');
  assert.equal(typeof provider.apiKey, 'string'); // a placeholder; the gateway drops it
  assert.deepEqual(provider.headers, { 'x-colonizer-colony': 'tok-1' });
  assert.deepEqual(provider.models, [{ id: 'deepseek-chat', name: 'deepseek-chat', api: 'anthropic-messages', reasoning: true, contextWindow: 131072 }]);
  const model = (effort) => buildModelsConfig(ROUTES, 'fake/m', effort).providers.fake.models[0];
  assert.equal(model('').reasoning, undefined); // reasoning only when thinking is requested
  assert.equal(model('off').reasoning, undefined);
  assert.equal(model('minimal').reasoning, true);
  assert.deepEqual(model('high'), { id: 'm', name: 'm', api: 'anthropic-messages', reasoning: true }); // no context_tokens on the fake route
});

test('piArgs builds the RPC command line; piEnv pins the agent dir and hides the token', () => {
  const base = ['--no-session', '--no-extensions', '--no-skills', '--no-prompt-templates', '--provider', 'fake', '--model', 'm', '--append-system-prompt', SYSTEM_PROMPT_APPEND];
  assert.deepEqual(piArgs({ provider: 'fake', modelId: 'm' }), base);
  assert.deepEqual(piArgs({ provider: 'fake', modelId: 'm', effort: 'max' }), [...base.slice(0, 8), '--thinking', 'max', ...base.slice(8)]);
  assert.ok(EFFORT_LEVELS.has('minimal') && !EFFORT_LEVELS.has(''));
  assert.deepEqual(piEnv({ COLONIZER_MODEL_ROUTES: '["...token..."]', HOME: '/root' }, '/tmp/pi-dir'), {
    HOME: '/root',
    PI_CODING_AGENT_DIR: '/tmp/pi-dir',
    PI_OFFLINE: '1',
    PI_SKIP_VERSION_CHECK: '1',
    PI_TELEMETRY: '0',
  }); // the gateway token never reaches pi or its shell commands
});

test('toolResultText flattens and caps; lfSplitter splits on LF bytes only', () => {
  assert.equal(toolResultText({ content: [{ type: 'text', text: 'a' }, { type: 'image', data: '...' }] }), 'a\n[image]');
  assert.equal(toolResultText('plain'), 'plain');
  assert.equal(toolResultText(null), '');
  const big = toolResultText({ content: [{ type: 'text', text: 'x'.repeat(25_000) }] });
  assert.ok(big.length <= 20_000 && /truncated 5\d\d\d characters/.test(big));
  const lines = [];
  const feed = lfSplitter((line) => lines.push(line));
  const payload = JSON.stringify({ text: 'a b c' }); // U+2028/U+2029 stay inside the line
  feed(Buffer.from(`{"a":1}\r\n${payload}\n{"b"`));
  feed(Buffer.from(':2}\n'));
  assert.deepEqual(lines, ['{"a":1}', payload, '{"b":2}']);
});

// The required fields of docs/agent-events.schema.json, checked on every event the tests emit.
const REQUIRED = {
  status: ['type', 'state'],
  user_message: ['type', 'id', 'text'],
  assistant_text_delta: ['type', 'message_id', 'block_index', 'delta'],
  assistant_text: ['type', 'message_id', 'block_index', 'text'],
  thinking: ['type', 'message_id', 'block_index', 'text'],
  tool_call: ['type', 'message_id', 'tool_call_id', 'name', 'input'],
  tool_result: ['type', 'tool_call_id', 'output', 'is_error'],
  turn_end: ['type', 'is_error', 'result', 'cost_usd', 'duration_ms'],
  log: ['type', 'level', 'message'],
  model_changed: ['type', 'model', 'previous'],
};

/** A child-process stand-in speaking pi's RPC stream; `onCommand` scripts each test. */
class FakePi extends EventEmitter {
  constructor() {
    super();
    this.stdin = new PassThrough();
    this.stdout = new PassThrough();
    this.stderr = new PassThrough();
    this.commands = [];
    this.spawnArgs = null;
    this.stdin.on('data', lfSplitter((line) => {
      if (!line.trim()) return;
      const command = JSON.parse(line);
      this.commands.push(command);
      this.onCommand?.(command);
    }));
    this.stdin.on('end', () => this.exit(0)); // an orderly shutdown closes pi's stdin, and pi exits
  }

  record(object) {
    this.stdout.write(`${JSON.stringify(object)}\n`);
  }

  respond(command) {
    this.record({ id: command.id, type: 'response', command: command.type, success: true });
  }

  exit(code = 0) {
    this.exitCode = code;
    this.stdout.end();
    this.stderr.end();
    this.emit('exit', code, null);
  }

  kill() {
    this.exit(null);
  }
}

/** The pi records of one assistant response, in the shapes verified against pi 0.87.1. */
function assistantResponse({ text = 'Hello', toolCall = null, stopReason = 'stop', errorMessage } = {}) {
  const content = [];
  const updates = [];
  if (toolCall) {
    updates.push({ type: 'message_update', assistantMessageEvent: { type: 'toolcall_start', contentIndex: 0, id: toolCall.id } });
    content.push({ type: 'toolCall', id: toolCall.id, name: toolCall.name, arguments: toolCall.arguments });
  } else {
    updates.push({ type: 'message_update', assistantMessageEvent: { type: 'text_delta', contentIndex: 0, delta: text } });
    content.push({ type: 'text', text });
  }
  const message = { role: 'assistant', content, provider: 'fake', model: 'some-model', stopReason, usage: { input: 10, output: 5, cacheRead: 2, cacheWrite: 1, cost: { total: 0 } } };
  if (errorMessage !== undefined) message.errorMessage = errorMessage;
  return [{ type: 'message_start', message: { role: 'assistant', content: [] } }, ...updates, { type: 'message_end', message }];
}

/** Wires runAgent around a FakePi whose `onCommand` is set by the caller; checks every event. */
function harness(pi) {
  const events = [];
  const commands = commandQueue();
  const done = runAgent({
    spawnPi: ({ args }) => ((pi.spawnArgs = args), pi),
    commands,
    emit: (event) => {
      assert.ok(REQUIRED[event.type], `unknown event type ${event.type}`);
      for (const field of REQUIRED[event.type]) assert.ok(field in event, `${event.type} is missing ${field}`);
      events.push(event);
    },
    selection: SELECTION,
    graceMs: 50,
  });
  return { pi, events, commands, done };
}

/** A session whose every prompt is answered by `runs` records followed by agent_settled. */
function session(runs) {
  const pi = new FakePi();
  pi.onCommand = (command) => {
    pi.respond(command);
    if (command.type === 'prompt') {
      pi.record({ type: 'agent_start' });
      for (const record of runs.flat()) pi.record(record);
      pi.record({ type: 'agent_settled' });
    }
  };
  const h = harness(pi);
  return { ...h, send: h.commands.push.bind(h.commands), finish: async () => { h.commands.push({ type: 'shutdown' }); await h.done; } };
}

const tick = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const types = (events) => events.map((event) => event.type);

test('a turn streams text, reports a tool call and capped result, and settles once', async () => {
  const s = await session([
    assistantResponse({ toolCall: { id: 'call_1', name: 'bash', arguments: { command: 'ls' } }, stopReason: 'toolUse' }),
    [{ type: 'tool_execution_end', toolCallId: 'call_1', result: { content: [{ type: 'text', text: 'x'.repeat(21_000) }] }, isError: false }],
    assistantResponse({ text: 'Ran ls.' }),
  ]);
  s.send({ type: 'user_message', id: 'initial', text: 'run ls' });
  await tick(50);
  await s.finish();

  assert.deepEqual(types(s.events), [
    'status', 'model_changed', 'log', 'user_message', 'status',
    'tool_call', 'tool_result', 'assistant_text_delta', 'assistant_text',
    'turn_end', 'status', 'status',
  ]);
  assert.deepEqual(s.events[1], { type: 'model_changed', model: 'fake/some-model', previous: null });
  assert.deepEqual(s.events.find((event) => event.type === 'tool_call'), { type: 'tool_call', message_id: 'pi-1', tool_call_id: 'call_1', name: 'bash', input: { command: 'ls' } });
  const result = s.events.find((event) => event.type === 'tool_result');
  assert.ok(result.output.length <= 20_000 && /truncated 1\d{3} characters/.test(result.output));
  const turn = s.events.find((event) => event.type === 'turn_end');
  assert.equal(turn.is_error, false);
  assert.equal(turn.result, 'Ran ls.'); // the last assistant text, after both messages
  assert.deepEqual(turn.model_usage, { 'fake/some-model': { input_tokens: 20, output_tokens: 10, cache_read_tokens: 4, cache_write_tokens: 2 } });
  assert.deepEqual(s.events.filter((event) => event.type === 'status').map((event) => event.state), ['idle', 'working', 'idle', 'exited']);
  assert.deepEqual(s.pi.spawnArgs, piArgs({ provider: 'fake', modelId: 'some-model' }));
});

test('a provider error turn fails the turn with the error message', async () => {
  const s = await session([assistantResponse({ text: '', stopReason: 'error', errorMessage: '404 status code (no body)' })]);
  s.send({ type: 'user_message', text: 'hi' });
  await tick(50);
  await s.finish();
  const turn = s.events.find((event) => event.type === 'turn_end');
  assert.equal(turn.is_error, true);
  assert.equal(turn.result, '404 status code (no body)');
});

test('a message arriving mid-turn queues as a follow-up and ends in the same turn', async () => {
  const pi = new FakePi();
  // The run stays open (no message_end, no agent_settled) until the test settles it.
  pi.onCommand = (command) => {
    pi.respond(command);
    if (command.type !== 'prompt' || pi.commands.filter((c) => c.type === 'prompt').length > 1) return;
    pi.record({ type: 'agent_start' });
    pi.record({ type: 'message_start', message: { role: 'assistant', content: [] } });
    pi.record({ type: 'message_update', assistantMessageEvent: { type: 'text_delta', contentIndex: 0, delta: 'wor' } });
  };
  const { events, commands, done } = harness(pi);
  commands.push({ type: 'user_message', text: 'one' });
  await tick(20);
  commands.push({ type: 'user_message', text: 'two' });
  await tick(20);

  const prompts = pi.commands.filter((command) => command.type === 'prompt');
  assert.deepEqual(prompts.map((prompt) => prompt.streamingBehavior), [undefined, 'followUp']);

  pi.record({ type: 'message_end', message: { role: 'assistant', content: [{ type: 'text', text: 'done' }], provider: 'fake', model: 'some-model', stopReason: 'stop', usage: { input: 1, output: 1, cost: { total: 0 } } } });
  pi.record({ type: 'agent_settled' });
  await tick(20);
  commands.push({ type: 'shutdown' });
  await done;

  const turns = events.filter((event) => event.type === 'turn_end');
  assert.equal(turns.length, 1); // the turn ends once, after the run settles; the second message joined it
  assert.equal(turns[0].result, 'done');
});

test('a response and its agent_settled read in one chunk keep the bookkeeping ordered', async () => {
  // Regression: accepting a prompt in a promise .then raced with the agent_settled arriving in the
  // same stdout chunk; the leaked counter sent the next message as a followUp of a turn that had
  // already ended.
  const pi = new FakePi();
  pi.onCommand = (command) => {
    if (command.type !== 'prompt') return pi.respond(command);
    pi.stdout.write( // one write, one 'data' chunk: the response and the whole run together
      [JSON.stringify({ id: command.id, type: 'response', command: 'prompt', success: true }), ...assistantResponse({ text: 'hi' }).map(JSON.stringify), '{"type":"agent_settled"}'].join('\n') + '\n',
    );
  };
  const { events, commands, done } = harness(pi);
  commands.push({ type: 'user_message', text: 'one' });
  await tick(20);
  commands.push({ type: 'user_message', text: 'two' });
  await tick(20);
  commands.push({ type: 'shutdown' });
  await done;

  // The first chunk closed its turn synchronously, so the second message starts a plain new one.
  assert.equal(events.filter((event) => event.type === 'turn_end').length, 2);
  assert.equal(pi.commands.filter((command) => command.type === 'prompt')[1].streamingBehavior, undefined);
});

test('two user_messages in one tick: the second prompt waits for the first response, then follows up', async () => {
  // Regression: both prompts used to be written at once, before either acceptance arrived, and pi
  // dropped the second while reporting success.
  const pi = new FakePi();
  pi.onCommand = (command) => {
    if (command.type !== 'prompt') return pi.respond(command);
    if (pi.commands.filter((c) => c.type === 'prompt').length > 1) return pi.respond(command);
    pi.record({ type: 'agent_start' }); // the first run starts, but its response is withheld
    pi.record({ type: 'message_start', message: { role: 'assistant', content: [] } });
  };
  const { events, commands, done } = harness(pi);
  commands.push({ type: 'user_message', text: 'one' });
  commands.push({ type: 'user_message', text: 'two' });
  await tick(20);
  assert.deepEqual(pi.commands.filter((c) => c.type === 'prompt').map((c) => c.message), ['one']); // two is not written before one is accepted

  pi.respond(pi.commands.find((c) => c.type === 'prompt')); // pi accepts the first message
  await tick(20);
  assert.deepEqual(
    pi.commands.filter((c) => c.type === 'prompt').map((c) => [c.message, c.streamingBehavior]),
    [['one', undefined], ['two', 'followUp']], // the run is still open, so two joins it
  );

  pi.record({ type: 'message_end', message: { role: 'assistant', content: [{ type: 'text', text: 'both' }], provider: 'fake', model: 'some-model', stopReason: 'stop', usage: { input: 1, output: 1, cost: { total: 0 } } } });
  pi.record({ type: 'agent_settled' });
  await tick(20);
  commands.push({ type: 'shutdown' });
  await done;
  const turns = events.filter((event) => event.type === 'turn_end');
  assert.equal(turns.length, 1); // both messages were one turn
  assert.equal(turns[0].result, 'both');
});

test('a refused prompt fails the turn instead of hanging it', async () => {
  const pi = new FakePi();
  pi.onCommand = (command) => {
    if (command.type === 'prompt') pi.record({ id: command.id, type: 'response', command: 'prompt', success: false, error: 'nope' });
  };
  const { events, commands, done } = harness(pi);
  commands.push({ type: 'user_message', text: 'hi' });
  await tick(30);
  commands.push({ type: 'shutdown' });
  await done;
  const turn = events.find((event) => event.type === 'turn_end');
  assert.equal(turn.is_error, true);
  assert.match(turn.result, /pi refused the message: nope/);
});

test('interrupt aborts; set_model switches only to the startup model; answer warns', async () => {
  const s = await session([assistantResponse({ text: 'Interrupted', stopReason: 'aborted' })]);
  s.send({ type: 'user_message', id: 'initial', text: 'go' });
  await tick(20);
  s.send({ type: 'interrupt' });
  s.send({ type: 'answer', question_id: 'q1', answers: {} });
  s.send({ type: 'set_model', model: 'claude/opus' });
  s.send({ type: 'set_model', model: 'nonsense' });
  s.send({ type: 'set_model', model: 'fake/some-model' }); // the one model models.json lists
  await tick(30);
  await s.finish();

  assert.equal(s.pi.commands.some((command) => command.type === 'abort'), true);
  assert.equal(s.events.find((event) => event.type === 'turn_end').is_error, true); // aborted reads as interrupted
  const logs = s.events.filter((event) => event.type === 'log').map((event) => event.message);
  assert.ok(logs.some((message) => /ignored an answer/.test(message)));
  assert.ok(logs.some((message) => /set_model claude\/opus failed: pi read models\.json only at startup and knows just fake\/some-model/.test(message)));
  assert.ok(logs.some((message) => /set_model nonsense failed/.test(message)));
  assert.deepEqual(s.pi.commands.find((command) => command.type === 'set_model'), { id: 'req-3', type: 'set_model', provider: 'fake', modelId: 'some-model' });
  assert.deepEqual(s.events.filter((event) => event.type === 'model_changed'), [
    { type: 'model_changed', model: 'fake/some-model', previous: null },
    { type: 'model_changed', model: 'fake/some-model', previous: 'fake/some-model' },
  ]);
});

test('pi stderr is logged as whole lines, with multi-byte characters intact across chunks', async () => {
  const pi = new FakePi();
  const { events, commands, done } = harness(pi);
  const bytes = Buffer.from('warn: café\npar', 'utf8'); // é is two bytes, split by the chunk boundary
  pi.stderr.write(bytes.subarray(0, 10));
  pi.stderr.write(bytes.subarray(10));
  await tick(10);
  commands.push({ type: 'shutdown' });
  await done;
  const logs = events.filter((event) => event.type === 'log' && event.level === 'warn').map((event) => event.message);
  assert.deepEqual(logs, ['pi: warn: café']); // one whole line; the one without its LF still waits
});

test('a pi crash mid-turn logs, fails the open turn and exits', async () => {
  const pi = new FakePi();
  pi.onCommand = (command) => {
    if (command.type !== 'prompt') return pi.respond(command);
    pi.respond(command);
    pi.record({ type: 'agent_start' });
    pi.record({ type: 'message_start', message: { role: 'assistant', content: [] } });
    pi.exit(7); // the process dies before any message_end
  };
  const { events, commands, done } = harness(pi);
  commands.push({ type: 'user_message', text: 'hi' });
  await done;

  assert.ok(events.some((event) => event.type === 'log' && event.level === 'error' && /pi exited unexpectedly \(code 7\)/.test(event.message)));
  assert.equal(events.find((event) => event.type === 'turn_end').is_error, true);
  assert.match(events.find((event) => event.type === 'turn_end').result, /pi exited unexpectedly/);
  assert.deepEqual(events.filter((event) => event.type === 'status').map((event) => event.state), ['idle', 'working', 'error', 'exited']);
});

test('a spawn failure is reported like an unexpected exit instead of hanging', async () => {
  const pi = new FakePi(); // never exited and never answering: only the error event ever fires
  const { events, done } = harness(pi);
  pi.emit('error', new Error('spawn pi ENOENT'));
  await done;
  assert.ok(events.some((event) => event.type === 'log' && event.level === 'error' && /pi failed to start \(spawn pi ENOENT\)/.test(event.message)));
  assert.equal(events.some((event) => event.type === 'turn_end'), false); // no turn was open to fail
  assert.deepEqual(events.filter((event) => event.type === 'status').map((event) => event.state), ['idle', 'error', 'exited']);
});

test('without a usable route no pi is started and every message fails its turn', async () => {
  const detail = 'no provider is configured, so Pi has no model to run on.';
  const events = [];
  const commands = commandQueue();
  const done = runUnconfigured({ commands, emit: (event) => events.push(event), detail });
  commands.push({ type: 'user_message', text: 'hello' }); // no id: the runner must mint one
  commands.push({ type: 'answer', question_id: 'q', answers: {} });
  commands.push({ type: 'set_model', model: 'a/b' });
  commands.push({ type: 'shutdown' });
  await done;

  assert.deepEqual(types(events), ['log', 'status', 'user_message', 'turn_end', 'log', 'log', 'status']);
  assert.deepEqual(events[1], { type: 'status', state: 'error', detail });
  assert.deepEqual(events[2], { type: 'user_message', id: 'u-1', text: 'hello' });
  assert.deepEqual(events[3], { type: 'turn_end', is_error: true, result: detail, cost_usd: 0, duration_ms: 0 });
  assert.ok(events[4].message.includes('ignored an answer'));
  assert.ok(events[5].message.includes('ignored a set_model'));
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
});
