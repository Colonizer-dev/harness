// The stub model's reply policy is what makes the end-to-end run come back `no_changes` (issue
// #368): exactly one Write of /harness/out/pr.md on the first orchestrator turn, plain text after
// that, never a question. These tests pin it without booting anything.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { PR_PATH, PR_TITLE, replyFrames, stubReply } from '../colony-e2e.mjs';

const firstTurn = { model: 'colony-e2e', tools: [{ name: 'Read' }, { name: 'Write' }, { name: 'Task' }], messages: [{ role: 'user', content: 'begin' }] };
const afterWrite = {
  ...firstTurn,
  messages: [
    ...firstTurn.messages,
    { role: 'assistant', content: [{ type: 'tool_use', id: 'toolu_e2e_pr', name: 'Write' }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'toolu_e2e_pr', content: 'wrote' }] },
  ],
};

test('the first turn with Write offered writes the PR description', () => {
  const reply = stubReply(firstTurn);
  assert.equal(reply.stop_reason, 'tool_use');
  assert.deepEqual(
    reply.content.map((b) => [b.type, b.name, b.input?.file_path]),
    [['tool_use', 'Write', PR_PATH]],
  );
  // The harness reads pr.md as a title line plus a body, and only a non-empty file counts as written.
  const content = reply.content[0].input.content;
  assert.ok(content.startsWith(`${PR_TITLE}\n\n`), 'pr.md starts with a title line and a blank line');
  assert.match(content, /\n\n.+/, 'pr.md has a body');
});

test('a conversation with a tool result back ends the turn with text', () => {
  const reply = stubReply(afterWrite);
  assert.equal(reply.stop_reason, 'end_turn');
  assert.deepEqual(reply.content.map((b) => b.type), ['text']);
  // autopilot publishes on turn_end only without an open question, and the runner re-prompts a turn
  // whose result ends asking something, so the text must not end with a question mark.
  assert.ok(!/[?]\s*$/.test(reply.content[0].text), 'the closing text does not end in a question');
  assert.ok(!reply.content.some((b) => b.type === 'tool_use' && b.name === 'AskUserQuestion'), 'never asks the user');
});

test('a request without the Write tool (background calls) is answered with text', () => {
  const reply = stubReply({ ...firstTurn, tools: [{ name: 'Read' }] });
  assert.equal(reply.stop_reason, 'end_turn');
  assert.deepEqual(reply.content.map((b) => b.type), ['text']);
});

test('the streaming frames carry the tool call through the SSE shapes the CLI parses', () => {
  const frames = replyFrames('colony-e2e', stubReply(firstTurn));
  assert.equal(frames[0][0], 'message_start');
  const kinds = frames.map(([event]) => event);
  assert.deepEqual(kinds.filter((k, i) => kinds.indexOf(k) === i), [
    'message_start',
    'content_block_start',
    'content_block_delta',
    'content_block_stop',
    'message_delta',
    'message_stop',
  ]);
  const start = frames.find(([, data]) => data.type === 'content_block_start');
  assert.equal(start[1].content_block.name, 'Write');
  const delta = frames.find(([, data]) => data.type === 'content_block_delta');
  assert.equal(delta[1].delta.type, 'input_json_delta');
  assert.equal(JSON.parse(delta[1].delta.partial_json).file_path, PR_PATH);
  const end = frames.find(([, data]) => data.type === 'message_delta');
  assert.equal(end[1].delta.stop_reason, 'tool_use');
});
