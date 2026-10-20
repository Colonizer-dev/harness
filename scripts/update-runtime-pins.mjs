#!/usr/bin/env node
// Proposes new pins for the colony images (crates/colonizer/images.lock), the guest Claude Code
// build (crates/colonizer/claude-code.lock) and the understand-anything skillset
// (crates/colonizer/understand-anything.lock) when their upstreams move.
//
//   node scripts/update-runtime-pins.mjs                       report what moved upstream (changes nothing, exits 1 when stale)
//   node scripts/update-runtime-pins.mjs --check               the same check, named for CI
//   node scripts/update-runtime-pins.mjs --write               also rewrite every lock file
//   node scripts/update-runtime-pins.mjs --summary out.md      write the report as Markdown (a PR or issue body)
//   node scripts/update-runtime-pins.mjs --images p --agent p --understand-anything p
//                                                             use other lock files (for testing)
//
// An image pin is resolved through the registry's own API: an anonymous pull token from
// auth.docker.io, then a HEAD manifest request whose Docker-Content-Digest header names the
// multi-arch OCI index digest — the one that covers linux/amd64 and linux/arm64, so one pin serves
// both colony architectures. A digest is only proposed after a GET of that index confirms it
// really carries both and really hashes to the digest the header named; a single-platform
// manifest fails the run instead of being pinned. Claude Code comes from Anthropic's release
// manifest, the one scripts/fetch-agent-binary.sh used before it read the lock: the stable channel
// names a version and manifest.json carries a checksum per guest platform, so the 200 MB binaries
// never have to be downloaded. A platform the manifest does not cover fails the run — a checksum
// is never invented. The understand-anything skillset comes from GitHub's own release API: the
// latest release's tag resolves to the commit behind it, and the tarball of that commit is
// downloaded and hashed, because the mothership fetches the same tarball and checks this checksum
// fail-closed before it unpacks anything.
//
// Nothing here decides to trust an update: images.lock is compiled into the mothership itself
// (include_str! in src/presets.rs), so a digest bump is a binary change, and every pin lands as a
// pull request that a person reads and merges.

import { createHash } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const imagesPath = option('--images') ?? join(root, 'crates/colonizer/images.lock');
const agentPath = option('--agent') ?? join(root, 'crates/colonizer/claude-code.lock');
const understandAnythingPath = option('--understand-anything') ?? join(root, 'crates/colonizer/understand-anything.lock');
const planetscalePath = option('--planetscale') ?? join(root, 'crates/colonizer/planetscale.lock');

// --check names the read-only mode the default already is, so CI can say what it means; it refuses
// to travel with --write, which asks for the opposite.
if (flag('--check') && flag('--write')) {
  console.error('--check and --write contradict each other: --check reports stale pins and never writes');
  process.exit(2);
}

const REGISTRY = 'https://registry-1.docker.io/v2';
const TOKEN = 'https://auth.docker.io/token?service=registry.docker.io&scope=repository';
const ANTHROPIC = 'https://downloads.claude.ai/claude-code-releases';
const UNDERSTAND_ANYTHING_REPO = 'Egonex-AI/Understand-Anything';
const UNDERSTAND_ANYTHING_API = `https://api.github.com/repos/${UNDERSTAND_ANYTHING_REPO}`;
const UNDERSTAND_ANYTHING_CODELOAD = `https://codeload.github.com/${UNDERSTAND_ANYTHING_REPO}/tar.gz`;
const PLANETSCALE_CLI_REPO = 'planetscale/cli';
const PLANETSCALE_CLI_API = `https://api.github.com/repos/${PLANETSCALE_CLI_REPO}`;
const PLANETSCALE_SKILLS_REPO = 'planetscale/skills';
const PLANETSCALE_SKILLS_API = `https://api.github.com/repos/${PLANETSCALE_SKILLS_REPO}`;
const PLANETSCALE_SKILLS_CODELOAD = `https://codeload.github.com/${PLANETSCALE_SKILLS_REPO}/tar.gz`;
const PLANETSCALE_MIN_VERSION = '0.292.0';
const USER_AGENT = 'colonizer-runtime-pins';
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const SHA256 = /^[0-9a-f]{64}$/;
const VERSION = /^\d[\w.+-]*$/;
const COMMIT = /^[0-9a-f]{40}$/;
// The index types are the multi-arch ones; the single-manifest types are in Accept only so the
// registry answers at all, and a single-manifest answer is rejected below rather than pinned.
const INDEX_TYPES = 'application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json';
const ACCEPT = `${INDEX_TYPES}, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json`;
const INDEX_MEDIA_TYPES = new Set([
  'application/vnd.oci.image.index.v1+json',
  'application/vnd.docker.distribution.manifest.list.v2+json',
]);

