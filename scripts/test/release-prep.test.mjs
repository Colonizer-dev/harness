// scripts/release-prep.mjs: the release train's mechanical half, run against fixture workspaces. A
// fixture is a throwaway tree with a CHANGELOG.md, fragments and the four release crates' manifests,
// so the bump, the crate-ahead case, the no-op and the tag gate are all exercised without cargo
// (`--no-lock`) or a network.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  RELEASE_CRATES,
  decideTags,
  laterFragments,
  latestRelease,
  main,
  nextPatch,
  packageVersion,
  plan,
  planCrates,
  prep,
  releaseNotesStub,
  releaseVersion,
  setPackageVersion,
  setPathDependencyVersions,
  verify,
} from '../release-prep.mjs';

const scratch = mkdtempSync(join(tmpdir(), 'release-prep-'));
after(() => rmSync(scratch, { recursive: true, force: true }));
let n = 0;

const HEADER = `# Changelog

What changed in each release of Colonizer.

## Unreleased

Entries for the next release are not written here. Each pull request adds its own file under
[\`changelog.d/\`](changelog.d/README.md), and cutting a release folds them in with
\`node scripts/changelog.mjs assemble\`, so parallel pull requests never collide in this file.

## [v0.2.10] - 2026-10-06

### Fixed

- **An old fix.** It was fixed. ([#1])

[#1]: https://github.com/Colonizer-dev/harness/issues/1
[v0.2.10]: https://github.com/Colonizer-dev/harness/releases/tag/v0.2.10
`;

function manifestText(name, version, extra = '') {
  return `[package]
name = "${name}"
# a comment that mentions [brackets]-free text
version = "${version}"
edition = "2024"

[dependencies]
${extra}serde = { version = "1.0.229", features = ["derive"] }
`;
}

/** A workspace at v0.2.10; `versions` overrides a crate's version, `fragments` is `{ file: text }`. */
function fixture({ versions = {}, fragments = {}, changelog = HEADER } = {}) {
  const root = join(scratch, `ws-${n++}`);
  mkdirSync(join(root, 'changelog.d'), { recursive: true });
  mkdirSync(join(root, 'docs', 'release-notes'), { recursive: true });
  writeFileSync(join(root, 'CHANGELOG.md'), changelog);
  writeFileSync(join(root, 'changelog.d', 'README.md'), '# Fragments\n');
  for (const [file, text] of Object.entries(fragments)) writeFileSync(join(root, 'changelog.d', file), text);
  for (const name of RELEASE_CRATES) {
    const dep = name === 'colonizer' || name === 'colonizer-observability'
      ? `colonizer-redact = { version = "${(versions['colonizer-redact'] ?? '0.2.10')}", path = "../colonizer-redact" }\n`
      : '';
    mkdirSync(join(root, 'crates', name), { recursive: true });
    writeFileSync(join(root, 'crates', name, 'Cargo.toml'), manifestText(name, versions[name] ?? '0.2.10', dep));
  }
  return root;
}

const toml = (root, name) => readFileSync(join(root, 'crates', name, 'Cargo.toml'), 'utf8');
const FRAGMENTS = {
  '1200.fixed.md': '**A thing is fixed.** It was broken, now it is not. ([#1200])\n',
  'cockpit-pager.added.md': '<!-- note -->\n- **Lists page ten at a time.** Useful. ([#1201])\n',
};

