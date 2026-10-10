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
//
// The site fetch is retried and reports separately: a page we never managed to read is a fetch
// failure, never a content failure.

import { createHash } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const REPO = 'Colonizer-dev/harness';
const SITE = 'https://colonizer.dev';
const CRATES = ['colonizer-harness', 'colonizer-agentd'];
// Keep in step with the assets release.yml attaches, Colonizer-arm64.dmg included (#1138).
export const ASSETS = ['colonizer-linux-x86_64.tar.gz', 'colonizer-darwin-arm64.tar.gz', 'Colonizer-arm64.dmg', 'install.sh', 'SHA256SUMS'];
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
async function get(url, headers = UA, fetchImpl = fetch) {
  const res = await fetchImpl(url, { headers, signal: AbortSignal.timeout(600000) });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res;
}
const text = async (url, fetchImpl = fetch) => (await get(url, UA, fetchImpl)).text();

// The GitHub half of the checks, returned like siteChecks' results so the test can drive them
// offline. Records nothing into the module's own `checks`, which belongs to a whole run.
export async function releaseChecks(tag, { fetchImpl = fetch } = {}) {
  const records = [];
  const record = (ok, text) => records.push({ ok, text });
  const recordIf = (ok, onOk, onFail) => record(ok, ok ? onOk : onFail);

  let release;
  try {
    release = await get(`https://api.github.com/repos/${REPO}/releases/tags/${tag}`, API, fetchImpl).then((r) => r.json());
  } catch (e) {
    record(false, `**release** — no GitHub release for ${tag} (${e.message})`);
    return records;
  }
  const present = new Set(release.assets.map((a) => a.name));
  const missing = ASSETS.filter((name) => !present.has(name));
  recordIf(missing.length === 0, `**release** — all ${ASSETS.length} assets present`,
    `**release** — missing ${missing.join(', ')}`);
  if (missing.length) return records;

  const base = `https://github.com/${REPO}/releases/download/${tag}`;
  let sums;
  try {
    sums = await text(`${base}/SHA256SUMS`, fetchImpl);
  } catch (e) {
    record(false, `**checksums** — could not read SHA256SUMS (${e.message})`);
    return records;
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
      const body = Buffer.from(await (await get(`${base}/${name}`, UA, fetchImpl)).arrayBuffer());
      const got = createHash('sha256').update(body).digest('hex');
      if (got !== want) bad.push(`${name} (sha256 ${got})`);
    } catch (e) {
      bad.push(`${name} (${e.message})`);
    }
  }
  recordIf(bad.length === 0, `**checksums** — ${lines.length} files match SHA256SUMS`, `**checksums** — ${bad.join(', ')}`);
  return records;
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

// colonizer.dev is behind Cloudflare, which answers an occasional single request with a 403 that
// the next one does not. Retrying a transient status costs a couple of seconds against a run that
// already waits out ten minutes of asset downloads; a 404 is an answer, not a hiccup, so it is
// never retried.
const SITE_ATTEMPTS = 3;
const SITE_BACKOFF_MS = 400;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const isTransient = (status) => status === 403 || status === 429 || status >= 500;

export async function fetchSiteText(url, { attempts = SITE_ATTEMPTS, fetchImpl = fetch, delay = sleep } = {}) {
  let last;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    let status;
    try {
      const res = await fetchImpl(url, { headers: UA, signal: AbortSignal.timeout(600000) });
      if (res.ok) return await res.text();
      status = res.status;
      last = new Error(`HTTP ${res.status}`);
    } catch (e) {
      last = e;
    }
    if (status !== undefined && !isTransient(status)) throw last;
    if (attempt < attempts) await delay(SITE_BACKOFF_MS * attempt);
  }
  throw last;
}

export async function siteChecks(tag, { fetchSite = fetchSiteText } = {}) {
  const pages = [
    ['website', `${SITE}/`, `${SITE}/install.sh`,
      '**website** — colonizer.dev shows the install line',
      `**website** — colonizer.dev has no ${SITE}/install.sh install line`],
    ['changelog', `${SITE}/docs/changelog`, tag,
      `**changelog** — colonizer.dev/docs/changelog names ${tag}`,
      `**changelog** — colonizer.dev/docs/changelog does not name ${tag}`],
  ];
  const results = [];
  for (const [name, url, needle, ok, fail] of pages) {
    let body;
    try {
      body = await fetchSite(url);
    } catch (e) {
      // The page was never read, so its content says nothing either way: claiming it "has no
      // install line" here is what made a passing check red for nine releases.
      results.push({ ok: false, text: `**${name}** — could not read ${url} (${e.message})` });
      continue;
    }
    const found = body.includes(needle);
    results.push({ ok: found, text: found ? ok : fail });
  }
  return results;
}

async function main() {
  let tag = option('--tag');
  if (!tag) {
    try {
      tag = (await get(`https://api.github.com/repos/${REPO}/releases/latest`, API).then((r) => r.json())).tag_name;
    } catch (e) {
      console.error(`could not resolve the latest release: ${e.message}`);
      return 1;
    }
  }
  const version = tag.replace(/^v/, '');

  console.log(`## Release health: ${tag}\n`);
  checks.push(...await releaseChecks(tag));
  await crateChecks(version);
  checks.push(...await siteChecks(tag));
  if (forceFailure) record(false, '**forced failure** — the workflow was run with force_failure');
  for (const c of checks) console.log(`- ${c.ok ? '✅' : '❌'} ${c.text}`);

  const failed = checks.filter((c) => !c.ok).length;
  if (failed) console.error(`${failed} of ${checks.length} release-health checks failed for ${tag}`);
  return failed ? 1 : 0;
}

// Only when run, not when its checks are imported by a test. The exit code is set rather than
// `process.exit`-ed, so a redirected stdout is not cut off.
//
// The comparison is on realpaths: `import.meta.url` is Node's resolved module path, symlinks and
// all, while a lexical normalisation of argv[1] would miss a symlinked invocation. That miss is
// not a harmless no-op — main() would never run, and a health check that exits 0 having checked
// nothing is the worst thing it could do. A path that does not exist (node -e, a stale argv[1])
// falls through to "not invoked" rather than throwing.
let invoked = false;
try {
  invoked = Boolean(process.argv[1]) && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url);
} catch {
  invoked = false;
}
if (invoked) {
  process.exitCode = await main();
}