async function registryToken(repository) {
  const response = await fetch(`${TOKEN}:${repository}:pull`, { headers: { 'user-agent': USER_AGENT } });
  if (!response.ok) throw new Error(`registry token for ${repository}: HTTP ${response.status}`);
  const { token } = await response.json();
  if (typeof token !== 'string' || !token) throw new Error(`registry token for ${repository}: the response carried no token`);
  return token;
}

/** `golang:1-bookworm` → the repository the registry knows (official images live under library/) and its tag. */
function splitReference(reference) {
  const colon = reference.lastIndexOf(':');
  const [path, tag] = colon < 0 ? [reference, 'latest'] : [reference.slice(0, colon), reference.slice(colon + 1)];
  if (!path || !tag || path.includes('@') || path.includes(':')) {
    throw new Error(`${reference}: not a name:tag reference this script can pin`);
  }
  return { repository: path.includes('/') ? path : `library/${path}`, tag };
}

/**
 * The multi-arch index digest a tag points at, by HEAD — the body never downloads. A registry that
 * answers without a sha256 digest, or with a single-platform manifest, fails instead of pinning.
 */
async function indexDigest(reference) {
  const { repository, tag } = splitReference(reference);
  const headers = { accept: ACCEPT, authorization: `Bearer ${await registryToken(repository)}`, 'user-agent': USER_AGENT };
  const response = await fetch(`${REGISTRY}/${repository}/manifests/${tag}`, { method: 'HEAD', headers });
  if (!response.ok) throw new Error(`${reference}: HTTP ${response.status} asking the registry for ${tag}`);
  const digest = response.headers.get('docker-content-digest');
  const mediaType = (response.headers.get('content-type') ?? '').split(';')[0].trim();
  if (!DIGEST.test(digest ?? '')) {
    throw new Error(`${reference}: the registry answered without a sha256 digest (got ${JSON.stringify(digest)})`);
  }
  if (!INDEX_MEDIA_TYPES.has(mediaType)) {
    throw new Error(
      `${reference}: ${tag} resolves to a single-platform manifest (${mediaType}), not a multi-arch index — ` +
        'one pin has to serve both colony architectures',
    );
  }
  return digest;
}

/** The check a proposal has to pass before it is worth a reviewer's time: both colony
 * architectures are really in the index, and the index really hashes to the digest. */
async function checkIndex(reference, digest) {
  const { repository } = splitReference(reference);
  const headers = { accept: INDEX_TYPES, authorization: `Bearer ${await registryToken(repository)}`, 'user-agent': USER_AGENT };
  const response = await fetch(`${REGISTRY}/${repository}/manifests/${digest}`, { headers });
  if (!response.ok) throw new Error(`${reference}: HTTP ${response.status} fetching the index by digest`);
  const body = Buffer.from(await response.arrayBuffer());
  const hashed = `sha256:${createHash('sha256').update(body).digest('hex')}`;
  if (hashed !== digest) throw new Error(`${reference}: the index hashes to ${hashed}, not the ${digest} the header named`);
  let index;
  try {
    index = JSON.parse(body);
  } catch {
    throw new Error(`${reference}: the index is not JSON`);
  }
  if (!Array.isArray(index.manifests)) throw new Error(`${reference}: the index carries no manifests list`);
  const covered = new Set(index.manifests.map((m) => `${m.platform?.os ?? '?'}/${m.platform?.architecture ?? '?'}`));
  const missing = ['linux/amd64', 'linux/arm64'].filter((arch) => !covered.has(arch));
  if (missing.length) throw new Error(`${reference}: the index does not carry ${missing.join(' or ')}`);
}

