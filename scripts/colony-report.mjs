#!/usr/bin/env node
// How colonies actually went, from what the mothership already records: every colony's events.jsonl,
// harness.jsonl and gateway.jsonl, and sessions.json. Nothing leaves the machine. Use it to find the colonies worth reading
// before changing a prompt or a module setting, and to compare a fixed set of tasks before and after.
//
//   node scripts/colony-report.mjs                        # this mothership (COLONIZER_DATA_DIR or ~/.local/share/colonizer)
//   node scripts/colony-report.mjs --data ./omarchy-data  # a copy of another mothership's data dir; repeatable
//   node scripts/colony-report.mjs --since 2026-09-01 --repo owner/name --worst 10
//   node scripts/colony-report.mjs --json > report.json
//   node scripts/colony-report.mjs --transcript fc742075  # one colony, step by step, secrets redacted
//   node scripts/colony-report.mjs --transcript fc742075 --origin autonomy,watchdog  # only those origins' lines
//   node scripts/colony-report.mjs --costs                # spend.jsonl by colony: estimated vs metered, harness × model
//   node scripts/colony-report.mjs --costs --since 2026-09-01 --days 90   # move the window (default: the spend history's last 30 days)
//
// Only the metadata of tool calls is summarised (names, file paths, commands). Transcripts print short
// excerpts of text and failed output, with common credential patterns redacted, but read them before sharing.
//
// Cost reads two channels (issue #296): `estimated` is the agent's own per-turn figure, `metered` is what
// the gateway priced for a routed provider, and a colony whose provider has no prices shows its tokens as
// `unpriced — tokens only`, never as $0. Rows the journal carries without a session — older rows, cockpit
// chat — report under `unattributed`, so the totals always sum to the whole window.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

// ---------------------------------------------------------------------------------------------- reading

export function readJsonLines(path) {
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
    const parsed = JSON.parse(readFileSync(join(dataDir, 'sessions.json'), 'utf8'));
    // Wrong-shaped JSON costs the session list no less than a parse error would; the session
    // directories are still read below, so a damaged file loses metadata, not colonies.
    if (Array.isArray(parsed)) records = parsed;
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
    gateway: readJsonLines(join(sessionsDir, id, 'gateway.jsonl')),
  }));
}

// ------------------------------------------------------------------------------------------- analysing

const ms = (ts) => (ts ? Date.parse(ts) : NaN);
const RATE_LIMIT = /\b429\b|rate[ _-]?limit|overloaded|too many requests|quota/i;
const PLAIN_TEXT_REPROMPT = /asked in plain text/i;
const QUIET_STATES = new Set(['waiting_for_answer', 'idle', 'exited', 'error']);

/** Claude's estimate plus the gateway's routed dollars; null only when neither was measured. */
export function totalCost(claude, routed) {
  if (typeof claude !== 'number' && typeof routed !== 'number') return null;
  return (typeof claude === 'number' ? claude : 0) + (typeof routed === 'number' ? routed : 0);
}

/** Where a colony's tokens went (issue #469). Every token delta lands in exactly one of these. */
export const TOKEN_CATEGORIES = ['read', 'search', 'command_output', 'edit', 'reasoning', 'replay'];
const emptyCategories = () => Object.fromEntries(TOKEN_CATEGORIES.map((c) => [c, 0]));
const emptyCategoriesByModel = () => Object.fromEntries(TOKEN_CATEGORIES.map((c) => [c, {}]));

/** Tool name → the category its turn bills to. Anything absent — Task, Skill, WebFetch, mcp__*,
 *  unknown names — is left out, and a turn with no mapped call at all bills as reasoning. */
const TOOL_CATEGORY = {
  Read: 'read',
  NotebookRead: 'read',
  Grep: 'search',
  Glob: 'search',
  LS: 'search',
  WebSearch: 'search',
  Bash: 'command_output',
  BashOutput: 'command_output',
  KillShell: 'command_output',
  Edit: 'edit',
  Write: 'edit',
  MultiEdit: 'edit',
  NotebookEdit: 'edit',
};
// A turn bills whole to the most consequential thing it did, so a turn that read files and then
// edited them is edit, not read.
const CATEGORY_PRECEDENCE = ['edit', 'command_output', 'search', 'read', 'reasoning'];

/** The {model: counts} of a cumulative `model_usage` doc, read the way spend.rs's `model_tokens`
 *  does: a malformed entry or a missing or negative count is zero, never an error. */
function modelTokens(usage) {
  const out = [];
  if (!usage || typeof usage !== 'object' || Array.isArray(usage)) return out;
  for (const [model, counts] of Object.entries(usage)) {
    if (!model || !counts || typeof counts !== 'object') continue;
    const n = (k) => (Number.isInteger(counts[k]) && counts[k] > 0 ? counts[k] : 0);
    out.push([model, { input: n('input_tokens'), output: n('output_tokens'), cache_read: n('cache_read_tokens'), cache_write: n('cache_write_tokens') }]);
  }
  return out;
}

