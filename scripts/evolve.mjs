#!/usr/bin/env node
// The offline evolver loop (issue #310): diagnose why bench tasks and red-team hunts fail, hold a
// failure class still, propose one prompt change for it, and keep it only when a rerun of the bench
// beats the baseline. It never launches colonies, never calls the mothership's API, never edits
// repository files and never opens anything anywhere: it reads logs, transcripts and reports, and
// writes proposal files into one output directory.
//
//   node scripts/evolve.mjs diagnose --bench bench-after.json --findings <data>/sessions/<id>/findings.jsonl --out diagnosis.json
//   node scripts/evolve.mjs propose --classes diagnosis.json --class module:claude-code/questions --module claude-code --constant SYSTEM_PROMPT_APPEND --text new-prompt.txt
//   node scripts/evolve.mjs evaluate --proposal <out>/<id>.md --baseline bench-before.json --candidate bench-after.json
//   node scripts/evolve.mjs retain --proposal <out>/<id>.md
//   node scripts/evolve.mjs list
//   node scripts/evolve.mjs approve <id>      # prints the `git apply` for an ordinary, human-reviewed PR
//   node scripts/evolve.mjs reject <id> --reason "…"
//
// Proposals are prompt-only: propose replaces the value of one exported `*_PROMPT`/`*_PROMPT_APPEND`
// string constant in `modules/agents/<module>/` and nothing else; the replacement text itself is the
// operator's to write (or a model they run's) — diagnose's classes are its brief, not its output.
// Nothing merges by machine: retain only stamps a verdict, and an approved proposal becomes a pull
// request by hand. Default output directory is `<data dir>/evolver/proposals`.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { basename, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { TOKEN_CATEGORIES, analyze, loadColonies, readJsonLines, totalCost } from './colony-report.mjs';
import { childEnv, summarizeRun } from './bench.mjs';
import { compareProposal } from './trajectory-monitor.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dataDir = () => process.env.COLONIZER_DATA_DIR || join(homedir(), '.local/share/colonizer');
const defaultOut = () => join(dataDir(), 'evolver/proposals');

// Scoring and diffing run git against operator-supplied paths; bench.mjs's childEnv keeps a colony
// sandbox's GIT_DIR and friends, and a test runner's NODE_TEST_CONTEXT, out of the child.
const git = (args, cwd) => execFileSync('git', args, { cwd, encoding: 'utf8', env: childEnv() }).trim();

// ----------------------------------------------------------------------------------------------- diagnose

/** The failure kinds evolve names, one per bench failure string scoreTask can produce, plus the two
 *  defect sources and a fallback for text nobody has seen yet. */
const FAILURE_KINDS = [
  [/asked \d+ questions?, expected/, 'questions'],
  [/changed files outside the task/, 'scope'],
  [/the check failed/, 'hidden-check'],
  [/broke the existing tests/, 'tests'],
  [/no pull request/, 'no-pr'],
  [/timed out/, 'timeout'],
];
const kindOf = (failure) => FAILURE_KINDS.find(([re]) => re.test(String(failure)))?.[1] ?? 'other';

