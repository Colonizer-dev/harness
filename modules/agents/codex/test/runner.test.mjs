import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import { AsyncQueue, buildThreadOptions, codexModel, extractQuestion, formatAnswer, modelError, normalizeQuestions, preflight, runAgent, SYSTEM_PROMPT_APPEND, toolResultText } from '../runner.mjs';

const BIN = { COLONIZER_CODEX_BIN: process.execPath };
const KEY = { OPENAI_API_KEY: 'sk-test' };
const ENV = { ...KEY, ...BIN };

// The schema's event vocabulary: every $def whose type is a const.
const schema = JSON.parse(readFileSync(new URL('../../../../docs/agent-events.schema.json', import.meta.url), 'utf8'));
const PROTOCOL_TYPES = new Set(Object.values(schema.$defs).map((def) => def?.properties?.type?.const).filter(Boolean));

/** A fake Codex SDK: script(input, turnOptions, calls) yields the turn's ThreadEvents. */
function fakeCodex(script) {
  const calls = { inputs: [], resumed: [], started: 0, options: null, signals: [] };
  let n = 0;
  const makeThread = (id, options) => ({
    id,
    options,
    runStreamed: async function (input, turnOptions) {
      this.id ??= `thread-${++n}`; // the real id populates once the first turn starts
      calls.inputs.push(input);
      calls.signals.push(turnOptions?.signal);
      return { events: script(input, turnOptions ?? {}, calls) };
    },
  });
  return {
    calls,
    createCodex: () => ({
      startThread: (options) => {
        calls.started += 1;
        calls.options = options;
        return makeThread(null, options);
      },
      resumeThread: (id, options) => {
        calls.resumed.push({ id, options });
        return makeThread(id, options);
      },
    }),
  };
}

const completed = (item) => ({ type: 'item.completed', item });
const done = (usage) => ({ type: 'turn.completed', usage });
const DEFAULT_USAGE = { input_tokens: 10, cached_input_tokens: 2, cache_write_input_tokens: 1, output_tokens: 5, reasoning_output_tokens: 0 };
async function* textTurn(text, usage) {
  yield completed({ id: 'a1', type: 'agent_message', text });
  yield done(usage === undefined ? DEFAULT_USAGE : usage); // null usage means the turn reports none
}

/** Drive runAgent until shutdown; react to events on the way. */
async function drive({ createCodex }, env, react, seed = [{ type: 'user_message', id: 'u-1', text: 'go' }]) {
  const events = [];
  const commands = new AsyncQueue();
  for (const command of seed) commands.push(command);
  const ok = await runAgent({ createCodex, commands, emit: (event) => (events.push(event), react?.(event, commands)), env, graceMs: 100 });
  return { events, ok };
}
const ofType = (events, type) => events.filter((e) => e.type === type);
const shutdownOnTurnEnd = (event, commands) => {
  if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
};

test('preflight refuses a missing key, a missing binary, and a non-openai provider', async () => {
  for (const [env, match] of [[{ ...BIN }, /API key/], [{ ...KEY, COLONIZER_CODEX_BIN: '/nonexistent/codex' }, /not executable/], [{ ...ENV, COLONIZER_MODEL: 'anthropic/claude-opus-5' }, /openai/]]) {
    assert.equal(preflight(env).ok, false);
    assert.match(preflight(env).error, match);
    const commands = new AsyncQueue();
    commands.close();
    const events = [];
    assert.equal(await runAgent({ createCodex: () => ({}), commands, emit: (e) => events.push(e), env }), false);
    assert.deepEqual(events, [{ type: 'status', state: 'error', detail: preflight(env).error }]);
  }
  assert.equal(preflight(ENV).ok, true);
  assert.equal(modelError('openai/gpt-5'), null);
  assert.equal(modelError('gpt-5'), null);
  assert.equal(codexModel('openai/gpt-5'), 'gpt-5');
});

