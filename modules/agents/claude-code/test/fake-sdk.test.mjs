import assert from 'node:assert/strict';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';

import { z } from 'zod';

import { DENIAL_HINTS } from '../denials.mjs';
import { createFindingsServer } from '../findings.mjs';
import { createMemoryServer } from '../memory.mjs';
import {
  AsyncQueue,
  buildOptions as buildOptionsWithDefaults,
  CAVEMAN_LEVELS,
  cavemanPrompt,
  childEnv,
  CHOICE_NUDGE,
  endsWithQuestion,
  MAX_TOOL_OUTPUT,
  rtkRewrite,
  riskClass,
  runAgent,
  SUPERPOWERS_COLONIZER_NOTE,
  SUPERPOWERS_SKILL,
  superpowersBootstrap,
  SYSTEM_PROMPT_APPEND,
  toolResultText,
} from '../runner.mjs';

// These tests cover other features. Delegation is enforced by default and has its own tests in
// delegate.test.mjs, so it is switched off here unless a test asks for it.
const buildOptions = (env = {}, extra) => buildOptionsWithDefaults({ COLONIZER_DELEGATE: 'off', ...env }, extra);

const stream = (event) => ({ type: 'stream_event', event, parent_tool_use_id: null });
const assistant = (id, content) => ({ type: 'assistant', message: { id, role: 'assistant', content }, parent_tool_use_id: null });
const user = (content) => ({ type: 'user', message: { role: 'user', content }, parent_tool_use_id: null });

/** A fake Agent SDK `query`: runs `turn` once per prompt message. */
function fakeQuery(turn) {
  const calls = { prompts: [], decisions: [], interrupts: 0, models: [], closed: false, options: null };
  const query = ({ prompt, options }) => {
    calls.options = options;
    const generator = (async function* () {
      for await (const message of prompt) {
        calls.prompts.push(message);
        yield* turn(options, calls);
      }
    })();
    generator.interrupt = async () => {
      calls.interrupts += 1;
    };
    generator.setModel = async (model) => {
      calls.models.push(model);
    };
    generator.close = () => {
      calls.closed = true;
    };
    return generator;
  };
  return { query, calls };
}

const askInput = {
  questions: [
    {
      question: 'Which file name?',
      header: 'File',
      multiSelect: false,
      options: [
        { label: 'hello.txt', description: 'Classic' },
        { label: 'hi.txt', description: 'Short', preview: 'hi' },
      ],
    },
  ],
};

/**
 * The colony's two in-process MCP servers, wired to one event stream, with the tool defs captured so
 * the fake turn can call them the way Claude Code would. Real zod, so the proposals and findings
 * that reach the test have passed the runner's own validation.
 */
function contractTools(emit) {
  const defs = [];
  const tool = (name, description, shape, handler) => {
    const def = { name, description, shape, handler };
    defs.push(def);
    return def;
  };
  const createSdkMcpServer = (server) => server;
  // memory_propose never reads the mount; only memory_search does, and the turn never calls it.
  createMemoryServer({ dir: join(tmpdir(), 'colonizer-contract-memory'), emit, createSdkMcpServer, tool, z });
  createFindingsServer({ emit, createSdkMcpServer, tool, z });
  const byName = (name) => defs.find((def) => def.name === name);
  return { propose: byName('memory_propose'), file: byName('finding_file') };
}

