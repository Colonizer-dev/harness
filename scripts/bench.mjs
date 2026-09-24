#!/usr/bin/env node
// The fixed set of tasks a change to an agent has to survive. Each task is an issue on a scratch
// repository; the bench launches a colony for it through the mothership's own API, answers the questions it
// asks, and scores the pull request against checks the colony never sees. Run it before a change and after,
// and compare. scripts/colony-report.mjs says what happened inside a colony; this says whether it worked.
//
//   node scripts/bench.mjs seed --repo owner/bench-repo     # create the repo, its files and the issues (once)
//   node scripts/bench.mjs run --repo owner/bench-repo --label before
//   node scripts/bench.mjs run --repo owner/bench-repo --label after --only add-helper,readme-typo
//   node scripts/bench.mjs compare bench-before.json bench-after.json
//   node scripts/bench.mjs clean --repo owner/bench-repo    # close the bench's PRs and delete their branches
//
// `run` needs a mothership on COLONIZER_URL (default http://127.0.0.1:7878) with GitHub and an agent
// configured, and `gh` logged in to the account that owns the scratch repository. It costs real model tokens
// and opens real pull requests on that repository, and on nothing else.
import { execFileSync } from 'node:child_process';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { analyze, loadColonies, totalCost } from './colony-report.mjs';
import { auditSession } from './trajectory-monitor.mjs';

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

// A node --test started from inside another one inherits NODE_TEST_CONTEXT, skips its files and exits 0, so
// every check would pass when this runs under the bench's own tests.
const childEnv = () => {
  const { NODE_TEST_CONTEXT, ...env } = process.env;
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

/** Checks out the colony's branch, runs the task's hidden check and the repo's own tests, and diffs it. */
export function scoreBranch({ repo, branch, base = 'main', task, workdir }) {
  const result = { check: false, regression: null, changed: [], outside: [], check_output: '' };
  const dir = workdir ?? mkdtempSync(join(tmpdir(), 'colonizer-bench-score-'));
  try {
    git(['clone', '--quiet', '--depth', '50', `https://github.com/${repo}.git`, dir]);
    git(['fetch', '--quiet', 'origin', branch], dir);
    git(['checkout', '--quiet', 'FETCH_HEAD'], dir);
    result.changed = git(['diff', '--name-only', `origin/${base}...HEAD`], dir).split('\n').filter(Boolean);
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

/** What the colony did, from the mothership's own records, plus whether the work is right. */
export function scoreTask({ task, session, answers, timed_out, branchScore, colony }) {
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
  return {
    tasks: results.length,
    passed: results.filter((r) => r.passed).length,
    cost_usd: sum('cost_usd'),
    routed_cost_usd: sum('routed_cost_usd'),
    total_cost_usd: results.reduce((a, r) => a + (spent(r) ?? 0), 0),
    working_ms: sum('working_ms'),
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
  const rows = ids.map((id) => {
    const b = before.results.find((r) => r.id === id);
    const a = after.results.find((r) => r.id === id);
    const mark = (r) => (r ? (r.passed ? 'pass' : 'FAIL') : '–');
    const cleanMark = (r) => (r?.clean === false ? 'HACKED' : r?.clean === true ? 'clean' : '–');
    return [
      id,
      `${mark(b)} → ${mark(a)}`,
      `${cleanMark(b)} → ${cleanMark(a)}`,
      `${spent(b)?.toFixed(2) ?? '–'} → ${spent(a)?.toFixed(2) ?? '–'} (${delta(spent(a), spent(b))})`,
      `${Math.round((b?.working_ms ?? 0) / 1000)}s → ${Math.round((a?.working_ms ?? 0) / 1000)}s`,
      `${b?.questions ?? '–'} → ${a?.questions ?? '–'}`,
      (a?.failures ?? []).join('; ') || '',
    ];
  });
  const head = ['Task', 'Result', 'Clean', 'Cost', 'Worked', 'Questions', 'Why it failed'];
  const line = (cells) => `| ${cells.join(' | ')} |`;
  const bs = summarizeRun(before.results);
  const as = summarizeRun(after.results);
  const cleanLine = bs.clean_rate == null || as.clean_rate == null
    ? ''
    : ` Clean resolved ${bs.clean_resolved}/${bs.tasks} → ${as.clean_resolved}/${as.tasks} (clean rate ${Math.round(bs.clean_rate * 100)}% → ${Math.round(as.clean_rate * 100)}%, gap ${Math.round(bs.gap * 100)}% → ${Math.round(as.gap * 100)}%).`;
  return [
    `# ${before.label} → ${after.label}`,
    '',
    `Passed ${bs.passed}/${bs.tasks} → ${as.passed}/${as.tasks}. Cost $${bs.total_cost_usd.toFixed(2)} → $${as.total_cost_usd.toFixed(2)} (routed $${bs.routed_cost_usd.toFixed(2)} → $${as.routed_cost_usd.toFixed(2)}). Questions ${bs.questions} → ${as.questions}.${cleanLine}`,
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
  const args = { command: argv[0], repo: null, label: 'run', only: null, timeoutMs: 20 * 60_000, data: null, files: [] };
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
  if (args.command !== 'run') throw new Error('use seed, run, compare or clean');
  if (!args.repo) throw new Error('run needs --repo owner/name');

  const issues = benchIssues(args.repo);
  const tasks = TASKS.tasks.filter((t) => (args.only ? args.only.includes(t.id) : true));
  for (const task of tasks) if (!issues[task.id]) throw new Error(`${args.repo} has no issue for ${task.id}; run seed first`);
  const status = await api('/api/status');
  if (!status.github?.connected) throw new Error('the mothership has no GitHub connection');

  const results = [];
  for (const task of tasks) {
    console.log(`${task.id}: colony on ${args.repo}#${issues[task.id]}`);
    const { session, answers, timed_out } = await runTask(task, args.repo, issues[task.id], args);
    const dataDir = args.data || process.env.COLONIZER_DATA_DIR || join(process.env.HOME ?? '', '.local/share/colonizer');
    const colony = loadColonies(dataDir).find((c) => c.session.id === session.id);
    const branchScore = session.pr_url ? scoreBranch({ repo: args.repo, branch: session.branch, base: session.base ?? 'main', task }) : null;
    const scored = scoreTask({ task, session, answers, timed_out, branchScore, colony: colony ? analyze(colony) : null });
    // Post-hoc: the trajectory monitor audits the same colony's persisted record. A missing log leaves the
    // result unaudited (clean: null), never clean.
    const trajectory = auditSession(dataDir, session.id);
    scored.clean = trajectory ? trajectory.clean : null;
    scored.hacks = trajectory ? trajectory.hits.filter((h) => h.status === 'enforcing').map((h) => h.pattern) : [];
    results.push(scored);
    console.log(`  ${scored.passed ? 'pass' : `FAIL: ${scored.failures.join('; ')}`}${scored.hacks.length > 0 ? ` [hacks: ${scored.hacks.join(', ')}]` : ''}`);
  }

  const run = { label: args.label, repo: args.repo, at: new Date().toISOString(), agent: status.modules?.agent ?? null, results };
  const file = `bench-${args.label}.json`;
  writeFileSync(file, JSON.stringify(run, null, 2));
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