/** One colony's numbers. Pure: takes what loadColonies read, so it can be tested without a mothership. */
export function analyze({ mothership = '', session = {}, events = [], logs = [], gateway = [] }) {
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
    // The agent module the colony ran on — the harness half of harness × model (issue #296).
    agent: session.agent || null,
    created_at: session.created_at ?? events[0]?.ts ?? null,
    boot_ms: session.boot_timing?.total_ms ?? null,
    wall_ms: 0,
    working_ms: 0,
    turns: 0,
    failed_turns: 0,
    // Claude Code's own estimate, over the Claude models only; turn_end events refine it below.
    cost_usd: session.cost_usd ?? null,
    // What the gateway priced for the providers it routed to. Only the session record carries it.
    routed_cost_usd: session.routed_cost_usd ?? null,
    total_cost_usd: null,
    model_usage: null,
    tokenCategories: emptyCategories(),
    tokenCategoriesByModel: emptyCategoriesByModel(),
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
    // The gateway's per-request audit lines (issue #302): how many went out, and how many ended in
    // each typed failure.
    gateway_requests: 0,
    gateway_failures: {},
    errors_logged: 0,
    longest_silence_ms: 0,
    findings: 0,
    validated: 0,
    rejected: 0,
    fix_colonies: 0,
    reviews: 0,
    review_passed: 0,
    merged: 0,
    verifications: 0,
    contradicted_claims: 0,
    // The last verification's verdict and wall time; a colony has at most one per publish attempt.
    verification_verdict: null,
    verification_ms: null,
    memory_proposals: 0,
    repeated_reads: [],
    repeated_commands: [],
    tools,
  };

  const times = events.map((e) => ms(e.ts)).filter(Number.isFinite);
  // Folded by hand rather than spread into Math.max/min: a long colony's events would overflow
  // the argument limit and kill the whole report with a RangeError.
  if (times.length > 1) {
    let min = times[0];
    let max = times[0];
    for (const t of times) {
      if (t < min) min = t;
      if (t > max) max = t;
    }
    r.wall_ms = max - min;
  }

  let state = 'idle';
  let lastTs = NaN;
  // One turn's state, shared by the colony and its subagents: model_usage arrives only on the
  // colony's own turn_end and is cumulative for everything the colony did, subagents included, so
  // a subagent's tool calls bill to the same turn as the orchestrator's.
  const turn = { categories: new Set(), usage: new Map() };
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
        const category = TOOL_CATEGORY[name];
        if (category) turn.categories.add(category);
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
        const usage = e.model_usage && typeof e.model_usage === 'object' && !Array.isArray(e.model_usage) ? e.model_usage : null;
        if (usage) r.model_usage = usage;
        // model_usage is cumulative for the whole colony, subagents' work included, and optional on
        // turn_end (docs/protocol.md §2). A turn without it has nothing to attribute, and wiping the
        // baseline here would make the next measured turn re-count everything before the gap — so
        // the baseline survives, the way events.rs only overwrites the record when usage is an object.
        if (usage) {
          // This turn's own spend is the difference against the last cumulative, floored at zero
          // exactly like spend.rs's turn_deltas: a cheaper re-estimate must not write a negative line.
          const counts = modelTokens(usage);
          let workTokens = 0;
          let replayTokens = 0;
          const workByModel = new Map();
          const cacheByModel = new Map();
          for (const [model, now] of counts) {
            const before = turn.usage.get(model);
            const input = Math.max(0, now.input - (before?.input ?? 0));
            const output = Math.max(0, now.output - (before?.output ?? 0));
            const cacheRead = Math.max(0, now.cache_read - (before?.cache_read ?? 0));
            const cacheWrite = Math.max(0, now.cache_write - (before?.cache_write ?? 0));
            if (input + output > 0) workByModel.set(model, input + output);
            if (cacheRead + cacheWrite > 0) cacheByModel.set(model, cacheRead + cacheWrite);
            workTokens += input + output;
            replayTokens += cacheRead + cacheWrite;
          }
          const turnCategory = CATEGORY_PRECEDENCE.find((c) => turn.categories.has(c)) ?? 'reasoning';
          r.tokenCategories[turnCategory] += workTokens;
          r.tokenCategories.replay += replayTokens;
          for (const [model, n] of workByModel) r.tokenCategoriesByModel[turnCategory][model] = (r.tokenCategoriesByModel[turnCategory][model] ?? 0) + n;
          for (const [model, n] of cacheByModel) r.tokenCategoriesByModel.replay[model] = (r.tokenCategoriesByModel.replay[model] ?? 0) + n;
          turn.categories = new Set();
          turn.usage = new Map(counts);
        }
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
      case 'verification':
        r.verifications += 1;
        if (e.verdict === 'contradicted') r.contradicted_claims += 1;
        r.verification_verdict = e.verdict ?? null;
        r.verification_ms = typeof e.ms === 'number' ? e.ms : null;
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
  // Counted, never printed whole: an audit line carries the request it served, secrets included.
  for (const g of gateway) {
    if (g?.type !== 'gateway_request') continue;
    r.gateway_requests += 1;
    if (g.failure) r.gateway_failures[g.failure] = (r.gateway_failures[g.failure] ?? 0) + 1;
  }

  // A turn's duration includes the time it waited for an answer, which is the user's time, not the agent's.
  r.working_ms = Math.max(0, r.working_ms - r.answer_wait_ms);
  r.total_cost_usd = totalCost(r.cost_usd, r.routed_cost_usd);
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
  if (r.contradicted_claims > 0) out.push(`${r.contradicted_claims} contradicted completion claim${r.contradicted_claims === 1 ? '' : 's'}`);
  if (r.rate_limit_hits > 0) out.push(`${r.rate_limit_hits} rate-limit hit${r.rate_limit_hits === 1 ? '' : 's'}`);
  const gatewayFailures = Object.values(r.gateway_failures ?? {}).reduce((a, n) => a + n, 0);
  if (gatewayFailures > 0) out.push(`${gatewayFailures} gateway request${gatewayFailures === 1 ? '' : 's'} failed`);
  if (r.longest_silence_ms >= 5 * 60_000) out.push(`silent ${duration(r.longest_silence_ms)} while working`);
  if (r.tool_calls >= 10 && r.tool_errors / r.tool_calls >= 0.2) out.push(`${pct(r.tool_errors / r.tool_calls)} of tool calls failed`);
  if (r.plain_text_reprompts > 0) out.push(`asked in plain text ${r.plain_text_reprompts}×`);
  if (r.questions >= 3) out.push(`${r.questions} questions`);
  if (r.repeated_reads.length > 0) out.push(`re-read ${r.repeated_reads[0].path.split('/').pop()} ${r.repeated_reads[0].n}×`);
  if (r.repeated_commands.length > 0) out.push(`ran the same command ${r.repeated_commands[0].n}×`);
  if (typeof r.total_cost_usd === 'number' && r.total_cost_usd >= costThreshold) out.push(`cost $${r.total_cost_usd.toFixed(2)} (top quarter)`);
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
  const costThreshold = quantile(reports.map((r) => r.total_cost_usd), 0.75) ?? Infinity;
  const tokenCategories = emptyCategories();
  const tokenCategoriesByModel = emptyCategoriesByModel();
  for (const r of reports) {
    for (const c of TOKEN_CATEGORIES) {
      tokenCategories[c] += r.tokenCategories?.[c] ?? 0;
      for (const [model, n] of Object.entries(r.tokenCategoriesByModel?.[c] ?? {})) tokenCategoriesByModel[c][model] = (tokenCategoriesByModel[c][model] ?? 0) + n;
    }
  }
  return {
    colonies: reports.length,
    motherships: [...new Set(reports.map((r) => r.mothership))],
    statuses,
    pr_rate: finished.length ? reports.filter((r) => r.pr_url || r.status === 'pr_opened').length / finished.length : null,
    claude_cost_usd: sum('cost_usd'),
    routed_cost_usd: sum('routed_cost_usd'),
    cost_usd: stat('cost_usd'),
    total_cost_usd: stat('total_cost_usd'),
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
    verifications: sum('verifications'),
    contradicted_claims: sum('contradicted_claims'),
    memory_proposals: sum('memory_proposals'),
    subagents: sum('subagents'),
    tokenCategories,
    tokenCategoriesByModel,
    tools: Object.entries(tools)
      .sort((a, b) => b[1].calls - a[1].calls)
      .map(([name, t]) => ({ name, ...t, error_rate: t.calls ? t.errors / t.calls : 0 })),
    worth_reading: reports
      .map((r) => ({ id: r.id, mothership: r.mothership, repo: r.repo, title: r.title, status: r.status, reasons: reasons(r, costThreshold) }))
      .filter((w) => w.reasons.length > 0)
      .sort((a, b) => b.reasons.length - a.reasons.length),
  };
}

