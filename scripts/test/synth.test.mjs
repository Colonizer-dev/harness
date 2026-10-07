import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cpSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  RETIRE_AFTER,
  REVIEW_QUORUM,
  admit,
  applyMutation,
  draw,
  generate,
  inventory,
  loadPool,
  mutationSites,
  parseTap,
  record,
  review,
  savePool,
} from '../bench/synth.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const FIXTURE = join(ROOT, 'scripts/bench/fixture');
// The site at its line, so expected findings read like the source they come from.
const at = (source, site) => `${site.line} ${site.operator} ${site.from} → ${site.to} → ${applyMutation(source, site).split('\n')[site.line - 1].trim()}`;

test('the scanner swaps code, never comments, strings, templates or regexes', () => {
  const source = [
    '// a + b < c',
    '/* && || */',
    "const s = 'x + y';",
    'const t = `a - b ${s + 1} c`;',
    'const re = /a+b/;',
    'const d = y / 2;',
    'ok = a < b && c > d;',
  ].join('\n');
  assert.deepEqual(mutationSites(source).map((s) => at(source, s)), [
    '6 binop / → * → const d = y * 2;',
    '6 literal 2 → 3 → const d = y / 3;',
    '7 binop < → <= → ok = a <= b && c > d;',
    '7 binop && → || → ok = a < b || c > d;',
    '7 binop > → >= → ok = a < b && c >= d;',
  ]);
});

test('arrows, increments, exponents and generator stars are not swapped into nonsense', () => {
  const source = 'const f = (a) => a + 1;\nlet i = 0;\ni++;\nconst p = a ** b;\nconst q = a += 2;\nfunction* g() { yield* h(); }';
  assert.deepEqual(mutationSites(source).map((s) => at(source, s)), [
    '1 binop + → - → const f = (a) => a - 1;',
    '1 literal 1 → 2 → const f = (a) => a + 2;',
    '2 literal 0 → 1 → let i = 1;',
    '5 literal 2 → 3 → const q = a += 3;',
  ]);
});

test('numbers that are not plain integers are left alone', () => {
  const source = 'const n = 1.5, m = 0x10, k = 1e3, big = 10n, sep = 1_000;';
  assert.deepEqual(mutationSites(source), []);
});

test('a slash after a keyword opens a regex, not a division', () => {
  assert.deepEqual(mutationSites('function f(x) { return /a+b/.test(x); }'), []);
  assert.deepEqual(mutationSites('const t = typeof /a<b/c;'), []);
});

test('the TAP reader takes ok, not ok, indentation and directives', () => {
  const tap = ['TAP version 13', '# Subtest: src/cart.test.js', 'ok 1 - a cart of whole euros adds up', '    not ok 2 - a wobbly one', 'ok 3 - skipped for now # SKIP', '1..3'].join('\n');
  assert.deepEqual(parseTap(tap), { passed: ['a cart of whole euros adds up', 'skipped for now'], failed: ['a wobbly one'] });
});

test('every operator yields a mutant that breaks a fixture test, with full provenance', (ctx) => {
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(poolDir, { recursive: true, force: true }));
  const tally = generate({ repo: FIXTURE, pool: poolDir });
  assert.ok(tally.admitted >= 1, `nothing was admitted: ${JSON.stringify(tally)}`);
  const pool = loadPool(poolDir);
  assert.deepEqual([...new Set(pool.heldout.map((e) => e.method))].sort(), ['procedural:binop', 'procedural:literal']);
  assert.equal(pool.raid.length, 0);
  for (const e of pool.heldout) {
    assert.ok(e.gate.f2p.length >= 1, `${e.id} admitted with nothing failing`);
    assert.ok(e.gate.margin > 0 && e.gate.margin <= 1);
    assert.equal(e.stack, 'node');
    // The fixture lives in this repository's work tree, clean as CI checks it out, so its commit is real.
    assert.ok(e.source.repo && typeof e.source.commit === 'string' && e.source.commit.length >= 7 && e.source.file);
    assert.ok(e.created && e.cost_usd === 0 && e.review === null && e.decisions === 0 && !e.retired);
    // The issue states the symptom, never the mutation: no operator, no offset, no line.
    assert.ok(!e.issue.title.includes(e.mutation.from) && !e.issue.body.includes(e.method) && !e.issue.body.includes(`line ${e.source.line}`));
    assert.equal(e.issue.title, `${e.source.file}: tests fail after a recent change`);
    assert.ok(e.issue.body.includes(e.gate.f2p[0]));
  }
});

