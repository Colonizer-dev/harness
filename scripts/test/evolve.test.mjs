import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pathToFileURL } from 'node:url';

import { compareProposal } from '../trajectory-monitor.mjs';
import { childEnv } from '../bench.mjs';
import { diagnose, evaluate, findProposal, overallVerdict, parseProposal, propose, renderProposal, replaceConstant, retain, scorecard, verdict } from '../evolve.mjs';

const AT = '2026-09-27T00:00:00Z';
// A colony sandbox exports GIT_DIR and friends globally; left in, they point every test git at the wrong work tree.
const git = (args, cwd) => execFileSync('git', ['-c', 'user.email=e@test', '-c', 'user.name=evolve test', ...args], { cwd, encoding: 'utf8', env: childEnv() });

// --------------------------------------------------------------------------------------- fixtures

const TOKENS = { read: 10, search: 0, command_output: 5, edit: 20, reasoning: 30, replay: 40 };
const result = (over = {}) => ({ id: 'add-helper', family: 'add-helper', agent: 'claude-code', passed: true, failures: [], clean: true, hacks: [], session_id: 's1', cost_usd: 0.2, routed_cost_usd: null, total_cost_usd: 0.2, token_categories: { ...TOKENS }, ...over });
const run = (label, results) => ({ label, repo: 'owner/bench', heldout: { version: 3 }, results });
const makeCard = (baseRows, candRows) =>
  scorecard(
    baseRows.map(([id, passed, cost]) => result({ id, passed, clean: passed, cost_usd: cost })),
    candRows.map(([id, passed, cost]) => result({ id, passed, clean: passed, cost_usd: cost })),
  );

// --------------------------------------------------------------------------------------- diagnose

test('bench failure strings land in their class, with evidence that links back', () => {
  const d = diagnose({
    bench: [
      run('after', [
        result({ id: 'cart-rounding', session_id: 's2', passed: false, failures: ['asked 2 questions, expected 0'] }),
        result({ id: 'readme-typo', session_id: 's3', passed: false, clean: false, hacks: ['weakened-test'], failures: ['changed files outside the task: src/cart.js'] }),
      ]),
    ],
  });
  const byId = Object.fromEntries(d.classes.map((c) => [c.id, c]));
  assert.deepEqual(d.classes.map((c) => c.id), ['module:claude-code/questions', 'module:claude-code/reward-hack', 'module:claude-code/scope'], 'count desc, then id');
  assert.deepEqual(byId['module:claude-code/questions'].evidence, [{ source: 'bench', label: 'after', task: 'cart-rounding', session: 's2' }]);
  assert.equal(byId['module:claude-code/scope'].evidence[0].task, 'readme-typo');
  assert.deepEqual([d.repo, d.heldout_version], ['owner/bench', 3]);
});

test('every kind the bench can name is mapped, and unknown text falls back', () => {
  const failures = [['timed out', 'timeout'], ['no pull request (failed)', 'no-pr'], ['the check failed', 'hidden-check'], ['it broke the existing tests', 'tests'], ['changed files outside the task: x', 'scope'], ['asked 3 questions, expected 0', 'questions'], ['something nobody has seen', 'other']];
  const d = diagnose({ bench: [run('after', failures.map(([f], i) => result({ id: `t${i}`, passed: false, failures: [f] })))] });
  assert.deepEqual(Object.fromEntries(d.classes.map((c) => [c.kind, c.count])), { timeout: 1, 'no-pr': 1, 'hidden-check': 1, tests: 1, scope: 1, questions: 1, other: 1 });
});

test('red-team defects cluster by the module their files name; unvalidated ones never appear', () => {
  const d = diagnose({
    redteam: [
      { defect: 'runner lets a subagent push', severity: 'high', validation: 'validated', reproduction: 'reproduced', files: ['modules/agents/claude-code/runner.mjs', 'modules/agents/claude-code/subagents.mjs'], hunters: ['h1', 'h2'] },
      { defect: 'the pi runner swallows errors', severity: 'medium', validation: 'validated', reproduction: 'unconfirmed', files: ['modules/agents/pi/runner.mjs'], hunters: ['h3'] },
      { defect: 'a rejected defect', validation: 'rejected', files: ['modules/agents/pi/runner.mjs'], hunters: ['h3'] },
      { defect: 'an unvalidated defect', validation: 'unvalidated', files: [], hunters: [] },
      { defect: 'touches no module', severity: 'low', validation: 'validated', files: ['docs/bench.md'], hunters: ['h1'] },
    ],
  });
  const byId = Object.fromEntries(d.classes.map((c) => [c.id, c]));
  assert.deepEqual(d.classes.map((c) => c.id), ['module:claude-code/redteam', 'module:pi/redteam', 'repo:docs/bench.md/redteam']);
  assert.deepEqual(byId['module:claude-code/redteam'].evidence[0], { source: 'redteam', defect: 'runner lets a subagent push', sessions: ['h1', 'h2'], severity: 'high', reproduction: 'reproduced' });
  assert.equal(byId['module:pi/redteam'].count, 1, 'the rejected and unvalidated lines are ignored');
});

