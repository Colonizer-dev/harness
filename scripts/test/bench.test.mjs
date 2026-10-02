import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { actTier, formatComparison, formatJevReport, formatRoutingReport, jevReport, journalScoring, outsideTask, parseArgs, routingOutcome, routingReport, routingVerdict, runCheck, runOwnTests, scoreTask, summarizeRun } from '../bench.mjs';
import { loadSpend, readJsonLines } from '../colony-report.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const TASKS = JSON.parse(readFileSync(join(ROOT, 'scripts/bench/tasks.json'), 'utf8'));

const task = { id: 'add-helper', expect: { pr: true, check: 'x', regression: true, changed_within: ['src/greet.js'], questions: 0 } };
const colony = { questions: 0, cost_usd: 0.2, working_ms: 60_000, wall_ms: 70_000, turns: 1, tool_calls: 9, tool_errors: 0, watchdog_nudges: 0, plain_text_reprompts: 0, subagents: 0 };
const clean = { check: true, regression: true, changed: ['src/greet.js'], outside: [] };
const session = { id: 's1', status: 'pr_opened', pr_url: 'https://…/1', branch: 'colonizer/issue-1-s1' };

test('a colony that did the task passes', () => {
  const r = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(r.passed, true);
  assert.deepEqual(r.failures, []);
  assert.equal(r.cost_usd, 0.2);
});

test('every way of failing is named', () => {
  const r = scoreTask({
    task,
    session,
    answers: [],
    timed_out: false,
    branchScore: { check: false, regression: false, changed: ['src/greet.js', 'src/cart.js'], outside: ['src/cart.js'] },
    colony: { ...colony, questions: 2 },
  });
  assert.equal(r.passed, false);
  assert.deepEqual(r.failures, [
    'the check failed',
    'it broke the existing tests',
    'changed files outside the task: src/cart.js',
    'asked 2 questions, expected 0',
  ]);
});

test('no pull request, and running out of time, are failures of their own', () => {
  const none = scoreTask({ task, session: { id: 's2', status: 'no_changes', pr_url: null }, answers: [], timed_out: false, branchScore: null, colony });
  assert.deepEqual(none.failures, ['no pull request (no_changes)']);
  const slow = scoreTask({ task, session: { id: 's3', status: 'running', pr_url: null }, answers: [], timed_out: true, branchScore: null, colony });
  assert.deepEqual(slow.failures, ['timed out', 'no pull request (running)']);
});

test('a task that should ask a question fails when it does not', () => {
  const asking = { id: 'ambiguous', expect: { pr: true, questions: 1 } };
  const silent = scoreTask({ task: asking, session, answers: [], timed_out: false, branchScore: clean, colony });
  assert.deepEqual(silent.failures, ['asked 0 questions, expected 1']);
  const asked = scoreTask({ task: asking, session, answers: [{ picked: 'Round up' }], timed_out: false, branchScore: clean, colony: { ...colony, questions: 1 } });
  assert.equal(asked.passed, true);
  assert.deepEqual(asked.answers, [{ picked: 'Round up' }]);
});

test('a run adds up', () => {
  const pass = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony });
  const fail = scoreTask({ task, session: { id: 's4', status: 'failed', pr_url: null }, answers: [], timed_out: false, branchScore: null, colony });
  const s = summarizeRun([pass, fail]);
  assert.equal(s.tasks, 2);
  assert.equal(s.passed, 1);
  assert.equal(s.pass_rate, 0.5);
  assert.equal(Number(s.cost_usd.toFixed(2)), 0.4);
});

test('a comparison shows what changed, per task', () => {
  const before = { label: 'before', results: [scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony })] };
  const after = {
    label: 'after',
    results: [scoreTask({ task, session: { id: 's5', status: 'failed', pr_url: null }, answers: [], timed_out: false, branchScore: null, colony: { ...colony, cost_usd: 0.5 } })],
  };
  const text = formatComparison(before, after);
  assert.match(text, /# before → after/);
  assert.match(text, /Passed 1\/1 → 0\/1/);
  assert.match(text, /pass → FAIL/);
  assert.match(text, /0\.20 → 0\.50 \(\+0\.30\)/);
  assert.match(text, /no pull request \(failed\)/);
});

