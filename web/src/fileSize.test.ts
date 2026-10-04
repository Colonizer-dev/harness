// The file-size gate: no source file under web/src may reach 1,500 lines. A module that large is a
// module nobody reviews, and it is almost always several features wearing one filename, so the fix
// is to split it along the feature it already belongs to (web/src/features/<feature>/), the way the
// cockpit is being carved up. The limit applies to tests and stylesheets too — a 900-line test file
// is the same problem as a 900-line component.

// Walking the tree and reading each file needs node builtins. vitest runs in plain node and resolves
// them fine, but this tsconfig types a browser build and carries no @types/node, so tsc must look
// away from exactly these three imports.
// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readdirSync, readFileSync } from "node:fs";
// @ts-expect-error node:path — no @types/node in this browser-facing tsconfig
import { join, relative } from "node:path";
// @ts-expect-error node:url — no @types/node in this browser-facing tsconfig
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const MAX_LINES = 1500;

// Files that are over the limit today and are being split, each with the cap it must not grow past.
// The list may only shrink: lower a cap as the file shrinks, and delete the entry the moment the
// file is under MAX_LINES. Never add an entry — split the file instead.
const ALLOWED: Record<string, number> = {
  "index.css": 2133, // global stylesheet; split it by feature to remove this entry
};

const SRC_DIR = fileURLToPath(new URL(".", import.meta.url));
const SOURCE_FILE = /\.(ts|tsx|css)$/;

/** Every .ts/.tsx/.css file under web/src with its line count, keyed by path relative to web/src —
 *  the same form ALLOWED uses ("mock.ts", "cockpit/ChatView.tsx"). */
function sourceFiles(): Map<string, number> {
  const files = new Map<string, number>();
  for (const entry of readdirSync(SRC_DIR, { withFileTypes: true, recursive: true })) {
    if (!entry.isFile() || !SOURCE_FILE.test(entry.name)) continue;
    const path = join(entry.parentPath, entry.name);
    // A trailing newline ends the last line without starting another, so count separators.
    files.set(relative(SRC_DIR, path), readFileSync(path, "utf8").split("\n").length - 1);
  }
  return files;
}

const named = (pairs: [string, number][]): string[] => pairs.map(([path, lines]) => `${path} (${lines} lines)`);
const splitByFeature = (rows: string[]): string =>
  `split each file by the feature it belongs to, under web/src/features/<feature>/, into files under ${MAX_LINES} lines:\n  ${rows.join("\n  ")}`;

describe("web/src file sizes", () => {
  const files = sourceFiles();

  it(`no source file exceeds ${MAX_LINES} lines`, () => {
    const offenders = [...files].filter(([path, lines]) => lines > MAX_LINES && !(path in ALLOWED));
    expect(offenders.length, splitByFeature(named(offenders))).toBe(0);
  });

  it("no allowlisted file has grown past its cap", () => {
    const grown: [string, number][] = [];
    for (const [path, cap] of Object.entries(ALLOWED)) {
      const lines = files.get(path) ?? 0;
      if (lines > cap) grown.push([path, lines]);
    }
    const rows = grown.map(([path, lines]) => `${path} (${lines} lines, cap ${ALLOWED[path]})`);
    expect(grown.length, `these allowlisted files have grown past their cap:\n  ${rows.join("\n  ")}`).toBe(0);
  });

  it("keeps the allowlist to files that are still over the limit", () => {
    const stale = Object.keys(ALLOWED).filter((path) => (files.get(path) ?? 0) <= MAX_LINES);
    expect(
      stale.length,
      `delete these allowlist entries — the files are already under ${MAX_LINES} lines (or gone):\n  ${stale.join("\n  ")}`,
    ).toBe(0);
  });
});
