// The poll schedule contract (issue #159): every poll loop App.tsx used to own as a setInterval
// has exactly one tick name with its exact cadence, so neither the worker nor the fallback can
// silently drop or slow a poll. Pure data, so it pins in the plain node environment.
import { describe, expect, it } from "vitest";

import { POLL_CADENCES, POLL_TICK_NAMES, type PollTickName } from "./pollSchedule";

describe("pollSchedule", () => {
  it("keeps the session poll at 4 s", () => {
    expect(POLL_CADENCES.sessions).toBe(4000);
  });

  it("carries every poll with its cadence", () => {
    expect(POLL_CADENCES).toEqual({
      sessions: 4_000,
      redRuns: 5_000,
      pendingMemory: 10_000,
      orgs: 15_000,
      status: 30_000,
      fleet: 30_000,
      update: 900_000,
    });
  });

  it("names each cadence exactly once", () => {
    const names = Object.keys(POLL_CADENCES);
    expect(new Set(names).size).toBe(names.length);
    expect([...POLL_TICK_NAMES].sort()).toEqual([...names].sort());
  });

  it("gives every tick name a positive cadence", () => {
    const names: PollTickName[] = ["sessions", "redRuns", "pendingMemory", "orgs", "status", "fleet", "update"];
    for (const name of names) expect(POLL_CADENCES[name]).toBeGreaterThan(0);
  });
});
