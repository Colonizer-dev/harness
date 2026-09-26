// scripts/changelog.mjs: fragments under changelog.d/ fold into CHANGELOG.md at a release. The pure
// parts (fragment parsing, section order, entry sort, link merging) are tested directly, and the CLI
// runs against throwaway repositories, including real git history for the `check --base` rule that
// keeps CHANGELOG.md out of ordinary pull requests.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  REPO_URL,
  assemble,
  compareFragments,
  convert,
  lintChangelog,
  lintFragment,
  mergeLinks,
  parseArgs,
  parseFragment,
  parseName,
  splitLinks,
} from '../changelog.mjs';

const SCRIPT = resolve(dirname(fileURLToPath(import.meta.url)), '../changelog.mjs');
const scratch = mkdtempSync(join(tmpdir(), 'changelog-'));
after(() => rmSync(scratch, { recursive: true, force: true }));

const HEAD = `# Changelog

Intro.

## Unreleased

Pending entries live in changelog.d/.

## [v0.1.0] - 2026-09-17

### Added

- **First.** The first release. ([#2])

[mem0]: https://mem0.ai
[#2]: ${REPO_URL}/pull/2
[v0.1.0]: ${REPO_URL}/releases/tag/v0.1.0
`;

const frag = (file, text) => ({ file, type: parseName(file).type, text });

let repos = 0;
function repo({ changelog = HEAD, fragments = {}, version = '0.1.0' } = {}) {
  const root = join(scratch, `repo-${++repos}`);
  mkdirSync(join(root, 'changelog.d'), { recursive: true });
  mkdirSync(join(root, 'crates/colonizer'), { recursive: true });
  writeFileSync(join(root, 'CHANGELOG.md'), changelog);
  writeFileSync(join(root, 'crates/colonizer/Cargo.toml'), `[package]\nname = "x"\nversion = "${version}"\n`);
  for (const [name, text] of Object.entries(fragments)) writeFileSync(join(root, 'changelog.d', name), text);
  return root;
}

function run(root, ...args) {
  return spawnSync(process.execPath, [SCRIPT, ...args, '--root', root], {
    encoding: 'utf8',
    env: { ...process.env, GITHUB_ACTIONS: '' },
  });
}

function git(root, ...args) {
  execFileSync('git', args, { cwd: root, stdio: 'ignore' });
}

function commitAll(root, message) {
  git(root, 'add', '-A');
  git(root, '-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-q', '-m', message);
}

// ---------------------------------------------------------------------------------------------- units

test('fragment names carry a slug and a known type', () => {
  assert.deepEqual(parseName('123.fixed.md'), { slug: '123', type: 'fixed' });
  assert.deepEqual(parseName('123-two.take-care.md'), { slug: '123-two', type: 'take-care' });
  assert.match(parseName('123.bugfix.md').error, /unknown type "bugfix"/);
  assert.match(parseName('Foo.added.md').error, /expected <issue-or-slug>/);
  assert.match(parseName('123.added.txt').error, /expected/);
  assert.match(parseName('no-type.md').error, /./);
});

test('a fragment reads the same with or without its bullet, and keeps its link definitions apart', () => {
  const bare = parseFragment('**Title.** Line one\ncontinues. ([#5])\n\n[mem0]: https://mem0.ai\n');
  const bullet = parseFragment('<!-- a note -->\n- **Title.** Line one\n  continues. ([#5])\n\n[mem0]: https://mem0.ai\n');
  assert.deepEqual(bare, bullet);
  assert.equal(bare.body, '**Title.** Line one\ncontinues. ([#5])');
  assert.deepEqual(bare.links, [['mem0', 'https://mem0.ai']]);
});

test('lint refuses empty, headed and two-entry fragments', () => {
  assert.match(lintFragment('1.added.md', '<!-- only a comment -->\n').join(), /empty/);
  assert.match(lintFragment('1.added.md', '### Added\n\ntext\n').join(), /no headings/);
  assert.match(lintFragment('1.added.md', '- **One.** a\n- **Two.** b\n').join(), /one entry per fragment/);
  assert.deepEqual(lintFragment('1.added.md', '- **One.** a\n  - a nested point\n'), []);
  assert.match(lintFragment('1.new.md', 'x').join(), /unknown type/);
});

test('fragments sort by issue number numerically, then by name, slugs last', () => {
  const names = ['cockpit.added.md', '100.added.md', '20-b.added.md', '20-a.added.md', '3.added.md', 'api.added.md'];
  assert.deepEqual(names.sort(compareFragments), [
    '3.added.md',
    '20-a.added.md',
    '20-b.added.md',
    '100.added.md',
    'api.added.md',
    'cockpit.added.md',
  ]);
});

