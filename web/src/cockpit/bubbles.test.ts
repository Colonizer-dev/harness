// Pins what the carrier ants say: every settler state, the 72-char clip, the colony
// fallback, and the overview density rule.
import { describe, expect, it } from "vitest";

import type { SubagentState, ToolRef } from "../sessionStream";
import {
  BUBBLE_MAX_LENGTH,
  BUBBLE_TONE,
  MAX_ANT_BUBBLES,
  colonySays,
  planAntBubbles,
  settlerSays,
} from "./bubbles";

const readNest: ToolRef = { name: "Read", input: { file_path: "src/cockpit/nest.ts" } };

function settler(
  state: SubagentState,
  extra: Partial<{ current: ToolRef | null; last: ToolRef | null; report: string }> = {},
): { state: SubagentState; current: ToolRef | null; last: ToolRef | null; report: string } {
  return { state, current: null, last: null, report: "", ...extra };
}

describe("settlerSays", () => {
  const cases: [SubagentState, { current?: ToolRef | null; last?: ToolRef | null; report?: string }, string][] = [
    ["working", { current: readNest }, "Reading nest.ts"],
    ["working", { last: readNest }, "Reading nest.ts"],
    ["working", {}, "Working…"],
    ["thinking", { last: readNest }, "Thinking after reading nest.ts"],
    ["thinking", {}, "Thinking…"],
    ["writing", { report: "Drafting the summary\nThe rest." }, "Drafting the summary"],
    ["writing", {}, "Writing its report"],
    ["done", { report: "Fixed the checkout\nDetails." }, "Done: Fixed the checkout"],
    ["done", {}, "Done"],
    ["continued", {}, "Carried on further down"],
  ];

  it.each(cases)("says %s", (state, extra, text) => {
    expect(settlerSays(settler(state, extra)).text).toBe(text);
  });

  it("clips long readings to one short line, keeping the full string in the title", () => {
    const line = `Drafting ${"a-very-long-summary ".repeat(6)}tonight`;
    const said = settlerSays(settler("writing", { report: `${line}\nThe rest.` }));
    expect(said.text.endsWith("…")).toBe(true);
    expect(said.text.length).toBeLessThanOrEqual(BUBBLE_MAX_LENGTH);
    expect(said.title).toBe(line);
    expect(said.title.length).toBeGreaterThan(BUBBLE_MAX_LENGTH);
  });

  it("tones every state", () => {
    expect(Object.keys(BUBBLE_TONE).sort()).toEqual(["continued", "done", "thinking", "working", "writing"]);
  });
});

describe("colonySays", () => {
  it("prefers the trimmed live detail over the feed line", () => {
    expect(colonySays("  Cloning acme/webshop  ", "webshop#42 is working").text).toBe("Cloning acme/webshop");
  });

  it("falls back to the feed line when the live detail is null or blank", () => {
    expect(colonySays(null, "webshop#42 is working").text).toBe("webshop#42 is working");
    expect(colonySays("   ", "webshop#42 is working").text).toBe("webshop#42 is working");
  });
});

describe("planAntBubbles", () => {
  it("keeps every rider when the crew fits", () => {
    expect(planAntBubbles(["a", "b"])).toEqual(["a", "b"]);
  });

  it("drops the oldest first, keeping the newest four", () => {
    expect(MAX_ANT_BUBBLES).toBe(4);
    expect(planAntBubbles(["a", "b", "c", "d", "e", "f"])).toEqual(["c", "d", "e", "f"]);
  });

  it("shows nothing at a zero limit", () => {
    expect(planAntBubbles(["a", "b"], 0)).toEqual([]);
  });
});
