#!/usr/bin/env node
// Pins the Hermes agent module to an upstream hermes-agent tag (issue #602). It regenerates
// modules/agents/hermes/hermes.lock, hermes-requirements.lock, and the module.json pin, the files
// the runner's first-boot staging (modules/agents/hermes/stage.mjs) reads:
//
//   node scripts/pin-hermes.mjs v2026.9.24                          re-pin, keeping the uv and Python pins
//   node scripts/pin-hermes.mjs v2026.9.24 --uv 0.12.23             also move uv
//   node scripts/pin-hermes.mjs v2026.9.24 --python 3.13.16+20261003  also move the fallback CPython
//   node scripts/pin-hermes.mjs v2026.9.24 --dir path               write another module dir (tests)
//
// What it does, and where each hash comes from:
// 1. Resolves the tag to its exact commit with `git ls-remote` (peeling an annotated tag).
// 2. Downloads the GitHub tarball of that commit and computes its sha256. GitHub publishes no
//    checksum for these, so the hash is this download's.
// 3. Downloads the linux x64 and arm64 uv archives and computes their sha256, refusing any that
//    differs from the `.sha256` file uv's release publishes beside it.
// 4. Downloads the linux x64 and arm64 python-build-standalone CPython builds and computes their
//    sha256, refusing any that differs from the release's SHA256SUMS.
// 5. Runs the pinned uv (the build for this machine, checked the same way) on the source tree:
//    `uv export --frozen` turns upstream's own uv.lock into requirements with every PyPI hash (the
//    `mcp` extra, which Hermes needs to load MCP servers, and no dev dependencies), and
//    `uv pip compile --generate-hashes` adds the build backend pyproject names (setuptools, wheel)
//    so the source tree can be installed without build isolation fetching anything unhashed.
// 6. Proves the result: a wheels-only `uv pip install --dry-run --require-hashes` for each of
//    Python 3.11, 3.12 and 3.13 on linux x86_64 and aarch64. Any failure writes nothing.
// Nothing is committed or pushed: review the diff and open a pull request, as with every pin.

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
export const REPO = 'https://github.com/NousResearch/hermes-agent';
export const UV_RELEASES = 'https://github.com/astral-sh/uv/releases/download';
export const PBS_RELEASES = 'https://github.com/astral-sh/python-build-standalone/releases/download';
const TRIPLES = { 'linux-x64': 'x86_64-unknown-linux-gnu', 'linux-arm64': 'aarch64-unknown-linux-gnu' };
const SHA256 = /^[0-9a-f]{64}$/;
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

/** `cmd args` → { code, stdout, stderr }. */
export function execRun(cmd, args, opts = {}) {
  return new Promise((resolve) => {
    execFile(cmd, args, { maxBuffer: 64 * 1024 * 1024, ...opts }, (error, stdout, stderr) =>
      resolve({ code: error ? (typeof error.code === 'number' ? error.code : 1) : 0, stdout: String(stdout ?? ''), stderr: String(stderr ?? '') || String(error?.message ?? '') }),
    );
  });
}

/** The commit a tag names: the peeled `^{}` line for an annotated tag, else the tag line itself. */
export function commitFromLsRemote(text, tag) {
  const lines = String(text).trim().split('\n').filter(Boolean).map((l) => l.split(/\s+/));
  const peeled = lines.find(([, ref]) => ref === `refs/tags/${tag}^{}`);
  const plain = lines.find(([, ref]) => ref === `refs/tags/${tag}`);
  const sha = (peeled ?? plain)?.[0];
  if (!sha || !/^[0-9a-f]{40}$/.test(sha)) throw new Error(`git ls-remote found no tag ${tag} in ${REPO}`);
  return sha;
}

/** The current pins in a hermes.lock, so a re-pin keeps uv and Python unless told otherwise. */
export function currentPins(lockText) {
  const rows = String(lockText ?? '').split('\n').filter((l) => l.trim() && !l.startsWith('#')).map((l) => l.trim().split(/\s+/));
  const kind = (k) => rows.find((r) => r[3] === k);
  return { uv: kind('tool')?.[1], python: kind('python')?.[1] };
}

/** The checksum a publisher lists for `name` in a SHA256SUMS-style text (`<sha>  <name>`). */
export function publishedSha(text, name) {
  for (const line of String(text).split('\n')) {
    const [sha, file] = line.trim().split(/\s+/);
    if (file && file.replace(/^\*/, '') === name && SHA256.test(sha)) return sha;
  }
  return null;
}

/** The uv archive name for the machine running this script. */
export function hostUvTriple(platform = process.platform, arch = process.arch) {
  const cpu = arch === 'arm64' ? 'aarch64' : arch === 'x64' ? 'x86_64' : null;
  if (!cpu) return null;
  if (platform === 'darwin') return `${cpu}-apple-darwin`;
  if (platform === 'linux') return `${cpu}-unknown-linux-gnu`;
  return null;
}

