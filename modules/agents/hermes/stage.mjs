// First-boot staging of the Hermes Agent CLI (issue #602). Hermes ships no release binaries and
// PyPI stops at 0.19.0, so a colony builds it from the pinned source commit instead of extracting a
// binary. Every byte that runs is checked against a hash in this module:
//
//   hermes.lock               the source tarball of the pinned commit, uv per arch, a CPython per arch,
//                             and the sha256 of hermes-requirements.lock — each sha256-checked here
//   hermes-requirements.lock  every Python dependency, wheels only, each with its PyPI hashes;
//                             uv refuses any wheel whose hash is not listed (--require-hashes)
//
// The build lands in the same cache the other fetched CLIs use (~/.cache/colonizer/<agent>), keyed
// by the commit and a digest of both lock files, and a marker written last makes later boots skip
// it. Any mismatch, or any failed step, throws: the runner then fails closed with the message.

import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
export const LOCK_FILE = 'hermes.lock';
export const REQUIREMENTS_FILE = 'hermes-requirements.lock';
export const MARKER = 'staged.json';
/** The interpreters Hermes supports (pyproject: requires-python >=3.11,<3.14). */
export const PYTHON_MINORS = [13, 12, 11];

/** The rows of a lock in the vendor.lock layout: name, version, platform, kind, sha256, url. */
export function parseLock(text) {
  return String(text ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith('#'))
    .map((line) => line.split(/\s+/))
    .filter((cols) => cols.length >= 6)
    .map(([name, version, platform, kind, sha256, url]) => ({ name, version, platform, kind, sha256, url }));
}

/** x64 takes linux-x64, arm64 linux-arm64: the two architectures colonies run on. */
export function archPlatform(arch = process.arch) {
  return arch === 'arm64' ? 'linux-arm64' : arch === 'x64' ? 'linux-x64' : null;
}

/** The rows this machine needs. Throws, naming what is missing, when the lock cannot serve it, or
 * when the source row does not name the pinned commit (the lock and module.json drifted). */
export function selectRows(rows, platform, pin = {}) {
  const one = (kind, plat) => {
    const matches = rows.filter((r) => r.kind === kind && (plat === undefined || r.platform === plat));
    if (matches.length !== 1) throw new Error(`hermes.lock has ${matches.length} ${kind} rows${plat ? ` for platform ${plat}` : ''}, needs exactly one`);
    return matches[0];
  };
  if (!platform) throw new Error('no pinned Hermes build for this architecture (only linux-x64 and linux-arm64)');
  const source = one('source');
  if (pin.source_rev && !source.url.includes(pin.source_rev)) {
    throw new Error(`hermes.lock's source url does not name the pinned commit ${pin.source_rev} (module.json requires.pins.hermes)`);
  }
  for (const row of rows) {
    if (!/^[0-9a-f]{64}$/.test(row.sha256)) throw new Error(`hermes.lock row ${row.name} ${row.platform}: "${row.sha256}" is not a sha256`);
  }
  return { source, requirements: one('requirements'), uv: one('tool', platform), python: one('python', platform) };
}