test('a --timeout that would wait for NaN fails at parse, before any colony exists', () => {
  assert.equal(parseArgs(['run', '--timeout', '30']).timeoutMs, 30_000);
  for (const bad of [[], [''], ['soon'], ['0'], ['-5']]) {
    assert.throws(() => parseArgs(['run', '--repo', 'o/r', '--timeout', ...bad]), /--timeout needs/);
  }
});

test('a flag left dangling for its value fails at parse, with the flag named', () => {
  for (const flag of ['--repo', '--label', '--only', '--timeout', '--data']) {
    assert.throws(() => parseArgs(['run', '--repo', 'o/r', flag]), new RegExp(`${flag} needs a value`));
  }
});

test('every task names a check that exists, and the fixture is there', () => {
  assert.ok(existsSync(join(ROOT, TASKS.fixture, 'package.json')));
  assert.ok(TASKS.tasks.length >= 3);
  for (const t of TASKS.tasks) {
    assert.ok(t.id && t.title && t.body, `${t.id} needs a title and a body`);
    assert.ok(existsSync(join(ROOT, t.expect.check)), `${t.id} names a check that is not there: ${t.expect.check}`);
    assert.ok(Array.isArray(t.expect.changed_within) && t.expect.changed_within.length > 0, `${t.id} needs the files it may touch`);
    assert.equal(typeof t.expect.questions, 'number', `${t.id} needs how many questions it should ask`);
  }
});

// A check that passes on the untouched fixture scores every colony as right, and one that fails on a correct
// fix scores every colony as wrong. Each task's reference solution holds the files its fix changes.
for (const t of TASKS.tasks) {
  test(`${t.id}: the check fails on the bare fixture and passes on the reference solution`, (ctx) => {
    const reference = join(ROOT, 'scripts/bench/reference', t.id);
    assert.ok(existsSync(reference), `${t.id} has no reference solution: add the fixed files under scripts/bench/reference/${t.id}/`);
    const files = readdirSync(reference, { recursive: true, withFileTypes: true })
      .filter((e) => !e.isDirectory())
      .map((e) => relative(reference, join(e.parentPath, e.name)));
    assert.ok(files.length > 0, `${t.id}'s reference solution is empty`);
    assert.deepEqual(outsideTask(t, files), [], `${t.id}'s reference touches files outside its changed_within`);

    const dir = mkdtempSync(join(tmpdir(), 'colonizer-bench-reference-'));
    ctx.after(() => rmSync(dir, { recursive: true, force: true }));
    cpSync(join(ROOT, TASKS.fixture), dir, { recursive: true });
    assert.equal(runCheck(t, dir).passed, false, `${t.id}'s check passes on the untouched fixture, so it cannot tell a fix from none`);

    cpSync(reference, dir, { recursive: true });
    const fixed = runCheck(t, dir);
    assert.ok(fixed.passed, `${t.id}'s check fails on the reference solution:\n${fixed.output}`);
    assert.ok(runOwnTests(dir), `the fixture's own tests fail with ${t.id}'s reference solution applied`);
  });
}

test('routed cost is recorded per task and counted in the run and the comparison', () => {
  const plain = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(plain.routed_cost_usd, null);
  assert.equal(plain.total_cost_usd, 0.2);

  const routed = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony: { ...colony, routed_cost_usd: 0.3 } });
  assert.equal(routed.routed_cost_usd, 0.3);
  assert.equal(routed.total_cost_usd, 0.5);
  const fromSession = scoreTask({ task, session: { ...session, routed_cost_usd: 0.1 }, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(fromSession.routed_cost_usd, 0.1, 'the session record when the colony report has none');

  const s = summarizeRun([plain, routed]);
  assert.equal(Number(s.cost_usd.toFixed(2)), 0.4);
  assert.equal(Number(s.routed_cost_usd.toFixed(2)), 0.3);
  assert.equal(Number(s.total_cost_usd.toFixed(2)), 0.7);

  // A run saved before routed cost was recorded has no total_cost_usd; it still compares.
  const { routed_cost_usd, total_cost_usd, ...old } = plain;
  const text = formatComparison({ label: 'before', results: [old] }, { label: 'after', results: [routed] });
  assert.match(text, /0\.20 → 0\.50 \(\+0\.30\)/);
  assert.match(text, /Cost \$0\.20 → \$0\.50 \(routed \$0\.00 → \$0\.30\)/);
});