/** The setuptools pin from pyproject's [build-system] requires (wheel is taken as resolved). */
export function buildRequires(pyproject) {
  const block = /\[build-system\][\s\S]*?requires\s*=\s*\[([^\]]*)\]/.exec(String(pyproject));
  if (!block) throw new Error('pyproject.toml has no [build-system] requires');
  return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/** The blocks of a hashed requirements text, keyed by normalized name. */
export function requirementBlocks(text) {
  const blocks = new Map();
  let name = null;
  for (const line of String(text).split('\n')) {
    if (/^[A-Za-z0-9]/.test(line)) {
      name = line.split(/[\s=<>!~;[]/)[0].toLowerCase().replace(/[-_.]+/g, '-');
      blocks.set(name, [line]);
    } else if (name && /^\s/.test(line)) blocks.get(name).push(line);
    else name = null;
  }
  return blocks;
}

/** Everything a pin needs, written to `dir` only after every check has passed. */
export async function pinHermes({ tag, dir = join(root, 'modules/agents/hermes'), uvVersion, python, fetchImpl = fetch, run = execRun, work = mkdtempSync(join(tmpdir(), 'pin-hermes-')), log = (m) => console.error(m) } = {}) {
  if (!tag) throw new Error('usage: node scripts/pin-hermes.mjs <tag> [--uv <version>] [--python <version>+<release>]');
  const lockPath = join(dir, 'hermes.lock');
  let existing = '';
  try {
    existing = readFileSync(lockPath, 'utf8');
  } catch {
    // A first pin: --uv and --python are then required.
  }
  const was = currentPins(existing);
  uvVersion ??= was.uv;
  python ??= was.python;
  if (!uvVersion || !python) throw new Error('no uv or Python pin to keep: pass --uv <version> and --python <version>+<release>');
  const [pyVersion, pbsRelease] = python.split('+');
  if (!pyVersion || !pbsRelease) throw new Error(`--python must read <version>+<release>, like 3.13.16+20261003; got ${python}`);

  const download = async (url) => {
    const res = await fetchImpl(url);
    if (!res?.ok) throw new Error(`HTTP ${res?.status ?? 'no response'} from ${url}`);
    return Buffer.from(await res.arrayBuffer());
  };
  const must = async (what, cmd, args, opts) => {
    const out = await run(cmd, args, opts);
    if (out.code !== 0) throw new Error(`${what} failed: ${(out.stderr || out.stdout).trim()}`);
    return out;
  };

  try {
    // 1. The commit.
    const commit = commitFromLsRemote((await must('git ls-remote', 'git', ['ls-remote', REPO, `refs/tags/${tag}`, `refs/tags/${tag}^{}`])).stdout, tag);
    log(`${tag} is commit ${commit}`);

    // 2. The source tarball, and the tree it unpacks to.
    const sourceUrl = `https://codeload.github.com/NousResearch/hermes-agent/tar.gz/${commit}`;
    const source = await download(sourceUrl);
    const sourceSha = sha256(source);
    writeFileSync(join(work, 'source.tar.gz'), source);
    const tree = join(work, 'src');
    mkdirSync(tree, { recursive: true });
    await must('extracting the source', 'tar', ['-xzf', join(work, 'source.tar.gz'), '-C', tree, '--strip-components=1']);
    const pyproject = readFileSync(join(tree, 'pyproject.toml'), 'utf8');
    const version = /^version\s*=\s*"([^"]+)"/m.exec(pyproject)?.[1];
    if (!version) throw new Error('pyproject.toml names no version');

    // 3. uv, per arch, checked against the release's own .sha256 files.
    const uvRows = [];
    const uvAsset = async (triple) => {
      const url = `${UV_RELEASES}/${uvVersion}/uv-${triple}.tar.gz`;
      const bytes = await download(url);
      const sha = sha256(bytes);
      const listed = publishedSha((await download(`${url}.sha256`)).toString('utf8'), `uv-${triple}.tar.gz`);
      if (listed !== sha) throw new Error(`uv ${uvVersion} ${triple}: the download hashes to ${sha}, the release lists ${listed}`);
      return { url, sha, bytes };
    };
    for (const [platform, triple] of Object.entries(TRIPLES)) {
      const { url, sha } = await uvAsset(triple);
      uvRows.push(['uv', uvVersion, platform, 'tool', sha, url]);
    }

    // 4. CPython, per arch, checked against the release's SHA256SUMS.
    const sums = (await download(`${PBS_RELEASES}/${pbsRelease}/SHA256SUMS`)).toString('utf8');
    const pyRows = [];
    for (const [platform, triple] of Object.entries(TRIPLES)) {
      const name = `cpython-${pyVersion}+${pbsRelease}-${triple}-install_only_stripped.tar.gz`;
      const url = `${PBS_RELEASES}/${pbsRelease}/${name.replace('+', '%2B')}`;
      const sha = sha256(await download(url));
      const listed = publishedSha(sums, name);
      if (listed !== sha) throw new Error(`CPython ${pyVersion} ${triple}: the download hashes to ${sha}, SHA256SUMS lists ${listed}`);
      pyRows.push(['python', `${pyVersion}+${pbsRelease}`, platform, 'python', sha, url]);
    }

    // 5. The hashed requirements, from upstream's uv.lock, plus the build backend.
    const hostTriple = hostUvTriple();
    if (!hostTriple) throw new Error(`no uv build for ${process.platform}/${process.arch} to generate the requirements with`);
    const host = await uvAsset(hostTriple);
    writeFileSync(join(work, 'uv-host.tar.gz'), host.bytes);
    mkdirSync(join(work, 'uv'), { recursive: true });
    await must('extracting uv', 'tar', ['-xzf', join(work, 'uv-host.tar.gz'), '-C', join(work, 'uv'), '--strip-components=1', `uv-${hostTriple}/uv`]);
    const uv = join(work, 'uv', 'uv');
    chmodSync(uv, 0o755);
    const uvEnv = { ...process.env, UV_NO_CONFIG: '1', UV_CACHE_DIR: join(work, 'uv-cache'), UV_NO_PROGRESS: '1' };
    const exported = (await must('uv export', uv, ['export', '--frozen', '--format', 'requirements-txt', '--extra', 'mcp', '--no-dev', '--no-emit-project', '--no-header'], { cwd: tree, env: uvEnv })).stdout;
    writeFileSync(join(work, 'export.txt'), exported);
    writeFileSync(join(work, 'build.in'), `${buildRequires(pyproject).join('\n')}\n`);
    const build = (await must('uv pip compile', uv, ['pip', 'compile', join(work, 'build.in'), '--universal', '--generate-hashes', '--python-version', '3.11', '--no-header', '--no-annotate', '-c', join(work, 'export.txt')], { cwd: work, env: uvEnv })).stdout;
    const have = requirementBlocks(exported);
    const extra = [...requirementBlocks(build)].filter(([name]) => !have.has(name)).map(([, lines]) => lines.join('\n'));
    const requirements =
      `# hermes-agent ${version} (${tag}, commit ${commit}) with the mcp extra, every dependency hash-pinned.\n` +
      '# Generated by scripts/pin-hermes.mjs from upstream\'s uv.lock (uv export --frozen), plus the\n' +
      '# build backend pyproject names, so the source installs with --no-build-isolation. Do not edit:\n' +
      '# hermes.lock pins this file\'s sha256, and the runner refuses a copy that differs.\n' +
      `${exported.trimEnd()}\n# The build backend for the source tree.\n${extra.join('\n')}\n`;
    const reqPath = join(work, 'hermes-requirements.lock');
    writeFileSync(reqPath, requirements);

    // 6. Wheels only, hashes required, on every interpreter and architecture a colony may have.
    for (const py of ['3.11', '3.12', '3.13']) {
      for (const platform of ['x86_64-manylinux_2_28', 'aarch64-manylinux_2_28']) {
        await must(`the wheels-only dry run for Python ${py} on ${platform}`, uv, ['pip', 'install', '--dry-run', '--target', join(work, `t-${py}-${platform}`), '--python-platform', platform, '--python-version', py, '--only-binary', ':all:', '--require-hashes', '-r', reqPath], { cwd: work, env: uvEnv });
      }
    }

    // Everything checked: write.
    const header = existing.split('\n').filter((l) => l.startsWith('#')).join('\n');
    const rows = [['hermes-agent', version, 'any', 'source', sourceSha, sourceUrl], ['hermes-agent', version, 'any', 'requirements', sha256(requirements), 'hermes-requirements.lock'], ...uvRows, ...pyRows];
    const widths = rows[0].map((_, i) => Math.max(...rows.map((r) => r[i].length)));
    const body = rows.map((r) => r.map((c, i) => (i === r.length - 1 ? c : c.padEnd(widths[i]))).join('  ')).join('\n');
    writeFileSync(lockPath, `${header ? `${header}\n` : ''}${body}\n`);
    writeFileSync(join(dir, 'hermes-requirements.lock'), requirements);
    const manifestPath = join(dir, 'module.json');
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    manifest.requires ??= {};
    manifest.requires.pins = { ...(manifest.requires.pins ?? {}), hermes: { version, tag, source_rev: commit } };
    writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    log(`pinned hermes-agent ${version} (${commit}), uv ${uvVersion}, CPython ${python}`);
    return { commit, version, sourceSha };
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const args = process.argv.slice(2);
  const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
  pinHermes({ tag: args.find((a, i) => !a.startsWith('--') && !['--uv', '--python', '--dir'].includes(args[i - 1])), uvVersion: option('--uv'), python: option('--python'), dir: option('--dir') ?? undefined }).catch((error) => {
    console.error(`pin-hermes: ${error.message}`);
    process.exit(1);
  });
}
