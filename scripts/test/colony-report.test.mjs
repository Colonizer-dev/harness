import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { analyze, costsReport, formatCosts, formatReport, formatTranscript, loadColonies, loadSpend, parseOrigins, reasons, redact, spendWindow, summarize, TOKEN_CATEGORIES, totalCost } from '../colony-report.mjs';

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

/** A colony whose lines carry the envelope `origin` (issue #312): a burn-down brief, a nudge, an
 *  operator note, a judge-answered question, the agent, a settler and a mothership line. */
const origins = {
  mothership: 'test',
  session: { id: 'origin01', repo: 'acme/webshop', status: 'pr_opened' },
  events: [
    { type: 'status', state: 'working', ts: at(0), origin: 'system' },
    { type: 'user_message', id: 'initial', text: 'Sweep the backlog', origin: 'burn_down', ts: at(0) },
    { type: 'user_message', id: 'watchdog-o1', text: 'Are you still working?', origin: 'watchdog', ts: at(60) },
    { type: 'user_message', id: 'u-1', text: 'skip the flaky one', origin: 'user', ts: at(90) },
    { type: 'question', question_id: 'q1', questions: [{ question: 'Keep going?', options: [{ label: 'Yes' }] }], ts: at(100) },
    { type: 'question_answered', question_id: 'q1', answers: { 'Keep going?': 'Yes' }, origin: 'autonomy', ts: at(110) },
    { type: 'assistant_text', message_id: 'm1', text: 'Done.', origin: 'agent', ts: at(120) },
    { type: 'tool_call', tool_call_id: 't1', name: 'Read', input: { file_path: '/workspace/a.js' }, agent: { id: 'k1', name: 'Explore' }, origin: 'subagent', ts: at(130) },
  ],
  logs: [{ type: 'harness_log', level: 'warn', message: 'restarting the colony', ts: at(140), origin: 'system' }],
};

test('a transcript tags each line by its origin, and a judge’s answer reads apart from the operator’s', () => {
  const text = formatTranscript(origins);
  assert.match(text, /\[burn-down\] brief: Sweep the backlog/);
  assert.match(text, /⚑ watchdog nudge/);
  assert.match(text, /you: skip the flaky one/);
  assert.match(text, /✓ \[judge\] answered \(after 10s\): Yes/, 'a judge’s answer is not the operator’s ✓ answered');
  assert.match(text, /says: Done\./, "the agent's own words need no tag");
  assert.match(text, /\[Explore\] → Read \/workspace\/a\.js/, 'a settler’s [name] is its tag');
  assert.match(text, /\[system\] ! mothership warn: restarting the colony/);
});

