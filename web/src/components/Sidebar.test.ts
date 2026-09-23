// The sidebar's Storage dot (issue #220): red while writes fail or the queue is paused for lack
// of disk, amber on low disk, otherwise plain — with the free-space reading in the label. The dot
// helper is pure, so these pin it directly without rendering the sidebar.
import { describe, expect, it } from "vitest";

import { storageDot } from "./Sidebar";

describe("storageDot", () => {
  it("stays plain with the free-space reading when all is well, or none when the mothership has none yet", () => {
    expect(storageDot({ ok: true, free_bytes: 12_884_901_888 })).toEqual({
      label: "Storage · 12G free",
      state: "ok",
    });
    expect(storageDot({ ok: true }).label).toBe("Storage");
  });

  it("warns on low disk, keeping the free-space reading", () => {
    const dot = storageDot({ ok: true, low_disk: true, free_bytes: 3_221_225_472 });
    expect(dot.state).toBe("warn");
    expect(dot.label).toContain("3G free");
  });

  it("reads bad when the queue is paused, naming the pause instead of the bytes", () => {
    const dot = storageDot({ ok: true, low_disk: true, admission_paused: true, free_bytes: 512_000_000 });
    expect(dot.state).toBe("bad");
    expect(dot.label).toContain("queue paused (low disk)");
  });

  it("still reads bad while writes fail, and keeps the older reclaim suffix", () => {
    const dot = storageDot({ ok: false, free_bytes: 12_884_901_888 }, { reclaimable: 1, unpushed: 1 });
    expect(dot.state).toBe("bad");
    expect(dot.label).toContain("1 reclaimable · 1 unpushed");
    expect(dot.label).toContain("12G free");
  });
});
