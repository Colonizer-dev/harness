#!/usr/bin/env node
// External calibration: the same colonies, on work this harness did not choose — real SWE-bench bug
// reports with hidden tests and a hidden gold patch. Each instance becomes a private single-commit
// snapshot of the upstream repo, runs under a budget envelope declared before anything starts, and is
// scored by the official harness outside every colony's reach; until the remaining controls land (#330's
// gold sanity gate, network and trajectory monitoring) every run is labeled uncalibrated — a signal to
// steer by, not a number to publish. Stages: Lite, then Verified, then Multilingual. See docs/bench.md.
//
//   node scripts/swebench.mjs fetch princeton-nlp/SWE-bench_Lite --limit 5 --out lite.json
//   node scripts/swebench.mjs run lite.json --owner my-org --task-budget 2 --total-budget 20 --label lite-before
//   node scripts/swebench.mjs score swebench-lite-before.json --dataset princeton-nlp/SWE-bench_Lite
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { api } from './bench.mjs';
import { analyze, loadColonies, totalCost } from './colony-report.mjs';

const DONE = new Set(['pr_opened', 'no_changes', 'failed', 'stopped', 'merged']);
const ROWS = 'https://datasets-server.huggingface.co/rows';
const CONTROLS = { single_commit_snapshot: true, concealed_eval_artifacts: true, no_network_answer_sources: false, gold_sanity_gate: false, trajectory_monitor: false };
// Git in our own scratch dirs must not inherit the caller's GIT_DIR/GIT_WORK_TREE — a colony shell sets
// both — and the snapshot commit carries its own identity, so nothing depends on the host's git config.
const GIT_ENV = (({ GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, ...env }) => env)(process.env);
export const git = (args, cwd) => execFileSync('git', args, { cwd, encoding: 'utf8', env: GIT_ENV }).trim();
const gh = (args, options = {}) => execFileSync('gh', args, { encoding: 'utf8', ...options }).trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------------------------- fetch

