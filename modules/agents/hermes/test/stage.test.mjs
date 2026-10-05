// First-boot staging of hermes-agent (stage.mjs, issue #602): the shipped locks are consistent, the
// right rows are picked per architecture, every hash mismatch refuses before anything runs, a
// finished build is reused, and the runner resolves the staged binary. No network, no real uv: the
// downloads and the commands are fakes that record what was asked of them.

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { resolveHermes } from '../runner.mjs';
import { archPlatform, MARKER, parseLock, readLocks, selectRows, stageHermes, unhashedRequirements } from '../stage.mjs';

const moduleDir = join(dirname(fileURLToPath(import.meta.url)), '..');
const sha = (data) => createHash('sha256').update(data).digest('hex');
const REV = 'f97608f178d1ffeca59860195ab7da295f7c8e5f';

test('the shipped hermes.lock parses, pins the module.json commit and the requirements file it ships beside', () => {
  const locks = readLocks({ dir: moduleDir });
  const rows = parseLock(locks.lockText);
  assert.equal(rows.length, 6);
  assert.equal(locks.pin.source_rev, REV);
  assert.equal(locks.pin.tag, 'v2026.9.24');
  for (const platform of ['linux-x64', 'linux-arm64']) {
    const picked = selectRows(rows, platform, locks.pin);
    assert.ok(picked.source.url.endsWith(REV), picked.source.url);
    assert.equal(picked.requirements.sha256, sha(locks.requirementsText), 'regenerate both with scripts/pin-hermes.mjs');
    assert.equal(picked.uv.platform, platform);
    assert.equal(picked.python.platform, platform);
  }
  assert.match(selectRows(rows, 'linux-x64').uv.url, /uv-x86_64-unknown-linux-gnu\.tar\.gz$/);
  assert.match(selectRows(rows, 'linux-arm64').uv.url, /uv-aarch64-unknown-linux-gnu\.tar\.gz$/);
  assert.match(selectRows(rows, 'linux-arm64').python.url, /aarch64-unknown-linux-gnu-install_only_stripped/);
  assert.deepEqual(unhashedRequirements(locks.requirementsText), [], 'every requirement carries a hash');
  for (const name of ['mcp', 'setuptools', 'wheel', 'pydantic-core', 'openai']) {
    assert.match(locks.requirementsText, new RegExp(`^${name}==`, 'm'), `${name} is pinned`);
  }
});

test('module.json marks hermes runner-fetched and lets the colony reach every host the locks name', () => {
  const manifest = JSON.parse(readFileSync(join(moduleDir, 'module.json'), 'utf8'));
  assert.deepEqual(manifest.requires.fetched_by_runner, ['hermes'], 'the harness must not refuse a stock-image launch for a binary the runner builds');
  const hosts = new Set(parseLock(readFileSync(join(moduleDir, 'hermes.lock'), 'utf8')).filter((r) => r.url.startsWith('https://')).map((r) => new URL(r.url).host));
  hosts.add('files.pythonhosted.org');
  for (const host of hosts) assert.ok(manifest.egress.extra.includes(host), `${host} missing from egress.extra`);
});

test('an unknown architecture, a missing row, or a source row off the pinned commit is refused', () => {
  const rows = parseLock(readFileSync(join(moduleDir, 'hermes.lock'), 'utf8'));
  assert.equal(archPlatform('riscv64'), null);
  assert.throws(() => selectRows(rows, archPlatform('riscv64')), /only linux-x64 and linux-arm64/);
  assert.throws(() => selectRows(rows.filter((r) => r.kind !== 'python'), 'linux-x64'), /0 python rows for platform linux-x64/);
  assert.throws(() => selectRows(rows, 'linux-x64', { source_rev: 'deadbeef' }), /does not name the pinned commit deadbeef/);
});

test('unhashedRequirements names requirements without a hash, wherever the hashes sit', () => {
  const text = 'a==1 \\\n    --hash=sha256:' + 'a'.repeat(64) + '\n    # via x\nb==2\nc==3 --hash=sha256:' + 'c'.repeat(64) + '\n';
  assert.deepEqual(unhashedRequirements(text), ['b']);
});

/** A fake world: a lock over fake artifacts, a fetch that serves them, and a `run` that plays tar
 * and uv by creating what they would create. `tamper` swaps the bytes served for one URL. */