async function* issueTurn(options, calls) {
  yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
  yield stream({ type: 'message_start', message: { id: 'msg_1' } });
  yield stream({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
  yield stream({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Let me ' } });
  yield stream({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'ask.' } });
  yield assistant('msg_1', [{ type: 'text', text: 'Let me ask.' }]);
  yield stream({ type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'toolu_ask', name: 'AskUserQuestion' } });
  yield assistant('msg_1', [{ type: 'tool_use', id: 'toolu_ask', name: 'AskUserQuestion', input: askInput }]);
  calls.decisions.push(
    await options.canUseTool('AskUserQuestion', askInput, { signal: new AbortController().signal, toolUseID: 'toolu_ask' }),
  );
  yield user([{ type: 'tool_result', tool_use_id: 'toolu_ask', content: 'User has answered your questions' }]);

  yield stream({ type: 'message_start', message: { id: 'msg_2' } });
  yield stream({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'toolu_bash', name: 'Bash' } });
  const bashInput = { command: 'echo hi > hello.txt' };
  yield assistant('msg_2', [{ type: 'thinking', thinking: 'Create hello.txt with a shell redirect.' }, { type: 'tool_use', id: 'toolu_bash', name: 'Bash', input: bashInput }]);
  calls.decisions.push(
    await options.canUseTool('Bash', bashInput, { signal: new AbortController().signal, toolUseID: 'toolu_bash' }),
  );
  yield user([{ type: 'tool_result', tool_use_id: 'toolu_bash', content: [{ type: 'text', text: 'x'.repeat(25_000) }] }]);
  // The colony's own tools emit protocol events too: a shared-memory proposal (§6.2) and a finding (§6.6).
  await calls.tools.propose.handler({ scope: 'repo', title: 'Redirect with > to create files', content: 'The colony image ships a plain shell; `>` creates the file without a heredoc.', tags: ['workspace'] });
  await calls.tools.file.handler({ title: 'hello.txt is written outside /harness/out', body: 'The turn writes `hello.txt` into the worktree root, where it would land in the pull request.', evidence: 'This turn: the Bash call above runs `echo hi > hello.txt` and the file appears in the worktree.' });
  // An errored tool result carries the denial layer's annotation (denials.mjs) without is_error or
  // the output being touched.
  yield assistant('msg_3', [{ type: 'tool_use', id: 'toolu_fetch', name: 'Bash', input: { command: 'git fetch origin' } }]);
  yield user([
    {
      type: 'tool_result',
      tool_use_id: 'toolu_fetch',
      content: [{ type: 'text', text: "fatal: unable to access 'https://github.com/anthropics/colonizer/': Could not resolve host" }],
      is_error: true,
    },
  ]);
  // modelUsage is per model: `cost_usd` sums the Claude models only, `model_usage` reports the rest as tokens.
  yield {
    type: 'result',
    subtype: 'success',
    is_error: false,
    result: 'Created hello.txt',
    total_cost_usd: 0.01,
    duration_ms: 1234,
    modelUsage: {
      'claude-opus-5': { inputTokens: 1200, outputTokens: 300, cacheReadInputTokens: 90_000, cacheCreationInputTokens: 8_000, costUSD: 0.42 },
    },
  };
}

test('maps a turn with a question to protocol events', async () => {
  const { query, calls } = fakeQuery(issueTurn);
  const events = [];
  calls.tools = contractTools((e) => events.push(e));
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'question') {
      const [q] = event.questions;
      commands.push({ type: 'answer', question_id: event.question_id, answers: { [q.question]: q.options[0].label } });
    }
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', id: 'initial', text: 'Fix the issue' });

  await runAgent({ query, commands, emit, options: { model: 'fake' }, graceMs: 100 });

  const ofType = (type) => events.filter((e) => e.type === type);
  assert.deepEqual(events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
  assert.deepEqual(ofType('user_message'), [{ type: 'user_message', id: 'initial', text: 'Fix the issue' }]);
  assert.deepEqual(calls.prompts, [{ type: 'user', message: { role: 'user', content: 'Fix the issue' }, parent_tool_use_id: null }]);
  assert.equal(calls.options.model, 'fake');

  assert.deepEqual(
    ofType('assistant_text_delta').map((e) => [e.message_id, e.block_index, e.delta]),
    [['msg_1', 0, 'Let me '], ['msg_1', 0, 'ask.']],
  );
  assert.deepEqual(ofType('assistant_text'), [{ type: 'assistant_text', message_id: 'msg_1', block_index: 0, text: 'Let me ask.' }]);

  assert.deepEqual(ofType('question'), [
    {
      type: 'question',
      question_id: 'toolu_ask',
      message_id: 'msg_1',
      risk: 'workspace_write',
      questions: [
        {
          question: 'Which file name?',
          header: 'File',
          multi_select: false,
          options: [
            { label: 'hello.txt', description: 'Classic', preview: null },
            { label: 'hi.txt', description: 'Short', preview: 'hi' },
          ],
        },
      ],
    },
  ]);
  assert.deepEqual(ofType('question_answered'), [
    { type: 'question_answered', question_id: 'toolu_ask', answers: { 'Which file name?': 'hello.txt' }, response: null },
  ]);
  assert.deepEqual(calls.decisions[0], {
    behavior: 'allow',
    updatedInput: { ...askInput, answers: { 'Which file name?': 'hello.txt' } },
  });
  assert.equal(calls.decisions[1].behavior, 'allow');

  // AskUserQuestion never leaks as a regular tool call or result.
  assert.deepEqual(ofType('tool_call'), [
    { type: 'tool_call', message_id: 'msg_2', tool_call_id: 'toolu_bash', name: 'Bash', input: { command: 'echo hi > hello.txt' } },
    { type: 'tool_call', message_id: 'msg_3', tool_call_id: 'toolu_fetch', name: 'Bash', input: { command: 'git fetch origin' } },
  ]);
  const results = ofType('tool_result');
  assert.equal(results.length, 2);
  assert.equal(results[0].tool_call_id, 'toolu_bash');
  assert.equal(results[0].is_error, false);
  assert.ok(results[0].output.length <= MAX_TOOL_OUTPUT);
  assert.match(results[0].output, /truncated 5000 characters\]$/);
  // An errored result is annotated with the denial layer's guidance, and nothing else about it moves.
  assert.equal(results[1].tool_call_id, 'toolu_fetch');
  assert.equal(results[1].is_error, true);
  assert.deepEqual(results[1].denial, { class: 'egress', hint: DENIAL_HINTS.egress });

  assert.deepEqual(ofType('turn_end'), [
    {
      type: 'turn_end',
      is_error: false,
      result: 'Created hello.txt',
      cost_usd: 0.42,
      duration_ms: 1234,
      model_usage: { 'claude-opus-5': { input_tokens: 1200, output_tokens: 300, cache_read_tokens: 90_000, cache_write_tokens: 8_000 } },
    },
  ]);

  const states = ofType('status').map((e) => e.state);
  assert.deepEqual(states, ['idle', 'working', 'waiting_for_answer', 'working', 'idle', 'exited']);
  const turnEnd = events.findIndex((e) => e.type === 'turn_end');
  assert.deepEqual(events[turnEnd + 1], { type: 'status', state: 'idle' });
});

