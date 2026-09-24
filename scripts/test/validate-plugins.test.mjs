import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { validatePack } from '../validate-plugins.mjs';

const rules = (result) => result.errors.map((e) => e.rule);

const manifest = (over = {}) =>
  JSON.stringify({ name: 'demo', version: '1.2.3', description: 'Demo pack', skills: ['hello'], ...over });

const skillMd = (name = 'hello', description = 'Says hello') => `---\nname: ${name}\ndescription: ${description}\n---\n\nBody.\n`;

/** A pack dir from {relative/path: content}; removed after the test. */
function pack(t, files) {
  const dir = mkdtempSync(join(tmpdir(), 'validate-plugins-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  for (const [rel, content] of Object.entries(files)) {
    const parts = rel.split('/');
    if (parts.length > 1) mkdirSync(join(dir, ...parts.slice(0, -1)), { recursive: true });
    writeFileSync(join(dir, ...parts), content);
  }
  return dir;
}

const valid = () => ({
  'plugin.json': manifest(),
  'skills/hello/SKILL.md': skillMd(),
  'mcp.json': JSON.stringify({ mcpServers: { local: { command: 'node', args: ['server.js'] } } }),
});

test('a valid pack passes, with named errors shaped as {file, rule, message}', (t) => {
  const result = validatePack(pack(t, valid()));
  assert.equal(result.ok, true);
  assert.deepEqual(result.errors, []);
});

test('bad manifest JSON fails with manifest-parse', (t) => {
  const result = validatePack(pack(t, { 'plugin.json': '{oops' }));
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('manifest-parse'));
  for (const e of result.errors) {
    assert.equal(typeof e.file, 'string');
    assert.equal(typeof e.rule, 'string');
    assert.equal(typeof e.message, 'string');
  }
});

test('a listed skill without SKILL.md fails with skill-missing', (t) => {
  const result = validatePack(pack(t, { 'plugin.json': manifest() }));
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('skill-missing'));
});

test('duplicate skill names fail case-insensitively', (t) => {
  const dir = pack(t, { 'plugin.json': manifest({ skills: ['hello', 'HELLO'] }), 'skills/hello/SKILL.md': skillMd() });
  const result = validatePack(dir);
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('skill-duplicate'));
});

test('a remote server without hosts fails; one with hosts passes', (t) => {
  const bad = validatePack(pack(t, { ...valid(), 'mcp.json': JSON.stringify({ greeter: { url: 'https://api.example/mcp' } }) }));
  assert.equal(bad.ok, false);
  assert.ok(rules(bad).includes('mcp-remote-hosts'));

  const good = validatePack(
    pack(t, { ...valid(), 'mcp.json': JSON.stringify({ greeter: { url: 'https://api.example/mcp', hosts: ['api.example'] } }) }),
  );
  assert.equal(good.ok, true);
});

test('a legacy directory-style skills entry is accepted, as plugins.rs accepts it', (t) => {
  // The exact shape upstream ecc ships; is_skill_tree in plugins.rs accepts it, so this must too.
  const ecc = validatePack(pack(t, { '.claude-plugin/plugin.json': manifest({ skills: ['./skills/'] }), 'skills/hello/SKILL.md': skillMd() }));
  assert.equal(ecc.ok, true);

  // "skills/" and "." mean the same thing: every skill under that directory.
  for (const entry of ['skills/', '.']) {
    const alt = validatePack(pack(t, { 'plugin.json': manifest({ skills: [entry] }), 'skills/hello/SKILL.md': skillMd() }));
    assert.equal(alt.ok, true, `${JSON.stringify(entry)} is a directory-style entry`);
  }

  // A symlinked skill directory counts, as Rust's is_dir() on the path (not a Dirent's lstat) counts it.
  const linked = pack(t, { '.claude-plugin/plugin.json': manifest({ skills: ['./skills/'] }), 'real/hello/SKILL.md': skillMd() });
  mkdirSync(join(linked, 'skills'));
  symlinkSync(join(linked, 'real/hello'), join(linked, 'skills/linked'));
  assert.equal(validatePack(linked).ok, true);

  // A directory entry with no skill beneath it is not a skill, and one that escapes the pack is
  // refused — plugins.rs rejects both (empty tree, `..`).
  const empty = validatePack(pack(t, { 'plugin.json': manifest({ skills: ['./skills/'] }) }));
  assert.equal(empty.ok, false);
  const escaping = validatePack(pack(t, { ...valid(), 'plugin.json': manifest({ skills: ['hello', '../x'] }) }));
  assert.equal(escaping.ok, false);
  assert.ok(rules(escaping).includes('skill-name'));
});

test('a numeric $schema fails with manifest-schema', (t) => {
  const result = validatePack(pack(t, { ...valid(), 'plugin.json': manifest({ $schema: 2 }) }));
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('manifest-schema'));
});

test('a legacy .claude-plugin/plugin.json pack passes (back-compat)', (t) => {
  const result = validatePack(pack(t, { '.claude-plugin/plugin.json': manifest(), 'skills/hello/SKILL.md': skillMd() }));
  assert.equal(result.ok, true);
});

test('a pack with neither manifest fails with manifest-missing', (t) => {
  const result = validatePack(pack(t, { 'skills/hello/SKILL.md': skillMd() }));
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('manifest-missing'));
});

test('a SKILL.md without frontmatter fails with skill-frontmatter', (t) => {
  const result = validatePack(pack(t, { 'plugin.json': manifest(), 'skills/hello/SKILL.md': 'Just a body, no frontmatter.\n' }));
  assert.equal(result.ok, false);
  assert.ok(rules(result).includes('skill-frontmatter'));
});

test('a manifest that is valid JSON but not an object fails with manifest-shape, not a throw', (t) => {
  for (const body of ['null', '42', '"a string"', '[]']) {
    const result = validatePack(pack(t, { 'plugin.json': body }));
    assert.equal(result.ok, false, `${body} should not validate`);
    assert.ok(rules(result).includes('manifest-shape'), `${body} should be a shape error, got ${rules(result)}`);
  }
});