function world({ imagePython = true, tamper = null, failInstall = null } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'hermes-stage-'));
  const bytes = { uv: Buffer.from('uv-x'), src: Buffer.from('src'), py: Buffer.from('py') };
  const requirementsText = `pyyaml==6.0.3 \\\n    --hash=sha256:${'1'.repeat(64)}\n`;
  const requirementsPath = join(dir, 'hermes-requirements.lock');
  writeFileSync(requirementsPath, requirementsText);
  const urls = {
    'https://x.invalid/uv-aarch64-unknown-linux-gnu.tar.gz': bytes.uv,
    [`https://x.invalid/src/${REV}`]: bytes.src,
    'https://x.invalid/cpython-aarch64.tar.gz': bytes.py,
  };
  const lockText = [
    `hermes-agent 0.21.5 any source ${sha(bytes.src)} https://x.invalid/src/${REV}`,
    `hermes-agent 0.21.5 any requirements ${sha(requirementsText)} hermes-requirements.lock`,
    `uv 0.12.23 linux-arm64 tool ${sha(bytes.uv)} https://x.invalid/uv-aarch64-unknown-linux-gnu.tar.gz`,
    `uv 0.12.23 linux-x64 tool ${sha('other')} https://x.invalid/uv-x86_64-unknown-linux-gnu.tar.gz`,
    `python 3.13.16 linux-arm64 python ${sha(bytes.py)} https://x.invalid/cpython-aarch64.tar.gz`,
    `python 3.13.16 linux-x64 python ${sha('other')} https://x.invalid/cpython-x64.tar.gz`,
  ].join('\n');
  const fetched = [];
  const fetchImpl = async (url) => {
    fetched.push(url);
    const body = tamper && url.includes(tamper) ? Buffer.from('tampered') : urls[url];
    return body ? { ok: true, arrayBuffer: async () => body } : { ok: false, status: 404 };
  };
  const ran = [];
  const run = async (cmd, args) => {
    ran.push([cmd, ...args]);
    if (cmd === 'tar') {
      const into = args[args.indexOf('-C') + 1];
      if (args.some((a) => a.endsWith('/uv'))) writeFileSync(join(into, 'uv'), '');
      else if (args[1].endsWith('python.tar.gz')) mkdirSync(join(into, 'python', 'bin'), { recursive: true });
      else writeFileSync(join(into, 'pyproject.toml'), '');
      return { code: 0, stdout: '', stderr: '' };
    }
    if (args[0] === 'venv') mkdirSync(join(args.at(-1), 'bin'), { recursive: true });
    if (args.includes('--require-hashes') && failInstall) return { code: 1, stdout: '', stderr: failInstall };
    if (args.includes('-e')) writeFileSync(join(dirname(args.at(-1)), 'venv', 'bin', 'hermes'), '#!/bin/sh\n');
    return { code: 0, stdout: '', stderr: '' };
  };
  const pythonPath = join(dir, 'python3.11'); // must exist: a build whose interpreter vanished is rebuilt
  writeFileSync(pythonPath, '');
  const findPython = async () => (imagePython ? { path: pythonPath, version: '3.11' } : null);
  const opts = { arch: 'arm64', env: {}, locks: { lockText, requirementsText, requirementsPath, pin: { source_rev: REV } }, fetchImpl, run, findPython, cacheDir: join(dir, 'cache') };
  return { dir, opts, fetched, ran, pythonPath };
}

test('stageHermes verifies, builds with hashes required and wheels only, and a second boot reuses the build', async () => {
  const w = world();
  const first = await stageHermes(w.opts);
  assert.equal(first.cached, false);
  assert.ok(first.bin.endsWith('/venv/bin/hermes'));
  assert.ok(existsSync(join(first.dir, MARKER)));
  assert.deepEqual(w.fetched, ['https://x.invalid/uv-aarch64-unknown-linux-gnu.tar.gz', `https://x.invalid/src/${REV}`], 'the arm64 uv, the source; no CPython when the image has one');
  const uv = w.ran.filter(([cmd]) => cmd.endsWith('/uv/uv')).map(([, ...args]) => args);
  assert.deepEqual(uv[0].slice(0, 3), ['venv', '--python', w.pythonPath]);
  for (const flag of ['--require-hashes', '--only-binary', ':all:', '-r', w.opts.locks.requirementsPath]) assert.ok(uv[1].includes(flag), `${flag} in ${uv[1]}`);
  for (const flag of ['--no-deps', '--no-build-isolation', '--no-index', '-e']) assert.ok(uv[2].includes(flag), `${flag} in ${uv[2]}`);

  const before = { fetched: w.fetched.length, ran: w.ran.length };
  const again = await stageHermes(w.opts);
  assert.equal(again.cached, true);
  assert.equal(again.bin, first.bin);
  assert.deepEqual({ fetched: w.fetched.length, ran: w.ran.length }, before, 'a cached build downloads and runs nothing');
  // The image's interpreter gone (another image under the same cache): the venv is dead, so rebuild.
  rmSync(w.pythonPath);
  assert.equal((await stageHermes(w.opts)).cached, false);
});