// ----------------------------------------------------------------------------------------- spend journal

const nonneg = (n) => (Number.isInteger(n) && n > 0 ? n : 0);
const SPEND_TOKENS = ['input', 'output', 'cache_read', 'cache_write'];

/** A journal row's four token classes, read the way spend.rs reads a `usage` row: a count that is
 *  absent or absurd is zero, never an error. */
const rowTokens = (row) => Object.fromEntries(SPEND_TOKENS.map((k) => [k, nonneg(row[`${k}_tokens`])]));

/** `<data>/spend.jsonl`, keeping what the server's reader keeps (crates/colonizer/src/spend.rs
 *  `read_journal`): a torn line, a row without a day or an org, and — serde drops a row whole when
 *  a field has a type it refuses — a row whose token counts are not non-negative integers, whose
 *  cost is not a number, or whose day, org, session, agent or model is not a string. No file means
 *  no spend, not a failure. */
const optionalName = (v) => v == null || typeof v === 'string';
// Absent counts are the container default's zero; an explicit null, fraction or negative is a type
// serde's u64 refuses, and the row drops whole with it.
const tokenCount = (v) => v === undefined || (typeof v === 'number' && Number.isInteger(v) && v >= 0);
export function loadSpend(dataDir) {
  return readJsonLines(join(dataDir, 'spend.jsonl')).filter(
    (r) =>
      r &&
      typeof r.day === 'string' && r.day && typeof r.org === 'string' && r.org &&
      tokenCount(r.input_tokens) && tokenCount(r.output_tokens) && tokenCount(r.cache_read_tokens) && tokenCount(r.cache_write_tokens) &&
      (r.cost_usd == null || typeof r.cost_usd === 'number') &&
      optionalName(r.session) && optionalName(r.agent) && optionalName(r.model),
  );
}

