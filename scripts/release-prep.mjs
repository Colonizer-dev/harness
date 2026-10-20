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
//       fragment is left) and every release crate is at that version. When the checkout is a git
//       repository whose HEAD is the `release: vX.Y.Z` commit, a fragment that landed on main after
//       the release pull request assembled its own is not "left": it belongs to the next release
//       (see `laterFragments`).
//   node scripts/release-prep.mjs tags [--ref origin/main]
//       what the release train's tag job does with each merged `release: vX.Y.Z` commit since the
//       latest tag, oldest first: `tag` it when `verify` passes on it, `skip` it when it does not but
//       a newer release commit exists (that one carries its leftovers, so it supersedes it), `fail`
//       otherwise. Prints `<action> <version> <sha>` lines and exits 1 on a `fail`. Tags nothing.
//
// A crate may be one version ahead of the rest (colonizer-redact is, when a feature pull request bumped
// it to publish something new): prep leaves a crate already at the target alone, and only moves the
// ones still at the latest release. A crate anywhere else is an error, never a guess.
// Any command takes `--root <dir>` (default: the repository this script is in).
import { execFileSync, spawnSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, posix, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { FRAGMENT_DIR, REPO_URL, check, main as changelog, readFragments, releases } from './changelog.mjs';
import { check as checkDocLinks } from './check-doc-links.mjs';

// The crates that carry the release version, by directory under crates/. colonizer-billing and
// repo-contracts are versioned on their own.
export const RELEASE_CRATES = ['colonizer', 'colonizer-agentd', 'colonizer-observability', 'colonizer-redact'];
export const SKIP_MARKER = /release-train:\s*skip\b/i;

/**
 * Rebase relative Markdown links and image paths from the repository root to a target directory.
 * Inline links `[t](p)`, images `![a](p)`, and reference definitions `[id]: p` are rebased.
 * Absolute URLs (with scheme), protocol-relative `//`, root-absolute `/`, and pure anchors `#`
 * are left alone. Fragments `#`, query strings `?`, and optional titles are preserved.
 * Code inside fenced code blocks is not processed. Inline code spans are also protected.
 */
export function rebaseLinks(markdown, root, notesPath) {
  const notesDir = dirname(notesPath);

  // Check whether a path part should be rebased: not empty, not absolute, not external.
  const shouldRebase = (pathPart) =>
    !!pathPart && !/^[a-z][a-z0-9+.-]*:/i.test(pathPart) && !pathPart.startsWith('//') && !pathPart.startsWith('/');

  // Extract path, fragment, and query from a target.
  const splitTarget = (target) => {
    const end = target.search(/[?#]/);
    const cut = end === -1 ? target.length : end;
    return { pathPart: target.slice(0, cut), fragAndQuery: target.slice(cut) };
  };

  // Rebase a relative path from root to notes directory (always using `/` separators).
  const rebasePath = (pathPart) => {
    const absolute = resolve(root, pathPart);
    const absolutePosix = absolute.split('\\').join('/');
    const notesDirPosix = notesDir.split('\\').join('/');
    return posix.relative(notesDirPosix, absolutePosix);
  };

  const out = [];
  let fence = null;

  for (const line of markdown.split('\n')) {
    // Track fenced code blocks.
    const open = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (fence) {
      const close = line.match(/^\s{0,3}(`{3,}|~{3,})\s*$/);
      if (close && close[1][0] === fence[0] && close[1].length >= fence.length) fence = null;
      out.push(line);
      continue;
    }
    if (open) {
      fence = open[1];
      out.push(line);
      continue;
    }

    // Protect inline code spans, rebase links, restore.
    const codePlaceholders = [];
    let processed = line
      .replace(/(`+)[\s\S]*?\1/g, (m) => {
        codePlaceholders.push(m);
        return '\0' + (codePlaceholders.length - 1) + '\0';
      })
      // Rebase inline links/images.
      .replace(
        /(!?\[(?:[^\]\\]|\\.)*\])\(\s*(<[^>]*>|[^)\s]+)((?:\s+(?:"[^"]*"|'[^']*'))?)\s*\)/g,
        (match, linkPart, linkTarget, titlePart) => {
          // Don't rebase angle-bracket targets.
          if (linkTarget.startsWith('<')) return match;
          const { pathPart, fragAndQuery } = splitTarget(linkTarget);
          if (!shouldRebase(pathPart)) return match;
          return linkPart + '(' + rebasePath(pathPart) + fragAndQuery + (titlePart ?? '') + ')';
        }
      )
      // Rebase reference definitions.
      .replace(
        /^\s{0,3}\[[^\]]+\]:\s+(\S+)/,
        (match, refTarget) => {
          const { pathPart, fragAndQuery } = splitTarget(refTarget);
          if (!shouldRebase(pathPart)) return match;
          return match.replace(refTarget, rebasePath(pathPart) + fragAndQuery);
        }
      )
      // Restore inline code spans.
      .replace(/\0(\d+)\0/g, (_, i) => codePlaceholders[i]);
    out.push(processed);
  }
  return out.join('\n');
}

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
  if (!existsSync(notes)) {
    let stub = releaseNotesStub(version, p.fragments);
    stub = rebaseLinks(stub, root, notes);
    writeFileSync(notes, stub);
  }

  // Validate that the release notes have no broken links.
  const docLinksProblems = checkDocLinks(relative(root, notes), { root });
  if (docLinksProblems.length) {
    throw new Error(
      `release notes have broken links:\n${docLinksProblems.join('\n')}`
    );
  }

  return { changed: true, version, pending: p.pending, crates };
}

// ---------------------------------------------------------------------------------------------- git

function git(root, args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** A release commit's subject, `release: vX.Y.Z` with an optional ` (#123)`: the version, or null. */
export function releaseVersion(subject) {
  return /^release: (v\d+\.\d+\.\d+)( \(#\d+\))?$/.exec(subject)?.[1] ?? null;
}

/**
 * Which of the fragments still in `changelog.d/` at a release commit landed after the release pull
 * request assembled its own, from git history alone (the pull request was squashed, so its base is
 * gone from main). The release commit deleted the fragments it folded; `prep` folds every fragment
 * present, so the newest commit on main (first parent) that added one of those is at or before the
 * release branch's base, and any fragment-adding commit between it and the base would have been
 * folded too. A fragment left over that was added after that commit therefore landed during the
 * freeze and belongs to the next release; one added at or before it should have been folded.
 *
 * Returns `{ later: Set<file>, cutoff: sha | null }`. With nothing folded (a release that deleted no
 * fragment) nothing counts as later. `rev` is the release commit (default HEAD of `root`).
 */
export function laterFragments(root, rev = 'HEAD') {
  const dir = `${FRAGMENT_DIR}/`;
  const folded = git(root, ['diff', '--name-only', '--no-renames', '--diff-filter=D', `${rev}^`, rev, '--', dir])
    .split('\n')
    .filter(Boolean);
  const left = git(root, ['ls-tree', '--name-only', rev, dir])
    .split('\n')
    .filter(Boolean);
  // Newest first: the first time a path shows up is the latest commit that added it.
  const order = new Map();
  const addedAt = new Map();
  let at = -1;
  let sha = null;
  for (const line of git(root, ['log', '--first-parent', '--diff-filter=A', '--no-renames', '--name-only', '--format=%x00%H', `${rev}^`, '--', dir]).split('\n')) {
    if (line.startsWith('\0')) {
      sha = line.slice(1);
      order.set(sha, ++at);
    } else if (line && !addedAt.has(line)) addedAt.set(line, sha);
  }
  // A lower index is newer. The cutoff is the newest commit that added a fragment the release folded.
  let cutoff = null;
  for (const path of folded) {
    const c = addedAt.get(path);
    if (c && (cutoff === null || order.get(c) < order.get(cutoff))) cutoff = c;
  }
  const later = new Set();
  if (cutoff === null) return { later, cutoff };
  for (const path of left) {
    const c = addedAt.get(path);
    if (c && order.get(c) < order.get(cutoff)) later.add(path.slice(dir.length));
  }
  return { later, cutoff };
}

/** HEAD of `root` when it is the `release: <version>` commit of a git repository, else null. */
function releaseHead(root, version) {
  try {
    const [sha, subject] = git(root, ['log', '-1', '--format=%H%x09%s', 'HEAD']).trim().split('\t');
    return releaseVersion(subject) === version ? sha : null;
  } catch {
    return null;
  }
}

/** Why `version` may not be tagged at `root`, as a list; empty means go. */
export function verify(root, version, { notes = [] } = {}) {
  if (!parse(version)) return [`"${version}" is not a vX.Y.Z version`];
  const problems = [];
  let later = new Set();
  const head = releaseHead(root, version);
  if (head) {
    try {
      later = laterFragments(root, head).later;
    } catch (e) {
      problems.push(`could not read the release's git history: ${e.message}`);
    }
  }
  try {
    const result = check(root, { release: version, later });
    problems.push(...result.errors);
    notes.push(...result.warnings);
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

/** The merged `release: vX.Y.Z` commits after the latest `vX.Y.Z` tag on `ref`, oldest first, as `{ sha, version }`. */
export function untaggedReleases(root, ref = 'origin/main') {
  const last = git(root, ['tag', '--list', 'v[0-9]*.[0-9]*.[0-9]*', '--sort=-v:refname'])
    .split('\n')
    .find(Boolean);
  const range = last ? `${last}..${ref}` : ref;
  return git(root, ['log', '--first-parent', '--reverse', '--format=%H%x09%s', range])
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      const [sha, subject] = line.split('\t');
      return { sha, version: releaseVersion(subject) };
    })
    .filter((r) => r.version);
}

/**
 * What the tag job does with each untagged release, oldest first: `{ sha, version, action, problems }`
 * where action is `tagged` (already), `tag`, `skip` (fails the gate, but a newer release commit
 * supersedes it) or `fail` (fails the gate and nothing newer exists). `check(release)` returns the
 * gate's problems; `isTagged(version)` says whether the tag exists.
 */
export function decideTags(list, check, isTagged = () => false) {
  return list.map((r, i) => {
    if (isTagged(r.version)) return { ...r, action: 'tagged', problems: [] };
    const problems = check(r);
    if (!problems.length) return { ...r, action: 'tag', problems };
    const newer = list.slice(i + 1).find((n) => !isTagged(n.version));
    return { ...r, action: newer ? 'skip' : 'fail', problems, supersededBy: newer?.version };
  });
}

/** `verify` on the release commit `sha`, checked out in a throwaway worktree of `root`. */
export function verifyAt(root, sha, version, { notes = [] } = {}) {
  const at = join(mkdtempSync(join(tmpdir(), 'release-gate-')), 'at');
  git(root, ['worktree', 'add', '--detach', at, sha]);
  try {
    return verify(at, version, { notes });
  } finally {
    git(root, ['worktree', 'remove', '--force', at]);
    rmSync(dirname(at), { recursive: true, force: true });
  }
}

function annotate(kind, message) {
  if (process.env.GITHUB_ACTIONS) console.error(`::${kind}::${message}`);
  else console.error(`${kind}: ${message}`);
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
  release-prep.mjs tags [--ref <rev>]
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
    if (['--message', '--date', '--root', '--ref'].includes(a)) {
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
      const notes = [];
      const problems = verify(root, positional[0], { notes });
      for (const n of notes) console.error(`notice: ${n}`);
      for (const p of problems) console.error(`error: ${p}`);
      if (problems.length) return 1;
      console.log(`${positional[0]} is ready to tag`);
      return 0;
    }
    if (command === 'tags') {
      const list = untaggedReleases(root, flags.ref ?? 'origin/main');
      const tagged = (v) => {
        try {
          git(root, ['rev-parse', '-q', '--verify', `refs/tags/${v}`]);
          return true;
        } catch {
          return false;
        }
      };
      const decisions = decideTags(
        list,
        (r) => {
          const notes = [];
          const problems = verifyAt(root, r.sha, r.version, { notes });
          for (const n of notes) annotate('notice', `${r.version}: ${n}`);
          return problems;
        },
        tagged,
      );
      for (const d of decisions) {
        for (const p of d.problems) annotate(d.action === 'fail' ? 'error' : 'notice', `${d.version}: ${p}`);
        if (d.action === 'skip') {
          annotate('notice', `${d.version} at ${d.sha} is not tagged: it fails the gate and ${d.supersededBy} supersedes it`);
        } else if (d.action === 'fail') {
          annotate('error', `${d.version} at ${d.sha} is not ready to tag; see above. Fix it on main, then run the train again`);
        }
        console.log(`${d.action} ${d.version} ${d.sha}`);
      }
      return decisions.some((d) => d.action === 'fail') ? 1 : 0;
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