const moduleOf = (file) => (/^modules\/agents\/([^/]+)\//.exec(String(file)) ?? [])[1] ?? null;

/** Folds a findings ledger (protocol §6.6: append-only, one line per stage) into one record per
 *  title, keeping only findings that reached a validated, filed or merged state and the best of
 *  those states. Rejected and blocked lines stay out: the evolver diagnoses confirmed defects. */
const LEDGER_STATES = new Map([['validated', 1], ['filed', 2], ['merged', 3]]);
function foldFindings(lines) {
  const byTitle = new Map();
  for (const l of lines) {
    if (!LEDGER_STATES.has(l.state)) continue;
    const cur = byTitle.get(l.title);
    const best = cur && LEDGER_STATES.get(cur.state) >= LEDGER_STATES.get(l.state) ? cur : { ...cur, ...l };
    byTitle.set(l.title, { ...best, sessions: [...new Set([...(cur?.sessions ?? []), l.session].filter(Boolean))] });
  }
  return [...byTitle.values()];
}

/** Clusters confirmed failures into stable classes. Pure: everything is read before this runs. `bench`
 *  is an array of run records (`results[]`), `redteam` the lines of synthesis reports (protocol
 *  §6.7, `validated` lines only), `findings` the lines of findings.jsonl ledgers (§6.6), and
 *  `sessions` an optional `{session id: {questions, tool_errors}}` map from the colony transcripts,
 *  joined into bench evidence when given. */
export function diagnose({ bench = [], redteam = [], findings = [], sessions = null } = {}) {
  const classes = new Map();
  const add = (surface, kind, evidence) => {
    const id = `${surface}/${kind}`;
    const c = classes.get(id) ?? { id, surface, kind, count: 0, evidence: [] };
    classes.set(id, { ...c, count: c.count + 1, evidence: [...c.evidence, evidence] });
  };
  for (const run of bench) {
    for (const r of run.results ?? []) {
      const surface = `module:${r.agent || 'unknown'}`;
      const evidence = { source: 'bench', label: run.label ?? null, task: r.id, session: r.session_id ?? null, ...(sessions?.[r.session_id] ?? {}) };
      for (const f of r.failures ?? []) add(surface, kindOf(f), evidence);
      if ((r.hacks ?? []).length > 0 || r.clean === false) add(surface, 'reward-hack', evidence);
    }
  }
  for (const d of redteam) {
    if (d.validation !== 'validated') continue; // synthesis report lines the validator did not uphold
    const modules = [...new Set((d.files ?? []).map(moduleOf).filter(Boolean))].sort();
    const rest = (d.files ?? []).filter((f) => !moduleOf(f)).sort();
    // One class per defect: the module it lands in when the files name exactly one, a joined surface
    // when they span several, else the first non-module file, else nowhere in particular.
    const surface = modules.length === 1 ? `module:${modules[0]}` : modules.length > 1 ? `module:${modules.join('+')}` : rest.length > 0 ? `repo:${rest[0]}` : 'repo:unassigned';
    add(surface, 'redteam', { source: 'redteam', defect: d.defect, sessions: d.hunters ?? [], severity: d.severity ?? null, reproduction: d.reproduction ?? null });
  }
  for (const f of foldFindings(findings)) {
    add('repo:unassigned', 'defect', { source: 'findings', title: f.title, sessions: f.sessions, severity: f.severity ?? null, state: f.state });
  }
  const order = (e) => `${e.source}\u0000${e.task ?? e.title ?? e.defect ?? ''}`;
  return {
    repo: bench.find((r) => r.repo)?.repo ?? null,
    heldout_version: bench.map((r) => r.heldout?.version).find((v) => v != null) ?? null,
    classes: [...classes.values()]
      .map((c) => ({ ...c, evidence: [...c.evidence].sort((a, b) => order(a).localeCompare(order(b))) }))
      .sort((a, b) => b.count - a.count || a.id.localeCompare(b.id)),
  };
}

// ------------------------------------------------------------------------------------------------ propose

/** The value of one prompt constant, as source text in the module's own style: an array of one
 *  single-quoted line per output line, joined — the shape SYSTEM_PROMPT_APPEND and its siblings use. */
function promptLiteral(text) {
  if (text === '') return "''";
  const lines = text.split('\n').map((l) => `  '${l.replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`);
  return `[\n${lines.join(',\n')},\n].join('\\n')`;
}

/** The source with one exported string constant's whole statement replaced by `text`. The scanner
 *  counts brackets outside string literals only, so a prompt line containing `(or Agent)` cannot end
 *  the statement early; it stops at the first depth-zero `;` after the declaration. */
export function replaceConstant(source, name, text) {
  const decl = new RegExp(`^export const ${name}\\b`, 'm').exec(source);
  if (!decl) throw new Error(`${name} is not declared in this file`);
  let quote = null;
  let depth = 0;
  for (let j = source.indexOf('=', decl.index); j < source.length; j++) {
    const ch = source[j];
    if (quote) {
      if (ch === '\\') j += 1;
      else if (ch === quote) quote = null;
    } else if (ch === "'" || ch === '"' || ch === '`') quote = ch;
    else if (ch === '(' || ch === '[' || ch === '{') depth += 1;
    else if (ch === ')' || ch === ']' || ch === '}') depth -= 1;
    else if (ch === ';' && depth === 0) return `${source.slice(0, decl.index)}export const ${name} = ${promptLiteral(text)};${source.slice(j + 1)}`;
  }
  throw new Error(`could not find the end of the ${name} statement`);
}

/** Every .mjs file under `dir` declaring `export const <name>`. */
function filesDeclaring(dir, name) {
  const hits = [];
  const walk = (d) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith('.mjs') && new RegExp(`^export const ${name}\\b`, 'm').test(readFileSync(p, 'utf8'))) hits.push(p);
    }
  };
  walk(dir);
  return hits;
}

