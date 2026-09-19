// The prototype's renderVals() arithmetic, pinned so the React port can't drift from the design:
// clamps, slot placement, tunnel anchors, branch counts and the grass hash are all asserted
// against hand-computed values for the default 880×470 plot.
import { describe, expect, it } from "vitest";

import { MAX_CHAMBERS, SURFACE_Y, branchPaths, normalizeBox, scaleFor, slotAt, surfaceGrass, tunnelPath } from "./nest";

const DEFAULT = normalizeBox(880, 470);

describe("normalizeBox", () => {
  it("clamps a tiny plot to the readable minimum", () => {
    expect(normalizeBox(100, 50)).toEqual({ width: 520, height: 360 });
  });

  it("falls back to the prototype's defaults on a 0×0 reading", () => {
    expect(normalizeBox(0, 0)).toEqual({ width: 880, height: 470 });
  });

  it("leaves a real plot alone", () => {
    expect(normalizeBox(1200, 900)).toEqual({ width: 1200, height: 900 });
  });
});

describe("scaleFor", () => {
  it("never shrinks below 0.62", () => {
    expect(scaleFor({ width: 520, height: 360 })).toBe(0.62);
  });

  it("never grows past 1.1", () => {
    expect(scaleFor({ width: 1200, height: 900 })).toBe(1.1);
  });

  it("tracks the tighter axis in between", () => {
    expect(scaleFor(DEFAULT)).toBeCloseTo(880 / 900, 10);
  });
});

describe("slotAt", () => {
  it("places every slot inside the plot, below the surface", () => {
    for (const box of [DEFAULT, normalizeBox(520, 360), normalizeBox(1400, 900)]) {
      for (let i = 0; i < MAX_CHAMBERS; i++) {
        const slot = slotAt(i, box);
        expect(slot.x - slot.r).toBeGreaterThanOrEqual(0);
        expect(slot.x + slot.r).toBeLessThanOrEqual(box.width);
        expect(slot.y - slot.r).toBeGreaterThanOrEqual(SURFACE_Y);
        expect(slot.y + slot.r).toBeLessThanOrEqual(box.height);
      }
    }
  });

  it("puts the first slot where the design puts it", () => {
    expect(slotAt(0, DEFAULT)).toEqual({ x: 440, y: 247, r: 66 });
  });

  it("throws past the last slot", () => {
    expect(() => slotAt(MAX_CHAMBERS, DEFAULT)).toThrow(RangeError);
    expect(() => slotAt(99, DEFAULT)).toThrow(RangeError);
  });
});

describe("tunnelPath", () => {
  it("starts at the mothership's mouth for every slot", () => {
    for (let i = 0; i < MAX_CHAMBERS; i++) {
      expect(tunnelPath(slotAt(i, DEFAULT), DEFAULT).startsWith("M440 104 Q")).toBe(true);
    }
  });

  it("keeps the design's control point for a central and a side chamber", () => {
    // slot 0 sits dead centre, so the curve drops straight; slot 1 bows out to the left.
    const centre = tunnelPath(slotAt(0, DEFAULT), DEFAULT);
    expect(centre.startsWith("M440 104 Q440 166 440 ")).toBe(true);
    expect(Number(centre.slice(centre.lastIndexOf(" ") + 1))).toBeCloseTo(210.7, 1);

    const side = tunnelPath(slotAt(1, DEFAULT), DEFAULT);
    expect(side.startsWith("M440 104 Q263 146 194 ")).toBe(true);
    expect(Number(side.slice(side.lastIndexOf(" ") + 1))).toBeCloseTo(168.4, 1);
  });
});

describe("branchPaths", () => {
  it("grows no side tunnels before 5 steps", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 0, DEFAULT)).toEqual([]);
  });

  it("grows one side tunnel per 5 steps", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 9, DEFAULT)).toHaveLength(1);
  });

  it("stops at 4 side tunnels", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 100, DEFAULT)).toHaveLength(4);
  });

  it("is stable across calls", () => {
    const slot = slotAt(3, DEFAULT);
    expect(branchPaths(slot, 40, DEFAULT)).toEqual(branchPaths(slot, 40, DEFAULT));
  });
});

describe("surfaceGrass", () => {
  it("plants 34 tufts", () => {
    expect(surfaceGrass(DEFAULT)).toHaveLength(34);
  });

  it("is deterministic: the same box grows the same grass", () => {
    expect(surfaceGrass(DEFAULT)).toEqual(surfaceGrass(DEFAULT));
  });

  it("keeps the design's hash for the first tuft", () => {
    expect(surfaceGrass(DEFAULT)[0]).toEqual({ left: 0, top: 72, height: 6, rotation: -20 });
  });

  it("hugs the ground line", () => {
    for (const tuft of surfaceGrass(DEFAULT)) {
      expect(tuft.top).toBeLessThan(SURFACE_Y);
      expect(tuft.top).toBeGreaterThanOrEqual(SURFACE_Y - 14);
    }
  });
});
