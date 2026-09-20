import assert from 'node:assert/strict';
import { test } from 'node:test';

import { analyze, formatReport, formatTranscript, reasons, redact, summarize } from '../colony-report.mjs';

const at = (s) => new Date(1_789_000_000_000 + s * 1000).toISOString();

/** A colony that asks one question, runs two tools (one failing) and ends its turn. */
const colony = {
  mothership: 'test',
  session: { id: 'ab12cd34', repo: 'acme/webshop', issue: 42, status: 'pr_opened', pr_url: 'https://…/1', cost_usd: 0.1, boot_timing: { total_ms: 3492 } },
  events: [
    { type: 'status', state: 'working', ts: at(0) },
    { type: 'user_message', id: 'initial', text: 'Resolve issue 42', ts: at(0) },
    { type: 'question', question_id: 'q1', questions: [{ question: 'Which one?', options: [{ label: 'A' }, { label: 'B' }] }], ts: at(10) },
    { type: 'status', state: 'waiting_for_answer', ts: at(10) },
    // 120 s of the user's time: neither work nor silence.
    { type: 'question_answered', question_id: 'q1', answers: { 'Which one?': 'A' }, ts: at(130) },
    { type: 'status', state: 'working', ts: at(130) },
    { type: 'tool_call', tool_call_id: 't1', name: 'Bash', input: { command: 'npm test' }, ts: at(131) },
    { type: 'tool_result', tool_call_id: 't1', is_error: true, output: 'HTTP 429 rate limit exceeded', ts: at(132) },
    { type: 'tool_call', tool_call_id: 't2', name: 'Read', input: { file_path: '/workspace/a.js' }, ts: at(133) },
    { type: 'tool_result', tool_call_id: 't2', is_error: false, output: 'ok', ts: at(134) },
    { type: 'turn_end', is_error: false, duration_ms: 300_000, cost_usd: 0.4, ts: at(300) },
  ],
  logs: [{ type: 'harness_log', level: 'warn', message: 'the private mesh is unavailable', ts: at(1) }],
};

test('a colony reports its cost, work, questions and tools', () => {
  const r = analyze(colony);
  assert.equal(r.cost_usd, 0.4, 'turn_end cost is cumulative, so the last one is the total');
  assert.equal(r.answer_wait_ms, 120_000);
  assert.equal(r.working_ms, 180_000, 'the 120 s wait for an answer is the user’s time, not the agent’s');
  assert.equal(r.questions, 1);
  assert.equal(r.questions_answered, 1);
  assert.equal(r.tool_calls, 2);
  assert.equal(r.tool_errors, 1);
  assert.deepEqual(r.tools.Bash, { calls: 1, errors: 1 });
  assert.equal(r.rate_limit_hits, 1);
  assert.equal(r.boot_ms, 3492);
  assert.equal(r.wall_ms, 300_000);
});

test('waiting for an answer is not counted as the agent going silent', () => {
  // The same colony, but the user takes ten minutes to answer: longer than any gap while it works.
  const slow = {
    ...colony,
    events: colony.events.map((e) => (e.type === 'question_answered' || (e.type === 'status' && e.ts === at(130)) ? { ...e, ts: at(610) } : e)),
  };
  const r = analyze(slow);
  assert.equal(r.answer_wait_ms, 600_000);
  assert.equal(r.longest_silence_ms, 166_000, 'the gap from the last tool result to the end of the turn, not the wait');
});

test('a watchdog nudge is not a message from the user', () => {
  const events = [
    { type: 'status', state: 'working', ts: at(0) },
    { type: 'user_message', id: 'watchdog-ab12', text: 'Are you still working?', ts: at(60) },
    { type: 'user_message', id: 'u-1', text: 'try the other file', ts: at(120) },
  ];
  const r = analyze({ session: { id: 'x' }, events });
  assert.equal(r.watchdog_nudges, 1);
  assert.equal(r.user_messages, 1, 'the brief and the nudge are not messages the user typed');
});

