import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { EXPLORE_DISALLOWED, loadCrew, parseAgentMd, subagentDefinitions } from '../subagents.mjs';

test('without an effort the built-ins stay in place and the always-on crew ships', () => {
  const definitions = subagentDefinitions();
  assert.deepEqual(Object.keys(definitions), ['mellie', 'sarge', 'silka', 'repo-explorer']);
  for (const agent of Object.values(definitions)) {
    assert.equal(agent.effort, undefined);
  }
});

test('with an effort every agent ships and carries it, and none names a model', () => {
  const definitions = subagentDefinitions('medium');
  assert.deepEqual(Object.keys(definitions), ['mellie', 'general-purpose', 'sarge', 'Explore', 'silka', 'repo-explorer']);
  for (const [name, agent] of Object.entries(definitions)) {
    assert.equal(agent.effort, 'medium', name);
    assert.ok(agent.description && agent.prompt, name);
    // No model on any definition, so the subagent model setting still decides it.
    assert.equal(agent.model, undefined, name);
    for (const key of Object.keys(agent)) {
      assert.ok(['description', 'prompt', 'effort', 'tools', 'disallowedTools', 'model'].includes(key), `${name}: ${key}`);
    }
  }
});

test('the queen is the main thread and is never an SDK subagent', () => {
  for (const definitions of [subagentDefinitions(), subagentDefinitions('high')]) {
    assert.ok(!('queen' in definitions));
  }
  const queen = loadCrew().find((agent) => agent.ant?.caste === 'queen');
  assert.equal(queen.name, 'queen');
});

test('Explore is as read-only as its deny list, wherever the list is read from', () => {
  assert.deepEqual(subagentDefinitions('medium').Explore.disallowedTools, EXPLORE_DISALLOWED);
  assert.deepEqual(subagentDefinitions()['repo-explorer'].disallowedTools, EXPLORE_DISALLOWED);
  // Task and Agent are the orchestrator's delegation machinery; Edit and Write are the point.
  for (const tool of ['Agent', 'Task', 'Edit', 'Write']) assert.ok(EXPLORE_DISALLOWED.includes(tool), tool);
});

test('the redefined built-ins stay byte-identical to the pinned snapshot', () => {
  const snapshot = JSON.parse(readFileSync(new URL('../../../../vendor/claude-code-builtins.json', import.meta.url), 'utf8'));
  const definitions = subagentDefinitions('medium');
  for (const agent of ['general-purpose', 'Explore']) {
    assert.equal(definitions[agent].description, snapshot[agent].description, `${agent} description`);
    assert.equal(definitions[agent].prompt, snapshot[agent].prompt.join('\n'), `${agent} prompt`);
  }
  for (const tool of snapshot.Explore.disallowedTools) {
    assert.ok(definitions.Explore.disallowedTools.includes(tool), tool);
  }
});

test('repo-explorer tries a shipped retrieval skill before find/grep', () => {
  const { prompt } = subagentDefinitions()['repo-explorer'];
  assert.match(prompt, /\bSkill\b/, 'the prompt names the Skill tool');
  assert.match(prompt, /\bgraft\b/, 'the prompt names graft as the example skill');
  assert.match(prompt, /find\/grep/, 'the prompt names the fallback');
});

test('parseAgentMd reads the YAML subset the crew pack sticks to', () => {
  const agent = parseAgentMd([
    '---',
    'name: tester',
    'description: "Quoted, with a colon: inside"',
    'tools: [Read, "Grep", Bash]',
    'disallowedTools: [Write]',
    'model: inherit',
    'effort: low',
    'skillsets: [archify]',
    'ant:',
    '  display_name: Tester',
    '  caste: worker',
    '  title: Test ant',
    '  colors: { body: "#112233", dark: "#445566", accent: "#778899" }',
    '  move: mandibles',
    '---',
    'Body line one.',
    '',
    'Body line two.',
    '',
  ].join('\n'), 'tester.md');
  assert.deepEqual(agent, {
    file: 'tester.md',
    name: 'tester',
    description: 'Quoted, with a colon: inside',
    tools: ['Read', 'Grep', 'Bash'],
    disallowedTools: ['Write'],
    model: 'inherit',
    effort: 'low',
    skillsets: ['archify'],
    ant: {
      display_name: 'Tester',
      caste: 'worker',
      title: 'Test ant',
      colors: { body: '#112233', dark: '#445566', accent: '#778899' },
      move: 'mandibles',
    },
    body: 'Body line one.\n\nBody line two.',
  });
});

test('parseAgentMd leaves what a file does not set unset', () => {
  const agent = parseAgentMd('---\nname: a\ndescription: b\n---\nBody.\n', 'a.md');
  assert.deepEqual(agent, { file: 'a.md', name: 'a', description: 'b', body: 'Body.' });
});