test('every question is risk-classified by rounding up across all of its text', () => {
  const q = (question, header = '', options = []) => [{ question, header, options }];
  assert.equal(riskClass(q('Which file name do you want?')), 'workspace_write', 'the default');
  assert.equal(riskClass([]), 'workspace_write');
  assert.equal(riskClass(q('Shall I copy the API key into the .env file?')), 'credential_adjacent');
  assert.equal(riskClass(q('Rotate the ssh key too?', 'Secrets')), 'credential_adjacent');
  assert.equal(riskClass(q('Proceed?', 'Deploy', [{ label: 'Yes', description: 'Push to main and tag the release' }])), 'publish_affecting');
  assert.equal(riskClass(q('Open the pull request once tests pass?')), 'publish_affecting');
  // Highest class wins wherever it appears, and word boundaries keep a tokenizer from being a token.
  assert.equal(riskClass([...q('Use which tokenizer library?'), ...q('And where do the API keys live?')]), 'credential_adjacent');
});

test('a full turn emits exactly the committed contract fixture, so runner drift fails here', async () => {
  // The fixture is the same turn, one event per line, in order. The Rust harness deserialises the
  // same file into its AgentEvent enum (crates/colonizer/src/protocol.rs) against the JSON Schema
  // (docs/agent-events.schema.json), which makes this file the seam between the JS runner and the
  // Rust harness: a changed event shape fails a test on both sides.
  const { query, calls } = fakeQuery(issueTurn);
  const events = [];
  calls.tools = contractTools((e) => events.push(e));
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'question') {
      const [q] = event.questions;
      commands.push({ type: 'answer', question_id: event.question_id, answers: { [q.question]: q.options[0].label } });
    }
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', id: 'initial', text: 'Fix the issue' });

  await runAgent({ query, commands, emit, options: { model: 'fake' }, graceMs: 100 });

  const fixture = readFileSync(new URL('./fixtures/events.jsonl', import.meta.url), 'utf8')
    .trimEnd()
    .split('\n')
    .map((line) => JSON.parse(line));
  assert.deepEqual(events, fixture);

  // Every event type of the protocol appears, and nothing outside it. The schema is not validated
  // here — node has no JSON-Schema validator and this module's tests stay on a lean npm ci — so
  // this type census is what keeps the fixture covering the whole contract, not a subset.
  const protocolTypes = [
    'status',
    'user_message',
    'assistant_text_delta',
    'assistant_text',
    'thinking',
    'tool_call',
    'tool_result',
    'question',
    'question_answered',
    'agent_session',
    'turn_end',
    'log',
    'model_changed',
    'memory_proposal',
    'finding',
  ];
  const types = new Set(fixture.map((e) => e.type));
  for (const type of protocolTypes) assert.ok(types.has(type), `the fixture has no ${type} event`);
  assert.deepEqual([...types].sort(), protocolTypes.slice().sort());
});