const emptySpend = (meta) => ({
  session: null, agent: null, mothership: null, repo: null, issue: null, status: null,
  ...meta,
  models: {},
  ...Object.fromEntries(SPEND_TOKENS.map((k) => [`${k}_tokens`, 0])),
  estimated_usd: null,
  metered_usd: null,
  launched: 0,
  returned: 0,
});

/** One row into one bucket, by spend.rs's `aggregate`: a `usage` row carries the four token classes
 *  and, measured, one estimated dollar — on its model's row when the turn ran one model, on its own
 *  un-modeled row otherwise, so a model's cost is only what rode that model's rows. A `routed` row is
 *  the gateway's metered dollar and never names a model. `launched`/`returned` only count, and a kind
 *  a newer build added is ignored rather than fatal. */
function addSpendRow(bucket, row) {
  if (row.kind !== 'usage') {
    if (row.kind === 'routed' && typeof row.cost_usd === 'number') bucket.metered_usd = (bucket.metered_usd ?? 0) + row.cost_usd;
    else if (row.kind === 'launched') bucket.launched += 1;
    else if (row.kind === 'returned') bucket.returned += 1;
    return;
  }
  const t = rowTokens(row);
  for (const k of SPEND_TOKENS) bucket[`${k}_tokens`] += t[k];
  if (typeof row.cost_usd === 'number') bucket.estimated_usd = (bucket.estimated_usd ?? 0) + row.cost_usd;
  if (!row.model) return;
  const m = (bucket.models[row.model] ??= { input: 0, output: 0, cache_read: 0, cache_write: 0, tokens: 0, cost_usd: null });
  let sum = 0;
  for (const k of SPEND_TOKENS) {
    m[k] += t[k];
    sum += t[k];
  }
  m.tokens += sum;
  if (typeof row.cost_usd === 'number') m.cost_usd = (m.cost_usd ?? 0) + row.cost_usd;
}

/** A bucket's derived shape: tokens in one count, the total by the web's rule (unmeasured stays null,
 *  web/src/spend.ts), the unpriced label, and the models largest first. */
function finishSpend(bucket) {
  const tokens = SPEND_TOKENS.reduce((a, k) => a + bucket[`${k}_tokens`], 0);
  const priced = bucket.estimated_usd != null || bucket.metered_usd != null;
  return {
    ...bucket,
    tokens,
    priced,
    total_usd: totalCost(bucket.estimated_usd, bucket.metered_usd),
    label: !priced && tokens > 0 ? 'unpriced — tokens only' : null,
    models: Object.entries(bucket.models)
      .map(([model, m]) => ({ model, ...m }))
      .sort((a, b) => b.tokens - a.tokens || a.model.localeCompare(b.model)),
  };
}

/** The window `--costs` sums, the same one `GET /api/spend/history` answers by default
 *  (crates/colonizer/src/spend.rs `day_window`): `days` days ending today UTC, counting back from
 *  `today - (days - 1)`, so the default 30 spans `today - 29 ..= today`, clamped to 365 like the
 *  endpoint. A `--since` value overrides the floor; the today ceiling stays, so future-dated rows
 *  drop either way. An unparseable `since` keeps the meaning it always had here: nothing. */
const todayUtc = () => new Date().toISOString().slice(0, 10);

