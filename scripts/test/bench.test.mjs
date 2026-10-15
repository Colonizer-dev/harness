import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { actTier, briefMetrics, briefReport, focusReport, formatBriefReport, formatComparison, formatFocusReport, formatJevReport, formatRateReport, formatRoutingReport, jevReport, journalScoring, outsideTask, parseArgs, rateReport, routingOutcome, routingReport, routingVerdict, runCheck, runOwnTests, scoreTask, summarizeRun } from '../bench.mjs';
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

// The brief-pick ledger rows the mothership writes (#585): one pick row per colony boot, and a used row
// per watched note or skill pack the colony was later seen to touch. briefMetrics grades the picks against
// those uses; a total pools the counts and recomputes the rates, and carries the per-colony means.
const pickRow = (session_id, mandatory, candidates, picks) => ({ kind: 'pick', session_id, at: '2026-10-01T00:00:00Z', model: 'jev-1.13.0', mandatory, candidates, picks, would_load: [...mandatory, ...picks], rounds: picks.length, missed: false });
const usedRow = (session_id, item) => ({ kind: 'used', session_id, at: '2026-10-01T00:00:01Z', item, via: 'Read' });
const BRIEF = [
  pickRow('s1', ['house-rule'], ['note:repo/a', 'note:repo/b', 'skill:pack'], ['note:repo/a', 'skill:pack']),
  usedRow('s1', 'note:repo/a'),
  usedRow('s1', 'note:repo/b'),
  usedRow('s1', 'house-rule'),
  pickRow('s2', ['security'], ['note:repo/c'], []),
  usedRow('s2', 'note:repo/c'),
];
const briefRun = { label: 'one', results: [{ session_id: 's1', id: 'add-helper', agent: 'claude-code', model: 'zai/glm-5.3-flash' }, { session_id: 's2', id: 'readme-typo', agent: 'codex', model: null }] };

test('briefMetrics grades pick against use, with mandatory notes out of the universe', () => {
  const m = briefMetrics(BRIEF[0], ['note:repo/a', 'note:repo/b', 'house-rule']);
  assert.deepEqual({ candidates: m.candidates, picks: m.picks, mandatory: m.mandatory, tp: m.tp, fp: m.fp, fn: m.fn }, { candidates: 3, picks: 2, mandatory: 1, tp: 1, fp: 1, fn: 1 }, 'a picked the colony used; a used one it did not pick; a picked one it never used; the mandatory use is ignored');
  assert.equal(m.precision, 0.5);
  assert.equal(m.recall, 0.5);
});

test('no picks leaves precision undefined, and an unpicked need is still a miss', () => {
  const m = briefMetrics(BRIEF[4], ['note:repo/c']);
  assert.deepEqual({ tp: m.tp, fp: m.fp, fn: m.fn }, { tp: 0, fp: 0, fn: 1 });
  assert.equal(m.precision, null, 'nothing was picked: undefined, not a zero score');
  assert.equal(m.recall, 0);
});