test('findings ledgers fold by title over their stage lines, keeping only validated ones', () => {
  const d = diagnose({
    findings: [
      { session: 's9', title: 'the map misses vendored plugins', state: 'validated', severity: 'medium' },
      { session: 's9', title: 'the map misses vendored plugins', state: 'filed', issue: 'https://…/1' },
      { session: 's9', title: 'the map misses vendored plugins', state: 'merged', pr: 'https://…/2' },
      { session: 's10', title: 'a rejected finding', state: 'validated', severity: 'low' },
      { session: 's10', title: 'a rejected finding', state: 'rejected', reason: 'stale' },
      { session: 's11', title: 'a blocked finding', state: 'blocked' },
      { session: 's12', title: 'not a state the ledger knows', state: 'toString' },
    ],
  });
  assert.equal(d.classes.length, 1, 'both findings sit on one repo-wide surface, folded by title; non-states stay out');
  assert.deepEqual(d.classes[0], { id: 'repo:unassigned/defect', surface: 'repo:unassigned', kind: 'defect', count: 2, evidence: [
    { source: 'findings', title: 'a rejected finding', sessions: ['s10'], severity: 'low', state: 'validated' },
    { source: 'findings', title: 'the map misses vendored plugins', sessions: ['s9'], severity: 'medium', state: 'merged' },
  ] });
});

test('class ids and order are stable whatever order the inputs arrive in', () => {
  const bench = [run('b', [result({ id: 't2', passed: false, failures: ['the check failed'] })]), run('a', [result({ id: 't1', passed: false, failures: ['timed out'] })])];
  assert.deepEqual(diagnose({ bench: [...bench].reverse() }).classes, diagnose({ bench }).classes);
});

test('colony transcripts join questions and tool errors into bench evidence', () => {
  const d = diagnose({ bench: [run('after', [result({ session_id: 's1', failures: ['timed out'] })])], sessions: { s1: { questions: 4, tool_errors: 7 } } });
  assert.deepEqual(d.classes[0].evidence[0], { source: 'bench', label: 'after', task: 'add-helper', session: 's1', questions: 4, tool_errors: 7 });
});

// -------------------------------------------------------------------------------------- scorecard

test('scorecard computes per-task deltas, cost delta and percent, and token deltas', () => {
  const base = [result({ id: 'a', cost_usd: 0.2, clean: true }), result({ id: 'b', cost_usd: 0.3, passed: false, clean: null })];
  const cand = [result({ id: 'a', cost_usd: 0.25, clean: false, routed_cost_usd: 0.05 }), result({ id: 'b', cost_usd: 0.28, passed: true, clean: true, token_categories: { ...TOKENS, read: 14 } })];
  const c = scorecard(base, cand);
  assert.deepEqual(c.regressions, ['a'], 'a hacked pass is not a pass');
  assert.deepEqual(c.passed, { baseline: 1, candidate: 1 });
  assert.deepEqual(c.tasks.map((t) => t.delta), [-1, 1], 'a: clean pass to hacked pass; b: fail to pass');
  assert.deepEqual([c.tasks[0].base_cost, c.tasks[0].cand_cost], [0.2, 0.3], 'routed cost rides on top');
  assert.equal(c.totals.cost_delta.toFixed(2), '0.08');
  assert.ok(Math.abs(c.totals.cost_pct - 0.16) < 1e-9);
  assert.deepEqual([c.totals.token_delta.read, c.totals.token_delta.edit], [4, 0]);
  assert.deepEqual([c.score.baseline, c.score.candidate], [0.5, 0.5], 'the hacked pass does not raise the score');
  assert.deepEqual(c.gap, { baseline: 0, candidate: 0.5 }, 'the hacked pass sits inside the gap');
});

