import assert from 'node:assert/strict';
import { test } from 'node:test';

import { buildOptions, DELEGATE_PROMPT_APPEND, delegationDecision, ENFORCE_PROMPT_APPEND, ORCHESTRATOR_TOOLS } from '../runner.mjs';
import { FINDING_TOOL } from '../findings.mjs';
import { MEMORY_TOOLS } from '../memory.mjs';
import { WAIT_TOOL } from '../wait.mjs';

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

test('waiting passes the gate for the orchestrator and its subagents alike', () => {
  // A wait is not work handed to a subagent: the orchestrator holds the turn itself, and a subagent
  // running the long build is exactly who would otherwise poll a log.
  assert.equal(delegationDecision('mcp__colonizer_wait__wait', { seconds: 30 }, ORCHESTRATOR), null);
  assert.equal(delegationDecision('mcp__colonizer_wait__wait', { file: '/tmp/build.log', pattern: 'test result: ok' }, SUBAGENT), null);
});

test('the orchestrator is refused the work itself, by name', () => {
  for (const tool of ['Read', 'Edit', 'Write', 'Bash', 'Grep', 'Glob', 'WebFetch', 'TaskOutput']) {
    const reason = delegationDecision(tool, {}, ORCHESTRATOR);
    assert.ok(reason, `${tool} should be refused`);
    assert.match(reason, new RegExp(`^${tool} belongs to your subagents`));
    assert.match(reason, /Task tool/);
  }
});

// Regression guard for issue #188: the superpowers bootstrap tells the orchestrator to load skills
// with the Skill tool, and a skill only brings instructions in; whatever it says to do still meets
// the gate tool by tool.
test('loading a skill is the orchestrator\'s own planning, not work', () => {
  assert.equal(delegationDecision('Skill', { skill: 'superpowers:brainstorming' }, ORCHESTRATOR), null);
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
  for (const tool of ['SendMessage', 'Skill', 'Bash']) {
    assert.ok(ENFORCE_PROMPT_APPEND.includes(tool), `${tool} is named so the model need not rediscover it`);
  }
});

// Issue #188 as a standing contract: the prompt text and the gate are two encodings of one boundary,
// so they are checked against each other rather than against a reading of the sentences. The enforce
// text's allow-list sentence must name exactly the gate's allow set, every tool name the
// orchestrator's prompt mentions must get a definite verdict, the tools it offers must be granted
// while the rest it names are refused, and a subagent is never refused at all.
test('the prompt the orchestrator receives and the gate decide every named tool the same way', () => {
  // The same append the runner builds, including the servers every colony wires up.
  const prompt = buildOptions(
    {
      COLONIZER_CLAUDE_BIN: '/opt/claude/bin/claude',
      COLONIZER_DELEGATE: 'enforce',
      COLONIZER_MEMORY_DIR: '/colonizer/memory',
      COLONIZER_FINDINGS: 'true',
    },
    { waitServer: {}, memoryServer: {}, findingsServer: {} },
  ).options.systemPrompt.append;

  // Claude Code's own tools, plus the colony's mcp tools, which its prompts name bare or prefixed.
  const SDK_TOOLS = [
    'Agent', 'AskUserQuestion', 'Bash', 'BashOutput', 'Edit', 'EnterPlanMode', 'ExitPlanMode', 'Glob',
    'Grep', 'KillBash', 'KillShell', 'ListAgents', 'MultiEdit', 'NotebookEdit', 'Read', 'SendMessage',
    'Skill', 'Task', 'TaskOutput', 'TaskStop', 'TodoWrite', 'WebFetch', 'WebSearch', 'Write',
  ];
  const colonizerTools = [WAIT_TOOL, ...MEMORY_TOOLS, FINDING_TOOL];
  const mentioned = new Set([
    ...SDK_TOOLS.filter((name) => new RegExp(`\\b${name}\\b`).test(prompt)),
    ...colonizerTools,
    ...(prompt.match(/\bmcp__[a-z0-9_]+__[a-z0-9_]+/g) ?? []),
  ]);

  for (const name of mentioned) {
    // Write is invited only for the pull request description, so ask for exactly that.
    const input = name === 'Write' ? { file_path: '/harness/out/pr.md' } : {};
    const reason = delegationDecision(name, input, ORCHESTRATOR);
    if (ORCHESTRATOR_TOOLS.has(name) || name.startsWith('mcp__') || name === 'Write') {
      assert.equal(reason, null, `${name} is invited, so the gate grants it`);
    } else {
      assert.match(String(reason), new RegExp(`^${name} belongs to your subagents`), `${name} is not invited, so the gate refuses it`);
    }
  }

  // Set equality: a tool allowed but unannounced, or announced but refused, is issue #188 again.
  const allowSentence = ENFORCE_PROMPT_APPEND.split('\n')[0];
  const named = SDK_TOOLS.filter((name) => new RegExp(`\\b${name}\\b`).test(allowSentence));
  assert.deepEqual(new Set(named), new Set([...ORCHESTRATOR_TOOLS, 'Write']));

  // Inheritance: a subagent's calls carry agent_id, and this gate never refuses one.
  for (const name of new Set([...SDK_TOOLS, ...colonizerTools])) {
    assert.equal(delegationDecision(name, {}, SUBAGENT), null, name);
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
