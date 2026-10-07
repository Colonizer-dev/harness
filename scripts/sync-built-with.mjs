#!/usr/bin/env node
// Checks the vendored "Built with" entry (crates/colonizer/built-with.json) against the Factory Zero
// registry it was copied from, and can refresh it.
//
//   node scripts/sync-built-with.mjs            report whether the vendored copy matches the registry (exits 1 when stale)
//   node scripts/sync-built-with.mjs --check    the same check, named for CI
//   node scripts/sync-built-with.mjs --write    refresh crates/colonizer/built-with.json from the registry
//   node scripts/sync-built-with.mjs --file p  compare some other vendored copy (for testing)
//
// The registry (https://factory0.ventures/stack.json) lists what every Factory Zero venture is built
// with; this repo vendors its own entry, FZ-006, so `colonizer about`, the Settings pane and the
// venture page can all say the same thing without the mothership reaching out at runtime. A vendored
// copy is a snapshot, and a stale snapshot is worse than none: a venture whose dependency has since
// gone live would still be described as planned, and the product would keep claiming less than it
// ships. So the copy is compared field by field against the registry, and every difference is
// printed for a person to read before anything is rewritten.
//
// The comparison itself is `diffUses` (the two uses lists) and `diffMeta` (the file's top level) below,
// both exported and pure: no fetch, no clock, no filesystem — the one date `diffMeta` needs is passed in
// as `today`. scripts/test/built-with.test.mjs drives them offline with fixtures, which is the whole of
// the check the acceptance criterion needs — a vendored entry that differs from the registry must fail.
// The script's own modes are the part that talks to the network.
//
// .github/workflows/built-with-drift.yml runs `--check` on a schedule, so a registry that moved without
// anyone noticing here is a red run rather than a stale claim nobody notices.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
const builtWithPath = option('--file') ?? join(root, 'crates/colonizer/built-with.json');

// --check names the read-only mode the default already is, so CI can say what it means; it refuses
// to travel with --write, which asks for the opposite.
if (flag('--check') && flag('--write')) {
  console.error('--check and --write contradict each other: --check reports drift and never writes');
  process.exit(2);
}

// Only ever a fallback for a vendored file that is missing or unreadable; a real copy names the
// registry it came from, and the check follows that file rather than this constant.
const DEFAULT_REGISTRY = 'https://factory0.ventures/stack.json';
const DEFAULT_VENTURE = 'FZ-006';
const USER_AGENT = 'colonizer-built-with';

// The fields an entry is made of, in the order the registry and the vendored file both write them.
// `status` leads the per-field comparison only because it is the one that changes what the product
// may claim; the order here is just the order the report reads in.
const FIELDS = ['name', 'kind', 'url', 'role', 'phrase', 'status', 'note'];
// A status move is worth its own sentence, so the comparison leads with it.
const STATUS_FIELD = 'status';

const label = (entry) => `${entry?.name ?? '?'} (${entry?.id ?? '?'})`;
const quote = (value) => JSON.stringify(value ?? '');

/**
 * Every way the vendored `uses` can differ from the registry's, as sentences a maintainer reads.
 * Pure: no fetch, no clock, no filesystem, order-sensitive.
 *
 * The registry's order is meaningful — it is the order the venture page lists the stack in — so two
 * arrays with the same members in a different order differ. Returns `[]` when they are identical.
 */
