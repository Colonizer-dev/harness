#!/usr/bin/env node
// Relative links in the repository's Markdown must resolve: the file (or directory) a link names has
// to exist, and a `#fragment` has to match a heading, or an explicit `<a id>`/`<a name>`, in the
// Markdown file it points at. Docs rot quietly when a file is renamed or a heading reworded, and a
// reader only finds out by clicking; CI (`scripts` job) runs this so the pull request that breaks a
// link is the one that fails.
//
//   node scripts/check-doc-links.mjs              check every tracked *.md file
//   node scripts/check-doc-links.mjs README.md    check the named files only
//
// Checked: inline links and images (`[text](path#frag)`, `![alt](path)`), reference definitions
// (`[ref]: path`), and `href`/`src` attributes in inline HTML. Not checked: absolute URLs
// (`https:`, `mailto:` and the like), links inside fenced or inline code, and the released history
// in CHANGELOG.md, whose old entries may name files that have since moved. A changelog.d/ fragment
// is checked as if it sat at the root, since that is where a release puts its text. vendor/ holds
// third-party packs and is skipped too. Anchors follow GitHub's rule: lower-case the heading text,
// drop everything that is not a letter, digit, space, `-` or `_`, turn spaces into `-`, and number
// repeats `-1`, `-2`, … in document order.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SKIP = [/^vendor\//, /^CHANGELOG\.md$/, /(^|\/)node_modules\//];

/**
 * Replace fenced code blocks, and unless `keepSpans` inline code spans too, with blanks, keeping
 * line numbers. Headings keep their spans: `### \`host\`` is anchored as `host`.
 */
export function stripCode(markdown, { keepSpans = false } = {}) {
  const out = [];
  let fence = null;
  for (const line of markdown.split('\n')) {
    const open = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (fence) {
      const close = line.match(/^\s{0,3}(`{3,}|~{3,})\s*$/);
      if (close && close[1][0] === fence[0] && close[1].length >= fence.length) fence = null;
      out.push('');
      continue;
    }
    if (open) {
      fence = open[1];
      out.push('');
      continue;
    }
    out.push(keepSpans ? line : line.replace(/(`+)[\s\S]*?\1/g, (m) => ' '.repeat(m.length)));
  }
  return out.join('\n');
}

/** GitHub's anchor for a heading's text. */
export function slug(text) {
  return text
    .replace(/<[^>]+>/g, '')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[*_`]/g, (c) => (c === '_' ? '_' : ''))
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\p{M} _-]/gu, '')
    .replace(/ /g, '-');
}

/** Every anchor a Markdown document defines: its headings (numbered on repeats) and explicit ids. */
export function anchors(markdown) {
  const found = new Set();
  const seen = new Map();
  const text = stripCode(markdown, { keepSpans: true });
  for (const line of text.split('\n')) {
    const heading = line.match(/^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$/);
    if (heading) {
      const base = slug(heading[1]);
      const n = seen.get(base) ?? 0;
      seen.set(base, n + 1);
      found.add(n === 0 ? base : `${base}-${n}`);
    }
    for (const m of line.matchAll(/<a\s[^>]*(?:id|name)\s*=\s*["']([^"']+)["']/gi)) found.add(m[1]);
  }
  // Setext headings (a line of text underlined with === or ---).
  const lines = text.split('\n');
  for (let i = 1; i < lines.length; i++) {
    if (/^\s{0,3}(=+|-+)\s*$/.test(lines[i]) && lines[i - 1].trim() && !/^\s*[-*+|>]/.test(lines[i - 1])) {
      if (/^\s{0,3}-+\s*$/.test(lines[i]) && lines[i - 1].includes('|')) continue;
      const base = slug(lines[i - 1]);
      const n = seen.get(base) ?? 0;
      seen.set(base, n + 1);
      found.add(n === 0 ? base : `${base}-${n}`);
    }
  }
  return found;
}

/** The link targets in a Markdown document, with the line each is on. */
export function links(markdown) {
  const out = [];
  const lines = stripCode(markdown).split('\n');
  lines.forEach((line, i) => {
    const push = (target) => out.push({ line: i + 1, target: target.trim().replace(/^<|>$/g, '') });
    for (const m of line.matchAll(/!?\[(?:[^\]\\]|\\.)*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/g)) push(m[1]);
    const def = line.match(/^\s{0,3}\[[^\]]+\]:\s+(\S+)/);
    if (def) push(def[1]);
    for (const m of line.matchAll(/\s(?:href|src)\s*=\s*["']([^"']+)["']/gi)) push(m[1]);
  });
  return out;
}

const isExternal = (target) => /^[a-z][a-z0-9+.-]*:/i.test(target) || target.startsWith('//');

/** Problems in one file, as `path:line: message` strings. `read` and `exists` are injectable for tests. */
export function check(file, { root = ROOT, read = (p) => readFileSync(p, 'utf8'), exists = existsSync } = {}) {
  const problems = [];
  const abs = join(root, file);
  const cache = new Map();
  const anchorsOf = (path) => {
    if (!cache.has(path)) cache.set(path, anchors(read(path)));
    return cache.get(path);
  };
  for (const { line, target } of links(read(abs))) {
    if (!target || isExternal(target)) continue;
    const hash = target.indexOf('#');
    const pathPart = decodeURIComponent(hash === -1 ? target : target.slice(0, hash)).replace(/\?.*$/, '');
    const fragment = hash === -1 ? '' : decodeURIComponent(target.slice(hash + 1));
    // A changelog fragment is folded into CHANGELOG.md at a release, so its links are written
    // relative to the repository root, where they will end up.
    const base = /^changelog\.d\/(?!README\.md$)/.test(file) ? root : dirname(abs);
    const dest = pathPart === '' ? abs : pathPart.startsWith('/') ? join(root, pathPart) : resolve(base, pathPart);
    if (!dest.startsWith(root)) {
      problems.push(`${file}:${line}: ${target} leaves the repository`);
      continue;
    }
    if (!exists(dest)) {
      problems.push(`${file}:${line}: ${target}: no such file ${relative(root, dest) || '.'}`);
      continue;
    }
    if (!fragment || !/\.md$/i.test(dest)) continue;
    if (/^L\d+(-L\d+)?$/.test(fragment)) continue;
    let isFile = true;
    try {
      isFile = statSync(dest).isFile();
    } catch {
      // An injected `exists` in a test; treat it as a file.
    }
    if (!isFile) continue;
    if (!anchorsOf(dest).has(fragment.toLowerCase()) && !anchorsOf(dest).has(fragment)) {
      problems.push(`${file}:${line}: ${target}: no heading or anchor #${fragment} in ${relative(root, dest)}`);
    }
  }
  return problems;
}

function trackedMarkdown() {
  const out = execFileSync('git', ['ls-files', '-z', '--', '*.md'], { cwd: ROOT, encoding: 'utf8' });
  return out
    .split('\0')
    .filter(Boolean)
    .filter((f) => !SKIP.some((re) => re.test(f)))
    .filter((f) => existsSync(join(ROOT, f)));
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const files = process.argv.length > 2 ? process.argv.slice(2) : trackedMarkdown();
  const problems = files.flatMap((f) => check(f));
  for (const p of problems) console.error(p);
  if (problems.length) {
    console.error(`\n${problems.length} broken link(s) in ${files.length} file(s). Fix the path or the anchor.`);
    process.exit(1);
  }
  console.log(`${files.length} Markdown file(s): every relative link resolves.`);
}
