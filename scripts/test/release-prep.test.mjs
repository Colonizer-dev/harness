// scripts/release-prep.mjs: the release train's mechanical half, run against fixture workspaces. A
// fixture is a throwaway tree with a CHANGELOG.md, fragments and the four release crates' manifests,
// so the bump, the crate-ahead case, the no-op and the tag gate are all exercised without cargo
// (`--no-lock`) or a network.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  RELEASE_CRATES,
  latestRelease,
  main,
  nextPatch,
  packageVersion,
  plan,
  planCrates,
  prep,
  releaseNotesStub,
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

test('the real repository: the workspace is at a state prep can read', () => {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
  const p = plan(root);
  const versions = Object.fromEntries(RELEASE_CRATES.map((c) => [c, packageVersion(readFileSync(join(root, 'crates', c, 'Cargo.toml'), 'utf8'))]));
  assert.ok(Object.values(versions).every(Boolean), JSON.stringify(versions));
  assert.doesNotThrow(() => planCrates(versions, p.current, p.next));
});
