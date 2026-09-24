#!/usr/bin/env node
// Proposes new pins for the vendored Claude Code plugins and prompt texts (caveman) in vendor/vendor.lock,
// described in skill terms.
//
//   node scripts/update-vendored-plugins.mjs                     report what moved upstream (changes nothing)
//   node scripts/update-vendored-plugins.mjs --write             also rewrite vendor/vendor.lock
//   node scripts/update-vendored-plugins.mjs --summary out.md    write the report as Markdown (a PR or issue body)
//   node scripts/update-vendored-plugins.mjs --lock path         use another lock file (for testing)
//
// A pin follows its upstream the way it was pinned: a codeload `refs/tags/<tag>` URL follows the latest
// GitHub release, and a codeload `<commit>` URL follows the default branch. The report lists skills added,
// removed and changed between the pinned archive and the new one, so a reviewer sees what an agent would
// now be told to do, not just a new hash. Nothing here decides to trust an update on its own: the new
// archive has to pass skill-pack validation (below) for the pin to be rewritten, scripts/fetch-vendor.sh
// still has to stage it, and a person merges it.
//
// Uses GH_TOKEN or GITHUB_TOKEN when set (a higher API rate limit); works without one.

import { validatePack } from './validate-plugins.mjs';

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const lockPath = option('--lock') ?? join(root, 'vendor/vendor.lock');
const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;

const CODELOAD = /^https:\/\/codeload\.github\.com\/([^/]+)\/([^/]+)\/tar\.gz\/(refs\/tags\/)?(.+)$/;

async function github(path) {
  const headers = { accept: 'application/vnd.github+json', 'user-agent': 'colonizer-vendor-updates' };
  if (token) headers.authorization = `Bearer ${token}`;
  const response = await fetch(`https://api.github.com${path}`, { headers });
  if (!response.ok) throw new Error(`GitHub ${path}: HTTP ${response.status}`);
  return response.json();
}

