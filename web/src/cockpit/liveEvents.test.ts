import { describe, expect, it } from "vitest";

import { BUMP_MS, FLASH_MS, diffSessions, isBumped, isFlashed, type LiveEvents } from "./liveEvents";
import { session } from "./testFixtures";

describe("diffSessions", () => {
  it("says nothing about the first list: a page load is not news", () => {
    expect(diffSessions(null, [session()], 1)).toEqual([]);
    expect(diffSessions([], [session()], 1)).toEqual([]);
  });

  it("names a status move in plain words", () => {
    const before = session({ id: "a", repo: "harness", issue: 7, status: "running" });
    const after = { ...before, status: "waiting_for_answer" as const };
    expect(diffSessions([before], [after], 5)).toEqual([{ id: "a", kind: "asked", text: "harness #7 asked a question", at: 5 }]);
  });

  it("reads a waiting colony going back to work as resumed, and a new one as started or queued", () => {
    const waiting = session({ id: "a", repo: "r", issue: 1, status: "waiting_for_answer" });
    const kinds = diffSessions(
      [waiting],
      [{ ...waiting, status: "running" }, session({ id: "b", repo: "r", issue: 2, status: "queued" })],
      0,
    ).map((e) => [e.kind, e.text]);
    expect(kinds).toEqual([
      ["resumed", "r #1 resumed"],
      ["started", "r #2 queued"],
    ]);
  });

  it("reports a cost rise, and nothing when the cost is unmeasured or unchanged", () => {
    const a = session({ id: "a", repo: "r", issue: 1, cost_usd: 1, routed_cost_usd: null });
    expect(diffSessions([a], [{ ...a, cost_usd: 1.25 }], 0).map((e) => e.kind)).toEqual(["spent"]);
    expect(diffSessions([a], [a], 0)).toEqual([]);
    const unmeasured = session({ id: "b", cost_usd: null, routed_cost_usd: null });
    expect(diffSessions([unmeasured], [unmeasured], 0)).toEqual([]);
  });
});

describe("flash windows", () => {
  const events: LiveEvents = { latest: null, recent: [], flashed: { a: 1000 }, bumped: { a: 1000 } };

  it("holds a flash and a bump only for their windows", () => {
    expect(isFlashed(events, "a", 1000 + FLASH_MS - 1)).toBe(true);
    expect(isFlashed(events, "a", 1000 + FLASH_MS)).toBe(false);
    expect(isBumped(events, "a", 1000 + BUMP_MS - 1)).toBe(true);
    expect(isBumped(events, "a", 1000 + BUMP_MS)).toBe(false);
    expect(isFlashed(events, "b", 1000)).toBe(false);
  });
});
