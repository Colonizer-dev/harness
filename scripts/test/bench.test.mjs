import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { formatComparison, scoreTask, summarizeRun } from '../bench.mjs';

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
