import assert from 'node:assert/strict';
import { test } from 'node:test';

import { EXPLORE_DISALLOWED, subagentDefinitions } from '../subagents.mjs';

test('without an effort only repo-explorer is redefined, and it carries none', () => {
  const definitions = subagentDefinitions();
  assert.deepEqual(Object.keys(definitions), ['repo-explorer']);
  assert.equal(definitions['repo-explorer'].effort, undefined);
});

test('with an effort all three agents are redefined and carry it', () => {
  const definitions = subagentDefinitions('high');
  assert.deepEqual(Object.keys(definitions), ['general-purpose', 'Explore', 'repo-explorer']);
  for (const agent of ['general-purpose', 'Explore', 'repo-explorer']) {
    assert.equal(definitions[agent].effort, 'high', agent);
  }
});

test('repo-explorer is as read-only as Explore', () => {
  assert.deepEqual(subagentDefinitions()['repo-explorer'].disallowedTools, EXPLORE_DISALLOWED);
});

test('repo-explorer tries a shipped retrieval skill before find/grep', () => {
  const { prompt } = subagentDefinitions()['repo-explorer'];
  assert.match(prompt, /\bSkill\b/, 'the prompt names the Skill tool');
  assert.match(prompt, /\bgraft\b/, 'the prompt names graft as the example skill');
  assert.match(prompt, /find\/grep/, 'the prompt names the fallback');
});
