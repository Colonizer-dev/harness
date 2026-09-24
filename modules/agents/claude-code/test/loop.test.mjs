import assert from 'node:assert/strict';
import { test } from 'node:test';

import { createLoopServer, LOOP_NEXT_TOOL, LOOP_SERVER, LOOP_STOP_TOOL, loopDecision, loopPromptAppend } from '../loop.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };

function fakeSdk() {
  const chain = new Proxy(() => chain, { get: () => () => chain });
  const z = { string: () => chain, number: () => chain };
  const tool = (name, description, schema, handler) => ({ name, description, schema, handler });
  const createSdkMcpServer = (server) => server;
  return { z, tool, createSdkMcpServer };
}

test('a self-paced loop colony gets loop_next and loop_stop; a fixed one only loop_stop', async () => {
  const events = [];
  const paced = createLoopServer({ emit: (e) => events.push(e), ...fakeSdk(), selfPaced: true });
  assert.equal(paced.name, LOOP_SERVER);
  assert.deepEqual(paced.tools.map((t) => t.name), ['loop_next', 'loop_stop']);
  const fixed = createLoopServer({ emit: () => {}, ...fakeSdk(), selfPaced: false });
  assert.deepEqual(fixed.tools.map((t) => t.name), ['loop_stop']);

  await paced.tools[0].handler({ delay_minutes: 5, reason: 'CI reruns soon' });
  await paced.tools[0].handler({ delay_minutes: 99999, reason: 'quiet' });
  await paced.tools[1].handler({ reason: 'all flakes fixed' });
  assert.deepEqual(events, [
    { type: 'loop_next', delay_minutes: 15, reason: 'CI reruns soon' },
    { type: 'loop_next', delay_minutes: 1440, reason: 'quiet' },
    { type: 'loop_stop', reason: 'all flakes fixed' },
  ]);
});

test('only the orchestrator paces or stops the loop', () => {
  assert.equal(loopDecision(LOOP_NEXT_TOOL, {}), null);
  assert.match(loopDecision(LOOP_STOP_TOOL, { agent_id: 'agent_01' }), /Only the orchestrator/);
  assert.equal(loopDecision('Bash', { agent_id: 'agent_01' }), null);
});

test('the loop tools and prompt are wired in only for a loop colony', () => {
  const server = { name: LOOP_SERVER };
  const off = buildOptions({ ...base }, { loopServer: server }).options;
  assert.equal(off.mcpServers, undefined);
  assert.doesNotMatch(off.systemPrompt.append, /one run of a loop/);
  const on = buildOptions({ ...base, COLONIZER_LOOP: 'true', COLONIZER_LOOP_SELF_PACED: 'true' }, { loopServer: server }).options;
  assert.equal(on.mcpServers[LOOP_SERVER], server);
  assert.match(on.systemPrompt.append, /call loop_next/);
  assert.match(loopPromptAppend(false), /fixed schedule/);
});
