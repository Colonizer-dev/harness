// Per-ant skillsets (issue #1163): the runner's registry of known ants, and the PreToolUse Skill
// gate that holds an ant to the skill packs its agent file lists.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

import { antRegistry, antSkillsetDenial, buildOptions, CREW_PACK_NAME } from '../runner.mjs';
import { EXPLORE_DISALLOWED } from '../subagents.mjs';

const SKILL = (skill, extra = {}) => ({ tool_name: 'Skill', tool_input: { skill, ...extra }, hook_event_name: 'PreToolUse' });

/**
 * A temp colony of packs: archify (two skills, three ants, one of them a queen), ponytail (skills
 * only, no agents dir), broken-pack (one unparseable agent file beside a good one) and empty-pack
 * (an agents dir with no files). `pack(name)` is the pack's directory, so the last path segment is
 * the pack name exactly as COLONIZER_PLUGIN_DIRS carries it.
 */
async function withPacks(build) {
  const root = mkdtempSync(join(tmpdir(), 'colonizer-ants-'));
  const pack = (name) => join(root, name);
  const skillMd = (p, name) => {
    mkdirSync(join(pack(p), 'skills', name), { recursive: true });
    writeFileSync(join(pack(p), 'skills', name, 'SKILL.md'), `---\nname: ${name}\ndescription: d\n---\nBody.\n`);
  };
  const agentMd = (p, file, frontmatter, body = 'Body.') => {
    mkdirSync(join(pack(p), 'agents'), { recursive: true });
    writeFileSync(join(pack(p), 'agents', file), `---\n${frontmatter}---\n${body}\n`);
  };
  skillMd('archify', 'graft');
  skillMd('archify', 'braid');
  agentMd('archify', 'kira.md', 'name: kira\ndescription: Fetches and carries.\nskillsets: [archify]\ntools: [Read, Grep]\ndisallowedTools: [Bash]\n');
  agentMd('archify', 'matriarch.md', 'name: matriarch\ndescription: Lays the work.\nskillsets: [archify]\nant:\n  display_name: Matriarch\n  caste: queen\n');
  agentMd('archify', 'hollow.md', 'name: hollow\ndescription: Carries an empty list.\nskillsets: []\n');
  skillMd('ponytail', 'comb');
  agentMd('broken-pack', 'bad.md', 'name: nox\ndescription: d\neffort: extreme\n');
  agentMd('broken-pack', 'ok.md', 'name: dex\ndescription: Lists no skillsets.\n');
  mkdirSync(join(pack('empty-pack'), 'agents'), { recursive: true });
  try {
    return await build({ pack });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test('the registry reads every pack’s agents, keyed by subagent type name', () => {
  withPacks(({ pack }) => {
    const { ants, queen, warnings } = antRegistry([
      { packName: 'archify', dir: pack('archify') },
      { packName: 'ponytail', dir: pack('ponytail') },
    ]);
    assert.deepEqual(warnings, []);
    assert.deepEqual([...ants.keys()].sort(), ['hollow', 'kira', 'matriarch'], 'a pack without agents/ contributes nothing');
    assert.deepEqual(ants.get('kira'), {
      name: 'kira',
      pack: 'archify',
      skillsets: ['archify'],
      tools: ['Read', 'Grep'],
      disallowedTools: ['Bash'],
    });
    assert.deepEqual(ants.get('hollow'), { name: 'hollow', pack: 'archify', skillsets: [] });
    assert.equal(queen, ants.get('matriarch'), 'the first queen-caste ant is the colony’s queen');
    assert.equal(queen.caste, 'queen');
  });
});

test('the built-in crew pack scans like any other pack', () => {
  const { ants, queen, warnings } = antRegistry([
    { packName: CREW_PACK_NAME, dir: fileURLToPath(new URL('../crew/', import.meta.url)) },
  ]);
  assert.deepEqual(warnings, []);
  assert.deepEqual(ants.get('silka').skillsets, ['archify'], 'silka carries archify');
  assert.equal(ants.get('silka').pack, CREW_PACK_NAME);
  assert.ok(!('skillsets' in ants.get('sarge')), 'sarge lists no skillsets');
  assert.deepEqual(ants.get('Explore').disallowedTools, EXPLORE_DISALLOWED);
  assert.ok(ants.has('general-purpose'));
  assert.equal(queen.name, 'queen');
});

test('an unparseable agent file is skipped with a warning, its pack’s other ants still register', () => {
  withPacks(({ pack }) => {
    const { ants, queen, warnings } = antRegistry([{ packName: 'broken-pack', dir: pack('broken-pack') }]);
    assert.equal(warnings.length, 1);
    assert.match(warnings[0], /broken-pack\/agents\/bad\.md/);
    assert.match(warnings[0], /effort/);
    assert.ok(ants.has('dex'));
    assert.ok(!ants.has('nox'));
    assert.equal(queen, null);
  });
});

test('an empty agents directory and a missing directory contribute nothing, without warnings', () => {
  withPacks(({ pack }) => {
    const { ants, queen, warnings } = antRegistry([
      { packName: 'empty-pack', dir: pack('empty-pack') },
      { packName: 'ghost', dir: pack('ghost') },
    ]);
    assert.deepEqual([...ants.keys()], []);
    assert.equal(queen, null);
    assert.deepEqual(warnings, []);
  });
});

test('the gate allows the ant that carries the pack and refuses the one that does not', () => {
  withPacks(({ pack }) => {
    const registry = antRegistry([{ packName: 'archify', dir: pack('archify') }]);
    const dirs = [pack('archify'), pack('ponytail')];
    // kira carries archify: graft and braid pass, however the skill is spelled.
    assert.equal(antSkillsetDenial({ agent_type: 'kira', tool_input: { skill: 'archify:graft' } }, registry, dirs), null);
    assert.equal(antSkillsetDenial({ agent_type: 'kira', tool_input: { skill: 'graft' } }, registry, dirs), null);
    assert.equal(antSkillsetDenial({ agent_type: 'kira', tool_input: { skill: 'archify:braid' } }, registry, dirs), null);
    // kira does not carry ponytail: comb is refused, naming her packs and the skill.
    const denied = antSkillsetDenial({ agent_type: 'kira', tool_input: { skill: 'ponytail:comb' } }, registry, dirs);
    assert.match(denied, /skillset "ponytail" is not one of kira's packs \(archify\)/);
    assert.match(denied, /comb/);
    // agent_id is a unique id per the SDK; one that exactly equals a type name still resolves.
    assert.match(antSkillsetDenial({ agent_id: 'kira', tool_input: { skill: 'comb' } }, registry, dirs), /ponytail/);
  });
});

test('every miss fails open: unknown ants, ants without a list, unattributable skills', () => {
  withPacks(({ pack }) => {
    const registry = antRegistry([
      { packName: 'archify', dir: pack('archify') },
      { packName: 'broken-pack', dir: pack('broken-pack') },
    ]);
    const dirs = [pack('archify'), pack('ponytail')];
    const comb = { tool_input: { skill: 'ponytail:comb' } };
    assert.equal(antSkillsetDenial({ agent_type: 'Plan', ...comb }, registry, dirs), null, 'an unregistered type');
    assert.equal(antSkillsetDenial({ agent_id: 'agent_01', ...comb }, registry, dirs), null, 'a unique agent id is no ant');
    assert.equal(antSkillsetDenial({ agent_type: 'dex', ...comb }, registry, dirs), null, 'dex lists no skillsets');
    assert.equal(
      antSkillsetDenial({ agent_type: 'kira', tool_input: { skill: 'superpowers:brainstorming' } }, registry, dirs),
      null,
      'no mounted pack ships it',
    );
    assert.equal(antSkillsetDenial({ agent_type: 'kira', tool_input: {} }, registry, dirs), null, 'no skill named');
    assert.equal(antSkillsetDenial({}, registry, dirs), null, 'no hook input at all');
    // An empty skillsets list is a whitelist of nothing: attributable skills are refused.
    assert.match(antSkillsetDenial({ agent_type: 'hollow', ...comb }, registry, dirs), /not one of hollow's packs \(\)/);
    // Without a queen in the registry, the main thread has no list to apply.
    const queenless = antRegistry([{ packName: 'broken-pack', dir: pack('broken-pack') }]);
    assert.equal(antSkillsetDenial(comb, queenless, dirs), null);
  });
});

test('the queen’s own list governs the main thread, where no agent field arrives', () => {
  withPacks(({ pack }) => {
    const registry = antRegistry([{ packName: 'archify', dir: pack('archify') }]);
    const dirs = [pack('archify'), pack('ponytail')];
    assert.equal(antSkillsetDenial({ tool_input: { skill: 'graft' } }, registry, dirs), null);
    assert.match(
      antSkillsetDenial({ tool_input: { skill: 'ponytail:comb' } }, registry, dirs),
      /not one of matriarch's packs \(archify\)/,
    );
    // An agent_type without an agent_id (--agent sessions) resolves like any other.
    assert.equal(antSkillsetDenial({ agent_type: 'matriarch', tool_input: { skill: 'graft' } }, registry, dirs), null);
  });
});

test('the mounted hook holds the crew’s silka to archify and leaves everyone else alone', async () => {
  await withPacks(async ({ pack }) => {
    const { options, warnings } = buildOptions({ COLONIZER_PLUGIN_DIRS: `${pack('archify')},${pack('ponytail')}` });
    assert.deepEqual(warnings, []);
    const hook = options.hooks.PreToolUse.filter((entry) => entry.matcher === 'Skill').at(-1).hooks[0];
    const call = (input, extra = {}) => hook({ tool_name: 'Skill', hook_event_name: 'PreToolUse', tool_input: input, ...extra });

    assert.deepEqual(await call({ skill: 'archify:graft' }, { agent_type: 'silka' }), { continue: true });
    const denied = await call({ skill: 'ponytail:comb' }, { agent_type: 'silka' });
    assert.equal(denied.continue, true, 'the turn carries on; only the call is refused');
    assert.equal(denied.hookSpecificOutput.permissionDecision, 'deny');
    assert.match(denied.hookSpecificOutput.permissionDecisionReason, /skillset "ponytail" is not one of silka's packs \(archify\)/);
    assert.deepEqual(await call({ skill: 'ponytail:comb' }, { agent_type: 'sarge' }), { continue: true }, 'sarge lists no skillsets');
    assert.deepEqual(await call({ skill: 'ponytail:comb' }, { agent_id: 'agent_01' }), { continue: true }, 'an unknown agent id passes');
    assert.deepEqual(await call({ skill: 'ponytail:comb' }), { continue: true }, 'the queen lists no skillsets, so the main thread passes');
  });
});

test('the gate is the second Skill hook: after the understand-anything guard, before the exec policy', async () => {
  await withPacks(async ({ pack }) => {
    const { options } = buildOptions({ COLONIZER_PLUGIN_DIRS: `${pack('archify')},understand-anything` });
    const entries = options.hooks.PreToolUse;
    assert.equal(entries[0].matcher, undefined, 'the delegation gate stays first');
    const skillAt = entries.map((entry) => entry.matcher).indexOf('Skill');
    assert.ok(skillAt > 0);
    assert.equal(entries[skillAt + 1].matcher, 'Skill', 'the ant-skillsets gate follows the understand-anything one');
    // The first Skill hook refuses `understand`; the second — the ant-skillsets gate — lets it pass.
    assert.equal((await entries[skillAt].hooks[0](SKILL('understand'))).hookSpecificOutput.permissionDecision, 'deny');
    assert.deepEqual(await entries[skillAt + 1].hooks[0](SKILL('understand')), { continue: true });
    assert.ok(entries.findIndex((entry) => entry.matcher === 'Bash') > skillAt + 1, 'the exec policy’s Bash gate comes after');
  });
});

test('a broken pack surfaces its warning through buildOptions without failing the boot', () => {
  withPacks(({ pack }) => {
    const { warnings } = buildOptions({ COLONIZER_PLUGIN_DIRS: pack('broken-pack') });
    assert.equal(warnings.length, 1);
    assert.match(warnings[0], /broken-pack\/agents\/bad\.md/);
  });
});

test('with no packs mounted there is no Skill gate at all', () => {
  assert.ok(!buildOptions({}).options.hooks.PreToolUse.some((entry) => entry.matcher === 'Skill'));
});