test('repeats and settlers are counted', () => {
  const events = [{ type: 'status', state: 'working', ts: at(0) }];
  for (let i = 0; i < 3; i++) {
    events.push({ type: 'tool_call', tool_call_id: `r${i}`, name: 'Read', input: { file_path: '/workspace/a.js' }, ts: at(i) });
    events.push({ type: 'tool_call', tool_call_id: `b${i}`, name: 'Bash', input: { command: 'npm test' }, ts: at(i) });
  }
  events.push({ type: 'tool_call', tool_call_id: 'task1', name: 'Task', input: { subagent_type: 'Explore' }, ts: at(9) });
  events.push({ type: 'tool_call', tool_call_id: 's1', name: 'Grep', input: { pattern: 'x' }, agent: { id: 'task1', name: 'Explore' }, ts: at(10) });
  const r = analyze({ session: { id: 'y' }, events });
  assert.deepEqual(r.repeated_reads, [{ path: '/workspace/a.js', n: 3 }]);
  assert.deepEqual(r.repeated_commands, [{ command: 'npm test', n: 3 }]);
  assert.equal(r.subagents, 1);
  assert.deepEqual(r.subagent_types, { Explore: 1 });
  assert.equal(r.subagent_tool_calls, 1);
});

test('the runner re-prompting a plain-text question is counted', () => {
  const events = [
    { type: 'status', state: 'working', ts: at(0) },
    { type: 'log', level: 'info', message: 'The agent asked in plain text; asking it to use a choice card instead.', ts: at(5) },
  ];
  assert.equal(analyze({ session: { id: 'z' }, events }).plain_text_reprompts, 1);
});

test('a colony is flagged for reading with its reasons', () => {
  const r = analyze(colony);
  const why = reasons(r, 0.3);
  assert.ok(why.some((w) => w.includes('rate-limit')), why.join('; '));
  assert.ok(why.some((w) => w.includes('top quarter')), why.join('; '));
  assert.deepEqual(reasons(analyze({ session: { id: 'quiet', status: 'pr_opened' }, events: [] })), []);
});

test('the summary adds up and ranks what to read', () => {
  const s = summarize([analyze(colony), analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })]);
  assert.equal(s.colonies, 2);
  assert.equal(s.statuses.pr_opened, 2);
  assert.equal(s.questions.total, 1);
  assert.equal(s.rate_limit_hits, 1);
  assert.equal(s.tools[0].name, 'Bash');
  assert.equal(s.worth_reading[0].id, 'ab12cd34');
  assert.ok(!s.worth_reading.some((w) => w.id === 'plain'));
});