test('token categories ride through scoring, the run summary and the comparison', () => {
  const tokenCategories = { read: 100, search: 0, command_output: 200, edit: 400, reasoning: 50, replay: 1000 };
  const scored = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony: { ...colony, tokenCategories } });
  assert.deepEqual(scored.token_categories, tokenCategories);
  const plain = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(plain.token_categories, null, 'no colony record, no categories');

  const s = summarizeRun([scored, scored]);
  assert.equal(s.token_categories.edit, 800);
  assert.equal(s.token_categories.replay, 2000);

  const text = formatComparison({ label: 'before', results: [plain] }, { label: 'after', results: [scored] });
  assert.match(text, /Tokens: read – → 100, command_output – → 200, edit – → 400, reasoning – → 50, replay – → 1000\./);
  const both = formatComparison({ label: 'before', results: [scored] }, { label: 'after', results: [scored] });
  assert.match(both, /Tokens: read 100 → 100/);
  assert.doesNotMatch(both, /search/, 'a category neither run spent is not on the line');
});

test('scoring records the harness and model beside the cost, and compare shows them', () => {
  const plain = scoreTask({ task, session, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(plain.agent, null, 'neither record names one');
  assert.equal(plain.model, null);
  assert.equal(plain.tier, null);
  const scored = scoreTask({
    task,
    session: { ...session, agent: 'claude-code', model_override: 'zai/glm-5.3-flash' },
    answers: [],
    timed_out: false,
    branchScore: clean,
    colony: { ...colony, agent: 'codex' },
  });
  assert.equal(scored.agent, 'codex', 'the colony report’s reading wins when there is one');
  assert.equal(scored.model, 'zai/glm-5.3-flash', 'a launch override is the model that ran');
  const routed = scoreTask({
    task,
    session: { ...session, model_routing: { tier: 'low', model: 'zai/glm-5.3-flash' } },
    answers: [],
    timed_out: false,
    branchScore: clean,
    colony,
  });
  assert.equal(routed.model, 'zai/glm-5.3-flash', 'the model boot recorded the routing as');
  assert.equal(routed.tier, 'low');
  // Routing off — or a routed model equal to the module's own — records no model (boot.rs keeps
  // model_routing.model null): the module's default lives in settings, not on the session.
  const tiered = scoreTask({ task, session: { ...session, model_routing: { tier: 'medium', model: null } }, answers: [], timed_out: false, branchScore: clean, colony });
  assert.equal(tiered.model, null, 'a tier is never a model');
  assert.equal(tiered.tier, 'medium', 'the tier rides beside the model, not in its place');
  const text = formatComparison({ label: 'before', results: [plain] }, { label: 'after', results: [scored] });
  assert.match(text, /\| add-helper \| – · – → codex · zai\/glm-5\.3-flash \|/);
});

test('the run summary counts clean resolutions, and the gap between the two rates', () => {
  const row = (passed, clean) => ({ passed, clean, cost_usd: 0, routed_cost_usd: 0, working_ms: 0, questions: 0, tool_errors: 0 });
  const s = summarizeRun([row(true, true), row(true, false), row(false, true), row(true, null)]);
  assert.equal(s.clean_resolved, 1);
  assert.equal(s.hacked_resolved, 1);
  assert.equal(s.clean_rate, 0.25);
  assert.equal(s.gap, s.pass_rate - s.clean_rate, 'the gap is resolved minus clean, never folded into the pass rate');
  const quiet = summarizeRun([row(true, null)]);
  assert.equal(quiet.clean_rate, null, 'a run with nothing audited has no clean rate, not a zero');
  assert.equal(quiet.gap, null);
});

test('a comparison carries the clean verdict per task and the gap per run', () => {
  const text = formatComparison(
    { label: 'before', results: [{ id: 'add-helper', passed: true, clean: true }, { id: 'cart-rounding', passed: true }] },
    { label: 'after', results: [{ id: 'add-helper', passed: true, clean: false }, { id: 'cart-rounding', passed: false, clean: null }] },
  );
  assert.match(text, /\| Clean \|/);
  assert.match(text, /clean → HACKED/);
  assert.match(text, /– → –/);
  assert.match(text, /Clean resolved 1\/2 → 0\/2 \(clean rate 50% → 0%, gap 50% → 50%\)/);
});

test('a run journals its scoring time into the spend journal, where the readers keep it', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-bench-journal-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  journalScoring(dir, 1500, new Date('2026-09-28T12:34:56Z'));
  assert.deepEqual(readJsonLines(join(dir, 'spend.jsonl')), [
    { ts: '2026-09-28T12:34:56.000Z', day: '2026-09-28', org: 'bench', kind: 'scoring', scoring_ms: 1500 },
  ]);
  assert.equal(loadSpend(dir).length, 1, "the server's own reader keeps the row");
  // A data dir that cannot be written warns instead of failing the run that just finished.
  assert.doesNotThrow(() => journalScoring(join(dir, 'missing'), 5, new Date('2026-09-28T12:34:56Z')));
});

// The Jev fixture ledger: three sessions — one graded under `before`, one under `after`, one in no run —
// covering all four outcomes, an unscored decision, a keep_result at and just below the threshold, an
// orphan reread, and a line a crash tore in half (scripts/test/fixtures/jev_ladder.jsonl).
const LEDGER = readJsonLines(join(ROOT, 'scripts/test/fixtures/jev_ladder.jsonl'));
const jevResult = (session_id, id, agent, model) => ({ session_id, id, agent, model, tier: 'low' });
const beforeRun = { label: 'before', results: [jevResult('7c1e2a91', 'add-helper', 'claude-code', 'zai/glm-5.3-flash')] };
const afterRun = { label: 'after', results: [jevResult('b2f9d304', 'cart-rounding', 'codex', null)] };

test('the jev report grades each colony against its rereads, under the run that ran it', () => {
  const report = jevReport(LEDGER, [beforeRun, afterRun], 0.5);
  assert.deepEqual(report.runs.map((r) => r.run), ['before', 'after', '(no run)'], 'a session no run names grades last, under (no run)');
  const alpha = report.runs[0].colonies[0];
  assert.equal(alpha.colony, '7c1e2a91');
  assert.equal(alpha.task, 'add-helper', 'the task, agent and model ride from the run result for display');
  assert.equal(alpha.agent, 'claude-code');
  assert.equal(alpha.model, 'zai/glm-5.3-flash');
  assert.deepEqual({ decisions: alpha.decisions, rereads: alpha.rereads, tp: alpha.tp, fp: alpha.fp, fn: alpha.fn, tn: alpha.tn }, { decisions: 5, rereads: 2, tp: 1, fp: 1, fn: 1, tn: 2 });
  assert.equal(alpha.precision, 0.5);
  assert.equal(alpha.recall, 0.5);
});

test('a keep_result exactly at the threshold was predicted positive, just below was not', () => {
  const beta = jevReport(LEDGER, [afterRun], 0.5).runs[0].colonies[0];
  assert.deepEqual({ tp: beta.tp, fp: beta.fp, fn: beta.fn, tn: beta.tn }, { tp: 1, fp: 0, fn: 2, tn: 0 }, '0.5 predicted; 0.49 and null did not');
  assert.equal(beta.precision, 1);
  assert.ok(Math.abs(beta.recall - 1 / 3) < 1e-9);
});

test('no positive predictions leaves precision undefined, and an orphan reread grades nothing', () => {
  const report = jevReport(LEDGER, [beforeRun, afterRun], 0.5);
  const gamma = report.runs[2].colonies[0];
  assert.equal(gamma.colony, 'e6a8c177');
  assert.equal(gamma.task, null);
  assert.deepEqual({ decisions: gamma.decisions, rereads: gamma.rereads, tp: gamma.tp, fp: gamma.fp, fn: gamma.fn, tn: gamma.tn }, { decisions: 2, rereads: 2, tp: 0, fp: 0, fn: 1, tn: 1 });
  assert.equal(gamma.precision, null, 'nothing was predicted needed: undefined, not a zero score');
  assert.equal(gamma.recall, 0, 'a confirmed need the plugin failed to predict');
});

test('run totals and the overall pool the group counts', () => {
  const report = jevReport(LEDGER, [beforeRun, afterRun], 0.5);
  assert.deepEqual({ tp: report.runs[0].total.tp, fn: report.runs[0].total.fn, tn: report.runs[0].total.tn, decisions: report.runs[0].total.decisions, precision: report.runs[0].total.precision }, { tp: 1, fn: 1, tn: 2, decisions: 5, precision: 0.5 });
  assert.deepEqual({ tp: report.total.tp, fp: report.total.fp, fn: report.total.fn, tn: report.total.tn, decisions: report.total.decisions, rereads: report.total.rereads }, { tp: 2, fp: 1, fn: 4, tn: 3, decisions: 10, rereads: 7 });
  assert.equal(report.total.precision, 2 / 3);
  assert.equal(report.total.recall, 1 / 3);
});

test('a raised threshold un-predicts the boundary decision', () => {
  const report = jevReport(LEDGER, [beforeRun, afterRun], 0.6);
  const beta = report.runs[1].colonies[0];
  assert.deepEqual({ tp: beta.tp, fp: beta.fp, fn: beta.fn }, { tp: 0, fp: 0, fn: 3 }, 'every one of its predictions is gone at 0.6');
  assert.equal(beta.precision, null, 'which leaves it with no positive predictions, so no precision');
  assert.deepEqual({ tp: report.total.tp, fn: report.total.fn }, { tp: 1, fn: 5 }, 'only 0.9 stays predicted above 0.6');
});

test('the fixture ledger keeps the rows around its torn line', () => {
  assert.match(readFileSync(join(ROOT, 'scripts/test/fixtures/jev_ladder.jsonl'), 'utf8'), /tore in half/);
  assert.equal(LEDGER.length, 17, 'the torn line is skipped, not counted');
});

test('the jev table shows the rates with the counts beside them, and – for undefined', () => {
  const text = formatJevReport(jevReport(LEDGER, [beforeRun, afterRun], 0.5));
  assert.match(text, /# Jev compaction, graded at threshold 0\.5/);
  assert.match(text, /\| Run \| Colony \| Task \| Harness · model \| Decisions \| Rereads \| TP \| FP \| FN \| TN \| Precision \| Recall \|/);
  assert.match(text, /\| before \| 7c1e2a91 \| add-helper \| claude-code · zai\/glm-5\.3-flash \| 5 \| 2 \| 1 \| 1 \| 1 \| 2 \| 0\.50 \| 0\.50 \|/);
  assert.match(text, /\| before \| total \| {2}\| {2}\| 5 \| 2 \| 1 \| 1 \| 1 \| 2 \| 0\.50 \| 0\.50 \|/);
  assert.match(text, /\| \(no run\) \| e6a8c177 \| – \| – · – \| 2 \| 2 \| 0 \| 0 \| 1 \| 1 \| – \| 0\.00 \|/);
  assert.match(text, /\| overall \| {2}\| {2}\| {2}\| 10 \| 7 \| 2 \| 1 \| 4 \| 3 \| 0\.67 \| 0\.33 \|/);
});

test('parseArgs takes the jev subcommand: a threshold, --json and the run files', () => {
  const args = parseArgs(['jev', '--threshold', '0.6', '--json', 'bench-before.json']);
  assert.equal(args.command, 'jev');
  assert.equal(args.threshold, 0.6);
  assert.equal(args.json, true);
  assert.deepEqual(args.files, ['bench-before.json']);
  assert.equal(parseArgs(['jev']).threshold, 0.5, 'the default is the keep threshold the plugin itself uses');
  assert.equal(parseArgs(['jev']).json, false);
  assert.throws(() => parseArgs(['jev', '--threshold']), /--threshold needs a value/);
  assert.throws(() => parseArgs(['jev', '--threshold', 'soon']), /--threshold needs a number/);
  assert.throws(() => parseArgs(['jev', '--json=false']), /unknown argument/);
});

// The routing fixture ledger (scripts/test/fixtures/routing.jsonl): old-format rows with no jev_agrees, a
// row with no opinion, an unconfident one, an operator override, a floor that cancels act, a colony act
// already ran on Jev's tier, a resumed colony's second decision, actual rows (one for a colony with no
// decision) and a torn line.
const ROUTING = readJsonLines(join(ROOT, 'scripts/test/fixtures/routing.jsonl'));
const ROUTING_SESSIONS = [
  { id: 'r-agree', status: 'merged' },
  { id: 'r-nojev', status: 'pr_opened' },
  { id: 'r-lower', status: 'pr_opened', merged_at: '2026-09-02T00:00:00Z' },
  { id: 'r-higher', status: 'failed' },
  { id: 'r-unsure', status: 'closed', cost_usd: 0.3, routed_cost_usd: 0.2 },
  { id: 'r-override', status: 'merged' },
  { id: 'r-floor', status: 'merged' },
  { id: 'r-acted', status: 'failed' },
];

test('routing outcomes map sessions.json statuses to merged, pr-open, failed, other and pending', () => {
  assert.equal(routingOutcome({ status: 'merged' }), 'merged');
  assert.equal(routingOutcome({ status: 'pr_opened', merged_at: '2026-09-02T00:00:00Z' }), 'merged', 'a merge the watcher saw counts');
  assert.equal(routingOutcome({ status: 'pr_opened' }), 'pr-open');
  assert.equal(routingOutcome({ status: 'closed' }), 'failed', 'a pull request closed unmerged');
  assert.equal(routingOutcome({ status: 'failed' }), 'failed');
  assert.equal(routingOutcome({ status: 'no_changes' }), 'other');
  assert.equal(routingOutcome({ status: 'running' }), 'pending');
  assert.equal(routingOutcome(undefined), 'pending', 'a colony sessions.json lost');
});

test('act never applies under an override or with routing off, and never below the floor', () => {
  const jev = (tier, confidence) => ({ tier, confidence });
  assert.deepEqual(actTier({ rule: 'high', source: 'rule', jev: jev('low', 0.9) }, 0.8), { tier: 'low', blocked: null });
  assert.deepEqual(actTier({ rule: 'high', source: 'rule', jev: jev('low', 0.79) }, 0.8), { tier: 'high', blocked: 'unconfident' });
  assert.deepEqual(actTier({ rule: 'high', source: 'rule', jev: jev('low', 0.8) }, 0.8), { tier: 'low', blocked: null }, 'at the threshold counts');
  assert.deepEqual(actTier({ rule: 'medium', source: 'override', jev: jev('low', 0.9) }, 0.8), { tier: 'medium', blocked: 'override' });
  assert.deepEqual(actTier({ rule: 'medium', source: 'off', jev: jev('low', 0.9) }, 0.8), { tier: 'medium', blocked: 'off' });
  assert.deepEqual(actTier({ rule: 'high', source: 'rule', floor: 'medium', jev: jev('low', 0.9) }, 0.8), { tier: 'medium', blocked: 'floor' }, 'raised to the floor, still a change');
  assert.deepEqual(actTier({ rule: 'low', source: 'rule', floor: 'medium', jev: jev('high', 0.9) }, 0.8), { tier: 'high', blocked: null }, 'a floor never caps a raise');
});

test('the routing report counts agreement, recomputing it for rows from before jev_agrees', () => {
  const r = routingReport(ROUTING, ROUTING_SESSIONS, 0.8);
  assert.equal(r.decision_rows, 9, 'the torn line is skipped');
  assert.equal(r.colonies, 8, 'a resumed colony is one colony, judged on its last decision');
  assert.equal(r.with_jev, 7);
  assert.equal(r.agreement_rate, 1 / 7);
  assert.deepEqual(r.agreement_by_rule.medium, { with_jev: 4, agree: 1, rate: 0.25 });
  assert.deepEqual(r.agreement_by_rule.high, { with_jev: 2, agree: 0, rate: 0 });
  assert.deepEqual(r.agreement_by_rule.low, { with_jev: 1, agree: 0, rate: 0 });
  assert.equal(r.confidence.agree.mean, 0.9);
  assert.equal(r.confidence.disagree.count, 6);
  assert.ok(Math.abs(r.confidence.disagree.mean - (0.85 + 0.95 + 0.4 + 0.9 + 0.9 + 0.88) / 6) < 1e-9);
});

test('the routing report says what act would have changed, and what capped it', () => {
  const r = routingReport(ROUTING, ROUTING_SESSIONS, 0.8);
  assert.deepEqual(r.act, { confident_disagreements: 5, would_change: 3, capped_by_floor: 1, floor_cancels: 1, blocked_by_override: 1, blocked_routing_off: 0 });
  const floor = r.disagreements.find((c) => c.session === 'r-floor');
  assert.equal(floor.act_tier, 'medium');
  const unsure = r.disagreements.find((c) => c.session === 'r-unsure');
  assert.equal(unsure.actual_cost_usd, 0.5, 'no actual row: the session total stands in');
  assert.equal(routingReport(ROUTING, ROUTING_SESSIONS, 0.3).act.would_change, 4, 'a lower threshold lets the unsure opinion act');
});

test('only shadow colonies act would have changed are evidence, split by direction', () => {
  const r = routingReport(ROUTING, ROUTING_SESSIONS, 0.8);
  assert.deepEqual(r.baseline, { colonies: 6, failed_rate: 2 / 6 }, 'the acted and overridden colonies did not run the rule');
  assert.deepEqual(r.directions.lower, { count: 1, merged: 1, failed: 0, merged_rate: 1, failed_rate: 0, mean_actual_cost_usd: 2.5 });
  assert.deepEqual(r.directions.higher, { count: 1, merged: 0, failed: 1, merged_rate: 0, failed_rate: 1, mean_actual_cost_usd: 0.4 });
  assert.equal(r.verdict.justified, false);
  assert.match(r.verdict.reason, /only 2 confident disagreements/);
  assert.match(r.verdict.recommend, /--label rule.*--label jev.*compare/);
});

test('the verdict needs enough samples and every judged direction to show the rule was wrong', () => {
  const dir = (count, merged_rate, failed_rate) => ({ count, merged_rate, failed_rate });
  const ok = routingVerdict({ samples: 20, lower: dir(12, 0.92, 0.08), higher: dir(8, 0.3, 0.5), baselineFailedRate: 0.2 });
  assert.equal(ok.justified, true);
  assert.equal(routingVerdict({ samples: 20, lower: dir(20, 0.95, 0), higher: dir(0, null, null), baselineFailedRate: 0.2 }).justified, true, 'one judged direction is enough');
  assert.match(routingVerdict({ samples: 20, lower: dir(12, 0.8, 0.2), higher: dir(8, 0.3, 0.5), baselineFailedRate: 0.2 }).reason, /merged 80%, under 90%/);
  assert.match(routingVerdict({ samples: 20, lower: dir(12, 0.92, 0), higher: dir(8, 0.5, 0.3), baselineFailedRate: 0.2 }).reason, /failed 30%, not 15% over the 20% baseline/);
  assert.match(routingVerdict({ samples: 20, lower: dir(4, 1, 0), higher: dir(4, 0, 1), baselineFailedRate: 0.2 }).reason, /neither direction has 5 samples/);
  assert.match(routingVerdict({ samples: 19, lower: dir(19, 1, 0), higher: dir(0, null, null), baselineFailedRate: 0 }).reason, /only 19/);
});

test('the routing table shows the counts, the disagreements and the verdict', () => {
  const text = formatRoutingReport(routingReport(ROUTING, ROUTING_SESSIONS, 0.8));
  assert.match(text, /# Tier routing: the rule against Jev's second opinion \(act threshold 0\.8\)/);
  assert.match(text, /8 routed colonies \(9 decision rows\); 7 with a Jev opinion; agreement 14%/);
  assert.match(text, /\| r-floor \| acme\/app#7 \| medium \| low \| 0\.90 \| medium \| medium \(floor\) \| merged \| – \|/);
  assert.match(text, /\| r-lower \| acme\/app#3 \| high \| low \| 0\.85 \| high \| low \| merged \| \$2\.50 \|/);
  assert.match(text, /\| higher \| 1 \| 0% \| 100% \| \$0\.40 \|/);
  assert.match(text, /Verdict: act is not yet justified: only 2 confident disagreements/);
  assert.equal(parseArgs(['routing']).threshold, 0.8, 'routing defaults to the act confidence');
  assert.equal(parseArgs(['routing', '--threshold', '0.7']).threshold, 0.7);
});

test('an empty ledger reports nothing to judge rather than failing', () => {
  const r = routingReport([], [], 0.8);
  assert.equal(r.colonies, 0);
  assert.equal(r.agreement_rate, null);
  assert.match(formatRoutingReport(r), /act is not yet justified: only 0 confident disagreements/);
});