test('link definitions deduplicate (first wins) and sort: names, then issues by number, then versions newest first', () => {
  const merged = mergeLinks(
    [
      ['#10', 'a'],
      ['v0.1.10', 'x'],
      ['v0.1.9', 'y'],
    ],
    [
      ['#10', 'b'],
      ['#9', 'c'],
      ['Zed', 'z'],
      ['mem0', 'm'],
    ],
  );
  assert.deepEqual(merged, [
    ['mem0', 'm'],
    ['Zed', 'z'],
    ['#9', 'c'],
    ['#10', 'a'],
    ['v0.1.10', 'x'],
    ['v0.1.9', 'y'],
  ]);
});

test('CHANGELOG.md with entries under Unreleased fails the lint', () => {
  assert.deepEqual(lintChangelog(HEAD), []);
  const dirty = HEAD.replace('Pending entries live in changelog.d/.', '### Added\n\n- **Sneaky.** x');
  assert.match(lintChangelog(dirty).join(), /under "## Unreleased"/);
  assert.match(lintChangelog('# Changelog\n').join(), /no "## Unreleased"/);
});

// ---------------------------------------------------------------------------------------------- assemble

test('assemble writes the sections in type order, entries sorted, links merged and generated', () => {
  const out = assemble(
    HEAD,
    [
      frag('security-note.security.md', '**Locked.** A door. ([#40])'),
      frag('30.take-care.md', '- **Careful.** Mind the step.'),
      frag('12.added.md', '**Twelve.** Uses [mem0] too. ([#12], [#2])\n\n[mem0]: https://example.invalid/other'),
      frag('7.fixed.md', '**Seven.** Fixed.\nOn two lines. ([#7])\n\n[#7]: https://example.invalid/pull/7'),
      frag('3.added.md', '**Three.** Added. ([#3])'),
    ],
    { version: 'v0.2.0', date: '2026-10-01' },
  );
  const section = out.slice(out.indexOf('## [v0.2.0]'), out.indexOf('## [v0.1.0]'));
  assert.equal(
    section,
    `## [v0.2.0] - 2026-10-01

### Added

- **Three.** Added. ([#3])
- **Twelve.** Uses [mem0] too. ([#12], [#2])

### Fixed

- **Seven.** Fixed.
  On two lines. ([#7])

### Security

- **Locked.** A door. ([#40])

### Take care

- **Careful.** Mind the step.

`,
  );
  assert.ok(out.indexOf('Pending entries live in changelog.d/.') < out.indexOf('## [v0.2.0]'), 'under the Unreleased note');
  const links = out.slice(out.lastIndexOf('\n\n') + 2).trimEnd().split('\n');
  assert.deepEqual(links, [
    '[mem0]: https://mem0.ai', // CHANGELOG.md's own definition wins over the fragment's
    `[#2]: ${REPO_URL}/pull/2`,
    `[#3]: ${REPO_URL}/issues/3`,
    '[#7]: https://example.invalid/pull/7', // a fragment's own definition beats a generated one
    `[#12]: ${REPO_URL}/issues/12`,
    `[#40]: ${REPO_URL}/issues/40`,
    `[v0.2.0]: ${REPO_URL}/releases/tag/v0.2.0`,
    `[v0.1.0]: ${REPO_URL}/releases/tag/v0.1.0`,
  ]);
});

test('assemble is deterministic whatever order the fragments arrive in', () => {
  const fragments = [frag('b.added.md', '**B.**'), frag('2.added.md', '**Two.**'), frag('a.fixed.md', '**A.**')];
  const opts = { version: 'v0.2.0', date: '2026-10-01' };
  assert.equal(assemble(HEAD, fragments, opts), assemble(HEAD, [...fragments].reverse(), opts));
});

test('assemble refuses nothing to do, a version already released, bad input and a dirty Unreleased', () => {
  const opts = { version: 'v0.2.0', date: '2026-10-01' };
  assert.throws(() => assemble(HEAD, [], opts), /nothing to release/);
  assert.throws(() => assemble(HEAD, [frag('1.added.md', 'x')], { ...opts, version: 'v0.1.0' }), /already has/);
  assert.throws(() => assemble(HEAD, [frag('1.added.md', 'x')], { ...opts, version: '0.2.0' }), /--version/);
  assert.throws(() => assemble(HEAD, [frag('1.added.md', 'x')], { ...opts, date: 'today' }), /--date/);
  assert.throws(() => assemble(HEAD, [frag('1.added.md', '')], opts), /empty/);
});