test('--origin filters a transcript to the origins named, and refuses anything outside the set', () => {
  const judge = formatTranscript({ ...origins, origins: parseOrigins('autonomy,watchdog') });
  assert.match(judge, /✓ \[judge\] answered: Yes/, 'the agent’s question is filtered out, so no wait is measured');
  assert.match(judge, /⚑ watchdog nudge/);
  assert.doesNotMatch(judge, /Sweep the backlog/);
  assert.doesNotMatch(judge, /skip the flaky one/);
  assert.doesNotMatch(judge, /says: Done\./);

  assert.throws(() => parseOrigins('autonomy,nope'), /unknown origin nope \(one of /);
  assert.throws(() => parseOrigins(','), /--origin needs/);
});

test('a line recorded before the envelope keeps today’s reading, tag-free and filterable by it', () => {
  const legacy = {
    mothership: 'test',
    session: { id: 'legacy01' },
    events: [
      { type: 'user_message', id: 'initial', text: 'Resolve issue 42', ts: at(0) },
      { type: 'user_message', id: 'watchdog-l1', text: 'Are you still working?', ts: at(60) },
      { type: 'user_message', id: 'u-1', text: 'try the other file', ts: at(120) },
      { type: 'question', question_id: 'q1', questions: [{ question: 'Which one?', options: [{ label: 'A' }] }], ts: at(130) },
      { type: 'question_answered', question_id: 'q1', answers: { 'Which one?': 'A' }, ts: at(140) },
    ],
  };
  const text = formatTranscript(legacy);
  assert.match(text, /brief: Resolve issue 42/);
  assert.match(text, /⚑ watchdog nudge/);
  assert.match(text, /you: try the other file/);
  assert.match(text, /✓ answered \(after 10s\): A/, 'an answer with no origin is the operator’s, as it always read');
  assert.doesNotMatch(text, /\[judge\]|\[burn-down\]|\[system\]/);

  assert.match(formatTranscript({ ...legacy, origins: parseOrigins('watchdog') }), /⚑ watchdog nudge/);
  assert.doesNotMatch(formatTranscript({ ...legacy, origins: parseOrigins('watchdog') }), /try the other file/);
  // The agent's question is filtered away with the rest of its lines, so no wait is measured to show.
  assert.match(formatTranscript({ ...legacy, origins: parseOrigins('user') }), /\+2m20s\s+✓ answered: A/);
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

/** A colony whose final turn claimed completion; the mothership then verified the claim on its own. */
const claimed = (verification) => ({
  mothership: 'test',
  session: { id: 'verif01', repo: 'acme/webshop', issue: 11, status: 'pr_opened', pr_url: 'https://…/2' },
  events: [
    { type: 'status', state: 'working', ts: at(0) },
    { type: 'assistant_text', message_id: 'm1', text: 'Done: the scanner now honours llms.txt, and the tests pass.', ts: at(280) },
    { type: 'turn_end', is_error: false, duration_ms: 300_000, cost_usd: 0.4, ts: at(300) },
    { type: 'verification', ...verification, ts: at(360) },
  ],
});

const confirmed = {
  verdict: 'confirmed', by_declaration: false, summary: '`npm test` green in a fresh checkout', contradictions: [],
  command: 'npm test', command_source: 'package.json', exit_code: 0, tests_ms: 8_100, commits: 2,
  files_changed: ['src/scan.rs', 'src/llms.txt', 'test/scan.rs', 'README.md'], snapshot: 'deadbeef', ms: 12_345,
};
const contradicted = {
  verdict: 'contradicted', by_declaration: false, summary: '`npm test` exited 1 in a fresh checkout',
  contradictions: ['described `x.rs` is not on the branch'], command: 'npm test', command_source: 'package.json',
  exit_code: 1, tests_ms: 38_000, commits: 3, files_changed: ['a', 'b'], snapshot: 'deadbeef', ms: 41_234,
};
const noCommand = {
  verdict: 'unverifiable', by_declaration: false, summary: 'no test command known for this repository', contradictions: [],
  command: null, command_source: null, exit_code: null, tests_ms: null, commits: 1, files_changed: ['a'], snapshot: null, ms: 900,
};
const byDeclaration = {
  verdict: 'unverifiable', by_declaration: true, summary: 'verify is none for this colony', contradictions: [],
  command: null, command_source: null, exit_code: null, tests_ms: null, commits: 1, files_changed: ['a'], snapshot: null, ms: 12,
};

test('a verification is counted, keeps the last verdict, and a contradicted claim is worth reading', () => {
  const r = analyze(claimed(confirmed));
  assert.equal(r.verifications, 1);
  assert.equal(r.verification_verdict, 'confirmed');
  assert.equal(r.verification_ms, 12_345);
  assert.deepEqual(reasons(r), []);
  const c = analyze(claimed(contradicted));
  assert.equal(c.contradicted_claims, 1);
  assert.deepEqual(reasons(c), ['1 contradicted completion claim']);
});

test('the summary counts verifications and the report names them', () => {
  const reports = [
    analyze(claimed(confirmed)),
    analyze(claimed(contradicted)),
    analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] }),
  ];
  const s = summarize(reports);
  assert.equal(s.verifications, 2);
  assert.equal(s.contradicted_claims, 1);
  assert.match(formatReport(s, reports), /Verifications: 2, 1 contradicted\./);
  const quiet = summarize([reports[2]]);
  assert.match(formatReport(quiet, [reports[2]]), /Verifications: 0\./);
});

