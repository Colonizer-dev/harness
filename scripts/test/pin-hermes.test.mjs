// scripts/pin-hermes.mjs with the network and every command mocked: the right commit, hashes that
// are the downloads' own and agree with what each publisher lists, the build backend appended to the
// requirements, and nothing written when any check fails.

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { buildRequires, commitFromLsRemote, currentPins, hostUvTriple, pinHermes, publishedSha, requirementBlocks } from '../pin-hermes.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const sha = (data) => createHash('sha256').update(data).digest('hex');
const COMMIT = 'a'.repeat(40);
const HASH = (c) => `--hash=sha256:${c.repeat(64)}`;

test('commitFromLsRemote peels an annotated tag and takes a lightweight one as is', () => {
  const annotated = `${'e'.repeat(40)}\trefs/tags/v1\n${'f'.repeat(40)}\trefs/tags/v1^{}\n`;
  assert.equal(commitFromLsRemote(annotated, 'v1'), 'f'.repeat(40));
  assert.equal(commitFromLsRemote(`${'e'.repeat(40)}\trefs/tags/v1\n`, 'v1'), 'e'.repeat(40));
  assert.throws(() => commitFromLsRemote('', 'v9'), /no tag v9/);
});

test('the helpers read the shipped lock, checksum files, pyproject and requirement blocks', () => {
  const pins = currentPins(readFileSync(join(root, 'modules/agents/hermes/hermes.lock'), 'utf8'));
  assert.match(pins.uv, /^\d+\.\d+\.\d+$/);
  assert.match(pins.python, /^3\.1[1-3]\.\d+\+\d{8}$/);
  assert.equal(publishedSha(`${'b'.repeat(64)}  x.tar.gz\n${'c'.repeat(64)} *y.tar.gz\n`, 'y.tar.gz'), 'c'.repeat(64));
  assert.equal(publishedSha('', 'y'), null);
  assert.deepEqual(buildRequires('[build-system]\nrequires = ["setuptools==83.0.0", "wheel"]\n'), ['setuptools==83.0.0', 'wheel']);
  assert.deepEqual([...requirementBlocks(`Ruamel.Yaml==1 \\\n    ${HASH('1')}\nb==2\n`).keys()], ['ruamel-yaml', 'b']);
  assert.equal(hostUvTriple('darwin', 'arm64'), 'aarch64-apple-darwin');
  assert.equal(hostUvTriple('linux', 'x64'), 'x86_64-unknown-linux-gnu');
});

/** A module dir copy, a fake network, and a fake `run` for git, tar and uv. */
function world({ badUvSum = false, dryRunFails = false } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'pin-hermes-'));
  const mod = join(dir, 'hermes');
  mkdirSync(mod);
  for (const f of ['hermes.lock', 'hermes-requirements.lock', 'module.json']) cpSync(join(root, 'modules/agents/hermes', f), join(mod, f));
  const host = hostUvTriple();
  const files = new Map();
  files.set(`https://codeload.github.com/NousResearch/hermes-agent/tar.gz/${COMMIT}`, Buffer.from('source'));
  const sums = [];
  for (const triple of ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', host]) {
    const url = `https://github.com/astral-sh/uv/releases/download/9.9.9/uv-${triple}.tar.gz`;
    const body = Buffer.from(`uv ${triple}`);
    files.set(url, body);
    files.set(`${url}.sha256`, Buffer.from(`${badUvSum && triple.startsWith('x86_64') ? '0'.repeat(64) : sha(body)}  uv-${triple}.tar.gz\n`));
  }
  for (const triple of ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu']) {
    const name = `cpython-3.13.99+20990101-${triple}-install_only_stripped.tar.gz`;
    const body = Buffer.from(name);
    files.set(`https://github.com/astral-sh/python-build-standalone/releases/download/20990101/${name.replace('+', '%2B')}`, body);
    sums.push(`${sha(body)}  ${name}`);
  }
  files.set('https://github.com/astral-sh/python-build-standalone/releases/download/20990101/SHA256SUMS', Buffer.from(`${sums.join('\n')}\n`));
  const fetched = [];
  const fetchImpl = async (url) => {
    fetched.push(url);
    const body = files.get(url);
    return body ? { ok: true, arrayBuffer: async () => body } : { ok: false, status: 404 };
  };
  const ran = [];
  const run = async (cmd, args, opts = {}) => {
    ran.push([cmd, ...args]);
    if (cmd === 'git') return { code: 0, stdout: `${'e'.repeat(40)}\trefs/tags/v9\n${COMMIT}\trefs/tags/v9^{}\n`, stderr: '' };
    if (cmd === 'tar') {
      const into = args[args.indexOf('-C') + 1];
      if (args[1].endsWith('uv-host.tar.gz')) writeFileSync(join(into, 'uv'), '');
      else writeFileSync(join(into, 'pyproject.toml'), '[project]\nname = "hermes-agent"\nversion = "9.9.9"\n[build-system]\nrequires = ["setuptools==83.0.0", "wheel"]\n');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (args[0] === 'export') {
      assert.ok(opts.cwd.endsWith('/src'), 'exported from the source tree, against its own uv.lock');
      return { code: 0, stdout: `mcp==2.0.0 \\\n    ${HASH('1')}\npackaging==26.0 \\\n    ${HASH('2')}\n`, stderr: '' };
    }
    if (args[1] === 'compile') return { code: 0, stdout: `packaging==26.0 \\\n    ${HASH('2')}\nsetuptools==83.0.0 \\\n    ${HASH('3')}\nwheel==0.48.0 \\\n    ${HASH('4')}\n`, stderr: '' };
    if (args.includes('--dry-run')) return dryRunFails && args.includes('3.12') ? { code: 1, stdout: '', stderr: 'no wheel for foo on cp312' } : { code: 0, stdout: '', stderr: '' };
    return { code: 1, stdout: '', stderr: `unexpected ${cmd} ${args.join(' ')}` };
  };
  const snapshot = () => ['hermes.lock', 'hermes-requirements.lock', 'module.json'].map((f) => readFileSync(join(mod, f), 'utf8'));
  return { mod, opts: { tag: 'v9', dir: mod, uvVersion: '9.9.9', python: '3.13.99+20990101', fetchImpl, run, work: mkdtempSync(join(tmpdir(), 'pin-hermes-work-')), log: () => {} }, fetched, ran, snapshot };
}