export function diffUses(vendoredUses, registryUses) {
  const vendored = Array.isArray(vendoredUses) ? vendoredUses : [];
  const registry = Array.isArray(registryUses) ? registryUses : [];
  const differences = [];
  const vendoredById = new Map(vendored.map((entry) => [entry?.id, entry]));
  const registryById = new Map(registry.map((entry) => [entry?.id, entry]));

  let membership = false;
  for (const [, entry] of vendoredById) {
    if (!registryById.has(entry?.id)) {
      differences.push(`${label(entry)}: in the vendored copy but no longer in the registry`);
      membership = true;
    }
  }
  for (const [, entry] of registryById) {
    if (!vendoredById.has(entry?.id)) {
      differences.push(`${label(entry)}: in the registry but missing from the vendored copy`);
      membership = true;
    }
  }

  for (const [id, registryEntry] of registryById) {
    const vendoredEntry = vendoredById.get(id);
    if (!vendoredEntry) continue;
    // The status first, in its own sentence: a planned dependency going live is the difference that
    // decides what the product is allowed to claim about itself.
    const field = (name) => {
      const old = vendoredEntry[name];
      if (old === registryEntry[name]) return undefined;
      return `${label(registryEntry)}: ${name} ${quote(old)} -> ${quote(registryEntry[name])}`;
    };
    const status = field(STATUS_FIELD);
    if (status !== undefined) {
      differences.push(
        vendoredEntry[STATUS_FIELD] === 'planned' && registryEntry[STATUS_FIELD] === 'live'
          ? `${status} (it is in use now, so the vendored copy understates the product)`
          : status,
      );
    }
    for (const name of FIELDS) {
      if (name === STATUS_FIELD) continue;
      const difference = field(name);
      if (difference !== undefined) differences.push(difference);
    }
    // A field the vendored file does not know, spelled a different way in the two files, would match
    // nothing above and silently drop the entry; compare the whole record as a last resort.
    for (const name of Object.keys(registryEntry)) {
      if (name === 'id' || FIELDS.includes(name)) continue;
      const difference = field(name);
      if (difference !== undefined) differences.push(difference);
    }
  }

  const vendoredOrder = vendored.map((entry) => entry?.id);
  const registryOrder = registry.map((entry) => entry?.id);
  // Only worth saying on its own when both lists hold the same entries; an added or removed one has
  // already moved the positions and reporting both ways reads as noise.
  if (!membership && vendoredOrder.join(',') !== registryOrder.join(',')) {
    differences.push(
      `order: vendored ${vendoredOrder.join(', ') || '(empty)'} vs registry ${registryOrder.join(', ') || '(empty)'}`,
    );
  }
  return differences;
}

// `retrieved` is a YYYY-MM-DD day, the way render() writes it.
const RETRIEVED_FORMAT = /^\d{4}-\d{2}-\d{2}$/;

/**
 * Every way the vendored file's top-level fields differ from the registry, as sentences a maintainer
 * reads. Pure: no fetch, no clock, no filesystem, order-sensitive. Returns `[]` when they agree.
 *
 * `diffUses` only ever saw the two `uses` arrays, so a venture that changed its name or its page, or a
 * file that names a registry it was not copied from, compared equal to anything. This is the other
 * half of the file.
 *
 * On `retrieved`, the rule is deliberately *not* "older than the file's mtime" and *not* "older than
 * today". Both fire every single day: a clone gives every file a fresh mtime, and a snapshot's date
 * is when it was taken, so an untouched copy is by definition older than the next day's check. Either
 * rule would fail CI on every pull request the morning after any refresh, which is the spurious
 * failure the check exists to avoid. What is a real difference is a date that could not have been
 * written by `--write` at all — not a date, or a date in the future, which means a hand edit or a
 * clock that disagreed with the one that wrote it. A past date over a body that still matches the
 * registry is the correct, quiet state, and is left alone.
 *
 * `text` is the file's own bytes, and they are compared against the same values written back out
 * through `render`, so a reordering, a reformatted indent or a hand-added top-level field is reported
 * too: the file is regenerated in exactly one shape, and a check that only read the parsed values would
 * call a differently shaped file identical. The round trip uses the *file's own* values, not the
 * registry's, so this rule is only ever about shape — a value that differs is the field rules above
 * reporting it once, not this rule reporting it a second time.
 */
