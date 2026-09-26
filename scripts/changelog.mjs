#!/usr/bin/env node
// Changelog fragments: every pull request adds its own file under changelog.d/ instead of editing
// CHANGELOG.md, and a release folds them in. Colonies open pull requests in parallel, and when each
// inserted its line at the top of `## Unreleased` and a link ref at the bottom, nearly every pair
// of them conflicted in CHANGELOG.md. A new file per change cannot conflict with anyone's.
//
//   node scripts/changelog.mjs new 123 fixed                  scaffold changelog.d/123.fixed.md
//   node scripts/changelog.mjs new cockpit-pager changed --text '**Lists page ten at a time.** … ([#123])'
//   node scripts/changelog.mjs check                          validate every fragment and CHANGELOG.md
//   node scripts/changelog.mjs check --base origin/main       …and refuse a hand edit of CHANGELOG.md outside a release
//   node scripts/changelog.mjs check --release v0.2.0         the tagged commit: its section exists, no fragment is left
//   node scripts/changelog.mjs assemble --version v0.2.0 [--date 2026-10-01] [--dry-run]
//   node scripts/changelog.mjs convert                        move entries written under `## Unreleased` into fragments
//
// A fragment is `changelog.d/<issue-or-slug>.<type>.md`, `<type>` one of TYPES below. Its content is
// the entry, written the way CHANGELOG.md's bullets are (a bold one-line summary, then what changed),
// with or without the leading `- `. A `[#123]` reference needs no definition: assemble writes
// `[#123]: …/issues/123` (GitHub forwards that to the pull request when 123 is one). Any other
// reference-style link is defined at the fragment's foot (`[mem0]: https://mem0.ai`), and assemble
// moves the definition to CHANGELOG.md's block, deduplicated and sorted. HTML comments are dropped.
//
// assemble writes `## [vX.Y.Z] - DATE` right under `## Unreleased`, with a `### <Section>` per type in
// TYPES order and the entries in each sorted by file name (issue numbers numerically first), then
// deletes the fragments it used. Run it in the release pull request, the one that bumps the crate
// versions; it is the only pull request CI lets change CHANGELOG.md.
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const REPO_URL = 'https://github.com/Colonizer-dev/harness';
export const FRAGMENT_DIR = 'changelog.d';

// The sections a release can have, in the order they print. The keys are the fragment types.
export const TYPES = [
  ['added', 'Added'],
  ['changed', 'Changed'],
  ['deprecated', 'Deprecated'],
  ['removed', 'Removed'],
  ['fixed', 'Fixed'],
  ['security', 'Security'],
  ['take-care', 'Take care'],
];
const TYPE_NAMES = new Map(TYPES);

// Files in changelog.d/ that are not fragments.
const NOT_FRAGMENTS = new Set(['README.md', '.gitkeep']);

// Paths whose change is worth a changelog line. A pull request touching one without adding a
// fragment gets a warning, never a failure: plenty of such changes (a refactor, a test) are not news.
const CODE_PATHS = [/^crates\//, /^modules\//, /^services\//, /^web\/src\//, /^scripts\/[^/]+\.(mjs|sh)$/];

const LINK_DEF = /^\[([^\]]+)\]:\s+(\S+)\s*$/;
const NAME = /^([a-z0-9]+(?:-[a-z0-9]+)*)\.([a-z-]+)\.md$/;
const RELEASE_HEADING = /^## \[(v\d+\.\d+\.\d+)\] - \d{4}-\d{2}-\d{2}\s*$/;

// ---------------------------------------------------------------------------------------------- fragments

/** Splits a fragment file name into its slug and type, or returns an error string. */
export function parseName(file) {
  const m = NAME.exec(file);
  if (!m) return { error: `${file}: expected <issue-or-slug>.<type>.md, lowercase letters, digits and dashes` };
  if (!TYPE_NAMES.has(m[2])) {
    return { error: `${file}: unknown type "${m[2]}" (one of ${TYPES.map(([t]) => t).join(', ')})` };
  }
  return { slug: m[1], type: m[2] };
}

