// The type scale (issue #1084): every text size is a named step from typeScale.css, so one media
// query can raise them all on a phone. This pins the floors the phone scale promises (body 16px,
// secondary 14px, metadata 13px, nothing under 12px, fields 16px so iOS does not zoom on focus,
// 44px touch targets), that the desktop sizes are exactly the pixel sizes the steps replaced, and
// that no arbitrary `text-[Npx]` size has crept back in to sit outside the scale.

// Reading the stylesheets and walking the tree needs node builtins. vitest runs in plain node and
// resolves them fine, but this tsconfig types a browser build and carries no @types/node, so tsc
// must look away from exactly these three imports.
// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readdirSync, readFileSync } from "node:fs";
// @ts-expect-error node:path — no @types/node in this browser-facing tsconfig
import { join, relative } from "node:path";
// @ts-expect-error node:url — no @types/node in this browser-facing tsconfig
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC_DIR: string = fileURLToPath(new URL(".", import.meta.url));
const read = (path: string): string => readFileSync(fileURLToPath(new URL(path, import.meta.url)), "utf8");

const scaleCss = read("./typeScale.css");
const indexCss = read("./index.css");

/** `--text-<step>: <n>px;` declarations inside one CSS block, as step → px. */
function steps(block: string): Map<string, number> {
  const out = new Map<string, number>();
  for (const m of block.matchAll(/--text-([a-z-]+):\s*([\d.]+)px;/g)) out.set(m[1], Number(m[2]));
  return out;
}

/** The body of the first `{ … }` after `start`, braces balanced. */
function blockAfter(css: string, start: string): string {
  const at = css.indexOf(start);
  if (at === -1) throw new Error(`no ${start} in the stylesheet`);
  const open = css.indexOf("{", at);
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    if (css[i] === "{") depth++;
    if (css[i] === "}" && --depth === 0) return css.slice(open + 1, i);
  }
  throw new Error(`unbalanced block after ${start}`);
}

const PHONE_QUERY = "@media (max-width: 639.98px), (pointer: coarse)";
const desktop = steps(blockAfter(scaleCss, "@theme"));
const phoneBlock = blockAfter(scaleCss, PHONE_QUERY);
const phone = steps(blockAfter(phoneBlock, ":root"));
/** What a step measures on a phone: its phone value, or its desktop one when that is left alone. */
const onPhone = (step: string): number => phone.get(step) ?? desktop.get(step) ?? NaN;

describe("the type scale on a desktop", () => {
  it("is exactly the pixel sizes the arbitrary text-[Npx] classes used, so nothing moves", () => {
    expect(Object.fromEntries(desktop)).toEqual({
      "micro-xs": 8.5,
      "micro-sm": 9,
      micro: 9.5,
      "micro-lg": 10,
      "meta-sm": 10.5,
      meta: 11,
      "meta-lg": 11.5,
      small: 12,
      "small-lg": 12.5,
      "body-sm": 13,
      body: 13.5,
      "body-lg": 14,
      "lead-sm": 14.5,
      lead: 15,
      "title-sm": 16,
      title: 17,
      "title-lg": 18,
      "display-sm": 22,
      display: 24,
      "display-lg": 28,
      "display-xl": 30,
    });
  });

  it("sets the page's body text from the scale (14px on a desktop)", () => {
    expect(blockAfter(indexCss, "\nbody {")).toContain("font-size: var(--text-body-lg);");
    expect(indexCss).toContain('@import "./typeScale.css";');
  });
});

describe("the type scale on a phone", () => {
  it("applies below 640px and on any coarse pointer", () => {
    expect(scaleCss).toContain(PHONE_QUERY);
  });

  it("measures body text at 16px", () => {
    for (const step of ["body-sm", "body", "body-lg"]) expect(onPhone(step)).toBe(16);
  });

  it("keeps secondary text at 14px or more and metadata at 13px or more", () => {
    for (const step of ["small", "small-lg"]) expect(onPhone(step)).toBeGreaterThanOrEqual(14);
    for (const step of ["meta-sm", "meta", "meta-lg"]) expect(onPhone(step)).toBeGreaterThanOrEqual(13);
  });

  it("puts nothing under 12px", () => {
    for (const step of desktop.keys()) expect(onPhone(step), step).toBeGreaterThanOrEqual(12);
  });

  it("never shrinks a step, and keeps the steps in order", () => {
    const ordered = [...desktop.keys()];
    for (const step of ordered) expect(onPhone(step), step).toBeGreaterThanOrEqual(desktop.get(step) ?? 0);
    for (let i = 1; i < ordered.length; i++) expect(onPhone(ordered[i]), ordered[i]).toBeGreaterThanOrEqual(onPhone(ordered[i - 1]));
  });

  it("sets text fields at 16px or more, so iOS Safari does not zoom in on focus", () => {
    expect(phoneBlock).toMatch(/textarea,\s*select\s*\{\s*font-size: max\(16px, 1em\);/);
  });

  it("makes every tap target at least 44px tall, and icon buttons 44px wide", () => {
    expect(phoneBlock).toMatch(/summary\s*\{\s*min-height: 44px;/);
    expect(phoneBlock).toMatch(/\[role="button"\]\[aria-label\]\s*\{\s*min-width: 44px;/);
    for (const target of ['button:not([role="switch"])', '[role="tab"]', '[role="option"]', '[role="menuitem"]']) expect(phoneBlock).toContain(target);
  });

  it("keeps pinch zoom: the viewport sets no maximum-scale and no user-scalable=no", () => {
    const viewport = read("../index.html").match(/<meta name="viewport" content="([^"]*)"/)?.[1] ?? "";
    expect(viewport).toContain("width=device-width");
    expect(viewport).not.toMatch(/maximum-scale|user-scalable/);
  });
});

describe("the components", () => {
  it("size text only through the scale, never an arbitrary text-[Npx]", () => {
    const offenders: string[] = [];
    for (const entry of readdirSync(SRC_DIR, { withFileTypes: true, recursive: true })) {
      if (!entry.isFile() || !/\.(ts|tsx|css)$/.test(entry.name) || entry.name === "typeScale.test.ts") continue;
      const path = join(entry.parentPath, entry.name);
      const found = readFileSync(path, "utf8").match(/text-\[\d+(\.\d+)?px\]/g);
      if (found) offenders.push(`${relative(SRC_DIR, path)}: ${[...new Set(found)].join(", ")}`);
    }
    expect(offenders, "use a step from typeScale.css (text-meta, text-small, text-body, …)").toEqual([]);
  });

  it("set no fixed px font-size in the stylesheet outside the scale", () => {
    expect(indexCss.match(/font-size:\s*\d+(\.\d+)?px/g) ?? []).toEqual([]);
  });
});