test('a transcript sets the claim and its verdict side by side, one line per verdict', () => {
  const text = formatTranscript(claimed(confirmed));
  assert.match(text, /says: Done: the scanner now honours llms\.txt/);
  assert.match(text, /∎ verification: CONFIRMED — `npm test` green in a fresh checkout, 4 files, 2 commits \(12\.3s\)/);
  assert.match(formatTranscript(claimed(contradicted)), /∎ verification: CONTRADICTED — `npm test` exited 1 in a fresh checkout; described `x\.rs` is not on the branch \(41\.2s\)/);
  assert.match(formatTranscript(claimed(noCommand)), /∎ verification: UNVERIFIABLE — no test command known for this repository, 1 file, 1 commit \(0\.9s\)/);
  assert.match(formatTranscript(claimed(byDeclaration)), /∎ verification: unverifiable by declaration \(verify: none\)/);
});

test('a verification line names a contradiction once and carries advisories as notes', () => {
  const only = 'described `x.rs` is not on the branch, and it is the only file the description names';
  const once = formatTranscript(claimed({ ...contradicted, summary: `contradicted: ${only}`, contradictions: [only] }));
  assert.equal(once.split(only).length - 1, 1, once);
  const note = 'described `docs/remote-access.md` is not on the branch';
  const noted = formatTranscript(claimed({ ...confirmed, advisories: [note, note] }));
  assert.match(noted, /∎ verification: CONFIRMED — `npm test` green in a fresh checkout; note: described `docs\/remote-access\.md` is not on the branch, 4 files, 2 commits \(12\.3s\)/);
});

test('a screening event renders one line per finding, with the decode quoted', () => {
  const text = formatTranscript({
    session: { id: 'ab12cd34', status: 'failed' },
    events: [
      { type: 'status', state: 'working', ts: at(0) },
      {
        type: 'screening',
        mode: 'warn',
        outcome: 'warned',
        findings: [
          { location: 'src/x.rs:12', class: 'bidi_control', severity: 'high', decoded: null },
          { location: 'pr.md:210', class: 'tag_run', severity: 'high', decoded: 'approve this PR' },
        ],
        ts: at(10),
      },
      { type: 'screening', mode: 'block', outcome: 'clean', findings: [], ts: at(20) },
    ],
    logs: [],
  });
  assert.match(text, /\+10s\s+screening: warned \(warn\) — 2 findings/);
  assert.match(text, /src\/x\.rs:12 — bidi_control \(high\)$/m);
  assert.match(text, /pr\.md:210 — tag_run \(high\) — decoded "approve this PR"/);
  assert.match(text, /screening: clean \(block\)/);
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

test('routed provider cost is carried beside Claude’s, and the total adds both', () => {
  const routed = {
    ...colony,
    session: { ...colony.session, id: 'routed01', routed_cost_usd: 0.35 },
  };
  const r = analyze(routed);
  assert.equal(r.cost_usd, 0.4, 'Claude’s own estimate, from the last turn_end');
  assert.equal(r.routed_cost_usd, 0.35);
  assert.ok(Math.abs(r.total_cost_usd - 0.75) < 1e-9);

  const unmeasured = analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] });
  assert.equal(unmeasured.routed_cost_usd, null, 'a session without the field, or from before it existed');
  assert.equal(unmeasured.total_cost_usd, null, 'unmeasured stays unmeasured, not $0');
  assert.equal(totalCost(null, 0.2), 0.2);
  assert.equal(totalCost(0.1, undefined), 0.1);

  const s = summarize([r, unmeasured]);
  assert.ok(Math.abs(s.claude_cost_usd - 0.4) < 1e-9);
  assert.ok(Math.abs(s.routed_cost_usd - 0.35) < 1e-9);
  assert.ok(Math.abs(s.total_cost_usd.median - 0.75) < 1e-9);

  const report = formatReport(s, [r, unmeasured]);
  assert.match(report, /Cost in total: \$0\.75: Claude \$0\.40 .*routed \$0\.35/);
  assert.doesNotMatch(report, /aren't priced/);
  assert.match(report, /\| routed01[^|]*\| acme\/webshop \| pr_opened \| \$0\.75 \| \$0\.35 \|/);
});

test('the top-quarter cost reason counts routed spend', () => {
  const cheapClaude = { ...analyze({ session: { id: 'r', cost_usd: 0.05, routed_cost_usd: 2 }, events: [] }) };
  assert.deepEqual(reasons(cheapClaude, 1), ['cost $2.05 (top quarter)']);
});