async function download(url) {
  const response = await fetch(url, { headers: { 'user-agent': 'colonizer-vendor-updates' } });
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

const sha256 = (data) => createHash('sha256').update(data).digest('hex');

/** An archive extracted to a temp dir: `root` is its single top-level entry, `dir` what to remove. */
function extract(archive) {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-vendor-'));
  writeFileSync(join(dir, 'archive.tgz'), archive);
  execFileSync('tar', ['-xzf', 'archive.tgz'], { cwd: dir });
  const top = readdirSync(dir).find((name) => name !== 'archive.tgz');
  return { dir, root: join(dir, top) };
}

/** Every skill in an archive, as `<dir under skills/>` → a hash of all its files. */
function skillsIn(archive) {
  const { dir, root } = extract(archive);
  try {
    const base = join(root, 'skills');
    const skills = new Map();
    const walk = (current) => {
      let entries;
      try {
        entries = readdirSync(current, { withFileTypes: true });
      } catch {
        return;
      }
      if (entries.some((e) => e.isFile() && e.name === 'SKILL.md')) {
        const hash = createHash('sha256');
        const files = [];
        const collect = (d) => {
          for (const e of readdirSync(d, { withFileTypes: true })) {
            const p = join(d, e.name);
            if (e.isDirectory()) collect(p);
            else if (e.isFile()) files.push(p);
          }
        };
        collect(current);
        for (const file of files.sort()) hash.update(relative(current, file)).update('\0').update(readFileSync(file));
        skills.set(relative(base, current), hash.digest('hex'));
        return;
      }
      for (const e of entries) if (e.isDirectory()) walk(join(current, e.name));
    };
    walk(base);
    return skills;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function diffSkills(before, after) {
  const added = [...after.keys()].filter((k) => !before.has(k)).sort();
  const removed = [...before.keys()].filter((k) => !after.has(k)).sort();
  const changed = [...after.keys()].filter((k) => before.has(k) && before.get(k) !== after.get(k)).sort();
  return { added, removed, changed, total: after.size };
}

// The plugin pins scripts/fetch-vendor.sh stages into dist/plugins/<name> straight from the archive's
// top-level directory, so that directory is the pack a colony would mount. Not here: google-skills,
// whose staged pack is synthesized at stage time (a generated manifest, Colonizer's finder, a rewritten
// catalog) and so has no pack in the archive to validate — CI validates the staged copy instead — and
// pins of other kinds (caveman's prompt text, fast-jev-compaction's hook), which stage no pack at all.
const ARCHIVE_STAGED_PACKS = new Set(['ecc', 'superpowers']);

/** validatePack on the pack a pin's archive stages into dist/plugins, or null for a pin that stages no
 * pack from its archive. The temp extraction is gone by the time this returns. */
export function validateArchivePack(name, kind, archive) {
  if (kind !== 'plugin' || !ARCHIVE_STAGED_PACKS.has(name)) return null;
  const { dir, root } = extract(archive);
  try {
    return validatePack(root);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The newest upstream pin for a lock entry, or null when it is already current. */
async function latest(entry) {
  const match = CODELOAD.exec(entry.url);
  if (!match) return null;
  const [, owner, repo, tagged, ref] = match;
  if (tagged) {
    const release = await github(`/repos/${owner}/${repo}/releases/latest`);
    if (release.tag_name === ref) return null;
    return {
      version: release.tag_name.replace(/^v/, ''),
      url: `https://codeload.github.com/${owner}/${repo}/tar.gz/refs/tags/${release.tag_name}`,
      label: release.tag_name,
      from: ref,
      link: release.html_url,
      compare: `https://github.com/${owner}/${repo}/compare/${ref}...${release.tag_name}`,
    };
  }
  const info = await github(`/repos/${owner}/${repo}`);
  const head = await github(`/repos/${owner}/${repo}/commits/${info.default_branch}`);
  if (head.sha === ref) return null;
  const date = head.commit.committer.date.slice(0, 10).replaceAll('-', '.');
  return {
    version: `${date}-${head.sha.slice(0, 7)}`,
    url: `https://codeload.github.com/${owner}/${repo}/tar.gz/${head.sha}`,
    label: head.sha.slice(0, 7),
    from: ref.slice(0, 7),
    link: head.html_url,
    compare: `https://github.com/${owner}/${repo}/compare/${ref}...${head.sha}`,
  };
}

function parseLock(text) {
  const lines = text.split('\n');
  const entries = [];
  lines.forEach((line, index) => {
    if (!line.trim() || line.trimStart().startsWith('#')) return;
    const [name, version, platform, kind, sha, url] = line.trim().split(/\s+/);
    if (kind === 'plugin' || kind === 'prompt') entries.push({ index, name, version, platform, kind, sha, url });
  });
  return { lines, entries };
}

/** The lock line with the new pin, and the comment block above it with the old version strings replaced. */
function applyUpdate(lines, entry, next, newSha) {
  lines[entry.index] = lines[entry.index]
    .replace(entry.version, next.version)
    .replace(entry.sha, newSha)
    .replace(entry.url, next.url);
  const oldShort = CODELOAD.exec(entry.url)[4].replace(/^refs\/tags\//, '');
  const oldDate = /^(\d{4})\.(\d{2})\.(\d{2})-/.exec(entry.version);
  for (let i = entry.index - 1; i >= 0 && lines[i].trimStart().startsWith('#'); i--) {
    let line = lines[i].replaceAll(oldShort, next.label).replaceAll(oldShort.slice(0, 7), next.label.slice(0, 7));
    if (oldDate) {
      const newDate = /^(\d{4})\.(\d{2})\.(\d{2})-/.exec(next.version);
      line = line.replaceAll(`${oldDate[1]}-${oldDate[2]}-${oldDate[3]}`, `${newDate[1]}-${newDate[2]}-${newDate[3]}`);
    }
    lines[i] = line;
  }
}

function section(entry, next, diff) {
  const list = (title, items) =>
    items.length ? `\n**${title}** (${items.length}): ${items.map((s) => `\`${s}\``).join(', ')}\n` : '';
  return [
    `### \`${entry.name}\`: ${next.from} → [${next.label}](${next.link})`,
    '',
    `${diff.total} skills after the update. [Upstream diff](${next.compare}).`,
    list('Added', diff.added) + list('Removed', diff.removed) + list('Changed', diff.changed) ||
      '\nNo skill changed; the update touches other files.\n',
  ].join('\n');
}

/** A summary block for an update not adopted: what moved, and which rules the new archive broke. */
function skippedSection(entry, next, errors) {
  return [
    `### \`${entry.name}\`: skipped, stays at ${entry.version}`,
    '',
    `[${next.label}](${next.link}) fails skill-pack validation, so the lock line was not rewritten. [Upstream diff](${next.compare}).`,
    ...errors.map((e) => `- \`${e.file}\` [${e.rule}] ${e.message}`),
  ].join('\n');
}

async function main() {
  const text = readFileSync(lockPath, 'utf8');
  const { lines, entries } = parseLock(text);
  const sections = [];
  const skipped = [];
  for (const entry of entries) {
    const next = await latest(entry);
    if (!next) {
      console.log(`${entry.name}: current (${entry.version})`);
      continue;
    }
    console.log(`${entry.name}: ${entry.version} -> ${next.version}`);
    const [before, after] = await Promise.all([download(entry.url), download(next.url)]);
    if (sha256(before) !== entry.sha) {
      console.log(`  note: the pinned archive no longer matches its sha256; GitHub regenerated it`);
    }
    const diff = diffSkills(skillsIn(before), skillsIn(after));
    console.log(`  skills: +${diff.added.length} -${diff.removed.length} ~${diff.changed.length} (${diff.total} total)`);
    const pack = validateArchivePack(entry.name, entry.kind, after);
    if (pack && !pack.ok) {
      console.log('  skipped: the new archive fails skill-pack validation');
      for (const e of pack.errors) console.log(`    ${e.file} [${e.rule}] ${e.message}`);
      skipped.push(skippedSection(entry, next, pack.errors));
      continue;
    }
    applyUpdate(lines, entry, next, sha256(after));
    sections.push(section(entry, next, diff));
  }

  const changed = sections.length > 0;
  if (changed && flag('--write')) {
    writeFileSync(lockPath, lines.join('\n'));
    console.log(`wrote ${relative(process.cwd(), lockPath)}`);
  }
  const summary = option('--summary');
  if (summary) {
    mkdirSync(dirname(summary), { recursive: true });
    writeFileSync(
      summary,
      changed || skipped.length > 0
        ? [
            changed
              ? 'Upstream moved for these vendored plugins. `vendor/vendor.lock` below pins the new archives, and `scripts/fetch-vendor.sh` staged them with its checks (no hooks, no MCP servers, the Google finder still local).'
              : 'Upstream moved, but every proposed pin below failed skill-pack validation, so `vendor/vendor.lock` is unchanged.',
            '',
            'These skills are instructions to an agent that can push: read what changed before merging.',
            '',
            ...sections,
            ...skipped,
            '',
            '_Opened by `.github/workflows/vendored-plugin-updates.yml`._',
          ].join('\n')
        : 'Every vendored plugin is at its upstream pin.\n',
    );
  }
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `changed=${changed}\n`);
  if (!changed) {
    if (skipped.length > 0) {
      // Red, not "no updates": upstream moved, and every proposal was refused. The workflow's
      // happy paths must not report the plugins as current when they are not.
      console.error('every proposed update failed skill-pack validation; the lock is unchanged');
      process.exitCode = 1;
    } else {
      console.log('no updates');
    }
  }
}

const invoked = process.argv[1] && import.meta.url.endsWith(encodeURI(process.argv[1].split('/').pop()));
if (invoked) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
