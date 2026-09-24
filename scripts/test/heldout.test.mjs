import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { formatComparison, scoreBranch, scoreHeldout, scoreTask, summarizeRun } from '../bench.mjs';
import { RETIRE_AFTER } from '../bench/synth.mjs';
import { DEFAULT_MAX_GAP, addCompanion, companionsFor, familyGaps, formatGaps, gapVerdict, loadSet, lockSet, newSet, recordDecisions, saveSet } from '../bench/heldout.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const FIXTURE = join(ROOT, 'scripts/bench/fixture');
const TASKS = JSON.parse(readFileSync(join(ROOT, 'scripts/bench/tasks.json'), 'utf8'));
const AT = '2026-09-24T00:00:00Z';

// The sandbox exports these globally; left in, they point every test git at the wrong work tree.
const env = { ...process.env };
for (const gone of ['NODE_TEST_CONTEXT', 'NODE_OPTIONS', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']) delete env[gone];
const git = (args, cwd) => execFileSync('git', ['-c', 'user.email=bench@test', '-c', 'user.name=bench test', ...args], { cwd, encoding: 'utf8', env });

const filesUnder = (root) =>
  readdirSync(root, { recursive: true, withFileTypes: true })
    .filter((e) => e.isFile() && !/(^|\/)(\.git|node_modules)(\/|$)/.test(relative(root, e.parentPath)))
    .map((e) => relative(root, join(e.parentPath, e.name)));

test('familyGaps puts the worst gap first, and a task without a pull request fails both', () => {
  const gaps = familyGaps([
    { id: 'add-helper', pr_url: 'p', visible: true, heldout: true },
    { id: 'cart-rounding', pr_url: 'p', visible: true, heldout: false },
    { id: 'readme-typo', pr_url: null, visible: null, heldout: null },
    { id: 't9', family: 'ambiguous-rounding', pr_url: 'p', visible: false, heldout: false },
  ]);
  assert.deepEqual(gaps.map((g) => g.family), ['cart-rounding', 'add-helper', 'ambiguous-rounding', 'readme-typo'], 'worst gap first, ties by name');
  assert.deepEqual(gaps[0], { family: 'cart-rounding', tasks: 1, visible: 1, heldout: 0, gap: 1 });
  assert.deepEqual(gaps[3], { family: 'readme-typo', tasks: 1, visible: 0, heldout: 0, gap: 0 }, 'never scored: failing both, gap 0');
});

test('gapVerdict passes at the threshold and fails above it, naming the family and numbers', () => {
  const gaps = [
    { family: 'add-helper', visible: 1, heldout: 0.75, gap: 0.25 },
    { family: 'cart-rounding', visible: 1, heldout: 0, gap: 1 },
  ];
  assert.equal(DEFAULT_MAX_GAP, 0.25);
  assert.equal(gapVerdict(gaps).ok, false, 'the default threshold applies');
  assert.equal(gapVerdict([gaps[0]], 0.25).ok, true, 'a gap equal to the threshold passes');
  assert.deepEqual(gapVerdict(gaps, 0.5).failures, ['cart-rounding: visible 100%, held-out 0%, gap 100% > 50%']);
});

test('a companion retires after RETIRE_AFTER decisions, and the retirement bumps the version', () => {
  const set = { version: 4, checks: [{ id: 'cart-rounding.2', family: 'cart-rounding', file: 'cart-rounding.2.test.mjs', decisions: 0, retired: false }], history: [] };
  assert.deepEqual(recordDecisions(set, ['cart-rounding.2'], AT), []);
  assert.equal(set.version, 4, 'no retirement, no bump');
  recordDecisions(set, ['cart-rounding.2'], AT);
  assert.deepEqual(recordDecisions(set, ['cart-rounding.2'], AT), ['cart-rounding.2']);
  assert.equal(set.checks[0].decisions, RETIRE_AFTER);
  assert.equal(set.checks[0].retired, true);
  assert.equal(set.version, 5);
  assert.deepEqual(set.history, [{ version: 5, at: AT, added: [], retired: ['cart-rounding.2'] }]);
  assert.throws(() => recordDecisions(set, ['nope'], AT), /no held-out companion nope/);
});

test('companionsFor resolves one active companion per family, or names every family without one', () => {
  const set = {
    dir: '/tmp/set',
    checks: [
      { id: 'add-helper.1', family: 'add-helper', retired: false },
      { id: 'add-helper.0', family: 'add-helper', retired: true },
      { id: 'cart-rounding.1', family: 'cart-rounding', retired: true },
    ],
  };
  assert.deepEqual([...companionsFor(set, ['add-helper']).keys()], ['add-helper'], 'the retired one is skipped');
  assert.throws(() => companionsFor(set, ['add-helper', 'cart-rounding', 'readme-typo']), /no active held-out companion for cart-rounding, readme-typo/);
});

test('addCompanion copies the check in, rotates the set, and a family keeps one active companion', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-heldout-set-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  const source = join(dir, 'my-check.test.mjs');
  writeFileSync(source, '// companion\n');

  const set = newSet(dir);
  const first = addCompanion(set, { family: 'cart-rounding', check: source, now: AT });
  assert.equal(first.id, 'cart-rounding.1');
  assert.equal(first.file, 'cart-rounding.1.test.mjs');
  assert.ok(existsSync(join(dir, first.file)), 'the check is copied into the set');
  assert.equal(set.version, 2);
  assert.deepEqual(set.history, [{ version: 2, at: AT, added: ['cart-rounding.1'], retired: [] }]);
  assert.throws(() => addCompanion(set, { family: 'cart-rounding', check: source, now: AT }), /already has an active companion/);
  assert.equal(addCompanion(set, { family: 'readme-typo', check: source, now: AT }).id, 'readme-typo.2');

  saveSet(set);
  assert.deepEqual(loadSet(dir), set, 'the manifest round-trips');

  const unlock = lockSet(dir);
  assert.throws(() => lockSet(dir), /another bench command/);
  unlock();
  lockSet(dir)(); // free again once the lock is released
});

test('a manifest path that escapes the set is refused, and so is a set inside this repository', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-heldout-esc-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(
    join(dir, 'heldout.json'),
    JSON.stringify({ version: 1, checks: [{ id: 'x.1', family: 'x', file: '../outside.test.mjs', decisions: 0, retired: false }], history: [] }),
  );
  assert.throws(() => loadSet(dir), /escapes the held-out set/);
  assert.throws(() => loadSet(join(ROOT, 'scripts/bench')), /inside this repository's working tree/);
  assert.throws(() => newSet(join(ROOT, 'scripts/bench')), /inside this repository's working tree/);
  // Not bypassed by a link into the tree, by a set created through one, or by a sibling named `..kept`.
  const linked = join(dir, 'into-the-tree');
  symlinkSync(join(ROOT, 'scripts/bench'), linked);
  assert.throws(() => loadSet(linked), /inside this repository's working tree/);
  assert.throws(() => newSet(join(linked, 'new-set')), /inside this repository's working tree/);
  assert.throws(() => newSet(join(ROOT, '..kept')), /inside this repository's working tree/);
});

test('a result carries both check results and the scoring time, and a companion never changes passed', () => {
  const task = TASKS.tasks.find((t) => t.id === 'cart-rounding');
  const session = { id: 's1', status: 'pr_opened', pr_url: 'https://…/1', branch: 'b' };
  const scored = scoreTask({
    task,
    session,
    answers: [],
    timed_out: false,
    branchScore: { check: true, outside: [] },
    colony: null,
    heldout: { companion: 'cart-rounding.1', heldout: false },
    scoring: { visible_ms: 1200, heldout_ms: 900 },
  });
  assert.equal(scored.family, 'cart-rounding', 'the family the gap report groups by');
  assert.equal(scored.visible, true);
  assert.equal(scored.heldout, false);
  assert.deepEqual(scored.scoring, { visible_ms: 1200, heldout_ms: 900 });
  assert.equal(scored.passed, true, 'held-out results do not change what passed');

  const unscored = scoreTask({ task, session: { id: 's2', status: 'no_changes', pr_url: null }, answers: [], timed_out: false, branchScore: null, colony: null });
  assert.equal(unscored.visible, null);
  assert.equal(unscored.heldout, null);
  assert.deepEqual(unscored.scoring, { visible_ms: null, heldout_ms: null });
  assert.equal(summarizeRun([scored, unscored]).scoring_ms, 2100);
});

test('compare names the set version each run was scored against, and flags a rotation', () => {
  const run = (version) => ({
    label: 'run',
    results: [],
    heldout: { version, max_gap: 0.25, families: [{ family: 'cart-rounding', tasks: 1, visible: 1, heldout: 0, gap: 1 }], failures: [] },
  });
  assert.match(formatComparison(run(1), run(2)), /held-out set v1 max gap 100% → held-out set v2 max gap 100%/);
  assert.match(formatComparison(run(1), run(2)), /not comparable across the rotation/);
  assert.doesNotMatch(formatComparison(run(2), run(2)), /not comparable/, 'the same scored version compares');
  assert.match(formatComparison({ label: 'r', results: [] }, run(2)), /no held-out suite scored → held-out set v2/);
});

// A companion for cart-rounding, on carts the visible check never names. The honest fix (integer cents,
// as scripts/bench/reference/cart-rounding) passes it; a branch that special-cases exactly the visible
// check's inputs passes the visible check and fails here — that is the gap the suite exists to catch.
const companionSource = `import assert from 'node:assert/strict';
import { test } from 'node:test';
import { total } from './src/cart.js';

test('carts the visible check never names add up too', () => {
  assert.equal(total([{ price: 0.05, quantity: 3 }]), 0.15);
  assert.equal(total([{ price: 0.1, quantity: 2 }, { price: 0.25, quantity: 1 }]), 0.45);
});
`;
const overfitSource = `// "Fixed" by making exactly the visible check's carts come out right.
export function total(items) {
  const key = JSON.stringify(items);
  if (key === JSON.stringify([{ price: 0.1, quantity: 1 }, { price: 0.2, quantity: 1 }, { price: 0.1, quantity: 1 }])) return 0.4;
  if (key === JSON.stringify([{ price: 1.15, quantity: 3 }])) return 3.45;
  return items.reduce((sum, item) => sum + item.price * item.quantity, 0);
}
`;

test('calibration, over a real local repo: the overfit branch gaps out, the honest one does not', (ctx) => {
  const scratch = mkdtempSync(join(tmpdir(), 'colonizer-heldout-cal-'));
  ctx.after(() => rmSync(scratch, { recursive: true, force: true }));
  const repo = join(scratch, 'repo');
  const setDir = join(scratch, 'heldout');
  const task = TASKS.tasks.find((t) => t.id === 'cart-rounding');

  cpSync(FIXTURE, repo, { recursive: true });
  git(['init', '-q', '-b', 'main'], repo);
  git(['add', '-A'], repo);
  git(['-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'the fixture'], repo);
  const branch = (name, files) => {
    git(['checkout', '-q', '-b', name], repo);
    for (const [file, source] of Object.entries(files)) writeFileSync(join(repo, file), source);
    git(['add', '-A'], repo);
    git(['-c', 'commit.gpgsign=false', 'commit', '-q', '-m', name], repo);
    git(['checkout', '-q', 'main'], repo);
  };
  branch('colonizer/overfit', { 'src/cart.js': overfitSource });
  branch('colonizer/honest', {
    'src/cart.js': readFileSync(join(ROOT, 'scripts/bench/reference/cart-rounding/src/cart.js'), 'utf8'),
    'src/cart.test.js': readFileSync(join(ROOT, 'scripts/bench/reference/cart-rounding/src/cart.test.js'), 'utf8'),
  });

  // The companion enters through the same door the CLI uses.
  mkdirSync(setDir, { recursive: true });
  writeFileSync(join(scratch, 'companion.test.mjs'), companionSource);
  const set = newSet(setDir);
  addCompanion(set, { family: 'cart-rounding', check: join(scratch, 'companion.test.mjs'), now: AT });
  saveSet(set);
  const companion = loadSet(setDir).checks[0];

  const score = (name) => {
    const branchScore = scoreBranch({ repo, branch: name, source: repo, task });
    const heldout = scoreHeldout({ repo, branch: name, source: repo, heldoutDir: setDir, companion });
    return scoreTask({ task, session: { id: name, status: 'pr_opened', pr_url: `local://${name}`, branch: name }, answers: [], timed_out: false, branchScore, colony: null, heldout, scoring: { visible_ms: 1, heldout_ms: 1 } });
  };

  const overfit = score('colonizer/overfit');
  assert.equal(overfit.visible, true, 'the overfit branch passes the visible check');
  assert.equal(overfit.heldout, false, '…and fails the companion it was never written for');
  const gaps = familyGaps([overfit]);
  const verdict = gapVerdict(gaps);
  assert.equal(verdict.ok, false);
  assert.deepEqual(verdict.failures, ['cart-rounding: visible 100%, held-out 0%, gap 100% > 25%']);
  assert.match(formatGaps(gaps, { version: set.version, maxGap: DEFAULT_MAX_GAP }), /\| \*\*cart-rounding\*\* \| \*\*100%\*\* \| \*\*0%\*\* \| \*\*100%\*\* \|/);

  const honest = score('colonizer/honest');
  assert.equal(honest.visible, true);
  assert.equal(honest.heldout, true);
  const honestGaps = familyGaps([honest]);
  assert.equal(honestGaps[0].gap, 0);
  assert.equal(gapVerdict(honestGaps).ok, true);

  const fresh = loadSet(setDir);
  recordDecisions(fresh, [companion.id], AT);
  assert.equal(fresh.checks[0].decisions, 1, 'a scored companion carries a decision');
});

test('no held-out material reaches the agents', () => {
  // The scratch repository is seeded from the fixture, and the colonies read the issues they get.
  for (const file of filesUnder(FIXTURE)) {
    assert.ok(!/heldout/i.test(file) && !/heldout/i.test(readFileSync(join(FIXTURE, file), 'utf8')), `${file} must not carry held-out material`);
  }
  for (const t of TASKS.tasks) assert.ok(!/heldout/i.test(`${t.title}\n${t.body}`), `${t.id} must not mention the held-out suite`);
  // What else the colonies are staged with: the agent modules and the vendored skills.
  for (const tree of ['modules', 'vendor']) {
    for (const file of filesUnder(join(ROOT, tree))) {
      if (!/\.(md|mjs|js|json|rs|toml|txt|sh|lock)$/.test(file)) continue;
      assert.ok(!/heldout/i.test(readFileSync(join(ROOT, tree, file), 'utf8')), `${tree}/${file} must not mention the held-out suite`);
    }
  }
  // And no manifest is committed anywhere in this repository — the set lives outside, on purpose.
  const tracked = execFileSync('git', ['ls-files'], { cwd: ROOT, encoding: 'utf8', env });
  assert.ok(!tracked.split('\n').some((f) => /(^|\/)heldout\.json$/.test(f)), `heldout.json is committed: ${tracked.split('\n').filter((f) => /heldout\.json$/.test(f)).join(', ')}`);
});
