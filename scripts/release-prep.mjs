#!/usr/bin/env node
// The mechanical half of a release, so the release train (.github/workflows/release-train.yml) and a
// person cut one the same way. What the release PRs #1151, #1159 and #1187 did by hand:
//
//   node scripts/release-prep.mjs plan [--message <head commit message>]
//       pending fragments, the version the next release would have, and whether
//       `release-train: skip` postpones it. Prints key=value lines (and appends them to
//       $GITHUB_OUTPUT when that is set). Changes nothing.
//   node scripts/release-prep.mjs prep [vX.Y.Z] [--date <YYYY-MM-DD>] [--no-lock] [--dry-run]
//       assembles the changelog, bumps the release crates, refreshes Cargo.lock and writes the
//       release-notes stub. No fragment pending: does nothing and says so. The version defaults to the
//       next patch of the latest release in CHANGELOG.md, and may be nothing but that: the train never
//       makes a major or minor bump.
//   node scripts/release-prep.mjs verify vX.Y.Z
//       the gate before a tag: `changelog.mjs check --release` is clean (the section exists, no
//       fragment is left) and every release crate is at that version.
//
// A crate may be one version ahead of the rest (colonizer-redact is, when a feature pull request bumped
// it to publish something new): prep leaves a crate already at the target alone, and only moves the
// ones still at the latest release. A crate anywhere else is an error, never a guess.
// Any command takes `--root <dir>` (default: the repository this script is in).
import { spawnSync } from 'node:child_process';
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { FRAGMENT_DIR, REPO_URL, check, main as changelog, readFragments, releases } from './changelog.mjs';

// The crates that carry the release version, by directory under crates/. colonizer-billing and
// repo-contracts are versioned on their own.
export const RELEASE_CRATES = ['colonizer', 'colonizer-agentd', 'colonizer-observability', 'colonizer-redact'];
export const SKIP_MARKER = /release-train:\s*skip\b/i;

const SEMVER = /^v(\d+)\.(\d+)\.(\d+)$/;
const parse = (v) => SEMVER.exec(v)?.slice(1).map(Number) ?? null;
const cmp = (a, b) => {
  const [x, y] = [parse(a), parse(b)];
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i];
  return 0;
};

export function nextPatch(version) {
  const [a, b, c] = parse(version);
  return `v${a}.${b}.${c + 1}`;
}

/** The newest `## [vX.Y.Z]` release in CHANGELOG.md text, or null. */
export function latestRelease(changelogText) {
  const all = releases(changelogText);
  return all.length ? all.reduce((a, b) => (cmp(a, b) >= 0 ? a : b)) : null;
}

const manifest = (root, crate) => join(root, 'crates', crate, 'Cargo.toml');

/** The line index of the `version = "x.y.z"` in a manifest's `[package]` table, or -1. */
function versionLine(lines) {
  let inPackage = false;
  for (let i = 0; i < lines.length; i++) {
    if (/^\[.*\]\s*$/.test(lines[i])) inPackage = lines[i].trim() === '[package]';
    else if (inPackage && /^version = "\d+\.\d+\.\d+"\s*$/.test(lines[i])) return i;
  }
  return -1;
}

/** The `version` of a manifest's `[package]`, as `vX.Y.Z`. */
export function packageVersion(toml) {
  const lines = toml.split('\n');
  const at = versionLine(lines);
  return at < 0 ? null : `v${/"(.*)"/.exec(lines[at])[1]}`;
}

/** The manifest with its `[package]` version replaced. */
export function setPackageVersion(toml, version) {
  const lines = toml.split('\n');
  const at = versionLine(lines);
  if (at < 0) throw new Error('no [package] version to set');
  lines[at] = `version = "${version.slice(1)}"`;
  return lines.join('\n');
}

