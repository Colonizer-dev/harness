// The understand-anything cost guard (issue #1014): what the colony is told when the skillset is
// mounted, and which three skills the PreToolUse hook refuses.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  buildOptions,
  skillNameFromInput,
  UNDERSTAND_ANYTHING_DENIED,
  UNDERSTAND_ANYTHING_PROMPT_APPEND,
  understandAnythingDenial,
  understandAnythingMounted,
} from '../runner.mjs';

const DIR = '/opt/colonizer/plugins/understand-anything';
const SKILL = (command, extra = {}) => ({ tool_name: 'Skill', tool_input: { command, ...extra }, hook_event_name: 'PreToolUse' });

test('the skillset counts as mounted by its in-VM path, or by a bare name', () => {
  assert.equal(understandAnythingMounted([DIR]), true);
  assert.equal(understandAnythingMounted(['understand-anything']), true, 'a hand-set value carries the bare name');
  assert.equal(understandAnythingMounted(['/opt/colonizer/plugins/graft']), false);
  // An operator's own directory with a similar name is not this plugin: only the last segment counts.
  assert.equal(understandAnythingMounted(['/opt/colonizer/plugins/understand-anything-fork']), false);
  assert.equal(understandAnythingMounted([]), false);
  assert.equal(understandAnythingMounted(undefined), false);
});

test('the skill is read out of the Skill tool input, however it is spelled', () => {
  assert.equal(skillNameFromInput({ skill: 'understand-anything:understand-explain' }), 'understand-explain');
  assert.equal(skillNameFromInput({ skill: 'understand' }), 'understand');
  assert.equal(skillNameFromInput({ skill: '/understand' }), 'understand', 'a leading slash is how it is typed');
  assert.equal(skillNameFromInput({ skill: 'understand-anything:understand src/main.rs', args: 'src/main.rs' }), 'understand');
  assert.equal(skillNameFromInput({ command: 'understand-anything:understand-figma' }), 'understand-figma', 'the older field name');
  assert.equal(skillNameFromInput({ name: 'understand-dashboard' }), 'understand-dashboard', 'and another');
  assert.equal(skillNameFromInput({ skill_name: 'understand' }), 'understand');
  assert.equal(skillNameFromInput({}), '');
  assert.equal(skillNameFromInput(null), '');
});

test('the full pass and the two network skills are refused, by exact name only', () => {
  const deny = (command) => understandAnythingDenial('Skill', { command }, [DIR]);
  for (const skill of UNDERSTAND_ANYTHING_DENIED.keys()) {
    assert.match(deny(`understand-anything:${skill}`), /understand-anything:/, skill);
    assert.match(deny(skill), /understand-anything:/, `${skill} unqualified`);
  }
  // The file-scoped commands the reason points at are exactly what stays allowed.
  for (const allowed of ['understand-anything:understand-explain', 'understand-explain', 'understand-anything:understand-diff', 'understand-anything:understand-chat', 'understand-anything:understand-domain']) {
    assert.equal(deny(allowed), null, allowed);
  }
  // Another pack's skill that happens to share a name is not this pack's skill.
  for (const other of ['other-pack:understand', 'other-pack:understand-dashboard', 'other-pack:understand-figma']) {
    assert.equal(deny(other), null, other);
  }
  assert.equal(understandAnythingDenial('Bash', { command: 'understand' }, [DIR]), null, 'only the Skill tool');
});

test('nothing is refused when the skillset was never mounted', () => {
  assert.equal(understandAnythingDenial('Skill', { command: 'understand' }, []), null);
  assert.equal(understandAnythingDenial('Skill', { command: 'understand' }, ['/opt/colonizer/plugins/graft']), null);
});

test('the reason explains the guard and names what to use instead', () => {
  assert.match(understandAnythingDenial('Skill', { command: 'understand' }, [DIR]), /\/understand-explain <file>/);
  assert.match(understandAnythingDenial('Skill', { command: 'understand' }, [DIR]), /token cost/);
  assert.match(understandAnythingDenial('Skill', { command: 'understand-dashboard' }, [DIR]), /vite server/);
  assert.match(understandAnythingDenial('Skill', { command: 'understand-figma' }, [DIR]), /Figma API/);
});

test('the prompt block and the gate appear only when the plugin is mounted', () => {
  const off = buildOptions({ COLONIZER_PLUGIN_DIRS: '/opt/colonizer/plugins/graft' });
  assert.equal(off.options.systemPrompt.append.includes(UNDERSTAND_ANYTHING_PROMPT_APPEND), false);
  assert.equal(off.options.hooks.PreToolUse.some((entry) => entry.matcher === 'Skill'), false);

  const on = buildOptions({ COLONIZER_PLUGIN_DIRS: DIR });
  assert.ok(on.options.systemPrompt.append.includes(UNDERSTAND_ANYTHING_PROMPT_APPEND));
  assert.match(on.options.systemPrompt.append, /git-ignored/);
  assert.ok(on.options.hooks.PreToolUse.some((entry) => entry.matcher === 'Skill'));
});

test('the installed hook denies the three skills and lets the rest through', async () => {
  const { options } = buildOptions({ COLONIZER_PLUGIN_DIRS: DIR });
  const hook = options.hooks.PreToolUse.find((entry) => entry.matcher === 'Skill').hooks[0];

  const allowed = await hook(SKILL('understand-anything:understand-explain src/api.rs'));
  assert.deepEqual(allowed, { continue: true });

  const denied = await hook(SKILL('understand-anything:understand'));
  assert.equal(denied.continue, true, 'the turn carries on; only the call is refused');
  assert.equal(denied.hookSpecificOutput.permissionDecision, 'deny');
  assert.match(denied.hookSpecificOutput.permissionDecisionReason, /understand-anything:/);

  // A subagent cannot get past it either — the whole-repository pass is expensive whoever runs it.
  const fromSubagent = await hook({ ...SKILL('understand'), agent_id: 'agent_01' });
  assert.equal(fromSubagent.hookSpecificOutput.permissionDecision, 'deny');
});