test('a resumed session resumes the reported session id and delivers the answer', async () => {
  // Issue #562: a colony suspended while it waited on its user boots to deliver the answer — the
  // session id travels as an SDK option, the answer as the first user message, so the conversation
  // continues where the suspended one stopped. Without an id to resume, no option is set.
  const { options } = buildOptions({ COLONIZER_RESUME_SESSION: 's1' });
  assert.equal(options.resume, 's1');
  const { options: fresh } = buildOptions({});
  assert.equal(fresh.resume, undefined);

  const { query, calls } = fakeQuery(async function* () {
    yield { type: 'system', subtype: 'init', session_id: 's1', model: 'fake-model' };
    yield { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
  });
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', id: 'initial', text: 'Q: Which file name?\nA: hello.txt' });

  await runAgent({ query, commands, emit, options, graceMs: 100 });

  assert.equal(calls.options.resume, 's1');
  assert.deepEqual(calls.prompts, [
    { type: 'user', message: { role: 'user', content: 'Q: Which file name?\nA: hello.txt' }, parent_tool_use_id: null },
  ]);
  // The id the resumed session reports is announced, and it is the one being resumed.
  assert.deepEqual(events.filter((e) => e.type === 'agent_session'), [{ type: 'agent_session', session_id: 's1' }]);
});

test('interrupt reaches the query, unknown answers are logged, EOF exits', async () => {
  const { query, calls } = fakeQuery(async function* () {});
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'interrupt' });
  commands.push({ type: 'answer', question_id: 'nope', answers: {} });
  commands.push({ type: 'something_new' });
  commands.close(); // stdin EOF

  await runAgent({ query, commands, emit: (e) => events.push(e), graceMs: 100 });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(calls.interrupts, 1);
  assert.ok(events.some((e) => e.type === 'log' && e.level === 'warn' && e.message.includes('nope')));
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
});

test('set_model switches the live query and announces the model it replaced', async () => {
  // init names the model in effect, as Claude Code's does at the start of each turn.
  const { query, calls } = fakeQuery(async function* (options, c) {
    yield { type: 'system', subtype: 'init', session_id: 's1', model: c.models.at(-1) ?? 'claude-opus-5-5' };
    yield { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
  });
  const events = [];
  const commands = new AsyncQueue();
  let turns = 0;
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push(turns++ ? { type: 'shutdown' } : { type: 'set_model', model: ' deepseek/deepseek-flash ' });
    if (event.type === 'model_changed' && event.previous) commands.push({ type: 'user_message', text: 'again' });
  };
  commands.push({ type: 'user_message', text: 'go' });

  await runAgent({ query, commands, emit, graceMs: 100 });

  assert.deepEqual(calls.models, ['deepseek/deepseek-flash']);
  assert.equal(calls.prompts.length, 2, 'the same query carries on after the switch');
  // The second turn's init names the model already announced, so it is not news.
  assert.deepEqual(
    events.filter((e) => e.type === 'model_changed'),
    [
      { type: 'model_changed', model: 'claude-opus-5-5', previous: null },
      { type: 'model_changed', model: 'deepseek/deepseek-flash', previous: 'claude-opus-5-5' },
    ],
  );
});

test('a set_model the SDK refuses, or one without a model, is a warning and changes nothing', async () => {
  const { query } = fakeQuery(async function* () {});
  const refusing = (args) =>
    Object.assign(query(args), {
      setModel: async (model) => {
        throw new Error(`unknown model ${model}`);
      },
    });
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'set_model', model: 'nope' });
  commands.push({ type: 'set_model', model: '  ' });
  commands.close();

  await runAgent({ query: refusing, commands, emit: (e) => events.push(e), graceMs: 100 });
  await new Promise((resolve) => setImmediate(resolve));

  assert.ok(!events.some((e) => e.type === 'model_changed'));
  const warnings = events.filter((e) => e.type === 'log' && e.level === 'warn').map((e) => e.message);
  assert.ok(warnings.some((m) => m.includes('unknown model nope')), warnings.join('\n'));
  assert.ok(warnings.includes('ignored a set_model without a model'), warnings.join('\n'));
});

test('shutdown cancels an open question with a deny', async () => {
  const { query, calls } = fakeQuery(async function* (options, c) {
    c.decisions.push(await options.canUseTool('AskUserQuestion', askInput, { signal: new AbortController().signal, toolUseID: 'toolu_q' }));
  });
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'question') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', text: 'go' });

  await runAgent({ query, commands, emit, graceMs: 100 });

  assert.equal(events.find((e) => e.type === 'user_message').id, 'u-1');
  assert.equal(calls.decisions[0].behavior, 'deny');
  assert.ok(!events.some((e) => e.type === 'question_answered'));
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
});

