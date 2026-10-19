// The mattpocock-skills guard (issue #1016): the setting's allowlist, what the PreToolUse hook
// refuses when the pack is mounted, and the pre-seeded setup configuration the prompt carries.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  buildOptions,
  MATTPOCOCK_SKILLS_DEFAULT,
  MATTPOCOCK_SKILLS_EXCLUDED,
  MATTPOCOCK_SKILLS_PROMPT_APPEND,
  mattpocockSkillsAllowlist,
  mattpocockSkillsConfig,
  mattpocockSkillsDenial,
  mattpocockSkillsMounted,
} from '../runner.mjs';

const DIR = '/opt/colonizer/plugins/mattpocock-skills';
const SKILL = (command, extra = {}) => ({ tool_name: 'Skill', tool_input: { command, ...extra }, hook_event_name: 'PreToolUse' });

test('the pack counts as mounted by its in-VM path, or by a bare name', () => {
  assert.equal(mattpocockSkillsMounted([DIR]), true);
  assert.equal(mattpocockSkillsMounted(['mattpocock-skills']), true, 'a hand-set value carries the bare name');
  assert.equal(mattpocockSkillsMounted(['/opt/colonizer/plugins/ponytail']), false);
  assert.equal(mattpocockSkillsMounted([]), false);
  assert.equal(mattpocockSkillsMounted(undefined), false);
});

test('the setting parses to a subset of the curated thirteen', () => {
  assert.deepEqual(mattpocockSkillsAllowlist(undefined), [...MATTPOCOCK_SKILLS_DEFAULT]);
  assert.deepEqual(mattpocockSkillsAllowlist(''), [...MATTPOCOCK_SKILLS_DEFAULT]);
  assert.deepEqual(mattpocockSkillsAllowlist('  '), [...MATTPOCOCK_SKILLS_DEFAULT]);
  assert.deepEqual(mattpocockSkillsAllowlist('tdd, pr'), ['tdd', 'pr']);
  assert.deepEqual(mattpocockSkillsAllowlist(' tdd , pr '), ['tdd', 'pr'], 'names are trimmed');
  // Names outside the curated set are ignored — the interactive and setup ones can never be
  // allowed by this setting, and a value of only such names allows nothing.
  assert.deepEqual(mattpocockSkillsAllowlist('tdd, grill-me, not-a-skill'), ['tdd']);
  assert.deepEqual(mattpocockSkillsAllowlist('grill-me, setup-matt-pocock-skills'), []);
});

