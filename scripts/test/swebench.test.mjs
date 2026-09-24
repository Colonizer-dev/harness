import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import {
  applyReport, budgetGate, buildSnapshot, diffFiles, formatSummary, git, isCalibrated, normalizeInstance,
  parseInstances, parseSuite, patchFlags, scratchName, selectInstances, summarize, toPrediction,
} from '../swebench.mjs';

// The fixture every pure test shares: enough of a SWE-bench instance to score, with the eval test the colony must never see.
const INSTANCE = {
  instance_id: 'demo__demo-1',
  repo: 'demo/demo',
  base_commit: 'abc123',
  problem_statement: 'The loader breaks on empty input.',
  test_patch: 'diff --git a/tests/test_x.py b/tests/test_x.py\n--- a/tests/test_x.py\n+++ b/tests/test_x.py\n',
};
const LEAK = 'diff --git a/src/app.py b/src/app.py\n--- a/src/app.py\n+++ b/src/app.py\ndiff --git a/tests/test_x.py b/tests/test_x.py\n';
const HONEST = 'diff --git a/src/app.py b/src/app.py\n--- a/src/app.py\n+++ b/src/app.py\n';

test('parseInstances takes a JSON array, JSONL, and what fetch writes', () => {
  const row = { instance_id: 'a', repo: 'a/a', base_commit: 'c', problem_statement: 'p' };
  assert.equal(parseInstances(JSON.stringify([row])).length, 1);
  assert.equal(parseInstances(`${JSON.stringify(row)}\n${JSON.stringify(row)}\n\n`).length, 2);
  const envelope = JSON.stringify({ suite: { dataset: 'princeton-nlp/SWE-bench_Lite' }, instances: [row] });
  assert.equal(parseInstances(envelope).length, 1);
  assert.equal(parseSuite(envelope).dataset, 'princeton-nlp/SWE-bench_Lite');
  assert.equal(parseSuite(JSON.stringify([row])), null);
  assert.throws(() => parseInstances('{"unrelated": true}'), /instances must be/);
});

test('normalizeInstance parses string-encoded FAIL_TO_PASS and rejects half an instance', () => {
  const row = { ...INSTANCE, FAIL_TO_PASS: '["tests/test_x.py::test_loader"]', PASS_TO_PASS: [], language: 'python', patch: 'the gold patch' };
  const n = normalizeInstance(row);
  assert.deepEqual(n.fail_to_pass, ['tests/test_x.py::test_loader']);
  assert.deepEqual(n.pass_to_pass, []);
  assert.equal(n.language, 'python'); assert.equal(n.patch, 'the gold patch');
  assert.deepEqual(normalizeInstance(n).fail_to_pass, n.fail_to_pass, 'a file fetch wrote re-fetches intact');
  assert.throws(() => normalizeInstance({ instance_id: 'x' }), /missing repo, base_commit, problem_statement/);
  assert.throws(() => normalizeInstance({ ...INSTANCE, problem_statement: '' }), /missing problem_statement/);
});

test('budgetGate stops at the envelope and allows what fits', () => {
  const caps = { task_usd: 2, total_usd: 10 };
  assert.equal(budgetGate(0, caps).proceed, true);
  assert.equal(budgetGate(8, caps).proceed, true, 'spending exactly to the cap is allowed');
  const stop = budgetGate(8.01, caps);
  assert.equal(stop.proceed, false);
  assert.match(stop.reason, /\$8\.01 plus the \$2\.00 task cap would pass the \$10\.00 envelope/);
});

test('selectInstances windows a file without eating it, and --ids must all be found', () => {
  const rows = ['a', 'b', 'c', 'd'].map((id) => ({ instance_id: id }));
  const ids = (kept) => kept.map((r) => r.instance_id);
  assert.equal(selectInstances(rows, {}).length, 4, 'no limit keeps everything');
  assert.deepEqual(ids(selectInstances(rows, { offset: 2 })), ['c', 'd'], 'an offset without a limit still windows');
  assert.deepEqual(ids(selectInstances(rows, { offset: 1, limit: 2 })), ['b', 'c']);
  assert.deepEqual(ids(selectInstances(rows, { limit: 3, paged: true })), ['a', 'b', 'c'], 'paged rows are already windowed');
  assert.deepEqual(ids(selectInstances(rows, { ids: ['d', 'a'] })), ['a', 'd']);
  assert.throws(() => selectInstances(rows, { ids: ['zzz'], dataset: 'demo' }), /demo has no instance zzz/);
});