test('starts idle, announces the model, and reports cumulative usage', async () => {
  const fake = fakeCodex(async function* (input) {
    yield* textTurn(input === 'again' ? 'Second' : 'Done', { input_tokens: 10, cached_input_tokens: 0, cache_write_input_tokens: 0, output_tokens: 5, reasoning_output_tokens: 0 });
  });
  let turns = 0;
  const { events, ok } = await drive(fake, { ...ENV, COLONIZER_MODEL: 'openai/gpt-5-mini' }, (event, commands) => {
    if (event.type === 'turn_end') commands.push(turns++ ? { type: 'shutdown' } : { type: 'user_message', id: 'u-2', text: 'again' });
  });
  assert.ok(ok);
  assert.deepEqual(events[0], { type: 'status', state: 'idle' });
  assert.deepEqual(ofType(events, 'model_changed'), [{ type: 'model_changed', model: 'gpt-5-mini', previous: null }]);
  assert.equal(fake.calls.options.model, 'gpt-5-mini');
  assert.equal(fake.calls.options.sandboxMode, 'danger-full-access');
  assert.equal(fake.calls.options.approvalPolicy, 'never');
  assert.equal(fake.calls.options.skipGitRepoCheck, true);
  assert.ok(fake.calls.inputs[0].startsWith(SYSTEM_PROMPT_APPEND), 'first turn carries the instructions');
  assert.equal(fake.calls.inputs[1], 'again');
  const ends = ofType(events, 'turn_end');
  assert.equal(ends.length, 2);
  assert.equal(ends[0].cost_usd, null);
  assert.deepEqual(ends[0].model_usage, { 'gpt-5-mini': { input_tokens: 10, output_tokens: 5, cache_read_tokens: 0, cache_write_tokens: 0 } });
  assert.equal(ends[1].model_usage['gpt-5-mini'].input_tokens, 20, 'usage is cumulative across turns');
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
});

test('maps every item type, pairs each tool_call with a tool_result, and truncates output', async () => {
  const fake = fakeCodex(async function* () {
    yield { type: 'item.started', item: { id: 'c1', type: 'command_execution', command: 'vitest run', aggregated_output: '', status: 'in_progress' } };
    yield completed({ id: 'c1', type: 'command_execution', command: 'vitest run', aggregated_output: `x${'y'.repeat(25_000)}`, exit_code: 1, status: 'failed' });
    yield completed({ id: 'f1', type: 'file_change', changes: [{ path: 'a.ts', kind: 'update' }], status: 'completed' });
    yield completed({ id: 'm1', type: 'mcp_tool_call', server: 'hub', tool: 'get', arguments: { k: 1 }, result: { content: [{ type: 'text', text: 'v' }] }, status: 'completed' });
    yield completed({ id: 'w1', type: 'web_search', query: 'codex sdk' });
    yield completed({ id: 't1', type: 'todo_list', items: [{ text: 'a', completed: true }, { text: 'b', completed: false }] });
    yield completed({ id: 'e1', type: 'error', message: 'minor hiccup' });
    yield completed({ id: 'r1', type: 'reasoning', text: 'thinking out loud' });
    yield* textTurn('All mapped', null);
  });
  const { events } = await drive(fake, ENV, shutdownOnTurnEnd);
  assert.deepEqual(ofType(events, 'assistant_text'), [{ type: 'assistant_text', message_id: 'a1', block_index: 0, text: 'All mapped' }]);
  assert.deepEqual(ofType(events, 'thinking'), [{ type: 'thinking', message_id: 'r1', block_index: 0, text: 'thinking out loud' }]);
  assert.deepEqual(ofType(events, 'tool_call').map((e) => [e.tool_call_id, e.name]), [['c1', 'Bash'], ['f1', 'Edit'], ['m1', 'mcp__hub__get'], ['w1', 'WebSearch']]);
  const results = new Map(ofType(events, 'tool_result').map((e) => [e.tool_call_id, e]));
  assert.equal(results.size, 4, 'every tool_call gets exactly one tool_result');
  assert.equal(results.get('c1').is_error, true);
  assert.ok(results.get('c1').output.length <= 20_000);
  assert.match(results.get('c1').output, /truncated \d+ characters\]$/);
  assert.deepEqual(ofType(events, 'tool_call').find((e) => e.tool_call_id === 'c1').input, { command: 'vitest run' });
  assert.equal(results.get('f1').is_error, false);
  assert.match(results.get('f1').output, /update a\.ts/);
  assert.equal(results.get('m1').output, 'v');
  assert.ok(ofType(events, 'log').some((e) => e.level === 'info' && e.message.includes('1/2 steps done')));
  assert.ok(ofType(events, 'log').some((e) => e.level === 'warn' && e.message === 'minor hiccup'));
  assert.equal(ofType(events, 'turn_end').length, 1);
  assert.ok(!('model_usage' in ofType(events, 'turn_end')[0]), 'no usage event means no model_usage field');
});

