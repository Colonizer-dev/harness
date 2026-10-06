// sync-built-with.mjs is the drift check between the vendored "Built with" entry
// (crates/colonizer/built-with.json) and the Factory Zero registry it was copied from. These tests
// drive the exported `diffUses` offline — no fetch of https://factory0.ventures/stack.json, no clock,
// no writing — so what is under test is the decision, not the network. The happy path is anchored to
// the real vendored file read from disk, so the test fails if the two ever drift apart.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { diffMeta, diffUses, findVenture, render } from '../sync-built-with.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '../..');
const vendored = JSON.parse(readFileSync(join(root, 'crates/colonizer/built-with.json'), 'utf8'));

/** One entry as the registry and the vendored copy write it. */
function entry(overrides = {}) {
  return {
    id: 'FZ-013',
    name: 'Owlpost',
    kind: 'factory-zero',
    url: 'https://owlpost.to/',
    role: 'email',
    phrase: 'Email by',
    status: 'planned',
    note: 'Waitlist confirmation mail.',
    ...overrides,
  };
}

test('the vendored entry compared with itself reports no differences', () => {
  assert.deepEqual(diffUses(vendored.uses, vendored.uses), []);
});

test('the vendored file is a well-formed snapshot the checker can round-trip byte for byte', () => {
  // `--write` must reproduce the file it read, or every run would show a formatting-only drift.
  const venture = { name: vendored.venture_name, page: vendored.venture_page, uses: vendored.uses };
  const path = join(root, 'crates/colonizer/built-with.json');
  assert.equal(render(vendored, venture, vendored.retrieved), readFileSync(path, 'utf8'));
});

test('a status flipping from planned to live is reported and named as the claim it changes', () => {
  // Owlpost is planned in the vendored file today, so this is the real planned→live move.
  const before = vendored.uses.find((u) => u.status === 'planned');
  assert.ok(before, 'the vendored file carries at least one planned entry to move');
  const differences = diffUses([before], [{ ...before, status: 'live' }]);
  assert.equal(differences.length, 1);
  assert.match(differences[0], /Owlpost \(FZ-013\)/);
  assert.match(differences[0], /status "planned" -> "live"/);
  assert.match(differences[0], /understates the product/);
});

test('a status flipping from live back to planned is reported too', () => {
  const [before] = vendored.uses;
  const differences = diffUses([before], [{ ...before, status: 'planned' }]);
  assert.equal(differences.length, 1);
  assert.match(differences[0], /status "live" -> "planned"/);
});

test('an entry the registry no longer lists is reported as removed', () => {
  const differences = diffUses(vendored.uses, vendored.uses.slice(1));
  // One sentence: an entry that came or went has already moved every position after it, so a
  // separate order line would only repeat that.
  assert.deepEqual(differences, ['Cratefield (FZ-004): in the vendored copy but no longer in the registry']);
});

test('an entry the vendored copy is missing is reported as added', () => {
  const differences = diffUses(vendored.uses.slice(1), vendored.uses);
  assert.deepEqual(differences, ['Cratefield (FZ-004): in the registry but missing from the vendored copy']);
});

test('a renamed entry is reported', () => {
  const differences = diffUses([entry()], [entry({ name: 'Owlpost Mail' })]);
  assert.deepEqual(differences, ['Owlpost Mail (FZ-013): name "Owlpost" -> "Owlpost Mail"']);
});

test('a changed note is reported', () => {
  const differences = diffUses([entry()], [entry({ note: 'Waitlist confirmation mail, now sending.' })]);
  assert.deepEqual(differences, [
    'Owlpost (FZ-013): note "Waitlist confirmation mail." -> "Waitlist confirmation mail, now sending."',
  ]);
});

test('a changed url is reported', () => {
  const differences = diffUses([entry()], [entry({ url: 'https://owlpost.example/' })]);
  assert.deepEqual(differences, [
    'Owlpost (FZ-013): url "https://owlpost.to/" -> "https://owlpost.example/"',
  ]);
});