test('names and prediction lines are safe for GitHub and the official harness', () => {
  assert.equal(scratchName('astropy__astropy-12345'), 'swebench-astropy-astropy-12345');
  assert.equal(scratchName('Weird--..Id!'), 'swebench-weird-id');
  assert.ok(scratchName('x'.repeat(300)).length <= 'swebench-'.length + 80);
  assert.deepEqual(toPrediction({ instance_id: 'demo__demo-1' }, 'a patch', 'model-x'), {
    instance_id: 'demo__demo-1', model_name_or_path: 'model-x', model_patch: 'a patch',
  });
  assert.deepEqual(toPrediction({ instance_id: 'i' }, ''), { instance_id: 'i', model_name_or_path: 'colonizer', model_patch: '' });
});

test('buildSnapshot rebuilds one commit with the base tree: no history, no remotes, no future files', (ctx) => {
  const upstream = mkdtempSync(join(tmpdir(), 'colonizer-swebench-upstream-'));
  ctx.after(() => rmSync(upstream, { recursive: true, force: true }));
  const commit = (msg) => {
    git(['add', '-A'], upstream);
    git(['-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', msg], upstream);
  };
  git(['init', '-q', '-b', 'main'], upstream);
  writeFileSync(join(upstream, 'one.txt'), 'one');
  commit('the base commit');
  const base = git(['rev-parse', 'HEAD'], upstream);
  const baseTree = git(['rev-parse', 'HEAD^{tree}'], upstream);
  writeFileSync(join(upstream, 'the-fix.txt'), 'what the gold patch will change, one commit later');
  commit('the future fix');

  const snap = buildSnapshot({ upstream: `file://${upstream}`, baseCommit: base });
  ctx.after(() => rmSync(snap.dir, { recursive: true, force: true }));
  assert.equal(git(['rev-list', '--count', 'HEAD'], snap.dir), '1', 'exactly one commit');
  assert.equal(git(['rev-parse', 'HEAD^{tree}'], snap.dir), baseTree, 'the tree is the base commit’s tree');
  assert.equal(git(['remote'], snap.dir), '', 'no remotes');
  assert.ok(!existsSync(join(snap.dir, 'the-fix.txt')), 'the later commit’s file is absent');
  assert.ok(existsSync(join(snap.dir, 'one.txt')));
});

test('patchFlags names a patch that edits the hidden eval tests, and an empty one', () => {
  assert.deepEqual(diffFiles(LEAK), ['src/app.py', 'tests/test_x.py']);
  assert.deepEqual(patchFlags(INSTANCE, LEAK), ['touches-eval-tests']);
  assert.deepEqual(patchFlags(INSTANCE, HONEST), []); assert.deepEqual(patchFlags(INSTANCE, ''), ['empty']);
  assert.deepEqual(patchFlags({ ...INSTANCE, test_patch: null }, LEAK), []);
});

// A run record with every outcome: two resolved, one leaked (resolved but flagged), one empty, two skipped at the envelope.
const RUN = () => ({
  label: 't',
  budget: { task_usd: 2, total_usd: 10, spent_usd: 3.5, stopped_at_cap: true },
  controls: { single_commit_snapshot: true, no_network_answer_sources: false },
  calibrated: false,
  tasks: [
    { instance_id: 'p1', language: 'python', outcome: 'patched', flags: [], cost_usd: 1, routed_cost_usd: null },
    { instance_id: 'p2', language: 'python', outcome: 'patched', flags: ['touches-eval-tests'], cost_usd: 1, routed_cost_usd: null },
    { instance_id: 'g1', language: 'go', outcome: 'patched', flags: [], cost_usd: 1, routed_cost_usd: null },
    { instance_id: 'e1', language: null, outcome: 'no-patch', flags: ['empty'], cost_usd: 0.5, routed_cost_usd: null },
    { instance_id: 's1', language: 'python', outcome: 'skipped-budget', flags: [], cost_usd: null, routed_cost_usd: null },
    { instance_id: 's2', language: 'go', outcome: 'skipped-budget', flags: [], cost_usd: null, routed_cost_usd: null },
  ],
});