test('a failed turn ends as an error and the runner stays idle', async () => {
  const fake = fakeCodex(async function* () {
    yield { type: 'error', message: 'boom upstream' };
    yield { type: 'turn.failed', error: { message: 'auth failed: 401' } };
  });
  const { events } = await drive(fake, ENV, shutdownOnTurnEnd);
  assert.ok(events.some((e) => e.type === 'log' && e.level === 'error' && e.message === 'auth failed: 401'));
  const [end] = ofType(events, 'turn_end');
  assert.deepEqual([end.is_error, end.result, end.cost_usd], [true, 'auth failed: 401', null]);
  assert.deepEqual(ofType(events, 'status').map((e) => e.state), ['idle', 'working', 'idle', 'exited']);
});

test('a question block asks before turn_end, and the answer starts a follow-up turn', async () => {
  const block = 'Pick one.\n```colonizer-question\n{"questions":[{"question":"Which file?","header":"File","multi_select":false,"options":[{"label":"a.txt","description":"First"},{"label":"b.txt","description":"Second"}]}]}\n```';
  const fake = fakeCodex(async function* (input) {
    yield* textTurn(input.includes('The user answered') ? 'Applied your choice' : block, { input_tokens: 1, cached_input_tokens: 0, cache_write_input_tokens: 0, output_tokens: 1, reasoning_output_tokens: 0 });
  });
  let turns = 0;
  const { events } = await drive(fake, ENV, (event, commands) => {
    if (event.type === 'question') {
      const [q] = event.questions;
      commands.push({ type: 'answer', question_id: event.question_id, answers: { [q.question]: q.options[1].label } });
      commands.push({ type: 'answer', question_id: 'unknown', answers: {} });
    }
    if (event.type === 'turn_end' && turns++ === 1) commands.push({ type: 'shutdown' });
  });
  const [question] = ofType(events, 'question');
  assert.equal(question.message_id, 'a1');
  assert.deepEqual(question.questions, [{ question: 'Which file?', header: 'File', multi_select: false, options: [{ label: 'a.txt', description: 'First', preview: null }, { label: 'b.txt', description: 'Second', preview: null }] }]);
  const end1 = events.findIndex((e) => e.type === 'turn_end');
  assert.ok(events.indexOf(question) < end1, 'the question opens before the turn ends, so autopilot cannot publish');
  assert.deepEqual(events[end1 + 1], { type: 'status', state: 'waiting_for_answer' });
  assert.deepEqual(ofType(events, 'question_answered'), [{ type: 'question_answered', question_id: question.question_id, answers: { 'Which file?': 'b.txt' }, response: null }]);
  assert.ok(ofType(events, 'log').some((e) => e.level === 'warn' && e.message.includes('unknown')));
  for (const text of ofType(events, 'assistant_text')) assert.ok(!text.text.includes('colonizer-question'), 'the fence is stripped from chat text');
  assert.ok(fake.calls.inputs[1].includes('b.txt'), 'the follow-up turn renders the answer as text');
  assert.equal(ofType(events, 'turn_end')[1].result, 'Applied your choice');
});

test('interrupt aborts the running turn but keeps the thread for later turns', async () => {
  const fake = fakeCodex(async function* (input, turn) {
    if (input.includes('slow')) {
      if (!turn.signal.aborted) await new Promise((resolve) => turn.signal.addEventListener('abort', resolve, { once: true }));
      return; // like the real SDK, an aborted turn ends without producing events
    }
    yield* textTurn('Recovered', null);
  });
  let interrupted = false;
  const { events } = await drive(fake, ENV, (event, commands) => {
    if (event.type === 'user_message' && !interrupted) {
      interrupted = true;
      commands.push({ type: 'interrupt' });
    }
    if (event.type === 'turn_end') commands.push(event.result === 'Recovered' ? { type: 'shutdown' } : { type: 'user_message', id: 'u-2', text: 'fast' });
  }, [{ type: 'user_message', id: 'u-1', text: 'slow' }]);
  assert.ok(fake.calls.signals[0].aborted, 'the abort signal reached the turn');
  assert.equal(fake.calls.started, 1, 'the thread survives the interrupt');
  assert.equal(ofType(events, 'turn_end').find((e) => e.is_error).result, 'interrupted');
  assert.ok(events.some((e) => e.type === 'assistant_text' && e.text === 'Recovered'));
});