test('pinHermes writes the commit, the downloads’ own hashes and the hashed requirements', async () => {
  const w = world();
  const out = await pinHermes(w.opts);
  assert.equal(out.commit, COMMIT);
  const lock = readFileSync(join(w.mod, 'hermes.lock'), 'utf8');
  assert.ok(lock.startsWith('# hermes-agent pinned by its source commit'), 'the header comment is kept');
  const rows = lock.split('\n').filter((l) => l && !l.startsWith('#')).map((l) => l.split(/\s+/));
  const requirements = readFileSync(join(w.mod, 'hermes-requirements.lock'), 'utf8');
  assert.deepEqual(rows.map((r) => [r[0], r[1], r[2], r[3]]), [
    ['hermes-agent', '9.9.9', 'any', 'source'],
    ['hermes-agent', '9.9.9', 'any', 'requirements'],
    ['uv', '9.9.9', 'linux-x64', 'tool'],
    ['uv', '9.9.9', 'linux-arm64', 'tool'],
    ['python', '3.13.99+20990101', 'linux-x64', 'python'],
    ['python', '3.13.99+20990101', 'linux-arm64', 'python'],
  ]);
  assert.equal(rows[0][4], sha('source'));
  assert.ok(rows[0][5].endsWith(COMMIT));
  assert.equal(rows[1][4], sha(requirements));
  assert.equal(rows[2][4], sha('uv x86_64-unknown-linux-gnu'));
  assert.match(requirements, /^mcp==2\.0\.0/m);
  assert.match(requirements, /^setuptools==83\.0\.0/m);
  assert.match(requirements, /^wheel==0\.48\.0/m);
  assert.equal(requirements.match(/^packaging==/gm).length, 1, 'a build requirement already in the export is not repeated');
  const manifest = JSON.parse(readFileSync(join(w.mod, 'module.json'), 'utf8'));
  assert.deepEqual(manifest.requires.pins.hermes, { version: '9.9.9', tag: 'v9', source_rev: COMMIT });
  assert.equal(w.ran.filter(([, a]) => a === 'pip').filter((c) => c.includes('--dry-run')).length, 6, 'three Pythons × two architectures');
});

test('a uv download that disagrees with its published .sha256 writes nothing', async () => {
  const w = world({ badUvSum: true });
  const before = w.snapshot();
  await assert.rejects(pinHermes(w.opts), /uv 9\.9\.9 x86_64-unknown-linux-gnu: the download hashes to/);
  assert.deepEqual(w.snapshot(), before);
});

test('a lock that fails the wheels-only dry run on any interpreter writes nothing', async () => {
  const w = world({ dryRunFails: true });
  const before = w.snapshot();
  await assert.rejects(pinHermes(w.opts), /dry run for Python 3\.12/);
  assert.deepEqual(w.snapshot(), before);
});