/** FAIL_TO_PASS / PASS_TO_PASS arrive from HF as JSON-encoded strings more often than as arrays. */
function list(value) {
  if (typeof value !== 'string' || !value.trim()) return Array.isArray(value) ? value : [];
  try {
    const parsed = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

/** The fields a run needs, checked at fetch time: anything missing here stops a colony cold later. */
export function normalizeInstance(row) {
  const missing = ['instance_id', 'repo', 'base_commit', 'problem_statement'].filter((k) => !row[k]);
  if (missing.length > 0) throw new Error(`an instance is missing ${missing.join(', ')}: ${JSON.stringify(row).slice(0, 160)}`);
  // fail_to_pass also reads its own output, so a file `fetch` wrote re-fetches intact. The gold and test
  // patches stay in this file for scoring-side checks only; `run` never sends them on.
  const out = { instance_id: row.instance_id, repo: row.repo, base_commit: row.base_commit, problem_statement: row.problem_statement,
    fail_to_pass: list(row.FAIL_TO_PASS ?? row.fail_to_pass), pass_to_pass: list(row.PASS_TO_PASS ?? row.pass_to_pass) };
  for (const key of ['language', 'patch', 'test_patch']) if (row[key]) out[key] = row[key];
  return out;
}

/** A JSON array, a JSONL stream, or the envelope `fetch` writes — always the instances. */
export function parseInstances(text) {
  try {
    const parsed = JSON.parse(text);
    if (Array.isArray(parsed)) return parsed;
    if (Array.isArray(parsed?.instances)) return parsed.instances;
  } catch {
    const lines = text.split('\n').filter((l) => l.trim());
    if (lines.length > 0) return lines.map((l) => JSON.parse(l));
  }
  throw new Error('instances must be a JSON array, JSONL, or what fetch wrote');
}

/** The suite an instances file speaks for, when `fetch` wrote it; null when it does not say. */
export const parseSuite = (text) => { try { return JSON.parse(text)?.suite ?? null; } catch { return null; } };

/** The HF datasets-server rows API, 100 rows at a time, until the limit or the end of the split. */
async function fetchRows(dataset, { config, split, limit, offset }) {
  const rows = [];
  for (;;) {
    const length = Math.min(100, limit == null ? 100 : limit - rows.length);
    if (length <= 0) break;
    const url = `${ROWS}?dataset=${encodeURIComponent(dataset)}&config=${encodeURIComponent(config)}&split=${encodeURIComponent(split)}&offset=${offset + rows.length}&length=${length}`;
    const res = await fetch(url);
    if (!res.ok) throw new Error(`datasets-server said ${res.status} ${await res.text()}`.slice(0, 300));
    const page = await res.json();
    rows.push(...(page.rows ?? []).map((r) => r.row));
    if ((page.rows?.length ?? 0) < length || rows.length >= (page.num_rows_total ?? Infinity)) break;
  }
  return rows;
}

/** The instances a fetch keeps: `--ids` names exact ones, else a window of the split. */
export function selectInstances(instances, { ids, offset = 0, limit = null, paged = false, dataset = '' }) {
  if (ids) {
    const kept = instances.filter((i) => ids.includes(i.instance_id));
    for (const id of ids) if (!kept.some((i) => i.instance_id === id)) throw new Error(`${dataset || 'the dataset'} has no instance ${id}`);
    return kept;
  }
  // The rows API already windowed with the offset and limit; a local file is windowed here.
  if (paged) return limit == null ? instances : instances.slice(0, limit);
  return instances.slice(offset, limit == null ? undefined : offset + limit);
}

async function fetchDataset(args) {
  const local = /\.(json|jsonl)$/.test(args.dataset);
  const suite = { dataset: args.dataset, split: args.split, config: args.config, revision: null };
  const text = local ? readFileSync(args.dataset, 'utf8') : null;
  const rows = local ? parseInstances(text) : await fetchRows(args.dataset, args);
  if (local) Object.assign(suite, parseSuite(text) ?? {});
  const instances = selectInstances(rows.map(normalizeInstance), { ...args, paged: !local, dataset: args.dataset });
  writeFileSync(args.out, `${JSON.stringify({ suite, instances }, null, 2)}\n`);
  console.log(`${instances.length} instances -> ${args.out}`);
  if (instances.length <= 20) for (const i of instances) console.log(`  ${i.instance_id}${i.language ? ` (${i.language})` : ''}`);
}

// ----------------------------------------------------------------------------------------------- run

/** instance_id -> a scratch repository name: GitHub-safe, and recognizably ours. */
export const scratchName = (instanceId) =>
  `swebench-${String(instanceId).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 80)}`;

/**
 * The anti-hacking control: a single-commit reconstruction of upstream@baseCommit in a fresh directory —
 * no upstream history (so no future commit holding the fix), no remote, no eval tests, no gold patch.
 */
export function buildSnapshot({ upstream, baseCommit, workdir }) {
  const dir = workdir ?? mkdtempSync(join(tmpdir(), 'colonizer-swebench-'));
  git(['init', '-q', '-b', 'main'], dir);
  git(['fetch', '--quiet', '--depth', '1', upstream, baseCommit], dir);
  git(['checkout', '--quiet', '--force', 'FETCH_HEAD'], dir);
  rmSync(join(dir, '.git'), { recursive: true, force: true });
  git(['init', '-q', '-b', 'main'], dir);
  git(['add', '-A'], dir);
  git(['-c', 'commit.gpgsign=false', '-c', 'user.name=colonizer', '-c', 'user.email=swebench@colonizer.dev',
    'commit', '-q', '-m', `Snapshot of ${baseCommit}`], dir);
  return { dir, tree: git(['rev-parse', 'HEAD^{tree}'], dir) };
}

/** Created private and pushed once, or reused when main still holds this snapshot's tree. */
function pushScratch(owner, name, dir, tree) {
  const full = `${owner}/${name}`;
  try {
    gh(['repo', 'view', full, '--json', 'name']);
  } catch {
    gh(['repo', 'create', full, '--private', '--source', dir, '--push', '--description', 'A single-commit SWE-bench snapshot; safe to delete']);
    return;
  }
  // The tree, not the commit sha: rebuilding the snapshot mints a new commit for the same content.
  const branch = JSON.parse(gh(['api', `repos/${full}/branches/main`]));
  if (branch?.commit?.commit?.tree?.sha !== tree) throw new Error(`${full}'s main is not this snapshot; delete it (gh repo delete ${full} --yes) or use another --owner`);
  console.log(`  ${full} already holds this snapshot`);
}

/** Whether one more task fits the envelope: a task never starts that cannot be paid for. */
export function budgetGate(spent, caps) {
  const over = spent + caps.task_usd > caps.total_usd;
  return { proceed: !over, reason: over ? `spent $${spent.toFixed(2)} plus the $${caps.task_usd.toFixed(2)} task cap would pass the $${caps.total_usd.toFixed(2)} envelope` : null };
}

/** One colony on the snapshot, stopped at the time limit or the task cap, whichever bites first. */
async function runColony(instance, scratch, options) {
  const started = Date.now();
  const body = { repo: scratch, title: instance.instance_id, instructions: instance.problem_statement, autopilot: true,
    ...(options.modelTier && { model_tier: options.modelTier }), ...(options.model && { model_override: options.model }),
    ...(options.subagentModel && { subagent_model_override: options.subagentModel }) };
  const session = await api('/api/sessions', { method: 'POST', body: JSON.stringify(body) });
  let current = session;
  let stopped = null;
  while (!DONE.has(current.status)) {
    await sleep(5000);
    current = await api(`/api/sessions/${session.id}`);
    process.stdout.write(`\r  ${instance.instance_id}: ${current.status} (${Math.round((Date.now() - started) / 1000)}s)   `);
    if (DONE.has(current.status)) break;
    // The session's own live cost is the cap's clock; the recorded one comes later, from the colony report.
    const spentUsd = (current.cost_usd ?? 0) + (current.routed_cost_usd ?? 0);
    const outOfTime = Date.now() - started >= options.timeoutMs;
    if (stopped == null && (outOfTime || spentUsd >= options.taskUsd)) {
      stopped = outOfTime ? 'time' : 'task-budget';
      console.log(`\n  stopping the colony: ${stopped === 'time' ? 'out of time' : `over the $${options.taskUsd} task cap`}`);
      await api(`/api/sessions/${session.id}/stop`, { method: 'POST' }).catch(() => {});
    }
    // A stop that never lands must not hang the whole run: give the teardown a quarter hour.
    if (stopped != null && Date.now() - started >= options.timeoutMs + 15 * 60_000) break;
  }
  process.stdout.write('\n');
  return { session: current, stopped, wallMs: Date.now() - started };
}

/** The colony's whole diff from the snapshot — whose tree is upstream's base_commit — as SWE-bench wants it. */
function extractPatch(scratch, branch) {
  if (!branch) return '';
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-swebench-patch-'));
  try {
    git(['clone', '--quiet', '--depth', '50', `https://github.com/${scratch}.git`, dir]);
    git(['fetch', '--quiet', 'origin', branch], dir);
    // origin/main is the pushed snapshot even when the scratch repo is reused (same tree, older commit).
    return git(['diff', 'origin/main...FETCH_HEAD'], dir);
  } catch (e) {
    console.error(`  no patch to extract: ${e.message}`);
    return '';
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The files a patch touches, from its `diff --git` headers. */
export const diffFiles = (patch) =>
  [...String(patch).matchAll(/^diff --git a\/(.+) b\/(.+)$/gm)].map((m) => (m[2] === m[1] ? m[1] : m[2]));

/** The honest-signal flags: a patch that edits the hidden eval tests, or no patch at all. */
export function patchFlags(instance, patch) {
  const flags = [];
  if (!String(patch).trim()) flags.push('empty');
  const evalTests = new Set(diffFiles(instance.test_patch ?? ''));
  if (evalTests.size > 0 && diffFiles(patch).some((f) => evalTests.has(f))) flags.push('touches-eval-tests');
  return flags;
}

/** How a task ended. A stopped colony that still pushed a patch is `patched` — it gets scored either way. */
const outcomeOf = ({ status, patch, stopped }) => (String(patch).trim() ? 'patched' : stopped ? 'timeout' : status === 'failed' ? 'failed' : 'no-patch');

/** One line of a SWE-bench predictions file. */
export const toPrediction = (instance, patch, modelName = 'colonizer') => ({
  instance_id: instance.instance_id, model_name_or_path: modelName, model_patch: String(patch ?? ''),
});

/** The task's real spend: the colony report when the mothership still has it, else the session's own count. */
function colonyCost(dataDir, session) {
  const colony = loadColonies(dataDir).find((c) => c.session.id === session.id);
  const a = colony ? analyze(colony) : null;
  const cost = a?.cost_usd ?? session.cost_usd ?? null;
  const routed = a?.routed_cost_usd ?? session.routed_cost_usd ?? null;
  return { cost_usd: cost, routed_cost_usd: routed, total: totalCost(cost, routed) };
}

/** Every control has to hold before a run may be called calibrated. */
export const isCalibrated = (controls) => Object.values(controls ?? {}).every(Boolean);

/** The task record before anything happened to it. */
const blankTask = (instance) => ({ instance_id: instance.instance_id, repo: instance.repo, language: instance.language ?? null,
  session_id: null, status: null, branch: null, cost_usd: null, routed_cost_usd: null, wall_ms: null, flags: [], outcome: null });

async function runInstances(args) {
  const text = readFileSync(args.file, 'utf8');
  const instances = parseInstances(text).map(normalizeInstance);
  const status = await api('/api/status');
  if (!status.github?.connected) throw new Error('the mothership has no GitHub connection');
  const caps = { task_usd: args.taskBudget, total_usd: args.totalBudget };
  const dataDir = process.env.COLONIZER_DATA_DIR || join(homedir(), '.local/share/colonizer');
  const run = {
    suite: parseSuite(text) ?? { dataset: args.file, split: null, config: null, revision: null },
    label: args.label, at: new Date().toISOString(),
    colony: { model_tier: args.modelTier ?? null, model: args.model ?? null, subagent_model: args.subagentModel ?? null, agent: status.modules?.agent ?? null },
    budget: { ...caps, spent_usd: 0, stopped_at_cap: false }, controls: CONTROLS, calibrated: isCalibrated(CONTROLS),
    tasks: [], summary: null,
  };
  const predictions = [];
  let spent = 0;
  let skipping = false;
  for (const instance of instances) {
    const gate = budgetGate(spent, caps);
    if (!gate.proceed && !skipping) {
      skipping = true;
      run.budget.stopped_at_cap = true;
      console.log(`\nstopping at the envelope: ${gate.reason}`);
    }
    if (skipping) {
      run.tasks.push({ ...blankTask(instance), outcome: 'skipped-budget' });
      continue;
    }

    console.log(`\n${instance.instance_id}: snapshot of ${instance.repo}@${instance.base_commit.slice(0, 10)}`);
    const name = scratchName(instance.instance_id);
    const scratch = `${args.owner}/${name}`;
    const snap = buildSnapshot({ upstream: `https://github.com/${instance.repo}.git`, baseCommit: instance.base_commit });
    try {
      pushScratch(args.owner, name, snap.dir, snap.tree);
      const { session, stopped, wallMs } = await runColony(instance, scratch, {
        timeoutMs: args.timeoutMin * 60_000, taskUsd: caps.task_usd, modelTier: args.modelTier, model: args.model, subagentModel: args.subagentModel,
      });
      const cost = colonyCost(dataDir, session);
      const patch = extractPatch(scratch, session.branch);
      const task = {
        ...blankTask(instance), session_id: session.id, status: session.status, branch: session.branch ?? null,
        cost_usd: cost.cost_usd, routed_cost_usd: cost.routed_cost_usd, wall_ms: wallMs, flags: patchFlags(instance, patch),
        outcome: outcomeOf({ status: session.status, patch, stopped }), stopped, scratch,
      };
      run.tasks.push(task);
      predictions.push(toPrediction(instance, patch));
      spent += cost.total ?? 0;
      console.log(`  ${task.outcome} (${task.flags.join(', ') || 'no flags'}) · $${(cost.total ?? 0).toFixed(2)}`);
    } finally {
      rmSync(snap.dir, { recursive: true, force: true });
    }
  }

  run.budget.spent_usd = Number(spent.toFixed(2));
  run.summary = summarize(run);
  writeFileSync(`swebench-${args.label}.json`, `${JSON.stringify(run, null, 2)}\n`);
  writeFileSync(`swebench-${args.label}.predictions.jsonl`, predictions.map((p) => JSON.stringify(p)).join('\n') + (predictions.length ? '\n' : ''));
  console.log(`\n${formatSummary(run)}`);
  console.log(`written to swebench-${args.label}.json; score it with: node scripts/swebench.mjs score swebench-${args.label}.json --dataset <hf-id-or-path>`);
}

// ---------------------------------------------------------------------------------------------- score

/** Folds the official harness's report into the run: each attempted task learns whether it resolved. */
export function applyReport(run, report) {
  const resolved = new Set(report.resolved_ids ?? []);
  const errors = new Set(report.error_ids ?? []);
  for (const task of run.tasks) {
    // A task the harness itself errored on is unknown, not unresolved.
    task.resolved = task.outcome === 'skipped-budget' || errors.has(task.instance_id) ? null : resolved.has(task.instance_id);
    if (errors.has(task.instance_id)) task.harness_error = true;
  }
  return run;
}

/** The counts that matter. Skipped tasks are not failures: they are reported, not extrapolated. */
export function summarize(run) {
  const attempted = run.tasks.filter((t) => t.outcome !== 'skipped-budget');
  const clean = attempted.filter((t) => (t.flags ?? []).length === 0);
  const resolvedCount = (tasks) => tasks.filter((t) => t.resolved === true).length;
  // A rate exists only over tasks the harness actually judged; before that, null reads as "not scored yet".
  const rate = (tasks) => {
    const known = tasks.filter((t) => t.resolved != null);
    return known.length ? resolvedCount(known) / known.length : null;
  };
  const byLanguage = {};
  for (const task of attempted) {
    const entry = (byLanguage[task.language ?? 'unknown'] ??= { attempted: 0, resolved: 0 });
    entry.attempted += 1;
    if (task.resolved === true) entry.resolved += 1;
  }
  return {
    attempted: attempted.length, skipped_budget: run.tasks.length - attempted.length,
    resolved: resolvedCount(attempted), raw_resolved_rate: rate(attempted),
    flagged: attempted.length - clean.length, clean_resolved_rate: rate(clean),
    harness_errors: attempted.filter((t) => t.harness_error).length, by_language: byLanguage,
    cost_usd: Number(run.tasks.reduce((a, t) => a + (Number(t.cost_usd) || 0) + (Number(t.routed_cost_usd) || 0), 0).toFixed(2)),
  };
}

/** The short honest read: what ran, what resolved, and that none of it is calibrated yet. */
export function formatSummary(run) {
  const s = run.summary ?? summarize(run);
  const pct = (rate) => (rate == null ? 'not scored yet' : `${(rate * 100).toFixed(1)}%`);
  const missing = Object.entries(run.controls ?? {}).filter(([, holds]) => !holds).map(([name]) => name);
  return [
    `${run.label}: ${s.attempted} attempted, ${s.skipped_budget} skipped at the budget envelope`,
    `resolved (raw) ${s.resolved}/${s.attempted} = ${pct(s.raw_resolved_rate)} · resolved (clean, ${s.flagged} flagged excluded) = ${pct(s.clean_resolved_rate)}`,
    ...(s.harness_errors ? [`${s.harness_errors} task(s) the harness itself errored on are in neither rate`] : []),
    ...Object.entries(s.by_language).sort().map(([language, v]) => `  ${language}: ${v.resolved}/${v.attempted} resolved`),
    `cost $${s.cost_usd.toFixed(2)} against the $${run.budget.total_usd.toFixed(2)} envelope (task cap $${run.budget.task_usd.toFixed(2)})${run.budget.stopped_at_cap ? ', stopped at the cap' : ''}`,
    run.calibrated ? 'calibrated: every control held' : `uncalibrated — ${missing.join(', ')} not enforced; not comparable to published SWE-bench numbers`,
  ].join('\n');
}

/** The official harness, run here — an environment no colony touched — over the predictions it never saw. */
function runHarness(run, predictionsPath, dataset) {
  const first = readFileSync(predictionsPath, 'utf8').split('\n').find((l) => l.trim());
  if (!first) throw new Error(`${predictionsPath} is empty; nothing to score`);
  const ids = run.tasks.filter((t) => t.outcome !== 'skipped-budget').map((t) => t.instance_id);
  if (ids.length === 0) throw new Error(`${run.label} attempted nothing, so there is nothing to score`);
  const reportDir = join('logs', 'run_evaluation');
  // --instance_ids is nargs="+": one argument per id, never a comma-joined string.
  execFileSync('python', ['-m', 'swebench.harness.run_evaluation', '--dataset_name', dataset, '--predictions_path', predictionsPath,
    '--run_id', run.label, '--report_dir', reportDir, '--instance_ids', ...ids], { stdio: 'inherit' });
  // The harness names its report <model_name_or_path, / as __>.<run_id>.json inside --report_dir.
  return JSON.parse(readFileSync(join(reportDir, `${JSON.parse(first).model_name_or_path.replace(/\//g, '__')}.${run.label}.json`), 'utf8'));
}

function scoreRun(args) {
  const run = JSON.parse(readFileSync(args.file, 'utf8'));
  const predictions = args.file.replace(/\.json$/, '.predictions.jsonl');
  applyReport(run, args.report ? JSON.parse(readFileSync(args.report, 'utf8')) : runHarness(run, predictions, args.dataset));
  run.summary = summarize(run);
  writeFileSync(args.file, `${JSON.stringify(run, null, 2)}\n`);
  console.log(formatSummary(run));
}

// -------------------------------------------------------------------------------------------- command

function parseArgs(argv) {
  const args = {
    command: argv[0], files: [], dataset: null, split: 'test', config: 'default', limit: null, offset: 0,
    ids: null, out: null, owner: null, taskBudget: null, totalBudget: null, label: 'run', timeoutMin: 60,
    modelTier: null, model: null, subagentModel: null, report: null,
  };
  const strings = { '--dataset': 'dataset', '--split': 'split', '--config': 'config', '--out': 'out', '--owner': 'owner', '--label': 'label',
    '--model-tier': 'modelTier', '--model': 'model', '--subagent-model': 'subagentModel', '--report': 'report' };
  const numbers = { '--limit': 'limit', '--offset': 'offset', '--task-budget': 'taskBudget', '--total-budget': 'totalBudget', '--timeout-min': 'timeoutMin' };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    const value = () => argv[++i];
    if (strings[a]) args[strings[a]] = value();
    else if (numbers[a]) args[numbers[a]] = Number(value());
    else if (a === '--ids') args.ids = value().split(',').map((s) => s.trim());
    else if (a.startsWith('--')) throw new Error(`unknown argument ${a}`);
    else args.files.push(a);
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const [file] = args.files;
  if (args.command === 'fetch') {
    if (!file || !args.out) throw new Error('fetch needs <dataset> and --out instances.json');
    await fetchDataset({ ...args, dataset: file });
  } else if (args.command === 'run') {
    if (!file || !args.owner) throw new Error('run needs <instances.json> and --owner <github-owner>');
    if (!(args.taskBudget > 0) || !(args.totalBudget > 0)) {
      throw new Error('run needs --task-budget <usd> and --total-budget <usd>: the envelope is declared before anything starts');
    }
    await runInstances({ ...args, file });
  } else if (args.command === 'score') {
    if (!file || !args.dataset) throw new Error('score needs <run.json>, --dataset <hf-id-or-path> and either Docker or --report');
    scoreRun({ ...args, file });
  } else {
    throw new Error('use fetch, run or score');
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((e) => {
    console.error(`swebench: ${e.message}`);
    process.exit(1);
  });
}