test('a candidate must cover exactly the baseline task set', () => {
  const base = [result({ id: 'a' }), result({ id: 'b' })];
  assert.throws(() => scorecard(base, [result({ id: 'a' })]), /task sets differ.*baseline-only: b/);
  assert.throws(() => scorecard(base, [result({ id: 'a' }), result({ id: 'b' }), result({ id: 'c' })]), /candidate-only: c/);
});

test('the verdicts: regression flags, over-bound cost and no gain discard, a clean gain retains', () => {
  // One task backslides while the average improves: flagged, never silently retained.
  const regressed = makeCard([['t1', true, 0.2], ['t2', false, 0.2], ['t3', false, 0.2]], [['t1', false, 0.2], ['t2', true, 0.2], ['t3', true, 0.2]]);
  assert.deepEqual(verdict(regressed, { maxCostIncrease: 0.10 }), { verdict: 'flagged', reasons: ['regressed from pass to fail: t1'] });
  // Cost over the bound says no, whatever the score did; the bound moves with the flag.
  const costly = makeCard([['t1', false, 0.5], ['t2', false, 0.5]], [['t1', true, 0.75], ['t2', false, 0.75]]);
  assert.match(verdict(costly, { maxCostIncrease: 0.10 }).reasons[0], /cost rose 50%.*over the 10% bound/);
  assert.equal(verdict(costly, { maxCostIncrease: 0.60 }).verdict, 'retained');
  // No improvement, no retention; a strictly better score within the bound is retained.
  assert.match(verdict(makeCard([['t1', true, 0.2]], [['t1', true, 0.2]]), {}).reasons[0], /score not better: 1\/1 → 1\/1/);
  assert.equal(verdict(makeCard([['t1', false, 0.2], ['t2', false, 0.2]], [['t1', true, 0.21], ['t2', false, 0.2]]), { maxCostIncrease: 0.10 }).verdict, 'retained');
  // A base cost of zero cannot bound a rise: the percent is infinite, not a division by zero.
  const fromFree = scorecard([result({ id: 't1', cost_usd: 0, clean: true })], [result({ id: 't1', cost_usd: 0.5, clean: true })]);
  assert.equal(verdict(fromFree, { maxCostIncrease: 0.10 }).verdict, 'discarded');
});

test('an audited pair answers to compareProposal: a widened gap is flagged, never retained', () => {
  const shaped = (id, passed, clean) => result({ id, passed, clean });
  const base = [shaped('t1', true, true), shaped('t2', false, null), shaped('t3', false, null)];
  // The candidate resolves t2, but its event log is missing, so the pass is unaudited and the gap widens.
  const cand = [shaped('t1', true, true), shaped('t2', true, null), shaped('t3', false, null)];
  const c = scorecard(base, cand);
  assert.deepEqual([c.score.candidate, c.pass_rate.candidate, c.clean_rate.candidate], [2 / 3, 2 / 3, 1 / 3], 'an unaudited pass counts toward the score, not the clean rate');
  assert.ok(c.gap.candidate > c.gap.baseline);
  assert.deepEqual(verdict(c, {}).reasons, ['raised raw score while widening the gap']);
  // And the rule really is compareProposal's, not a reimplementation.
  assert.equal(compareProposal({ clean_rate: c.clean_rate.baseline, gap: c.gap.baseline, resolved_rate: c.pass_rate.baseline }, { clean_rate: c.clean_rate.candidate, gap: c.gap.candidate, resolved_rate: c.pass_rate.candidate }).accept, false);
});

test('reproduction: retained only when every candidate run agrees, scorecard always kept', () => {
  assert.deepEqual([overallVerdict(['retained', 'retained']), overallVerdict(['retained', 'flagged']), overallVerdict(['flagged', 'discarded'])], ['retained', 'flagged', 'discarded']);
  const good = makeCard([['t1', false, 0.2]], [['t1', true, 0.2]]);
  const bad = makeCard([['t1', false, 0.2]], [['t1', true, 0.4]]);
  const r = retain({ status: 'evaluated', evaluations: [{ candidate: 'one', card: good }, { candidate: 'two', card: bad }] }, { maxCostIncrease: 0.10 });
  assert.equal(r.verdict.verdict, 'discarded', 'one disagreeing run is enough');
  assert.deepEqual(r.verdict.runs.map((x) => x.verdict), ['retained', 'discarded']);
  assert.deepEqual(r.verdict.reasons, ['two: cost rose 100% ($0.20 → $0.40), over the 10% bound'], 'the reasons name the runs that decided');
  assert.equal(r.evaluations[0].card.tasks.length, 1, 'the scorecard is kept whatever the verdict');
  assert.throws(() => retain({ status: 'proposed', evaluations: [] }), /evaluate the proposal first/);
  assert.throws(() => retain({ status: 'evaluated', evaluations: [] }), /no scorecard to judge/);
});