test('the curated skills are allowed, and only curated ones can be refused by the allowlist', () => {
  const deny = (command, allowlist) => mattpocockSkillsDenial('Skill', { command }, [DIR], allowlist);
  for (const skill of MATTPOCOCK_SKILLS_DEFAULT) {
    assert.equal(deny(skill), null, skill);
    assert.equal(deny(`mattpocock-skills:${skill}`), null, `${skill} in the pack's namespace`);
  }
  // A narrowed allowlist refuses the curated skills it does not name, bare or namespaced.
  assert.match(deny('pr', ['tdd']), /mattpocock-skills:/);
  assert.match(deny('mattpocock-skills:pr', ['tdd']), /not in this colony's allowlist/);
  // Another pack's namespace is that pack's skill, not this one's, so it stays allowed.
  assert.equal(deny('ecc:pr'), null);
  assert.equal(deny('ecc:code-review'), null);
  // Only the Skill tool is gated.
  assert.equal(mattpocockSkillsDenial('Bash', { command: 'tdd' }, [DIR]), null);
});

test('the setup skill and the interactive ones are refused, by exact name only', () => {
  const deny = (command) => mattpocockSkillsDenial('Skill', { command }, [DIR]);
  assert.match(deny('setup-matt-pocock-skills'), /never run here/);
  assert.match(deny('mattpocock-skills:setup-matt-pocock-skills'), /system prompt/);
  for (const skill of MATTPOCOCK_SKILLS_EXCLUDED) {
    if (skill === 'setup-matt-pocock-skills') continue;
    assert.match(deny(skill), /interactive/, skill);
    assert.match(deny(`mattpocock-skills:${skill}`), /mattpocock-skills:/, `${skill} unqualified`);
  }
  // The default allowlist cannot be widened into the excluded names: the denial holds even when a
  // caller passes them (a later setting would have to opt in through its own path).
  assert.match(mattpocockSkillsDenial('Skill', { command: 'grill-me' }, [DIR], [...MATTPOCOCK_SKILLS_DEFAULT, 'grill-me']), /interactive/);
});

test('nothing is refused when the pack was never mounted', () => {
  assert.equal(mattpocockSkillsDenial('Skill', { command: 'setup-matt-pocock-skills' }, []), null);
  assert.equal(mattpocockSkillsDenial('Skill', { command: 'grill-me' }, []), null);
  assert.equal(mattpocockSkillsDenial('Skill', { command: 'tdd' }, ['/opt/colonizer/plugins/graft'], ['pr']), null);
});

test('the pre-seeded configuration answers what the setup skill would have asked', () => {
  const config = mattpocockSkillsConfig();
  assert.match(config, /origin/, 'the issue tracker is the repository itself');
  assert.match(config, /GitHub tools/);
  assert.match(config, /needs-triage/);
  assert.match(config, /ready-for-agent/);
  assert.match(config, /wontfix/);
  assert.match(config, /never create one/, 'labels are only ever applied, never created');
  assert.match(config, /single-context/);
  assert.match(config, /docs\//);
  // The arguments are honoured, so a caller can seed another repository's answers.
  assert.match(mattpocockSkillsConfig({ repo: 'acme/widgets', docs: 'the `doc/` folder' }), /acme\/widgets/);
  assert.match(mattpocockSkillsConfig({ repo: 'acme/widgets', docs: 'the `doc/` folder' }), /doc\/` folder/);

  const append = MATTPOCOCK_SKILLS_PROMPT_APPEND;
  assert.ok(append.includes(config), 'the prompt append carries the configuration');
  assert.match(append, /setup-matt-pocock-skills/);
  assert.match(append, /## Agent skills/);
  assert.match(append, /never merge, push, or create branches/i);
  assert.match(append, /pr\.md/);
  assert.match(append, /state the assumption/i);
});

test('the prompt block and the gate appear only when the pack is mounted', async () => {
  const denies = async (entries, call) => {
    for (const entry of entries.filter((e) => e.matcher === 'Skill')) {
      const out = await entry.hooks[0](call);
      if (out?.hookSpecificOutput?.permissionDecision === 'deny') return out.hookSpecificOutput.permissionDecisionReason;
    }
    return null;
  };
  const off = buildOptions({ COLONIZER_PLUGIN_DIRS: '/opt/colonizer/plugins/ponytail' });
  assert.equal(off.options.systemPrompt.append.includes(MATTPOCOCK_SKILLS_PROMPT_APPEND), false);
  assert.equal(await denies(off.options.hooks.PreToolUse, SKILL('grill-me')), null);

  const on = buildOptions({ COLONIZER_PLUGIN_DIRS: DIR });
  assert.ok(on.options.systemPrompt.append.includes(MATTPOCOCK_SKILLS_PROMPT_APPEND));
  assert.match(await denies(on.options.hooks.PreToolUse, SKILL('mattpocock-skills:grill-me')), /mattpocock-skills:/);
});

test('the installed hook holds the colony to its allowlist and lets the rest through', async () => {
  const { options } = buildOptions({ COLONIZER_PLUGIN_DIRS: DIR, COLONIZER_MATTPOCOCK_SKILLS: 'tdd, pr' });
  const deny = async (call) => {
    for (const entry of options.hooks.PreToolUse.filter((e) => e.matcher === 'Skill')) {
      const out = await entry.hooks[0](call);
      if (out?.hookSpecificOutput?.permissionDecision === 'deny') return out;
    }
    return null;
  };
  const allowed = await deny(SKILL('mattpocock-skills:tdd'));
  assert.equal(allowed, null);

  const offList = await deny(SKILL('mattpocock-skills:triage'));
  assert.equal(offList.hookSpecificOutput.permissionDecision, 'deny');
  assert.match(offList.hookSpecificOutput.permissionDecisionReason, /allowlist/);

  // A subagent cannot get past it either: the interactive skills stay refused whoever calls them.
  const fromSubagent = await deny({ ...SKILL('mattpocock-skills:setup-matt-pocock-skills'), agent_id: 'agent_01' });
  assert.equal(fromSubagent.hookSpecificOutput.permissionDecision, 'deny');
});