test('a changed phrase is reported', () => {
  const differences = diffUses([entry()], [entry({ phrase: 'Mail by' })]);
  assert.deepEqual(differences, ['Owlpost (FZ-013): phrase "Email by" -> "Mail by"']);
});

test('a changed role and a changed kind are reported', () => {
  assert.deepEqual(diffUses([entry()], [entry({ role: 'inbound-email' })]), [
    'Owlpost (FZ-013): role "email" -> "inbound-email"',
  ]);
  assert.deepEqual(diffUses([entry()], [entry({ kind: 'third-party' })]), [
    'Owlpost (FZ-013): kind "factory-zero" -> "third-party"',
  ]);
});

test('the same entries in a different order are reported as a reordering', () => {
  const polar = entry({ id: 'polar', name: 'Polar' });
  const differences = diffUses([entry(), polar], [polar, entry()]);
  assert.deepEqual(differences, ['order: vendored FZ-013, polar vs registry polar, FZ-013']);
});

test('a reordering is not reported when an entry also came or went, which would be noise', () => {
  const differences = diffUses([entry(), entry({ id: 'polar', name: 'Polar' })], [entry()]);
  assert.deepEqual(differences, ['Polar (polar): in the vendored copy but no longer in the registry']);
});

test('several differences in one file are all reported', () => {
  const [a, b] = vendored.uses;
  const differences = diffUses([a, b], [{ ...a, url: 'https://cratefield.example/' }, { ...b, status: 'live' }]);
  assert.equal(differences.length, 2);
  assert.match(differences[0], /Cratefield \(FZ-004\): url/);
  assert.match(differences[1], /Owlpost \(FZ-013\): status "planned" -> "live"/);
});

test('the notes’ typographic apostrophes survive the comparison untouched', () => {
  // The vendored notes carry U+2019. A comparison that normalised them would silently pass a real
  // edit, and one that rewrote them would mangle the text the venture page shows.
  const curly = entry({ note: 'The screen module’s provider.' });
  assert.deepEqual(diffUses([curly], [{ ...curly }]), []);
  const differences = diffUses([curly], [entry({ note: "The screen module's provider." })]);
  assert.equal(differences.length, 1, 'a straight apostrophe is a different note and is reported as one');
});

test('a registry without the venture named fails instead of comparing nothing', () => {
  const registry = { ventures: [{ id: 'FZ-001', name: 'Other', uses: [] }] };
  assert.throws(() => findVenture(registry, 'FZ-006'), /no venture FZ-006/);
  assert.throws(() => findVenture({}, 'FZ-006'), /no ventures list/);
});

test('findVenture picks the entry by id, never by position', () => {
  const registry = {
    ventures: [
      { id: 'FZ-001', name: 'Other', uses: [] },
      { id: 'FZ-006', name: 'Colonizer', uses: [entry()] },
    ],
  };
  assert.equal(findVenture(registry, 'FZ-006').name, 'Colonizer');
  assert.throws(() => findVenture({ ventures: [{ id: 'FZ-006', name: 'Colonizer' }] }, 'FZ-006'), /no uses list/);
});
// The top level of the vendored file — the venture's name, its page, the registry it came from, the
// date it was taken and the shape of the whole file — is compared by `diffMeta`. It is a separate
// function from `diffUses` because it answers a different question: not "does the stack match" but
// "is this the file `--write` would have produced", which is what a user reading the Settings pane
// takes the copy to mean.
const venture = {
  id: vendored.venture,
  name: vendored.venture_name,
  page: vendored.venture_page,
  uses: vendored.uses,
};
// `overrides` edits the vendored file; the bytes handed to `diffMeta` are what `--write` would write
// for that edited file, so each test sees one difference of its own making and not the shape rule.
// `today` is the vendored file's own day, unless a test says otherwise — the shape of the file is
// compared against its own values, so an edited `retrieved` alone never trips that rule too.
const meta = (overrides = {}, input = {}) => {
  const file = { ...vendored, ...overrides };
  return diffMeta({
    text: render(file, venture, file.retrieved),
    vendored: file,
    venture,
    registryUrl: file.registry,
    ...input,
    today: input.today ?? vendored.retrieved,
  });
};