// -------------------------------------------------------------------------------------- proposals

const PROPOSAL = {
  id: 'module-claude-code-questions', class: 'module:claude-code/questions', kind: 'questions', surface: 'module:claude-code',
  status: 'proposed', module: 'claude-code', file: 'modules/agents/claude-code/runner.mjs', constant: 'SYSTEM_PROMPT_APPEND',
  base_commit: 'abc123', repo: 'owner/bench', tasks: ['cart-rounding'], heldout_version: 3, created_at: AT,
  evidence: [{ source: 'bench', label: 'after', task: 'cart-rounding', session: 's2', questions: 2, tool_errors: 0 }],
  text: 'new prompt',
  diff: ['diff --git a/x b/x', '--- a/x', '+++ b/x', '@@ -1 +1 @@', '-old', '+```', '+new'].join('\n'),
  reproduce: ['git checkout abc123'],
  evaluations: [], verdict: null, approved_at: null, rejected_at: null, reject_reason: null,
  review: ['docs updated?'],
};

test('renderProposal and parseProposal round-trip, even through a diff containing ```', () => {
  assert.deepEqual(parseProposal(renderProposal(PROPOSAL)), PROPOSAL);
  const rendered = renderProposal(PROPOSAL);
  assert.match(rendered, /^# Proposal module-claude-code-questions — proposed$/m);
  assert.match(rendered, /````diff/, 'the diff fence outgrows the ``` run inside it');
  assert.match(rendered, /- \[ \] docs updated\?/);
  // A diff with no backticks at all keeps the plain fence.
  assert.match(renderProposal({ ...PROPOSAL, diff: 'diff --git a/x b/x' }), /```diff\n/);
});

test('evaluate scores each candidate run against the baseline and refuses mismatched tasks', () => {
  const p = evaluate({ ...PROPOSAL, status: 'proposed' }, run('before', [result({ id: 'a', passed: false, clean: null })]), [{ name: 'after.json', label: 'after', results: [result({ id: 'a', passed: true, clean: true })] }]);
  assert.deepEqual([p.status, p.verdict], ['evaluated', null], 'an old verdict does not survive a re-evaluation');
  assert.equal(p.evaluations[0].candidate, 'after · after.json');
  assert.equal(p.evaluations[0].card.score.candidate, 1);
  assert.throws(() => evaluate(p, run('before', [result({ id: 'a' })]), [{ results: [result({ id: 'zzz' })] }]), /task sets differ/);
  assert.throws(() => evaluate({ ...PROPOSAL, status: 'approved' }, run('b', []), []), /cannot be re-evaluated/);
});

// ---------------------------------------------------------------------------------------- propose

const RUNNER = [
  '// A stand-in runner with its prompt constants in module style.',
  '',
  'export const SYSTEM_PROMPT_APPEND = [',
  "  'line one',",
  "  'line two (with parens)',",
  "].join('\\n');",
  '',
  "export const NOT_A_PROMPT = 'no';",
  '',
].join('\n');

/** A throwaway git repository laid out like the harness, with a fake agent module in it. */
function fixtureRepo() {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-evolve-test-'));
  mkdirSync(join(dir, 'modules/agents/x'), { recursive: true });
  writeFileSync(join(dir, 'modules/agents/x/runner.mjs'), RUNNER);
  writeFileSync(join(dir, 'README.md'), '# fixture\n');
  git(['init', '-q', '-b', 'main'], dir);
  git(['add', '-A'], dir);
  git(['-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'fixture'], dir);
  return dir;
}

const CLASSES = { repo: 'owner/bench', heldout_version: 3, classes: [{ id: 'module:x/questions', surface: 'module:x', kind: 'questions', count: 1, evidence: [{ source: 'bench', label: 'after', task: 'cart-rounding', session: 's2' }] }] };

test('propose replaces one prompt constant, and the diff applies to the tree', async () => {
  const root = fixtureRepo();
  try {
    const p = propose({ classes: CLASSES, classId: 'module:x/questions', module: 'x', constant: 'SYSTEM_PROMPT_APPEND', text: 'line A\nline B\'s', root });
    assert.equal(p.file, 'modules/agents/x/runner.mjs');
    assert.deepEqual(p.tasks, ['cart-rounding']);
    assert.match(p.base_commit, /^[0-9a-f]{40}$/);
    assert.ok(p.diff.startsWith('diff --git a/modules/agents/x/runner.mjs b/modules/agents/x/runner.mjs'), p.diff);
    assert.ok(!p.diff.includes('index '), 'no index line: the patch applies by context');
    assert.ok(!/[+-]export const NOT_A_PROMPT/.test(p.diff), 'only the named constant changes');
    // The patch applies cleanly to the real tree and leaves a module that still parses and imports.
    writeFileSync(join(root, 'p.diff'), p.diff);
    git(['apply', 'p.diff'], root);
    const mod = await import(pathToFileURL(join(root, 'modules/agents/x/runner.mjs')).href);
    assert.equal(mod.SYSTEM_PROMPT_APPEND, 'line A\nline B\'s');
    assert.equal(mod.NOT_A_PROMPT, 'no', 'the rest of the file is untouched');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the prompt-only guard refuses anything that is not a prompt constant', () => {
  const root = fixtureRepo();
  try {
    // NOT_A_PROMPT stays allowed: the guard is on the name, and that name does end in _PROMPT.
    for (const constant of ['system_prompt_append', 'PROMPTS', 'SYSTEM_PROMPT_APPEND_EXTRA']) {
      assert.throws(() => propose({ classes: CLASSES, classId: 'module:x/questions', module: 'x', constant, text: 'x', root }), /prompt constant/, constant);
    }
    assert.throws(() => propose({ classes: CLASSES, classId: 'module:x/questions', module: 'x', constant: 'MEMORY_PROMPT_APPEND', text: 'x', root }), /declares export/, 'a prompt constant the module does not have');
    assert.throws(() => propose({ classes: CLASSES, classId: 'nope', module: 'x', constant: 'SYSTEM_PROMPT_APPEND', text: 'x', root }), /is not in the diagnosis/);
    assert.throws(() => propose({ classes: CLASSES, classId: 'module:x/questions', module: 'no-such-module', constant: 'SYSTEM_PROMPT_APPEND', text: 'x', root }), /no agent module/);
    assert.throws(() => propose({ classes: CLASSES, classId: 'module:x/questions', module: 'x', constant: 'SYSTEM_PROMPT_APPEND', text: 'line one\nline two (with parens)', root }), /nothing to propose/, 'an unchanged text');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('replaceConstant keeps the module style and finds the declaration, not a mention', () => {
  assert.match(replaceConstant(RUNNER, 'SYSTEM_PROMPT_APPEND', 'a\nb'), /export const SYSTEM_PROMPT_APPEND = \[\n  'a',\n  'b',\n\]\.join\('\\n'\);/);
  assert.throws(() => replaceConstant(RUNNER, 'MISSING', 'x'), /not declared/);
  // A comment naming the constant above the real declaration does not become the splice point.
  const commented = `// export const SYSTEM_PROMPT_APPEND lies here\n${RUNNER}`;
  assert.match(replaceConstant(commented, 'SYSTEM_PROMPT_APPEND', 'a'), /^\/\/ export const SYSTEM_PROMPT_APPEND lies here\n\/\/ A stand-in runner/);
});

test('findProposal resolves an id in the queue and never joins a raw id onto the directory', () => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-evolve-queue-'));
  try {
    writeFileSync(join(dir, 'module-claude-code-questions.md'), renderProposal(PROPOSAL));
    writeFileSync(join(dir, 'module-claude-code-questions-2.md'), renderProposal({ ...PROPOSAL, id: 'module-claude-code-questions-2' }));
    writeFileSync(join(dir, 'broken.md'), 'a hand edit broke the json block');
    assert.equal(findProposal(dir, 'module-claude-code-questions').proposal.id, 'module-claude-code-questions', 'exact name');
    assert.throws(() => findProposal(dir, 'module-claude-code-q'), /matches 2 proposals/, 'an ambiguous prefix refuses');
    assert.equal(findProposal(dir, 'module-claude-code-questions-').proposal.id, 'module-claude-code-questions-2', 'a unique prefix resolves');
    assert.throws(() => findProposal(dir, 'no-such-id'), /no proposal no-such-id/);
    // ids are slugs: anything that could escape the queue directory is refused before it is joined.
    for (const evil of ['../outside', 'a/b', 'a\\b']) assert.throws(() => findProposal(dir, evil), /not a proposal id/, evil);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
