import { describe, expect, it } from "vitest";

import { questionOf, watchdogFlagged } from "./questions";
import { session } from "./testFixtures";

describe("questionOf", () => {
  it("takes the question out of a waiting diagnosis", () => {
    expect(questionOf({ state: "waiting_on_human", text: "waiting for an answer: Retry the subagents?" })).toBe("Retry the subagents?");
  });

  it("says nothing when the diagnosis names no question", () => {
    expect(questionOf({ state: "waiting_on_human", text: "waiting for an answer" })).toBeNull();
    expect(questionOf({ state: "waiting_on_human", text: "idle, waiting for the next message" })).toBeNull();
    expect(questionOf({ state: "working", text: "waiting for an answer: stale" } as never)).toBeNull();
    expect(questionOf(null)).toBeNull();
  });
});

describe("watchdogFlagged", () => {
  it("is not the watchdog when the flag only records an open question", () => {
    expect(watchdogFlagged(session({ attention: { reason: "waiting_for_answer", nudges: 0 } as never }))).toBe(false);
    expect(watchdogFlagged(session({ attention: { reason: "stalled", nudges: 2 } as never }))).toBe(true);
    expect(watchdogFlagged(session({ attention: null as never }))).toBe(false);
  });
});
