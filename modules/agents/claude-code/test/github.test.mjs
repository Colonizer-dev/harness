import assert from 'node:assert/strict';
import { test } from 'node:test';

import { z } from 'zod';

import { createGithubServer, GITHUB_PROMPT_APPEND, GITHUB_SERVER, GITHUB_TOOLS, githubDecision, QUEUED_REPLY } from '../github.mjs';
import { buildOptions } from '../runner.mjs';

const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude', COLONIZER_DELEGATE: 'off' };
const fakeServer = { name: GITHUB_SERVER };

/** Stand-ins for the SDK's tool() and createSdkMcpServer(); zod is real, so each shape holds real schemas. */
function fakeSdk() {
  const tool = (name, description, shape, handler) => ({ name, description, shape, handler });
  const createSdkMcpServer = (server) => server;
  return { z, tool, createSdkMcpServer };
}

const serverOf = (events) => {
  const server = createGithubServer({ emit: (e) => events.push(e), ...fakeSdk() });
  return { server, tools: Object.fromEntries(server.tools.map((t) => [t.name, t])) };
};

test('the six write tools each emit their action and tell the agent where the outcome goes', async () => {
  const events = [];
  const { server, tools } = serverOf(events);
  assert.equal(server.name, GITHUB_SERVER);
  assert.deepEqual(server.tools.map((t) => t.name), [
    'issue_label',
    'issue_comment',
    'issue_close_duplicate',
    'pr_comment',
    'pr_label',
    'pr_merge',
  ]);
  assert.deepEqual(GITHUB_TOOLS, [
    'mcp__colonizer_github__issue_label',
    'mcp__colonizer_github__issue_comment',
    'mcp__colonizer_github__issue_close_duplicate',
    'mcp__colonizer_github__pr_comment',
    'mcp__colonizer_github__pr_label',
    'mcp__colonizer_github__pr_merge',
  ]);

  const sha = 'a'.repeat(40);
  const replies = [
    await tools.issue_label.handler({ issue: 42, labels: ['bug', 'P1'] }),
    await tools.issue_comment.handler({ issue: 42, body: 'Which version?' }),
    await tools.issue_close_duplicate.handler({ issue: 43, duplicate_of: 42 }),
    await tools.pr_comment.handler({ pr: 219, body: 'Looks good.' }),
    await tools.pr_label.handler({ pr: 219, labels: ['needs-human'] }),
    await tools.pr_merge.handler({ pr: 231, head_sha: sha, reason: 'checks are green' }),
  ];
  assert.deepEqual(events, [
    { type: 'github_action', tool: 'issue_label', issue: 42, labels: ['bug', 'P1'] },
    { type: 'github_action', tool: 'issue_comment', issue: 42, body: 'Which version?' },
    { type: 'github_action', tool: 'issue_close_duplicate', issue: 43, duplicate_of: 42 },
    { type: 'github_action', tool: 'pr_comment', pr: 219, body: 'Looks good.' },
    { type: 'github_action', tool: 'pr_label', pr: 219, labels: ['needs-human'] },
    { type: 'github_action', tool: 'pr_merge', pr: 231, head_sha: sha, reason: 'checks are green' },
  ]);
  for (const reply of replies) assert.equal(reply.content[0].text, QUEUED_REPLY);
});

test('the shape of each tool names exactly the fields the host validates', () => {
  // The zod bounds (a positive whole issue or PR number, at most ten labels, a comment up to 20 000
  // chars, a 40-hex head SHA) are enforced by the SDK through these shapes and re-checked on the host,
  // which is the boundary that matters (loop_github.rs::parse_action).
  const { tools } = serverOf([]);
  assert.deepEqual(Object.keys(tools.issue_label.shape), ['issue', 'labels']);
  assert.deepEqual(Object.keys(tools.issue_comment.shape), ['issue', 'body']);
  assert.deepEqual(Object.keys(tools.issue_close_duplicate.shape), ['issue', 'duplicate_of']);
  assert.deepEqual(Object.keys(tools.pr_comment.shape), ['pr', 'body']);
  assert.deepEqual(Object.keys(tools.pr_label.shape), ['pr', 'labels']);
  assert.deepEqual(Object.keys(tools.pr_merge.shape), ['pr', 'head_sha', 'reason']);
});

test('the zod bounds reject a bad PR number, a bad head_sha or over-long labels before anything is emitted', () => {
  const schemaFor = (name) => z.object(serverOf([]).tools[name].shape);
  assert.equal(schemaFor('issue_comment').safeParse({ issue: 0, body: 'x' }).success, false, 'issue must be positive');
  assert.equal(schemaFor('pr_comment').safeParse({ pr: 0, body: 'x' }).success, false, 'pr must be positive');
  assert.equal(schemaFor('pr_label').safeParse({ pr: -1, labels: ['a'] }).success, false, 'pr must be positive');
  assert.equal(schemaFor('pr_merge').safeParse({ pr: 231, head_sha: 'nope', reason: 'x' }).success, false, 'head_sha must be hex');
  assert.equal(schemaFor('pr_merge').safeParse({ pr: 231, head_sha: 'a'.repeat(39), reason: 'x' }).success, false, 'head_sha must be 40 chars');
  assert.equal(schemaFor('pr_merge').safeParse({ pr: 231, head_sha: 'a'.repeat(40), reason: 'x' }).success, true, 'a 40-hex SHA is accepted');
  assert.equal(schemaFor('pr_merge').safeParse({ pr: 231, head_sha: 'A'.repeat(40), reason: 'x' }).success, true, 'either case, the host lowercases');
  assert.equal(schemaFor('pr_label').safeParse({ pr: 1, labels: [] }).success, false, 'at least one label');
  assert.equal(schemaFor('pr_label').safeParse({ pr: 1, labels: ['a'.repeat(101)] }).success, false, 'a label of at most 100 chars');
});

test('only the orchestrator writes: a subagent is told to report instead', () => {
  for (const tool of GITHUB_TOOLS) assert.equal(githubDecision(tool, {}), null, 'the orchestrator may ask');
  for (const tool of GITHUB_TOOLS) {
    assert.match(githubDecision(tool, { agent_id: 'agent_01' }), /Only the orchestrator writes to GitHub/);
  }
  assert.equal(githubDecision('Bash', { agent_id: 'agent_01' }), null, 'other tools are not this gate’s business');
});

test('the prompt tells the model the PR tools exist and that pr_merge is opt-in', () => {
  assert.match(GITHUB_PROMPT_APPEND, /pr_comment, pr_label and pr_merge/);
  assert.match(GITHUB_PROMPT_APPEND, /pr_merge is refused unless the operator has switched merges on/);
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
