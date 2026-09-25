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
//   node scripts/bench.mjs clean --repo owner/bench-repo    # close the bench's PRs and delete their branches
//
// `run` needs a mothership on COLONIZER_URL (default http://127.0.0.1:7878) with GitHub and an agent
// configured, and `gh` logged in to the account that owns the scratch repository. It costs real model tokens
// and opens real pull requests on that repository, and on nothing else.
import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { analyze, loadColonies, TOKEN_CATEGORIES, totalCost } from './colony-report.mjs';
import { auditSession } from './trajectory-monitor.mjs';
import { DEFAULT_MAX_GAP, addCompanion, companionsFor, familyGaps, familyOf, formatGaps, gapVerdict, loadSet, lockSet, newSet, outsideRepo, recordDecisions, saveSet } from './bench/heldout.mjs';

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

// A node --test started from inside another one inherits NODE_TEST_CONTEXT, skips its files and exits 0 —
// every check would pass under the bench's own tests — and a colony sandbox exports GIT_DIR and friends
// that would point scorer git at the wrong work tree. None of it rides into a scored child.
const childEnv = () => {
  const { NODE_TEST_CONTEXT, NODE_OPTIONS, GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, ...env } = process.env;
  return env;
};

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

/** Scores a held-out companion on its own fresh clone. Only the pass bit and the companion's id come
 *  back; the output is dropped, so held-out material never lands in anything the bench writes. */
export function scoreHeldout({ repo, branch, source, heldoutDir, companion }) {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-bench-heldout-'));
  try {
    cloneBranch({ repo, branch, source, dir });
    cpSync(join(heldoutDir, companion.file), join(dir, 'heldout-check.test.mjs'));
    let heldout;
    try {
      execFileSync('node', ['--test', 'heldout-check.test.mjs'], { cwd: dir, encoding: 'utf8', env: childEnv() });
      heldout = true;
    } catch {
      heldout = false;
    } finally {
      rmSync(join(dir, 'heldout-check.test.mjs'), { force: true });
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
  const args = { command: argv[0], repo: null, label: 'run', only: null, timeoutMs: 20 * 60_000, data: null, heldout: null, maxGap: DEFAULT_MAX_GAP, family: null, check: null, files: [] };
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
    else if (a === '--heldout') args.heldout = value();
    else if (a === '--max-gap') args.maxGap = Number(value());
    else if (a === '--family') args.family = value();
    else if (a === '--check') args.check = value();
    else if (a.startsWith('--')) throw new Error(`unknown argument ${a}`);
    else args.files.push(a);
  }
  return args;
}

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
  if (args.command !== 'run') throw new Error('use seed, run, heldout add, compare or clean');
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
    const dataDir = args.data || process.env.COLONIZER_DATA_DIR || join(process.env.HOME ?? '', '.local/share/colonizer');
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
  console.log(`\n${s.passed}/${s.tasks} passed · $${s.total_cost_usd.toFixed(2)} (routed $${s.routed_cost_usd.toFixed(2)}) · ${s.questions} questions · written to ${file}`);
  console.log(`Compare with: node scripts/bench.mjs compare bench-<other>.json ${file}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((e) => {
    console.error(`bench: ${e.message}`);
    process.exit(1);
  });
}