export function diffMeta({ text, vendored, venture, registryUrl, today }) {
  const differences = [];
  const field = (name, expected) => {
    const have = vendored?.[name];
    if (have === expected) return;
    differences.push(`${label(venture)}: ${name} ${quote(have)} -> ${quote(expected)}`);
  };

  // The registry names the venture; the vendored file repeats that name and the page it is published
  // on, and both are shown to a user. The registry's own URL is where the check just read from, so a
  // file that names none (or names another one) is saying it came from somewhere else.
  field('venture_name', venture.name);
  field('venture_page', venture.page);
  field('registry', registryUrl);

  const retrieved = vendored?.retrieved;
  if (typeof retrieved !== 'string' || !RETRIEVED_FORMAT.test(retrieved)) {
    differences.push(`retrieved: ${quote(retrieved)} is not a YYYY-MM-DD date`);
  } else if (today && retrieved > today) {
    differences.push(`retrieved: ${quote(retrieved)} is later than today (${quote(today)})`);
  }

  // The file's own values, read back out: only the shape can differ here.
  const expected = render(
    vendored,
    { name: vendored?.venture_name, page: vendored?.venture_page, uses: vendored?.uses },
    retrieved,
  );
  if (expected !== text) {
    differences.push('the file’s shape differs from what --write writes: a reordered, reformatted or hand-edited top level');
  }
  return differences;
}

async function fetchRegistry(url) {
  const response = await fetch(url, { headers: { 'user-agent': USER_AGENT } });
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  return response.json();
}

/** The registry's entry for one venture, by id — never a positional guess. */
export function findVenture(registry, ventureId) {
  if (!Array.isArray(registry?.ventures)) throw new Error('the registry carries no ventures list');
  const venture = registry.ventures.find((v) => v?.id === ventureId);
  if (!venture) throw new Error(`the registry carries no venture ${ventureId}`);
  if (!Array.isArray(venture.uses)) throw new Error(`the registry's ${ventureId} carries no uses list`);
  return venture;
}

/** The vendored file with the registry's entry in it, in the same key order and formatting. */
export function render(vendored, venture, retrieved) {
  return `${JSON.stringify(
    {
      registry: vendored.registry,
      venture: vendored.venture,
      venture_name: venture.name,
      venture_page: venture.page,
      retrieved,
      uses: venture.uses,
    },
    null,
    2,
  )}\n`;
}

async function main() {
  const text = readFileSync(builtWithPath, 'utf8');
  let vendored;
  try {
    vendored = JSON.parse(text);
  } catch {
    throw new Error(`${relative(process.cwd(), builtWithPath)}: not JSON`);
  }
  // The vendored file names the registry it came from and the venture it is; the constants above are
  // only what a file that cannot say is assumed to be.
  const registryUrl = typeof vendored.registry === 'string' && vendored.registry ? vendored.registry : DEFAULT_REGISTRY;
  const ventureId = typeof vendored.venture === 'string' && vendored.venture ? vendored.venture : DEFAULT_VENTURE;

  // Everything resolves before anything writes: a failure below leaves the vendored file untouched.
  const registry = await fetchRegistry(registryUrl);
  const venture = findVenture(registry, ventureId);
  const differences = [
    ...diffUses(vendored.uses, venture.uses),
    ...diffMeta({ text, vendored, venture, registryUrl, today: new Date().toISOString().slice(0, 10) }),
  ];

  if (differences.length) {
    const where = relative(process.cwd(), builtWithPath);
    console.error(`${where} has drifted from ${registryUrl} (venture ${ventureId}):`);
    for (const difference of differences) console.error(`  ${difference}`);
    console.error(`run: node scripts/sync-built-with.mjs --write   (${differences.length} difference(s))`);
  } else {
    console.log(`${relative(process.cwd(), builtWithPath)} matches ${registryUrl} (venture ${ventureId})`);
  }

  if (flag('--write')) {
    // The registry's uses are copied verbatim — JSON.stringify leaves the notes' characters alone,
    // and no note is ever rewritten by hand here. A file already matching is not rewritten, so
    // `--write` on a clean tree touches nothing.
    const after = render(vendored, venture, new Date().toISOString().slice(0, 10));
    if (after === text) return;
    writeFileSync(builtWithPath, after);
    console.log(`wrote ${relative(process.cwd(), builtWithPath)}`);
    return;
  }
  // The check is meant to be wired to something: drift fails it, --write resolves it.
  if (differences.length) process.exitCode = 1;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}