test('the vendored file’s top level compared with the registry reports no differences', () => {
  assert.deepEqual(meta(), []);
});

test('a venture the registry renamed is reported, because the name is what a user reads', () => {
  const differences = meta({}, { venture: { ...venture, name: 'Colonizer 2' } });
  assert.deepEqual(differences, [
    'Colonizer 2 (FZ-006): venture_name "Colonizer" -> "Colonizer 2"',
  ]);
});

test('a venture the registry moved to another page is reported', () => {
  const differences = meta({}, { venture: { ...venture, page: 'https://factory0.ventures/ventures/colonizer-2/' } });
  assert.equal(differences.length, 1);
  assert.match(differences[0], /venture_page "https:\/\/factory0\.ventures\/ventures\/colonizer\/" ->/);
});

test('a file that names another registry is reported, so it cannot claim another source', () => {
  const differences = meta({}, { registryUrl: 'https://example.invalid/stack.json' });
  assert.equal(differences.length, 1);
  assert.match(differences[0], /registry "https:\/\/factory0\.ventures\/stack\.json" -> "https:\/\/example\.invalid\/stack\.json"/);
});

test('a retrieved that is not a date is reported', () => {
  for (const bad of [undefined, '', '2026/10/05', 'yesterday', 20261005]) {
    const differences = meta({ retrieved: bad });
    assert.equal(differences.length, 1, `${JSON.stringify(bad)} is not a date and is reported as one`);
    assert.match(differences[0], /^retrieved: .* is not a YYYY-MM-DD date$/);
  }
});

test('a retrieved later than today is reported: the file claims a day it was not taken on', () => {
  const differences = meta({ retrieved: '2099-01-01' });
  assert.deepEqual(differences, ['retrieved: "2099-01-01" is later than today ("2026-10-05")']);
});

test('a retrieved a day behind today is not drift either — only a day ahead of it is', () => {
  assert.deepEqual(meta({}, { today: '2026-10-06' }), []);
});

test('a retrieved older than today is not drift, so the check cannot fail on its own schedule', () => {
  // The rule that must not exist: `--write` stamps the day it ran, so every untouched copy is older
  // than the next day's run. Comparing the date to the clock would fail CI every morning after a
  // refresh, on a body that still matches the registry exactly. A past date over a matching body is
  // the correct state.
  const differences = meta({ retrieved: '2020-01-01' });
  assert.deepEqual(differences, []);
});

test('a file that is not the shape --write writes is reported, so a reformat is not silently equal', () => {
  // A check that only read the parsed values would call both of these identical to the registry.
  const reordered = { registry: vendored.retrieved, venture: vendored.venture, retrieved: vendored.registry };
  assert.equal(
    diffMeta({ text: `${JSON.stringify(reordered, null, 2)}\n`, vendored, venture, registryUrl: vendored.registry, today: vendored.retrieved }).length,
    1,
  );
  const compact = diffMeta({ text: JSON.stringify(vendored), vendored, venture, registryUrl: vendored.registry, today: vendored.retrieved });
  assert.equal(compact.length, 1);
  assert.match(compact[0], /shape differs from what --write writes/);
});

test('the real vendored file is byte for byte what --write would write for the registry it names', () => {
  // The anchor the shape rule depends on: if render() and the file ever drift apart, the shape rule
  // above would report a difference that no one can fix with `--write`.
  const path = join(root, 'crates/colonizer/built-with.json');
  assert.equal(render(vendored, venture, vendored.retrieved), readFileSync(path, 'utf8'));
});