test('a mutant the tests still survive is not admitted', (ctx) => {
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(poolDir, { recursive: true, force: true }));
  const green = () => ({ passed: ['everything'], failed: [] });
  const tally = generate({ repo: FIXTURE, pool: poolDir, run: green });
  assert.equal(tally.admitted, 0);
  assert.equal(tally.survived, tally.candidates);
  assert.deepEqual(loadPool(poolDir).heldout, []);
});

test('a repository whose reference is already red aborts generation', (ctx) => {
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(poolDir, { recursive: true, force: true }));
  const red = () => ({ passed: [], failed: ['already broken'] });
  assert.throws(() => generate({ repo: FIXTURE, pool: poolDir, run: red }), /reference is not green/);
});

test('a mutant that does not parse is rejected before any bugged run', (ctx) => {
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(poolDir, { recursive: true, force: true }));
  let runs = 0;
  const tally = generate({
    repo: FIXTURE,
    pool: poolDir,
    run: (...args) => (runs++, { passed: ['x'], failed: [] }), // called once: the reference
    check: () => false,
  });
  assert.equal(tally.syntax, tally.candidates);
  assert.equal(tally.admitted, 0);
  assert.equal(runs, 1);
});

test('a mutant that fails differently on each run goes to the raid set, not the held-out pool', (ctx) => {
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(poolDir, { recursive: true, force: true }));
  let runs = 0;
  const wobbly = () => {
    runs++;
    if (runs === 1) return { passed: ['x'], failed: [] }; // the reference, green
    return runs % 2 === 0 ? { passed: ['x'], failed: ['one thing'] } : { passed: [], failed: ['one thing', 'another'] };
  };
  const tally = generate({ repo: FIXTURE, pool: poolDir, run: wobbly });
  assert.equal(tally.flaky, tally.candidates);
  assert.equal(tally.admitted, 0);
  const pool = loadPool(poolDir);
  assert.deepEqual(pool.heldout, []);
  assert.equal(pool.raid.length, tally.candidates);
  for (const e of pool.raid) assert.match(e.brief, new RegExp(`An injected bug of class ${e.method} lives in`));
});

