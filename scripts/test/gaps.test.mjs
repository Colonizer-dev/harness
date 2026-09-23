// docs/gaps.md lists what the design (vision.md, the website's illustrations, the prototypes the UI was
// ported from) shows that the code does not build yet. A drift log nobody updates is worse than none,
// because it reads as authoritative, so this pins what can be checked from here: vision.md and this
// repository's README link the page, every repository path it cites still exists, and every gap
// names its tracking issue or says plainly that none is filed. The website, its illustrations and
// its README live in another repository; keeping the rows true to them is the pull request
// template's job, not this test's.
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const GAPS = join(ROOT, 'docs/gaps.md');
const read = (path) => readFileSync(join(ROOT, path), 'utf8');
const ISSUE = /^\[#(\d+)\]\(https:\/\/github\.com\/Colonizer-dev\/harness\/issues\/\1\)$/;

/** The body of a `## ` section, up to the next one. */
function section(markdown, title) {
  const start = markdown.search(new RegExp(`^## ${title}\\r?$`, 'm'));
  assert.notEqual(start, -1, `docs/gaps.md has no "## ${title}" section`);
  const end = markdown.indexOf('\n## ', start);
  return markdown.slice(start, end === -1 ? undefined : end);
}

const cells = (line) => line.trim().replace(/^\||\|$/g, '').split(/(?<!\\)\|/).map((cell) => cell.trim());

test('the register exists and is linked from vision.md and the README', () => {
  assert.ok(existsSync(GAPS), 'docs/gaps.md is missing');
  assert.match(read('docs/vision.md'), /\]\(gaps\.md\)/, 'docs/vision.md does not link gaps.md');
  assert.match(read('README.md'), /\]\(docs\/gaps\.md\)/, 'README.md does not link docs/gaps.md');
});

test('every repository path and relative link in the register exists', () => {
  const gaps = read('docs/gaps.md');
  // A backticked token is a repository path when its first segment is an entry at the root, so
  // `web/src/App.tsx` and `README.md` are checked and `colonizer-website/index.html` is not. A
  // `:48`, `#L12` or `#anchor` suffix is dropped first.
  const roots = new Set(readdirSync(ROOT).filter((entry) => entry !== '.git'));
  for (const [, token] of gaps.matchAll(/`([^`\n]+)`/g)) {
    const path = token.replace(/[:#].*$/, '');
    if (roots.has(path.split('/')[0])) assert.ok(existsSync(join(ROOT, path)), `docs/gaps.md cites \`${token}\`, which does not exist`);
  }
  for (const [, target] of gaps.matchAll(/\]\(([^)#\s]+)(?:#[^)]*)?\)/g)) {
    if (!/^[a-z]+:/.test(target)) assert.ok(existsSync(join(ROOT, 'docs', target)), `docs/gaps.md links ${target}, which does not exist`);
  }
});

test('every Not built row names its tracking issue or says none is filed', () => {
  const body = section(read('docs/gaps.md'), 'Not built');
  const [header, , ...rows] = body.split(/\r?\n/).filter((line) => line.startsWith('|'));
  // An empty register is allowed, but only said out loud: a table that vanished or stopped parsing
  // must not pass as "nothing missing".
  if (!header) return assert.match(body, /^Nothing is known to be missing\.$/m, 'the Not built section has no table');
  assert.deepEqual(cells(header), ['Element', 'Where the design shows it', 'What the code has instead', 'Issue']);
  assert.ok(rows.length > 0, 'the Not built table has no rows; drop it and say "Nothing is known to be missing." instead');
  for (const row of rows) {
    const [element, , , issue, ...extra] = cells(row);
    assert.ok(issue !== undefined && extra.length === 0, `malformed row in docs/gaps.md: ${row}`);
    assert.ok(
      issue === 'none filed' || issue.split(/,\s*/).every((link) => ISSUE.test(link)),
      `"${element}" in docs/gaps.md needs a https://github.com/Colonizer-dev/harness/issues/N link or "none filed", not "${issue}"`,
    );
  }
});