test('the CLI assembles, deletes the fragments, and a second run is a no-op', () => {
  const root = repo({ fragments: { '5.fixed.md': '**Five.** ([#5])\n', 'README.md': '# not a fragment\n' } });
  const first = run(root, 'assemble', '--version', 'v0.2.0', '--date', '2026-10-01');
  assert.equal(first.status, 0, first.stderr);
  const once = readFileSync(join(root, 'CHANGELOG.md'), 'utf8');
  assert.match(once, /## \[v0\.2\.0\] - 2026-10-01\n\n### Fixed\n\n- \*\*Five\.\*\* \(\[#5\]\)/);
  assert.deepEqual(readdirSync(join(root, 'changelog.d')), ['README.md']);

  const second = run(root, 'assemble', '--version', 'v0.2.0', '--date', '2026-10-01');
  assert.equal(second.status, 0, second.stderr);
  assert.match(second.stdout, /nothing to do/);
  assert.equal(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'), once);
  assert.equal(run(root, 'check').status, 0);
});

test('--dry-run prints the result and touches nothing', () => {
  const root = repo({ fragments: { '5.fixed.md': '**Five.**\n' } });
  const r = run(root, 'assemble', '--version', 'v0.2.0', '--date', '2026-10-01', '--dry-run');
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /## \[v0\.2\.0\]/);
  assert.equal(readFileSync(join(root, 'CHANGELOG.md'), 'utf8'), HEAD);
  assert.ok(existsSync(join(root, 'changelog.d/5.fixed.md')));
});

// ---------------------------------------------------------------------------------------------- new

test('new scaffolds a template that check refuses until it is written, or takes --text', () => {
  const root = repo();
  const made = run(root, 'new', '42', 'fixed');
  assert.equal(made.status, 0, made.stderr);
  assert.equal(made.stdout.trim(), 'changelog.d/42.fixed.md');
  const unfilled = run(root, 'check');
  assert.equal(unfilled.status, 1);
  assert.match(unfilled.stderr, /42\.fixed\.md: empty/);

  assert.equal(run(root, 'new', '42', 'fixed').status, 1, 'an existing fragment is not overwritten');
  assert.equal(run(root, 'new', '#43', 'added', '--text', '**Forty-three.** ([#43])').status, 0);
  assert.equal(readFileSync(join(root, 'changelog.d/43.added.md'), 'utf8'), '**Forty-three.** ([#43])\n');
  assert.equal(run(root, 'new', '44', 'bugfix').status, 2);
});

// ---------------------------------------------------------------------------------------------- check

test('check reports bad names, bad types and empty fragments', () => {
  const root = repo({ fragments: { 'Bad Name.md': 'x', '1.oops.md': 'x', '2.added.md': '\n', '3.added.md': '**Fine.**' } });
  const r = run(root, 'check');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /Bad Name\.md: expected/);
  assert.match(r.stderr, /1\.oops\.md: unknown type "oops"/);
  assert.match(r.stderr, /2\.added\.md: empty/);
  assert.doesNotMatch(r.stderr, /3\.added\.md/);
});

test('check --base fails a pull request that edits CHANGELOG.md, and passes one that adds a fragment', () => {
  const root = repo();
  git(root, 'init', '-q', '-b', 'main');
  commitAll(root, 'base');
  git(root, 'checkout', '-q', '-b', 'feature');

  mkdirSync(join(root, 'crates/colonizer/src'), { recursive: true });
  writeFileSync(join(root, 'crates/colonizer/src/lib.rs'), '// code\n');
  commitAll(root, 'code only');
  const warned = run(root, 'check', '--base', 'main');
  assert.equal(warned.status, 0, warned.stderr);
  assert.match(warned.stderr, /warning: this change touches code but adds no changelog\.d\/ fragment/);

  writeFileSync(join(root, 'changelog.d/9.added.md'), '**Nine.** ([#9])\n');
  commitAll(root, 'fragment');
  const clean = run(root, 'check', '--base', 'main');
  assert.equal(clean.status, 0, clean.stderr);
  assert.doesNotMatch(clean.stderr, /warning/);

  writeFileSync(join(root, 'CHANGELOG.md'), HEAD.replace('The first release.', 'The first release, edited.'));
  commitAll(root, 'hand edit');
  const edited = run(root, 'check', '--base', 'main');
  assert.equal(edited.status, 1);
  assert.match(edited.stderr, /This pull request edits CHANGELOG\.md, which only a release does/);
  assert.match(edited.stderr, /git checkout origin\/main -- CHANGELOG\.md/);
  assert.match(edited.stderr, /node scripts\/changelog\.mjs new <issue>/);
  assert.equal(run(root, 'check', '--base', 'main', '--allow-changelog-edit').status, 0);
});

test('check --base lets the release pull request change CHANGELOG.md, and only for the bumped version', () => {
  const root = repo({ fragments: { '9.added.md': '**Nine.** ([#9])\n' } });
  git(root, 'init', '-q', '-b', 'main');
  commitAll(root, 'base');
  git(root, 'checkout', '-q', '-b', 'release');

  assert.equal(run(root, 'assemble', '--version', 'v0.2.0', '--date', '2026-10-01').status, 0);
  commitAll(root, 'assemble without the bump');
  const unbumped = run(root, 'check', '--base', 'main');
  assert.equal(unbumped.status, 1, 'the section must match the crate version');
  assert.match(unbumped.stderr, /which only a release does/);

  writeFileSync(join(root, 'crates/colonizer/Cargo.toml'), '[package]\nname = "x"\nversion = "0.2.0"\n');
  commitAll(root, 'bump');
  const release = run(root, 'check', '--base', 'main');
  assert.equal(release.status, 0, release.stderr);
  assert.equal(run(root, 'check', '--release', 'v0.2.0').status, 0);
});

test('check --release refuses a tag without its section or with fragments left over', () => {
  const root = repo({ fragments: { '9.added.md': '**Nine.**\n' } });
  const r = run(root, 'check', '--release', 'v0.2.0');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /no "## \[v0\.2\.0\] - <date>" section/);
  assert.match(r.stderr, /1 fragment\(s\) left in changelog\.d\//);
});

test('check fails a CHANGELOG.md that has entries under Unreleased', () => {
  const root = repo({ changelog: HEAD.replace('Pending entries live in changelog.d/.', '- **Sneaky.** x') });
  const r = run(root, 'check');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /1 entry under "## Unreleased"/);
  assert.match(r.stderr, /node scripts\/changelog\.mjs convert/, 'the failure says how to fix it');
});