test('free-text response is passed to Claude and echoed', async () => {
  const { query, calls } = fakeQuery(async function* (options, c) {
    c.decisions.push(await options.canUseTool('AskUserQuestion', askInput, { signal: new AbortController().signal, toolUseID: 'toolu_r' }));
    yield { type: 'result', subtype: 'success', is_error: false, result: 'ok', total_cost_usd: 0, duration_ms: 1 };
  });
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'question') {
      commands.push({ type: 'answer', question_id: 'toolu_r', answers: { 'Which file name?': 'greeting.md' }, response: 'Use markdown' });
    }
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', text: 'go' });

  await runAgent({ query, commands, emit, graceMs: 100 });

  assert.deepEqual(calls.decisions[0].updatedInput.answers, { 'Which file name?': 'greeting.md' });
  assert.equal(calls.decisions[0].updatedInput.response, 'Use markdown');
  assert.equal(events.find((e) => e.type === 'question_answered').response, 'Use markdown');
});

test('a plain-text question ending a turn is re-asked as a choice card once', async () => {
  const { query, calls } = fakeQuery(async function* (options, c) {
    const result = c.prompts.length === 1 ? 'Which database should I use?' : 'Still unsure, which one?';
    yield { type: 'result', subtype: 'success', is_error: false, result, total_cost_usd: 0, duration_ms: 1 };
  });
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', text: 'go' });

  await runAgent({ query, commands, emit, graceMs: 100 });

  assert.equal(calls.prompts.length, 2);
  assert.deepEqual(calls.prompts[1], {
    type: 'user',
    message: { role: 'user', content: CHOICE_NUDGE },
    parent_tool_use_id: null,
    isSynthetic: true,
  });
  // The first turn_end is withheld (autopilot must not publish mid-question); only one nudge per message.
  const turnEnds = events.filter((e) => e.type === 'turn_end');
  assert.equal(turnEnds.length, 1);
  assert.equal(turnEnds[0].result, 'Still unsure, which one?');
  assert.ok(events.some((e) => e.type === 'log' && e.message.includes('choice card')));
  assert.ok(!events.some((e) => e.type === 'user_message' && e.text === CHOICE_NUDGE));
});

test('choice enforcement can be turned off; question detection', async () => {
  assert.ok(endsWithQuestion('Which one?'));
  assert.ok(endsWithQuestion('Should I proceed? **'));
  assert.ok(!endsWithQuestion('Done. Created hello.txt'));
  assert.ok(!endsWithQuestion('Is it fixed? Yes, it is.'));
  assert.ok(!endsWithQuestion(null));

  const { query, calls } = fakeQuery(async function* () {
    yield { type: 'result', subtype: 'success', is_error: false, result: 'Which one?', total_cost_usd: 0, duration_ms: 1 };
  });
  const events = [];
  const commands = new AsyncQueue();
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  commands.push({ type: 'user_message', text: 'go' });

  await runAgent({ query, commands, emit, graceMs: 100, enforceChoices: false });

  assert.equal(calls.prompts.length, 1);
  assert.equal(events.filter((e) => e.type === 'turn_end').length, 1);
});

test('options come from the environment', () => {
  const { options, warnings } = buildOptions({
    COLONIZER_MODEL: 'haiku',
    COLONIZER_EFFORT: 'extreme',
    CLAUDECODE: '1',
    CLAUDE_CODE_ENTRYPOINT: 'cli',
    CLAUDE_CODE_OAUTH_TOKEN: 'placeholder',
    PATH: '/usr/bin',
  });
  assert.equal(options.model, 'haiku');
  assert.equal(options.effort, undefined);
  assert.equal(warnings.length, 1);
  assert.equal(options.permissionMode, 'default');
  assert.equal(options.pathToClaudeCodeExecutable, '/opt/claude/bin/claude');
  assert.deepEqual(options.systemPrompt, { type: 'preset', preset: 'claude_code', append: SYSTEM_PROMPT_APPEND });
  assert.deepEqual(options.env, {
    COLONIZER_DELEGATE: 'off',
    COLONIZER_MODEL: 'haiku',
    COLONIZER_EFFORT: 'extreme',
    CLAUDE_CODE_OAUTH_TOKEN: 'placeholder',
    PATH: '/usr/bin',
  });
  assert.equal(buildOptions({ COLONIZER_EFFORT: 'high' }).options.effort, 'high');
  assert.deepEqual(childEnv({ CLAUDE_PID: '1', HOME: '/root' }), { HOME: '/root' });
});