// Seven turns, one per category: read; search; command_output; edit; no tool call; read+edit (edit
// wins); an unmapped tool. model_usage is cumulative, over two models on some turns.
const counts = (input, output, cache_read = 0, cache_write = 0) => ({ input_tokens: input, output_tokens: output, cache_read_tokens: cache_read, cache_write_tokens: cache_write });
const call = (id, name) => ({ type: 'tool_call', tool_call_id: id, name, input: {}, ts: at(0) });
const end = (model_usage, extra = {}) => ({ type: 'turn_end', is_error: false, duration_ms: 1000, model_usage, ts: at(0), ...extra });
const categorized = {
  mothership: 'test',
  session: { id: 'tokens01', repo: 'acme/webshop', status: 'pr_opened' },
  events: [
    { type: 'status', state: 'working', ts: at(0) },
    call('r1', 'Read'),
    end({ 'claude-opus-5': counts(100, 50, 1000, 200) }, { ts: at(1) }),
    call('g1', 'Grep'),
    call('g2', 'Glob'),
    end({ 'claude-opus-5': counts(300, 90, 3000, 200) }, { ts: at(2) }),
    call('b1', 'Bash'),
    end({ 'claude-opus-5': counts(500, 140, 3000, 600), 'zai/glm-5.3-flash': counts(70, 30) }, { ts: at(3) }),
    call('e1', 'Edit'),
    call('e2', 'Write'),
    end({ 'claude-opus-5': counts(700, 190, 5000, 600), 'zai/glm-5.3-flash': counts(170, 30) }, { ts: at(4) }),
    end({ 'claude-opus-5': counts(800, 200, 5000, 600), 'zai/glm-5.3-flash': counts(170, 30) }, { ts: at(5) }),
    call('m1', 'Read'),
    call('m2', 'Edit'),
    end({ 'claude-opus-5': counts(1000, 250, 5000, 600), 'zai/glm-5.3-flash': counts(170, 30) }, { ts: at(6) }),
    call('t1', 'Task'),
    end({ 'claude-opus-5': counts(1050, 260, 5000, 600), 'zai/glm-5.3-flash': counts(170, 30) }, { ts: at(7) }),
  ],
};

test('each turn bills its whole spend to one category, by the tools it called', () => {
  const r = analyze(categorized);
  assert.equal(r.tokenCategories.read, 150);
  assert.equal(r.tokenCategories.search, 240);
  assert.equal(r.tokenCategories.command_output, 350, 'every model of the turn, the routed one included');
  assert.equal(r.tokenCategories.edit, 600, 'a turn that read and then edited bills edit, whole');
  assert.equal(r.tokenCategories.reasoning, 170, 'no tool call at all, or an unmapped one, is reasoning');
  assert.equal(r.tokenCategories.replay, 5600, 'cache reads and writes are replay, whatever else the turn did');
  assert.deepEqual(r.tokenCategoriesByModel.command_output, { 'claude-opus-5': 250, 'zai/glm-5.3-flash': 100 });
  assert.deepEqual(r.tokenCategoriesByModel.replay, { 'claude-opus-5': 5600 });
});

test('the categories add up exactly to the recorded usage', () => {
  const r = analyze(categorized);
  const filed = TOKEN_CATEGORIES.reduce((a, c) => a + r.tokenCategories[c], 0);
  const recorded = Object.values(r.model_usage).reduce((a, m) => a + m.input_tokens + m.output_tokens + m.cache_read_tokens + m.cache_write_tokens, 0);
  assert.equal(filed, recorded, 'every token delta is assigned to exactly one bucket');
});

test('a subagent’s tool calls bill to the colony’s turn, which they are part of', () => {
  const r = analyze({
    mothership: 'test',
    session: { id: 'tokens02' },
    events: [
      { type: 'status', state: 'working', ts: at(0) },
      call('r1', 'Read'),
      { ...call('g1', 'Grep'), agent: { id: 'task1', name: 'Explore' } },
      { ...call('e1', 'Edit'), agent: { id: 'task1', name: 'Explore' } },
      end({ 'claude-opus-5': counts(300, 90) }, { ts: at(1) }),
    ],
  });
  assert.equal(r.tokenCategories.edit, 390, 'the subagent’s Edit outranks the reads, over the union of the turn’s tools');
  assert.equal(r.tokenCategories.reasoning, 0, 'delegation is not reasoning');
  const filed = TOKEN_CATEGORIES.reduce((a, c) => a + r.tokenCategories[c], 0);
  const recorded = Object.values(r.model_usage).reduce((a, m) => a + m.input_tokens + m.output_tokens, 0);
  assert.equal(filed, recorded);
});

