#!/usr/bin/env node
// Synthetic bench tasks, SWE-smith style: inject a bug into real source, keep only the mutants that break
// the repository's own tests, admit them through a gate, and feed a held-out pool the bench draws from.
// Flaky-but-real mutants go to a separate raid set: never score on the raid set, never raid the scoring
// set. Stage one is procedural and Node-only, so an accepted task costs $0. The pool lives outside this
// repository on purpose — a held-out set committed next to the agents being scored is visible to them —
// so --pool is required and never defaulted. docs/bench.md says the rest.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, cpSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

/** A held-out pool opens to scoring only once this many accepted tasks carry a human verdict. */
export const REVIEW_QUORUM = 20;
/** A held-out task is retired after this many scoring decisions, so the pool stays fresh. */
export const RETIRE_AFTER = 3;

// ------------------------------------------------------------------------------------------------ scanner

// The swaps, `from` → `to`. Narrow on purpose: each stays valid JavaScript almost everywhere, so the
// syntax gate rejects little and the test gate does the deciding.
const BINOPS = [
  ['===', '!=='], ['!==', '==='], ['<=', '<'], ['>=', '>'], ['&&', '||'], ['||', '&&'],
  ['<', '<='], ['>', '>='], ['+', '-'], ['-', '+'], ['*', '/'], ['/', '*'],
];
const IDENT = /[A-Za-z0-9_$]/;
// A `/` after one of these cannot be division, so it opens a regex.
const REGEX_KEYWORDS = new Set(['return', 'typeof', 'case', 'throw', 'void', 'in', 'of', 'new', 'delete', 'instanceof', 'yield', 'await', 'else']);

const skipString = (src, i) => {
  const quote = src[i];
  i++;
  while (i < src.length && src[i] !== quote) i += src[i] === '\\' ? 2 : 1;
  return i + 1;
};
// Templates are skipped whole, interpolations included: code inside `${…}` is part of a string being built.
const skipTemplate = (src, i) => {
  i++;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\') i += 2;
    else if (c === '`') return i + 1;
    else if (c === '$' && src[i + 1] === '{') i = skipInterpolation(src, i + 2);
    else i++;
  }
  return i;
};
const skipInterpolation = (src, i) => {
  let depth = 1;
  while (i < src.length && depth > 0) {
    const c = src[i];
    if (c === '\\') i += 2;
    else if (c === "'" || c === '"') i = skipString(src, i);
    else if (c === '`') i = skipTemplate(src, i);
    else if (c === '/' && src[i + 1] === '/') while (i < src.length && src[i] !== '\n') i++;
    else if (c === '/' && src[i + 1] === '*') i = skipBlock(src, i);
    else if (c === '{') { depth++; i++; }
    else if (c === '}') { depth--; i++; }
    else i++;
  }
  return i;
};
const skipBlock = (src, i) => {
  while (i < src.length && !(src[i] === '*' && src[i + 1] === '/')) i++;
  return i + 2;
};
// A regex ends at the first unescaped `/` outside a character class; its `+` and `<` are not ours to swap.
const skipRegex = (src, i) => {
  i++;
  let inClass = false;
  while (i < src.length) {
    const c = src[i];
    if (c === '\\') i += 2;
    else if (c === '\n') return i; // a regex cannot span lines; bail rather than swallow the file
    else if (c === '[') { inClass = true; i++; }
    else if (c === ']') { inClass = false; i++; }
    else if (c === '/' && !inClass) return i + 1 + /^[dgimsuy]*/.exec(src.slice(i + 1))[0].length;
    else i++;
  }
  return i;
};

/** Token-level, not an AST: yields `{ operator, offset, from, to, line }` for operator swaps (`binop`) and
 *  literal nudges (`literal`) outside comments, strings, templates and regexes. Ambiguous constructs are
 *  skipped rather than risk nonsense — that loses a candidate, never admits a bad one. */
