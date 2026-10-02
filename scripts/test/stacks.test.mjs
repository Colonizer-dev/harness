import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { applyMutation, generate, loadPool, mutationSites } from '../bench/synth.mjs';
import { detectStack, parseCargoTest, parseGoTest, STACKS } from '../bench/stacks.mjs';

// The site at its line, so expected findings read like the source they come from.
const at = (source, site) => `${site.line} ${site.operator} ${site.from} → ${site.to} → ${applyMutation(source, site).split('\n')[site.line - 1].trim()}`;
const rustSites = (source) => mutationSites(source, STACKS.rust.scanner);
const goSites = (source) => mutationSites(source, STACKS.go.scanner);
// The e2e tests below run the real toolchains; CI without them skips, and stays green.
const onPath = (cmd, arg) => spawnSync(cmd, [arg], { stdio: 'ignore' }).status === 0;
const HAS_CARGO = onPath('cargo', '--version');
const HAS_GO = onPath('go', 'version');

test('detectStack reads the root marker, most specific first, and names no stack for none', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-stacks-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  assert.equal(detectStack(dir), null);
  writeFileSync(join(dir, 'package.json'), '{}');
  assert.equal(detectStack(dir), 'node');
  writeFileSync(join(dir, 'go.mod'), 'module x\n');
  assert.equal(detectStack(dir), 'go');
  writeFileSync(join(dir, 'Cargo.toml'), '[package]\n');
  assert.equal(detectStack(dir), 'rust', 'the marker order, not the file set, decides');
});

test('each stack tells its source files from its test files', () => {
  assert.ok(STACKS.node.isTest('src/cart.test.js') && STACKS.node.isTest('tests/a.mjs'));
  assert.ok(STACKS.node.isSource('src/cart.js') && STACKS.node.isSource('src/cart.mjs'));
  assert.ok(!STACKS.node.isSource('src/cart.test.js') && !STACKS.node.isSource('README.md'));
  assert.ok(STACKS.rust.isTest('tests/add.rs') && STACKS.rust.isTest('crates/x/benches/b.rs'));
  assert.ok(STACKS.rust.isSource('src/lib.rs') && STACKS.rust.isSource('src/main.rs'));
  assert.ok(!STACKS.rust.isSource('tests/add.rs') && !STACKS.rust.isSource('src/lib.txt'));
  assert.ok(STACKS.go.isTest('math_test.go') && STACKS.go.isTest('pkg/x/sum_test.go'));
  assert.ok(STACKS.go.isSource('math.go') && STACKS.go.isSource('pkg/x/sum.go'));
  assert.ok(!STACKS.go.isSource('pkg/x/sum_test.go') && !STACKS.go.isSource('go.mod'));
});

test('the Rust scanner skips strings, raw strings, chars and nested comments', () => {
  const source = [
    '// a == b && c < d',
    '/* /* a + b */ c != d */',
    'const S: &str = "a + b";',
    'const R: &str = r"a + b";',
    'const RAW: &str = r#"a + "b" + c"#;',
    "const C: char = '+';",
    "const B: &[u8] = b\"a+b\";",
    'let n = 1_u32;',
  ].join('\n');
  assert.deepEqual(rustSites(source), []);
});

test('the Rust scanner reads a lifetime as a lifetime, never an unterminated char', () => {
  const source = "fn f<'a>(x: &'a str) -> &'a str { x }";
  // Only the generic brackets are sites (each compiles nowhere, so the gate discards them); the
  // lifetimes themselves, and `->`, are not.
  assert.deepEqual(rustSites(source).map((s) => s.from), ['<', '>']);
});

test('the Rust scanner finds its swaps in code and stops at #[cfg(test)]', () => {
  const source = [
    'pub fn add(a: i32, b: i32) -> i32 {',
    '    if a == b || a > 0 {',
    '        a + b',
    '    } else {',
    '        a - b',
    '    }',
    '}',
    '',
    '#[cfg(test)]',
    'mod tests {',
    '    #[test]',
    '    fn adds() { assert_eq!(add(1, 2), 3); }',
    '}',
  ].join('\n');
  assert.deepEqual(rustSites(source).map((s) => at(source, s)), [
    '2 binop == → != → if a != b || a > 0 {',
    '2 binop || → && → if a == b && a > 0 {',
    '2 binop > → >= → if a == b || a >= 0 {',
    '2 literal 0 → 1 → if a == b || a > 1 {',
    '3 binop + → - → a - b',
    '5 binop - → + → a + b',
  ]);
});

