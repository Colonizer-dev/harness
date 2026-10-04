#!/usr/bin/env node
// Checks a released tag the way a user meets it, one Markdown line per check, for
// .github/workflows/release-health.yml (after a release, and nightly):
//
//   node scripts/release-health.mjs [--tag v0.1.11] [--force-failure]
//
// It checks the GitHub release's assets and SHA256SUMS, both crates on crates.io at the tag's
// version, and that colonizer.dev names it. Installing and `colonizer update --check` need a real
// machine, so they are the workflow's matrix. The site check is /docs/changelog: the home page's
// install line is deliberately version-less, so it is only asserted still to be there. Exit is 0
// only when every check passes. GITHUB_TOKEN, if set, is used for the GitHub API.

import { createHash } from 'node:crypto';

const REPO = 'Colonizer-dev/harness';
const SITE = 'https://colonizer.dev';
const CRATES = ['colonizer-harness', 'colonizer-agentd'];
// Keep in step with the assets release.yml attaches.
const ASSETS = ['colonizer-linux-x86_64.tar.gz', 'colonizer-darwin-arm64.tar.gz', 'install.sh', 'SHA256SUMS'];
const USER_AGENT = 'colonizer-release-health';

const args = process.argv.slice(2);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const forceFailure = args.includes('--force-failure') || process.env.RELEASE_HEALTH_FORCE_FAILURE === 'true';
const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;

const UA = { 'user-agent': USER_AGENT };
const API = { ...UA, accept: 'application/vnd.github+json' };
if (token) API.authorization = `Bearer ${token}`;

const checks = [];
const record = (ok, text) => checks.push({ ok, text });
const recordIf = (ok, onOk, onFail) => record(ok, ok ? onOk : onFail);

// Downloads and the site get no Authorization, so a redirect to the asset host is followed cleanly.
async function get(url, headers = UA) {
  const res = await fetch(url, { headers, signal: AbortSignal.timeout(600000) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res;
}
const text = async (url) => (await get(url)).text();

async function releaseChecks(tag) {
  let release;
  try {
    release = await get(`https://api.github.com/repos/${REPO}/releases/tags/${tag}`, API).then((r) => r.json());
  } catch (e) {
    return record(false, `**release** — no GitHub release for ${tag} (${e.message})`);
  }
  const present = new Set(release.assets.map((a) => a.name));
  const missing = ASSETS.filter((name) => !present.has(name));
  recordIf(missing.length === 0, `**release** — all ${ASSETS.length} assets present`,
    `**release** — missing ${missing.join(', ')}`);
  if (missing.length) return;

  const base = `https://github.com/${REPO}/releases/download/${tag}`;
  let sums;
  try {
    sums = await text(`${base}/SHA256SUMS`);
  } catch (e) {
    return record(false, `**checksums** — could not read SHA256SUMS (${e.message})`);
  }
  const lines = sums.split('\n').map((l) => l.trim()).filter(Boolean);
  const listed = lines.map((l) => (l.split(/\s+/)[1] ?? '').replace(/^\*/, ''));
  const unlisted = ASSETS.filter((name) => name !== 'SHA256SUMS' && !listed.includes(name));
  recordIf(unlisted.length === 0, '**checksums** — SHA256SUMS lists every asset',
    `**checksums** — SHA256SUMS does not list ${unlisted.join(', ')}`);

  const bad = [];
  for (const line of lines) {
    const [want, raw] = line.split(/\s+/);
    const name = (raw ?? '').replace(/^\*/, '');
    try {
      const body = Buffer.from(await (await get(`${base}/${name}`)).arrayBuffer());
      const got = createHash('sha256').update(body).digest('hex');
      if (got !== want) bad.push(`${name} (sha256 ${got})`);
    } catch (e) {
      bad.push(`${name} (${e.message})`);
    }
  }
  recordIf(bad.length === 0, `**checksums** — ${lines.length} files match SHA256SUMS`, `**checksums** — ${bad.join(', ')}`);
}

async function crateChecks(version) {
  for (const crate of CRATES) {
    // A raw fetch: a 404 is the answer we are looking for, not an error to throw on.
    try {
      const res = await fetch(`https://crates.io/api/v1/crates/${crate}/${version}`,
        { headers: UA, signal: AbortSignal.timeout(60000) });
      recordIf(res.status === 200, `**crates.io** — ${crate} ${version} is published`,
        `**crates.io** — ${crate} ${version} is missing (HTTP ${res.status})`);
    } catch (e) {
      record(false, `**crates.io** — ${crate} ${version}: ${e.message}`);
    }
  }
}

async function siteChecks(tag) {
  const pages = [
    [`${SITE}/`, `${SITE}/install.sh`,
      '**website** — colonizer.dev shows the install line',
      `**website** — colonizer.dev has no ${SITE}/install.sh install line`],
    [`${SITE}/docs/changelog`, tag,
      `**changelog** — colonizer.dev/docs/changelog names ${tag}`,
      `**changelog** — colonizer.dev/docs/changelog does not name ${tag}`],
  ];
  for (const [url, needle, ok, fail] of pages) {
    try {
      recordIf((await text(url)).includes(needle), ok, fail);
    } catch (e) {
      record(false, `${fail} (${e.message})`);
    }
  }
}

let tag = option('--tag');
if (!tag) {
  try {
    tag = (await get(`https://api.github.com/repos/${REPO}/releases/latest`, API).then((r) => r.json())).tag_name;
  } catch (e) {
    console.error(`could not resolve the latest release: ${e.message}`);
    process.exit(1);
  }
}
const version = tag.replace(/^v/, '');

console.log(`## Release health: ${tag}\n`);
await releaseChecks(tag);
await crateChecks(version);
await siteChecks(tag);
if (forceFailure) record(false, '**forced failure** — the workflow was run with force_failure');
for (const c of checks) console.log(`- ${c.ok ? '✅' : '❌'} ${c.text}`);

const failed = checks.filter((c) => !c.ok).length;
if (failed) console.error(`${failed} of ${checks.length} release-health checks failed for ${tag}`);
process.exit(failed ? 1 : 0);
