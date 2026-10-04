import assert from 'node:assert/strict';
import { test } from 'node:test';

import { createGithubServer, GITHUB_PROMPT_APPEND, GITHUB_SERVER, GITHUB_TOOLS, githubDecision, QUEUED_REPLY } from '../github.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const fakeServer = { name: GITHUB_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(), and a zod that accepts anything. */
function fakeSdk() {
  const chain = new Proxy(() => chain, { get: () => () => chain });
  const z = { string: () => chain, number: () => chain, array: () => chain };
  const tool = (name, description, schema, handler) => ({ name, description, schema, handler });
  const createSdkMcpServer = (server) => server;
  return { z, tool, createSdkMcpServer };
}

const serverOf = (events) => {
  const server = createGithubServer({ emit: (e) => events.push(e), ...fakeSdk() });
  return { server, tools: Object.fromEntries(server.tools.map((t) => [t.name, t])) };
};

test('the three write tools each emit their action and tell the agent where the outcome goes', async () => {
  const events = [];
  const { server, tools } = serverOf(events);
  assert.equal(server.name, GITHUB_SERVER);
  assert.deepEqual(server.tools.map((t) => t.name), ['issue_label', 'issue_comment', 'issue_close_duplicate']);
  assert.deepEqual(GITHUB_TOOLS, [
    'mcp__colonizer_github__issue_label',
    'mcp__colonizer_github__issue_comment',
    'mcp__colonizer_github__issue_close_duplicate',
  ]);

  const label = await tools.issue_label.handler({ issue: 42, labels: ['bug', 'P1'] });
  const comment = await tools.issue_comment.handler({ issue: 42, body: 'Which version?' });
  const close = await tools.issue_close_duplicate.handler({ issue: 43, duplicate_of: 42 });
  assert.deepEqual(events, [
    { type: 'github_action', tool: 'issue_label', issue: 42, labels: ['bug', 'P1'] },
    { type: 'github_action', tool: 'issue_comment', issue: 42, body: 'Which version?' },
    { type: 'github_action', tool: 'issue_close_duplicate', issue: 43, duplicate_of: 42 },
  ]);
  for (const reply of [label, comment, close]) assert.equal(reply.content[0].text, QUEUED_REPLY);
});

test('the shape of each tool names exactly the fields the host validates', () => {
  // The zod bounds (a positive whole issue number, at most ten labels, a comment up to 20 000 chars)
  // are enforced by the SDK through these shapes and re-checked on the host, which is the boundary
  // that matters (loop_github.rs::parse_action).
  const { tools } = serverOf([]);
  assert.deepEqual(Object.keys(tools.issue_label.schema), ['issue', 'labels']);
  assert.deepEqual(Object.keys(tools.issue_comment.schema), ['issue', 'body']);
  assert.deepEqual(Object.keys(tools.issue_close_duplicate.schema), ['issue', 'duplicate_of']);
});

test('only the orchestrator writes: a subagent is told to report instead', () => {
  for (const tool of GITHUB_TOOLS) assert.equal(githubDecision(tool, {}), null, 'the orchestrator may ask');
  assert.match(githubDecision(GITHUB_TOOLS[0], { agent_id: 'agent_01' }), /Only the orchestrator writes to GitHub/);
  assert.equal(githubDecision('Bash', { agent_id: 'agent_01' }), null, 'other tools are not this gate’s business');
});

test('the tools are wired in only when the mothership switched them on and the server exists', () => {
  const off = buildOptions({ ...base }, { githubServer: fakeServer }).options;
  assert.equal(off.mcpServers, undefined);
  assert.ok(!off.systemPrompt.append.includes(GITHUB_PROMPT_APPEND));

  const noServer = buildOptions({ ...base, COLONIZER_GITHUB: 'true' }).options;
  assert.equal(noServer.mcpServers, undefined);

  const on = buildOptions({ ...base, COLONIZER_GITHUB: 'true' }, { githubServer: fakeServer }).options;
  assert.deepEqual(on.mcpServers, { [GITHUB_SERVER]: fakeServer });
  assert.ok(on.systemPrompt.append.includes(GITHUB_PROMPT_APPEND));
});

// The buildOptions wiring (coexistence with findings/memory, the PreToolUse hook plumbing) is
// runner.test.mjs's coverage; here only the gating and the server's own behaviour are asserted.
