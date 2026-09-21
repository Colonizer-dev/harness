#!/usr/bin/env node
// How colonies actually went, from what the mothership already records: every colony's events.jsonl and
// harness.jsonl, and sessions.json. Nothing leaves the machine. Use it to find the colonies worth reading
// before changing a prompt or a module setting, and to compare a fixed set of tasks before and after.
//
//   node scripts/colony-report.mjs                        # this mothership (COLONIZER_DATA_DIR or ~/.local/share/colonizer)
//   node scripts/colony-report.mjs --data ./omarchy-data  # a copy of another mothership's data dir; repeatable
//   node scripts/colony-report.mjs --since 2026-09-01 --repo owner/name --worst 10
//   node scripts/colony-report.mjs --json > report.json
//   node scripts/colony-report.mjs --transcript fc742075  # one colony, step by step, secrets redacted
//
// Only the metadata of tool calls is summarised (names, file paths, commands). Transcripts print short
// excerpts of text and failed output, with common credential patterns redacted, but read them before sharing.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

// ---------------------------------------------------------------------------------------------- reading

function readJsonLines(path) {
  if (!existsSync(path)) return [];
  const out = [];
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    if (!line.trim()) continue;
    try {
      out.push(JSON.parse(line));
    } catch {
      // A line cut off by a crash or a full disk: skip it rather than lose the colony.
    }
  }
  return out;
}

/** Every colony in a data dir: the ones sessions.json knows, and any session directory it has lost. */
export function loadColonies(dataDir, label = basename(resolve(dataDir))) {
  let records = [];
  try {
    records = JSON.parse(readFileSync(join(dataDir, 'sessions.json'), 'utf8'));
  } catch {
    records = [];
  }
  const byId = new Map(records.map((s) => [s.id, s]));
  const sessionsDir = join(dataDir, 'sessions');
  const ids = new Set(byId.keys());
  if (existsSync(sessionsDir)) for (const id of readdirSync(sessionsDir)) ids.add(id);
  return [...ids].map((id) => ({
    mothership: label,
    session: byId.get(id) ?? { id },
    events: readJsonLines(join(sessionsDir, id, 'events.jsonl')),
    logs: readJsonLines(join(sessionsDir, id, 'harness.jsonl')),
  }));
}

// ------------------------------------------------------------------------------------------- analysing

const ms = (ts) => (ts ? Date.parse(ts) : NaN);
const RATE_LIMIT = /\b429\b|rate[ _-]?limit|overloaded|too many requests|quota/i;
const PLAIN_TEXT_REPROMPT = /asked in plain text/i;
const QUIET_STATES = new Set(['waiting_for_answer', 'idle', 'exited', 'error']);