test('a deliberately leaked answer is caught, scored raw, and left out of the clean count', () => {
  // p2 is the leak: it resolved only by editing the hidden eval test, so raw keeps it and clean does not.
  const run = applyReport(RUN(), { resolved_ids: ['p2'], unresolved_ids: ['p1', 'g1', 'e1'], submitted_ids: ['p1', 'p2', 'g1', 'e1'] });
  assert.equal(run.tasks.find((t) => t.instance_id === 'p2').resolved, true, 'the leak still counts in the raw rate');
  assert.equal(run.tasks.find((t) => t.instance_id === 's1').resolved, null);

  const s = summarize(run);
  assert.equal(s.attempted, 4, 'skipped tasks leave the denominator');
  assert.equal(s.skipped_budget, 2, 'and are reported separately');
  assert.equal(s.resolved, 1); assert.equal(s.raw_resolved_rate, 0.25);
  assert.equal(s.flagged, 2);
  assert.equal(s.clean_resolved_rate, 0, 'clean drops the flagged tasks from numerator and denominator alike');
  assert.deepEqual(s.by_language, { python: { attempted: 2, resolved: 1 }, go: { attempted: 1, resolved: 0 }, unknown: { attempted: 1, resolved: 0 } });
  assert.equal(s.cost_usd, 3.5);

  const text = formatSummary(run);
  assert.match(text, /resolved \(raw\) 1\/4 = 25\.0%/);
  assert.match(text, /clean, 2 flagged excluded\) = 0\.0%/);
  assert.match(text, /python: 1\/2 resolved/);
  assert.match(text, /\$3\.50 against the \$10\.00 envelope \(task cap \$2\.00\), stopped at the cap/);
  assert.match(text, /uncalibrated — no_network_answer_sources not enforced; not comparable to published SWE-bench numbers/);
});

test('a task the harness itself errored on is unknown, not unresolved', () => {
  const run = applyReport(RUN(), { resolved_ids: ['p1'], unresolved_ids: ['p2'], error_ids: ['g1', 'e1'], submitted_ids: ['p1', 'p2', 'g1', 'e1'] });
  const errored = run.tasks.find((t) => t.instance_id === 'g1');
  assert.equal(errored.resolved, null, 'not scored, not failed'); assert.equal(errored.harness_error, true);
  const s = summarize(run);
  assert.equal(s.harness_errors, 2);
  assert.equal(s.raw_resolved_rate, 1 / 2, 'only the tasks the harness judged are in a rate');
  assert.equal(s.clean_resolved_rate, 1, 'the other clean task errored too, so only p1 is in the clean rate');
  assert.match(formatSummary(run), /errored on are in neither rate/);
});

test('before scoring, the rates say so rather than reading as zero', () => {
  const s = summarize(RUN());
  assert.equal(s.resolved, 0);
  assert.equal(s.raw_resolved_rate, null); assert.equal(s.clean_resolved_rate, null);
  assert.match(formatSummary(RUN()), /not scored yet/);
});

test('a run is calibrated only when every control holds', () => {
  assert.equal(isCalibrated({ single_commit_snapshot: true, concealed_eval_artifacts: true }), true);
  assert.equal(isCalibrated({ single_commit_snapshot: true, no_network_answer_sources: false }), false);
  assert.match(formatSummary({ ...RUN(), controls: { single_commit_snapshot: true }, calibrated: true }), /calibrated: every control held/);
});