test('the Rust scanner stops at any test-only cfg, and only at a test-only one', () => {
  const wrapped = [
    'pub fn add(a: i32, b: i32) -> i32 { a + b }',
    '',
    '#[cfg(all(test, feature = "nightly"))]',
    'mod tests {',
    '    #[test]',
    '    fn adds() { assert_eq!(add(1, 2), 3); }',
    '}',
  ].join('\n');
  assert.deepEqual(rustSites(wrapped).map((s) => s.line), [1], 'the test module wrapped in all() is not scanned');
  const other = 'pub fn f(a: i32, b: i32) -> i32 {\n    #[cfg(unix)]\n    { a + b }\n}\n';
  assert.deepEqual(rustSites(other).map((s) => s.line), [3], 'a cfg that is not about tests is not a cutoff');
});

test('the Go scanner skips runes, raw strings and the channel operator', () => {
  const source = [
    '// a == b && c < d',
    'var s = "a + b"',
    'var r = `a + b ${x}`',
    "var c = '+'",
    'ch := make(chan int)',
    'v := <-ch',
    'ch <- v',
  ].join('\n');
  assert.deepEqual(goSites(source), []);
});

test('the Go scanner finds its swaps in code', () => {
  const source = [
    'func Add(a, b int) int {',
    '\tif a == b && a > 0 {',
    '\t\treturn a + b',
    '\t}',
    '\treturn 0',
    '}',
  ].join('\n');
  assert.deepEqual(goSites(source).map((s) => at(source, s)), [
    '2 binop == → != → if a != b && a > 0 {',
    '2 binop && → || → if a == b || a > 0 {',
    '2 binop > → >= → if a == b && a >= 0 {',
    '2 literal 0 → 1 → if a == b && a > 1 {',
    '3 binop + → - → return a - b',
  ]);
});

test('the cargo reader takes ok, FAILED and doc-tests, and skips the summary lines', () => {
  const out = [
    '   Compiling tiny v0.1.0 (/tmp/x)',
    '     Running unittests src/lib.rs (/tmp/x/target/debug/deps/tiny-abc)',
    'test adds ... ok',
    'test tests::subtracts ... FAILED',
    'failures:',
    'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s',
    'test src/lib.rs - add (line 3) ... ok',
  ].join('\n');
  assert.deepEqual(parseCargoTest(out), { passed: ['adds', 'src/lib.rs - add (line 3)'], failed: ['tests::subtracts'] });
});

test('the go reader qualifies tests with their package, and a package-level fail fails under it', () => {
  const out = [
    '{"Action":"start","Package":"tiny"}',
    '{"Action":"run","Package":"tiny","Test":"TestAdd"}',
    '{"Action":"pass","Package":"tiny","Test":"TestAdd"}',
    '{"Action":"output","Package":"tiny","Test":"TestBad","Output":"    bad.go:9: nope\\n"}',
    '{"Action":"fail","Package":"tiny","Test":"TestBad"}',
    '{"Action":"skip","Package":"tiny/noguards"}',
    'not json at all',
    '{"Action":"fail","Package":"tiny/broken"}',
  ].join('\n');
  assert.deepEqual(parseGoTest(out), { passed: ['tiny.TestAdd'], failed: ['tiny.TestBad', 'tiny/broken'] });
});

test('generation admits a breaking Rust mutant and records the stack', { skip: HAS_CARGO ? false : 'cargo is not on PATH' }, (ctx) => {
  const repo = mkdtempSync(join(tmpdir(), 'colonizer-synth-rust-'));
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => {
    for (const dir of [repo, poolDir]) rmSync(dir, { recursive: true, force: true });
  });
  writeFileSync(join(repo, 'Cargo.toml'), '[package]\nname = "tiny"\nversion = "0.1.0"\nedition = "2021"\n');
  mkdirSync(join(repo, 'src'));
  // Three candidates before the cutoff (`>`, `+`, `-`); the literals behind `#[cfg(test)]` are none.
  writeFileSync(
    join(repo, 'src/lib.rs'),
    [
      'pub fn add(a: i32, b: i32) -> i32 {',
      '    if a > 0 { a + b } else { a - b }',
      '}',
      '',
      '#[cfg(test)]',
      'mod tests {',
      '    use super::add;',
      '',
      '    #[test]',
      '    fn adds() {',
      '        assert_eq!(add(1, 2), 3);',
      '    }',
      '}',
      '',
    ].join('\n'),
  );
  mkdirSync(join(repo, 'tests'));
  writeFileSync(join(repo, 'tests/add.rs'), 'use tiny::add;\n\n#[test]\nfn adds_from_outside() {\n    assert_eq!(add(1, 2), 3);\n}\n');

  const tally = generate({ repo, pool: poolDir });
  assert.deepEqual({ candidates: tally.candidates, syntax: tally.syntax, survived: tally.survived, flaky: tally.flaky, admitted: tally.admitted }, { candidates: 4, syntax: 0, survived: 2, flaky: 0, admitted: 2 });
  const [nudge, swap] = loadPool(poolDir).heldout;
  for (const entry of [nudge, swap]) {
    assert.equal(entry.stack, 'rust');
    assert.equal(entry.source.file, 'src/lib.rs');
    // cargo test stops at the first failing target, so the unit test's failure is what the gate saw.
    assert.deepEqual(entry.gate.f2p, ['tests::adds']);
  }
  assert.deepEqual([nudge.mutation.from, nudge.mutation.to], ['0', '1']);
  assert.deepEqual([swap.mutation.from, swap.mutation.to], ['+', '-']);
});