test('an image without Python 3.11–3.13 gets the pinned CPython, hash-checked', async () => {
  const w = world({ imagePython: false });
  const staged = await stageHermes(w.opts);
  assert.ok(w.fetched.includes('https://x.invalid/cpython-aarch64.tar.gz'));
  const venv = w.ran.find(([, arg]) => arg === 'venv');
  assert.equal(venv[3], join(staged.dir, 'python', 'bin', 'python3'));
});

for (const [what, tamper, imagePython] of [
  ['uv', 'uv-aarch64', true],
  ['the source tarball', '/src/', true],
  ['the pinned CPython', 'cpython', false],
]) {
  test(`a sha256 mismatch on ${what} refuses, runs nothing it downloaded, and leaves no marker`, async () => {
    const w = world({ tamper, imagePython });
    await assert.rejects(stageHermes(w.opts), /Hermes staging refused: sha256 mismatch for/);
    assert.ok(!w.ran.some(([cmd]) => cmd.endsWith('/uv/uv')), 'uv never ran');
    const tarred = w.ran.filter(([cmd]) => cmd === 'tar').map((c) => c.join(' '));
    assert.ok(!tarred.some((c) => c.includes(tamper === '/src/' ? 'source.tar.gz' : tamper === 'cpython' ? 'python.tar.gz' : 'uv.tar.gz')), `the bad archive was never extracted: ${tarred}`);
    assert.ok(!readdirSync(w.opts.cacheDir, { recursive: true }).some((f) => String(f).endsWith(MARKER)), 'no marker anywhere in the cache');
  });
}

test('a requirement that fails its hash check refuses, and the next boot rebuilds instead of trusting it', async () => {
  // uv's real output shape (0.12): the cause sits above eight lines of expected and computed hashes.
  const failInstall = ['Resolved 1 package in 0.62ms', 'error: Failed to download `pyyaml==6.0.3`', '  cause: Hash mismatch for `pyyaml==6.0.3`', '', '         Expected:', '           sha256:1111', '           sha256:3333', '', '         Computed:', '           sha256:2222'].join('\n');
  const w = world({ failInstall });
  await assert.rejects(stageHermes(w.opts), /a dependency failed its hash check against hermes-requirements\.lock[\s\S]*Hash mismatch for `pyyaml==6\.0\.3`/);
  const fetchedOnce = w.fetched.length;
  await assert.rejects(stageHermes(w.opts), /hash check/);
  assert.equal(w.fetched.length, fetchedOnce * 2, 'no marker, so the second boot starts over');
});

test('a requirements file that differs from the pinned sha256, or lists an unhashed requirement, is refused before any download', async () => {
  const w = world();
  await assert.rejects(stageHermes({ ...w.opts, locks: { ...w.opts.locks, requirementsText: `${w.opts.locks.requirementsText}evil==1\n` } }), /hermes-requirements\.lock has sha256 .*, hermes\.lock pins/);
  const unhashed = 'evil==1\n';
  const lockText = w.opts.locks.lockText.replace(/requirements \w+/, `requirements ${sha(unhashed)}`);
  await assert.rejects(stageHermes({ ...w.opts, locks: { ...w.opts.locks, lockText, requirementsText: unhashed } }), /without a hash: evil/);
  assert.equal(w.fetched.length, 0);
});

test('resolveHermes takes COLONIZER_HERMES_BIN, then PATH, then stages — so a stock image is no longer refused', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hermes-resolve-'));
  let staged = 0;
  const stage = async () => {
    staged++;
    return { bin: '/cache/venv/bin/hermes', cached: false };
  };
  assert.deepEqual((await resolveHermes({ env: { COLONIZER_HERMES_BIN: 'node stub.mjs' }, stage })).bin, ['node', 'stub.mjs']);
  mkdirSync(join(dir, 'bin'));
  writeFileSync(join(dir, 'bin', 'hermes'), '');
  assert.deepEqual((await resolveHermes({ env: { PATH: join(dir, 'bin') }, stage })).bin, [join(dir, 'bin', 'hermes')]);
  assert.equal(staged, 0);
  const fromStage = await resolveHermes({ env: { PATH: '/nonexistent', COLONIZER_HERMES_BIN: '  ' }, stage });
  assert.deepEqual(fromStage, { bin: ['/cache/venv/bin/hermes'], source: 'staged', venvBin: '/cache/venv/bin' });
  await assert.rejects(resolveHermes({ env: { PATH: '' }, stage: async () => { throw new Error('Hermes staging refused: sha256 mismatch for uv'); } }), /sha256 mismatch/);
});