test('a bare scalar keeps embedded quotes, and the body starts under the closing line', () => {
  const agent = parseAgentMd('---\nname: a\ndescription: say "hello" loudly\n---\nfirst\n\nsecond\n', 'a.md');
  assert.equal(agent.description, 'say "hello" loudly');
  assert.equal(agent.body, 'first\n\nsecond');
});

test('parseAgentMd fails loudly, naming the file and the problem', () => {
  const bad = (text, file = 'broken.md') => () => parseAgentMd(text, file);
  const file = (name, description, rest) => `---\nname: ${name}\ndescription: ${description}\n${rest}\n---\nBody.\n`;
  assert.throws(bad('no frontmatter at all'), /broken\.md: does not open with a `---` frontmatter line/);
  assert.throws(bad('---\nname: a\ndescription: d\n'), /broken\.md: the frontmatter is never closed with a `---` line/);
  assert.throws(bad('---\nname: a\ndescription: d\n---\n'), /broken\.md: has no body/);
  assert.throws(bad('---\ndescription: d\n---\nBody.\n'), /broken\.md: has no `name:` in the frontmatter/);
  assert.throws(bad(file('../escape', 'd', '')), /is not a plain directory name/);
  assert.throws(bad('---\nname: a\n---\nBody.\n'), /broken\.md: has no `description:` in the frontmatter/);
  assert.throws(bad(file('a', '""', '')), /`description` must be a non-empty string/);
  assert.throws(bad(file('a', 'd', 'effort: extreme')), /`effort: extreme` is not one of low, medium, high/);
  assert.throws(bad(file('a', 'd', 'tools: [Read, ""]')), /inline array \[Read, ""\] has an empty item/);
  assert.throws(bad(file('a', 'd', 'skillsets: [../nope]')), /is not a plain name/);
  // The ant block: caste is an enum, display_name is required, colors are pinned.
  assert.throws(bad(file('a', 'd', 'ant:\n  display_name: X\n  caste: general')), /`ant\.caste: general` is not one of/);
  assert.throws(bad(file('a', 'd', 'ant:\n  caste: soldier')), /`ant\.display_name` must be a non-empty string/);
  assert.throws(bad(file('a', 'd', 'ant:\n  display_name: X\n  caste: soldier\n  colors: { body: red, dark: "#8a2a1a", accent: "#f2c14e" }')), /`ant\.colors\.body: red` is not a "#rrggbb" hex color/);
  assert.throws(bad(file('a', 'd', 'ant:\n  display_name: X\n  caste: soldier\n  colors: { body: "#112233", dark: "#445566" }')), /`ant.colors` must have exactly the keys body, dark and accent/);
  assert.throws(bad(file('a', 'd', 'ant: soldier')), /`ant:` is a nested block/);
  assert.throws(bad(file('a', 'd', 'other:\n  nested: value')), /indented line outside a nested block/);
  assert.throws(bad(file('a', 'd', 'ant:\n   three: spaces')), /exactly two spaces/);
  // The ant block is Colonizer's, so a typo'd key is an error; unknown top-level keys are allowed
  // (Claude Code grows fields of its own) and ignored.
  assert.throws(bad(file('a', 'd', 'ant:\n  display_name: X\n  caste: soldier\n  colur: {}')), /unknown `ant:` key "colur"/);
  const extra = parseAgentMd('---\nname: a\ndescription: d\nnew-field: whatever\n---\nBody.\n', 'a.md');
  assert.equal(extra.description, 'd');
});

test('loadCrew reads the pack sorted by filename, and fails loudly on a bad one', () => {
  assert.deepEqual(loadCrew().map((agent) => agent.file), ['mellie.md', 'pip.md', 'queen.md', 'sarge.md', 'scout.md', 'silka.md']);
  assert.throws(() => loadCrew(new URL('file:///nonexistent/crew/agents/')), /cannot read the crew's agents directory/);
  assert.throws(() => loadCrew(mkdtempSync(join(tmpdir(), 'empty-crew-'))), /no agent files in the crew pack/);
});

test('loadCrew refuses two files answering to one name, case-insensitively', () => {
  const dir = mkdtempSync(join(tmpdir(), 'dupe-crew-'));
  writeFileSync(join(dir, 'a.md'), '---\nname: sarge\ndescription: d\n---\nBody.\n');
  writeFileSync(join(dir, 'b.md'), '---\nname: Sarge\ndescription: d\n---\nBody.\n');
  assert.throws(() => loadCrew(dir), /agent "Sarge" is defined by both a\.md and b\.md: agent names must be unique/);
});