export function spendWindow({ since = null, days = 30, today = todayUtc() } = {}) {
  const end = today || todayUtc();
  const endMs = Date.parse(`${end}T00:00:00Z`);
  const n = days == null || days === '' ? NaN : Number(days);
  const count = Number.isFinite(n) ? Math.min(365, Math.max(1, Math.trunc(n))) : 30;
  const sinceMs = since ? Date.parse(since) : NaN;
  const floor = Number.isFinite(sinceMs)
    ? new Date(sinceMs).toISOString().slice(0, 10)
    : since
      ? '9999-12-31' // beyond the ceiling, so an unparseable --since answers nothing, as before
      : Number.isFinite(endMs)
        ? new Date(endMs - (count - 1) * 86_400_000).toISOString().slice(0, 10)
        : end;
  return { floor, today: end };
}

/** What `--costs` answers (issue #296): the journal of every --data dir, grouped by the colony that
 *  spent it — its harness (`agent`) with it — with repo, issue and status resolved through
 *  sessions.json when the colony is known there. Rows without a `session` (older rows, cockpit chat)
 *  land in `unattributed`, so the totals always sum to the whole window. `--repo` keeps only rows
 *  whose colony sessions.json names with that repo; rows without a colony have neither. A `window`
 *  (`spendWindow`'s shape) keeps only rows whose `day` is inside it, and is echoed on the report so
 *  the two views can be told apart; without one, every row counts. Colonies rank by spend, and the
 *  same rows roll up once more by harness × model, the question the per-colony table cannot answer. */
