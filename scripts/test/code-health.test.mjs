// scripts/code-health.mjs: the report's sections and the hidden week-on-week block. Only the pure
// functions are driven here — main() is glue onto `gh` and the filesystem, so it is checked by a
// --dry-run against the real repository instead.
import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  aggregateFlakeRecords,
  buildReport,
  countRustTests,
  isTestFile,
  listRustFileSizes,
  parseAllowlist,
  parsePreviousData,
  parseTestCount,
  renderFlakes,
  renderHiddenBlock,
  renderLargestFiles,
  renderMergeTrain,
  renderReleaseHealth,
  renderTestTrend,
} from '../code-health.mjs';

const ALLOWLIST = [
  '# Non-test Rust source files over 2000 lines that predate the limit (issue #825).',
  '',
  'crates/colonizer/src/boot.rs   ',
  '# a comment',
  'crates/colonizer/src/chat.rs',
  '',
].join('\n');

test('parseAllowlist reads paths, dropping comments, blanks and trailing space', () => {
  assert.deepEqual([...parseAllowlist(ALLOWLIST)], [
    'crates/colonizer/src/boot.rs',
    'crates/colonizer/src/chat.rs',
  ]);
  assert.deepEqual([...parseAllowlist('')], []);
});

test('renderLargestFiles ranks the top ten, trends them and says the limit holds', () => {
  const current = Array.from({ length: 12 }, (_, i) => ({ path: `f${i}.rs`, lines: 3000 - i * 10 }));
  const out = renderLargestFiles(current, null, []);
  const rows = out.split('\n').filter((l) => l.startsWith('| `f'));
  assert.equal(rows.length, 10, 'only the top ten are listed');
  assert.match(rows[0], /\| `f0\.rs` \| 3000 \| new \|/);
  assert.ok(!out.includes('f10.rs'), 'the eleventh file is not in the table');
  assert.match(out, /✅ all non-test Rust files within 2000 lines/);
});