/** A unified diff of one file, computed without touching the working tree: before and after land in
 *  a temp directory, `git diff --no-index` runs there, and the header paths are rewritten to
 *  repo-relative a/ b/ paths. Index lines are dropped, so the patch applies by context alone. */
function diffFor(rel, before, after) {
  if (before === after) throw new Error('the replacement text equals the constant as it stands; nothing to propose');
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-evolve-'));
  try {
    for (const side of ['old', 'new']) mkdirSync(join(dir, side, dirname(rel)), { recursive: true });
    writeFileSync(join(dir, 'old', rel), before);
    writeFileSync(join(dir, 'new', rel), after);
    let raw;
    try {
      raw = git(['diff', '--no-index', '--no-ext-diff', join('old', rel), join('new', rel)], dir);
    } catch (e) {
      raw = String(e.stdout ?? ''); // --no-index exits 1 when the files differ, which is the point
    }
    return raw.split('\n').filter((l) => !l.startsWith('index ')).map((l) => l.split(join('old', rel)).join(rel).split(join('new', rel)).join(rel)).join('\n');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const slug = (id) => id.replace(/[^A-Za-z0-9._-]+/g, '-');

/** Builds a prompt-only proposal for one failure class. Reads the module file under `root`, computes
 *  the diff, and returns the proposal record; the caller writes it. */
export function propose({ classes, classId, module, constant, text, root = ROOT }) {
  if (!/^[A-Z][A-Z0-9_]*$/.test(constant) || !/(_PROMPT_APPEND|_PROMPT)$/.test(constant)) {
    throw new Error(`${constant} is not a prompt constant; only names ending in _PROMPT or _PROMPT_APPEND may be proposed against`);
  }
  const cls = (classes.classes ?? []).find((c) => c.id === classId);
  if (!cls) throw new Error(`${classId} is not in the diagnosis (have: ${(classes.classes ?? []).map((c) => c.id).join(', ') || 'nothing'})`);
  const moduleDir = join(root, 'modules/agents', module);
  if (!existsSync(moduleDir)) throw new Error(`no agent module at ${moduleDir}`);
  const hits = filesDeclaring(moduleDir, constant);
  if (hits.length === 0) throw new Error(`no file under modules/agents/${module}/ declares export const ${constant}`);
  if (hits.length > 1) throw new Error(`${constant} is declared in ${hits.length} files (${hits.map((h) => relative(root, h)).join(', ')}); say which module you mean`);
  const file = relative(root, hits[0]);
  const before = readFileSync(hits[0], 'utf8');
  const tasks = [...new Set(cls.evidence.filter((e) => e.source === 'bench' && e.task).map((e) => e.task))].sort();
  const repo = classes.repo ?? null;
  const base = git(['rev-parse', 'HEAD'], root);
  const id = slug(classId);
  const benchRun = (label) => `node scripts/bench.mjs run --repo ${repo ?? '<owner/bench>'} --label ${label} --only ${tasks.join(',') || '<task ids>'}`;
  return {
    id, class: classId, kind: cls.kind, surface: cls.surface, status: 'proposed', module, file, constant,
    base_commit: base, repo, tasks, heldout_version: classes.heldout_version ?? null,
    created_at: new Date().toISOString(), evidence: cls.evidence, text,
    diff: diffFor(file, before, replaceConstant(before, constant, text)),
    reproduce: [`git checkout ${base}`, benchRun('baseline'), `git apply <out>/${id}.diff`, benchRun('candidate'), `git checkout ${base} -- ${file}`],
    evaluations: [], verdict: null, approved_at: null, rejected_at: null, reject_reason: null,
    review: [
      'docs updated for the changed prompt (docs/runner-authoring.md, the module README)?',
      'pins affected (modules/agents/*/package.json, vendored plugins, runtime pins)?',
      'audit: does the new text promise the colony anything the no-write policy forbids — committing, pushing or filing issues itself (docs/audit.md, "No-write policy, issue #84")?',
      'bench gain re-confirmed by hand on a second candidate run?',
    ],
  };
}

// ----------------------------------------------------------------------------------------------- proposal

/** A code fence longer than any run of backticks inside `s`, so the block cannot close itself early. */
const fenceFor = (s) => '`'.repeat(Math.max(3, (String(s).match(/`+/g) ?? []).reduce((m, r) => Math.max(m, r.length), 0) + 1));
const usd = (x) => (typeof x === 'number' ? `$${x.toFixed(2)}` : '–');
const pctOf = (x) => (Number.isFinite(x) ? `${Math.round(x * 100)}%` : '∞%');
const verdictMark = (r) => (!r?.passed ? 'FAIL' : r.clean === false ? 'hack' : 'pass');
const table = (headers, rows) => {
  const line = (cells) => `| ${cells.join(' | ')} |`;
  return [line(headers), line(headers.map(() => '---')), ...rows.map((row) => line(row))].join('\n');
};

/** The proposal as reviewable Markdown. Everything machine-read lives in the trailing ```json block,
 *  fenced longer than any backtick run inside it, so a diff containing ``` cannot break parsing. */
export function renderProposal(p) {
  const json = JSON.stringify(p, null, 2);
  const diffFence = fenceFor(p.diff ?? '');
  const evidence = p.evidence.map((e) =>
    e.source === 'bench'
      ? `- bench ${e.label ?? '–'}, task \`${e.task}\`, session \`${e.session ?? '–'}\`${e.questions != null ? `, ${e.questions} questions` : ''}${e.tool_errors != null ? `, ${e.tool_errors} tool errors` : ''}`
      : e.source === 'redteam'
        ? `- red-team defect “${e.defect}” (${e.severity ?? '–'}, ${e.reproduction ?? '–'}), hunters ${(e.sessions ?? []).join(', ') || '–'}`
        : `- finding “${e.title}” (${e.state}${e.severity ? `, ${e.severity}` : ''}), sessions ${(e.sessions ?? []).join(', ') || '–'}`);
  const card = (e) => {
    const c = e.card;
    const row = (t) => [t.id, verdictMark(t.base_result), verdictMark(t.cand_result), `${t.delta >= 0 ? '+' : ''}${t.delta}`, `${usd(t.base_cost)} → ${usd(t.cand_cost)}`, usd(t.cost_delta)];
    const n = c.tasks.length;
    const tokens = TOKEN_CATEGORIES.filter((k) => c.totals.token_delta[k] !== 0).map((k) => `${k} ${c.totals.token_delta[k] >= 0 ? '+' : ''}${c.totals.token_delta[k]}`).join(', ') || 'no change';
    return [
      `### ${e.candidate}`, '',
      table(['Task', 'Baseline', 'Candidate', 'Δ', 'Cost', 'Δ'], c.tasks.map(row)), '',
      `Score (passed and not hacked) ${c.passed.baseline}/${n} → ${c.passed.candidate}/${n}. Cost ${usd(c.totals.baseline_cost_usd)} → ${usd(c.totals.candidate_cost_usd)} (${usd(c.totals.cost_delta)}, ${pctOf(c.totals.cost_pct)}). Tokens: ${tokens}.`,
      ...(c.regressions.length ? ['', `Regressed from pass to fail: ${c.regressions.join(', ')}.`] : []),
    ];
  };
  return [
    `# Proposal ${p.id} — ${p.status}`, '',
    `Class \`${p.class}\` (kind \`${p.kind}\`, surface \`${p.surface}\`).`, '',
    '## Claim', '', ...evidence, '',
    '## Diff', '', `${diffFence}diff`, p.diff ?? '', diffFence, '',
    '## Reproduce', '', '```', ...p.reproduce, '```', '',
    `- base commit: \`${p.base_commit}\``,
    `- tasks: ${p.tasks.join(', ') || 'none recorded — pick the tasks this class came from'}`,
    `- held-out set version: ${p.heldout_version != null ? `v${p.heldout_version}` : 'none recorded'}`,
    '',
    '## Scorecard', '',
    ...(p.evaluations.length ? p.evaluations.flatMap((e) => [...card(e), '']) : ['Not evaluated yet. Run the baseline and the candidate on the same task ids, then `evolve.mjs evaluate`.']),
    '## Verdict', '',
    ...(p.verdict
      ? [`${p.verdict.verdict.toUpperCase()} — ${p.verdict.reasons.join('; ')}`, ...(p.verdict.runs ?? []).length > 1 ? ['', p.verdict.runs.map((r) => `${r.candidate}: ${r.verdict}`).join(' · ')] : []]
      : ['No verdict yet. Run `evolve.mjs retain`.']),
    '',
    '## Review checklist', '',
    ...p.review.map((item) => `- [ ] ${item}`),
    '',
    `${fenceFor(json)}json`, json, fenceFor(json), '',
  ].join('\n');
}

/** The machine record back out of a rendered proposal: the trailing ```json block, last in the file. */
export function parseProposal(text) {
  const lines = text.split('\n');
  for (let i = lines.length - 1; i >= 0; i--) {
    const m = /^(`{3,})json$/.exec(lines[i]);
    if (!m) continue;
    const end = lines.findIndex((l, j) => j > i && /^`+$/.test(l) && l.length >= m[1].length);
    if (end === -1) throw new Error('the json block is not closed; not an evolver proposal');
    return JSON.parse(lines.slice(i + 1, end).join('\n'));
  }
  throw new Error('no json block found; not an evolver proposal');
}

// ----------------------------------------------------------------------------------------------- evaluate

/** A pass is a task that passed and was not hacked: `passed && clean !== false`, so an unaudited pass
 *  counts and a hacked one never does. */
const passOf = (r) => !!r?.passed && r.clean !== false;

/** Per task and in total, what the candidate run did against the baseline on the same task set.
 *  Costs and token sums come from bench.mjs's own summarizer, so the two tools cannot drift. */
export function scorecard(baseline, candidate) {
  const ids = baseline.map((r) => r.id);
  const cand = new Map(candidate.map((r) => [r.id, r]));
  const missing = ids.filter((id) => !cand.has(id));
  const extra = [...cand.keys()].filter((id) => !ids.includes(id));
  if (missing.length || extra.length) throw new Error(`task sets differ (baseline-only: ${missing.join(', ') || 'none'}; candidate-only: ${extra.join(', ') || 'none'}); rerun the candidate on exactly the baseline's tasks`);
  const cost = (r) => totalCost(r.cost_usd, r.routed_cost_usd);
  const rows = ids.map((id) => {
    const b = baseline.find((r) => r.id === id);
    const a = cand.get(id);
    return {
      id,
      base_result: { passed: !!b.passed, clean: b.clean ?? null }, cand_result: { passed: !!a.passed, clean: a.clean ?? null },
      delta: (passOf(a) ? 1 : 0) - (passOf(b) ? 1 : 0),
      base_cost: cost(b), cand_cost: cost(a), cost_delta: (cost(a) ?? 0) - (cost(b) ?? 0),
    };
  });
  const bs = summarizeRun(baseline);
  const cs = summarizeRun(candidate);
  const rate = (side) => rows.filter((r) => passOf(side === 'b' ? r.base_result : r.cand_result)).length / (rows.length || 1);
  // A baseline that cost nothing cannot bound a rise: the percent is infinite, never a division by zero.
  const costPct = bs.total_cost_usd > 0 ? (cs.total_cost_usd - bs.total_cost_usd) / bs.total_cost_usd : cs.total_cost_usd - bs.total_cost_usd > 0 ? Infinity : 0;
  return {
    tasks: rows,
    regressions: rows.filter((r) => passOf(r.base_result) && !passOf(r.cand_result)).map((r) => r.id),
    passed: { baseline: rows.filter((r) => passOf(r.base_result)).length, candidate: rows.filter((r) => passOf(r.cand_result)).length },
    score: { baseline: rate('b'), candidate: rate('c') },
    pass_rate: { baseline: bs.pass_rate, candidate: cs.pass_rate },
    clean_rate: { baseline: bs.clean_rate, candidate: cs.clean_rate },
    gap: { baseline: bs.gap, candidate: cs.gap },
    totals: {
      baseline_cost_usd: bs.total_cost_usd, candidate_cost_usd: cs.total_cost_usd, cost_delta: cs.total_cost_usd - bs.total_cost_usd, cost_pct: costPct,
      token_delta: Object.fromEntries(TOKEN_CATEGORIES.map((k) => [k, cs.token_categories[k] - bs.token_categories[k]])),
    },
  };
}

/** The retain verdict for one scorecard. Order matters: an over-bound cost says no outright, a
 *  regression is never silently retained, and the score has to actually move. When both runs were
 *  audited, trajectory-monitor's compareProposal gets the last word, so a proposal whose clean rate
 *  does not improve — a widened gap included — is flagged for a human even with a better average. */
export function verdict(card, { maxCostIncrease = 0.10 } = {}) {
  const t = card.totals;
  if (t.cost_pct > maxCostIncrease) return { verdict: 'discarded', reasons: [`cost rose ${pctOf(t.cost_pct)} (${usd(t.baseline_cost_usd)} → ${usd(t.candidate_cost_usd)}), over the ${Math.round(maxCostIncrease * 100)}% bound`] };
  if (card.regressions.length > 0) return { verdict: 'flagged', reasons: [`regressed from pass to fail: ${card.regressions.join(', ')}`] };
  if (!(card.score.candidate > card.score.baseline)) return { verdict: 'discarded', reasons: [`score not better: ${card.passed.baseline}/${card.tasks.length} → ${card.passed.candidate}/${card.tasks.length}`] };
  if (card.clean_rate.baseline != null && card.clean_rate.candidate != null) {
    // The audited pair answers to the monitor's compareProposal, not to a reimplementation here.
    const monitorCard = (side) => ({ clean_rate: card.clean_rate[side], gap: card.gap[side], resolved_rate: card.pass_rate[side] });
    const cmp = compareProposal(monitorCard('baseline'), monitorCard('candidate'));
    if (!cmp.accept) return { verdict: 'flagged', reasons: [cmp.reason] };
  }
  const kept = `score ${card.passed.baseline}/${card.tasks.length} → ${card.passed.candidate}/${card.tasks.length}`;
  return { verdict: 'retained', reasons: [kept, `cost ${usd(t.baseline_cost_usd)} → ${usd(t.candidate_cost_usd)} (${pctOf(t.cost_pct)}), within the ${Math.round(maxCostIncrease * 100)}% bound`] };
}

/** Reproduction: retained only when every candidate run says so; a flag is never silently kept. */
export const overallVerdict = (verdicts) => (verdicts.every((v) => v === 'retained') ? 'retained' : verdicts.some((v) => v === 'discarded') ? 'discarded' : 'flagged');

/** Scores each candidate run against the one baseline. Pure; the caller reads and writes the file. */
export function evaluate(proposal, baselineRun, candidateRuns) {
  if (!['proposed', 'evaluated'].includes(proposal.status)) throw new Error(`a ${proposal.status} proposal cannot be re-evaluated`);
  const baseline = baselineRun.results ?? [];
  const evaluations = candidateRuns.map((cand, i) => ({
    candidate: [cand.label, cand.name].filter(Boolean).join(' · ') || `candidate ${i + 1}`,
    card: scorecard(baseline, cand.results ?? []),
  }));
  return { ...proposal, status: 'evaluated', evaluations, verdict: null };
}

/** Stamps the verdict and its reasons, per run and overall. The scorecard is kept whatever it says. */
export function retain(proposal, { maxCostIncrease = 0.10 } = {}) {
  if (proposal.status !== 'evaluated') throw new Error('evaluate the proposal first');
  if (!proposal.evaluations?.length) throw new Error('the proposal has no scorecard to judge; evaluate it first');
  const runs = proposal.evaluations.map((e) => ({ candidate: e.candidate, ...verdict(e.card, { maxCostIncrease }) }));
  const overall = overallVerdict(runs.map((r) => r.verdict));
  const reasons = overall === 'retained' ? [`all ${runs.length} candidate run(s) retained`] : runs.filter((r) => r.verdict === overall).flatMap((r) => `${r.candidate}: ${r.reasons.join('; ')}`);
  return { ...proposal, verdict: { verdict: overall, reasons, runs, max_cost_increase: maxCostIncrease } };
}

// ----------------------------------------------------------------------------------------------- command

export function parseArgs(argv) {
  const args = { command: argv[0] ?? null, findings: [], bench: [], candidate: [], ids: [], out: null, data: null, classes: null, class: null, module: null, constant: null, text: null, root: null, proposal: null, baseline: null, reason: null, maxCostIncrease: 0.10 };
  const one = { '--data': 'data', '--out': 'out', '--classes': 'classes', '--class': 'class', '--module': 'module', '--constant': 'constant', '--text': 'text', '--root': 'root', '--proposal': 'proposal', '--baseline': 'baseline', '--reason': 'reason' };
  const many = { '--findings': 'findings', '--bench': 'bench', '--candidate': 'candidate' };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    // Refuse a missing value here: a flag left dangling would otherwise be read as undefined or
    // silently dropped, after the diagnosis or the scorecard it belongs to has already been computed.
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`${a} needs a value`);
      return argv[++i];
    };
    if (Object.hasOwn(many, a)) args[many[a]].push(value());
    else if (Object.hasOwn(one, a)) args[one[a]] = value();
    else if (a === '--max-cost-increase') {
      const n = Number(value());
      if (!Number.isFinite(n) || n < 0) throw new Error('--max-cost-increase needs a non-negative number');
      args.maxCostIncrease = n;
    } else if (!a.startsWith('--')) args.ids.push(a);
    else throw new Error(`unknown argument ${a}`);
  }
  return args;
}

const outDirOf = (args) => resolve(args.out ?? defaultOut());
const readProposalFile = (path) => ({ path, proposal: parseProposal(readFileSync(path, 'utf8')) });
const writeProposal = (path, p) => writeFileSync(path, renderProposal(p));
const sessionStats = (data) => Object.fromEntries(loadColonies(data).map((c) => {
  const a = analyze(c);
  return [a.id, { questions: a.questions, tool_errors: a.tool_errors }];
}));

/** The proposal file for an id: exact name first, else a unique id prefix among the queue's parseable
 *  proposals. Only slugs get this far — a raw id is never joined onto the directory, so `approve
 *  ../x` reads and writes nothing outside the queue. */
export function findProposal(dir, id) {
  if (!/^[A-Za-z0-9._-]+$/.test(id)) throw new Error(`not a proposal id: ${id}`);
  const exact = join(dir, `${id}.md`);
  if (existsSync(exact)) return readProposalFile(exact);
  const parse = (f) => {
    try {
      return readProposalFile(join(dir, f));
    } catch {
      return null; // a proposal a hand edit broke is skipped here; `list` is where it is named
    }
  };
  const hits = readdirSync(dir).filter((f) => f.endsWith('.md')).map(parse).filter(Boolean).filter((x) => x.proposal.id === id || x.proposal.id.startsWith(id));
  if (hits.length !== 1) throw new Error(hits.length ? `${id} matches ${hits.length} proposals; be more specific` : `no proposal ${id} in ${dir}`);
  return hits[0];
}

function cmdDiagnose(args) {
  if (!args.bench.length && !args.findings.length) throw new Error('diagnose needs --bench <bench.json> and/or --findings <findings.jsonl>');
  if (args.data && !existsSync(args.data)) throw new Error(`no data directory at ${args.data}`);
  const runs = args.bench.map((f) => JSON.parse(readFileSync(f, 'utf8')));
  const lines = args.findings.flatMap((f) => readJsonLines(f));
  const redteam = lines.filter((l) => typeof l.defect === 'string');
  const findings = lines.filter((l) => typeof l.state === 'string' && typeof l.title === 'string');
  const unrecognised = lines.length - redteam.length - findings.length;
  if (unrecognised > 0) console.error(`evolve: skipped ${unrecognised} line(s) that were neither redteam-report nor findings-ledger records`);
  const diagnosis = { at: new Date().toISOString(), ...diagnose({ bench: runs, redteam, findings, sessions: args.data ? sessionStats(args.data) : null }) };
  const json = `${JSON.stringify(diagnosis, null, 2)}\n`;
  if (args.out) {
    writeFileSync(args.out, json);
    console.log(`wrote ${args.out}`);
  } else {
    process.stdout.write(json);
  }
  if (diagnosis.classes.length === 0) console.log('no confirmed failures found in those inputs');
  else console.log(`\n${table(['Count', 'Class', 'Kind'], diagnosis.classes.map((c) => [c.count, c.id, c.kind]))}`);
}

function cmdPropose(args) {
  for (const [flag, v] of [['--classes', args.classes], ['--class', args.class], ['--module', args.module], ['--constant', args.constant], ['--text', args.text]]) {
    if (!v) throw new Error(`propose needs ${flag}`);
  }
  const classes = JSON.parse(readFileSync(args.classes, 'utf8'));
  const text = readFileSync(args.text, 'utf8').replace(/\n$/, '');
  const root = args.root ? resolve(args.root) : ROOT;
  const dir = outDirOf(args);
  const p = propose({ classes, classId: args.class, module: args.module, constant: args.constant, text, root });
  mkdirSync(dir, { recursive: true });
  const path = join(dir, `${p.id}.md`);
  if (existsSync(path)) throw new Error(`${path} already exists; delete it or pass another --out`);
  writeProposal(path, p);
  writeFileSync(join(dir, `${p.id}.diff`), `${p.diff}\n`);
  console.log(`proposed ${p.id} (${p.kind} on ${p.surface}) against ${p.constant} in ${p.file}`);
  console.log(`wrote ${path} and ${join(dir, `${p.id}.diff`)}`);
  console.log('\nReproduce (baseline first, then the candidate with the diff applied):');
  for (const line of p.reproduce) console.log(`  ${line}`);
}

function cmdEvaluate(args) {
  if (!args.proposal || !args.baseline || !args.candidate.length) throw new Error('evaluate needs --proposal <file> --baseline <bench.json> --candidate <bench.json>');
  const { path, proposal } = readProposalFile(resolve(args.proposal));
  const candidates = args.candidate.map((f) => ({ ...JSON.parse(readFileSync(f, 'utf8')), name: basename(f) }));
  const updated = evaluate(proposal, JSON.parse(readFileSync(args.baseline, 'utf8')), candidates);
  writeProposal(path, updated);
  console.log(`evaluated ${updated.id} against ${candidates.length} candidate run(s); written to ${path}`);
  for (const e of updated.evaluations) console.log(`  ${e.candidate}: score ${e.card.passed.baseline}/${e.card.tasks.length} → ${e.card.passed.candidate}/${e.card.tasks.length}, cost ${usd(e.card.totals.baseline_cost_usd)} → ${usd(e.card.totals.candidate_cost_usd)}${e.card.regressions.length ? `, regressions: ${e.card.regressions.join(', ')}` : ''}`);
  console.log(`now run: node scripts/evolve.mjs retain --proposal ${path}`);
}

function cmdRetain(args) {
  if (!args.proposal) throw new Error('retain needs --proposal <file>');
  const { path, proposal } = readProposalFile(resolve(args.proposal));
  const updated = retain(proposal, { maxCostIncrease: args.maxCostIncrease });
  writeProposal(path, updated);
  console.log(`${updated.id}: ${updated.verdict.verdict.toUpperCase()}`);
  for (const reason of updated.verdict.reasons) console.log(`  ${reason}`);
  for (const run of updated.verdict.runs ?? []) console.log(`  ${run.candidate}: ${run.verdict}`);
  if (updated.verdict.verdict === 'retained') console.log(`approve with: node scripts/evolve.mjs approve ${updated.id} --out ${dirname(path)}`);
}

function cmdList(args) {
  const dir = outDirOf(args);
  if (!existsSync(dir)) throw new Error(`no proposals directory at ${dir}`);
  const rows = readdirSync(dir).filter((f) => f.endsWith('.md')).flatMap((f) => {
    try {
      const p = parseProposal(readFileSync(join(dir, f), 'utf8'));
      return [[p.id, p.class, p.status, p.verdict?.verdict ?? '–']];
    } catch (e) {
      return [[f, `unparseable (${e.message})`, '–', '–']];
    }
  }).sort((a, b) => a[0].localeCompare(b[0]));
  if (rows.length === 0) return console.log(`no proposals in ${dir}`);
  console.log(table(['Id', 'Class', 'Status', 'Verdict'], rows));
}

function cmdApprove(args) {
  if (args.ids.length !== 1) throw new Error('approve needs one proposal id');
  const { path, proposal } = findProposal(outDirOf(args), args.ids[0]);
  if (proposal.status === 'approved') return console.log(`${proposal.id} is already approved`);
  if (proposal.status === 'rejected') throw new Error(`${proposal.id} is rejected; reject --reason says no, approve never overrules it`);
  if (proposal.verdict?.verdict !== 'retained') throw new Error(`only a retained proposal can be approved (verdict: ${proposal.verdict?.verdict ?? 'none'})`);
  writeProposal(path, { ...proposal, status: 'approved', approved_at: new Date().toISOString() });
  console.log(`approved ${proposal.id}. Turn it into an ordinary pull request by hand:`);
  const diff = path.replace(/\.md$/, '.diff');
  if (existsSync(diff)) console.log(`  git apply ${diff}`);
  else console.log(`  (the diff file ${diff} is missing — copy it out of the proposal's Diff section)`);
}

function cmdReject(args) {
  if (args.ids.length !== 1) throw new Error('reject needs one proposal id');
  const { path, proposal } = findProposal(outDirOf(args), args.ids[0]);
  if (proposal.status === 'rejected') return console.log(`${proposal.id} is already rejected`);
  if (proposal.status === 'approved') throw new Error(`${proposal.id} is approved; a decision already turned into a pull request is not undone here`);
  writeProposal(path, { ...proposal, status: 'rejected', rejected_at: new Date().toISOString(), reject_reason: args.reason ?? null });
  console.log(`rejected ${proposal.id}${args.reason ? `: ${args.reason}` : ''}`);
}

const COMMANDS = { diagnose: cmdDiagnose, propose: cmdPropose, evaluate: cmdEvaluate, retain: cmdRetain, list: cmdList, approve: cmdApprove, reject: cmdReject };

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const cmd = COMMANDS[args.command];
  if (!cmd) throw new Error('use diagnose, propose, evaluate, retain, list, approve or reject (see the header of scripts/evolve.mjs)');
  return cmd(args);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((e) => {
    console.error(`evolve: ${e.message}`);
    process.exit(1);
  });
}