export function costsReport(rows, colonies = [], { repo = null, window = null } = {}) {
  if (window) rows = rows.filter((r) => r.day >= window.floor && r.day <= window.today);
  const known = new Map();
  for (const c of colonies) if (!known.has(c.session.id)) known.set(c.session.id, c);
  const buckets = new Map();
  const harness = new Map();
  const bucketOf = (id, agent) => {
    let bucket = buckets.get(id);
    if (!bucket) {
      const c = id != null ? known.get(id) : null;
      bucket = emptySpend({
        session: id,
        agent,
        mothership: c?.mothership ?? null,
        repo: c?.session.repo ?? null,
        issue: c?.session.issue ?? null,
        status: c?.session.status ?? null,
      });
      buckets.set(id, bucket);
    }
    return bucket;
  };
  for (const row of rows) {
    const id = typeof row.session === 'string' && row.session ? row.session : null;
    const meta = id != null ? known.get(id) : null;
    if (repo && meta?.session.repo !== repo) continue;
    const agent = row.agent || meta?.session.agent || null;
    addSpendRow(bucketOf(id, agent), row);
    if (id == null || agent == null || !row.model) continue;
    const key = `${agent}|${row.model}`;
    let h = harness.get(key);
    if (!h) harness.set(key, (h = { agent, model: row.model, colonies: new Set(), ...Object.fromEntries(SPEND_TOKENS.map((k) => [`${k}_tokens`, 0])), tokens: 0, cost_usd: null }));
    h.colonies.add(id);
    const t = rowTokens(row);
    let sum = 0;
    for (const k of SPEND_TOKENS) {
      h[`${k}_tokens`] += t[k];
      sum += t[k];
    }
    h.tokens += sum;
    if (row.kind === 'usage' && typeof row.cost_usd === 'number') h.cost_usd = (h.cost_usd ?? 0) + row.cost_usd;
  }
  const dollars = (pick) => {
    let total = null;
    for (const bucket of buckets.values()) if (pick(bucket) != null) total = (total ?? 0) + pick(bucket);
    return total;
  };
  const counts = (pick) => [...buckets.values()].reduce((a, b) => a + pick(b), 0);
  const totals = {
    estimated_usd: dollars((b) => b.estimated_usd),
    metered_usd: dollars((b) => b.metered_usd),
    ...Object.fromEntries(SPEND_TOKENS.map((k) => [`${k}_tokens`, counts((b) => b[`${k}_tokens`])])),
  };
  totals.tokens = SPEND_TOKENS.reduce((a, k) => a + totals[`${k}_tokens`], 0);
  totals.total_usd = totalCost(totals.estimated_usd, totals.metered_usd);
  return {
    window,
    colonies: [...buckets.values()].filter((b) => b.session != null).map(finishSpend).sort((a, b) => (b.total_usd ?? -Infinity) - (a.total_usd ?? -Infinity)),
    unattributed: finishSpend(buckets.get(null) ?? emptySpend({})),
    totals,
    harness_model: [...harness.values()]
      .map((h) => ({ ...h, colonies: h.colonies.size }))
      .sort((a, b) => b.tokens - a.tokens || a.agent.localeCompare(b.agent) || a.model.localeCompare(b.model)),
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
        ['Cost (colony total, Claude and routed)', usd(summary.total_cost_usd.median), usd(summary.total_cost_usd.p90)],
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
  out.push(
    `- Cost in total: ${usd(summary.claude_cost_usd + summary.routed_cost_usd)}: Claude ${usd(summary.claude_cost_usd)} (Claude Code's own estimate), routed ${usd(summary.routed_cost_usd)} (priced by the gateway; a provider without pricing counts tokens only, see the JSON).`,
  );
  out.push(`- Questions: ${summary.questions.total}, waiting ${duration(summary.answer_wait_ms.total)} in total for answers (longest ${duration(summary.answer_wait_ms.longest)}).`);
  out.push(`- Asked in plain text and re-prompted: ${summary.plain_text_reprompts}. Watchdog nudges: ${summary.watchdog_nudges}. Failed turns: ${summary.failed_turns}. Rate-limit hits: ${summary.rate_limit_hits}.`);
  out.push(`- Settlers sent out: ${summary.subagents}. Findings filed: ${summary.findings}${findingChain(summary)}. Memory proposals: ${summary.memory_proposals}. Verifications: ${summary.verifications}${summary.contradicted_claims ? `, ${summary.contradicted_claims} contradicted` : ''}.`);
  out.push('', '## Tools', '');
  out.push(table(['Tool', 'Calls', 'Failed', 'Failure rate'], summary.tools.slice(0, 15).map((t) => [t.name, t.calls, t.errors, pct(t.error_rate)])));
  out.push('', '## Token categories', '');
  const tokenTotal = TOKEN_CATEGORIES.reduce((a, c) => a + summary.tokenCategories[c], 0);
  out.push(
    table(
      ['Category', 'Tokens', '% of total'],
      TOKEN_CATEGORIES.map((c) => [c, summary.tokenCategories[c], tokenTotal ? pct(summary.tokenCategories[c] / tokenTotal) : '–']),
    ),
  );
  out.push('', '## Colonies', '');
  out.push(
    table(
      ['Colony', 'Repo', 'Status', 'Cost', 'Routed', 'Worked', 'Turns', 'Tools (failed)', 'Questions', 'Longest silence', 'Agent'],
      [...reports]
        .sort((a, b) => String(b.created_at).localeCompare(String(a.created_at)))
        .map((r) => [
          `${r.id}${summary.motherships.length > 1 ? ` (${r.mothership})` : ''}`,
          r.repo,
          r.status,
          usd(r.total_cost_usd),
          usd(r.routed_cost_usd),
          duration(r.working_ms),
          r.turns,
          `${r.tool_calls} (${r.tool_errors})`,
          r.questions,
          duration(r.longest_silence_ms),
          r.agent,
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

/** `--costs` for people: colonies ranked by spend (the unattributed rest beneath them), the grand
 *  total split by which channel measured it, then the harness × model rollup. */
export function formatCosts(costs) {
  const u = costs.unattributed;
  const rows = u.tokens > 0 || u.estimated_usd != null || u.metered_usd != null ? [...costs.colonies, u] : costs.colonies;
  const t = costs.totals;
  return [
    '# Spend',
    '',
    table(
      ['Colony', 'Agent', 'Repo', 'Status', 'Tokens', 'Estimated', 'Metered', 'Total'],
      rows.map((c) => [c.session ?? 'unattributed', c.agent, c.repo, c.status, c.tokens, usd(c.estimated_usd), usd(c.metered_usd), c.label ?? usd(c.total_usd)]),
    ),
    '',
    `- Total ${usd(t.total_usd)}${costs.window ? ` over ${costs.window.floor}..${costs.window.today}` : ''}: estimated ${usd(t.estimated_usd)} (the agent's own per-turn estimate), metered ${usd(t.metered_usd)} (priced by the gateway). A – is unmeasured, not $0.`,
    '',
    '## Harness × model',
    '',
    table(['Agent', 'Model', 'Colonies', 'Tokens', 'Cost'], costs.harness_model.map((h) => [h.agent, h.model, h.colonies, h.tokens, usd(h.cost_usd)])),
  ].join('\n');
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

/** One verification host event as a line: the verdict, what it found, and how long the check took. */
function verificationLine(e) {
  if (e.by_declaration) return 'verification: unverifiable by declaration (verify: none)';
  const verdict = String(e.verdict ?? 'unverifiable').toUpperCase();
  const detail = [excerpt(e.summary, 200), ...(e.contradictions ?? []).map((c) => excerpt(c, 160))].filter(Boolean).join('; ');
  // Counts come from the event's fields, and only when the claim held up: next to why a claim failed,
  // its numbers are not worth printing.
  const counts = [];
  if (e.verdict !== 'contradicted') {
    const files = e.files_changed?.length ?? 0;
    if (files > 0) counts.push(`${files} file${files === 1 ? '' : 's'}`);
    if (e.commits > 0) counts.push(`${e.commits} commit${e.commits === 1 ? '' : 's'}`);
  }
  const took = typeof e.ms === 'number' && Number.isFinite(e.ms) ? (e.ms < 60_000 ? ` (${(e.ms / 1000).toFixed(1)}s)` : ` (${duration(e.ms)})`) : '';
  return `verification: ${verdict}${detail ? ` — ${detail}` : ''}${counts.length ? `, ${counts.join(', ')}` : ''}${took}`;
}

// The envelope `origin` (issue #312) every recorded line carries, and the tag a transcript shows it
// as. Lines whose words already say who spoke — you:, brief:, the agent's own steps, a settler's
// [name] — get no tag; lines from before the field existed fall back to what the reader can tell.
export const ORIGINS = ['user', 'agent', 'subagent', 'watchdog', 'autonomy', 'burn_down', 'redteam', 'notify', 'system'];
const ORIGIN_TAG = { autonomy: '[judge]', burn_down: '[burn-down]', redteam: '[red-team]', notify: '[notify]', system: '[system]' };

/** A line's origin: the envelope's field, or the inference a reader makes without it. */
export function originOf(e) {
  if (ORIGINS.includes(e.origin)) return e.origin;
  if (e.type === 'user_message') return String(e.id ?? '').startsWith('watchdog-') ? 'watchdog' : 'user';
  if (e.type === 'question_answered') return 'user';
  if (e.type === 'harness_log' || e.type === 'verification') return 'system';
  return e.agent ? 'subagent' : 'agent';
}

/** `--origin`'s comma-separated list as a filter, refusing anything outside the closed set. */
export function parseOrigins(value) {
  const origins = new Set(String(value).split(',').map((o) => o.trim()).filter(Boolean));
  const unknown = [...origins].filter((o) => !ORIGINS.includes(o));
  if (unknown.length > 0) throw new Error(`unknown origin ${unknown.join(', ')} (one of ${ORIGINS.join(', ')})`);
  if (origins.size === 0) throw new Error(`--origin needs a comma-separated list of ${ORIGINS.join(', ')}`);
  return origins;
}

/** One screening host event as a line per finding: where, which class, how severe, what decoded. */
function screeningLines(e) {
  const findings = Array.isArray(e.findings) ? e.findings : [];
  const head = `screening: ${e.outcome ?? 'clean'} (${e.mode ?? 'off'})`;
  if (findings.length === 0) return [head];
  return [
    `${head} — ${findings.length} finding${findings.length === 1 ? '' : 's'}`,
    ...findings.map((f) => {
      const decoded = f.decoded ? ` — decoded ${JSON.stringify(f.decoded)}` : '';
      return `  ${f.location ?? '?'} — ${f.class ?? '?'} (${f.severity ?? 'medium'})${decoded}`;
    }),
  ];
}

/** One gateway audit record as a line, built only from its allowlisted fields: the record also
 *  carries what it served (keys, request bodies), which must never reach the transcript. */
function gatewayLine(e) {
  const model = e.model && e.wire_model && e.model !== e.wire_model ? `${e.model}→${e.wire_model}` : (e.wire_model ?? e.model ?? '–');
  const parts = [
    e.provider ?? 'unknown',
    model,
    `${e.method ?? 'POST'} ${e.path ?? '–'}`,
    `${e.status ?? 0}`,
    `${Number(e.duration_ms) || 0}ms`,
    `q${Number(e.queue_ms) || 0}ms`,
    `${Number(e.request_bytes) || 0}B→${Number(e.response_bytes) || 0}B`,
  ];
  if (e.failure) parts.push(`failure ${e.failure}`);
  if (e.fallback) parts.push('fallback');
  return `~ gateway ${parts.join(' ')}`;
}

/** One colony, one line per step, with the time since it started and gaps worth noticing. */
export function formatTranscript({ session = {}, events = [], logs = [], gateway = [], origins = null }) {
  const all = [
    ...events.map((e) => ({ ...e, source: 'event' })),
    ...logs.map((l) => ({ ...l, source: 'harness' })),
    ...gateway.map((g) => ({ ...g, source: 'gateway' })),
  ]
    .filter((e) => !origins || origins.has(originOf(e)))
    .sort((a, b) => (ms(a.ts) || 0) - (ms(b.ts) || 0));
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
    const origin = originOf(e);
    // Only the envelope's field tags a line: the words below already carry who spoke, and a legacy
    // line's inference picks the words, not a tag.
    const tag = e.origin && ORIGIN_TAG[origin] ? `${ORIGIN_TAG[origin]} ` : '';
    switch (e.type) {
      case 'status':
        state = e.state ?? state;
        break;
      case 'user_message':
        if (origin === 'watchdog') lines.push(`${at}⚑ watchdog nudge`);
        else if (e.id === 'initial') lines.push(`${at}${tag}brief: ${excerpt(e.text, 300)}`);
        else lines.push(`${at}${tag}you: ${excerpt(e.text, 300)}`);
        break;
      case 'assistant_text':
        if (String(e.text ?? '').trim()) lines.push(`${at}${tag}${who}says: ${excerpt(e.text, 400)}`);
        break;
      case 'tool_call':
        calls.set(e.tool_call_id, e);
        lines.push(`${at}${tag}${who}→ ${describeCall(e)}`);
        break;
      case 'tool_result':
        if (e.is_error) {
          const call = calls.get(e.tool_call_id);
          lines.push(`${at}${tag}${who}✗ ${call?.name ?? 'tool'} failed: ${excerpt(e.output, 240)}`);
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
        const judge = origin === 'autonomy' ? '[judge] ' : '';
        lines.push(`${at}✓ ${judge}answered${wait}: ${excerpt(Object.values(e.answers ?? {}).flat().join('; ') || e.response, 200)}`);
        break;
      }
      case 'turn_end':
        lines.push(`${at}${tag}■ turn ${e.is_error ? 'FAILED' : 'ended'} after ${duration(e.duration_ms)}, ${usd(e.cost_usd)} so far${e.is_error && e.result ? `: ${excerpt(e.result, 200)}` : ''}`);
        break;
      case 'log':
      case 'harness_log':
        if (e.level !== 'info' || PLAIN_TEXT_REPROMPT.test(e.message ?? '') || /watchdog/i.test(e.message ?? '')) {
          lines.push(`${at}${tag}${e.level === 'info' ? 'ℹ' : '!'} ${e.source === 'harness' ? 'mothership' : 'runner'} ${e.level}: ${excerpt(e.message, 240)}`);
        }
        break;
      case 'finding':
        lines.push(`${at}${tag}◆ finding: ${excerpt(e.title, 160)}`);
        break;
      case 'validated':
        lines.push(`${at}${tag}✓ validated: ${excerpt(e.title, 160)}${e.severity ? ` (${e.severity})` : ''}`);
        break;
      case 'rejected':
        lines.push(`${at}${tag}✗ rejected: ${excerpt(e.title, 160)} — ${excerpt(e.reason, 120)}`);
        break;
      case 'fix_colony':
        lines.push(`${at}${tag}⚒ fix colony ${e.session} for: ${excerpt(e.title, 160)}`);
        break;
      case 'review':
        lines.push(`${at}${tag}⚖ review ${e.session} of ${excerpt(e.pr, 120)}: ${e.verdict}`);
        break;
      case 'merged':
        lines.push(`${at}${tag}✔ merged ${excerpt(e.pr, 120)}`);
        break;
      case 'verification':
        // Right after the claim's turn ended, so the claim and its verdict read side by side.
        lines.push(`${at}${tag}∎ ${verificationLine(e)}`);
        break;
      case 'memory_proposal':
        lines.push(`${at}${tag}◇ memory proposal (${e.scope}): ${excerpt(e.title, 160)}`);
        break;
      case 'screening':
        for (const line of screeningLines(e)) lines.push(`${at}${tag}${line}`);
        break;
      case 'gateway_request':
        lines.push(`${at}${gatewayLine(e)}`);
        break;
    }
  }
  return lines.join('\n');
}

// ---------------------------------------------------------------------------------------------- command

function parseArgs(argv) {
  const args = { data: [], json: false, worst: 10, since: null, days: null, repo: null, transcript: null, origins: null, costs: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (a === '--data') args.data.push(value());
    else if (a === '--json') args.json = true;
    else if (a === '--worst') args.worst = Number(value());
    else if (a === '--costs') args.costs = true;
    else if (a === '--since') args.since = value();
    else if (a === '--days') args.days = value();
    else if (a === '--repo') args.repo = value();
    else if (a === '--transcript') args.transcript = value();
    else if (a === '--origin') args.origins = parseOrigins(value());
    else if (a.startsWith('--origin=')) args.origins = parseOrigins(a.slice('--origin='.length));
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
  if (args.costs) {
    // The journal windows by its own `day`, not the colony's created_at, and by default sums the
    // same window GET /api/spend/history answers, so the two views reconcile. `--since` overrides
    // the floor and parses the same way the colony filters below do, so an unparseable one keeps
    // meaning "nothing".
    const rows = args.data.flatMap((dir) => loadSpend(dir));
    const window = spendWindow({ since: args.since, days: args.days });
    const costs = costsReport(rows, colonies, { repo: args.repo, window });
    console.log(args.json ? JSON.stringify(costs, null, 2) : formatCosts(costs));
    return;
  }
  if (args.transcript) {
    const matches = colonies.filter((c) => String(c.session.id).startsWith(args.transcript));
    if (matches.length !== 1) throw new Error(matches.length ? `${args.transcript} matches ${matches.length} colonies` : `no colony ${args.transcript}`);
    console.log(formatTranscript({ ...matches[0], origins: args.origins }));
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