test('set_model resumes the thread on the new model and refuses foreign providers', async () => {
  const fake = fakeCodex(async function* () {
    yield* textTurn('ok', null);
  });
  let turns = 0;
  const { events } = await drive(fake, { ...ENV, COLONIZER_MODEL: 'gpt-5' }, (event, commands) => {
    if (event.type === 'turn_end' && turns++ === 0) {
      commands.push({ type: 'set_model', model: ' openai/gpt-5-mini ' });
      commands.push({ type: 'set_model', model: 'anthropic/x' });
      commands.push({ type: 'set_model', model: '  ' });
    }
    if (event.type === 'model_changed' && event.previous) commands.push({ type: 'shutdown' });
  });
  assert.deepEqual(fake.calls.resumed.map((r) => [r.id, r.options.model]), [['thread-1', 'gpt-5-mini']]);
  assert.deepEqual(ofType(events, 'model_changed'), [{ type: 'model_changed', model: 'gpt-5', previous: null }, { type: 'model_changed', model: 'gpt-5-mini', previous: 'gpt-5' }]);
  const warnings = ofType(events, 'log').filter((e) => e.level === 'warn').map((e) => e.message);
  assert.ok(warnings.some((m) => m.includes('anthropic')), warnings.join('\n'));
  assert.ok(warnings.includes('ignored a set_model without a model'), warnings.join('\n'));
});

test('shutdown and EOF exit; unknown commands are ignored; every event type is in the schema', async () => {
  const fake = fakeCodex(async function* () {
    yield* textTurn('ok', null);
  });
  const events = [];
  const commands = new AsyncQueue();
  commands.push({ type: 'user_message', id: 'u-1', text: 'go' });
  commands.push({ type: 'something_new' });
  commands.push({ type: 'answer', question_id: 'nope', answers: {} });
  const emit = (event) => {
    events.push(event);
    if (event.type === 'turn_end') commands.push({ type: 'shutdown' });
  };
  assert.equal(await runAgent({ createCodex: fake.createCodex, commands, emit, env: ENV, graceMs: 100 }), true);
  assert.ok(events.some((e) => e.type === 'log' && e.message.includes('nope')));
  assert.deepEqual(events.at(-1), { type: 'status', state: 'exited' });
  const eof = []; // stdin EOF without a shutdown exits the same way
  const closing = new AsyncQueue();
  closing.push({ type: 'user_message', text: 'go' });
  closing.close();
  await runAgent({ createCodex: fake.createCodex, commands: closing, emit: (e) => eof.push(e), env: ENV, graceMs: 100 });
  assert.deepEqual(eof.at(-1), { type: 'status', state: 'exited' });
  for (const event of [...events, ...eof]) assert.ok(PROTOCOL_TYPES.has(event.type), `event type ${event.type} is outside the schema`);
});

test('question helpers handle fences, models, options and truncation', () => {
  assert.deepEqual(extractQuestion('plain').questions, null);
  assert.deepEqual(extractQuestion('```colonizer-question\nnot json\n```').questions, null);
  const { clean, questions } = extractQuestion('Lead.\n```colonizer-question\n{"questions":[{"question":"Q?","header":"H","multi_select":true,"options":[{"label":"L"}]}]}\n```');
  assert.equal(clean, 'Lead.');
  assert.equal(questions.length, 1);
  assert.deepEqual(normalizeQuestions(questions), [{ question: 'Q?', header: 'H', multi_select: true, options: [{ label: 'L', description: '', preview: null }] }]);
  assert.deepEqual(normalizeQuestions(null), []);
  assert.equal(formatAnswer({ 'Q?': ['a', 'b'] }, 'note'), 'The user answered your question:\n- Q?: a, b\nFree-text reply: note');
  assert.equal(buildThreadOptions({ COLONIZER_EFFORT: 'bogus' }).modelReasoningEffort, undefined);
  assert.equal(buildThreadOptions({ COLONIZER_EFFORT: 'high' }).modelReasoningEffort, 'high');
  assert.match(buildThreadOptions({ COLONIZER_EFFORT: 'bogus' }).warning, /COLONIZER_EFFORT/);
  assert.ok(toolResultText('x'.repeat(25_000)).length <= 20_000);
});