test('credentials in commands and output are redacted', () => {
  assert.equal(redact('curl -H "Authorization: Bearer abcdef0123456789"'), 'curl -H "Authorization: Bearer …"');
  assert.equal(redact('export GH_TOKEN=ghp_0123456789abcdefghijklmnopqrstuvwxyz'), 'export GH_TOKEN=gh…');
  assert.equal(redact('key sk-ant-oat01-abcdef'), 'key sk-ant-…');
  assert.match(redact('api_key: "swordfishy"'), /api_key: "…/);
});

test('a transcript reads as steps, with the question, the wait and the failure', () => {
  const text = formatTranscript(colony);
  assert.match(text, /ab12cd34 acme\/webshop #42/);
  assert.match(text, /\+10s\s+\? Which one\? \[A \| B\]/);
  assert.match(text, /✓ answered \(after 2m00s\): A/);
  assert.match(text, /✗ Bash failed: HTTP 429 rate limit exceeded/);
  assert.match(text, /■ turn ended after 5m00s, \$0.40 so far/);
  assert.match(text, /mothership warn: the private mesh is unavailable/);
});

/** A finding that is validated, fixed by a colony, reviewed and merged, end to end. */
const chain = {
  mothership: 'test',
  session: { id: 'chain01', repo: 'acme/webshop', issue: 7, status: 'pr_opened', pr_url: 'https://github.com/acme/webshop/pull/8' },
  events: [
    { type: 'status', state: 'working', ts: at(0) },
    { type: 'finding', title: 'llms.txt promises career pages the scanner cannot fetch', ts: at(1) },
    { type: 'validated', title: 'llms.txt promises career pages the scanner cannot fetch', severity: 'medium', ts: at(2) },
    { type: 'fix_colony', title: 'llms.txt promises career pages the scanner cannot fetch', session: 'fix00aa', issue: 'https://github.com/acme/webshop/issues/9', ts: at(3) },
    { type: 'review', title: 'llms.txt promises career pages the scanner cannot fetch', session: 'rev00bb', verdict: 'pass', pr: 'https://github.com/acme/webshop/pull/8', ts: at(4) },
    { type: 'merged', title: 'llms.txt promises career pages the scanner cannot fetch', session: 'fix00aa', pr: 'https://github.com/acme/webshop/pull/8', ts: at(5) },
  ],
};

test("a finding's whole chain is counted: validated, fix colony, review, merge", () => {
  const r = analyze(chain);
  assert.equal(r.findings, 1);
  assert.equal(r.validated, 1);
  assert.equal(r.fix_colonies, 1);
  assert.equal(r.reviews, 1);
  assert.equal(r.review_passed, 1);
  assert.equal(r.merged, 1);
});

test('a transcript prints the chain, one line per host event', () => {
  const text = formatTranscript(chain);
  assert.match(text, /◆ finding: llms\.txt promises career pages/);
  assert.match(text, /✓ validated: llms\.txt promises career pages.*\(medium\)/);
  assert.match(text, /⚒ fix colony fix00aa for: llms\.txt promises career pages/);
  assert.match(text, /⚖ review rev00bb of https:\/\/github\.com\/acme\/webshop\/pull\/8: pass/);
  assert.match(text, /✔ merged https:\/\/github\.com\/acme\/webshop\/pull\/8/);
});

test('a rejected finding is counted and keeps its reason', () => {
  const rejected = analyze({
    mothership: 'test',
    session: { id: 'rej00cc', repo: 'acme/webshop', issue: 8, status: 'pr_opened' },
    events: [
      { type: 'status', state: 'working', ts: at(0) },
      { type: 'finding', title: 'docs promise a query endpoint the code lacks', ts: at(1) },
      { type: 'rejected', title: 'docs promise a query endpoint the code lacks', reason: 'the endpoint is behind gated auth', ts: at(2) },
    ],
  });
  assert.equal(rejected.findings, 1);
  assert.equal(rejected.rejected, 1);
  assert.equal(rejected.validated, 0);
  const text = formatTranscript({
    mothership: 'test',
    session: { id: 'rej00cc', repo: 'acme/webshop', issue: 8, status: 'pr_opened' },
    events: [
      { type: 'status', state: 'working', ts: at(0) },
      { type: 'rejected', title: 'docs promise a query endpoint the code lacks', reason: 'the endpoint is behind gated auth', ts: at(1) },
    ],
  });
  assert.match(text, /✗ rejected: docs promise a query endpoint the code lacks — the endpoint is behind gated auth/);
});

test('a failed review is worth reading, a rejection is not', () => {
  const failed = analyze({
    session: { id: 'failfix', status: 'pr_opened' },
    events: [
      { type: 'review', title: '…', session: 'rev00bb', verdict: 'fail', pr: 'https://github.com/acme/webshop/pull/8', ts: at(1) },
    ],
  });
  assert.deepEqual(reasons(failed), ['1 fix review failed']);
  const rejectedOnly = analyze({
    session: { id: 'quietrej', status: 'pr_opened' },
    events: [
      { type: 'rejected', title: '…', reason: '…', ts: at(1) },
    ],
  });
  assert.deepEqual(reasons(rejectedOnly), []);
});

test('the summary adds up the finding chain and names it in the report', () => {
  const s = summarize([analyze(chain), analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })]);
  assert.equal(s.findings, 1);
  assert.equal(s.validated, 1);
  assert.equal(s.rejected, 0);
  assert.equal(s.fix_colonies, 1);
  assert.equal(s.reviews, 1);
  assert.equal(s.review_passed, 1);
  assert.equal(s.merged, 1);
  const report = formatReport(s, [analyze(chain), analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })]);
  assert.match(report, /Findings filed: 1 \(1 validated, 1 fix, 1 review, 1 passed, merged: 1\)/);
  const quietSummary = summarize([analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })]);
  const quietReport = formatReport(quietSummary, [analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })]);
  assert.match(quietReport, /Findings filed: 0\./);
  assert.ok(!quietReport.split('\n').find((l) => l.startsWith('- Settlers')).includes('validated'));
});
