// The BOOT section's pure half (issue #360): phases arrive in boot order and render in that order,
// exactly one is flagged slowest, and the summary line says how far a starting boot got or what a
// finished one took. A colony with no timing and no boot under way gets no section at all.
import { describe, expect, it } from "vitest";

import type { Session } from "../types";
import { bootMedians, bootView } from "./bootTiming";

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

// The mothership pane's cross-colony medians: per-phase medians across recent finished boots.
describe("bootMedians", () => {
  const colony = (id: string, created_at: string, timing: Session["boot_timing"]): Session =>
    ({ id, created_at, boot_timing: timing }) as Session;
  const done = (ms: number, total = 10_000) => ({ total_ms: total, phases: [{ name: "git", ms }, { name: "vm-boot", ms: ms * 10 }] });
  const gitOf = (view: ReturnType<typeof bootMedians>) => view?.rows.find((r) => r.name === "git")?.duration;

  it("null when no boot finished", () => {
    expect(bootMedians([])).toBeNull();
    expect(bootMedians([colony("a", "2026-09-18T09:00:00Z", null), colony("b", "2026-09-18T09:01:00Z", undefined)])).toBeNull();
  });

  it("excludes boots still under way or stopped part way", () => {
    const view = bootMedians([
      colony("a", "2026-09-18T09:00:00Z", { phases: [{ name: "git", ms: 50 }] }),
      colony("b", "2026-09-18T09:01:00Z", done(1_000)),
      colony("c", "2026-09-18T09:02:00Z", { phases: [{ name: "git", ms: 9_999 }] }),
    ]);
    expect(view?.count).toBe(1);
    expect(view?.rows).toEqual([
      { name: "git", duration: "1s", slowest: false },
      { name: "vm-boot", duration: "10s", slowest: true },
    ]);
    expect(view?.summary).toBe("median total 10s");
  });

  it("even counts take the upper middle over the most recent boots", () => {
    const all = [100, 200, 300, 400, 500].map((ms, i) => colony(`s${i}`, `2026-09-18T09:0${i}:00Z`, done(ms, ms * 10)));
    expect(gitOf(bootMedians(all))).toBe("300 ms");
    const limited = bootMedians(all, 4);
    expect(limited?.count).toBe(4);
    expect(gitOf(limited)).toBe("400 ms");
    expect(limited?.summary).toBe("median total 4s");
  });

  it("a phase missing from some boots medians over the ones that ran it, slowest flagged", () => {
    const view = bootMedians([
      colony("a", "2026-09-18T09:00:00Z", { total_ms: 5_000, phases: [{ name: "vm-boot", ms: 4_000 }] }),
      colony("b", "2026-09-18T09:01:00Z", { total_ms: 6_000, phases: [{ name: "git", ms: 500 }, { name: "vm-boot", ms: 5_000 }] }),
    ]);
    expect(view?.rows.map((r) => r.name)).toEqual(["git", "vm-boot"]);
    expect(view?.rows.filter((r) => r.slowest).map((r) => r.name)).toEqual(["vm-boot"]);
  });
});