/**
 * A fragment's text as `{ body, links }`: `body` is the entry without its leading `- ` and with the
 * continuation lines' two-space indent removed, `links` the reference definitions at its foot.
 */
export function parseFragment(text) {
  const lines = text
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\r\n/g, '\n')
    .split('\n');
  const links = [];
  const kept = [];
  for (const line of lines) {
    const m = LINK_DEF.exec(line.trim());
    if (m && !line.startsWith(' ')) links.push([m[1], m[2]]);
    else kept.push(line.replace(/\s+$/, ''));
  }
  while (kept.length && kept[0] === '') kept.shift();
  while (kept.length && kept[kept.length - 1] === '') kept.pop();
  if (kept.length && /^[-*] /.test(kept[0])) {
    kept[0] = kept[0].slice(2);
    if (kept.slice(1).every((l) => l === '' || l.startsWith('  '))) {
      for (let i = 1; i < kept.length; i++) kept[i] = kept[i].slice(2);
    }
  }
  return { body: kept.join('\n'), links };
}

/** The entry as a CHANGELOG.md bullet. */
export function renderEntry(body) {
  return body
    .split('\n')
    .map((line, i) => (i === 0 ? `- ${line}` : line === '' ? '' : `  ${line}`))
    .join('\n');
}

/** Problems with one fragment, as strings. */
export function lintFragment(file, text) {
  const name = parseName(file);
  if (name.error) return [name.error];
  const { body } = parseFragment(text);
  const errors = [];
  if (!body.trim()) errors.push(`${file}: empty — write the entry (a bold one-line summary, then what changed)`);
  if (/^#{1,6} /m.test(body)) errors.push(`${file}: no headings in a fragment; its type (${name.type}) picks the section`);
  // Written as a bullet, a second bullet at the margin is a second entry: it would sort as one.
  const raw = text
    .replace(/<!--[\s\S]*?-->/g, '')
    .split('\n')
    .filter((l) => l.trim() && !LINK_DEF.test(l));
  if (raw.length && /^[-*] /.test(raw[0])) {
    const second = raw.slice(1).find((l) => /^[-*] /.test(l));
    if (second) errors.push(`${file}: one entry per fragment — put "${second.slice(0, 40)}…" in a file of its own`);
  }
  return errors;
}

/** Orders fragment files: a leading issue number numerically, then the name. */
export function compareFragments(a, b) {
  const na = /^(\d+)/.exec(a);
  const nb = /^(\d+)/.exec(b);
  if (na && nb && Number(na[1]) !== Number(nb[1])) return Number(na[1]) - Number(nb[1]);
  if (na && !nb) return -1;
  if (!na && nb) return 1;
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Every fragment file in `dir`, sorted, as `{ file, slug, type, text }`; errors for names that are not. */
export function readFragments(dir) {
  if (!existsSync(dir)) return { fragments: [], errors: [] };
  const fragments = [];
  const errors = [];
  for (const file of readdirSync(dir).sort(compareFragments)) {
    if (NOT_FRAGMENTS.has(file) || file.startsWith('.')) continue;
    const name = parseName(file);
    if (name.error) {
      errors.push(name.error);
      continue;
    }
    fragments.push({ file, ...name, text: readFileSync(join(dir, file), 'utf8') });
  }
  return { fragments, errors };
}

// ---------------------------------------------------------------------------------------------- CHANGELOG.md

/** Splits CHANGELOG.md into its prose and the reference definitions block at its foot. */
export function splitLinks(text) {
  const lines = text.replace(/\s+$/, '').split('\n');
  const blank = (line) => line.trim() === '';
  // The block is the run of definitions at the foot, blank lines between them included: a merge or a
  // rebase often leaves one there, and it must not end the block (or become a definition).
  let start = lines.length;
  while (start > 0 && (LINK_DEF.test(lines[start - 1]) || (blank(lines[start - 1]) && start < lines.length))) start--;
  while (start < lines.length && blank(lines[start])) start++;
  const links = lines
    .slice(start)
    .filter((line) => !blank(line))
    .map((line) => LINK_DEF.exec(line))
    .filter(Boolean)
    .map((m) => [m[1], m[2]]);
  return { prose: lines.slice(0, start).join('\n').replace(/\s+$/, ''), links };
}

function semverKey(label) {
  return label.slice(1).split('.').map(Number);
}

/** Link definition order: named links alphabetically, then `#N` by number, then versions newest first. */
export function compareLinks([a], [b]) {
  const rank = (l) => (/^#\d+$/.test(l) ? 1 : /^v\d+\.\d+\.\d+$/.test(l) ? 2 : 0);
  const ra = rank(a);
  const rb = rank(b);
  if (ra !== rb) return ra - rb;
  if (ra === 1) return Number(a.slice(1)) - Number(b.slice(1));
  if (ra === 2) {
    const [x, y] = [semverKey(a), semverKey(b)];
    for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return y[i] - x[i];
    return 0;
  }
  return a.toLowerCase() < b.toLowerCase() ? -1 : a.toLowerCase() > b.toLowerCase() ? 1 : 0;
}

/** Merges definitions (the first of each label wins, so CHANGELOG.md's own beat a fragment's) and sorts them. */
export function mergeLinks(...lists) {
  const seen = new Map();
  for (const list of lists) for (const [label, url] of list) if (!seen.has(label)) seen.set(label, url);
  return [...seen].sort(compareLinks);
}

/** `[#N]` references in `text` that are used as links, not defined. */
export function issueRefs(text) {
  return [...text.matchAll(/\[#(\d+)\](?![:(])/g)].map((m) => m[1]);
}

/** The line indexes of `## Unreleased` and of the first heading after it (or the end). */
function unreleasedBounds(lines) {
  const at = lines.findIndex((l) => /^## Unreleased\s*$/.test(l));
  if (at < 0) return null;
  let end = at + 1;
  while (end < lines.length && !/^## /.test(lines[end])) end++;
  return { at, end };
}

/** The release headings (`vX.Y.Z`) in CHANGELOG.md. */
export function releases(text) {
  return text
    .split('\n')
    .map((l) => RELEASE_HEADING.exec(l))
    .filter(Boolean)
    .map((m) => m[1]);
}

/** What `## Unreleased` says once its entries live in fragments; `convert` writes it back. */
export const UNRELEASED_NOTE = [
  'Entries for the next release are not written here. Each pull request adds its own file under',
  '[`changelog.d/`](changelog.d/README.md), and cutting a release folds them in with',
  '`node scripts/changelog.mjs assemble`, so parallel pull requests never collide in this file.',
].join('\n');

// How to fix a pull request that edited CHANGELOG.md, in the words CI prints: a colony reads this
// and has to be able to act on it without anything else.
const HOW_TO_FRAGMENT =
  'Pending entries live in changelog.d/, one file per change, never in CHANGELOG.md. To fix this: ' +
  'if you wrote entries under "## Unreleased", run `node scripts/changelog.mjs convert`, which moves each one ' +
  'into its own changelog.d/<issue>.<type>.md and puts CHANGELOG.md back, then commit both; ' +
  'otherwise restore CHANGELOG.md from the base branch (`git checkout origin/main -- CHANGELOG.md`) and add your ' +
  'entry with `node scripts/changelog.mjs new <issue> <added|changed|fixed|security|take-care>`. See changelog.d/README.md.';

/** Problems with CHANGELOG.md itself. */
export function lintChangelog(text) {
  const lines = text.split('\n');
  const bounds = unreleasedBounds(lines);
  if (!bounds) return ['CHANGELOG.md: no "## Unreleased" heading (assemble writes each release under it)'];
  const entries = lines.slice(bounds.at + 1, bounds.end).filter((l) => /^[-*] /.test(l));
  if (entries.length) {
    return [`CHANGELOG.md has ${entries.length} entr${entries.length === 1 ? 'y' : 'ies'} under "## Unreleased". ${HOW_TO_FRAGMENT}`];
  }
  return [];
}

function slugify(text) {
  return text
    .toLowerCase()
    .replace(/[`*_]/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .split('-')
    .slice(0, 6)
    .join('-')
    .replace(/-+$/, '');
}

/**
 * Moves every entry written under `## Unreleased` into a fragment: `{ changelog, fragments }`, the
 * file with its Unreleased section back to the note and the fragments to write, each named after
 * the entry's first `[#N]` and its bold summary. A reference definition that only the moved entries
 * use moves into their fragments. `existing` is the file names already in changelog.d/: an entry whose
 * fragment is already there with the same text is not written twice. Throws on an entry outside a
 * known `### <Section>`.
 */
export function convert(changelog, existing = new Map()) {
  const { prose, links } = splitLinks(changelog);
  const lines = prose.split('\n');
  const bounds = unreleasedBounds(lines);
  if (!bounds) throw new Error('CHANGELOG.md has no "## Unreleased" heading');
  const byTitle = new Map(TYPES.map(([type, title]) => [title.toLowerCase(), type]));
  const entries = [];
  let type = null;
  let current = null;
  for (const line of lines.slice(bounds.at + 1, bounds.end)) {
    const heading = /^### (.+?)\s*$/.exec(line);
    if (heading) {
      type = byTitle.get(heading[1].toLowerCase());
      if (!type) throw new Error(`"### ${heading[1]}" under Unreleased is not a section (${TYPES.map(([, t]) => t).join(', ')})`);
      current = null;
    } else if (/^[-*] /.test(line)) {
      if (!type) throw new Error(`"${line.slice(0, 50)}…" is under Unreleased but no "### <Section>": put it under one`);
      current = { type, lines: [line] };
      entries.push(current);
    } else if (current) {
      current.lines.push(line);
    }
  }
  const linkMap = new Map(links);
  const used = (text) => [...text.matchAll(/\[([^\]]+)\](?![:(])/g)].map((m) => m[1]).filter((l) => linkMap.has(l));
  const rest = [...lines.slice(0, bounds.at), ...lines.slice(bounds.end)].join('\n');
  const usedElsewhere = new Set(used(rest));

  const fragments = [];
  const taken = new Map(existing);
  const moved = new Set();
  for (const entry of entries) {
    try {
      convertEntry(entry);
    } catch (e) {
      throw new Error(`could not convert the ${entry.type} entry "${entry.lines[0].slice(0, 60)}…": ${e.message}`);
    }
  }
  function convertEntry(entry) {
    while (entry.lines.length && entry.lines[entry.lines.length - 1].trim() === '') entry.lines.pop();
    const body = entry.lines.join('\n');
    const title = /\*\*(.+?)\*\*/.exec(body)?.[1] ?? body.slice(2, 60);
    const issue = /\[#(\d+)\]/.exec(body)?.[1];
    const refs = [...new Set(used(body))].sort();
    refs.forEach((r) => moved.add(r));
    const foot = refs.length ? `\n\n${refs.map((r) => `[${r}]: ${linkMap.get(r)}`).join('\n')}` : '';
    const text = `${body}${foot}\n`;
    const stem = [issue, slugify(title)].filter(Boolean).join('-') || 'entry';
    let file = `${stem}.${entry.type}.md`;
    for (let n = 2; taken.has(file) && taken.get(file) !== text; n++) file = `${stem}-${n}.${entry.type}.md`;
    if (taken.get(file) === text) return;
    taken.set(file, text);
    fragments.push({ file, text });
  }
  const kept = links.filter(([label]) => !moved.has(label) || usedElsewhere.has(label));
  const head = [...lines.slice(0, bounds.at + 1), '', UNRELEASED_NOTE, ''];
  const body = [...head, ...lines.slice(bounds.end)].join('\n');
  const block = mergeLinks(kept).map(([l, u]) => `[${l}]: ${u}`).join('\n');
  return { changelog: `${body}${block ? `\n\n${block}` : ''}\n`, fragments };
}

/**
 * CHANGELOG.md with `fragments` folded in as `version`, dated `date`. Pure: the caller deletes the
 * fragments. Throws when there is nothing to fold or the version is already there.
 */
export function assemble(changelog, fragments, { version, date }) {
  if (!/^v\d+\.\d+\.\d+$/.test(version)) throw new Error(`--version must look like v1.2.3, not "${version}"`);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) throw new Error(`--date must look like 2026-10-01, not "${date}"`);
  if (releases(changelog).includes(version)) throw new Error(`CHANGELOG.md already has a section for ${version}`);
  if (!fragments.length) throw new Error(`no fragments in ${FRAGMENT_DIR}/: nothing to release`);
  const problems = [...lintChangelog(changelog), ...fragments.flatMap((f) => lintFragment(f.file, f.text))];
  if (problems.length) throw new Error(problems.join('\n'));

  const parsed = fragments
    .map((f) => ({ ...f, ...parseFragment(f.text) }))
    .sort((a, b) => compareFragments(a.file, b.file));
  const section = [`## [${version}] - ${date}`];
  for (const [type, title] of TYPES) {
    const entries = parsed.filter((f) => f.type === type);
    if (!entries.length) continue;
    section.push('', `### ${title}`, '', ...entries.map((f) => renderEntry(f.body)));
  }

  const { prose, links } = splitLinks(changelog);
  const lines = prose.split('\n');
  const { end } = unreleasedBounds(lines);
  let before = lines.slice(0, end);
  while (before.length && before[before.length - 1] === '') before.pop();
  const after = lines.slice(end);
  const body = [...before, '', ...section, ...(after.length ? ['', ...after] : [])].join('\n');

  const fromFragments = parsed.flatMap((f) => f.links);
  const generated = [...new Set(issueRefs(body))].map((n) => [`#${n}`, `${REPO_URL}/issues/${n}`]);
  const tag = [[version, `${REPO_URL}/releases/tag/${version}`]];
  const merged = mergeLinks(links, fromFragments, generated, tag);
  return `${body}\n\n${merged.map(([l, u]) => `[${l}]: ${u}`).join('\n')}\n`;
}

// ---------------------------------------------------------------------------------------------- git

function git(root, args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** Files the branch changed since `base`, as `[{ status, path }]`. */
export function changedFiles(root, base) {
  return git(root, ['diff', '--name-status', '--no-renames', base, 'HEAD'])
    .split('\n')
    .filter(Boolean)
    .map((l) => {
      const [status, path] = l.split('\t');
      return { status, path };
    });
}

function crateVersion(root) {
  const toml = join(root, 'crates/colonizer/Cargo.toml');
  if (!existsSync(toml)) return null;
  const m = /^version = "([^"]+)"$/m.exec(readFileSync(toml, 'utf8'));
  return m ? `v${m[1]}` : null;
}

/**
 * Every problem `check` finds, as `{ errors, warnings }`. With `base`, CHANGELOG.md may change only in
 * a release: one that adds the `## [vX.Y.Z]` heading matching the crate version.
 */
export function check(root, { base, release, allowChangelogEdit = false } = {}) {
  const errors = [];
  const warnings = [];
  const dir = join(root, FRAGMENT_DIR);
  const { fragments, errors: nameErrors } = readFragments(dir);
  errors.push(...nameErrors.map((e) => `${FRAGMENT_DIR}/${e}`));
  for (const f of fragments) errors.push(...lintFragment(f.file, f.text).map((e) => `${FRAGMENT_DIR}/${e}`));

  const changelogPath = join(root, 'CHANGELOG.md');
  const changelog = existsSync(changelogPath) ? readFileSync(changelogPath, 'utf8') : '';
  errors.push(...lintChangelog(changelog));

  if (release) {
    if (!releases(changelog).includes(release)) errors.push(`CHANGELOG.md has no "## [${release}] - <date>" section`);
    if (fragments.length) {
      errors.push(
        `${fragments.length} fragment(s) left in ${FRAGMENT_DIR}/ at ${release}: run ` +
          `node scripts/changelog.mjs assemble --version ${release} in the release pull request`,
      );
    }
  }

  if (base) {
    const changed = changedFiles(root, base);
    if (changed.some((c) => c.path === 'CHANGELOG.md') && !allowChangelogEdit) {
      let before = '';
      try {
        before = git(root, ['show', `${base}:CHANGELOG.md`]);
      } catch {
        // No CHANGELOG.md at the base: any heading it now has is new.
      }
      const added = releases(changelog).filter((v) => !releases(before).includes(v));
      const version = crateVersion(root);
      if (!added.length || (version && !added.includes(version))) {
        errors.push(
          `This pull request edits CHANGELOG.md, which only a release does. ${HOW_TO_FRAGMENT} ` +
            '(A deliberate fix to an already released entry takes the "changelog-edit" label instead.)',
        );
      }
    }
    const touchesCode = changed.some((c) => CODE_PATHS.some((re) => re.test(c.path)));
    const addsFragment = changed.some((c) => c.status !== 'D' && c.path.startsWith(`${FRAGMENT_DIR}/`));
    if (touchesCode && !addsFragment && !changed.some((c) => c.path === 'CHANGELOG.md')) {
      warnings.push(
        `this change touches code but adds no ${FRAGMENT_DIR}/ fragment; if a user would notice it, ` +
          'add one with node scripts/changelog.mjs new <issue-or-slug> <type>',
      );
    }
  }
  return { errors, warnings, fragments: fragments.length };
}

// ---------------------------------------------------------------------------------------------- CLI

const USAGE = `usage:
  changelog.mjs new <issue-or-slug> <type> [--text <entry>]
  changelog.mjs check [--base <ref>] [--release <vX.Y.Z>] [--allow-changelog-edit]
  changelog.mjs assemble --version <vX.Y.Z> [--date <YYYY-MM-DD>] [--dry-run]
  changelog.mjs convert [--dry-run]
  (any command: [--root <dir>], the repository root; default the one this script is in)
types: ${TYPES.map(([t]) => t).join(', ')}`;

/** Parses argv into `{ command, positional, flags }`, or `{ error }`. */
export function parseArgs(argv) {
  const [command, ...rest] = argv;
  if (!['new', 'check', 'assemble', 'convert'].includes(command)) return { error: USAGE };
  const valued = new Set(['--text', '--base', '--release', '--version', '--date', '--root']);
  const bare = new Set(['--dry-run', '--allow-changelog-edit']);
  const flags = {};
  const positional = [];
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i];
    if (valued.has(a)) {
      if (rest[i + 1] === undefined) return { error: `${a} needs a value\n${USAGE}` };
      flags[a.slice(2)] = rest[++i];
    } else if (bare.has(a)) flags[a.slice(2)] = true;
    else if (a.startsWith('--')) return { error: `unknown option ${a}\n${USAGE}` };
    else positional.push(a);
  }
  return { command, positional, flags };
}

const TEMPLATE = `<!--
One entry, the way CHANGELOG.md's bullets read: a bold one-line summary of what a user notices,
then what changed and anything they should know, ending with the issue or pull request as ([#123]).
[#123] needs no definition; define any other reference-style link at the foot of this file.
Everything inside this comment is dropped. Delete it once the entry is written.
-->
`;

function today() {
  return new Date().toISOString().slice(0, 10);
}

function annotate(kind, message) {
  // GitHub Actions turns these into annotations on the pull request; elsewhere they are plain lines.
  if (process.env.GITHUB_ACTIONS) console.log(`::${kind}::${message}`);
  else console.error(`${kind}: ${message}`);
}

export function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.error) {
    console.error(args.error);
    return 2;
  }
  const { command, positional, flags } = args;
  const root = resolve(flags.root ?? join(dirname(fileURLToPath(import.meta.url)), '..'));
  const dir = join(root, FRAGMENT_DIR);

  if (command === 'new') {
    const [slug, type] = positional;
    if (!slug || !type) {
      console.error(USAGE);
      return 2;
    }
    const file = `${slug.replace(/^#/, '')}.${type}.md`;
    const name = parseName(file);
    if (name.error) {
      console.error(name.error);
      return 2;
    }
    const path = join(dir, file);
    if (existsSync(path)) {
      console.error(`${FRAGMENT_DIR}/${file} already exists; pick another slug (e.g. ${name.slug}-2)`);
      return 1;
    }
    writeFileSync(path, flags.text ? `${flags.text.trim()}\n` : TEMPLATE);
    console.log(`${FRAGMENT_DIR}/${file}`);
    return 0;
  }

  if (command === 'check') {
    let result;
    try {
      result = check(root, { base: flags.base, release: flags.release, allowChangelogEdit: !!flags['allow-changelog-edit'] });
    } catch (e) {
      console.error(`check failed: ${e.message}`);
      return 1;
    }
    for (const w of result.warnings) annotate('warning', w);
    for (const e of result.errors) annotate('error', e);
    if (result.errors.length) return 1;
    console.log(`changelog: ${result.fragments} pending fragment(s), all valid`);
    return 0;
  }

  if (command === 'convert') {
    const changelogPath = join(root, 'CHANGELOG.md');
    const existing = new Map(
      readFragments(dir).fragments.map((f) => [f.file, f.text]),
    );
    let result;
    try {
      result = convert(readFileSync(changelogPath, 'utf8'), existing);
    } catch (e) {
      console.error(e.message);
      return 1;
    }
    if (flags['dry-run']) {
      for (const f of result.fragments) console.log(`${FRAGMENT_DIR}/${f.file}\n${f.text}`);
      return 0;
    }
    writeFileSync(changelogPath, result.changelog);
    for (const f of result.fragments) writeFileSync(join(dir, f.file), f.text);
    for (const f of result.fragments) console.log(`${FRAGMENT_DIR}/${f.file}`);
    console.log(`moved ${result.fragments.length} entr${result.fragments.length === 1 ? 'y' : 'ies'} out of CHANGELOG.md`);
    return 0;
  }

  // assemble
  if (!flags.version) {
    console.error(`assemble needs --version\n${USAGE}`);
    return 2;
  }
  const changelogPath = join(root, 'CHANGELOG.md');
  const changelog = readFileSync(changelogPath, 'utf8');
  const { fragments, errors } = readFragments(dir);
  if (errors.length) {
    for (const e of errors) console.error(`${FRAGMENT_DIR}/${e}`);
    return 1;
  }
  if (!fragments.length && releases(changelog).includes(flags.version)) {
    console.log(`CHANGELOG.md already has ${flags.version} and no fragment is pending: nothing to do`);
    return 0;
  }
  let next;
  try {
    next = assemble(changelog, fragments, { version: flags.version, date: flags.date ?? today() });
  } catch (e) {
    console.error(e.message);
    return 1;
  }
  if (flags['dry-run']) {
    process.stdout.write(next);
    return 0;
  }
  writeFileSync(changelogPath, next);
  for (const f of fragments) rmSync(join(dir, f.file));
  console.log(`CHANGELOG.md: ${flags.version} from ${fragments.length} fragment(s); removed them from ${FRAGMENT_DIR}/`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  // Every expected failure already returns a message; anything else is a bug, and says where.
  try {
    process.exit(main());
  } catch (e) {
    console.error(`changelog.mjs ${process.argv[2] ?? ''} failed unexpectedly: ${e.stack ?? e}`);
    process.exit(1);
  }
}