test('a Rust reference that does not compile aborts generation, however green its output looks', { skip: HAS_CARGO ? false : 'cargo is not on PATH' }, (ctx) => {
  const repo = mkdtempSync(join(tmpdir(), 'colonizer-synth-rust-'));
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => {
    for (const dir of [repo, poolDir]) rmSync(dir, { recursive: true, force: true });
  });
  writeFileSync(join(repo, 'Cargo.toml'), '[package]\nname = "tiny"\nversion = "0.1.0"\nedition = "2021"\n');
  mkdirSync(join(repo, 'src'));
  writeFileSync(join(repo, 'src/lib.rs'), 'pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n'); // no closing brace

  assert.throws(() => generate({ repo, pool: poolDir }), /reference is not green/);
});

test('generation admits a breaking Go mutant and records the stack', { skip: HAS_GO ? false : 'go is not on PATH' }, (ctx) => {
  const repo = mkdtempSync(join(tmpdir(), 'colonizer-synth-go-'));
  const poolDir = mkdtempSync(join(tmpdir(), 'colonizer-synth-test-'));
  ctx.after(() => {
    for (const dir of [repo, poolDir]) rmSync(dir, { recursive: true, force: true });
  });
  writeFileSync(join(repo, 'go.mod'), 'module tiny\n\ngo 1.21\n');
  writeFileSync(join(repo, 'math.go'), 'package tiny\n\nfunc Add(a, b int) int {\n\treturn a + b\n}\n');
  writeFileSync(
    join(repo, 'math_test.go'),
    'package tiny\n\nimport "testing"\n\nfunc TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\t\tt.Fatal("Add(1, 2) is not 3")\n\t}\n}\n',
  );

  const tally = generate({ repo, pool: poolDir });
  assert.deepEqual({ candidates: tally.candidates, syntax: tally.syntax, survived: tally.survived, flaky: tally.flaky, admitted: tally.admitted }, { candidates: 1, syntax: 0, survived: 0, flaky: 0, admitted: 1 });
  const [entry] = loadPool(poolDir).heldout;
  assert.equal(entry.stack, 'go');
  assert.equal(entry.source.file, 'math.go');
  assert.deepEqual(entry.gate.f2p, ['tiny.TestAdd']);
});

test('a test run that dies without naming a failure is still a failure', (ctx) => {
  const dir = mkdtempSync(join(tmpdir(), 'colonizer-stacks-node-'));
  ctx.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'package.json'), '{}');
  assert.deepEqual(STACKS.node.runTests(dir, ['not-there.test.mjs']).failed, ['the test run failed']);
});

test('the go runner runs for real every time, cache or no cache', { skip: HAS_GO ? false : 'go is not on PATH' }, (ctx) => {
  const home = mkdtempSync(join(tmpdir(), 'colonizer-stacks-gocache-'));
  ctx.after(() => rmSync(home, { recursive: true, force: true }));
  const dir = join(home, 'module');
  mkdirSync(dir);
  writeFileSync(join(dir, 'go.mod'), 'module flip\n\ngo 1.21\n');
  // The flip lives outside the module, where go's result cache cannot see it — the same vantage a
  // flaky mutant's nondeterminism has. A replayed pass would keep this green forever.
  const marker = join(home, 'marker');
  writeFileSync(
    join(dir, 'flip_test.go'),
    [
      'package flip',
      '',
      'import (',
      '\t"os"',
      '\t"testing"',
      ')',
      '',
      'func TestFlip(t *testing.T) {',
      `\tif _, err := os.Stat(${JSON.stringify(marker)}); err == nil {`,
      '\t\tt.Fatal("the marker is there")',
      '\t}',
      '}',
      '',
    ].join('\n'),
  );
  assert.deepEqual(STACKS.go.runTests(dir).failed, [], 'green before the marker exists');
  writeFileSync(marker, 'x');
  assert.deepEqual(STACKS.go.runTests(dir).failed, ['flip.TestFlip'], 'green again, not a replay');
});