/** One colony's numbers. Pure: takes what loadColonies read, so it can be tested without a mothership. */
export function analyze({ mothership = '', session = {}, events = [], logs = [] }) {
  const tools = {};
  const reads = new Map();
  const commands = new Map();
  const calls = new Map();
  const subagents = new Map();
  const openQuestions = new Map();
  const r = {
    id: session.id,
    mothership,
    repo: session.repo ?? null,
    issue: session.issue ?? null,
    title: session.issue_title ?? null,
    status: session.status ?? 'unknown',
    pr_url: session.pr_url ?? null,
    created_at: session.created_at ?? events[0]?.ts ?? null,
    boot_ms: session.boot_timing?.total_ms ?? null,
    wall_ms: 0,
    working_ms: 0,
    turns: 0,
    failed_turns: 0,
    cost_usd: session.cost_usd ?? null,
    model_usage: null,
    tool_calls: 0,
    tool_errors: 0,
    subagent_tool_calls: 0,
    subagents: 0,
    subagent_types: {},
    questions: 0,
    questions_answered: 0,
    answer_wait_ms: 0,
    longest_answer_wait_ms: 0,
    user_messages: 0,
    watchdog_nudges: 0,
    plain_text_reprompts: 0,
    rate_limit_hits: 0,
    errors_logged: 0,
    longest_silence_ms: 0,
    findings: 0,
    validated: 0,
    rejected: 0,
    fix_colonies: 0,
    reviews: 0,
    review_passed: 0,
    merged: 0,
    memory_proposals: 0,
    repeated_reads: [],
    repeated_commands: [],
    tools,
  };

  const times = events.map((e) => ms(e.ts)).filter(Number.isFinite);
  if (times.length > 1) r.wall_ms = Math.max(...times) - Math.min(...times);

  let state = 'idle';
  let lastTs = NaN;
  for (const e of events) {
    const t = ms(e.ts);
    // Silence counts only while the agent is meant to be working: waiting for an answer is the user's time.
    if (Number.isFinite(t) && Number.isFinite(lastTs) && !QUIET_STATES.has(state)) {
      r.longest_silence_ms = Math.max(r.longest_silence_ms, t - lastTs);
    }
    if (Number.isFinite(t)) lastTs = t;

    switch (e.type) {
      case 'status':
        state = e.state ?? state;
        break;
      case 'user_message':
        if (String(e.id ?? '').startsWith('watchdog-')) r.watchdog_nudges += 1;
        else if (e.id !== 'initial') r.user_messages += 1;
        break;
      case 'tool_call': {
        r.tool_calls += 1;
        const name = e.name ?? 'unknown';
        (tools[name] ??= { calls: 0, errors: 0 }).calls += 1;
        calls.set(e.tool_call_id, name);
        if (e.agent) {
          r.subagent_tool_calls += 1;
        }
        if (name === 'Task' || name === 'Agent') {
          const type = e.input?.subagent_type ?? 'general-purpose';
          subagents.set(e.tool_call_id, type);
        }
        const path = e.input?.file_path ?? e.input?.path;
        if (name === 'Read' && path) reads.set(path, (reads.get(path) ?? 0) + 1);
        if (name === 'Bash' && e.input?.command) {
          const command = String(e.input.command).trim();
          commands.set(command, (commands.get(command) ?? 0) + 1);
        }
        break;
      }
      case 'tool_result': {
        const name = calls.get(e.tool_call_id) ?? 'unknown';
        if (e.is_error) {
          r.tool_errors += 1;
          (tools[name] ??= { calls: 0, errors: 0 }).errors += 1;
        }
        if (e.is_error && RATE_LIMIT.test(String(e.output ?? '').slice(0, 2000))) r.rate_limit_hits += 1;
        break;
      }
      case 'question':
        r.questions += 1;
        openQuestions.set(e.question_id, t);
        break;
      case 'question_answered': {
        r.questions_answered += 1;
        const asked = openQuestions.get(e.question_id);
        if (Number.isFinite(asked) && Number.isFinite(t)) {
          r.answer_wait_ms += t - asked;
          r.longest_answer_wait_ms = Math.max(r.longest_answer_wait_ms, t - asked);
        }
        break;
      }
      case 'turn_end':
        r.turns += 1;
        if (e.is_error) {
          r.failed_turns += 1;
          if (RATE_LIMIT.test(String(e.result ?? ''))) r.rate_limit_hits += 1;
        }
        r.working_ms += Number(e.duration_ms) || 0;
        // Cumulative for the colony (docs/protocol.md §2), so the last one is the total.
        if (typeof e.cost_usd === 'number') r.cost_usd = e.cost_usd;
        if (e.model_usage) r.model_usage = e.model_usage;
        break;
      case 'log':
        if (e.level === 'error') r.errors_logged += 1;
        if (PLAIN_TEXT_REPROMPT.test(e.message ?? '')) r.plain_text_reprompts += 1;
        if (e.level !== 'info' && RATE_LIMIT.test(e.message ?? '')) r.rate_limit_hits += 1;
        break;
      case 'finding':
        r.findings += 1;
        break;
      case 'validated':
        r.validated += 1;
        break;
      case 'rejected':
        r.rejected += 1;
        break;
      case 'fix_colony':
        r.fix_colonies += 1;
        break;
      case 'review':
        r.reviews += 1;
        if (e.verdict === 'pass') r.review_passed += 1;
        break;
      case 'merged':
        r.merged += 1;
        break;
      case 'memory_proposal':
        r.memory_proposals += 1;
        break;
    }
  }
  for (const log of logs) {
    if (log.level === 'error') r.errors_logged += 1;
    if (log.level !== 'info' && RATE_LIMIT.test(log.message ?? '')) r.rate_limit_hits += 1;
  }

  // A turn's duration includes the time it waited for an answer, which is the user's time, not the agent's.
  r.working_ms = Math.max(0, r.working_ms - r.answer_wait_ms);
  r.subagents = subagents.size;
  for (const type of subagents.values()) r.subagent_types[type] = (r.subagent_types[type] ?? 0) + 1;
  r.repeated_reads = [...reads].filter(([, n]) => n >= 3).sort((a, b) => b[1] - a[1]).map(([path, n]) => ({ path, n }));
  r.repeated_commands = [...commands]
    .filter(([, n]) => n >= 3)
    .sort((a, b) => b[1] - a[1])
    .map(([command, n]) => ({ command: redact(command).slice(0, 160), n }));
  return r;
}