async function stableVersion() {
  const response = await fetch(`${ANTHROPIC}/stable`, { headers: { 'user-agent': USER_AGENT } });
  if (!response.ok) throw new Error(`the stable channel: HTTP ${response.status}`);
  const version = (await response.text()).trim();
  if (!VERSION.test(version)) throw new Error(`unexpected version from the stable channel: ${version}`);
  return version;
}

/** Orders two versions the way semver does: numerically per `.` segment (all of them, not just the
 * first three), a pre-release like `2.1.281-rc1` sorts before its release, and `+build` metadata
 * never decides. A NaN here would make the pinned-ahead guard in proposeAgent silently fail. */
export function compareVersions(a, b) {
  const segments = (v) =>
    v.split('.').map((s) => {
      const match = /^(\d+)([-+].*)?$/.exec(s);
      if (!match) return [0, s];
      const suffix = match[2] ?? '';
      return [Number(match[1]), suffix.startsWith('+') ? '' : suffix];
    });
  const [x, y] = [segments(a), segments(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const [xn, xs] = x[i] ?? [0, ''];
    const [yn, ys] = y[i] ?? [0, ''];
    if (xn !== yn) return xn - yn;
    if (xs !== ys) {
      if (!xs || !ys) return xs ? -1 : 1; // the one carrying a pre-release suffix is the older
      return xs < ys ? -1 : 1;
    }
  }
  return 0;
}

async function releaseManifest(version) {
  const response = await fetch(`${ANTHROPIC}/${version}/manifest.json`, { headers: { 'user-agent': USER_AGENT } });
  if (!response.ok) throw new Error(`the ${version} manifest: HTTP ${response.status}`);
  const manifest = await response.json();
  if (manifest?.version !== version || manifest?.platforms === null || typeof manifest?.platforms !== 'object') {
    throw new Error(`the ${version} manifest does not name a version and its platforms`);
  }
  return manifest;
}

/**
 * The headers a GitHub request carries. The repository is public, so this works anonymously; the
 * workflow's own token is used when there is one, which is the difference between 60 and 5000
 * requests an hour.
 */
function githubHeaders() {
  const headers = { accept: 'application/vnd.github+json', 'user-agent': USER_AGENT, 'x-github-api-version': '2022-11-28' };
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
  if (token) headers.authorization = `Bearer ${token}`;
  return headers;
}

/** The latest published release's tag, or the error GitHub gave. The pin moves on releases only. */
async function latestReleaseTag() {
  const response = await fetch(`${UNDERSTAND_ANYTHING_API}/releases/latest`, { headers: githubHeaders() });
  if (!response.ok) throw new Error(`the latest ${UNDERSTAND_ANYTHING_REPO} release: HTTP ${response.status}`);
  const tag = (await response.json())?.tag_name;
  if (typeof tag !== 'string' || !tag.trim()) throw new Error('the latest release names no tag');
  return tag.trim();
}

/**
 * The commit a tag points at. codeload serves a tarball of a ref too, but the lock pins the commit
 * so the URL cannot move under the checksum if the tag is ever repointed.
 */
async function commitOfTag(tag) {
  const response = await fetch(`${UNDERSTAND_ANYTHING_API}/commits/${encodeURIComponent(tag)}`, { headers: githubHeaders() });
  if (!response.ok) throw new Error(`the ${tag} commit: HTTP ${response.status}`);
  const sha = (await response.json())?.sha;
  if (!COMMIT.test(sha ?? '')) throw new Error(`${tag} resolved to no 40-hex commit (got ${JSON.stringify(sha)})`);
  return sha;
}

/** The sha256 of the exact tarball the mothership downloads. Hashed here rather than trusted. */
async function tarballSha(commit) {
  const response = await fetch(`${UNDERSTAND_ANYTHING_CODELOAD}/${commit}`, { headers: { 'user-agent': USER_AGENT } });
  if (!response.ok) throw new Error(`the ${commit} tarball: HTTP ${response.status}`);
  return createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex');
}

/**
 * One understand-anything lock line as an object, or null when the line is not one. The row is the
 * six lock columns: name, version (the upstream tag), platform `any`, kind, sha256, tarball URL.
 */
export function parseUnderstandAnythingLock(line) {
  const text = String(line ?? '').trim();
  if (!text || text.startsWith('#')) return null;
  const [name, version, platform, kind, sha, url, ...rest] = text.split(/\s+/);
  if (rest.length || name !== 'understand-anything' || !version || !platform || !kind) return null;
  if (!SHA256.test(sha ?? '')) return null;
  if (!/^https:\/\/codeload\.github\.com\//.test(url ?? '')) return null;
  return { name, version, platform, kind, sha, url };
}

/** The lock line for a pin, in the column order parseUnderstandAnythingLock reads. */
export function formatUnderstandAnythingLock({ version, sha, commit }) {
  return ['understand-anything', version, 'any', 'source', sha, `${UNDERSTAND_ANYTHING_CODELOAD}/${commit}`].join('  ');
}

function parseLock(text, path) {
  const lines = text.split('\n');
  const entries = [];
  lines.forEach((line, index) => {
    if (!line.trim() || line.trimStart().startsWith('#')) return;
    const [name, version, platform, kind, sha, url] = line.trim().split(/\s+/);
    if (!url || !SHA256.test(sha)) throw new Error(`${path}:${index + 1}: not a six-column lock row with a sha256`);
    entries.push({ index, name, version, platform, kind, sha, url });
  });
  return { lines, entries };
}

/**
 * A rewritten lock file keeps the trailing newline the hand-written one has. `parseLock` splits on
 * `\n` and the rewrite joins on `\n`, so the file's own last line (empty, for a file ending in a
 * newline) round-trips; this only puts it back for a lock file that somehow lost it.
 */
export function withTrailingNewline(text) {
  return text.endsWith('\n') ? text : `${text}\n`;
}

/** The lock line with the new pin. Only the fields that moved change, so the header comments,
 * column alignment and row order around them are untouched. */
function applyUpdate(lines, entry, next) {
  let line = lines[entry.index];
  if (next.version !== entry.version) line = line.replace(entry.version, next.version);
  if (next.sha !== entry.sha) line = line.replace(entry.sha, next.sha);
  if (next.url !== entry.url) line = line.replace(entry.url, next.url);
  lines[entry.index] = line;
}

function imageSection(entry, digest) {
  const { repository, tag } = splitReference(entry.url);
  const hub = repository.startsWith('library/')
    ? `https://hub.docker.com/_/${repository.slice('library/'.length)}`
    : `https://hub.docker.com/r/${repository}`;
  return [
    `### \`${entry.name}\` — \`${entry.url}\` ([Docker Hub](${hub}/tags?name=${tag}))`,
    '',
    `index digest \`sha256:${entry.sha}\` → \`${digest}\``,
    '',
    'Both architectures were confirmed in the new index before proposing it.',
  ].join('\n');
}

function agentSection(rows, version) {
  const moved = rows.some((row) => row.version !== version);
  const title = moved
    ? `### \`claude-code\`: ${rows[0].version} → [${version}](${ANTHROPIC}/${version}/manifest.json)`
    : `### \`claude-code\`: a checksum moved under ${version} — [the manifest](${ANTHROPIC}/${version}/manifest.json)`;
  return [title, '', ...rows.map((row) => `- \`${row.platform}\`: \`${row.sha}\` → \`${row.nextSha}\``)].join('\n');
}

async function proposeImages(lines, entries) {
  const sections = [];
  for (const entry of entries) {
    const digest = await indexDigest(entry.url);
    if (digest === `sha256:${entry.sha}`) {
      console.log(`${entry.name} (${entry.url}): current (${digest})`);
      continue;
    }
    await checkIndex(entry.url, digest);
    console.log(`${entry.name} (${entry.url}): sha256:${entry.sha} -> ${digest}`);
    // The version column is the tag from the reference, so it only moves if the reference did.
    const { tag } = splitReference(entry.url);
    applyUpdate(lines, entry, { version: tag, sha: digest.slice('sha256:'.length), url: entry.url });
    sections.push(imageSection(entry, digest));
  }
  return sections;
}

async function proposeAgent(lines, entries) {
  const version = await stableVersion();
  const manifest = await releaseManifest(version);
  const rows = [];
  for (const entry of entries) {
    const checksum = manifest.platforms[entry.platform]?.checksum;
    if (!SHA256.test(checksum ?? '')) {
      throw new Error(`the ${version} manifest carries no checksum for ${entry.platform} — not inventing one`);
    }
    if (checksum === entry.sha && version === entry.version) {
      console.log(`${entry.name} ${entry.platform}: current (${version})`);
      continue;
    }
    // A pin can be moved ahead of stable by hand when a newer build is needed — Claude Opus 5.5 is
    // refused by the API below Claude Code 2.1.280 — and following stable back down would break it.
    if (compareVersions(version, entry.version) < 0) {
      console.log(`${entry.name} ${entry.platform}: pinned ${entry.version} is ahead of stable ${version}; kept`);
      continue;
    }
    console.log(`${entry.name} ${entry.platform}: ${entry.version}/${entry.sha.slice(0, 12)} -> ${version}/${checksum.slice(0, 12)}`);
    applyUpdate(lines, entry, {
      version,
      sha: checksum,
      url: `${ANTHROPIC}/${version}/${entry.platform}/claude`,
    });
    rows.push({ platform: entry.platform, version: entry.version, sha: entry.sha, nextSha: checksum });
  }
  return rows.length ? [agentSection(rows, version)] : [];
}

function understandAnythingSection(entry, { version, sha, commit }) {
  return [
    `### \`understand-anything\`: ${entry.version} → [${version}](https://github.com/${UNDERSTAND_ANYTHING_REPO}/releases/tag/${version})`,
    '',
    `commit \`${entry.url.split('/').pop()}\` → \`${commit}\``,
    '',
    `sha256 \`${entry.sha}\` → \`${sha}\``,
    '',
    'The tarball of that commit was downloaded and hashed here; the mothership checks the same checksum before it unpacks anything, and refuses a download that does not match.',
  ].join('\n');
}

async function proposeUnderstandAnything(lines, entries) {
  const sections = [];
  for (const entry of entries) {
    const pinned = parseUnderstandAnythingLock(lines[entry.index]);
    if (!pinned) throw new Error(`${UNDERSTAND_ANYTHING_REPO}: the lock row is not a six-column skillset row`);
    const version = await latestReleaseTag();
    const commit = await commitOfTag(version);
    // Already on this commit: the checksum was computed against the same bytes the mothership would
    // download, so there is nothing to say.
    if (pinned.url.endsWith(`/${commit}`)) {
      console.log(`${pinned.name} ${version}: current (${commit})`);
      continue;
    }
    const sha = await tarballSha(commit);
    console.log(`${pinned.name} ${pinned.version}/${pinned.sha.slice(0, 12)} -> ${version}/${sha.slice(0, 12)}`);
    // The whole data line is rewritten: the version and the URL both move, and applyUpdate's
    // field-by-field replace cannot put the commit in the URL. Header comments are untouched.
    lines[entry.index] = formatUnderstandAnythingLock({ version, sha, commit });
    sections.push(understandAnythingSection(pinned, { version, sha, commit }));
  }
  return sections;
}

async function proposePlanetscale(lines, entries) {
  const pscaleRows = [];
  const sections = [];
  const [psEntries, skillsEntries] = [entries.filter((e) => e.name === 'pscale'), entries.filter((e) => e.name === 'planetscale-skills')];

  // Get latest pscale version and verify minimum
  const r = await fetch(`${PLANETSCALE_CLI_API}/releases/latest`, { headers: githubHeaders() });
  if (!r.ok) throw new Error(`pscale latest: HTTP ${r.status}`);
  const tag = (await r.json())?.tag_name?.slice(1);
  if (!VERSION.test(tag)) throw new Error(`pscale version ${tag} invalid`);
  if (compareVersions(tag, PLANETSCALE_MIN_VERSION) < 0) throw new Error(`pscale ${tag} below ${PLANETSCALE_MIN_VERSION}`);

  // Get pscale checksums
  const chkR = await fetch(`https://github.com/planetscale/cli/releases/download/v${tag}/pscale_${tag}_checksums.txt`, { headers: { 'user-agent': USER_AGENT } });
  if (!chkR.ok) throw new Error(`pscale checksums: HTTP ${chkR.status}`);
  const chk = new Map();
  for (const line of (await chkR.text()).trim().split('\n')) {
    const [sha, f] = line.trim().split(/\s+/);
    const m = f?.match(/pscale_[^_]+_(linux)_(amd64|arm64)\.tar\.gz/) || f?.match(/pscale_[^_]+_(macOS)_(arm64)\.tar\.gz/);
    if (m && SHA256.test(sha)) {
      const pm = { 'linux_amd64': 'linux-x86_64', 'linux_arm64': 'linux-aarch64', 'macOS_arm64': 'darwin-aarch64' };
      chk.set(pm[`${m[1]}_${m[2]}`], sha);
    }
  }

  // Check pscale entries
  for (const entry of psEntries) {
    const pinned = { name: 'pscale', ...Object.fromEntries([entry, lines[entry.index]].slice(0, 1).map((_, i) => {
      const [n, v, p, k, s, u] = lines[entry.index].trim().split(/\s+/);
      return [[n, v, p, k, s, u]];
    })[0] ?? []) };
    if (!pinned.version) throw new Error('pscale lock invalid');
    const url = pinned.url.replace(/pscale_[^_]+_/, `pscale_${tag}_`);
    const res = await fetch(url, { headers: { 'user-agent': USER_AGENT } });
    if (!res.ok) throw new Error(`pscale ${entry.platform} tarball: HTTP ${res.status}`);
    const sha = createHash('sha256').update(Buffer.from(await res.arrayBuffer())).digest('hex');
    const expected = chk.get(entry.platform);
    if (!expected) throw new Error(`pscale checksums missing ${entry.platform}`);
    if (sha !== expected) throw new Error(`pscale ${entry.platform} checksum mismatch`);
    if (tag === pinned.version && sha === pinned.sha) {
      console.log(`pscale ${entry.platform}: current (${tag})`);
      continue;
    }
    console.log(`pscale ${entry.platform}: ${pinned.version}/${pinned.sha.slice(0, 12)} -> ${tag}/${sha.slice(0, 12)}`);
    applyUpdate(lines, entry, { version: tag, sha, url });
    pscaleRows.push({ platform: entry.platform, version: tag, oldVersion: pinned.version, oldSha: pinned.sha, sha });
  }

  // Check planetscale-skills
  let skillsRow = null;
  if (skillsEntries.length > 0) {
    const br = await fetch(PLANETSCALE_SKILLS_API, { headers: githubHeaders() });
    if (!br.ok) throw new Error(`planetscale/skills: HTTP ${br.status}`);
    const branch = (await br.json())?.default_branch;
    const ref = await fetch(`${PLANETSCALE_SKILLS_API}/git/refs/heads/${encodeURIComponent(branch)}`, { headers: githubHeaders() });
    if (!ref.ok) throw new Error(`planetscale/skills ref: HTTP ${ref.status}`);
    const commit = (await ref.json())?.object?.sha;
    if (!COMMIT.test(commit)) throw new Error(`planetscale/skills commit invalid`);

    for (const entry of skillsEntries) {
      const pinned = lines[entry.index].trim().split(/\s+/);
      if (pinned[0] !== 'planetscale-skills') throw new Error('planetscale-skills lock invalid');
      if (pinned[1] === commit && pinned[4] === pinned[5]?.split('/').pop()) {
        console.log(`planetscale-skills: current (${commit})`);
        continue;
      }
      const tr = await fetch(`${PLANETSCALE_SKILLS_CODELOAD}/${commit}`, { headers: { 'user-agent': USER_AGENT } });
      if (!tr.ok) throw new Error(`planetscale-skills tarball: HTTP ${tr.status}`);
      const sha = createHash('sha256').update(Buffer.from(await tr.arrayBuffer())).digest('hex');
      console.log(`planetscale-skills: ${pinned[1].slice(0, 12)} -> ${commit.slice(0, 12)}`);
      lines[entry.index] = ['planetscale-skills', commit, 'any', 'source', sha, `${PLANETSCALE_SKILLS_CODELOAD}/${commit}`].join('  ');
      skillsRow = { commit, oldCommit: pinned[1], sha, oldSha: pinned[4] };
    }
  }

  if (pscaleRows.length || skillsRow) {
    let sec = '';
    if (pscaleRows.length) {
      const moved = pscaleRows.some((r) => r.version !== pscaleRows[0].version);
      sec += `### \`pscale\`: ${moved ? `${pscaleRows[0].oldVersion} → [${pscaleRows[0].version}](${PLANETSCALE_CLI_API}/releases/tag/v${pscaleRows[0].version})` : `checksums moved under ${pscaleRows[0].version}`}\n\n`;
      for (const r of pscaleRows) sec += `- \`${r.platform}\`: \`${r.oldSha.slice(0, 12)}\` → \`${r.sha.slice(0, 12)}\`\n`;
      sec += '\n';
    }
    if (skillsRow) {
      sec += `### \`planetscale-skills\`\n\ncommit \`${skillsRow.oldCommit.slice(0, 12)}\` → \`${skillsRow.commit.slice(0, 12)}\`\n\nsha256 \`${skillsRow.oldSha.slice(0, 12)}\` → \`${skillsRow.sha.slice(0, 12)}\`\n\n`;
    }
    sections.push(sec);
  }
  return sections;
}

async function main() {
  const read = (path) => {
    const text = readFileSync(path, 'utf8');
    return { text, ...parseLock(text, path) };
  };
  const images = read(imagesPath);
  const agent = read(agentPath);
  // The skillset locks are optional until the mothership ships them: a run without them only reports the
  // other pins, and nothing here invents a file that does not exist yet.
  const understandAnything = existsSync(understandAnythingPath) ? read(understandAnythingPath) : null;
  const planetscale = existsSync(planetscalePath) ? read(planetscalePath) : null;
  // Everything resolves before anything writes: a failure above leaves every file untouched.
  const sections = [
    ...(await proposeImages(images.lines, images.entries.filter((entry) => entry.kind === 'image'))),
    ...(await proposeAgent(agent.lines, agent.entries.filter((entry) => entry.kind === 'agent'))),
  ];
  // The skillsets come from GitHub's API, which answers 403 to an unauthenticated or rate-limited
  // request and 404 to a repository that moved. That says nothing about the other pins, so these
  // ones are reported and stepped over rather than failing the whole run — under --check they simply
  // cannot say whether the skillsets moved, and say that instead of claiming a pin is current.
  let understandAnythingFailure;
  if (understandAnything) {
    try {
      sections.push(...(await proposeUnderstandAnything(understandAnything.lines, understandAnything.entries)));
    } catch (error) {
      understandAnythingFailure = error.message;
      console.warn(`warning: ${UNDERSTAND_ANYTHING_REPO}: could not check the skillset pin: ${error.message}`);
      console.warn('warning: the other runtime pins are unaffected; re-run with GITHUB_TOKEN to check the skillset too');
    }
  }
  let planetscaleFailure;
  if (planetscale) {
    try {
      sections.push(...(await proposePlanetscale(planetscale.lines, planetscale.entries)));
    } catch (error) {
      planetscaleFailure = error.message;
      console.warn(`warning: ${PLANETSCALE_CLI_REPO}: could not check the planetscale pin: ${error.message}`);
      console.warn('warning: the other runtime pins are unaffected');
    }
  }

  const changed = sections.length > 0;
  if (changed && flag('--write')) {
    for (const [path, before, lines] of [
      [imagesPath, images, images.lines],
      [agentPath, agent, agent.lines],
      // A run that failed part-way through the skillset rows has half of them rewritten in memory;
      // writing that would pin a mixture of two releases, so the file is left exactly as it was.
      ...(understandAnything && !understandAnythingFailure
        ? [[understandAnythingPath, understandAnything, understandAnything.lines]]
        : []),
      ...(planetscale && !planetscaleFailure
        ? [[planetscalePath, planetscale, planetscale.lines]]
        : []),
    ]) {
      const after = lines.join('\n');
      if (after === before.text) continue;
      writeFileSync(path, withTrailingNewline(after));
      console.log(`wrote ${relative(process.cwd(), path)}`);
    }
  }
  const summary = option('--summary');
  if (summary) {
    const skipped = [
      ...(understandAnythingFailure ? [`The \`understand-anything\` pin was not checked: ${understandAnythingFailure}. Re-run with \`GITHUB_TOKEN\` set, or run the script by hand.`] : []),
      ...(planetscaleFailure ? [`The \`planetscale\` pin was not checked: ${planetscaleFailure}.`] : []),
    ];
    const skippedSection = skipped.length > 0 ? ['', ...skipped] : [];
    mkdirSync(dirname(summary), { recursive: true });
    writeFileSync(
      summary,
      changed
        ? [
            'Upstream moved for these runtime pins. `crates/colonizer/images.lock`, `crates/colonizer/claude-code.lock`, `crates/colonizer/understand-anything.lock` and `crates/colonizer/planetscale.lock` below pin the new digests, checksums and skillset releases.',
            '',
            'These pins are what a release runs: images.lock is compiled into the mothership (include_str! in src/presets.rs), so a digest bump is a binary change, and every colony built from the next release boots the exact bytes the new digest names. Nothing here merges on its own — read the digest-to-digest diffs before merging.',
            '',
            ...sections,
            ...skippedSection,
            '',
            '_Opened by `.github/workflows/runtime-pin-updates.yml`._',
          ].join('\n')
        : [
            (understandAnythingFailure || planetscaleFailure)
              ? 'Every runtime pin this run could check is at its upstream.'
              : 'Every runtime pin is at its upstream.',
            ...skippedSection,
            '',
          ].join('\n'),
    );
  }
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `changed=${changed}\n`);
  // The check is meant to be wired to something: stale pins fail it, --write resolves them.
  if (changed && !flag('--write')) process.exitCode = 1;
  if (!changed) console.log('no updates');
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}

// Exports for testing
export function parsePlanetscaleLock(line) {
  const [n, v, p, k, s, u, ...r] = String(line ?? '').trim().split(/\s+/);
  if (r.length || !['pscale', 'planetscale-skills'].includes(n) || !SHA256.test(s)) return null;
  if (n === 'pscale' && !/^https:\/\/github\.com\/planetscale\/cli\/releases\/download/.test(u)) return null;
  if (n === 'planetscale-skills' && !/^https:\/\/codeload\.github\.com\/planetscale\/skills/.test(u)) return null;
  return { name: n, version: v, platform: p, kind: k, sha: s, url: u };
}
export function formatPscaleLock({ version, platform, sha, url }) {
  return ['pscale', version, platform, 'archive', sha, url].join('  ');
}
export function formatPlanetscaleSkillsLock({ commit, sha }) {
  return ['planetscale-skills', commit, 'any', 'source', sha, `${PLANETSCALE_SKILLS_CODELOAD}/${commit}`].join('  ');
}
