// The panels that open over the page (notifications, Ask, Colonize, the header menus) are solid:
// nothing behind them shows through or is blurred into them, and their faint text is lifted to a
// colour that meets AA on the panel.

// @ts-expect-error node:fs — no @types/node in this browser-facing tsconfig
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css: string = readFileSync(new URL("./spotlight.css", import.meta.url), "utf8");

/** The declarations of the first rule whose selector list is exactly `selector`. */
function rule(selector: string): string {
  const at = css.search(new RegExp(`(^|\\n)${selector.replace(/[.]/g, "\\.")}\\s*\\{`));
  expect(at).toBeGreaterThanOrEqual(0);
  return css.slice(css.indexOf("{", at) + 1, css.indexOf("}", at));
}

describe("the panel surface", () => {
  it("is the opaque panel colour, with no backdrop blur", () => {
    const panel = rule(".spot-panel");
    expect(panel).toMatch(/background:\s*var\(--panel\);/);
    expect(panel).not.toMatch(/transparent|backdrop-filter/);
  });

  it("reads faint text as muted inside a panel", () => {
    expect(rule(".spot-panel")).toMatch(/--faint:\s*var\(--muted\);/);
  });
});