test('disabled tools become the session disallow list, and stay absent by default', () => {
  // Harness-level switch (module.json `disabled_tools`): whatever endpoint serves the model, these
  // tools are gone — distinct from the per-connection strip the provider gateway does.
  assert.equal(buildOptions({}).options.disallowedTools, undefined);
  assert.equal(buildOptions({ COLONIZER_DISABLED_TOOLS: '' }).options.disallowedTools, undefined);
  assert.equal(buildOptions({ COLONIZER_DISABLED_TOOLS: '  ,  ' }).options.disallowedTools, undefined);
  const { options } = buildOptions({ COLONIZER_DISABLED_TOOLS: ' WebSearch , Write,,Bash ' });
  assert.deepEqual(options.disallowedTools, ['WebSearch', 'Write', 'Bash']);
});

test('subagent effort redefines the built-in agents the orchestrator delegates to', () => {
  // Unset: only repo-explorer is added; the two built-ins stay and inherit the orchestrator's effort.
  const unset = buildOptions({ COLONIZER_EFFORT: 'xhigh' }).options.agents;
  assert.deepEqual(Object.keys(unset), ['repo-explorer']);
  assert.equal(unset['repo-explorer'].effort, undefined);

  const { options, warnings } = buildOptions({
    COLONIZER_EFFORT: 'xhigh',
    COLONIZER_SUBAGENT_EFFORT: 'medium',
    COLONIZER_SUBAGENT_MODEL: 'claude-opus-5-5',
  });
  assert.deepEqual(warnings, []);
  assert.equal(options.effort, 'xhigh');
  assert.deepEqual(Object.keys(options.agents).sort(), ['Explore', 'general-purpose', 'repo-explorer']);
  for (const agent of Object.values(options.agents)) {
    assert.equal(agent.effort, 'medium');
    assert.ok(agent.description && agent.prompt);
    // No model on the definition, so the subagent model setting still decides it.
    assert.equal(agent.model, undefined);
  }
  assert.equal(options.env.CLAUDE_CODE_SUBAGENT_MODEL, 'claude-opus-5-5');
  // Explore stays read-only: the prompt says so and the deny list enforces it. The list must never be
  // narrower than the built-in's, extracted from the Claude Code build vendor/claude-code.lock pins.
  const builtIns = JSON.parse(readFileSync(new URL('../../../../vendor/claude-code-builtins.json', import.meta.url), 'utf8'));
  for (const tool of builtIns.Explore.disallowedTools) assert.ok(options.agents.Explore.disallowedTools.includes(tool), tool);
  assert.equal(options.agents['general-purpose'].disallowedTools, undefined);

  const bad = buildOptions({ COLONIZER_SUBAGENT_EFFORT: 'extreme' });
  // The bad effort is ignored, so only repo-explorer is added and the built-ins keep the session's.
  assert.deepEqual(Object.keys(bad.options.agents), ['repo-explorer']);
  assert.equal(bad.options.agents['repo-explorer'].effort, undefined);
  assert.equal(bad.warnings.length, 1);
  assert.match(bad.warnings[0], /COLONIZER_SUBAGENT_EFFORT=extreme/);
});

test('tool result text handles strings, parts and short output', () => {
  assert.equal(toolResultText('ok'), 'ok');
  assert.equal(toolResultText([{ type: 'text', text: 'a' }, { type: 'image' }]), 'a\n[image]');
  assert.equal(toolResultText(undefined), '');
});

test('plugin directories become local plugin entries, and are absent by default', () => {
  // Default: nothing mounted, nothing loaded. A colony that was never told to
  // load a plugin must not get one.
  assert.equal(buildOptions({}).options.plugins, undefined);
  assert.equal(buildOptions({ COLONIZER_PLUGIN_DIRS: '' }).options.plugins, undefined);
  assert.equal(buildOptions({ COLONIZER_PLUGIN_DIRS: '  ,  ' }).options.plugins, undefined);

  // The mothership has already rewritten these to in-VM paths.
  const { options } = buildOptions({
    COLONIZER_PLUGIN_DIRS: '/opt/colonizer/plugins/ecc, /opt/colonizer/plugins/house-style',
  });
  assert.deepEqual(options.plugins, [
    { type: 'local', path: '/opt/colonizer/plugins/ecc' },
    { type: 'local', path: '/opt/colonizer/plugins/house-style' },
  ]);

  // Loading a plugin must not quietly widen where settings come from: a
  // project-scope install would land in the pull request.
  assert.deepEqual(options.settingSources, ['project']);
});

