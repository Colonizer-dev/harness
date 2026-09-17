import assert from 'node:assert/strict';
import { test } from 'node:test';

import { createFindingsServer, FILED_REPLY, FINDING_TOOL, FINDINGS_PROMPT_APPEND, FINDINGS_SERVER, findingDecision } from '../findings.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const fakeServer = { name: FINDINGS_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(), and a zod that accepts anything. */
function fakeSdk() {
  const chain = new Proxy(() => chain, { get: () => () => chain });
  const z = { string: () => chain };
  const tool = (name, description, schema, handler) => ({ name, description, schema, handler });
  const createSdkMcpServer = (server) => server;
  return { z, tool, createSdkMcpServer };
}

test('finding_file emits the finding and tells the agent where the outcome goes', async () => {
  const events = [];
  const server = createFindingsServer({ emit: (e) => events.push(e), ...fakeSdk() });
  assert.equal(server.name, FINDINGS_SERVER);
  const [file] = server.tools;
  assert.equal(file.name, 'finding_file');
  assert.deepEqual(Object.keys(file.schema), ['title', 'body', 'evidence'], 'evidence is part of the call, not optional');

  const reply = await file.handler({ title: 'Career pages promised', body: 'llms.txt says…', evidence: 'A subagent read model.rs' });
  assert.deepEqual(events, [{ type: 'finding', title: 'Career pages promised', body: 'llms.txt says…', evidence: 'A subagent read model.rs' }]);
  assert.equal(reply.content[0].text, FILED_REPLY);
});

test('only the orchestrator files: a subagent is told to report instead', () => {
  assert.equal(findingDecision(FINDING_TOOL, {}), null, 'the orchestrator may file');
  assert.match(findingDecision(FINDING_TOOL, { agent_id: 'agent_01' }), /Only the orchestrator files findings/);
  assert.equal(findingDecision('Bash', { agent_id: 'agent_01' }), null, 'other tools are not this gate’s business');
});

test('findings are wired in only when the mothership switched them on and the server exists', () => {
  const off = buildOptions({ ...base }, { findingsServer: fakeServer }).options;
  assert.equal(off.mcpServers, undefined);
  assert.ok(!off.systemPrompt.append.includes(FINDINGS_PROMPT_APPEND));

  const noServer = buildOptions({ ...base, COLONIZER_FINDINGS: 'true' }).options;
  assert.equal(noServer.mcpServers, undefined);

  const on = buildOptions({ ...base, COLONIZER_FINDINGS: 'true' }, { findingsServer: fakeServer }).options;
  assert.deepEqual(on.mcpServers, { [FINDINGS_SERVER]: fakeServer });
  assert.ok(on.systemPrompt.append.includes(FINDINGS_PROMPT_APPEND));
});

test('findings sit beside shared memory rather than replacing it', () => {
  const memoryServer = { name: 'colonizer_memory' };
  const { options } = buildOptions(
    { ...base, COLONIZER_FINDINGS: 'true', COLONIZER_MEMORY_DIR: '/colonizer/memory' },
    { findingsServer: fakeServer, memoryServer },
  );
  assert.deepEqual(Object.keys(options.mcpServers).sort(), ['colonizer_findings', 'colonizer_memory']);
});

test('the gate refuses a subagent even when delegation is off', async () => {
  const { options } = buildOptions({ ...base, COLONIZER_FINDINGS: 'true' }, { findingsServer: fakeServer });
  const hooks = options.hooks.PreToolUse.flatMap((entry) => entry.hooks);
  const decide = async (input) => {
    for (const hook of hooks) {
      const out = await hook({ hook_event_name: 'PreToolUse', tool_input: {}, ...input });
      if (out.hookSpecificOutput?.permissionDecision === 'deny') return out.hookSpecificOutput;
    }
    return null;
  };
  assert.equal(await decide({ tool_name: FINDING_TOOL }), null);
  const denied = await decide({ tool_name: FINDING_TOOL, agent_id: 'agent_01' });
  assert.match(denied.permissionDecisionReason, /Only the orchestrator files findings/);
});