test('a clean git tree records its commit, a dirty one is refused, and outside git the commit is null', (ctx) => {
  const scratch = mkdtempSync(join(tmpdir(), 'colonizer-synth-git-'));
  const pools = [mkdtempSync(join(tmpdir(), 'colonizer-synth-test-')), mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'))];
  ctx.after(() => {
    rmSync(scratch, { recursive: true, force: true });
    for (const p of pools) rmSync(p, { recursive: true, force: true });
  });
  // Flaky-fake runner, so every candidate lands in the raid set, whose entries carry the same provenance.
  // The reference run is told apart by its temp dir, since this runner serves three generate calls.
  let runs = 0;
  const wobbly = (dir) => {
    runs++;
    if (dir.includes('colonizer-synth-ref-')) return { passed: ['x'], failed: [] };
    return runs % 2 === 0 ? { passed: ['x'], failed: ['a'] } : { passed: [], failed: ['a', 'b'] };
  };

  const repo = join(scratch, 'repo');
  cpSync(FIXTURE, repo, { recursive: true });
  const env = { ...process.env };
  for (const gone of ['NODE_TEST_CONTEXT', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']) delete env[gone];
  const git = (args) => execFileSync('git', ['-c', 'user.email=synth@test', '-c', 'user.name=synth test', ...args], { cwd: repo, encoding: 'utf8', env });
  git(['init', '-q', '-b', 'main']);
  git(['add', '-A']);
  git(['commit', '-q', '-m', 'the fixture']);

  generate({ repo, pool: pools[0], run: wobbly });
  const raid = loadPool(pools[0]).raid;
  assert.ok(raid.length >= 1);
  assert.equal(typeof raid[0].source.commit, 'string');

  writeFileSync(join(repo, 'dirt.txt'), 'uncommitted');
  assert.throws(() => generate({ repo, pool: pools[0], run: wobbly }), /uncommitted changes/);

  // Its own pool: the same fixture bytes would otherwise dedup against the ids already raided above.
  const bare = join(scratch, 'bare');
  cpSync(FIXTURE, bare, { recursive: true });
  generate({ repo: bare, pool: pools[1], run: wobbly });
  const outside = loadPool(pools[1]).raid;
  assert.ok(outside.length >= 1, 'the non-git repo produced entries');
  assert.equal(outside[0].source.commit, null);
});

test('an id cannot be admitted to both sets', () => {
  const pool = { heldout: [], raid: [], runs: [] };
  admit(pool, { id: 'raid1' }, 'raid');
  assert.throws(() => admit(pool, { id: 'raid1' }, 'heldout'), /already in the raid set/);
  admit(pool, { id: 'hold1' }, 'heldout');
  assert.throws(() => admit(pool, { id: 'hold1' }, 'raid'), /already in the held-out set/);
});

const task = (id, day, verdict) => ({
  id,
  method: 'procedural:binop',
  stack: 'node',
  created: `2025-01-${String(day).padStart(2, '0')}T00:00:00Z`,
  review: verdict ? { verdict, at: `2025-02-${String(day).padStart(2, '0')}T00:00:00Z` } : null,
  decisions: 0,
  retired: false,
});

test('draw opens at the quorum and hands out genuine tasks oldest-first, vacuous never', () => {
  const pool = { heldout: [], raid: [], runs: [] };
  for (let day = 1; day <= 19; day++) pool.heldout.push(task(`t${day}`, day, day % 3 ? 'genuine' : 'vacuous'));
  pool.heldout.push(task('t20', 20, null)); // accepted but unreviewed
  assert.throws(() => draw(pool, 5), new RegExp(`pool not open: 19/${REVIEW_QUORUM}`));

  review(pool, 't20', 'genuine');
  const drawn = draw(pool, 100);
  const expected = pool.heldout.filter((e) => e.review.verdict === 'genuine').sort((a, b) => a.created.localeCompare(b.created));
  assert.deepEqual(drawn.map((e) => e.id), expected.map((e) => e.id), 'oldest first, vacuous and unreviewed absent');
  assert.ok(!drawn.some((e) => e.review.verdict === 'vacuous'));

  assert.throws(() => review(pool, 'nope', 'genuine'), /no held-out task nope/);
  assert.throws(() => review(pool, 't1', 'maybe'), /genuine or vacuous/);
});

test('record retires a task after three decisions, and it stops being drawn', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  const pool = { heldout: [], raid: [], runs: [] };
  for (let day = 1; day <= REVIEW_QUORUM; day++) pool.heldout.push(task(`t${day}`, day, 'genuine'));
  assert.ok(draw(pool, REVIEW_QUORUM).includes(pool.heldout[0]));

  record(pool, ['t1']);
  assert.equal(pool.heldout[0].decisions, 1);
  assert.equal(pool.heldout[0].retired, false);
  assert.ok(draw(pool, REVIEW_QUORUM).some((e) => e.id === 't1'), 'still drawable before the third decision');

  const result = record(pool, ['t1', 't1']);
  assert.deepEqual(result, { recorded: 2, retired: ['t1'] });
  assert.equal(pool.heldout[0].decisions, RETIRE_AFTER);
  assert.ok(!draw(pool, REVIEW_QUORUM).some((e) => e.id === 't1'), 'a retired task is never drawn');
  assert.throws(() => record(pool, ['nope']), /no held-out task nope/);

  savePool(dir, pool);
  assert.deepEqual(loadPool(dir), pool, 'the pool round-trips through its files');
});

test('inventory counts the pool and the gate’s pass rate', () => {
  const fresh = task('new', 1, 'genuine');
  fresh.created = new Date().toISOString();
  const pool = {
    heldout: [fresh, task('old', 2, 'vacuous'), task('older', 3, 'genuine'), task('ancient', 4, 'genuine')],
    raid: [{ method: 'procedural:literal' }, { method: 'procedural:literal' }],
    runs: [
      { candidates: 10, admitted: 4, cost_usd: 0 },
      { candidates: 5, admitted: 3, cost_usd: 0 },
    ],
  };
  // Three of the four are dated 2025, which is more than 90 days before any run of this test.
  const inv = inventory(pool);
  assert.equal(inv.heldout, 4);
  assert.deepEqual(inv.by_status, { genuine: 3, vacuous: 1, 'pending-review': 0, retired: 0 });
  assert.equal(inv.raid, 2);
  assert.deepEqual(inv.raid_by_method, { 'procedural:literal': 2 });
  assert.equal(inv.age['<30d'], 1);
  assert.equal(inv.age['>90d'], 3);
  assert.equal(inv.gate.pass_rate, 7 / 15);
  assert.equal(inv.gate.cost_per_accepted_usd, 0);
});
