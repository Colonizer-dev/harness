// The BOOT section's pure half (issue #360): phases arrive in boot order and render in that order,
// exactly one is flagged slowest, and the summary line says how far a starting boot got or what a
// finished one took. A colony with no timing and no boot under way gets no section at all.
import { describe, expect, it } from "vitest";

import { bootView } from "./bootTiming";

const FULL = {
  total_ms: 94_320,
  phases: [
    { name: "issue", ms: 240 },
    { name: "git", ms: 1_180 },
    { name: "providers", ms: 310 },
    { name: "mesh-start", ms: 2_080 },
    { name: "image-pull", ms: 1_900 },
    { name: "vm-boot", ms: 86_400 },
    { name: "mesh-join", ms: 1_460 },
    { name: "agentd", ms: 720 },
  ],
};

describe("bootView", () => {
  it("a finished boot lists every phase in order, flags the slowest, and totals the boot", () => {
    const view = bootView(FULL, false);
    expect(view?.rows).toEqual([
      { name: "issue", duration: "240 ms", slowest: false },
      { name: "git", duration: "1.2s", slowest: false },
      { name: "providers", duration: "310 ms", slowest: false },
      { name: "mesh-start", duration: "2.1s", slowest: false },
      { name: "image-pull", duration: "1.9s", slowest: false },
      { name: "vm-boot", duration: "1m 26s", slowest: true },
      { name: "mesh-join", duration: "1.5s", slowest: false },
      { name: "agentd", duration: "720 ms", slowest: false },
    ]);
    expect(view?.summary).toBe("total 94s");
  });

  it("a boot still starting shows the last finished phase, and the slowest among those so far", () => {
    const view = bootView({ phases: FULL.phases.slice(0, 4) }, true);
    expect(view?.rows.map((r) => r.name)).toEqual(["issue", "git", "providers", "mesh-start"]);
    expect(view?.rows.filter((r) => r.slowest).map((r) => r.name)).toEqual(["mesh-start"]);
    expect(view?.summary).toBe("starting · last done: mesh-start");
  });

  it("a boot that has not finished a phase yet says so, with or without a timing object", () => {
    expect(bootView({ phases: [] }, true)).toEqual({ rows: [], summary: "starting · no phase finished yet" });
    expect(bootView(null, true)).toEqual({ rows: [], summary: "starting · no phase finished yet" });
    expect(bootView(undefined, true)).toEqual({ rows: [], summary: "starting · no phase finished yet" });
  });

  it("a boot that stopped part way names where it stopped, never a partial total", () => {
    // No `total_ms`: the mothership only sends one for a boot that finished.
    const view = bootView({ phases: FULL.phases.slice(0, 5) }, false);
    expect(view?.rows.map((r) => r.name)).toEqual(["issue", "git", "providers", "mesh-start", "image-pull"]);
    expect(view?.summary).toBe("stopped after image-pull");
    expect(bootView({ phases: [] }, false)).toEqual({ rows: [], summary: "stopped before the first phase" });
  });

  it("no timing and no boot under way is nothing to show", () => {
    expect(bootView(null, false)).toBeNull();
    expect(bootView(undefined, false)).toBeNull();
  });

  it("a tie flags only the first of the equal phases", () => {
    const view = bootView({ total_ms: 2_000, phases: [{ name: "a", ms: 900 }, { name: "b", ms: 900 }] }, false);
    expect(view?.rows.map((r) => r.slowest)).toEqual([true, false]);
  });
});