test('a loaded superpowers plugin puts its bootstrap in the system prompt, since its hook never runs', () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-plugins-'));
  try {
    const superpowers = join(root, 'superpowers');
    const ecc = join(root, 'ecc');
    mkdirSync(dirname(join(superpowers, SUPERPOWERS_SKILL)), { recursive: true });
    writeFileSync(join(superpowers, SUPERPOWERS_SKILL), '---\nname: using-superpowers\n---\n\nCheck for a skill before any response.\n\n');
    mkdirSync(join(ecc, 'skills'), { recursive: true });

    // A plugin without the skill (ECC) changes nothing about the prompt.
    assert.equal(buildOptions({ COLONIZER_PLUGIN_DIRS: ecc }).options.systemPrompt.append, SYSTEM_PROMPT_APPEND);

    const { options } = buildOptions({ COLONIZER_PLUGIN_DIRS: `${ecc},${superpowers}` });
    const append = options.systemPrompt.append;
    assert.ok(append.startsWith(SYSTEM_PROMPT_APPEND), "Colonizer's own instructions come first");
    assert.ok(append.includes('<EXTREMELY_IMPORTANT>\nYou have superpowers.\n'), 'the hook\'s own wrapper');
    assert.ok(append.includes('Check for a skill before any response.\n</EXTREMELY_IMPORTANT>'), 'the skill text, trailing blank lines trimmed');
    assert.ok(append.endsWith(SUPERPOWERS_COLONIZER_NOTE), 'the note about the two skills that are not staged');
    assert.equal(append.split('<EXTREMELY_IMPORTANT>').length, 2, 'once, however many plugins load');
    // Loading it is still just a plugin entry; nothing registers a hook for it. (Only the
    // denial layer's PostToolUseFailure hook is always present.)
    assert.deepEqual(options.plugins, [
      { type: 'local', path: ecc },
      { type: 'local', path: superpowers },
    ]);
    assert.equal(options.hooks.PreToolUse, undefined);

    assert.equal(superpowersBootstrap('x').split('\n')[0], '<EXTREMELY_IMPORTANT>');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('a migrated skill pack (root plugin.json beside the legacy manifest) loads and bootstraps', () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-pack-'));
  try {
    const pack = join(root, 'superpowers');
    mkdirSync(join(pack, 'skills/using-superpowers'), { recursive: true });
    mkdirSync(join(pack, '.claude-plugin'), { recursive: true });
    writeFileSync(join(pack, '.claude-plugin/plugin.json'), JSON.stringify({ name: 'superpowers', version: '6.4.1' }));
    writeFileSync(join(pack, 'plugin.json'), JSON.stringify({
      name: 'superpowers',
      version: '6.4.1',
      description: 'Core skills library',
      skills: ['using-superpowers'],
    }));
    writeFileSync(join(pack, SUPERPOWERS_SKILL), '---\nname: using-superpowers\n---\n\nCheck for a skill before any response.\n\n');

    // The migrated layout boots exactly like the legacy one: one local plugin
    // entry, and the bootstrap text in the system prompt.
    const { options } = buildOptions({ COLONIZER_PLUGIN_DIRS: pack });
    assert.deepEqual(options.plugins, [{ type: 'local', path: pack }]);
    assert.ok(options.systemPrompt.append.includes('Check for a skill before any response.'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('caveman puts its ruleset in the system prompt at the chosen level, with what stays plain', () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-caveman-'));
  try {
    const skill = join(root, 'SKILL.md');
    writeFileSync(skill, '---\nname: caveman\ndescription: >\n  Ultra-compressed.\n---\n\nRespond terse like smart caveman.\n\nDefault: **full**. Switch: `/caveman lite|full|ultra`.\n');

    assert.equal(buildOptions({}).options.systemPrompt.append, SYSTEM_PROMPT_APPEND, 'off by default');
    assert.equal(buildOptions({ COLONIZER_CAVEMAN: 'false', COLONIZER_CAVEMAN_SKILL: skill }).options.systemPrompt.append, SYSTEM_PROMPT_APPEND);

    const { options, warnings } = buildOptions({ COLONIZER_CAVEMAN: 'true', COLONIZER_CAVEMAN_SKILL: skill, COLONIZER_CAVEMAN_LEVEL: 'ultra' });
    const append = options.systemPrompt.append;
    assert.deepEqual(warnings, []);
    assert.ok(append.startsWith(SYSTEM_PROMPT_APPEND));
    assert.ok(append.includes('at the ultra level'));
    assert.ok(append.includes('Respond terse like smart caveman.'));
    assert.ok(!append.includes('name: caveman'), 'the frontmatter is not part of the rules');
    assert.ok(append.includes('pull request description, AskUserQuestion questions and options'), 'what a colony writes for other people stays plain');

    for (const level of ['', 'wenyan-ultra', 'LOUD']) {
      assert.ok(buildOptions({ COLONIZER_CAVEMAN: 'true', COLONIZER_CAVEMAN_SKILL: skill, COLONIZER_CAVEMAN_LEVEL: level }).options.systemPrompt.append.includes('at the full level'), `${level || '(unset)'} falls back to full`);
    }
    assert.deepEqual([...CAVEMAN_LEVELS], ['lite', 'full', 'ultra']);

    const missing = buildOptions({ COLONIZER_CAVEMAN: 'true', COLONIZER_CAVEMAN_SKILL: join(root, 'nope.md') });
    assert.equal(missing.options.systemPrompt.append, SYSTEM_PROMPT_APPEND);
    assert.equal(missing.warnings.length, 1, 'a missing ruleset is reported, not fatal');
    assert.equal(cavemanPrompt('no frontmatter', 'lite').split('\n')[2], 'no frontmatter');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('rtk rewrites Bash commands through `rtk rewrite` and leaves every other outcome alone', async () => {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-rtk-'));
  try {
    // rtk's exit codes: 0 rewrite, 3 rewrite that its ask rules flag, 1 no equivalent, 2 deny rule.
    const rtk = join(root, 'rtk');
    writeFileSync(
      rtk,
      [
        '#!/bin/sh',
        '[ "$1" = rewrite ] || exit 9',
        'case "$2" in',
        '  "git status") echo "rtk git status"; exit 3 ;;',
        '  "cargo test") echo "rtk cargo test"; exit 0 ;;',
        '  "rm -rf /") echo "rtk rm -rf /"; exit 2 ;;',
        '  "same") echo "same"; exit 0 ;;',
        '  "slow") sleep 5; echo "rtk slow"; exit 0 ;;',
        '  *) exit 1 ;;',
        'esac',
        '',
      ].join('\n'),
    );
    chmodSync(rtk, 0o755);

    assert.equal(await rtkRewrite('git status', rtk), 'rtk git status', 'exit 3 still rewrites');
    assert.equal(await rtkRewrite('cargo test', rtk), 'rtk cargo test');
    assert.equal(await rtkRewrite('echo hi', rtk), null, 'no equivalent');
    assert.equal(await rtkRewrite('rm -rf /', rtk), null, 'a deny rule leaves the command to the colony');
    assert.equal(await rtkRewrite('same', rtk), null, 'an identical rewrite is no rewrite');
    assert.equal(await rtkRewrite('git status', join(root, 'missing-rtk')), null, 'rtk not installed');
    assert.equal(await rtkRewrite('slow', rtk), null, 'a slow rtk never holds a command back');

    assert.equal(buildOptions({}).options.hooks.PreToolUse, undefined, 'off by default');
    const { options } = buildOptions({ COLONIZER_RTK: 'true', COLONIZER_RTK_BIN: rtk, PATH: '/usr/bin' });
    assert.equal(options.env.PATH, `${root}:/usr/bin`, 'rewritten commands call rtk, so it is on the PATH');
    const [entry] = options.hooks.PreToolUse;
    assert.equal(entry.matcher, 'Bash');
    const hook = entry.hooks[0];
    const rewritten = await hook({ tool_name: 'Bash', tool_input: { command: 'cargo test', description: 'Run tests' } });
    assert.deepEqual(rewritten.hookSpecificOutput, {
      hookEventName: 'PreToolUse',
      updatedInput: { command: 'rtk cargo test', description: 'Run tests' },
    });
    assert.equal(rewritten.hookSpecificOutput.permissionDecision, undefined, 'never a permission decision');
    assert.deepEqual(await hook({ tool_name: 'Bash', tool_input: { command: 'echo hi' } }), { continue: true });

    // Alongside the delegation gate, both hooks are registered and the gate keeps its deny.
    const both = buildOptions({ COLONIZER_RTK: 'true', COLONIZER_RTK_BIN: rtk, COLONIZER_DELEGATE: 'enforce' }).options;
    assert.equal(both.hooks.PreToolUse.length, 2);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