test('versions: next patch, newest release, and the [package] line only', () => {
  assert.equal(nextPatch('v0.2.11'), 'v0.2.12');
  assert.equal(nextPatch('v1.9.99'), 'v1.9.100');
  assert.equal(latestRelease(HEADER), 'v0.2.10');
  assert.equal(
    latestRelease('## [v0.2.9] - 2026-10-01\n\n## [v0.2.10] - 2026-10-02\n\n## [v0.2.2] - 2026-09-01\n'),
    'v0.2.10',
    'numeric, not alphabetical',
  );
  assert.equal(latestRelease('# nothing'), null);
  const t = manifestText('x', '0.2.10', 'colonizer-redact = { version = "0.2.10", path = "../colonizer-redact" }\n');
  assert.equal(packageVersion(t), 'v0.2.10');
  const bumped = setPackageVersion(t, 'v0.2.11');
  assert.equal(packageVersion(bumped), 'v0.2.11');
  assert.match(bumped, /serde = \{ version = "1\.0\.229"/, 'a dependency version is not the package version');
  const deps = setPathDependencyVersions(t, { 'colonizer-redact': 'v0.2.11', other: 'v9.9.9' });
  assert.match(deps, /colonizer-redact = \{ version = "0\.2\.11", path = /);
  assert.match(deps, /serde = \{ version = "1\.0\.229"/, 'a registry dependency is untouched');
});

test('planCrates: bump the crates at the latest release, leave one already at the target', () => {
  const all = { colonizer: 'v0.2.10', 'colonizer-agentd': 'v0.2.10', 'colonizer-observability': 'v0.2.10', 'colonizer-redact': 'v0.2.10' };
  assert.deepEqual(planCrates(all, 'v0.2.10', 'v0.2.11').map((c) => c.action), ['bump', 'bump', 'bump', 'bump']);
  const ahead = planCrates({ ...all, 'colonizer-redact': 'v0.2.11' }, 'v0.2.10', 'v0.2.11');
  assert.equal(ahead.find((c) => c.name === 'colonizer-redact').action, 'ahead');
  assert.throws(() => planCrates({ ...all, 'colonizer-redact': 'v0.2.12' }, 'v0.2.10', 'v0.2.11'), /colonizer-redact is at v0\.2\.12/);
  assert.throws(() => planCrates({ ...all, colonizer: 'v0.2.9' }, 'v0.2.10', 'v0.2.11'), /colonizer is at v0\.2\.9/);
  assert.throws(() => planCrates({ ...all, colonizer: null }, 'v0.2.10', 'v0.2.11'), /no \[package\] version/);
});

test('prep: the normal bump assembles the changelog, moves every crate and writes a notes stub', () => {
  const root = fixture({ fragments: FRAGMENTS });
  const r = prep(root, { date: '2026-10-08', lock: false });
  assert.equal(r.changed, true);
  assert.equal(r.version, 'v0.2.11');
  const changelog = readFileSync(join(root, 'CHANGELOG.md'), 'utf8');
  assert.match(changelog, /^## \[v0\.2\.11\] - 2026-10-08$/m);
  assert.match(changelog, /A thing is fixed/);
  assert.deepEqual(readdirSync(join(root, 'changelog.d')), ['README.md'], 'the fragments are folded in and gone');
  for (const name of RELEASE_CRATES) assert.equal(packageVersion(toml(root, name)), 'v0.2.11', name);
  assert.match(toml(root, 'colonizer'), /colonizer-redact = \{ version = "0\.2\.11", path/);
  assert.match(toml(root, 'colonizer-observability'), /colonizer-redact = \{ version = "0\.2\.11", path/);
  const notes = readFileSync(join(root, 'docs', 'release-notes', 'v0.2.11.md'), 'utf8');
  assert.match(notes, /^# Colonizer v0\.2\.11$/m);
  assert.match(notes, /- \*\*A thing is fixed\.\*\* It was broken, now it is not\. \(\[#1200\]\(https:\/\/github\.com\/Colonizer-dev\/harness\/issues\/1200\)\)/);
  assert.match(notes, /- \*\*Lists page ten at a time\.\*\*/);
  assert.deepEqual(verify(root, 'v0.2.11'), [], 'what prep makes is what the tag gate accepts');
});

test('prep: a crate already ahead (colonizer-redact) is left alone, its dependents follow it', () => {
  const root = fixture({ fragments: FRAGMENTS, versions: { 'colonizer-redact': '0.2.11' } });
  const before = toml(root, 'colonizer-redact');
  const r = prep(root, { date: '2026-10-08', lock: false });
  assert.equal(r.crates.find((c) => c.name === 'colonizer-redact').action, 'ahead');
  assert.equal(toml(root, 'colonizer-redact'), before, 'redact is byte-identical');
  assert.equal(packageVersion(toml(root, 'colonizer')), 'v0.2.11');
  assert.match(toml(root, 'colonizer'), /colonizer-redact = \{ version = "0\.2\.11", path/);
  assert.deepEqual(verify(root, 'v0.2.11'), []);
});

test('prep: no fragment is a no-op that touches nothing', () => {
  const root = fixture();
  const r = prep(root, { lock: false });
  assert.equal(r.changed, false);
  assert.equal(r.pending, 0);
  assert.equal(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'), HEADER);
  assert.equal(packageVersion(toml(root, 'colonizer')), 'v0.2.10');
  assert.equal(existsSync(join(root, 'docs', 'release-notes', 'v0.2.11.md')), false);
});

test('prep refuses a minor or major bump, a crate in the wrong place, and a bad version', () => {
  const root = fixture({ fragments: FRAGMENTS });
  assert.throws(() => prep(root, { version: 'v0.3.0', lock: false }), /only bumps the patch/);
  assert.throws(() => prep(root, { version: 'v1.0.0', lock: false }), /only bumps the patch/);
  assert.throws(() => prep(root, { version: '0.2.11', lock: false }), /not a vX\.Y\.Z/);
  assert.equal(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'), HEADER, 'a refusal changes nothing');
  const odd = fixture({ fragments: FRAGMENTS, versions: { 'colonizer-agentd': '0.2.5' } });
  assert.throws(() => prep(odd, { lock: false }), /colonizer-agentd is at v0\.2\.5/);
  assert.equal(readFileSync(join(odd, 'CHANGELOG.md'), 'utf8'), HEADER);
  const dry = fixture({ fragments: FRAGMENTS });
  assert.equal(prep(dry, { lock: false, dryRun: true }).changed, false);
  assert.equal(readFileSync(join(dry, 'CHANGELOG.md'), 'utf8'), HEADER, 'a dry run changes nothing');
});

test('plan: pending count, next version and the skip marker', () => {
  const root = fixture({ fragments: FRAGMENTS });
  const p = plan(root, { message: 'fix: a thing\n\nrelease-train: skipping nothing' });
  assert.deepEqual([p.pending, p.current, p.next, p.skip], [2, 'v0.2.10', 'v0.2.11', false], 'the marker is "release-train: skip"');
  assert.equal(plan(root, { message: 'chore\n\nrelease-train: skip' }).skip, true);
  assert.equal(plan(root, { message: 'Release-Train: Skip' }).skip, true);
  assert.equal(plan(root).skip, false);
  assert.equal(plan(fixture()).pending, 0);
});

test('verify: the tag gate wants the section, no fragment left, and every crate at the version', () => {
  const root = fixture({ fragments: FRAGMENTS });
  const early = verify(root, 'v0.2.11');
  assert.ok(early.some((e) => /no "## \[v0\.2\.11\]/.test(e)), early.join('\n'));
  assert.ok(early.some((e) => /2 fragment\(s\) left/.test(e)));
  assert.ok(early.some((e) => /colonizer\/Cargo\.toml is at v0\.2\.10/.test(e)));
  prep(root, { lock: false });
  writeFileSync(join(root, 'changelog.d', 'late.fixed.md'), '**Landed late.** After the assemble. ([#1300])\n');
  assert.ok(verify(root, 'v0.2.11').some((e) => /1 fragment\(s\) left/.test(e)), 'a late fragment blocks the tag');
  rmSync(join(root, 'changelog.d', 'late.fixed.md'));
  writeFileSync(join(root, 'crates', 'colonizer-redact', 'Cargo.toml'), manifestText('colonizer-redact', '0.2.12'));
  assert.ok(verify(root, 'v0.2.11').some((e) => /colonizer-redact\/Cargo\.toml is at v0\.2\.12, not v0\.2\.11/.test(e)));
  assert.deepEqual(verify(root, 'nope'), ['"nope" is not a vX.Y.Z version']);
});

test('releaseNotesStub: a summary per fragment, issue references linked, notices and comments skipped', () => {
  const stub = releaseNotesStub('v0.2.11', [
    { file: 'a.fixed.md', text: '<!-- hi -->\nfixes-running: Stacked chains hold\n**Bold.** More. ([#7])\n' },
  ]);
  assert.match(stub, /^- \*\*Bold\.\*\* More\. \(\[#7\]\(https:\/\/github\.com\/Colonizer-dev\/harness\/issues\/7\)\)$/m);
  assert.doesNotMatch(stub, /fixes-running/);
});

test('the command line: plan, prep, verify and their exit codes', () => {
  const lines = [];
  const log = console.log;
  const err = console.error;
  console.log = (...a) => lines.push(a.join(' '));
  console.error = () => {};
  try {
    const root = fixture({ fragments: FRAGMENTS, versions: { 'colonizer-redact': '0.2.11' } });
    assert.equal(main(['plan', '--root', root]), 0);
    assert.ok(lines.includes('pending=2') && lines.includes('next=v0.2.11') && lines.includes('skip=false'));
    assert.equal(main(['verify', 'v0.2.11', '--root', root]), 1);
    assert.equal(main(['prep', '--no-lock', '--date', '2026-10-08', '--root', root]), 0);
    assert.ok(lines.includes('colonizer-redact: already v0.2.11, left alone'));
    assert.ok(lines.includes('colonizer: v0.2.10 -> v0.2.11'));
    assert.equal(main(['verify', 'v0.2.11', '--root', root]), 0);
    lines.length = 0;
    assert.equal(main(['prep', '--no-lock', '--root', root]), 0, 'a second run has nothing pending');
    assert.ok(lines.some((l) => /nothing to release/.test(l)));
    assert.equal(main(['prep', 'v0.9.0', '--root', root]), 0, 'nothing pending wins over a bad version');
    assert.equal(main(['bogus']), 2);
    assert.equal(main(['prep', '--wat']), 2);
    assert.equal(main(['verify']), 2);
  } finally {
    console.log = log;
    console.error = err;
  }
});

// --- The tag gate against git history (#1396): fixtures that are repositories, with release commits
// squashed onto main the way a merged release pull request lands.

function git(root, ...args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

function commit(root, message) {
  git(root, 'add', '-A');
  git(root, '-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '-m', message);
  return git(root, 'rev-parse', 'HEAD').trim();
}

/** Lands a pull request that adds one fragment. */
function land(root, file) {
  writeFileSync(join(root, 'changelog.d', file), `**${file} landed.** Something a user notices. ([#1300])\n`);
  return commit(root, `add ${file}`);
}

/** A repository at v0.2.10 (tagged), with no fragment pending. */
function repo() {
  const root = fixture();
  git(root, 'init', '-q', '-b', 'main');
  commit(root, 'base');
  git(root, 'tag', 'v0.2.10');
  return root;
}

/**
 * The squashed release commit: `prep` run on main as it was, except that `notYet` fragments are hidden
 * from it, as if they landed while the release pull request was open.
 */
function release(root, notYet = [], subject) {
  const aside = mkdtempSync(join(scratch, 'aside-'));
  for (const f of notYet) renameSync(join(root, 'changelog.d', f), join(aside, f));
  const r = prep(root, { lock: false, date: '2026-10-19' });
  for (const f of notYet) renameSync(join(aside, f), join(root, 'changelog.d', f));
  return commit(root, subject ?? `release: ${r.version} (#1388)`);
}

/** Runs the command line, returning its exit code and what it printed to stdout. */
function run(argv) {
  const lines = [];
  const log = console.log;
  const err = console.error;
  console.log = (...a) => lines.push(a.join(' '));
  console.error = () => {};
  try {
    return { code: main(argv), lines };
  } finally {
    console.log = log;
    console.error = err;
  }
}

test('releaseVersion reads a squashed release subject and nothing else', () => {
  assert.equal(releaseVersion('release: v0.2.16 (#1388)'), 'v0.2.16');
  assert.equal(releaseVersion('release: v0.2.16'), 'v0.2.16');
  assert.equal(releaseVersion('release: v0.2.16 and more'), null);
  assert.equal(releaseVersion('fix: release: v0.2.16'), null);
});

test('verify: a fragment that landed during the freeze does not block the release that left it', () => {
  const root = repo();
  land(root, '1300.fixed.md');
  land(root, '1301.added.md');
  land(root, '1302.fixed.md'); // merged while the release pull request was open
  const sha = release(root, ['1302.fixed.md']);
  assert.deepEqual([...laterFragments(root, sha).later], ['1302.fixed.md']);
  const notes = [];
  assert.deepEqual(verify(root, 'v0.2.11', { notes }), []);
  assert.match(notes.join('\n'), /1 fragment\(s\) .* belong to the next release: 1302\.fixed\.md/);
  // Away from the release commit there is no history to go by, and the gate is strict again.
  land(root, '1303.fixed.md');
  assert.ok(verify(root, 'v0.2.11').some((e) => /2 fragment\(s\) left/.test(e)));
});

test('verify: a fragment that was there when the release was cut still blocks it', () => {
  const root = repo();
  land(root, '1300.fixed.md');
  land(root, '1301.added.md');
  const sha = release(root, ['1300.fixed.md']); // predates 1301, which the release folded
  assert.deepEqual([...laterFragments(root, sha).later], []);
  const problems = verify(root, 'v0.2.11');
  assert.ok(problems.some((e) => /1 fragment\(s\) left in changelog\.d\/ at v0\.2\.11 \(1300\.fixed\.md\)/.test(e)), problems.join('\n'));
});

test('decideTags: an older release that fails the gate is skipped when a newer one supersedes it', () => {
  const list = [
    { sha: 'a', version: 'v0.2.15' },
    { sha: 'b', version: 'v0.2.16' },
  ];
  const bad = new Set(['v0.2.15']);
  const gate = (r) => (bad.has(r.version) ? ['2 fragment(s) left'] : []);
  assert.deepEqual(
    decideTags(list, gate).map((d) => [d.version, d.action, d.supersededBy]),
    [['v0.2.15', 'skip', 'v0.2.16'], ['v0.2.16', 'tag', undefined]],
  );
  bad.add('v0.2.16');
  assert.deepEqual(decideTags(list, gate).map((d) => d.action), ['skip', 'fail'], 'the newest one still fails');
  bad.clear();
  assert.deepEqual(decideTags(list, gate).map((d) => d.action), ['tag', 'tag']);
  assert.deepEqual(decideTags(list, gate, (v) => v === 'v0.2.15').map((d) => d.action), ['tagged', 'tag']);
});

test('tags: skips the untaggable older release with a notice and tags the newer one', () => {
  const root = repo();
  land(root, '1300.fixed.md');
  land(root, '1301.added.md');
  const older = release(root, ['1300.fixed.md'], 'release: v0.2.11 (#1375)'); // left one it should have folded
  land(root, '1302.fixed.md');
  land(root, '1303.fixed.md');
  const newer = release(root, ['1303.fixed.md'], 'release: v0.2.12 (#1388)'); // folds 1300 and 1302; 1303 is later
  land(root, 'unrelated.changed.md');
  const r = run(['tags', '--ref', 'main', '--root', root]);
  assert.equal(r.code, 0);
  assert.deepEqual(r.lines, [`skip v0.2.11 ${older}`, `tag v0.2.12 ${newer}`]);
  assert.equal(git(root, 'worktree', 'list').trim().split('\n').length, 1, 'no worktree is left behind');
  assert.equal(git(root, 'tag', '--list', 'v0.2.1[12]'), '', 'the command tags nothing itself');
});

test('tags: the newest release failing the gate fails the run; nothing pending is a quiet success', () => {
  const root = repo();
  assert.deepEqual(run(['tags', '--ref', 'main', '--root', root]), { code: 0, lines: [] });
  land(root, '1300.fixed.md');
  land(root, '1301.added.md');
  const sha = release(root, ['1300.fixed.md'], 'release: v0.2.11 (#1375)');
  assert.deepEqual(run(['tags', '--ref', 'main', '--root', root]), { code: 1, lines: [`fail v0.2.11 ${sha}`] });
});

test('the real repository: the workspace is at a state prep can read', () => {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
  const p = plan(root);
  const versions = Object.fromEntries(RELEASE_CRATES.map((c) => [c, packageVersion(readFileSync(join(root, 'crates', c, 'Cargo.toml'), 'utf8'))]));
  assert.ok(Object.values(versions).every(Boolean), JSON.stringify(versions));
  assert.doesNotThrow(() => planCrates(versions, p.current, p.next));
});
