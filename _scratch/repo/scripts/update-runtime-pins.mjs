#!/usr/bin/env node
// Proposes new pins for the colony images (crates/colonizer/images.lock) and the guest Claude Code
// build (vendor/claude-code.lock) when their upstreams move.
//
//   node scripts/update-runtime-pins.mjs                       report what moved upstream (changes nothing, exits 1 when stale)
//   node scripts/update-runtime-pins.mjs --check               the same check, named for CI
//   node scripts/update-runtime-pins.mjs --write               also rewrite both lock files
//   node scripts/update-runtime-pins.mjs --summary out.md      write the report as Markdown (a PR or issue body)
//   node scripts/update-runtime-pins.mjs --images p --agent p  use other lock files (for testing)
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
// is never invented.
//
// Nothing here decides to trust an update: images.lock is compiled into the mothership itself
// (include_str! in src/presets.rs), so a digest bump is a binary change, and every pin lands as a
// pull request that a person reads and merges.

import { createHash } from 'node:crypto';
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const imagesPath = option('--images') ?? join(root, 'crates/colonizer/images.lock');
const agentPath = option('--agent') ?? join(root, 'vendor/claude-code.lock');

// --check names the read-only mode the default already is, so CI can say what it means; it refuses
// to travel with --write, which asks for the opposite.
if (flag('--check') && flag('--write')) {
  console.error('--check and --write contradict each other: --check reports stale pins and never writes');
  process.exit(2);
}

const REGISTRY = 'https://registry-1.docker.io/v2';
const TOKEN = 'https://auth.docker.io/token?service=registry.docker.io&scope=repository';
const ANTHROPIC = 'https://downloads.claude.ai/claude-code-releases';
const USER_AGENT = 'colonizer-runtime-pins';
const DIGEST = /^sha256:[0-9a-f]{64}$/;
const SHA256 = /^[0-9a-f]{64}$/;
const VERSION = /^\d[\w.+-]*$/;
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

/** Orders two `x.y.z` versions numerically: negative when `a` is older than `b`. */
function compareVersions(a, b) {
  const [x, y] = [a, b].map((v) => v.split('.').map(Number));
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i];
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

async function main() {
  const read = (path) => {
    const text = readFileSync(path, 'utf8');
    return { text, ...parseLock(text, path) };
  };
  const images = read(imagesPath);
  const agent = read(agentPath);
  // Everything resolves before anything writes: a failure above leaves both files untouched.
  const sections = [
    ...(await proposeImages(images.lines, images.entries.filter((entry) => entry.kind === 'image'))),
    ...(await proposeAgent(agent.lines, agent.entries.filter((entry) => entry.kind === 'agent'))),
  ];

  const changed = sections.length > 0;
  if (changed && flag('--write')) {
    for (const [path, before, lines] of [
      [imagesPath, images, images.lines],
      [agentPath, agent, agent.lines],
    ]) {
      const after = lines.join('\n');
      if (after === before.text) continue;
      writeFileSync(path, after);
      console.log(`wrote ${relative(process.cwd(), path)}`);
    }
  }
  const summary = option('--summary');
  if (summary) {
    mkdirSync(dirname(summary), { recursive: true });
    writeFileSync(
      summary,
      changed
        ? [
            'Upstream moved for these runtime pins. `crates/colonizer/images.lock` and `vendor/claude-code.lock` below pin the new digests and checksums.',
            '',
            'These pins are what a release runs: images.lock is compiled into the mothership (include_str! in src/presets.rs), so a digest bump is a binary change, and every colony built from the next release boots the exact bytes the new digest names. Nothing here merges on its own — read the digest-to-digest diffs before merging.',
            '',
            ...sections,
            '',
            '_Opened by `.github/workflows/runtime-pin-updates.yml`._',
          ].join('\n')
        : 'Every runtime pin is at its upstream.\n',
    );
  }
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `changed=${changed}\n`);
  // The check is meant to be wired to something: stale pins fail it, --write resolves them.
  if (changed && !flag('--write')) process.exitCode = 1;
  if (!changed) console.log('no updates');
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
