#!/usr/bin/env node
// The fixed set of tasks a change to an agent has to survive. Each task is an issue on a scratch
// repository; the bench launches a colony for it through the mothership's own API, answers the questions it
// asks, and scores the pull request against checks the colony never sees. Run it before a change and after,
// and compare. scripts/colony-report.mjs says what happened inside a colony; this says whether it worked.
//
//   node scripts/bench.mjs seed --repo owner/bench-repo     # create the repo, its files and the issues (once)
//   node scripts/bench.mjs run --repo owner/bench-repo --label before
//   node scripts/bench.mjs run --repo owner/bench-repo --label after --only add-helper,readme-typo --heldout ~/bench-heldout
//   node scripts/bench.mjs heldout add --heldout ~/bench-heldout --family cart-rounding --check my-check.test.mjs
//   node scripts/bench.mjs compare bench-before.json bench-after.json
//   node scripts/bench.mjs jev bench-before.json bench-after.json   # grade Jev compaction across the runs
//   node scripts/bench.mjs brief bench-before.json bench-after.json  # grade Jev's boot brief picks against use
//   node scripts/bench.mjs routing [--threshold 0.8] [--json]       # the tier rule against Jev's second opinion
//   node scripts/bench.mjs clean --repo owner/bench-repo    # close the bench's PRs and delete their branches
//
// `routing` reads <data dir>/routing.jsonl and sessions.json (`--data`, else COLONIZER_DATA_DIR, else
// ~/.local/share/colonizer) and says whether Jev routing `act` mode is justified. It is, only when there
// are at least 20 confident (confidence >= --threshold, default 0.8) disagreements judged in shadow — the
// rule's tier ran and the colony has an outcome — and every direction with at least 5 of them says the
// rule was wrong: where Jev would go lower, the rule's tier merged at least 90% of the time (the task was
// easy); where Jev would go higher, the rule's tier failed at least 15 points more often than the baseline
// of all shadow colonies. Otherwise it reads "not yet justified" and points at the bench comparison.
//
// `brief` reads <data dir>/brief_picks.jsonl (same `--data`/COLONIZER_DATA_DIR/`~/.local/share/colonizer`
// rule) and grades the shadow boot brief of #585: the notes and skill packs Jev picked against the ones the
// colony was later seen to use, per colony, per run and overall. Mandatory notes (always loaded, never
// offered) are out of the universe; precision over no picks and recall over no uses read as undefined.
//
// `run` needs a mothership on COLONIZER_URL (default http://127.0.0.1:7878) with GitHub and an agent
// configured, and `gh` logged in to the account that owns the scratch repository. It costs real model tokens
// and opens real pull requests on that repository, and on nothing else.
import { execFileSync } from 'node:child_process';
import { appendFileSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { analyze, loadColonies, readJsonLines, TOKEN_CATEGORIES, totalCost } from './colony-report.mjs';
import { auditSession } from './trajectory-monitor.mjs';
import { DEFAULT_MAX_GAP, addCompanion, companionsFor, familyGaps, familyOf, formatGaps, gapVerdict, loadSet, lockSet, newSet, outsideRepo, recordDecisions, saveSet } from './bench/heldout.mjs';
import { childEnv, detectStack, STACKS } from './bench/stacks.mjs';

// What a scored child must not inherit — see scripts/bench/stacks.mjs; re-exported for evolve.mjs.
export { childEnv };

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const MOTHERSHIP = process.env.COLONIZER_URL || 'http://127.0.0.1:7878';

// The mothership's API needs its per-install token: `COLONIZER_API_TOKEN` wins, else the same
// `api-token` file the server minted. Absent entirely, requests go out unauthenticated.
function apiToken() {
  if (process.env.COLONIZER_API_TOKEN) return process.env.COLONIZER_API_TOKEN;
  const dir = process.env.COLONIZER_CONFIG_DIR || join(homedir(), '.config/colonizer');
  try {
    return readFileSync(join(dir, 'api-token'), 'utf8').trim() || null;
  } catch {
    return null;
  }
}
const TOKEN = apiToken();
const authHeaders = TOKEN ? { authorization: `Bearer ${TOKEN}` } : {};
const TASKS = JSON.parse(readFileSync(join(ROOT, 'scripts/bench/tasks.json'), 'utf8'));
const ISSUE_MARK = '<!-- colonizer-bench -->';

const gh = (args, options = {}) => execFileSync('gh', args, { encoding: 'utf8', ...options }).trim();
const git = (args, cwd) => execFileSync('git', args, { cwd, encoding: 'utf8' }).trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function api(path, init) {
  // authHeaders last: a caller must not be able to clobber the token.
  const res = await fetch(`${MOTHERSHIP}${path}`, { ...init, headers: { 'content-type': 'application/json', ...init?.headers, ...authHeaders } });
  const text = await res.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  if (!res.ok) throw new Error(`${path}: ${res.status} ${body?.error ?? text}`);
  return body;
}

// ------------------------------------------------------------------------------------------------- seed

/** Creates the scratch repository from the fixture, and one open issue per task. Safe to run again. */
function seed(repo) {
  let exists = true;
  try {
    gh(['repo', 'view', repo, '--json', 'name']);
  } catch {
    exists = false;
  }
  if (!exists) {
    const dir = mkdtempSync(join(tmpdir(), 'colonizer-bench-'));
    cpSync(join(ROOT, TASKS.fixture), dir, { recursive: true });
    git(['init', '-q', '-b', 'main'], dir);
    git(['add', '-A'], dir);
    git(['-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'The bench fixture'], dir);
    gh(['repo', 'create', repo, '--private', '--source', dir, '--push', '--description', "Colonizer's fixed bench tasks"]);
    rmSync(dir, { recursive: true, force: true });
    console.log(`created ${repo} from ${TASKS.fixture}`);
  } else {
    console.log(`${repo} already exists; leaving its files alone`);
  }

  const open = JSON.parse(gh(['issue', 'list', '--repo', repo, '--state', 'all', '--limit', '100', '--json', 'number,title,state,body']));
  const issues = {};
  for (const task of TASKS.tasks) {
    const found = open.find((i) => i.body?.includes(`${ISSUE_MARK} ${task.id}`));
    if (found) {
      if (found.state !== 'OPEN') gh(['issue', 'reopen', String(found.number), '--repo', repo]);
      issues[task.id] = found.number;
      console.log(`  #${found.number} ${task.id} (already there)`);
      continue;
    }
    const body = `${task.body}\n\n${ISSUE_MARK} ${task.id}`;
    const url = gh(['issue', 'create', '--repo', repo, '--title', task.title, '--body', body]);
    const number = Number(url.split('/').pop());
    issues[task.id] = number;
    console.log(`  #${number} ${task.id}`);
  }
  return issues;
}

function benchIssues(repo) {
  const list = JSON.parse(gh(['issue', 'list', '--repo', repo, '--state', 'all', '--limit', '100', '--json', 'number,body']));
  const issues = {};
  for (const task of TASKS.tasks) {
    const found = list.find((i) => i.body?.includes(`${ISSUE_MARK} ${task.id}`));
    if (found) issues[task.id] = found.number;
  }
  return issues;
}

// -------------------------------------------------------------------------------------------------- run

/**
 * Answers a colony's questions while it works, so a run needs nobody watching. It picks the first option
 * whose label matches the task's `answer.prefer`, else the first option, and records what it chose.
 */
function autoAnswer(sessionId, task, log) {
  const url = `${MOTHERSHIP.replace(/^http/, 'ws')}/api/sessions/${sessionId}/events?since=0`;
  // The token rides the handshake: Node's WebSocket takes custom `{ headers }` since Node 22.
  const socket = TOKEN ? new WebSocket(url, { headers: { authorization: `Bearer ${TOKEN}` } }) : new WebSocket(url);
  socket.addEventListener('message', (event) => {
    let frame;
    try {
      frame = JSON.parse(event.data);
    } catch {
      return;
    }
    // The mothership broadcasts a colony's events as they are, so a question arrives as itself.
    const e = frame;
    if (e?.type !== 'question') return;
    const answers = {};
    for (const q of e.questions ?? []) {
      const options = (q.options ?? []).map((o) => o.label);
      const prefer = (task.answer?.prefer ?? []).map((p) => p.toLowerCase());
      const picked = options.find((label) => prefer.some((p) => label.toLowerCase().includes(p))) ?? options[0];
      answers[q.question] = q.multi_select ? [picked] : picked;
      log.push({ question: q.question, options, picked });
    }
    socket.send(JSON.stringify({ type: 'answer', question_id: e.question_id, answers, response: null }));
  });
  return () => socket.close();
}

const DONE = new Set(['pr_opened', 'no_changes', 'failed', 'stopped', 'merged']);

async function runTask(task, repo, issue, options) {
  const answers = [];
  const started = Date.now();
  const session = await api('/api/sessions', { method: 'POST', body: JSON.stringify({ repo, issue, autopilot: true }) });
  const close = autoAnswer(session.id, task, answers);
  let current = session;
  try {
    while (Date.now() - started < options.timeoutMs) {
      await sleep(5000);
      current = await api(`/api/sessions/${session.id}`);
      process.stdout.write(`\r  ${task.id}: ${current.status} (${Math.round((Date.now() - started) / 1000)}s)   `);
      if (DONE.has(current.status)) break;
    }
  } finally {
    close();
  }
  process.stdout.write('\n');
  return { session: current, answers, timed_out: !DONE.has(current.status) };
}

// ------------------------------------------------------------------------------------------------ score

/** The files a change touched that its task does not allow. */
export const outsideTask = (task, changed) => changed.filter((file) => !(task.expect.changed_within ?? []).includes(file));

/** Runs the task's hidden check at the root of a checkout, where the colony never saw it. */
export function runCheck(task, dir) {
  cpSync(join(ROOT, task.expect.check), join(dir, 'bench-check.test.mjs'));
  try {
    return { passed: true, output: execFileSync('node', ['--test', 'bench-check.test.mjs'], { cwd: dir, encoding: 'utf8', env: childEnv() }) };
  } catch (e) {
    return { passed: false, output: `${e.stdout ?? ''}${e.stderr ?? ''}`.slice(-2000) };
  } finally {
    rmSync(join(dir, 'bench-check.test.mjs'), { force: true });
  }
}

/** Whether the checkout's own tests still pass. */
export function runOwnTests(dir) {
  try {
    execFileSync('npm', ['test', '--silent'], { cwd: dir, encoding: 'utf8', env: childEnv() });
    return true;
  } catch {
    return false;
  }
}

// The scorer's git runs against a colony's branch — agent-controlled content — under the same `-c` guards
// the mothership puts on every host-side git (`HOST_GIT_NO_EXEC`, crates/colonizer/src/github.rs).
const SCORER_GIT_FLAGS = ['-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-c', 'gc.auto=0', '-c', 'maintenance.auto=false'];
const scorerGit = (args, cwd) => execFileSync('git', [...SCORER_GIT_FLAGS, ...args], { cwd, encoding: 'utf8', env: childEnv() }).trim();

/** A fresh clone of a branch into `dir` — each scorer gets its own, never the other's checkout — with
 *  `source` injectable so tests can score a local repository. */
export function cloneBranch({ repo, branch, source = `https://github.com/${repo}.git`, dir }) {
  scorerGit(['clone', '--quiet', '--depth', '50', source, dir]);
  scorerGit(['fetch', '--quiet', 'origin', branch], dir);
  scorerGit(['checkout', '--quiet', 'FETCH_HEAD'], dir);
  return dir;
}

/** Checks out the colony's branch, runs the task's hidden check and the repo's own tests, and diffs it. */
export function scoreBranch({ repo, branch, base = 'main', task, workdir, source }) {
  const result = { check: false, regression: null, changed: [], outside: [], check_output: '' };
  const dir = workdir ?? mkdtempSync(join(tmpdir(), 'colonizer-bench-score-'));
  try {
    cloneBranch({ repo, branch, source, dir });
    result.changed = scorerGit(['diff', '--name-only', `origin/${base}...HEAD`], dir).split('\n').filter(Boolean);
    result.outside = outsideTask(task, result.changed);
    if (task.expect.check) {
      const check = runCheck(task, dir);
      result.check = check.passed;
      result.check_output = check.output;
    }
    if (task.expect.regression) result.regression = runOwnTests(dir);
  } finally {
    if (!workdir) rmSync(dir, { recursive: true, force: true });
  }
  return result;
}

/** Scores a held-out companion on its own fresh clone. The clone's stack decides where the companion
 *  lands and what runs it (scripts/bench/stacks.mjs). Only the pass bit and the companion's id come
 *  back; the output is dropped, so held-out material never lands in anything the bench writes. The
 *  clone is the colony's work, so its layout is untrusted: a branch can match no stack, or hold a
 *  `tests` that is a symlink to somewhere on this host. Those score as a held-out fail — nothing is
 *  written outside the clone, and the run goes on. */
export function scoreHeldout({ repo, branch, source, heldoutDir, companion }) {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-bench-heldout-'));
  try {
    cloneBranch({ repo, branch, source, dir });
    let heldout = false;
    let check = null;
    let placed = false;
    try {
      const stack = STACKS[detectStack(dir) ?? ''];
      if (!stack) throw new Error(`${dir} matches no supported stack (Cargo.toml, go.mod, package.json); cannot place the held-out companion`);
      check = stack.companion.path(dir);
      // The companion is written through real directories only: a symlink in its path would land it
      // outside the clone.
      for (const part of [dirname(check), check]) {
        if (lstatSync(part, { throwIfNoEntry: false })?.isSymbolicLink()) throw new Error(`${part} is a symlink; refusing to write the held-out companion through it`);
      }
      mkdirSync(dirname(check), { recursive: true });
      cpSync(join(heldoutDir, companion.file), check);
      placed = true;
      stack.companion.run(dir, check);
      heldout = true;
    } catch {
      heldout = false;
    } finally {
      // Only what we placed is ours to remove — sweeping an untrusted path would follow a symlink out
      // of the clone and delete something on the host.
      if (placed) rmSync(check, { force: true });
    }
    return { companion: companion.id, heldout };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** What the colony did, from the mothership's own records, plus whether the work is right. `visible` and
 *  `heldout` are the check results (null when never scored); `passed` stays visible-based, so runs from
 *  before the suite compare. */
export function scoreTask({ task, session, answers, timed_out, branchScore, colony, heldout = null, scoring = null }) {
  const expect = task.expect ?? {};
  const failures = [];
  const cost = colony?.cost_usd ?? session.cost_usd ?? null;
  const routed = colony?.routed_cost_usd ?? session.routed_cost_usd ?? null;
  if (timed_out) failures.push('timed out');
  if (expect.pr && !session.pr_url) failures.push(`no pull request (${session.status})`);
  if (session.pr_url) {
    if (branchScore && expect.check && !branchScore.check) failures.push('the check failed');
    if (branchScore && expect.regression && branchScore.regression === false) failures.push('it broke the existing tests');
    if (branchScore && branchScore.outside.length > 0) failures.push(`changed files outside the task: ${branchScore.outside.join(', ')}`);
  }
  if (expect.questions != null && colony && colony.questions !== expect.questions) {
    failures.push(`asked ${colony.questions} question${colony.questions === 1 ? '' : 's'}, expected ${expect.questions}`);
  }
  return {
    id: task.id,
    family: familyOf(task),
    // Which harness ran the task, and on what model (issue #296): the colony report's reading when
    // there is one, else the session's. The model is a launch override, else what boot recorded the
    // routing as (crates/colonizer/src/boot.rs) — null when the colony stayed on its module's own
    // model, which lives in settings, not on the session. A tier is a label, not a model, so it
    // rides beside it as `tier` and never stands in for one.
    agent: colony?.agent || session.agent || null,
    model: session.model_override ?? session.model_routing?.model ?? null,
    tier: session.model_routing?.tier ?? null,
    passed: failures.length === 0,
    failures,
    pr_url: session.pr_url ?? null,
    status: session.status,
    cost_usd: cost,
    routed_cost_usd: routed,
    total_cost_usd: totalCost(cost, routed),
    working_ms: colony?.working_ms ?? null,
    wall_ms: colony?.wall_ms ?? null,
    turns: colony?.turns ?? null,
    tool_calls: colony?.tool_calls ?? null,
    tool_errors: colony?.tool_errors ?? null,
    questions: colony?.questions ?? null,
    watchdog_nudges: colony?.watchdog_nudges ?? null,
    plain_text_reprompts: colony?.plain_text_reprompts ?? null,
    subagents: colony?.subagents ?? null,
    token_categories: colony?.tokenCategories ?? null,
    visible: branchScore && expect.check ? branchScore.check : null,
    heldout: heldout?.heldout ?? null,
    scoring: scoring ?? { visible_ms: null, heldout_ms: null },
    changed: branchScore?.changed ?? [],
    outside: branchScore?.outside ?? [],
    answers,
    session_id: session.id,
  };
}

/** A result's whole spend; recomputed, so a run saved before routed cost was recorded still compares. */
const spent = (r) => (r ? totalCost(r.cost_usd, r.routed_cost_usd) : null);

export function summarizeRun(results) {
  const n = results.length || 1;
  const sum = (key) => results.reduce((a, r) => a + (Number(r[key]) || 0), 0);
  const clean = results.filter((r) => r.passed && r.clean === true).length;
  const audited = results.some((r) => typeof r.clean === 'boolean');
  const token_categories = Object.fromEntries(TOKEN_CATEGORIES.map((c) => [c, 0]));
  for (const r of results) for (const c of TOKEN_CATEGORIES) token_categories[c] += r.token_categories?.[c] ?? 0;
  return {
    tasks: results.length,
    passed: results.filter((r) => r.passed).length,
    cost_usd: sum('cost_usd'),
    routed_cost_usd: sum('routed_cost_usd'),
    total_cost_usd: results.reduce((a, r) => a + (spent(r) ?? 0), 0),
    token_categories,
    working_ms: sum('working_ms'),
    // Scoring makes no model calls, so its only cost is the time it took.
    scoring_ms: results.reduce((a, r) => a + (r.scoring?.visible_ms ?? 0) + (r.scoring?.heldout_ms ?? 0), 0),
    questions: sum('questions'),
    tool_errors: sum('tool_errors'),
    pass_rate: results.filter((r) => r.passed).length / n,
    clean_resolved: clean,
    hacked_resolved: results.filter((r) => r.passed && r.clean === false).length,
    clean_rate: audited ? clean / n : null,
    gap: audited ? results.filter((r) => r.passed).length / n - clean / n : null,
  };
}

/** Journals a run's scoring time into the mothership's spend.jsonl (docs/protocol.md §6.8): one
 *  `scoring` row under the `bench` org, so the spend history shows it beside the colonies' spend.
 *  Scoring makes no model calls, so the row carries no tokens and no dollar — the time is all it
 *  spent. A write failure is a lost measurement, not a failed run, so it only warns. */
export function journalScoring(dataDir, scoringMs, now = new Date()) {
  const row = { ts: now.toISOString(), day: now.toISOString().slice(0, 10), org: 'bench', kind: 'scoring', scoring_ms: scoringMs };
  try {
    appendFileSync(join(dataDir, 'spend.jsonl'), `${JSON.stringify(row)}\n`);
  } catch (e) {
    console.error(`bench: could not journal the scoring time to ${join(dataDir, 'spend.jsonl')}: ${e.message}`);
  }
}

// ---------------------------------------------------------------------------------------------- compare

const delta = (after, before, digits = 2) => {
  if (typeof after !== 'number' || typeof before !== 'number') return '–';
  const d = after - before;
  return `${d >= 0 ? '+' : ''}${d.toFixed(digits)}`;
};

export function formatComparison(before, after) {
  const ids = [...new Set([...before.results.map((r) => r.id), ...after.results.map((r) => r.id)])];
  // Which harness ran the task and on what model, when the run recorded it (issue #296). Joined by
  // `·`, not `/`: model names carry slashes of their own (zai/glm-5.3-flash).
  const harness = (r) => (r ? `${r.agent ?? '–'} · ${r.model ?? '–'}` : '–');
  const rows = ids.map((id) => {
    const b = before.results.find((r) => r.id === id);
    const a = after.results.find((r) => r.id === id);
    const mark = (r) => (r ? (r.passed ? 'pass' : 'FAIL') : '–');
    const cleanMark = (r) => (r?.clean === false ? 'HACKED' : r?.clean === true ? 'clean' : '–');
    return [
      id,
      `${harness(b)} → ${harness(a)}`,
      `${mark(b)} → ${mark(a)}`,
      `${cleanMark(b)} → ${cleanMark(a)}`,
      `${spent(b)?.toFixed(2) ?? '–'} → ${spent(a)?.toFixed(2) ?? '–'} (${delta(spent(a), spent(b))})`,
      `${Math.round((b?.working_ms ?? 0) / 1000)}s → ${Math.round((a?.working_ms ?? 0) / 1000)}s`,
      `${b?.questions ?? '–'} → ${a?.questions ?? '–'}`,
      (a?.failures ?? []).join('; ') || '',
    ];
  });
  const head = ['Task', 'Harness · model', 'Result', 'Clean', 'Cost', 'Worked', 'Questions', 'Why it failed'];
  const line = (cells) => `| ${cells.join(' | ')} |`;
  const bs = summarizeRun(before.results);
  const as = summarizeRun(after.results);
  const cleanLine = bs.clean_rate == null || as.clean_rate == null
    ? ''
    : ` Clean resolved ${bs.clean_resolved}/${bs.tasks} → ${as.clean_resolved}/${as.tasks} (clean rate ${Math.round(bs.clean_rate * 100)}% → ${Math.round(as.clean_rate * 100)}%, gap ${Math.round(bs.gap * 100)}% → ${Math.round(as.gap * 100)}%).`;
  // The held-out numbers only mean something against the set version that scored them.
  const bh = before.heldout;
  const ah = after.heldout;
  const say = (h) => (h ? `held-out set v${h.version} max gap ${Math.round((h.families ?? []).reduce((m, f) => Math.max(m, f.gap), 0) * 100)}%` : 'no held-out suite scored');
  let heldoutLine = null;
  if (bh || ah) {
    heldoutLine = `Held-out: ${say(bh)} → ${say(ah)}.`;
    if (bh && ah && bh.version !== ah.version) heldoutLine += ' The set versions differ, so the held-out numbers are not comparable across the rotation.';
  }
  // Only the categories either run actually spent, so the line stays readable; a run saved before
  // the categories were recorded reads as –, not as a zero it never measured.
  const hasCategories = (run) => run.results.some((r) => r.token_categories);
  const bCats = hasCategories(before);
  const aCats = hasCategories(after);
  const spentCategories = TOKEN_CATEGORIES.filter((c) => (bCats ? bs.token_categories[c] : 0) + (aCats ? as.token_categories[c] : 0) > 0);
  const catCell = (s, has, c) => (has ? s.token_categories[c] : '–');
  const tokenLine = spentCategories.length ? `Tokens: ${spentCategories.map((c) => `${c} ${catCell(bs, bCats, c)} → ${catCell(as, aCats, c)}`).join(', ')}.` : null;
  return [
    `# ${before.label} → ${after.label}`,
    '',
    `Passed ${bs.passed}/${bs.tasks} → ${as.passed}/${as.tasks}. Cost $${bs.total_cost_usd.toFixed(2)} → $${as.total_cost_usd.toFixed(2)} (routed $${bs.routed_cost_usd.toFixed(2)} → $${as.routed_cost_usd.toFixed(2)}). Questions ${bs.questions} → ${as.questions}.${cleanLine}`,
    ...(heldoutLine ? ['', heldoutLine] : []),
    ...(tokenLine ? ['', tokenLine] : []),
    '',
    line(head),
    line(head.map(() => '---')),
    ...rows.map(line),
    '',
    'One run of one task is one sample: treat small differences as noise.',
  ].join('\n');
}

// -------------------------------------------------------------------------------------------------- jev

// Stage 2 of the Jev visibility ladder: the bench-wide report grading colonies' Jev compaction against
// each other (#637). The grading mirrors precision_recall in crates/colonizer/src/jev_ladder.rs, keeping
// what the server's reader keeps: predicted positive is a `decision` row whose keep_result is present and
// at or above the threshold (an unscored chunk predicts nothing), actually positive is any `reread` row
// naming that decision's tool_call_id, and a zero denominator is null — undefined, never a zero score.
// Rows are partitioned by session first, so a reread only ever grades decisions from its own session.
function jevMetrics(rows, threshold) {
  const counts = { tp: 0, fp: 0, fn: 0, tn: 0 };
  const decisions = rows.filter((r) => r?.kind === 'decision');
  for (const d of decisions) {
    const reread = rows.some((r) => r?.kind === 'reread' && r.matched_tool_call_id === d.tool_call_id);
    const predicted = d.keep_result != null && d.keep_result >= threshold;
    if (predicted && reread) counts.tp += 1;
    else if (predicted) counts.fp += 1;
    else if (reread) counts.fn += 1;
    else counts.tn += 1;
  }
  return {
    ...counts,
    decisions: decisions.length,
    rereads: rows.filter((r) => r?.kind === 'reread').length,
    precision: counts.tp + counts.fp > 0 ? counts.tp / (counts.tp + counts.fp) : null,
    recall: counts.tp + counts.fn > 0 ? counts.tp / (counts.tp + counts.fn) : null,
  };
}

// A run total pools its groups' counts and recomputes the rates from the pooled counts — the same
// numbers precision_recall would give over that run's rows, because sessions partition the ledger.
function pool(groups) {
  const t = { tp: 0, fp: 0, fn: 0, tn: 0, decisions: 0, rereads: 0 };
  for (const g of groups) for (const k of Object.keys(t)) t[k] += g[k];
  return { ...t, precision: t.tp + t.fp > 0 ? t.tp / (t.tp + t.fp) : null, recall: t.tp + t.fn > 0 ? t.tp / (t.tp + t.fn) : null };
}

const NO_RUN = '(no run)';

/** The ledger graded per (run, colony) with a per-run and an overall total. A colony is a session; its
 *  run is the first of `runs` (parsed `bench-<label>.json` files) whose results name its session_id,
 *  which also carries that result's task, agent and model for display. A session no given run names —
 *  chat colonies, run files left off the command line — grades under `(no run)`. */
export function jevReport(rows, runs, threshold) {
  const runsOf = new Map();
  for (const run of runs) {
    for (const r of run.results ?? []) {
      if (r?.session_id && !runsOf.has(r.session_id)) {
        runsOf.set(r.session_id, { run: run.label ?? NO_RUN, task: r.id ?? null, agent: r.agent ?? null, model: r.model ?? null });
      }
    }
  }
  const groups = new Map();
  for (const row of rows) {
    if (row?.kind !== 'decision' && row?.kind !== 'reread') continue;
    const meta = runsOf.get(row.session) ?? { run: NO_RUN, task: null, agent: null, model: null };
    const key = `${meta.run}\u0000${row.session}`;
    if (!groups.has(key)) groups.set(key, { ...meta, colony: row.session, rows: [] });
    groups.get(key).rows.push(row);
  }
  const order = [...new Set([...runs.map((r) => r.label ?? NO_RUN), NO_RUN])];
  const runsOut = order
    .filter((label) => [...groups.values()].some((g) => g.run === label))
    .map((label) => {
      const colonies = [...groups.values()]
        .filter((g) => g.run === label)
        .map(({ rows: groupRows, run: _label, ...colony }) => ({ ...colony, ...jevMetrics(groupRows, threshold) }));
      return { run: label, colonies, total: pool(colonies) };
    });
  return { threshold, runs: runsOut, total: pool(runsOut.flatMap((r) => r.colonies)) };
}

/** The human table, in the style of formatComparison: precision and recall to two places, `–` where the
 *  denominator is zero, and the counts beside them so a small sample is visible. */
export function formatJevReport(report) {
  const score = (v) => (v == null ? '–' : v.toFixed(2));
  const harness = (c) => `${c.agent ?? '–'} · ${c.model ?? '–'}`;
  const head = ['Run', 'Colony', 'Task', 'Harness · model', 'Decisions', 'Rereads', 'TP', 'FP', 'FN', 'TN', 'Precision', 'Recall'];
  const line = (cells) => `| ${cells.join(' | ')} |`;
  const colonyRow = (run, c) => [run, c.colony, c.task ?? '–', harness(c), c.decisions, c.rereads, c.tp, c.fp, c.fn, c.tn, score(c.precision), score(c.recall)];
  const totalRow = (run, label, t) => [run, label, '', '', t.decisions, t.rereads, t.tp, t.fp, t.fn, t.tn, score(t.precision), score(t.recall)];
  const rows = report.runs.flatMap((r) => [...r.colonies.map((c) => colonyRow(r.run, c)), totalRow(r.run, 'total', r.total)]);
  rows.push(totalRow('overall', '', report.total));
  return [
    `# Jev compaction, graded at threshold ${report.threshold}`,
    '',
    line(head),
    line(head.map(() => '---')),
    ...rows.map(line),
    '',
    'Precision over no positive predictions and recall over no actual positives read as –: undefined, not a bad score. One colony in one run is a small sample; read the counts beside the rates.',
  ].join('\n');
}

// ------------------------------------------------------------------------------------------------- brief

// Grading Jev brief picks (#585): the boot picker asks Jev, in shadow, which shared-memory notes and
// skill packs a colony should load, and `<data dir>/brief_picks.jsonl` records the picks and the items
// the colony was later seen to use. This grades the picks against that use, over the offered
// candidates (mandatory notes, always loaded and never offered, are excluded): a picked candidate the
// colony used is a true positive, a picked one it never used a false positive, a used one it did not
// pick a false negative. Precision over no picks and recall over no uses read as null: undefined, not
// a bad score — the same rule the compaction and routing reports keep.

/** One session's picks against its uses. `pickRow` is the session's last `pick` row (a colony booted
 *  twice is judged on its last boot), `usedItems` every item its `used` rows name. */
export function briefMetrics(pickRow, usedItems) {
  const candidates = pickRow?.candidates ?? [];
  const mandatory = new Set(pickRow?.mandatory ?? []);
  const picks = (pickRow?.picks ?? []).filter((p) => candidates.includes(p));
  const used = new Set((usedItems ?? []).filter((i) => !mandatory.has(i) && candidates.includes(i)));
  const picked = new Set(picks);
  let tp = 0;
  let fp = 0;
  let fn = 0;
  for (const c of candidates) {
    const p = picked.has(c);
    const u = used.has(c);
    if (p && u) tp += 1;
    else if (p) fp += 1;
    else if (u) fn += 1;
  }
  return {
    candidates: candidates.length,
    picks: picks.length,
    mandatory: mandatory.size,
    tp,
    fp,
    fn,
    precision: tp + fp > 0 ? tp / (tp + fp) : null,
    recall: tp + fn > 0 ? tp / (tp + fn) : null,
  };
}

// A total pools its colonies' counts and recomputes the rates from the pooled counts, and carries the
// per-colony means of the three counts — one colony's pick is a small sample, so read them together.
function poolBrief(groups) {
  const t = { tp: 0, fp: 0, fn: 0 };
  for (const g of groups) for (const k of Object.keys(t)) t[k] += g[k];
  const mean = (key) => (groups.length > 0 ? groups.reduce((sum, g) => sum + g[key], 0) / groups.length : null);
  return {
    ...t,
    colonies: groups.length,
    precision: t.tp + t.fp > 0 ? t.tp / (t.tp + t.fp) : null,
    recall: t.tp + t.fn > 0 ? t.tp / (t.tp + t.fn) : null,
    mean_candidates: mean('candidates'),
    mean_picks: mean('picks'),
    mean_mandatory: mean('mandatory'),
  };
}

/** The brief-pick ledger graded per (run, colony), with a per-run and an overall total, in the same
 *  shape `jevReport` uses: a session grades under the first run whose results name it, else `(no run)`. */
export function briefReport(rows, runs) {
  const runsOf = new Map();
  for (const run of runs) {
    for (const r of run.results ?? []) {
      if (r?.session_id && !runsOf.has(r.session_id)) {
        runsOf.set(r.session_id, { run: run.label ?? NO_RUN, task: r.id ?? null, agent: r.agent ?? null, model: r.model ?? null });
      }
    }
  }
  const picks = new Map();
  const used = new Map();
  for (const row of rows) {
    if (!row?.session_id) continue;
    if (row.kind === 'pick') picks.set(row.session_id, row);
    else if (row.kind === 'used') {
      if (!used.has(row.session_id)) used.set(row.session_id, []);
      used.get(row.session_id).push(row.item);
    }
  }
  const groups = new Map();
  for (const [session, pickRow] of picks) {
    const meta = runsOf.get(session) ?? { run: NO_RUN, task: null, agent: null, model: null };
    groups.set(`${meta.run}\u0000${session}`, { ...meta, colony: session, ...briefMetrics(pickRow, used.get(session) ?? []) });
  }
  const order = [...new Set([...runs.map((r) => r.label ?? NO_RUN), NO_RUN])];
  const runsOut = order
    .filter((label) => [...groups.values()].some((g) => g.run === label))
    .map((label) => {
      const colonies = [...groups.values()].filter((g) => g.run === label);
      return { run: label, colonies, total: poolBrief(colonies) };
    });
  return { runs: runsOut, total: poolBrief(runsOut.flatMap((r) => r.colonies)) };
}

/** The human table, in the style of formatJevReport: precision and recall to two places, `–` where the
 *  denominator is zero, and the counts beside them. The total row's first three numbers are the means. */
export function formatBriefReport(report) {
  const score = (v) => (v == null ? '–' : v.toFixed(2));
  const mean = (v) => (v == null ? '–' : v.toFixed(1));
  const harness = (c) => `${c.agent ?? '–'} · ${c.model ?? '–'}`;
  const head = ['Run', 'Colony', 'Task', 'Harness · model', 'Candidates', 'Picks', 'Mandatory', 'TP', 'FP', 'FN', 'Precision', 'Recall'];
  const line = (cells) => `| ${cells.join(' | ')} |`;
  const colonyRow = (run, c) => [run, c.colony, c.task ?? '–', harness(c), c.candidates, c.picks, c.mandatory, c.tp, c.fp, c.fn, score(c.precision), score(c.recall)];
  const totalRow = (run, label, t) => [run, label, '', '', mean(t.mean_candidates), mean(t.mean_picks), mean(t.mean_mandatory), t.tp, t.fp, t.fn, score(t.precision), score(t.recall)];
  const rows = report.runs.flatMap((r) => [...r.colonies.map((c) => colonyRow(r.run, c)), totalRow(r.run, 'total', r.total)]);
  rows.push(totalRow('overall', '', report.total));
  return [
    '# Jev brief picks, graded against the notes and packs the colony used',
    '',
    line(head),
    line(head.map(() => '---')),
    ...rows.map(line),
    '',
    'The universe is the offered candidates; mandatory notes (house-rule/security) always load and are excluded. Precision over no picks and recall over no uses read as –: undefined, not a bad score. In a total row the Candidates, Picks and Mandatory columns are per-colony means.',
  ].join('\n');
}

// ---------------------------------------------------------------------------------------------- routing

// Is Jev `act` mode justified? (#583) The mothership's routing.jsonl ledger holds one `decision` row per
// routed boot — the rule's tier, the source that won, and Jev's second opinion when one was asked — and an
// `actual` row with the dollars the colony really spent once it ends. This joins them to each colony's
// outcome in sessions.json and asks what `act` would have changed, and whether the colonies it would have
// changed suggest it should.

const TIERS = ['low', 'medium', 'high'];
const tierRank = (t) => TIERS.indexOf(t);
const maxTier = (a, b) => (tierRank(a) >= tierRank(b) ? a : b);

// The verdict's criteria, in one place so the help text and the report cannot drift apart.
export const ROUTING_VERDICT = {
  // Confident disagreements, judged in shadow (the rule's tier is the one that ran), with an outcome.
  minSamples: 20,
  // A direction (Jev lower / Jev higher) with fewer than this many samples is not judged at all.
  minPerDirection: 5,
  // Jev lower: the rule's tier must have merged at least this often — the task was easy, so the cheaper
  // tier was likely enough.
  lowerMergedRate: 0.9,
  // Jev higher: the rule's tier must have failed at least this much more often than the baseline (every
  // shadow colony with an outcome) — the task was harder than the rule thought.
  higherFailMargin: 0.15,
};

/** A colony's end state, from its sessions.json record: merged, pr-open, failed (the run failed, or its
 *  pull request was closed unmerged), other (no changes, stopped, parked), or pending (still running, or
 *  sessions.json has lost it). */
export function routingOutcome(session) {
  if (!session) return 'pending';
  if (session.status === 'merged' || session.merged_at) return 'merged';
  if (session.status === 'pr_opened') return 'pr-open';
  if (session.status === 'failed' || session.status === 'closed') return 'failed';
  if (['no_changes', 'stopped', 'parked'].includes(session.status)) return 'other';
  return 'pending';
}

const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const median = (xs) => {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};

/** The tier `act` mode would have run: Jev's, when it was confident enough and disagreed, never under an
 *  operator override or with routing off, and never below the decision's floor. */
export function actTier(d, threshold) {
  const rule = d.rule;
  const jev = d.jev;
  if (!jev || !TIERS.includes(jev.tier) || jev.tier === rule) return { tier: rule, blocked: null };
  if (typeof jev.confidence !== 'number' || jev.confidence < threshold) return { tier: rule, blocked: 'unconfident' };
  if (d.source === 'override') return { tier: rule, blocked: 'override' };
  if (d.source === 'off') return { tier: rule, blocked: 'off' };
  const floored = TIERS.includes(d.floor) ? maxTier(jev.tier, d.floor) : jev.tier;
  return { tier: floored, blocked: floored === jev.tier ? null : 'floor' };
}

function directionStats(items) {
  const n = items.length;
  const count = (o) => items.filter((i) => i.outcome === o).length;
  const costs = items.map((i) => i.actual_cost_usd).filter((c) => typeof c === 'number');
  return {
    count: n,
    merged: count('merged'),
    failed: count('failed'),
    merged_rate: n ? count('merged') / n : null,
    failed_rate: n ? count('failed') / n : null,
    mean_actual_cost_usd: mean(costs),
  };
}

/** The verdict on `act`, from the shadow evidence: see ROUTING_VERDICT. */
export function routingVerdict({ samples, lower, higher, baselineFailedRate }, criteria = ROUTING_VERDICT) {
  const how = 'run the bench both ways instead: `bench.mjs run --label rule` with jev_routing_act off, `bench.mjs run --label jev` with it on, then `bench.mjs compare bench-rule.json bench-jev.json`';
  if (samples < criteria.minSamples) {
    return { justified: false, reason: `only ${samples} confident disagreement${samples === 1 ? '' : 's'} with an outcome in shadow; need ${criteria.minSamples}`, recommend: how };
  }
  const judged = [];
  const failures = [];
  if (lower.count >= criteria.minPerDirection) {
    judged.push('lower');
    if (lower.merged_rate < criteria.lowerMergedRate) failures.push(`where Jev would go lower, the rule's tier merged ${pct(lower.merged_rate)}, under ${pct(criteria.lowerMergedRate)}: the cheaper tier is not shown to be enough`);
  }
  if (higher.count >= criteria.minPerDirection) {
    judged.push('higher');
    const bar = (baselineFailedRate ?? 0) + criteria.higherFailMargin;
    if (higher.failed_rate < bar) failures.push(`where Jev would go higher, the rule's tier failed ${pct(higher.failed_rate)}, not ${pct(criteria.higherFailMargin)} over the ${pct(baselineFailedRate ?? 0)} baseline`);
  }
  if (judged.length === 0) return { justified: false, reason: `neither direction has ${criteria.minPerDirection} samples to judge`, recommend: how };
  if (failures.length) return { justified: false, reason: failures.join('; '), recommend: how };
  return { justified: true, reason: `${samples} confident disagreements, and every direction with ${criteria.minPerDirection}+ samples (${judged.join(', ')}) shows the rule's tier was wrong`, recommend: 'confirm with the bench comparison before turning jev_routing_act on everywhere' };
}

const pct = (v) => (v == null ? '–' : `${Math.round(v * 100)}%`);

/** The routing ledger joined to outcomes. `rows` are routing.jsonl's lines; `sessions` are sessions.json's
 *  records (or loadColonies' `{session}` wrappers). A colony booted more than once is judged on its last
 *  decision; its actual cost is its last `actual` row, else its session's own total. */
export function routingReport(rows, sessions, threshold = 0.8) {
  const byId = new Map((sessions ?? []).map((s) => s?.session ?? s).filter((s) => s?.id).map((s) => [s.id, s]));
  const decisionRows = rows.filter((r) => r?.kind === 'decision' && r.decision && r.session);
  const last = new Map();
  for (const r of decisionRows) last.set(r.session, r);
  const actual = new Map();
  for (const r of rows) if (r?.kind === 'actual' && r.session && typeof r.actual_cost_usd === 'number') actual.set(r.session, r.actual_cost_usd);

  const colonies = [...last.values()].map((r) => {
    const d = r.decision;
    const session = byId.get(r.session);
    const jev = d.jev && TIERS.includes(d.jev.tier) ? d.jev : null;
    const agrees = typeof d.jev_agrees === 'boolean' ? d.jev_agrees : jev ? jev.tier === d.rule : null;
    const act = actTier({ ...d, jev }, threshold);
    return {
      session: r.session,
      repo: r.repo ?? null,
      issue: r.issue ?? null,
      rule: d.rule,
      ran: d.tier,
      source: d.source,
      floor: d.floor ?? null,
      jev_mode: d.jev_mode ?? null,
      jev_tier: jev?.tier ?? null,
      confidence: typeof jev?.confidence === 'number' ? jev.confidence : null,
      agrees,
      act_tier: act.tier,
      act_blocked: act.blocked,
      outcome: routingOutcome(session),
      actual_cost_usd: actual.get(r.session) ?? (session ? totalCost(session.cost_usd, session.routed_cost_usd) : null),
    };
  });

  const withJev = colonies.filter((c) => c.agrees !== null);
  const agreeRate = (cs) => (cs.length ? cs.filter((c) => c.agrees).length / cs.length : null);
  const byRule = Object.fromEntries(
    TIERS.map((t) => {
      const cs = withJev.filter((c) => c.rule === t);
      return [t, { with_jev: cs.length, agree: cs.filter((c) => c.agrees).length, rate: agreeRate(cs) }];
    }),
  );
  const conf = (cs) => {
    const xs = cs.map((c) => c.confidence).filter((x) => x != null);
    return { count: xs.length, mean: mean(xs), median: median(xs) };
  };

  const confident = withJev.filter((c) => !c.agrees && c.confidence != null && c.confidence >= threshold);
  const disagreements = withJev
    .filter((c) => !c.agrees)
    .map((c) => ({ ...c, direction: tierRank(c.jev_tier) < tierRank(c.rule) ? 'lower' : 'higher' }));
  // Shadow evidence: the rule's tier is the one that ran, so its outcome says something about the rule,
  // and act would have run another tier — a disagreement an override or the floor cancels is not about act.
  const shadow = (c) => c.ran === c.rule && c.outcome !== 'pending';
  const evidence = disagreements.filter((c) => shadow(c) && c.act_tier !== c.rule);
  const baselinePool = colonies.filter(shadow);
  const baselineFailedRate = baselinePool.length ? baselinePool.filter((c) => c.outcome === 'failed').length / baselinePool.length : null;
  const lower = directionStats(evidence.filter((c) => c.direction === 'lower'));
  const higher = directionStats(evidence.filter((c) => c.direction === 'higher'));

  return {
    threshold,
    criteria: ROUTING_VERDICT,
    decision_rows: decisionRows.length,
    colonies: colonies.length,
    with_jev: withJev.length,
    agreement_rate: agreeRate(withJev),
    agreement_by_rule: byRule,
    confidence: { agree: conf(withJev.filter((c) => c.agrees)), disagree: conf(withJev.filter((c) => !c.agrees)) },
    act: {
      confident_disagreements: confident.length,
      would_change: colonies.filter((c) => c.act_tier !== c.rule).length,
      capped_by_floor: confident.filter((c) => c.act_blocked === 'floor').length,
      floor_cancels: confident.filter((c) => c.act_blocked === 'floor' && c.act_tier === c.rule).length,
      blocked_by_override: confident.filter((c) => c.act_blocked === 'override').length,
      blocked_routing_off: confident.filter((c) => c.act_blocked === 'off').length,
    },
    disagreements,
    baseline: { colonies: baselinePool.length, failed_rate: baselineFailedRate },
    directions: { lower, higher },
    verdict: routingVerdict({ samples: evidence.length, lower, higher, baselineFailedRate }),
  };
}

export function formatRoutingReport(report) {
  const num = (v, digits = 2) => (v == null ? '–' : v.toFixed(digits));
  const money = (v) => (v == null ? '–' : `$${v.toFixed(2)}`);
  const line = (cells) => `| ${cells.join(' | ')} |`;
  const table = (head, rows) => [line(head), line(head.map(() => '---')), ...rows.map(line)];
  const a = report.act;
  const out = [
    `# Tier routing: the rule against Jev's second opinion (act threshold ${report.threshold})`,
    '',
    `${report.colonies} routed colonies (${report.decision_rows} decision rows); ${report.with_jev} with a Jev opinion; agreement ${pct(report.agreement_rate)}.`,
    `Confidence when Jev agrees: mean ${num(report.confidence.agree.mean)}, median ${num(report.confidence.agree.median)} (n=${report.confidence.agree.count}); when it disagrees: mean ${num(report.confidence.disagree.mean)}, median ${num(report.confidence.disagree.median)} (n=${report.confidence.disagree.count}).`,
    `Under act at ${report.threshold}: ${a.confident_disagreements} confident disagreements; ${a.would_change} colonies would have run another tier. ${a.blocked_by_override} blocked by an operator override, ${a.blocked_routing_off} with routing off, ${a.capped_by_floor} raised by the floor (${a.floor_cancels} of them back to the rule's tier).`,
    '',
    ...table(
      ['Rule tier', 'With Jev', 'Agree', 'Rate'],
      TIERS.map((t) => [t, report.agreement_by_rule[t].with_jev, report.agreement_by_rule[t].agree, pct(report.agreement_by_rule[t].rate)]),
    ),
  ];
  if (report.disagreements.length) {
    out.push(
      '',
      '## Disagreements',
      '',
      ...table(
        ['Colony', 'Issue', 'Rule', 'Jev', 'Confidence', 'Ran', 'Act would run', 'Outcome', 'Actual cost'],
        report.disagreements.map((c) => [c.session, c.repo ? `${c.repo}${c.issue != null ? `#${c.issue}` : ''}` : '–', c.rule, c.jev_tier, num(c.confidence), c.ran ?? '–', c.act_blocked ? `${c.act_tier} (${c.act_blocked})` : c.act_tier, c.outcome, money(c.actual_cost_usd)]),
      ),
    );
  }
  const d = report.directions;
  out.push(
    '',
    `## Confident disagreements in shadow, by direction (baseline failed rate ${pct(report.baseline.failed_rate)} over ${report.baseline.colonies} colonies)`,
    '',
    ...table(
      ['Jev vs rule', 'Count', 'Merged', 'Failed', 'Mean actual cost'],
      [
        ['lower', d.lower.count, pct(d.lower.merged_rate), pct(d.lower.failed_rate), money(d.lower.mean_actual_cost_usd)],
        ['higher', d.higher.count, pct(d.higher.merged_rate), pct(d.higher.failed_rate), money(d.higher.mean_actual_cost_usd)],
      ],
    ),
    '',
    report.verdict.justified ? `Verdict: act is justified: ${report.verdict.reason}; ${report.verdict.recommend}.` : `Verdict: act is not yet justified: ${report.verdict.reason}. ${report.verdict.recommend[0].toUpperCase()}${report.verdict.recommend.slice(1)}.`,
  );
  return out.join('\n');
}

// ------------------------------------------------------------------------------------------------ clean

function clean(repo) {
  const prs = JSON.parse(gh(['pr', 'list', '--repo', repo, '--state', 'open', '--limit', '100', '--json', 'number,headRefName']));
  for (const pr of prs.filter((p) => p.headRefName.startsWith('colonizer/'))) {
    gh(['pr', 'close', String(pr.number), '--repo', repo, '--delete-branch', '--comment', 'Closed by the bench.']);
    console.log(`closed #${pr.number} (${pr.headRefName})`);
  }
  if (prs.length === 0) console.log('nothing to close');
}

// ---------------------------------------------------------------------------------------------- command

export function parseArgs(argv) {
  // `threshold` defaults per command (below).
  const args = { command: argv[0], repo: null, label: 'run', only: null, timeoutMs: 20 * 60_000, data: null, threshold: null, json: false, heldout: null, maxGap: DEFAULT_MAX_GAP, family: null, check: null, files: [] };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    // Refuse a missing value here: a flag left dangling would otherwise be read as undefined
    // (a NaN --timeout) or silently dropped (--data), after the colonies have already been created.
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (a === '--repo') args.repo = value();
    else if (a === '--label') args.label = value();
    else if (a === '--only') args.only = value().split(',').map((s) => s.trim());
    else if (a === '--timeout') {
      const seconds = Number(value());
      if (!Number.isFinite(seconds) || seconds <= 0) throw new Error('--timeout needs a positive number of seconds');
      args.timeoutMs = seconds * 1000;
    } else if (a === '--data') args.data = value();
    else if (a === '--threshold') {
      const threshold = Number(value());
      if (!Number.isFinite(threshold)) throw new Error('--threshold needs a number');
      args.threshold = threshold;
    } else if (a === '--json') args.json = true;
    else if (a === '--heldout') args.heldout = value();
    else if (a === '--max-gap') args.maxGap = Number(value());
    else if (a === '--family') args.family = value();
    else if (a === '--check') args.check = value();
    else if (a.startsWith('--')) throw new Error(`unknown argument ${a}`);
    else args.files.push(a);
  }
  // `jev`'s default mirrors PREDICTED_THRESHOLD in crates/colonizer/src/jev_ladder.rs: the score the
  // plugin itself kept at, so `jev` grades the decisions compaction actually made. `routing`'s is the
  // confidence Jev must reach before `act` mode would let it change a tier.
  if (args.threshold == null) args.threshold = args.command === 'routing' ? 0.8 : 0.5;
  return args;
}

// The mothership's data dir the readers look at: the colony report, the trajectory monitor and `jev`'s
// ledger all read it.
const dataDirOf = (args) => args.data || process.env.COLONIZER_DATA_DIR || join(process.env.HOME ?? '', '.local/share/colonizer');

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.command === 'seed') {
    if (!args.repo) throw new Error('seed needs --repo owner/name');
    seed(args.repo);
    return;
  }
  if (args.command === 'clean') {
    if (!args.repo) throw new Error('clean needs --repo owner/name');
    clean(args.repo);
    return;
  }
  if (args.command === 'compare') {
    const [a, b] = args.files.map((f) => JSON.parse(readFileSync(f, 'utf8')));
    if (!a || !b) throw new Error('compare needs two run files');
    console.log(formatComparison(a, b));
    return;
  }
  if (args.command === 'jev') {
    const rows = readJsonLines(join(dataDirOf(args), 'jev_ladder.jsonl'));
    const report = jevReport(rows, args.files.map((f) => JSON.parse(readFileSync(f, 'utf8'))), args.threshold);
    console.log(args.json ? JSON.stringify(report, null, 2) : formatJevReport(report));
    return;
  }
  if (args.command === 'routing') {
    // Verdict criteria (ROUTING_VERDICT): act is justified only with ≥20 confident (≥ --threshold)
    // disagreements judged in shadow — the rule's tier ran and the colony has an outcome — and, for each
    // direction with ≥5 of them, the evidence says the rule was wrong: where Jev would go lower, the rule's
    // tier merged ≥90% of the time; where Jev would go higher, the rule's tier failed ≥15 points more
    // often than every shadow colony's baseline. Anything less reads "not yet justified".
    const dataDir = dataDirOf(args);
    const rows = readJsonLines(join(dataDir, 'routing.jsonl'));
    let sessions = [];
    try {
      const parsed = JSON.parse(readFileSync(join(dataDir, 'sessions.json'), 'utf8'));
      if (Array.isArray(parsed)) sessions = parsed;
    } catch {
      // No sessions.json: every outcome reads pending, and the verdict says there is no evidence.
    }
    const report = routingReport(rows, sessions, args.threshold);
    console.log(args.json ? JSON.stringify(report, null, 2) : formatRoutingReport(report));
    return;
  }
  if (args.command === 'brief') {
    const rows = readJsonLines(join(dataDirOf(args), 'brief_picks.jsonl'));
    const report = briefReport(rows, args.files.map((f) => JSON.parse(readFileSync(f, 'utf8'))));
    console.log(args.json ? JSON.stringify(report, null, 2) : formatBriefReport(report));
    return;
  }
  if (args.command === 'heldout') {
    if (args.files[0] !== 'add' || !args.heldout || !args.family || !args.check) throw new Error('use heldout add --heldout <dir> --family <family> --check <file>');
    outsideRepo(args.heldout); // before the lock, which would create the directory it guards
    const unlock = lockSet(args.heldout);
    try {
      const set = existsSync(join(args.heldout, 'heldout.json')) ? loadSet(args.heldout) : newSet(args.heldout);
      const entry = addCompanion(set, { family: args.family, check: args.check });
      saveSet(set);
      console.log(`added ${entry.id} (${entry.file}); held-out set at ${set.dir} is now v${set.version}`);
    } finally {
      unlock();
    }
    return;
  }
  if (args.command !== 'run') throw new Error('use seed, run, heldout add, compare, jev, brief, routing or clean');
  if (!args.repo) throw new Error('run needs --repo owner/name');

  const issues = benchIssues(args.repo);
  const tasks = TASKS.tasks.filter((t) => (args.only ? args.only.includes(t.id) : true));
  for (const task of tasks) if (!issues[task.id]) throw new Error(`${args.repo} has no issue for ${task.id}; run seed first`);
  // The set and its companions are resolved before any colony launches: a family without an active
  // companion fails the run here, before it spends.
  const set = args.heldout ? loadSet(args.heldout) : null;
  if (set && !Number.isFinite(args.maxGap)) throw new Error(`--max-gap needs a number, not ${args.maxGap}`);
  const companions = set ? companionsFor(set, tasks.map(familyOf)) : null;
  const status = await api('/api/status');
  if (!status.github?.connected) throw new Error('the mothership has no GitHub connection');

  const results = [];
  const scoredCompanions = [];
  for (const task of tasks) {
    console.log(`${task.id}: colony on ${args.repo}#${issues[task.id]}`);
    const { session, answers, timed_out } = await runTask(task, args.repo, issues[task.id], args);
    const dataDir = dataDirOf(args);
    const colony = loadColonies(dataDir).find((c) => c.session.id === session.id);
    const scoring = { visible_ms: null, heldout_ms: null };
    let branchScore = null;
    let companionScore = null;
    if (session.pr_url) {
      const visibleAt = Date.now();
      branchScore = scoreBranch({ repo: args.repo, branch: session.branch, base: session.base ?? 'main', task });
      scoring.visible_ms = Date.now() - visibleAt;
      if (companions) {
        const heldoutAt = Date.now();
        companionScore = scoreHeldout({ repo: args.repo, branch: session.branch, heldoutDir: set.dir, companion: companions.get(familyOf(task)) });
        scoring.heldout_ms = Date.now() - heldoutAt;
        scoredCompanions.push(companionScore.companion);
      }
    }
    const scored = scoreTask({ task, session, answers, timed_out, branchScore, colony: colony ? analyze(colony) : null, heldout: companionScore, scoring });
    // Post-hoc: the trajectory monitor audits the same colony's persisted record. A missing log leaves the
    // result unaudited (clean: null), never clean.
    const trajectory = auditSession(dataDir, session.id);
    scored.clean = trajectory ? trajectory.clean : null;
    scored.hacks = trajectory ? trajectory.hits.filter((h) => h.status === 'enforcing').map((h) => h.pattern) : [];
    results.push(scored);
    console.log(`  ${scored.passed ? 'pass' : `FAIL: ${scored.failures.join('; ')}`}${scored.hacks.length > 0 ? ` [hacks: ${scored.hacks.join(', ')}]` : ''}`);
  }

  // One scoring decision per companion actually scored. The set is re-read under the lock, so a
  // `heldout add` during the run survives; the report names the scored version, rotation in next_version.
  let heldout = null;
  let gapReport = null;
  if (set) {
    const scoredVersion = set.version;
    let current = set;
    const unlock = lockSet(set.dir);
    try {
      current = loadSet(set.dir);
      recordDecisions(current, scoredCompanions);
      saveSet(current);
    } finally {
      unlock();
    }
    const gaps = familyGaps(results);
    const verdict = gapVerdict(gaps, args.maxGap);
    heldout = { version: scoredVersion, max_gap: args.maxGap, families: gaps, failures: verdict.failures };
    if (current.version !== scoredVersion) heldout.next_version = current.version;
    gapReport = formatGaps(gaps, { version: scoredVersion, nextVersion: heldout.next_version, maxGap: args.maxGap });
  }

  const run = { label: args.label, repo: args.repo, at: new Date().toISOString(), agent: status.modules?.agent ?? null, heldout, results };
  const file = `bench-${args.label}.json`;
  writeFileSync(file, JSON.stringify(run, null, 2));
  if (heldout) {
    console.log(`\n${gapReport}`);
    for (const failure of heldout.failures) console.log(`held-out gap over threshold: ${failure}`);
    if (heldout.failures.length > 0) process.exitCode = 1;
  } else {
    console.log('\nNo held-out suite was scored (pass --heldout <dir> to score one).');
  }
  const s = summarizeRun(results);
  journalScoring(dataDirOf(args), s.scoring_ms);
  console.log(`\n${s.passed}/${s.tasks} passed · $${s.total_cost_usd.toFixed(2)} (routed $${s.routed_cost_usd.toFixed(2)}) · ${s.questions} questions · written to ${file}`);
  console.log(`Compare with: node scripts/bench.mjs compare bench-<other>.json ${file}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((e) => {
    console.error(`bench: ${e.message}`);
    process.exit(1);
  });
}