test('the brief report groups under its run and pools the counts into a total', () => {
  const report = briefReport(BRIEF, [briefRun]);
  assert.deepEqual(report.runs.map((r) => r.run), ['one']);
  assert.equal(report.runs[0].colonies.length, 2);
  const s1 = report.runs[0].colonies[0];
  assert.equal(s1.task, 'add-helper', 'the task, agent and model ride from the run result for display');
  assert.deepEqual({ tp: report.total.tp, fp: report.total.fp, fn: report.total.fn, precision: report.total.precision, recall: report.total.recall }, { tp: 1, fp: 1, fn: 2, precision: 0.5, recall: 1 / 3 });
  assert.deepEqual({ candidates: report.total.mean_candidates, picks: report.total.mean_picks, mandatory: report.total.mean_mandatory }, { candidates: 2, picks: 1, mandatory: 1 });
  const text = formatBriefReport(report);
  assert.match(text, /# Jev brief picks, graded against the notes and packs the colony used/);
  assert.match(text, /\| Run \| Colony \| Task \| Harness · model \| Candidates \| Picks \| Mandatory \| TP \| FP \| FN \| Precision \| Recall \|/);
  assert.match(text, /\| one \| s1 \| add-helper \| claude-code · zai\/glm-5\.3-flash \| 3 \| 2 \| 1 \| 1 \| 1 \| 1 \| 0\.50 \| 0\.50 \|/);
  assert.match(text, /\| one \| s2 \| readme-typo \| codex · – \| 1 \| 0 \| 1 \| 0 \| 0 \| 1 \| – \| 0\.00 \|/);
  assert.match(text, /\| overall \| {2}\| {2}\| {2}\| 2\.0 \| 1\.0 \| 1\.0 \| 1 \| 1 \| 2 \| 0\.50 \| 0\.33 \|/);
});

test('parseArgs takes the brief subcommand, its run files and --json', () => {
  const args = parseArgs(['brief', '--json', 'bench-one.json']);
  assert.equal(args.command, 'brief');
  assert.equal(args.json, true);
  assert.deepEqual(args.files, ['bench-one.json']);
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

test('the focus report joins the rule measurement with Jev shadow asks, skipping green runs', () => {
  const focus = [
    { kind: 'focus', mode: 'shadow', would_catch: true, actual_first_failure_ms: 300, focused_first_failure_ms: 100, total_ms: 500 },
    { kind: 'focus', mode: 'act', would_catch: false, actual_first_failure_ms: 200, focused_first_failure_ms: 250, total_ms: 400 },
    { kind: 'focus', mode: 'shadow', would_catch: null, actual_first_failure_ms: null, focused_first_failure_ms: null, total_ms: 90 },
    { kind: 'decision', point: 'routing.tier', pick: 'high', latency_ms: 99 },
  ];
  const decisions = [
    ...focus,
    { kind: 'decision', point: 'verify.focus', pick: 'b: cargo test', latency_ms: 30, outcome: { would_catch: true } },
    { kind: 'decision', point: 'verify.focus', pick: null, miss: 'no_key', latency_ms: 0 },
  ];
  const r = focusReport(focus, decisions);
  assert.deepEqual(r.by_mode, { shadow: 2, act: 1 });
  assert.equal(r.verifications, 3);
  assert.equal(r.failed, 2, 'a green verification has no would-catch to judge');
  assert.equal(r.rule_would_catch, 0.5);
  assert.equal(r.median_actual_first_failure_ms, 250);
  assert.equal(r.median_focused_first_failure_ms, 175);
  assert.equal(r.median_total_ms, 400);
  assert.equal(r.asked, 2);
  assert.equal(r.answered, 1);
  assert.equal(r.jev_would_catch, 1, 'the one answer caught it');
  assert.equal(r.median_jev_latency_ms, 30, 'the median ask is over the answers; a miss carries none');
  const text = formatFocusReport(r);
  assert.match(text, /median time-to-first-failure: 250 ms as ran vs 175 ms focused, 30% faster focused/);
  assert.match(text, /Jev asked in shadow: 2, answered 1/);
  const slower = formatFocusReport(focusReport(
    [{ kind: 'focus', mode: 'shadow', would_catch: true, actual_first_failure_ms: 200, focused_first_failure_ms: 250, total_ms: 400 }],
    [],
  ));
  assert.match(slower, /200 ms as ran vs 250 ms focused, 25% slower focused/, 'a slower focused median is not dressed up as a gain');
});

test('an empty ledger reports nothing to judge rather than failing', () => {
  const r = routingReport([], [], 0.8);
  assert.equal(r.colonies, 0);
  assert.equal(r.agreement_rate, null);
  assert.match(formatRoutingReport(r), /act is not yet justified: only 0 confident disagreements/);
});

const RATE_COLONIES = [
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-opus-5' }, pr_url: 'https://…/1', ci_state: 'success', status: 'merged' },
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-opus-5' }, pr_url: 'https://…/2', ci_state: 'success', status: 'merged' },
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-opus-5' }, pr_url: 'https://…/3', ci_state: 'pending', status: 'pr_opened' },
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-opus-5' }, pr_url: null, status: 'no_changes' },
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-sonnet-4-5' }, pr_url: 'https://…/4', ci_state: 'success', status: 'merged' },
  { source: 'colonies', agent: 'claude-code', model_routing: { model: 'claude-sonnet-4-5' }, pr_url: 'https://…/5', ci_state: 'success', status: 'merged' },
  { source: 'bench', agent: 'claude-code', model: 'claude-opus-5', pr_url: 'https://…/6', visible: true, status: 'pr_opened' },
];

test('the rate table groups by harness · model and source, counting the three stages separately', () => {
  const text = formatRateReport(rateReport(RATE_COLONIES));
  assert.match(text, /# Resolved rate by harness · model/);
  assert.match(text, /6 colonies and 1 bench tasks, counted separately/);
  // Biggest sample first, then by name: the colonies of a model before the one bench task beside them.
  // The fourth opus colony settled with no pull request at all, so PR opened is 3 of the 4 that got
  // that far: a stage the record could speak for, and a row that did not pass it.
  assert.match(text, /\| claude-code · claude-opus-5 \| colonies \| 4 \| 3\/4 \(75%\) \| 2\/3 \(67%\) \| 2\/4 \(50%\) \|/);
  assert.match(text, /\| claude-code · claude-sonnet-4-5 \| colonies \| 2 \| 2\/2 \(100%\) \| 2\/2 \(100%\) \| 2\/2 \(100%\) \|/);
  assert.match(text, /\| claude-code · claude-opus-5 \| bench \| 1 \| 1\/1 \(100%\) \| 1\/1 \(100%\) \| 0\/1 \(0%\) \|/);
  assert.match(text, /The bench never merges/);
  assert.match(text, /hidden check/);
  assert.match(text, /One colony is one sample/);
});

test('a model is read from the override, then what routing recorded, then the row, never a tier', () => {
  const r = rateReport([
    { source: 'colonies', agent: 'a', model_override: 'override-model', model_routing: { model: 'routed-model' }, model_tier: 'high', model: 'row-model', status: 'merged' },
    { source: 'colonies', agent: 'a', model_routing: { model: 'routed-model' }, model_tier: 'high', model: 'row-model', status: 'merged' },
    { source: 'colonies', agent: 'a', model_tier: 'high', model: 'row-model', status: 'merged' },
    { source: 'colonies', agent: 'a', model: 'row-model', status: 'merged' },
    { source: 'colonies', agent: null, status: 'merged' },
  ]);
  // The two rows that resolve to the row's own model are the biggest group and come first, then the rest
  // by name. A tier is a label, not a model — it never becomes the model name (the rule the `compare`
  // rows above already keep), so `model_tier: 'high'` groups with neither `override-model` nor
  // `routed-model` and never renders as `high`.
  assert.deepEqual(r.groups.map((g) => g.model), ['row-model', null, 'override-model', 'routed-model']);
  assert.match(formatRateReport(r), /\| – · – \| colonies \| 1 \| 0\/1 \(0%\) \| – \| 1\/1 \(100%\) \|/);
});

test('a tier alone never stands in for a model: the row reads –', () => {
  const text = formatRateReport(rateReport([{ source: 'colonies', agent: 'claude-code', model_tier: 'high', status: 'merged' }]));
  assert.match(text, /\| claude-code · – \| colonies \| 1 \| 0\/1 \(0%\) \| – \| 1\/1 \(100%\) \|/);
  assert.doesNotMatch(text, /claude-code · high/);
});

test('a bench group rates the rows that opened no pull request: 1/3, not 1/1', () => {
  const text = formatRateReport(rateReport([
    { source: 'bench', agent: 'claude-code', model: 'm', pr_url: 'https://…/1', visible: true, status: 'pr_opened' },
    { source: 'bench', agent: 'claude-code', model: 'm', pr_url: null, visible: false, status: 'failed' },
    { source: 'bench', agent: 'claude-code', model: 'm', pr_url: null, visible: true, status: 'pr_opened' },
  ]));
  // Every bench row ran its task to a scored result, so all three could be measured for the
  // pull-request stage; only one of them opened one. Measuring only the rows with a `pr_url` would make
  // this column read 1/1 (100%) and hide the two tasks that never got as far as a pull request.
  assert.match(text, /\| claude-code · m \| bench \| 3 \| 1\/3 \(33%\) \| 2\/3 \(67%\) \| 0\/3 \(0%\) \|/);
});

test('a colony still in flight reads – for every stage, not a failure it has not had a chance at', () => {
  const inFlight = ['queued', 'blocked', 'starting', 'running', 'waiting_for_answer', 'publishing', 'idle'];
  const r = rateReport(inFlight.map((status) => ({ source: 'colonies', agent: 'a', model: 'm', status })));
  // None of the seven has reached the pull-request stage, tests green or merged: all three are unmeasured,
  // so every cell reads – rather than a 0 the model has not earned.
  assert.deepEqual(r.groups[0].stages, { pr_opened: { green: 0, measured: 0 }, tests_green: { green: 0, measured: 0 }, merged: { green: 0, measured: 0 } });
  assert.match(formatRateReport(r), /\| a · m \| colonies \| 7 \| – \| – \| – \|/);
});

test('a settled colony that failed is a real zero, where an in-flight one is –', () => {
  const text = formatRateReport(rateReport([
    { source: 'colonies', agent: 'a', model: 'm', status: 'running', pr_url: null, ci_state: 'pending' },
    { source: 'colonies', agent: 'a', model: 'm', status: 'queued', pr_url: null, ci_state: null },
    { source: 'colonies', agent: 'a', model: 'm', status: 'failed', pr_url: null, ci_state: 'failure' },
  ]));
  // The two in flight drop out of every denominator; the one that finished and failed is counted, and the
  // CI it did report is counted too — so this is 0/1, not 0/3 and not –.
  assert.match(text, /\| a · m \| colonies \| 3 \| 0\/1 \(0%\) \| 0\/1 \(0%\) \| 0\/1 \(0%\) \|/);
});

test('a colony whose status is missing or unknown reads –, not 0', () => {
  const r = rateReport([
    { source: 'colonies', agent: 'a', model: 'm', status: null, pr_url: null, ci_state: 'success' },
    { source: 'colonies', agent: 'a', model: 'm', status: 'not-a-status-we-know', pr_url: null, ci_state: 'success' },
    { source: 'colonies', agent: 'a', model: 'm', pr_url: null, ci_state: 'success' },
  ]);
  assert.deepEqual(r.groups[0].stages.pr_opened, { green: 0, measured: 0 });
  assert.deepEqual(r.groups[0].stages.merged, { green: 0, measured: 0 });
  assert.match(formatRateReport(r), /\| a · m \| colonies \| 3 \| – \| – \| – \|/);
});

test('a stage nothing was measured for reads – , not a zero it never had a chance at', () => {
  const text = formatRateReport(rateReport([
    { source: 'bench', agent: 'a', model: 'm', pr_url: 'https://…/1', visible: null, status: 'pr_opened' },
  ]));
  assert.match(text, /\| a · m \| bench \| 1 \| 1\/1 \(100%\) \| – \| 0\/1 \(0%\) \|/);
  assert.doesNotMatch(text, /\| 0% \|/);
  assert.match(text, /reads –, not 0: a stage that never ran is not a stage that failed/);
});

test('merged_at and status each say merged on their own', () => {
  const r = rateReport([
    { source: 'colonies', agent: 'a', model: 'm', status: 'closed', merged_at: '2026-01-02T03:04:05Z' },
    { source: 'colonies', agent: 'a', model: 'm', status: 'merged', merged_at: null },
    { source: 'colonies', agent: 'a', model: 'm', status: 'pr_opened', merged_at: null },
  ]);
  assert.deepEqual(r.groups[0].stages.merged, { green: 2, measured: 3 });
});

test('for colonies tests green is its CI succeeding, and only that', () => {
  const r = rateReport([
    { source: 'colonies', agent: 'a', model: 'm', ci_state: 'success', status: 'merged' },
    { source: 'colonies', agent: 'a', model: 'm', ci_state: 'pending', status: 'merged' },
    { source: 'colonies', agent: 'a', model: 'm', status: 'merged' },
  ]);
  assert.deepEqual(r.groups[0].stages.tests_green, { green: 1, measured: 2 });
});

test('the overall row pools the counts instead of averaging the per-group rates', () => {
  const one = { source: 'colonies', agent: 'a', model: 'm', pr_url: 'https://…/1', status: 'merged' };
  const none = { source: 'colonies', agent: 'b', model: 'm', pr_url: null, status: 'failed' };
  const text = formatRateReport(rateReport([one, none, none, none]));
  // The one colony that merged is 100% and the three that did not are 0%: averaging the two rates would
  // say 50%; the pooled counts say 1 of the 4 that got that far did — 25%.
  assert.match(text, /\| a · m \| colonies \| 1 \|.*\| 1\/1 \(100%\) \|/);
  assert.match(text, /\| b · m \| colonies \| 3 \|.*\| 0\/3 \(0%\) \|/);
  assert.match(text, /\| – \| overall \| 4 \|.*\| 1\/4 \(25%\) \|$/m);
});

test('no records is an empty table, not a crash', () => {
  const r = rateReport([]);
  assert.deepEqual(r.groups, []);
  assert.deepEqual(r.overall, { n: 0, stages: { pr_opened: { green: 0, measured: 0 }, tests_green: { green: 0, measured: 0 }, merged: { green: 0, measured: 0 } } });
  const text = formatRateReport(r);
  assert.doesNotMatch(text, /overall/);
  assert.match(text, /0 colonies and 0 bench tasks/);
});

test('the rate report is the json the docs read: groups and a pooled overall', () => {
  assert.deepEqual(rateReport([{ source: 'bench', agent: 'a', model: 'm', pr_url: 'https://…/1', visible: false, status: 'pr_opened' }]), {
    groups: [{ harness: 'a', model: 'm', source: 'bench', n: 1, stages: { pr_opened: { green: 1, measured: 1 }, tests_green: { green: 0, measured: 1 }, merged: { green: 0, measured: 1 } } }],
    overall: { n: 1, stages: { pr_opened: { green: 1, measured: 1 }, tests_green: { green: 0, measured: 1 }, merged: { green: 0, measured: 1 } } },
  });
});

test('rate takes run files, a data dir and --json like every other report', () => {
  const args = parseArgs(['rate', 'a.json', '--data', '/tmp/d', '--json']);
  assert.equal(args.command, 'rate');
  assert.deepEqual(args.files, ['a.json']);
  assert.equal(args.data, '/tmp/d');
  assert.equal(args.json, true);
});