/** The manifest with every path dependency on a release crate asking for that crate's version. */
export function setPathDependencyVersions(toml, versions) {
  return toml.replace(
    /^(([a-z0-9-]+) = \{ version = ")(\d+\.\d+\.\d+)(", path = "[^"]*")/gm,
    (whole, head, name, _old, tail) => (versions[name] ? `${head}${versions[name].slice(1)}${tail}` : whole),
  );
}

/**
 * What a release at `version` does to each crate, or throws when a crate is somewhere it should not
 * be. `versions` is `{ crate: 'vX.Y.Z' }`.
 */
export function planCrates(versions, current, version) {
  return RELEASE_CRATES.map((name) => {
    const at = versions[name];
    if (!at) throw new Error(`crates/${name}/Cargo.toml has no [package] version`);
    if (at === version) return { name, from: at, to: version, action: 'ahead' };
    if (at === current) return { name, from: at, to: version, action: 'bump' };
    throw new Error(
      `crates/${name} is at ${at}, which is neither the latest release (${current}) nor the target (${version}); ` +
        'fix its version by hand before the train runs',
    );
  });
}

/** Pending fragments, the next version and whether a commit message postpones the release. */
export function plan(root, { message = '' } = {}) {
  const changelogPath = join(root, 'CHANGELOG.md');
  const current = latestRelease(existsSync(changelogPath) ? readFileSync(changelogPath, 'utf8') : '');
  if (!current) throw new Error('CHANGELOG.md has no release to count from');
  const { fragments, errors } = readFragments(join(root, FRAGMENT_DIR));
  if (errors.length) throw new Error(errors.map((e) => `${FRAGMENT_DIR}/${e}`).join('\n'));
  return {
    pending: fragments.length,
    current,
    next: nextPatch(current),
    skip: SKIP_MARKER.test(message),
    fragments,
  };
}

/** The first line of a fragment that reads as its summary: not a comment or a notice line. */
function summary(text) {
  const body = text.replace(/<!--[\s\S]*?-->/g, '');
  const line = body
    .split('\n')
    .map((l) => l.replace(/^-\s+/, '').trim())
    .find((l) => l && !/^(critical|fixes-running|probe):/i.test(l));
  return (line ?? '').replace(/\[#(\d+)\](?![:(])/g, `[#$1](${REPO_URL}/issues/$1)`);
}

/** The release-notes stub: the fragments' summaries, for a person to rewrite before the tag. */
export function releaseNotesStub(version, fragments) {
  const entries = fragments.map((f) => `- ${summary(f.text)}`).join('\n');
  return `<!--
Prepended by .github/workflows/release.yml to the ${version} release body, above the generic
install and provenance text. The release train wrote this list from the changelog.d/ fragments the
release folded in; reword it for a reader before the release goes out if it matters.
-->
# Colonizer ${version}

## What's new

${entries}
`;
}

/** Cuts the release in the working tree. Returns what it did; `changed` is false when nothing was pending. */
export function prep(root, { version, date, lock = true, dryRun = false } = {}) {
  const p = plan(root);
  if (!p.pending) return { changed: false, version: version ?? p.next, pending: 0 };
  version ??= p.next;
  if (!parse(version)) throw new Error(`"${version}" is not a vX.Y.Z version`);
  if (version !== p.next) {
    throw new Error(`the release train only bumps the patch: ${p.current} is followed by ${p.next}, not ${version}`);
  }
  const versions = {};
  for (const name of RELEASE_CRATES) {
    const path = manifest(root, name);
    if (!existsSync(path)) throw new Error(`${path} is missing`);
    versions[name] = packageVersion(readFileSync(path, 'utf8'));
  }
  const crates = planCrates(versions, p.current, version);
  if (dryRun) return { changed: false, dryRun: true, version, pending: p.pending, crates };

  const args = ['assemble', '--version', version, '--root', root];
  if (date) args.push('--date', date);
  const code = changelog(args);
  if (code !== 0) throw new Error(`changelog.mjs assemble exited ${code}`);

  const finalVersions = Object.fromEntries(crates.map((c) => [c.name, c.to]));
  for (const name of RELEASE_CRATES) {
    const path = manifest(root, name);
    let toml = readFileSync(path, 'utf8');
    toml = setPackageVersion(toml, finalVersions[name]);
    toml = setPathDependencyVersions(toml, finalVersions);
    writeFileSync(path, toml);
  }
  if (lock) {
    const cargo = process.env.CARGO ?? 'cargo';
    // Offline first (a laptop with a warm registry cache); a fresh CI runner has no cache, so
    // fall back to the network. `-w` only rewrites the workspace's own versions either way.
    let r = spawnSync(cargo, ['update', '-w', '--offline'], { cwd: root, stdio: 'inherit' });
    if (r.status !== 0) r = spawnSync(cargo, ['update', '-w'], { cwd: root, stdio: 'inherit' });
    if (r.status !== 0) throw new Error(`cargo update -w failed (${r.status ?? r.error})`);
  }
  const notes = join(root, 'docs', 'release-notes', `${version}.md`);
  if (!existsSync(notes)) writeFileSync(notes, releaseNotesStub(version, p.fragments));
  return { changed: true, version, pending: p.pending, crates };
}

/** Why `version` may not be tagged at `root`, as a list; empty means go. */
export function verify(root, version) {
  if (!parse(version)) return [`"${version}" is not a vX.Y.Z version`];
  const problems = [];
  try {
    problems.push(...check(root, { release: version }).errors);
  } catch (e) {
    problems.push(`changelog check failed: ${e.message}`);
  }
  for (const name of RELEASE_CRATES) {
    const path = manifest(root, name);
    const at = existsSync(path) ? packageVersion(readFileSync(path, 'utf8')) : null;
    if (at !== version) problems.push(`crates/${name}/Cargo.toml is at ${at ?? 'no version'}, not ${version}`);
  }
  return problems;
}

function emit(pairs) {
  for (const [k, v] of Object.entries(pairs)) console.log(`${k}=${v}`);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(pairs).map(([k, v]) => `${k}=${v}\n`).join(''));
  }
}

const USAGE = `usage:
  release-prep.mjs plan [--message <commit message>]
  release-prep.mjs prep [vX.Y.Z] [--date <YYYY-MM-DD>] [--no-lock] [--dry-run]
  release-prep.mjs verify vX.Y.Z
  (any command: [--root <dir>])`;

function fail(message) {
  console.error(message);
  return 2;
}

export function main(argv = process.argv.slice(2)) {
  const [command, ...rest] = argv;
  const flags = {};
  const positional = [];
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i];
    if (['--message', '--date', '--root'].includes(a)) {
      if (rest[i + 1] === undefined) return fail(`${a} needs a value\n${USAGE}`);
      flags[a.slice(2)] = rest[++i];
    } else if (['--no-lock', '--dry-run'].includes(a)) flags[a.slice(2)] = true;
    else if (a.startsWith('--')) return fail(`unknown option ${a}\n${USAGE}`);
    else positional.push(a);
  }
  const root = resolve(flags.root ?? join(dirname(fileURLToPath(import.meta.url)), '..'));
  try {
    if (command === 'plan') {
      const p = plan(root, { message: flags.message ?? '' });
      emit({ pending: p.pending, current: p.current, next: p.next, skip: p.skip });
      return 0;
    }
    if (command === 'prep') {
      const r = prep(root, {
        version: positional[0],
        date: flags.date,
        lock: !flags['no-lock'],
        dryRun: !!flags['dry-run'],
      });
      if (!r.changed && !r.dryRun) {
        console.log('no changelog.d/ fragment is pending: nothing to release');
        emit({ changed: false, version: r.version });
        return 0;
      }
      for (const c of r.crates) {
        console.log(`${c.name}: ${c.action === 'ahead' ? `already ${c.to}, left alone` : `${c.from} -> ${c.to}`}`);
      }
      emit({ changed: r.changed, version: r.version });
      return 0;
    }
    if (command === 'verify') {
      if (!positional[0]) return fail(USAGE);
      const problems = verify(root, positional[0]);
      for (const p of problems) console.error(`error: ${p}`);
      if (problems.length) return 1;
      console.log(`${positional[0]} is ready to tag`);
      return 0;
    }
  } catch (e) {
    console.error(e.message);
    return 1;
  }
  return fail(USAGE);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    process.exit(main());
  } catch (e) {
    console.error(`release-prep.mjs failed unexpectedly: ${e.stack ?? e}`);
    process.exit(1);
  }
}