test('renderLargestFiles shows growth, shrinkage and no movement against the previous week', () => {
  const previous = [
    { path: 'grew.rs', lines: 100 },
    { path: 'shrank.rs', lines: 300 },
    { path: 'same.rs', lines: 200 },
  ];
  const out = renderLargestFiles(
    [
      { path: 'grew.rs', lines: 130 },
      { path: 'shrank.rs', lines: 250 },
      { path: 'same.rs', lines: 200 },
      { path: 'fresh.rs', lines: 210 },
    ],
    previous,
    ['grew.rs'],
  );
  assert.match(out, /\| `grew\.rs` \| 130 \| ↑30 \|/);
  assert.match(out, /\| `shrank\.rs` \| 250 \| ↓50 \|/);
  assert.match(out, /\| `same\.rs` \| 200 \| — \|/);
  assert.match(out, /\| `fresh\.rs` \| 210 \| new \|/);
  assert.match(out, /❌ 1 file over 2000 lines not on the allowlist — see #825: `grew\.rs`/);
});

test('isTestFile mirrors the exemption check-rust-file-size.sh applies', () => {
  for (const path of [
    'tests.rs',
    'crates/colonizer/src/gateway/tests.rs',
    'crates/colonizer/src/llm_tests.rs',
    'crates/colonizer/tests/route_table.rs',
  ]) {
    assert.ok(isTestFile(path), `${path} is a test file`);
  }
  for (const path of ['crates/colonizer/src/boot.rs', 'crates/colonizer/src/testsuite.rs', 'crates/x/testing.rs']) {
    assert.ok(!isTestFile(path), `${path} is not a test file`);
  }
});

test('a large test file is listed but does not count against the limit', () => {
  const out = renderLargestFiles([{ path: 'src/gateway/tests.rs', lines: 3400 }], null, []);
  assert.match(out, /\| `src\/gateway\/tests\.rs` \| 3400 \| new \|/);
  assert.match(out, /✅ all non-test Rust files within 2000 lines/);
});

test('renderMergeTrain says plainly that there is nothing to read yet', () => {
  assert.equal(
    renderMergeTrain(),
    '➖ not yet tracked — merge-train does not log conflict events yet.',
  );
});

/** A flake record as scripts/ci/retry-flaky.mjs appends it. */
const record = (failed, quarantined = false) => ({
  job: 'runner',
  command: 'cargo test',
  cwd: '.',
  attempt: 2,
  failed_tests: failed,
  quarantined,
  run_url: 'https://github.com/Colonizer-dev/harness/actions/runs/1',
});

test('aggregateFlakeRecords dedups by name and splits new from quarantined', () => {
  const records = [
    record(['alpha', 'beta']),
    record(['alpha']),
    record(['gamma'], true),
  ];
  const { newlyFlaky, alreadyQuarantined } = aggregateFlakeRecords(records, new Set());
  assert.deepEqual(newlyFlaky, ['alpha', 'beta']);
  assert.deepEqual(alreadyQuarantined, ['gamma']);
});

test('a name on the quarantine list is known even when the record did not say so', () => {
  const { newlyFlaky, alreadyQuarantined } = aggregateFlakeRecords(
    [record(['known::test', 'unknown::test'])],
    new Set(['known::test']),
  );
  assert.deepEqual(newlyFlaky, ['unknown::test']);
  assert.deepEqual(alreadyQuarantined, ['known::test']);
});

test('a name seen both as new and as quarantined is counted as known, not as a new flake', () => {
  const { newlyFlaky, alreadyQuarantined } = aggregateFlakeRecords(
    [record(['twice']), record(['twice'], true)],
    new Set(),
  );
  assert.deepEqual(newlyFlaky, []);
  assert.deepEqual(alreadyQuarantined, ['twice']);
});

test('a record with no readable test names contributes nothing', () => {
  assert.deepEqual(aggregateFlakeRecords([{ ...record([]), failed_tests: undefined }], new Set()), {
    newlyFlaky: [],
    alreadyQuarantined: [],
  });
});

test('renderFlakes passes when nothing new flaked, naming the quarantined ones', () => {
  assert.match(renderFlakes({ newlyFlaky: [], alreadyQuarantined: [] }), /^✅ no new flakes/);
  assert.match(
    renderFlakes({ newlyFlaky: [], alreadyQuarantined: ['a::b'] }),
    /✅ no new flakes in the last 7 days of CI \(1 quarantined in scripts\/flaky-tests\.txt\)/,
  );
});

test('renderFlakes fails on a new flake, listing it and pointing at the flake reporting issue', () => {
  const out = renderFlakes({ newlyFlaky: ['alpha'], alreadyQuarantined: ['gamma'] });
  assert.match(out, /^❌ 1 new flake in the last 7 days of CI — see #977/);
  assert.match(out, /New: `alpha`/);
  assert.match(out, /Quarantined \(known\): `gamma`/);
});

test('renderReleaseHealth points at the open issue, or says there is none', () => {
  assert.match(
    renderReleaseHealth({ number: 7, title: 'release-health: v0.2.0', url: 'https://x/7' }),
    /^❌ a release-health issue is open: https:\/\/x\/7/,
  );
  assert.equal(renderReleaseHealth(null), '✅ no open release-health issues');
});

test('parseTestCount sums git grep -c output, ignoring files with no match', () => {
  assert.equal(
    parseTestCount('crates/a/src/lib.rs:12\ncrates/b/src/main.rs:7\nweb/src/thing.ts:3\n'),
    22,
  );
  assert.equal(parseTestCount(''), 0);
  assert.equal(parseTestCount('\n  \n'), 0);
});

test('countRustTests counts this repository\'s test attributes', () => {
  assert.ok(countRustTests() > 100, 'this repository has more than a hundred test attributes');
});

test('listRustFileSizes returns tracked Rust files, largest first', () => {
  const sizes = listRustFileSizes();
  assert.ok(sizes.length > 0, 'the repository has Rust files');
  assert.ok(sizes.every((s) => s.path.endsWith('.rs') && Number.isInteger(s.lines)));
  for (let i = 1; i < sizes.length; i += 1) assert.ok(sizes[i - 1].lines >= sizes[i].lines);
});

test('renderTestTrend deltas against the previous week and marks the first run', () => {
  assert.match(
    renderTestTrend({ count: 800, minutes: 22 }, { count: 760, minutes: 25 }),
    /- Test count: 800 \(↑40\)\n- Last CI run on main: 22 min \(↓3\)/,
  );
  assert.equal(
    renderTestTrend({ count: 800, minutes: 22 }, null),
    '- Test count: 800\n- Last CI run on main: 22 min',
  );
});

test('renderTestTrend shows no movement as an em dash', () => {
  assert.match(renderTestTrend({ count: 800, minutes: 22 }, { count: 800, minutes: 22 }), /800 \(—\)/);
});

test('renderTestTrend keeps the test count when there is no run to time', () => {
  assert.equal(
    renderTestTrend({ count: 800, minutes: undefined }, null),
    '- Test count: 800\n- Last CI run on main: ➖ no completed main run found',
  );
});

test('the hidden block round-trips through parsePreviousData', () => {
  const data = {
    largestFiles: [{ path: 'crates/colonizer/src/boot.rs', lines: 4321 }],
    testCount: 812,
    testMinutes: 24,
  };
  const body = `Some prose.\n\n${renderHiddenBlock(data)}\n`;
  assert.deepEqual(parsePreviousData(body), data);
  assert.match(renderHiddenBlock(data), /^<!-- code-health-data\n\{.*\}\n-->$/s);
});

test('parsePreviousData is null for a body with no block, a damaged one, or none at all', () => {
  assert.equal(parsePreviousData('no data here'), null);
  assert.equal(parsePreviousData(''), null);
  assert.equal(parsePreviousData(null), null);
  assert.equal(parsePreviousData('<!-- code-health-data\n{not json}\n-->'), null);
  assert.equal(parsePreviousData('<!-- code-health-data\n"a string"\n-->'), null);
});

test('buildReport puts the sections in order and the hidden block last, and it round-trips', () => {
  const data = { largestFiles: [{ path: 'boot.rs', lines: 4321 }], testCount: 812, testMinutes: 24 };
  const report = buildReport({
    largestFiles: 'LARGEST',
    flakes: 'FLAKES',
    releaseHealth: 'RELEASE',
    testTrend: 'TREND',
    mergeTrain: 'MERGE',
    hiddenBlock: renderHiddenBlock(data),
  });
  const order = ['# Code health', '## Largest files', '## Flaky tests', '## Release health', '## Test count and time', '## Merge-train conflict hotspots'];
  let at = -1;
  for (const heading of order) {
    const found = report.indexOf(heading);
    assert.ok(found > at, `${heading} is missing or out of order`);
    at = found;
  }
  for (const text of ['LARGEST', 'FLAKES', 'RELEASE', 'TREND', 'MERGE']) {
    assert.ok(report.includes(text), `${text} is missing`);
  }
  assert.ok(report.includes(renderHiddenBlock(data)), 'the hidden block is last');
  assert.deepEqual(parsePreviousData(report), data, 'next week can read its numbers back');
});
