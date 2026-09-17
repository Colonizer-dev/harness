import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';

import {
  AsyncQueue,
  buildOptions,
  childEnv,
  CHOICE_NUDGE,
  endsWithQuestion,
  MAX_TOOL_OUTPUT,
  runAgent,
  SUPERPOWERS_COLONIZER_NOTE,
  SUPERPOWERS_SKILL,
  superpowersBootstrap,
  SYSTEM_PROMPT_APPEND,
  toolResultText,
} from '../runner.mjs';

const stream = (event) => ({ type: 'stream_event', event, parent_tool_use_id: null });
const assistant = (id, content) => ({ type: 'assistant', message: { id, role: 'assistant', content }, parent_tool_use_id: null });
const user = (content) => ({ type: 'user', message: { role: 'user', content }, parent_tool_use_id: null });

/** A fake Agent SDK `query`: runs `turn` once per prompt message. */
function fakeQuery(turn) {
  const calls = { prompts: [], decisions: [], interrupts: 0, closed: false, options: null };
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
  yield assistant('msg_2', [{ type: 'tool_use', id: 'toolu_bash', name: 'Bash', input: bashInput }]);
  calls.decisions.push(
    await options.canUseTool('Bash', bashInput, { signal: new AbortController().signal, toolUseID: 'toolu_bash' }),
  );
  yield user([{ type: 'tool_result', tool_use_id: 'toolu_bash', content: [{ type: 'text', text: 'x'.repeat(25_000) }] }]);
  yield { type: 'result', subtype: 'success', is_error: false, result: 'Created hello.txt', total_cost_usd: 0.01, duration_ms: 1234 };
}

test('maps a turn with a question to protocol events', async () => {
  const { query, calls } = fakeQuery(issueTurn);
  const events = [];
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
  ]);
  const results = ofType('tool_result');
  assert.equal(results.length, 1);
  assert.equal(results[0].tool_call_id, 'toolu_bash');
  assert.equal(results[0].is_error, false);
  assert.ok(results[0].output.length <= MAX_TOOL_OUTPUT);
  assert.match(results[0].output, /truncated 5000 characters\]$/);

  assert.deepEqual(ofType('turn_end'), [
    { type: 'turn_end', is_error: false, result: 'Created hello.txt', cost_usd: 0.01, duration_ms: 1234 },
  ]);

  const states = ofType('status').map((e) => e.state);
  assert.deepEqual(states, ['idle', 'working', 'waiting_for_answer', 'working', 'idle', 'exited']);
  const turnEnd = events.findIndex((e) => e.type === 'turn_end');
  assert.deepEqual(events[turnEnd + 1], { type: 'status', state: 'idle' });
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
    COLONIZER_MODEL: 'haiku',
    COLONIZER_EFFORT: 'extreme',
    CLAUDE_CODE_OAUTH_TOKEN: 'placeholder',
    PATH: '/usr/bin',
  });
  assert.equal(buildOptions({ COLONIZER_EFFORT: 'high' }).options.effort, 'high');
  assert.deepEqual(childEnv({ CLAUDE_PID: '1', HOME: '/root' }), { HOME: '/root' });
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
    // Loading it is still just a plugin entry; nothing registers a hook for it.
    assert.deepEqual(options.plugins, [
      { type: 'local', path: ecc },
      { type: 'local', path: superpowers },
    ]);
    assert.equal(options.hooks, undefined);

    assert.equal(superpowersBootstrap('x').split('\n')[0], '<EXTREMELY_IMPORTANT>');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