// ---------------------------------------------------------------------------------------------- convert

const WITH_ENTRIES = HEAD.replace(
  'Pending entries live in changelog.d/.',
  `### Added

- **Shiny.** A new thing,
  on two lines. ([#12])
- **Also [mem0].** Uses a named link. ([#13])

### Take care

- **Mind it.** Something to know. ([#2])`,
).replace(`[#2]: ${REPO_URL}/pull/2`, `[#2]: ${REPO_URL}/pull/2\n[#12]: ${REPO_URL}/pull/12`);

test('convert moves each Unreleased entry into a fragment with the link definitions only it uses', () => {
  const { changelog, fragments } = convert(WITH_ENTRIES);
  assert.deepEqual(
    fragments.map((f) => f.file),
    ['12-shiny.added.md', '13-also-mem0.added.md', '2-mind-it.take-care.md'],
  );
  assert.equal(fragments[0].text, `- **Shiny.** A new thing,\n  on two lines. ([#12])\n\n[#12]: ${REPO_URL}/pull/12\n`);
  // [#2] is also used by the released section, so its definition stays in CHANGELOG.md as well.
  assert.equal(fragments[1].text, '- **Also [mem0].** Uses a named link. ([#13])\n\n[mem0]: https://mem0.ai\n');
  assert.match(changelog, /## Unreleased\n\nEntries for the next release are not written here\./);
  assert.doesNotMatch(changelog, /Shiny|### Take care/);
  assert.doesNotMatch(changelog, /\[#12\]:|\[mem0\]:/, 'a definition only the moved entries used leaves with them');
  assert.match(changelog, /\[#2\]: /);
  assert.match(fragments[2].text, /\[#2\]: /);
  assert.deepEqual(lintChangelog(changelog), []);
});

test('convert then assemble gives back the same entries', () => {
  const { changelog, fragments } = convert(WITH_ENTRIES);
  const out = assemble(
    changelog,
    fragments.map((f) => frag(f.file, f.text)),
    { version: 'v0.2.0', date: '2026-10-01' },
  );
  for (const line of ['- **Shiny.** A new thing,\n  on two lines. ([#12])', '- **Mind it.** Something to know. ([#2])']) {
    assert.ok(out.includes(line), line);
  }
  assert.match(out, new RegExp(`\\[#12\\]: ${REPO_URL}/pull/12`), 'the moved definition comes back as it was');
});

test('convert skips a fragment that is already there, and numbers a clashing name', () => {
  const first = convert(WITH_ENTRIES);
  const existing = new Map(first.fragments.map((f) => [f.file, f.text]));
  assert.deepEqual(convert(WITH_ENTRIES, existing).fragments, [], 'nothing twice');
  const clash = new Map([['12-shiny.added.md', 'something else\n']]);
  assert.equal(convert(WITH_ENTRIES, clash).fragments[0].file, '12-shiny-2.added.md');
  assert.throws(() => convert(HEAD.replace('Pending entries live in changelog.d/.', '- **Loose.** x')), /no "### <Section>"/);
  assert.throws(() => convert(HEAD.replace('Pending entries live in changelog.d/.', '### Misc\n\n- x')), /not a section/);
});

// Issue #574's branch: a rebase left a blank line inside the link-definition block, and convert
// crashed with "Cannot read properties of null (reading '1')". These are that branch's entries: a
// bold summary holding a code span and a path, and a "### Take care" section.
const FROM_574 = `${HEAD.trimEnd().replace(
  'Pending entries live in changelog.d/.',
  `Pending entries live in changelog.d/.

### Added

- **Waiting colonies free their slot.** A colony whose question has waited past a grace period is
  now suspended, at \`POST /api/sessions/{id}/answer\` too. ([#562])

### Take care

- **Claude Code colonies now mount a writable host directory at \`/root/.claude/projects\`**, where
  the runner keeps its session transcripts. ([#562])
- **Colonies waiting on an answer when you upgrade get suspended** once the grace has passed. ([#562])`,
)}\n\n[#562]: ${REPO_URL}/issues/562\n`;

test('convert survives a blank line inside the link block and code spans in a summary (#574)', () => {
  const { changelog, fragments } = convert(FROM_574);
  assert.deepEqual(
    fragments.map((f) => f.file),
    [
      '562-waiting-colonies-free-their-slot.added.md',
      '562-claude-code-colonies-now-mount-a.take-care.md',
      '562-colonies-waiting-on-an-answer-when.take-care.md',
    ],
  );
  assert.match(fragments[1].text, /^- \*\*Claude Code colonies now mount a writable host directory at `\/root\/\.claude\/projects`\*\*/);
  assert.match(fragments[0].text, new RegExp(`\\[#562\\]: ${REPO_URL}/issues/562\\n$`));
  assert.deepEqual(lintChangelog(changelog), []);
  assert.doesNotMatch(changelog, /#562/);
  const root = repo({ changelog: FROM_574 });
  const r = run(root, 'convert');
  assert.equal(r.status, 0, r.stderr);
  assert.equal(run(root, 'check').status, 0);
});

test('splitLinks keeps definitions across blank lines and never returns a blank one', () => {
  const { prose, links } = splitLinks('# T\n\ntext\n\n[a]: https://a\n\n[#2]: https://b\n   \n[v0.1.0]: https://c\n');
  assert.equal(prose, '# T\n\ntext');
  assert.deepEqual(links, [
    ['a', 'https://a'],
    ['#2', 'https://b'],
    ['v0.1.0', 'https://c'],
  ]);
});

test('an entry convert cannot place is named in the error', () => {
  assert.throws(
    () => convert(HEAD.replace('Pending entries live in changelog.d/.', '- **Loose entry.** No section above it.')),
    /"- \*\*Loose entry\.\*\* No section above it\.…" is under Unreleased but no "### <Section>"/,
  );
});

test('the CLI converts in place, and check passes afterwards', () => {
  const root = repo({ changelog: WITH_ENTRIES });
  assert.equal(run(root, 'check').status, 1);
  const r = run(root, 'convert');
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /moved 3 entries/);
  assert.equal(run(root, 'check').status, 0);
  assert.match(run(root, 'convert').stdout, /moved 0 entries/, 'a second run has nothing to move');
});

test('arguments: unknown commands and options, and valued options without a value, are usage errors', () => {
  assert.ok(parseArgs(['frobnicate']).error);
  assert.ok(parseArgs(['check', '--nope']).error);
  assert.ok(parseArgs(['assemble', '--version']).error);
  assert.deepEqual(parseArgs(['assemble', '--version', 'v1.0.0', '--dry-run']), {
    command: 'assemble',
    positional: [],
    flags: { version: 'v1.0.0', 'dry-run': true },
  });
  assert.equal(run(repo(), 'assemble').status, 2);
});