export function mutationSites(source) {
  const sites = [];
  const push = (operator, offset, from, to) =>
    sites.push({ operator, offset, from, to, line: source.slice(0, offset).split('\n').length });
  let i = 0;
  let prev = ''; // the last significant code character: decides whether a `/` divides or opens a regex
  let word = ''; // the last identifier, so `function*` is not swapped into `function/`
  while (i < source.length) {
    const c = source[i];
    const d = source[i + 1] ?? '';
    if (/\s/.test(c)) { i++; continue; }
    if (c === '/' && d === '/') { while (i < source.length && source[i] !== '\n') i++; prev = ''; word = ''; continue; }
    if (c === '/' && d === '*') { i = skipBlock(source, i); continue; }
    if (c === "'" || c === '"') { i = skipString(source, i); prev = c; word = ''; continue; }
    if (c === '`') { i = skipTemplate(source, i); prev = '`'; word = ''; continue; }
    // A `/` divides when it follows a value (a name, a number, `)`, `]`, a literal) and opens a regex
    // otherwise; `}` is read as regex-opening, which is the safer wrong.
    if (c === '/' && (REGEX_KEYWORDS.has(word) || prev === '' || !/[A-Za-z0-9_$)\]'"`]/.test(prev))) { i = skipRegex(source, i); prev = 'x'; word = ''; continue; }
    if (/[0-9]/.test(c) && !IDENT.test(prev) && prev !== '.') {
      let j = i;
      while (j < source.length && /[0-9]/.test(source[j])) j++;
      // An exponent, fraction, hex prefix, BigInt suffix or separator means it is not a plain integer.
      if (!IDENT.test(source[j] ?? '') && source[j] !== '.') push('literal', i, source.slice(i, j), String(Number(source.slice(i, j)) + 1));
      prev = 'x'; word = ''; i = j; continue;
    }
    if (IDENT.test(c)) {
      let j = i;
      while (j < source.length && IDENT.test(source[j])) j++;
      const w = source.slice(i, j);
      if (w === 'true' || w === 'false') push('literal', i, w, w === 'true' ? 'false' : 'true');
      prev = 'x'; word = w; i = j; continue;
    }
    const rest = source.slice(i, i + 3);
    if (rest === '>>>' || rest === '===' || rest === '!==') {
      if (rest !== '>>>') push('binop', i, rest, rest === '===' ? '!==' : '===');
      i += 3; prev = 'x'; word = ''; continue;
    }
    const two = source.slice(i, i + 2);
    // `++`, `--`, `**`, `+=`, `-=`, `*=`, `/=`, `<<`, `>>` and `=>` are single tokens with no swap.
    if (['++', '--', '**', '+=', '-=', '*=', '/=', '<<', '>>', '=>'].includes(two)) {
      i += 2; prev = c; word = ''; continue;
    }
    const twoSwap = BINOPS.find(([from]) => from === two);
    const oneSwap = twoSwap ? null : BINOPS.find(([from]) => from === c);
    // A `*` after `function`, `yield`, `async` or an opening bracket is a generator's star, not multiplication.
    const generatorStar = c === '*' && (['function', 'yield', 'async'].includes(word) || prev === '' || '{('.includes(prev));
    if (twoSwap) push('binop', i, twoSwap[0], twoSwap[1]);
    else if (oneSwap && !generatorStar) push('binop', i, oneSwap[0], oneSwap[1]);
    prev = c; word = ''; i += twoSwap ? 2 : 1;
  }
  return sites;
}

/** Splices one site into source. */
export const applyMutation = (source, site) => source.slice(0, site.offset) + site.to + source.slice(site.offset + site.from.length);

// --------------------------------------------------------------------------------------------------- gate

// A node --test started from inside another one inherits NODE_TEST_CONTEXT, and a colony sandbox exports
// GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE that would point git at the wrong work tree; neither rides along.
const childEnv = () => {
  const { NODE_TEST_CONTEXT, GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, ...env } = process.env;
  return env;
};

/** Reads TAP into name sets; any indentation counts, and a trailing `# directive` is not part of the name. */
export function parseTap(out) {
  const passed = [];
  const failed = [];
  for (const m of out.matchAll(/^[ \t]*(not ok|ok)[ \t]+\d+[ \t]+-[ \t]*(.+)$/gm)) {
    (m[1] === 'ok' ? passed : failed).push(m[2].replace(/[ \t]*#[^\n]*$/, '').trim());
  }
  return { passed, failed };
}

/** Runs `node --test` in a checkout and reads its TAP. A non-zero exit is tests failing; anything else
 *  (a spawn failure, a lost buffer) is rethrown rather than read as a result. */
export function runNodeTests(dir, testFiles) {
  let out;
  try {
    out = execFileSync('node', ['--test', '--test-reporter=tap', ...testFiles], { cwd: dir, encoding: 'utf8', env: childEnv(), maxBuffer: 64 * 1024 * 1024 });
  } catch (e) {
    if (e.code || !Number.isInteger(e.status)) throw e;
    out = `${e.stdout ?? ''}`; // the TAP still rides stdout when tests fail
  }
  return parseTap(out);
}

/** `node --check` on the mutated file: a mutant that does not parse is vacuous breakage and never enters. */
export function parses(dir, file) {
  try {
    execFileSync('node', ['--check', file], { cwd: dir, encoding: 'utf8', env: childEnv() });
    return true;
  } catch (e) {
    if (e.code || !Number.isInteger(e.status)) throw e;
    return false;
  }
}

// --------------------------------------------------------------------------------------------------- pool

/** Reads a pool directory; a file that will not parse is an error, not an empty set quietly overwritten. */
export function loadPool(dir) {
  const read = (name) => {
    const file = join(dir, name);
    if (!existsSync(file)) return [];
    return JSON.parse(readFileSync(file, 'utf8'));
  };
  return { heldout: read('heldout.json'), raid: read('raid.json'), runs: read('runs.json') };
}

/** Writes each pool file through a temp file and rename, so a crash cannot leave a half-written set. */
export function savePool(dir, pool) {
  mkdirSync(dir, { recursive: true });
  for (const name of ['heldout', 'raid', 'runs']) {
    const file = join(dir, `${name}.json`);
    writeFileSync(`${file}.tmp`, `${JSON.stringify(pool[name], null, 2)}\n`);
    renameSync(`${file}.tmp`, file);
  }
}

/** An exclusive lock over a pool for the mutating commands; a hard kill leaves pool.lock to remove by hand. */
const lockPool = (dir) => {
  mkdirSync(dir, { recursive: true });
  try {
    closeSync(openSync(join(dir, 'pool.lock'), 'wx'));
  } catch (e) {
    if (e.code === 'EEXIST') throw new Error(`another synth command is using the pool at ${dir}; remove ${join(dir, 'pool.lock')} only if it was left by a crash`);
    throw e;
  }
  return () => rmSync(join(dir, 'pool.lock'), { force: true });
};

/** Short sha256 of repo-relative file + offset + from + to: the same mutant is always the same task. */
const mutantId = (file, site) => createHash('sha256').update(`${file}\0${site.offset}\0${site.from}\0${site.to}`).digest('hex').slice(0, 12);

/** Puts an entry in one of the two sets; an id already in the other throws — nothing is scored and raided. */
export function admit(pool, entry, set) {
  const inSet = (list) => list.some((e) => e.id === entry.id);
  const [own, other, otherName] = set === 'heldout' ? [pool.heldout, pool.raid, 'raid'] : [pool.raid, pool.heldout, 'held-out'];
  if (inSet(other)) throw new Error(`${entry.id} is already in the ${otherName} set; the two stay separate`);
  if (inSet(own)) throw new Error(`${entry.id} is already in the ${set} set`);
  own.push(entry);
  return entry;
}

// The issue states the symptom, never the mutation: which tests fail, and that the fix belongs in source.
const issueText = (file, failed) => ({
  title: `${file}: tests fail after a recent change`,
  body: [
    'These tests fail on the current checkout:',
    '',
    ...failed.map((name) => `- \`${name}\``),
    '',
    'A recent change to the source broke them. Find the cause and fix it in the source; do not edit, skip or weaken the tests.',
    '',
    'The task is fully specified. Nothing here needs a decision from the user.',
  ].join('\n'),
});

// The raid brief says the quiet part out loud: a hunter is told what class of bug it is hunting.
const raidBrief = (method, file) => `An injected bug of class ${method} lives in ${file}; find it and fix it.`;

// ------------------------------------------------------------------------------------------------ discover

const walk = (root, dir = root, out = []) => {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.name === 'node_modules' || e.name === '.git') continue;
    if (e.isDirectory()) walk(root, join(dir, e.name), out);
    else out.push(relative(root, join(dir, e.name)).split(sep).join('/'));
  }
  return out;
};
const isTestFile = (file) => /\.(test|spec)\.[cm]?js$/.test(file) || /(^|\/)tests?\//.test(file);
const isSourceFile = (file) => /\.[cm]?js$/.test(file) && !isTestFile(file);

const tryGit = (args, repo) => {
  try {
    return execFileSync('git', args, { cwd: repo, encoding: 'utf8', env: childEnv() }).trim() || null;
  } catch {
    return null;
  }
};
const repoLabel = (repo) => {
  const url = tryGit(['remote', 'get-url', 'origin'], repo);
  const ownerName = url?.replace(/\.git$/, '').match(/([^/:]+)\/([^/:]+)$/);
  return ownerName ? ownerName[0] : relative(process.cwd(), repo).split(sep).join('/') || repo.split(sep).pop();
};

// ------------------------------------------------------------------------------------------------ generate

/** Generates instances from a repository into a pool. The reference runs once and must be green; each
 *  candidate must parse, then break the same tests twice — identically for the held-out pool, differently
 *  for the raid set, not at all for the survived pile. `run` and `check` are injectable for unit tests. */
export function generate({ repo, pool: poolDir, files, tests, limit = Infinity, run = runNodeTests, check = parses }) {
  const repoPath = resolve(repo);
  const sources = files ? [...new Set(files)] : walk(repoPath).filter(isSourceFile).sort();
  const testFiles = tests ?? walk(repoPath).filter(isTestFile).sort();
  if (testFiles.length === 0) throw new Error(`${repo} has no test files; pass --tests`);
  const overlap = sources.filter((f) => testFiles.includes(f));
  if (overlap.length > 0) throw new Error(`refusing to mutate the tests the gate runs: ${overlap.join(', ')}`);
  // The commit recorded with a task must reproduce the mutated source, so a work tree with uncommitted
  // changes under the repo is refused; outside git the commit is null and says so.
  let commit = null;
  if (tryGit(['rev-parse', '--is-inside-work-tree'], repoPath) === 'true') {
    if ((tryGit(['status', '--porcelain', '--', '.'], repoPath) ?? '') !== '') throw new Error(`${repo} has uncommitted changes; commit them before generating, so the recorded commit reproduces the source`);
    commit = tryGit(['rev-parse', 'HEAD'], repoPath);
  }
  const pool = loadPool(poolDir);
  const taken = new Set([...pool.heldout, ...pool.raid].map((e) => e.id));

  const refDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-ref-'));
  let reference;
  try {
    copyRepo(repoPath, refDir);
    reference = run(refDir, testFiles);
  } finally {
    rmSync(refDir, { recursive: true, force: true });
  }
  if (reference.failed.length > 0) throw new Error(`the reference is not green (${reference.failed.join(', ')}); fix the repo before generating from it`);

  const tally = { at: new Date().toISOString(), repo: repoLabel(repoPath), commit, candidates: 0, syntax: 0, survived: 0, flaky: 0, admitted: 0, cost_usd: 0 };
  const entry = (set, file, site, first) => {
    const method = `procedural:${site.operator}`;
    const base = {
      id: mutantId(file, site),
      method,
      stack: 'node',
      source: { repo: tally.repo, commit, file, line: site.line },
      mutation: { offset: site.offset, from: site.from, to: site.to },
      gate: { reference: 'green', bugged: 'red', f2p: first.failed, p2p: first.passed, margin: first.failed.length / (first.failed.length + first.passed.length) },
      cost_usd: 0,
      created: new Date().toISOString(),
    };
    return set === 'heldout'
      ? { ...base, issue: issueText(file, first.failed), review: null, decisions: 0, retired: false }
      : { ...base, brief: raidBrief(method, file) };
  };

  outer: for (const file of sources) {
    const source = readFileSync(join(repoPath, file), 'utf8');
    for (const site of mutationSites(source)) {
      if (taken.has(mutantId(file, site))) continue;
      tally.candidates++;
      const dir = mkdtempSync(join(tmpdir(), 'colonizer-synth-'));
      try {
        copyRepo(repoPath, dir);
        writeFileSync(join(dir, file), applyMutation(source, site));
        if (!check(dir, file)) {
          tally.syntax++;
          continue;
        }
        const first = run(dir, testFiles);
        const second = run(dir, testFiles);
        const a = [...first.failed].sort();
        const b = [...second.failed].sort();
        const same = a.length === b.length && a.every((name, k) => name === b[k]);
        if (first.failed.length === 0) {
          tally.survived++;
        } else if (!same) {
          tally.flaky++;
          admit(pool, entry('raid', file, site, first), 'raid');
        } else {
          tally.admitted++;
          admit(pool, entry('heldout', file, site, first), 'heldout');
          if (tally.admitted >= limit) break outer;
        }
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    }
  }
  pool.runs.push(tally);
  savePool(poolDir, pool);
  return tally;
}

/** A pristine copy of a repo: node_modules and .git stay behind, so mutants run against code, not caches. */
const copyRepo = (repo, dest) =>
  cpSync(repo, dest, {
    recursive: true,
    filter: (src) => {
      const parts = relative(repo, src).split(sep);
      return parts[0] === '' || (!parts.includes('node_modules') && !parts.includes('.git'));
    },
  });

// ------------------------------------------------------------------------------------------------ rotation

/** Records a human verdict — `genuine` or `vacuous` — on an accepted held-out task. */
export function review(pool, id, verdict) {
  if (verdict !== 'genuine' && verdict !== 'vacuous') throw new Error(`verdict must be genuine or vacuous, not ${verdict}`);
  const entry = pool.heldout.find((e) => e.id === id);
  if (!entry) throw new Error(`no held-out task ${id}`);
  entry.review = { verdict, at: new Date().toISOString() };
  return entry;
}

/** Held-out tasks to score: reviewed genuine, not retired, oldest first. Throws until the quorum is met. */
export function draw(pool, n) {
  const reviewed = pool.heldout.filter((e) => e.review).length;
  if (reviewed < REVIEW_QUORUM) throw new Error(`pool not open: ${reviewed}/${REVIEW_QUORUM} held-out tasks reviewed`);
  return pool.heldout
    .filter((e) => e.review?.verdict === 'genuine' && !e.retired)
    .sort((a, b) => a.created.localeCompare(b.created))
    .slice(0, n);
}

/** Counts one scoring decision per id, retiring a task after RETIRE_AFTER of them. */
export function record(pool, ids) {
  const retired = [];
  for (const id of ids) {
    const entry = pool.heldout.find((e) => e.id === id);
    if (!entry) throw new Error(`no held-out task ${id}`);
    entry.decisions++;
    if (entry.decisions >= RETIRE_AFTER && !entry.retired) {
      entry.retired = true;
      retired.push(id);
    }
  }
  return { recorded: ids.length, retired };
}

// ------------------------------------------------------------------------------------------------ inventory

const count = (entries, key, base = {}) => entries.reduce((m, e) => ((m[key(e)] = (m[key(e)] ?? 0) + 1), m), { ...base });
const DAY_MS = 86_400_000;

/** What the pool holds and what the gate has passed, so the quality bar is visible without opening files. */
export function inventory(pool) {
  const heldout = pool.heldout;
  const status = (e) => (e.retired ? 'retired' : e.review?.verdict ?? 'pending-review');
  const days = (e) => (Date.now() - Date.parse(e.created)) / DAY_MS;
  const age = { '<30d': 0, '30-90d': 0, '>90d': 0 };
  for (const e of heldout) age[days(e) < 30 ? '<30d' : days(e) <= 90 ? '30-90d' : '>90d']++;
  const candidates = pool.runs.reduce((s, r) => s + r.candidates, 0);
  const admitted = pool.runs.reduce((s, r) => s + r.admitted, 0);
  const cost = pool.runs.reduce((s, r) => s + (r.cost_usd ?? 0), 0);
  return {
    heldout: heldout.length,
    by_method: count(heldout, (e) => e.method),
    by_stack: count(heldout, (e) => e.stack),
    by_status: count(heldout, status, { 'pending-review': 0, genuine: 0, vacuous: 0, retired: 0 }),
    age: { ...age, oldest: heldout.map((e) => e.created).sort()[0] ?? null },
    raid: pool.raid.length,
    raid_by_method: count(pool.raid, (e) => e.method),
    gate: {
      candidates,
      admitted,
      pass_rate: candidates ? admitted / candidates : null,
      cost_per_accepted_usd: admitted ? cost / admitted : null,
    },
  };
}

// ------------------------------------------------------------------------------------------------ command

function parseArgs(argv) {
  const args = { command: argv[0], pool: null, repo: null, files: null, tests: null, limit: Infinity, id: null, verdict: null, n: 10 };
  const positive = (flag, raw) => {
    const n = Number(raw);
    if (!Number.isInteger(n) || n < 1) throw new Error(`${flag} needs a positive integer, not ${raw}`);
    return n;
  };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    const value = () => argv[++i];
    if (a === '--pool') args.pool = value();
    else if (a === '--repo') args.repo = value();
    else if (a === '--files') args.files = value().split(',').map((s) => s.trim()).filter(Boolean);
    else if (a === '--tests') args.tests = value().split(',').map((s) => s.trim()).filter(Boolean);
    else if (a === '--limit') args.limit = positive('--limit', value());
    else if (a === '--id') args.id = value();
    else if (a === '--verdict') args.verdict = value();
    else if (a === '--n') args.n = positive('--n', value());
    else throw new Error(`unknown argument ${a}`);
  }
  return args;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.pool) throw new Error(`${args.command ?? '(no command)'} needs --pool <dir>`);
  if (args.command === 'generate') {
    if (!args.repo) throw new Error('generate needs --repo <dir>');
    const unlock = lockPool(args.pool);
    try {
      const t = generate({ repo: args.repo, pool: args.pool, files: args.files, tests: args.tests, limit: args.limit });
      console.log(`${t.admitted} admitted of ${t.candidates} candidates (${t.syntax} syntax, ${t.survived} survived, ${t.flaky} flaky) · pool ${args.pool}`);
    } finally {
      unlock();
    }
    return;
  }
  const unlock = args.command === 'review' || args.command === 'record' ? lockPool(args.pool) : null;
  try {
    const pool = loadPool(args.pool);
    if (args.command === 'inventory') {
      console.log(JSON.stringify(inventory(pool), null, 2));
    } else if (args.command === 'review') {
      if (!args.id || !args.verdict) throw new Error('review needs --id and --verdict genuine|vacuous');
      const e = review(pool, args.id, args.verdict);
      savePool(args.pool, pool);
      console.log(`${e.id}: ${args.verdict}`);
    } else if (args.command === 'draw') {
      console.log(JSON.stringify(draw(pool, args.n).map((e) => ({ id: e.id, title: e.issue.title, created: e.created })), null, 2));
    } else if (args.command === 'record') {
      if (!args.id) throw new Error('record needs --id <id>[,<id>]');
      const r = record(pool, args.id.split(',').map((s) => s.trim()).filter(Boolean));
      savePool(args.pool, pool);
      console.log(`${r.recorded} recorded, ${r.retired.length} retired (${r.retired.join(', ') || 'none'})`);
    } else {
      throw new Error('use generate, inventory, review, draw or record');
    }
  } finally {
    if (unlock) unlock();
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch((e) => {
    console.error(`synth: ${e.message}`);
    process.exit(1);
  });
}
