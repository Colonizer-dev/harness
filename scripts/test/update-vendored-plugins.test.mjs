// update-vendored-plugins.mjs gates a pin rewrite on the new archive validating as the pack
// scripts/fetch-vendor.sh stages from it. These tests drive the exported helper offline: a real
// tgz assembled with tar, no network, no lock file, no GitHub API.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';

import { validateArchivePack } from '../update-vendored-plugins.mjs';

/** A tgz of `files` ({relative path: content}) under one `top/` directory, as codeload ships. */
function archive(t, top, files) {
  const dir = mkdtempSync(join(tmpdir(), 'update-vendored-plugins-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  for (const [rel, content] of Object.entries(files)) {
    const path = join(dir, top, rel);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, content);
  }
  execFileSync('tar', ['-czf', join(dir, 'pack.tgz'), top], { cwd: dir });
  return readFileSync(join(dir, 'pack.tgz'));
}

const manifest = JSON.stringify({ name: 'demo', version: '1.2.3', description: 'Demo pack', skills: ['hello'] });
const skill = '---\nname: hello\ndescription: Says hello.\n---\n\nBody.\n';

test('a pin staged from its archive is validated: a valid pack passes', (t) => {
  const tgz = archive(t, 'demo-1.2.3', { 'plugin.json': manifest, 'skills/hello/SKILL.md': skill });
  const result = validateArchivePack('superpowers', 'plugin', tgz);
  assert.equal(result.ok, true);
  assert.deepEqual(result.errors, []);
});

test('a pin whose new archive breaks a rule is refused with the named error', (t) => {
  const tgz = archive(t, 'demo-2.0.0', {
    '.claude-plugin/plugin.json': manifest,
    'skills/hello/SKILL.md': skill,
    'mcp.json': JSON.stringify({ greeter: { url: 'https://api.example/mcp' } }),
  });
  const result = validateArchivePack('ecc', 'plugin', tgz);
  assert.equal(result.ok, false);
  assert.ok(result.errors.some((e) => e.rule === 'mcp-remote-hosts' && e.file === 'mcp.json'));
});

test('pins fetch-vendor.sh does not stage from the archive are not validated', (t) => {
  // google-skills stages a pack synthesized at stage time; the archive holds no pack.
  const tgz = archive(t, 'skills-abc123', { 'index.json': '{"skills": []}' });
  assert.equal(validateArchivePack('google-skills', 'plugin', tgz), null);
  // caveman stages prompt text, fast-jev-compaction a hook: no pack either.
  assert.equal(validateArchivePack('caveman', 'prompt', tgz), null);
  assert.equal(validateArchivePack('fast-jev-compaction', 'hook', tgz), null);
});
