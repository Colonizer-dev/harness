// scripts/check-doc-links.mjs: every relative link in the repository's Markdown resolves. The anchor
// rule is GitHub's, so it is pinned here on the headings this repository actually writes; the file
// check runs against a throwaway tree, and the last test runs the real check over the repository.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { anchors, check, links, slug, stripCode } from '../check-doc-links.mjs';

const SCRIPT = resolve(dirname(fileURLToPath(import.meta.url)), '../check-doc-links.mjs');
const scratch = mkdtempSync(join(tmpdir(), 'doc-links-'));
after(() => rmSync(scratch, { recursive: true, force: true }));

test('slugs follow GitHub: lower case, punctuation dropped, spaces to dashes', () => {
  assert.equal(slug('Adding a module'), 'adding-a-module');
  assert.equal(slug('`colonizer diff` and `map`'), 'colonizer-diff-and-map');
  assert.equal(slug('Remote access (experimental)'), 'remote-access-experimental');
  assert.equal(slug('What it is, and how to turn it on'), 'what-it-is-and-how-to-turn-it-on');
  assert.equal(slug('snake_case stays'), 'snake_case-stays');
  assert.equal(slug('GET /api/sessions/{id}'), 'get-apisessionsid');
});

test('repeated headings are numbered, and explicit anchors count', () => {
  const found = anchors('# Top\n## Limits\ntext\n## Limits\n<a id="custom"></a>\n```\n# not a heading\n```\n');
  assert.ok(found.has('top'));
  assert.ok(found.has('limits'));
  assert.ok(found.has('limits-1'));
  assert.ok(found.has('custom'));
  assert.ok(!found.has('not-a-heading'));
  assert.ok(anchors("### `host`\n").has("host"));
});

test('links in code are not links', () => {
  const md = 'see [a](a.md) and `[b](b.md)`\n```\n[c](c.md)\n```\n[d]: d.md\n<img src="e.png">\n';
  assert.deepEqual(
    links(md).map((l) => l.target),
    ['a.md', 'd.md', 'e.png'],
  );
  assert.equal(stripCode('```\nx\n```\ny').split('\n').length, 4);
});

test('a missing file and a missing anchor are both reported, externals are not', () => {
  mkdirSync(join(scratch, 'docs'), { recursive: true });
  writeFileSync(join(scratch, 'docs/a.md'), '# A\n## Real heading\n');
  writeFileSync(
    join(scratch, 'README.md'),
    [
      '[ok](docs/a.md#real-heading)',
      '[self](#top)',
      '[gone](docs/b.md)',
      '[bad anchor](docs/a.md#nope)',
      '[web](https://example.com/x.md)',
      '# Top',
    ].join('\n'),
  );
  const problems = check('README.md', { root: scratch });
  assert.equal(problems.length, 2, problems.join('\n'));
  assert.match(problems[0], /README\.md:3: docs\/b\.md: no such file/);
  assert.match(problems[1], /README\.md:4: .*#nope/);
});

test('a changelog fragment links from the root, where a release puts it', () => {
  mkdirSync(join(scratch, 'changelog.d'), { recursive: true });
  writeFileSync(join(scratch, 'changelog.d/1.added.md'), '**X.** See [a](docs/a.md). ([#1])\n');
  assert.deepEqual(check('changelog.d/1.added.md', { root: scratch }), []);
});

test("this repository's Markdown has no broken relative link", () => {
  execFileSync(process.execPath, [SCRIPT], { stdio: 'pipe' });
});