/** Why a colony is worth reading, strongest reason first. Empty when nothing stands out. */
export function reasons(r, costThreshold = Infinity) {
  const out = [];
  const failedReviews = r.reviews - r.review_passed;
  if (r.status === 'failed') out.push('ended failed');
  if (r.failed_turns > 0) out.push(`${r.failed_turns} failed turn${r.failed_turns === 1 ? '' : 's'}`);
  if (r.status === 'no_changes') out.push('ended with no changes');
  if (r.watchdog_nudges > 0) out.push(`watchdog nudged ${r.watchdog_nudges}×`);
  if (failedReviews > 0) out.push(`${failedReviews} fix review${failedReviews === 1 ? '' : 's'} failed`);
  if (r.rate_limit_hits > 0) out.push(`${r.rate_limit_hits} rate-limit hit${r.rate_limit_hits === 1 ? '' : 's'}`);
  if (r.longest_silence_ms >= 5 * 60_000) out.push(`silent ${duration(r.longest_silence_ms)} while working`);
  if (r.tool_calls >= 10 && r.tool_errors / r.tool_calls >= 0.2) out.push(`${pct(r.tool_errors / r.tool_calls)} of tool calls failed`);
  if (r.plain_text_reprompts > 0) out.push(`asked in plain text ${r.plain_text_reprompts}×`);
  if (r.questions >= 3) out.push(`${r.questions} questions`);
  if (r.repeated_reads.length > 0) out.push(`re-read ${r.repeated_reads[0].path.split('/').pop()} ${r.repeated_reads[0].n}×`);
  if (r.repeated_commands.length > 0) out.push(`ran the same command ${r.repeated_commands[0].n}×`);
  if (typeof r.cost_usd === 'number' && r.cost_usd >= costThreshold) out.push(`cost $${r.cost_usd.toFixed(2)} (top quarter)`);
  return out;
}

function quantile(values, q) {
  const v = values.filter((x) => typeof x === 'number' && Number.isFinite(x)).sort((a, b) => a - b);
  if (v.length === 0) return null;
  return v[Math.min(v.length - 1, Math.floor(q * v.length))];
}

export function summarize(reports) {
  const finished = reports.filter((r) => !['running', 'starting', 'queued', 'idle', 'waiting_for_answer', 'publishing'].includes(r.status));
  const statuses = {};
  for (const r of reports) statuses[r.status] = (statuses[r.status] ?? 0) + 1;
  const tools = {};
  for (const r of reports) {
    for (const [name, t] of Object.entries(r.tools)) {
      const agg = (tools[name] ??= { calls: 0, errors: 0 });
      agg.calls += t.calls;
      agg.errors += t.errors;
    }
  }
  const sum = (key) => reports.reduce((a, r) => a + (Number(r[key]) || 0), 0);
  const stat = (key) => ({ median: quantile(reports.map((r) => r[key]), 0.5), p90: quantile(reports.map((r) => r[key]), 0.9) });
  const costThreshold = quantile(reports.map((r) => r.cost_usd), 0.75) ?? Infinity;
  return {
    colonies: reports.length,
    motherships: [...new Set(reports.map((r) => r.mothership))],
    statuses,
    pr_rate: finished.length ? reports.filter((r) => r.pr_url || r.status === 'pr_opened').length / finished.length : null,
    claude_cost_usd: sum('cost_usd'),
    cost_usd: stat('cost_usd'),
    working_ms: stat('working_ms'),
    wall_ms: stat('wall_ms'),
    turns: stat('turns'),
    tool_calls: stat('tool_calls'),
    questions: { total: sum('questions'), ...stat('questions') },
    answer_wait_ms: { total: sum('answer_wait_ms'), longest: Math.max(0, ...reports.map((r) => r.longest_answer_wait_ms)) },
    boot_ms: stat('boot_ms'),
    watchdog_nudges: sum('watchdog_nudges'),
    plain_text_reprompts: sum('plain_text_reprompts'),
    rate_limit_hits: sum('rate_limit_hits'),
    failed_turns: sum('failed_turns'),
    findings: sum('findings'),
    validated: sum('validated'),
    rejected: sum('rejected'),
    fix_colonies: sum('fix_colonies'),
    reviews: sum('reviews'),
    review_passed: sum('review_passed'),
    merged: sum('merged'),
    memory_proposals: sum('memory_proposals'),
    subagents: sum('subagents'),
    tools: Object.entries(tools)
      .sort((a, b) => b[1].calls - a[1].calls)
      .map(([name, t]) => ({ name, ...t, error_rate: t.calls ? t.errors / t.calls : 0 })),
    worth_reading: reports
      .map((r) => ({ id: r.id, mothership: r.mothership, repo: r.repo, title: r.title, status: r.status, reasons: reasons(r, costThreshold) }))
      .filter((w) => w.reasons.length > 0)
      .sort((a, b) => b.reasons.length - a.reasons.length),
  };
}

