// Per-stack bench knowledge: the root marker that names a stack, the files each one reads as source or
// test, what a copy of a repo leaves behind, the command that gates a mutant's syntax, the command that
// runs its tests and the reader for that output, and where a held-out companion lands in a fresh clone.
// Node, Rust and Go; docs/bench.md documents the commands and the detection order.
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

// Root markers, most specific first: a napi-style Rust crate that ships a package.json is still Rust —
// its tests are cargo's, not npm's.
const MARKERS = [['Cargo.toml', 'rust'], ['go.mod', 'go'], ['package.json', 'node']];

/** The stack of the repository at `dir`, by root marker only; null when none of them is there. */
export const detectStack = (dir) => MARKERS.find(([marker]) => existsSync(join(dir, marker)))?.[1] ?? null;

// A node --test started from inside another one inherits NODE_TEST_CONTEXT and skips its files, a colony
// sandbox exports GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE that would point git at the wrong work tree, and a
// stray NODE_OPTIONS could hand a child its own reporter. None of it rides into a scored child.
export const childEnv = () => {
  const { NODE_TEST_CONTEXT, NODE_OPTIONS, GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, ...env } = process.env;
  return env;
};

// ----------------------------------------------------------------------------------------------- readers

/** Reads TAP into name sets; any indentation counts, and a trailing `# directive` is not part of the name. */
export function parseTap(out) {
  const passed = [];
  const failed = [];
  for (const m of out.matchAll(/^[ \t]*(not ok|ok)[ \t]+\d+[ \t]+-[ \t]*(.+)$/gm)) {
    (m[1] === 'ok' ? passed : failed).push(m[2].replace(/[ \t]*#[^\n]*$/, '').trim());
  }
  return { passed, failed };
}

/** Reads `cargo test` output into name sets: one `test <path> ... ok|FAILED` line per test, doc-tests
 *  included; the `test result:` summary lines name nothing and are skipped. */
export function parseCargoTest(out) {
  const passed = [];
  const failed = [];
  for (const m of out.matchAll(/^test (.+) \.\.\. (ok|FAILED)$/gm)) (m[2] === 'ok' ? passed : failed).push(m[1]);
  return { passed, failed };
}

/** Reads `go test -json` events into name sets, each test qualified by its package. A package also fails
 *  as a whole when any of its tests does; only a fail with no named test — a build or vet error — counts
 *  under the package's own name. A line that is not JSON is skipped, never read as a result. */
export function parseGoTest(out) {
  const passed = [];
  const failed = [];
  const broken = new Set();
  for (const line of out.split('\n')) {
    let e;
    try { e = JSON.parse(line); } catch { continue; }
    if (e.Action === 'pass' && e.Test) passed.push(`${e.Package}.${e.Test}`);
    else if (e.Action === 'fail' && e.Test) failed.push(`${e.Package}.${e.Test}`);
    else if (e.Action === 'fail') broken.add(e.Package);
  }
  const named = new Set([...passed, ...failed].map((name) => name.slice(0, name.lastIndexOf('.'))));
  return { passed, failed: [...failed, ...[...broken].filter((pkg) => !named.has(pkg))] };
}

// ----------------------------------------------------------------------------------------------- runners

// The convention runNodeTests set: a non-zero exit is tests failing — the names still ride stdout — while
// anything else (a spawn failure, a lost buffer) is rethrown rather than read as a result. A non-zero exit
// with no failing test named is a build (or vet) error, not a green run, and is named as one: `broken`.
const readTests = (cmd, args, dir, read, broken) => {
  let status = 0;
  let out;
  try {
    out = execFileSync(cmd, args, { cwd: dir, encoding: 'utf8', env: childEnv(), maxBuffer: 64 * 1024 * 1024 });
  } catch (e) {
    if (e.code || !Number.isInteger(e.status)) throw e;
    out = `${e.stdout ?? ''}`;
    status = e.status;
  }
  const result = read(out);
  if (status !== 0 && result.failed.length === 0) result.failed.push(broken);
  return result;
};

// The syntax gate: a mutant that does not compile is vacuous breakage and never enters.
const compiles = (cmd, args, dir) => {
  try {
    execFileSync(cmd, args, { cwd: dir, encoding: 'utf8', env: childEnv(), maxBuffer: 64 * 1024 * 1024 });
    return true;
  } catch (e) {
    if (e.code || !Number.isInteger(e.status)) throw e;
    return false;
  }
};

// A held-out companion passes or fails by its exit code alone and the output is dropped, so `ignore`
// keeps a chatty build from tripping a buffer.
const runCompanion = (cmd, args, dir) => execFileSync(cmd, args, { cwd: dir, env: childEnv(), stdio: 'ignore' });

/** `node --test` in a checkout, read as TAP. */
export const runNodeTests = (dir, testFiles) => readTests('node', ['--test', '--test-reporter=tap', ...testFiles], dir, parseTap, 'the test run failed');
const runCargoTests = (dir) => readTests('cargo', ['test'], dir, parseCargoTest, 'the crate did not compile');
// `-count=1`: the gate runs the tests twice and compares, so go's result cache must never replay the
// first run as the second.
const runGoTests = (dir) => readTests('go', ['test', '-count=1', '-json', './...'], dir, parseGoTest, 'the module did not compile');

/** `node --check` on one file. */
export const parses = (dir, file) => compiles('node', ['--check', file], dir);

const isNodeTest = (file) => /\.(test|spec)\.[cm]?js$/.test(file) || /(^|\/)tests?\//.test(file);
const isRustTest = (file) => /(^|\/)(tests|benches)\//.test(file);
const isGoTest = (file) => /(^|\/)[^/]*_test\.go$/.test(file);

// The swaps for the compiled stacks, `from` → `to`: synth's JavaScript table without the triple operators
// those languages lack, and `==`↔`!=` in their place.
const COMPILED_BINOPS = [
  ['==', '!='], ['!=', '=='], ['<=', '<'], ['>=', '>'], ['&&', '||'], ['||', '&&'],
  ['<', '<='], ['>', '>='], ['+', '-'], ['-', '+'], ['*', '/'], ['/', '*'],
];

/** Everything the bench needs per stack, keyed `node | rust | go`. `scanner` feeds synth's
 *  `mutationSites`; `companion` is where a held-out check lands in a fresh clone and what runs it.
 *  `runsWholeSuite` marks a runner that ignores the file list and runs the crate's or module's whole
 *  suite, inline tests included — so a repository whose tests are all inline still generates. */
export const STACKS = {
  node: {
    name: 'node',
    marker: 'package.json',
    scanner: { lex: 'js' },
    isSource: (file) => /\.[cm]?js$/.test(file) && !isNodeTest(file),
    isTest: isNodeTest,
    excludes: ['node_modules', '.git'],
    check: parses,
    runTests: runNodeTests,
    companion: {
      path: (dir) => join(dir, 'heldout-check.test.mjs'),
      run: (dir, file) => runCompanion('node', ['--test', file], dir),
    },
  },
  rust: {
    name: 'rust',
    marker: 'Cargo.toml',
    scanner: { lex: 'rust', binops: COMPILED_BINOPS },
    isSource: (file) => file.endsWith('.rs') && !isRustTest(file),
    isTest: isRustTest,
    excludes: ['target', '.git'],
    check: (dir) => compiles('cargo', ['test', '--no-run'], dir),
    runTests: runCargoTests,
    runsWholeSuite: true,
    companion: {
      path: (dir) => join(dir, 'tests', 'heldout_check.rs'),
      run: (dir) => runCompanion('cargo', ['test', '--test', 'heldout_check'], dir),
    },
  },
  go: {
    name: 'go',
    marker: 'go.mod',
    scanner: { lex: 'go', binops: COMPILED_BINOPS },
    isSource: (file) => file.endsWith('.go') && !isGoTest(file),
    isTest: isGoTest,
    excludes: ['.git'],
    check: (dir) => compiles('go', ['test', '-count=1', '-run', '^$', './...'], dir),
    runTests: runGoTests,
    runsWholeSuite: true,
    companion: {
      path: (dir) => join(dir, 'heldout_check_test.go'),
      run: (dir) => runCompanion('go', ['test', '-count=1', '.'], dir),
    },
  },
};