test('a turn_end without model_usage keeps the baseline the next measured turn diffs against', () => {
  const r = analyze({
    mothership: 'test',
    session: { id: 'tokens03' },
    events: [
      { type: 'status', state: 'working', ts: at(0) },
      call('r1', 'Read'),
      end({ 'claude-opus-5': counts(100, 50, 1000) }, { ts: at(1) }),
      call('b1', 'Bash'),
      { type: 'turn_end', is_error: false, duration_ms: 1000, ts: at(2) },
      call('e1', 'Edit'),
      end({ 'claude-opus-5': counts(200, 90, 1000) }, { ts: at(3) }),
    ],
  });
  assert.equal(r.tokenCategories.read, 150);
  assert.equal(r.tokenCategories.edit, 140, 'the delta over the surviving baseline, not over an emptied one');
  assert.equal(r.tokenCategories.command_output, 0, 'the unmeasured turn attributes nothing itself');
  const filed = TOKEN_CATEGORIES.reduce((a, c) => a + r.tokenCategories[c], 0);
  const recorded = Object.values(r.model_usage).reduce((a, m) => a + m.input_tokens + m.output_tokens + m.cache_read_tokens + m.cache_write_tokens, 0);
  assert.equal(filed, recorded, 'no double count across the gap');
});

test('the summary and the report carry the categories', () => {
  const reports = [analyze(categorized), analyze({ session: { id: 'plain', status: 'pr_opened', pr_url: 'u' }, events: [] })];
  const s = summarize(reports);
  assert.equal(s.tokenCategories.read, 150);
  assert.equal(s.tokenCategories.edit, 600);
  const report = formatReport(s, reports);
  assert.match(report, /## Token categories/);
  assert.match(report, /\| replay \| 5600 \| 79%/);
});

/** A colony whose gateway logged one routed request and two failures. Every line smuggles the fields
 *  a real audit record carries beside the allowlisted ones — keys and the request body itself. */
const secrets = { authorization: 'Bearer sk-ant-api03-FAKEKEY', x_api_key: 'sk-FAKE', body: 'SECRET_PROMPT_TEXT' };
const routed = (ts, extra = {}) => ({
  type: 'gateway_request', ts, colony: 'gatew01', provider: 'downstream', wire: 'anthropic',
  method: 'POST', path: '/v1/messages', fallback: false, queue_ms: 0, duration_ms: 40,
  request_bytes: 512, response_bytes: 0, input_tokens: null, output_tokens: null, ...secrets, ...extra,
});
const gatewayColony = {
  mothership: 'test',
  session: { id: 'gatew01', repo: 'acme/webshop', status: 'pr_opened' },
  events: [{ type: 'status', state: 'working', ts: at(0) }],
  logs: [],
  // Stored out of order: the transcript still reads them by their ts.
  gateway: [
    routed(at(3), { status: 429, failure: 'queue_full', queue_ms: 5_000, duration_ms: 5_001, model: 'claude-opus-5', wire_model: 'claude-opus-5' }),
    routed(at(1), { provider: 'zai', wire: 'openai', status: 200, failure: null, queue_ms: 3, duration_ms: 812, request_bytes: 1024, response_bytes: 4096, input_tokens: 100, output_tokens: 50, model: 'claude-opus-5', wire_model: 'glm-5.3-flash' }),
    routed(at(2), { status: 502, failure: 'unreachable', fallback: true, model: null, wire_model: null }),
  ],
};

test('a transcript renders gateway requests in ts order, from the allowlisted fields only', () => {
  const text = formatTranscript(gatewayColony);
  const gate = text.split('\n').filter((l) => l.includes('~ gateway'));
  assert.equal(gate.length, 3);
  assert.match(gate[0], /~ gateway zai claude-opus-5→glm-5\.3-flash POST \/v1\/messages 200 812ms q3ms 1024B→4096B$/);
  assert.match(gate[1], /~ gateway downstream – POST \/v1\/messages 502 40ms q0ms 512B→0B failure unreachable fallback/);
  assert.match(gate[2], /~ gateway downstream claude-opus-5 POST \/v1\/messages 429 5001ms q5000ms 512B→0B failure queue_full$/);
  // What the lines were carrying beside the allowlisted fields stays in the file.
  assert.doesNotMatch(text, /sk-|Bearer |SECRET_PROMPT_TEXT/);
});

test('gateway requests and their failures are counted per colony', () => {
  const r = analyze(gatewayColony);
  assert.equal(r.gateway_requests, 3);
  assert.deepEqual(r.gateway_failures, { unreachable: 1, queue_full: 1 });
  assert.ok(reasons(r).includes('2 gateway requests failed'), reasons(r).join('; '));
  assert.deepEqual(analyze({ session: { id: 'quiet' }, events: [] }).gateway_failures, {});
});

/** A data dir whose one colony is known only from its sessions/ directory; removed after the test. */
function dataDir(t, sessionsJson) {
  const dir = mkdtempSync(join(tmpdir(), 'colony-report-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'sessions.json'), sessionsJson);
  mkdirSync(join(dir, 'sessions', 'lost'), { recursive: true });
  return dir;
}

test('a sessions.json that is valid JSON but not an array is treated as corrupt, not fatal', (t) => {
  for (const body of ['{}', 'null', '"x"', '{broken']) {
    const [colony] = loadColonies(dataDir(t, body), 'd');
    assert.deepEqual(colony, { mothership: 'd', session: { id: 'lost' }, events: [], logs: [], gateway: [] });
  }
});

test('a colony with more events than Math.max can take still reports its span', () => {
  const n = 200_000;
  const events = Array.from({ length: n }, (_, i) => ({ type: 'tool_call', tool_call_id: `t${i}`, name: 'Read', input: {}, ts: at(i) }));
  const r = analyze({ session: { id: 'big' }, events, logs: [] });
  assert.equal(r.wall_ms, (n - 1) * 1000);
});

// -------------------------------------------------------------------------------------------- --costs

/** The shared journal fixture, also read by the Rust reader test (crates/colonizer/src/spend.rs
 *  include_str!s it), as a data dir with the sessions its rows name. */
const FIXTURE = join(dirname(fileURLToPath(import.meta.url)), 'fixtures', 'spend-costs.jsonl');
// The window the fixture tests are pinned to: it covers the fixture's days (2026-09-14..15), so the
// totals below stay the whole journal's whatever day the suite runs on.
const WINDOW = { floor: '2026-09-01', today: '2026-09-30' };
const SESSIONS = [
  { id: 'claudeaa', repo: 'acme/webshop', issue: 296, status: 'pr_opened', agent: 'claude-code' },
  { id: 'routedbb', repo: 'acme/gateway', status: 'merged', agent: 'codex' },
  { id: 'freecc', repo: 'acme/webshop', status: 'failed', agent: 'opencode' },
];
function spendDir(t) {
  const dir = dataDir(t, JSON.stringify(SESSIONS));
  writeFileSync(join(dir, 'spend.jsonl'), readFileSync(FIXTURE, 'utf8'));
  return dir;
}

/** The server's own sum over the whole journal (crates/colonizer/src/spend.rs `aggregate`): usage
 *  rows carry the four token classes and the estimated dollar, routed rows the metered one. */
function historyTotals(rows) {
  const t = { cost_usd: null, routed_cost_usd: null, input: 0, output: 0, cache_read: 0, cache_write: 0 };
  for (const row of rows) {
    if (row.kind !== 'usage') {
      if (row.kind === 'routed' && typeof row.cost_usd === 'number') t.routed_cost_usd = (t.routed_cost_usd ?? 0) + row.cost_usd;
      continue;
    }
    for (const k of ['input', 'output', 'cache_read', 'cache_write']) t[k] += row[`${k}_tokens`] ?? 0;
    if (typeof row.cost_usd === 'number') t.cost_usd = (t.cost_usd ?? 0) + row.cost_usd;
  }
  return t;
}

test('--costs groups the journal by colony, harness first, and ranks by spend', (t) => {
  const dir = spendDir(t);
  const rows = loadSpend(dir);
  assert.equal(rows.length, 17, 'the torn line is skipped, the rest read');
  const costs = costsReport(rows, loadColonies(dir), { window: WINDOW });
  assert.deepEqual(
    costs.colonies.map((c) => [c.session, c.agent, c.total_usd]),
    [['claudeaa', 'claude-code', 1.0], ['routedbb', 'codex', 0.375], ['freecc', 'opencode', null]],
    'priced colonies first, the unpriced one last',
  );
  const cc = costs.colonies[0];
  assert.equal(cc.repo, 'acme/webshop');
  assert.equal(cc.status, 'pr_opened');
  assert.equal(cc.estimated_usd, 1.0);
  assert.equal(cc.metered_usd, null, 'no routed rows, no metered dollars');
  assert.deepEqual(cc.models.map((m) => m.model), ['claude-opus-5', 'zai/glm-5.3-flash', 'deepseek/deepseek-flash'], 'largest first');
  const opus = cc.models[0];
  assert.deepEqual([opus.tokens, opus.input, opus.output, opus.cache_read], [660, 400, 60, 200]);
  assert.equal(opus.cost_usd, 0.75, 'the single-model turns’ cost rides the model row');
  assert.equal(cc.models[1].cost_usd, null);
  assert.equal(cc.models[2].cost_usd, null, 'the multi-model turn’s dollar is attributed nowhere');
  assert.deepEqual([cc.launched, cc.returned], [1, 1], 'run edges count, and add no dollars');
  const rb = costs.colonies[1];
  assert.equal(rb.estimated_usd, null);
  assert.equal(rb.metered_usd, 0.375);
  assert.equal(rb.models[0].cost_usd, null, 'metered dollars never ride a model row');
});

test('--costs sends legacy and chat rows to unattributed, so the totals sum to the whole journal', (t) => {
  const dir = spendDir(t);
  const costs = costsReport(loadSpend(dir), loadColonies(dir), { window: WINDOW });
  const u = costs.unattributed;
  assert.equal(u.session, null);
  assert.equal(u.agent, null);
  assert.equal(u.estimated_usd, 0.1875);
  assert.equal(u.metered_usd, null);
  assert.equal(u.models[0].model, 'claude-opus-5');
  assert.equal(u.models[0].cost_usd, 0.1875);
  // Reconciliation: the same fixture, summed the way GET /api/spend/history sums it. The Rust reader
  // test (crates/colonizer/src/spend.rs) asserts these same constants over the same fixture.
  const history = historyTotals(loadSpend(dir));
  assert.ok(Math.abs(history.cost_usd - 1.1875) < 1e-9, 'usage-row cost, chat and legacy included');
  assert.ok(Math.abs(history.routed_cost_usd - 0.375) < 1e-9);
  assert.equal(history.input, 1300);
  assert.equal(history.output, 305);
  assert.equal(history.cache_read, 200);
  assert.equal(history.cache_write, 0);
  assert.ok(Math.abs(costs.totals.estimated_usd - history.cost_usd) < 1e-9);
  assert.ok(Math.abs(costs.totals.metered_usd - history.routed_cost_usd) < 1e-9);
  assert.equal(costs.totals.input_tokens, 1300);
  assert.equal(costs.totals.output_tokens, 305);
  assert.equal(costs.totals.cache_read_tokens, 200);
  assert.equal(costs.totals.cache_write_tokens, 0);
  assert.ok(Math.abs(costs.totals.total_usd - 1.5625) < 1e-9);
});

test('an unpriced colony shows its tokens under the unpriced label, never as $0', (t) => {
  const dir = spendDir(t);
  const costs = costsReport(loadSpend(dir), loadColonies(dir), { window: WINDOW });
  const free = costs.colonies.find((c) => c.session === 'freecc');
  assert.equal(free.priced, false);
  assert.equal(free.label, 'unpriced — tokens only');
  assert.equal(free.total_usd, null);
  assert.deepEqual([free.input_tokens, free.output_tokens], [500, 120]);
  const text = formatCosts(costs);
  assert.match(text, /unpriced — tokens only/);
  assert.match(text, /\| claudeaa \| claude-code \| acme\/webshop \| pr_opened \| 775 \| \$1\.00 \| – \| \$1\.00 \|/);
  assert.match(text, /\| unattributed \| – \| – \| – \| 170 \| \$0\.19 \| – \| \$0\.19 \|/);
  assert.match(text, /Total \$1\.56 over 2026-09-01\.\.2026-09-30: estimated \$1\.19 \(the agent's own per-turn estimate\), metered \$0\.38 \(priced by the gateway\)/);
  assert.match(text, /\| codex \| deepseek\/deepseek-flash \| 1 \| 240 \| – \|/, 'a model’s cost stays – until a dollar rode its rows');
  assert.equal(costs.harness_model.filter((h) => h.model === 'claude-opus-5').length, 1, 'legacy and chat rows name no harness, so roll up nowhere');
});

test('--costs honours --repo through sessions.json, and rows without a session have neither', (t) => {
  const dir = spendDir(t);
  const costs = costsReport(loadSpend(dir), loadColonies(dir), { repo: 'acme/webshop', window: WINDOW });
  assert.deepEqual(costs.colonies.map((c) => c.session), ['claudeaa', 'freecc']);
  assert.equal(costs.unattributed.tokens, 0, 'legacy and chat name no repo, so they drop with it');
  assert.ok(Math.abs(costs.totals.estimated_usd - 1.0) < 1e-9);
  assert.equal(costs.totals.metered_usd, null);
});

test('--costs defaults to the window the spend history answers: 30 days ending today, floor at today - 29', () => {
  // spend.rs `day_window`: the floor is today - (days - 1), so days=1 is just today.
  assert.deepEqual(spendWindow({ today: '2026-09-25' }), { floor: '2026-08-27', today: '2026-09-25' });
  assert.deepEqual(spendWindow({ days: 1, today: '2026-09-25' }), { floor: '2026-09-25', today: '2026-09-25' });
  assert.deepEqual(spendWindow({ days: 400, today: '2026-09-25' }), { floor: '2025-09-26', today: '2026-09-25' }, 'clamped to 365, like the endpoint');
  assert.deepEqual(spendWindow({ since: '2026-09-01T12:00:00Z', today: '2026-09-25' }), { floor: '2026-09-01', today: '2026-09-25' }, '--since overrides the floor, the today ceiling stays');
  assert.deepEqual(spendWindow({ since: 'garbage', today: '2026-09-25' }), { floor: '9999-12-31', today: '2026-09-25' }, 'an unparseable --since still means nothing');
});

test('--costs keeps only the rows inside its window, future-dated ones with the rest', (t) => {
  const dir = spendDir(t);
  const rows = loadSpend(dir);
  const colonies = loadColonies(dir);
  assert.ok(Math.abs(costsReport(rows, colonies, { window: WINDOW }).totals.estimated_usd - 1.1875) < 1e-9, 'a window over the fixture keeps every row');
  const day = costsReport(rows, colonies, { window: { floor: '2026-09-15', today: '2026-09-15' } });
  assert.ok(Math.abs(day.totals.estimated_usd - 0.0625) < 1e-9, "the 14th's usage rows drop");
  assert.ok(Math.abs(day.totals.metered_usd - 0.375) < 1e-9);
  assert.deepEqual(day.colonies.map((c) => c.session), ['routedbb', 'freecc']);
  const future = { kind: 'usage', ts: 'x', day: '2026-10-01', org: 'acme', session: 'claudeaa', input_tokens: 9 };
  assert.equal(
    costsReport([...rows, future], colonies, { window: WINDOW }).totals.input_tokens,
    costsReport(rows, colonies, { window: WINDOW }).totals.input_tokens,
    'a row dated after today drops',
  );
});

test('loadSpend drops a row whole when a field has the type the server would refuse', (t) => {
  const dir = dataDir(t, '[]');
  const good = { ts: '2026-09-15T10:00:00Z', day: '2026-09-15', org: 'acme', kind: 'usage', session: 's1', input_tokens: 3, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, cost_usd: 0.1 };
  const rows = [
    { ...good, input_tokens: -1 },
    { ...good, output_tokens: 1.5 },
    { ...good, cache_read_tokens: null },
    { ...good, cost_usd: '0.1' },
    { ...good, session: 7 },
    { ...good, day: 20260915 },
    good,
    { ...good, cost_usd: null, session: null, model: null }, // Option fields: null is "absent", kept
  ];
  writeFileSync(join(dir, 'spend.jsonl'), rows.map((r) => JSON.stringify(r)).join('\n') + '\n');
  assert.deepEqual(loadSpend(dir), [rows[6], rows[7]]);
});