// ---------------------------------------------------------------------------------------------- printing

export function redact(text) {
  return String(text)
    .replace(/sk-ant-[A-Za-z0-9_-]{8,}/g, 'sk-ant-…')
    .replace(/\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/g, 'gh…')
    .replace(/\bxox[abprs]-[A-Za-z0-9-]{10,}/g, 'xox…')
    .replace(/\bAKIA[0-9A-Z]{16}\b/g, 'AKIA…')
    .replace(/(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{12,}/gi, '$1 …')
    .replace(/((?:api[_-]?key|token|secret|password|passwd)["']?\s*[:=]\s*["']?)[^\s"',]{6,}/gi, '$1…');
}

function duration(msValue) {
  if (msValue == null || !Number.isFinite(msValue)) return '–';
  const s = Math.round(msValue / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`;
}

const pct = (x) => (x == null ? '–' : `${Math.round(x * 100)}%`);
const usd = (x) => (typeof x === 'number' ? `$${x.toFixed(2)}` : '–');

/** The finding chain counts worth naming in the findings sentence, nonzero terms only. */
function findingChain(summary) {
  const parts = [];
  if (summary.validated) parts.push(`${summary.validated} validated`);
  if (summary.rejected) parts.push(`${summary.rejected} rejected`);
  if (summary.fix_colonies) parts.push(`${summary.fix_colonies} fix${summary.fix_colonies === 1 ? '' : 'es'}`);
  if (summary.reviews) {
    parts.push(`${summary.reviews} review${summary.reviews === 1 ? '' : 's'}${summary.review_passed ? `, ${summary.review_passed} passed` : ''}`);
  }
  if (summary.merged) parts.push(`merged: ${summary.merged}`);
  return parts.length ? ` (${parts.join(', ')})` : '';
}

function table(headers, rows) {
  const line = (cells) => `| ${cells.join(' | ')} |`;
  return [line(headers), line(headers.map(() => '---')), ...rows.map((row) => line(row.map((c) => String(c ?? '–').replace(/\|/g, '\\|'))))].join('\n');
}

export function formatReport(summary, reports, worst = 10) {
  const out = [];
  out.push(`# Colony report`, '');
  out.push(`${summary.colonies} colonies from ${summary.motherships.join(', ') || 'no mothership'}.`, '');
  out.push(
    table(
      ['', 'median', 'p90'],
      [
        ['Claude cost (colony total)', usd(summary.cost_usd.median), usd(summary.cost_usd.p90)],
        ['Time the agent worked (answer waits excluded)', duration(summary.working_ms.median), duration(summary.working_ms.p90)],
        ['Wall-clock time', duration(summary.wall_ms.median), duration(summary.wall_ms.p90)],
        ['Boot', duration(summary.boot_ms.median), duration(summary.boot_ms.p90)],
        ['Turns', summary.turns.median, summary.turns.p90],
        ['Tool calls', summary.tool_calls.median, summary.tool_calls.p90],
        ['Questions', summary.questions.median, summary.questions.p90],
      ],
    ),
  );
  out.push('');
  out.push(`- Outcomes: ${Object.entries(summary.statuses).map(([s, n]) => `${s} ${n}`).join(', ')}. Pull requests from finished colonies: ${pct(summary.pr_rate)}.`);
  out.push(`- Claude cost in total: ${usd(summary.claude_cost_usd)}. Routed models (with a \`/\`) aren't priced; see the JSON for their tokens.`);
  out.push(`- Questions: ${summary.questions.total}, waiting ${duration(summary.answer_wait_ms.total)} in total for answers (longest ${duration(summary.answer_wait_ms.longest)}).`);
  out.push(`- Asked in plain text and re-prompted: ${summary.plain_text_reprompts}. Watchdog nudges: ${summary.watchdog_nudges}. Failed turns: ${summary.failed_turns}. Rate-limit hits: ${summary.rate_limit_hits}.`);
  out.push(`- Settlers sent out: ${summary.subagents}. Findings filed: ${summary.findings}${findingChain(summary)}. Memory proposals: ${summary.memory_proposals}.`);
  out.push('', '## Tools', '');
  out.push(table(['Tool', 'Calls', 'Failed', 'Failure rate'], summary.tools.slice(0, 15).map((t) => [t.name, t.calls, t.errors, pct(t.error_rate)])));
  out.push('', '## Colonies', '');
  out.push(
    table(
      ['Colony', 'Repo', 'Status', 'Cost', 'Worked', 'Turns', 'Tools (failed)', 'Questions', 'Longest silence'],
      [...reports]
        .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))
        .map((r) => [
          `${r.id}${summary.motherships.length > 1 ? ` (${r.mothership})` : ''}`,
          r.repo,
          r.status,
          usd(r.cost_usd),
          duration(r.working_ms),
          r.turns,
          `${r.tool_calls} (${r.tool_errors})`,
          r.questions,
          duration(r.longest_silence_ms),
        ]),
    ),
  );
  out.push('', '## Worth reading', '');
  if (summary.worth_reading.length === 0) out.push('Nothing stands out.');
  for (const w of summary.worth_reading.slice(0, worst)) {
    out.push(`- \`${w.id}\` ${w.repo ?? ''} ${w.title ? `“${w.title}”` : ''}: ${w.reasons.join('; ')}`);
  }
  if (summary.worth_reading.length > 0) out.push('', 'Read one with `node scripts/colony-report.mjs --transcript <id>`.');
  return out.join('\n');
}

function excerpt(text, max) {
  const clean = redact(String(text ?? '')).replace(/\s+/g, ' ').trim();
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

function describeCall(e) {
  const input = e.input ?? {};
  const detail = input.command ?? input.file_path ?? input.path ?? input.pattern ?? input.url ?? input.description ?? input.prompt ?? '';
  return `${e.name}${detail ? ` ${excerpt(detail, 140)}` : ''}`;
}

/** One colony, one line per step, with the time since it started and gaps worth noticing. */
export function formatTranscript({ session = {}, events = [], logs = [] }) {
  const all = [
    ...events.map((e) => ({ ...e, source: 'event' })),
    ...logs.map((l) => ({ ...l, source: 'harness' })),
  ].sort((a, b) => (ms(a.ts) || 0) - (ms(b.ts) || 0));
  const start = ms(all[0]?.ts);
  const calls = new Map();
  const asked = new Map();
  const lines = [`# ${session.id} ${session.repo ?? ''} ${session.issue ? `#${session.issue}` : ''} ${session.issue_title ?? ''}`.trim(), `status ${session.status ?? '?'}${session.pr_url ? ` · ${session.pr_url}` : ''}`, ''];
  let state = 'idle';
  let last = NaN;
  for (const e of all) {
    const t = ms(e.ts);
    if (Number.isFinite(t) && Number.isFinite(last) && t - last >= 60_000 && !QUIET_STATES.has(state)) {
      lines.push(`        ‖ ${duration(t - last)} without an event while ${state}`);
    }
    if (Number.isFinite(t)) last = t;
    const at = Number.isFinite(t) && Number.isFinite(start) ? `+${duration(t - start)}`.padEnd(8) : '        ';
    const who = e.agent ? `[${e.agent.name}] ` : '';
    switch (e.type) {
      case 'status':
        state = e.state ?? state;
        break;
      case 'user_message':
        if (e.id === 'initial') lines.push(`${at}brief: ${excerpt(e.text, 300)}`);
        else if (String(e.id).startsWith('watchdog-')) lines.push(`${at}⚑ watchdog nudge`);
        else lines.push(`${at}you: ${excerpt(e.text, 300)}`);
        break;
      case 'assistant_text':
        if (String(e.text ?? '').trim()) lines.push(`${at}${who}says: ${excerpt(e.text, 400)}`);
        break;
      case 'tool_call':
        calls.set(e.tool_call_id, e);
        lines.push(`${at}${who}→ ${describeCall(e)}`);
        break;
      case 'tool_result':
        if (e.is_error) {
          const call = calls.get(e.tool_call_id);
          lines.push(`${at}${who}✗ ${call?.name ?? 'tool'} failed: ${excerpt(e.output, 240)}`);
        }
        break;
      case 'question':
        asked.set(e.question_id, t);
        for (const q of e.questions ?? []) {
          lines.push(`${at}? ${excerpt(q.question, 200)} [${(q.options ?? []).map((o) => o.label).join(' | ')}]`);
        }
        break;
      case 'question_answered': {
        const wait = Number.isFinite(asked.get(e.question_id)) ? ` (after ${duration(t - asked.get(e.question_id))})` : '';
        lines.push(`${at}✓ answered${wait}: ${excerpt(Object.values(e.answers ?? {}).flat().join('; ') || e.response, 200)}`);
        break;
      }
      case 'turn_end':
        lines.push(`${at}■ turn ${e.is_error ? 'FAILED' : 'ended'} after ${duration(e.duration_ms)}, ${usd(e.cost_usd)} so far${e.is_error && e.result ? `: ${excerpt(e.result, 200)}` : ''}`);
        break;
      case 'log':
      case 'harness_log':
        if (e.level !== 'info' || PLAIN_TEXT_REPROMPT.test(e.message ?? '') || /watchdog/i.test(e.message ?? '')) {
          lines.push(`${at}${e.level === 'info' ? 'ℹ' : '!'} ${e.source === 'harness' ? 'mothership' : 'runner'} ${e.level}: ${excerpt(e.message, 240)}`);
        }
        break;
      case 'finding':
        lines.push(`${at}◆ finding: ${excerpt(e.title, 160)}`);
        break;
      case 'validated':
        lines.push(`${at}✓ validated: ${excerpt(e.title, 160)}${e.severity ? ` (${e.severity})` : ''}`);
        break;
      case 'rejected':
        lines.push(`${at}✗ rejected: ${excerpt(e.title, 160)} — ${excerpt(e.reason, 120)}`);
        break;
      case 'fix_colony':
        lines.push(`${at}⚒ fix colony ${e.session} for: ${excerpt(e.title, 160)}`);
        break;
      case 'review':
        lines.push(`${at}⚖ review ${e.session} of ${excerpt(e.pr, 120)}: ${e.verdict}`);
        break;
      case 'merged':
        lines.push(`${at}✔ merged ${excerpt(e.pr, 120)}`);
        break;
      case 'memory_proposal':
        lines.push(`${at}◇ memory proposal (${e.scope}): ${excerpt(e.title, 160)}`);
        break;
    }
  }
  return lines.join('\n');
}

// ---------------------------------------------------------------------------------------------- command

function parseArgs(argv) {
  const args = { data: [], json: false, worst: 10, since: null, repo: null, transcript: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (a === '--data') args.data.push(value());
    else if (a === '--json') args.json = true;
    else if (a === '--worst') args.worst = Number(value());
    else if (a === '--since') args.since = value();
    else if (a === '--repo') args.repo = value();
    else if (a === '--transcript') args.transcript = value();
    else if (a === '-h' || a === '--help') args.help = true;
    else throw new Error(`unknown argument ${a}`);
  }
  if (args.data.length === 0) {
    args.data.push(process.env.COLONIZER_DATA_DIR || join(homedir(), '.local/share/colonizer'));
    args.here = true;
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    console.log(readFileSync(new URL(import.meta.url), 'utf8').split('\n').filter((l) => l.startsWith('//')).map((l) => l.slice(3)).join('\n'));
    return;
  }
  let colonies = args.data.flatMap((dir) => {
    if (!existsSync(dir)) throw new Error(`no data directory at ${dir}`);
    // A copied data dir is named after its directory; the one this machine uses needs no name.
    return loadColonies(dir, args.here ? 'this mothership' : undefined);
  });
  if (args.transcript) {
    const matches = colonies.filter((c) => String(c.session.id).startsWith(args.transcript));
    if (matches.length !== 1) throw new Error(matches.length ? `${args.transcript} matches ${matches.length} colonies` : `no colony ${args.transcript}`);
    console.log(formatTranscript(matches[0]));
    return;
  }
  if (args.repo) colonies = colonies.filter((c) => c.session.repo === args.repo);
  if (args.since) {
    const since = Date.parse(args.since);
    colonies = colonies.filter((c) => ms(c.session.created_at ?? c.events[0]?.ts) >= since);
  }
  const reports = colonies.map(analyze);
  const summary = summarize(reports);
  console.log(args.json ? JSON.stringify({ summary, colonies: reports }, null, 2) : formatReport(summary, reports, args.worst));
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try {
    main();
  } catch (e) {
    console.error(`colony-report: ${e.message}`);
    process.exit(1);
  }
}
