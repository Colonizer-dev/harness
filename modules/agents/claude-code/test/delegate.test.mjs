import assert from 'node:assert/strict';
import { test } from 'node:test';

import { buildOptions, DELEGATE_PROMPT_APPEND, delegationDecision, ENFORCE_PROMPT_APPEND } from '../runner.mjs';

const ORCHESTRATOR = {};
const SUBAGENT = { agent_id: 'agent_01' };

test('the orchestrator keeps planning, asking and delegating', () => {
  for (const tool of ['Task', 'Agent', 'AskUserQuestion', 'TodoWrite', 'EnterPlanMode', 'ExitPlanMode']) {
    assert.equal(delegationDecision(tool, {}, ORCHESTRATOR), null, tool);
  }
  // Shared memory is the orchestrator's own context, not work handed to a subagent.
  assert.equal(delegationDecision('mcp__colonizer_memory__memory_search', {}, ORCHESTRATOR), null);
});

// Regression guard for issue #182, where the gate refused 98.5% of SendMessage calls: the Agent tool's
// result text hands back an agentId to continue with SendMessage, ListAgents is the directory that
// resolves that id, and TaskStop retires a background agent, so the harness was inviting exactly what
// its own gate refused.
test('the directing tools the harness\'s own text points the orchestrator at are allowed', () => {
  for (const tool of ['SendMessage', 'ListAgents', 'TaskStop']) {
    assert.equal(delegationDecision(tool, {}, ORCHESTRATOR), null, tool);
  }
});

test('the orchestrator is refused the work itself, by name', () => {
  for (const tool of ['Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'WebFetch', 'TaskOutput']) {
    const reason = delegationDecision(tool, {}, ORCHESTRATOR);
    assert.ok(reason, `${tool} should be refused`);
    assert.match(reason, new RegExp(`^${tool} belongs to your subagents`));
    assert.match(reason, /Task tool/);
  }
});

test('a tool nobody has heard of is refused rather than allowed', () => {
  assert.ok(delegationDecision('SomeToolAddedNextYear', {}, ORCHESTRATOR));
});

test('the PR description stays the orchestrator\'s to write', () => {
  assert.equal(delegationDecision('Write', { file_path: '/harness/out/pr.md' }, ORCHESTRATOR), null);
  assert.equal(delegationDecision('Read', { file_path: '/harness/out/pr.md' }, ORCHESTRATOR), null);
  assert.ok(delegationDecision('Write', { file_path: '/workspace/src/main.rs' }, ORCHESTRATOR));
});

test('a subagent is not constrained: it is the one doing the work', () => {
  for (const tool of ['Read', 'Edit', 'Write', 'Bash', 'Grep']) {
    assert.equal(delegationDecision(tool, { file_path: '/workspace/src/main.rs' }, SUBAGENT), null, tool);
  }
});

test('delegation is enforced unless it is explicitly loosened', () => {
  const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude' };
  const gated = (env) => Boolean(buildOptions({ ...base, ...env }).options.hooks?.PreToolUse?.[0]?.hooks?.[0]);
  assert.ok(gated({}), 'unset means enforce');
  assert.ok(gated({ COLONIZER_DELEGATE: 'enforce' }));
  assert.ok(gated({ COLONIZER_DELEGATE: 'nonsense' }), 'a typo must not quietly switch the gate off');
  assert.ok(!gated({ COLONIZER_DELEGATE: 'encourage' }));
  assert.ok(!gated({ COLONIZER_DELEGATE: 'off' }));
});

test('both delegating modes tell the agent, and off says nothing', () => {
  const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude' };
  const appendOf = (env) => buildOptions({ ...base, ...env }).options.systemPrompt.append;
  assert.ok(appendOf({}).includes(DELEGATE_PROMPT_APPEND), 'the default delegates');
  assert.ok(!appendOf({ COLONIZER_DELEGATE: 'off' }).includes(DELEGATE_PROMPT_APPEND));
  assert.ok(appendOf({ COLONIZER_DELEGATE: 'encourage' }).includes(DELEGATE_PROMPT_APPEND));
  assert.ok(appendOf({ COLONIZER_DELEGATE: 'enforce' }).includes(DELEGATE_PROMPT_APPEND));
});

test('the enforced boundary is named in the prompt under enforce, and only there', () => {
  const base = { COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude' };
  const appendOf = (env) => buildOptions({ ...base, ...env }).options.systemPrompt.append;
  assert.ok(appendOf({ COLONIZER_DELEGATE: 'enforce' }).includes(ENFORCE_PROMPT_APPEND));
  assert.ok(appendOf({}).includes(ENFORCE_PROMPT_APPEND), 'the default is enforce');
  assert.ok(!appendOf({ COLONIZER_DELEGATE: 'encourage' }).includes(ENFORCE_PROMPT_APPEND), 'encourage has no gate, so the text would be false');
  assert.ok(!appendOf({ COLONIZER_DELEGATE: 'off' }).includes(ENFORCE_PROMPT_APPEND), 'off has no gate either');
  for (const tool of ['SendMessage', 'Bash']) {
    assert.ok(ENFORCE_PROMPT_APPEND.includes(tool), `${tool} is named so the model need not rediscover it`);
  }
});

test('the installed hook denies with the reason the model will read', async () => {
  const hook = buildOptions({ COLONIZER_DELEGATE: 'enforce' }).options.hooks.PreToolUse[0].hooks[0];

  const allowed = await hook({ tool_name: 'Task', tool_input: {}, hook_event_name: 'PreToolUse' });
  assert.deepEqual(allowed, { continue: true });

  const denied = await hook({ tool_name: 'Bash', tool_input: { command: 'ls' }, hook_event_name: 'PreToolUse' });
  assert.equal(denied.continue, true, 'the turn carries on; only the call is refused');
  assert.equal(denied.hookSpecificOutput.permissionDecision, 'deny');
  assert.match(denied.hookSpecificOutput.permissionDecisionReason, /Bash belongs to your subagents/);

  const fromSubagent = await hook({ tool_name: 'Bash', tool_input: { command: 'ls' }, agent_id: 'agent_01', hook_event_name: 'PreToolUse' });
  assert.deepEqual(fromSubagent, { continue: true }, 'a subagent runs commands freely');
});