/** The requirement names in a requirements file that carry no --hash. Empty for a sound lock. */
export function unhashedRequirements(text) {
  const missing = [];
  let current = null;
  let hashed = false;
  const close = () => {
    if (current && !hashed) missing.push(current);
  };
  for (const raw of String(text ?? '').split('\n')) {
    const line = raw.replace(/\s+#.*$/, '');
    if (!line.trim() || line.trim().startsWith('#')) continue;
    if (!/^\s/.test(line) && !line.startsWith('--')) {
      close();
      current = line.split(/[\s=<>!~;\[]/)[0];
      hashed = false;
    }
    if (/--hash=sha256:[0-9a-f]{64}/.test(line)) hashed = true;
  }
  close();
  return missing;
}

const sha256 = (data) => createHash('sha256').update(data).digest('hex');

/** Where the build lives: the colony's /tmp is a small tmpfs, so under the cache dir instead. */
export function defaultCacheDir(env = process.env) {
  const base = env.XDG_CACHE_HOME || (env.HOME ? join(env.HOME, '.cache') : null);
  return base ? join(base, 'colonizer', 'hermes') : join(tmpdir(), 'colonizer-hermes-cache');
}

/** `cmd args` → { code, stdout, stderr }; never rejects, so callers word their own failure. */
export function execRun(cmd, args, opts = {}) {
  return new Promise((resolve) => {
    execFile(cmd, args, { maxBuffer: 64 * 1024 * 1024, ...opts }, (error, stdout, stderr) => {
      resolve({ code: error ? (typeof error.code === 'number' ? error.code : 1) : 0, stdout: String(stdout ?? ''), stderr: String(stderr ?? '') || (error && typeof error.code !== 'number' ? String(error.message) : '') });
    });
  });
}

/** Downloads `row.url` to `file`, hashing as it goes; throws on HTTP failure or a sha256 mismatch,
 * deleting the partial file, so nothing unverified is ever left behind or extracted. */
export async function fetchVerified(row, file, { fetchImpl = fetch, what = `${row.name} ${row.version} (${row.platform})` } = {}) {
  const res = await fetchImpl(row.url);
  if (!res?.ok) throw new Error(`downloading ${what} failed: HTTP ${res?.status ?? 'no response'} from ${row.url}`);
  const hash = createHash('sha256');
  const chunks = [];
  if (res.body && typeof res.body[Symbol.asyncIterator] === 'function') {
    for await (const chunk of res.body) {
      hash.update(chunk);
      chunks.push(Buffer.from(chunk));
    }
  } else {
    const bytes = Buffer.from(await res.arrayBuffer());
    hash.update(bytes);
    chunks.push(bytes);
  }
  const got = hash.digest('hex');
  if (got !== row.sha256) throw new Error(`Hermes staging refused: sha256 mismatch for ${what} (hermes.lock pins ${row.sha256}, the download is ${got})`);
  writeFileSync(file, Buffer.concat(chunks));
}

/** A python3.11–3.13 the image carries, or null. Candidates come from PATH, newest first. */
export async function findImagePython({ env = process.env, run = execRun } = {}) {
  const names = [...PYTHON_MINORS.map((m) => `python3.${m}`), 'python3'];
  for (const name of names) {
    for (const dir of String(env.PATH ?? '').split(':')) {
      const candidate = dir && join(dir, name);
      if (!candidate || !existsSync(candidate)) continue;
      const out = await run(candidate, ['-c', 'import sys; print(sys.version_info[0], sys.version_info[1])']);
      const [major, minor] = out.stdout.trim().split(/\s+/).map(Number);
      if (out.code === 0 && major === 3 && PYTHON_MINORS.includes(minor)) return { path: candidate, version: `3.${minor}` };
    }
  }
  return null;
}

/** The two lock files next to this module (or given), their digest, and the parsed pin. */
export function readLocks({ dir = here } = {}) {
  const lockText = readFileSync(join(dir, LOCK_FILE), 'utf8');
  const requirementsText = readFileSync(join(dir, REQUIREMENTS_FILE), 'utf8');
  const pin = JSON.parse(readFileSync(join(dir, 'module.json'), 'utf8'))?.requires?.pins?.hermes ?? {};
  return { lockText, requirementsText, requirementsPath: join(dir, REQUIREMENTS_FILE), pin };
}

/**
 * Builds (or reuses) the pinned Hermes and returns the path of its `hermes` entry point.
 * Every collaborator is injectable so the tests drive it without a network or a real uv.
 */
export async function stageHermes({
  env = process.env,
  arch = process.arch,
  locks = readLocks(),
  fetchImpl = fetch,
  run = execRun,
  findPython = findImagePython,
  cacheDir = defaultCacheDir(env),
  log = () => {},
} = {}) {
  const { lockText, requirementsText, requirementsPath, pin } = locks;
  const platform = archPlatform(arch);
  const rows = selectRows(parseLock(lockText), platform, pin);
  const reqSha = sha256(requirementsText);
  if (reqSha !== rows.requirements.sha256) {
    throw new Error(`Hermes staging refused: ${REQUIREMENTS_FILE} has sha256 ${reqSha}, hermes.lock pins ${rows.requirements.sha256}`);
  }
  const unhashed = unhashedRequirements(requirementsText);
  if (unhashed.length) throw new Error(`Hermes staging refused: ${REQUIREMENTS_FILE} lists requirements without a hash: ${unhashed.join(', ')}`);

  const rev = pin.source_rev || rows.source.url.split('/').pop();
  const key = `${rev.slice(0, 12)}-${sha256(lockText + requirementsText).slice(0, 12)}`;
  const dest = join(cacheDir, key, platform);
  const bin = join(dest, 'venv', 'bin', 'hermes');
  const marker = join(dest, MARKER);
  if (existsSync(marker) && existsSync(bin)) {
    try {
      const staged = JSON.parse(readFileSync(marker, 'utf8'));
      if (!staged.python || existsSync(staged.python)) return { bin, cached: true, dir: dest };
    } catch {
      // A torn marker is no marker: rebuild below.
    }
  }

  // A build without its marker is a half-finished one: start over, from an empty directory.
  rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  const started = Date.now();
  log({ level: 'info', message: `staging hermes-agent ${rows.source.version} (commit ${rev}) on first boot, once: about a minute on a fast link, and about 0.5 GB of disk` });
  const uvCache = join(dest, 'uv-cache');
  // No uv config from the image or the source tree, no managed-Python downloads behind our back.
  const uvEnv = { ...env, UV_NO_CONFIG: '1', UV_PYTHON_DOWNLOADS: 'never', UV_CACHE_DIR: uvCache, UV_LINK_MODE: 'copy', UV_NO_PROGRESS: '1' };
  const step = async (what, cmd, args, opts = {}) => {
    const out = await run(cmd, args, { cwd: dest, env: uvEnv, ...opts });
    if (out.code !== 0) {
      const full = `${out.stderr || out.stdout}`.trim();
      const lines = full.split('\n');
      const from = Math.max(0, lines.findIndex((l) => /^error/i.test(l.trim())));
      const detail = lines.slice(from, from + 12).join('\n'); // uv's error and its causes, not its progress lines
      if (/hash mismatch|hashes? (do|does) not match|requires? a hash|missing.*hash/i.test(full)) {
        throw new Error(`Hermes staging refused: a dependency failed its hash check against ${REQUIREMENTS_FILE} (${what}): ${detail}`);
      }
      throw new Error(`Hermes staging failed at ${what}: ${detail || `exit ${out.code}`}`);
    }
    return out;
  };
  const tar = (file, into, extra = []) => step(`extracting ${file}`, 'tar', ['-xzf', file, '-C', into, ...extra]);

  // uv: the archive holds uv-<triple>/uv; only that member is extracted.
  const uvTgz = join(dest, 'uv.tar.gz');
  await fetchVerified(rows.uv, uvTgz, { fetchImpl, what: `uv ${rows.uv.version} (${platform})` });
  mkdirSync(join(dest, 'uv'));
  const uvMember = `${rows.uv.url.split('/').pop().replace(/\.tar\.gz$/, '')}/uv`;
  await tar(uvTgz, join(dest, 'uv'), ['--strip-components=1', uvMember]);
  rmSync(uvTgz, { force: true });
  const uv = join(dest, 'uv', 'uv');

  // Python: the image's own 3.11–3.13 when it has one, else the pinned CPython build.
  let python = await findPython({ env, run });
  if (python) {
    log({ level: 'info', message: `hermes staging uses the image's Python ${python.version} at ${python.path}` });
  } else {
    const pyTgz = join(dest, 'python.tar.gz');
    await fetchVerified(rows.python, pyTgz, { fetchImpl, what: `CPython ${rows.python.version} (${platform})` });
    await tar(pyTgz, dest); // python/bin/python3, python/lib/…
    rmSync(pyTgz, { force: true });
    python = { path: join(dest, 'python', 'bin', 'python3'), version: rows.python.version };
    log({ level: 'info', message: `the image has no Python 3.11–3.13; hermes staging uses the pinned CPython ${rows.python.version}` });
  }

  // The source tree of the pinned commit, kept: the install below is editable, as upstream's is.
  const srcTgz = join(dest, 'source.tar.gz');
  await fetchVerified(rows.source, srcTgz, { fetchImpl, what: `hermes-agent ${rows.source.version} source (${rev})` });
  mkdirSync(join(dest, 'src'));
  await tar(srcTgz, join(dest, 'src'), ['--strip-components=1']);
  rmSync(srcTgz, { force: true });

  const venvPython = join(dest, 'venv', 'bin', 'python');
  await step('creating the venv', uv, ['venv', '--python', python.path, join(dest, 'venv')]);
  await step('installing the hash-pinned dependencies', uv, ['pip', 'install', '--python', venvPython, '--require-hashes', '--only-binary', ':all:', '-r', requirementsPath]);
  await step('installing hermes-agent from the pinned source', uv, ['pip', 'install', '--python', venvPython, '--no-deps', '--no-build-isolation', '--no-index', '-e', join(dest, 'src')]);
  if (!existsSync(bin)) throw new Error(`Hermes staging failed: the install left no hermes entry point at ${bin}`);
  rmSync(uvCache, { recursive: true, force: true });

  writeFileSync(marker, `${JSON.stringify({ source_rev: rev, version: rows.source.version, python: python.path, python_version: python.version, seconds: Math.round((Date.now() - started) / 1000) })}\n`);
  // Older builds (a previous pin) are dead weight on a small disk.
  for (const entry of readdirSync(cacheDir)) if (entry !== key) rmSync(join(cacheDir, entry), { recursive: true, force: true });
  log({ level: 'info', message: `staged hermes-agent ${rows.source.version} in ${Math.round((Date.now() - started) / 1000)}s` });
  return { bin, cached: false, dir: dest };
}
