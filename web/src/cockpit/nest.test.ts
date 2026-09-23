// The prototype's renderVals() arithmetic, pinned so the React port can't drift from the design:
// clamps, slot placement, tunnel anchors, branch counts and the grass hash are all asserted
// against hand-computed values for the default 880×470 plot.
import { describe, expect, it } from "vitest";

import { MAX_CHAMBERS, SURFACE_Y, branchPaths, chamberCount, normalizeBox, scaleFor, slotAt, surfaceGrass, tunnelPath, tunnelSeed } from "./nest";

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

describe("chamberCount", () => {
  it("reads unknown capacity as the default 5", () => {
    expect(chamberCount(null)).toBe(5);
    expect(chamberCount(undefined)).toBe(5);
    expect(chamberCount(NaN)).toBe(5);
  });

  it("clamps to what the nest can hold", () => {
    expect(chamberCount(14)).toBe(8);
    expect(chamberCount(0)).toBe(1);
  });

  it("floors a fractional capacity", () => {
    expect(chamberCount(4.7)).toBe(4);
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

  it("keeps the roomier five-slot layout inside the plot, below the surface", () => {
    for (const box of [DEFAULT, normalizeBox(520, 360), normalizeBox(1400, 900)]) {
      for (let i = 0; i < 5; i++) {
        const slot = slotAt(i, box, 5);
        expect(slot.x - slot.r).toBeGreaterThanOrEqual(0);
        expect(slot.x + slot.r).toBeLessThanOrEqual(box.width);
        expect(slot.y - slot.r).toBeGreaterThanOrEqual(SURFACE_Y);
        expect(slot.y + slot.r).toBeLessThanOrEqual(box.height);
      }
    }
  });

  it("leaves room to breathe between the five-slot chambers", () => {
    for (const box of [DEFAULT, normalizeBox(520, 360), normalizeBox(1400, 900)]) {
      const slots = Array.from({ length: 5 }, (_, i) => slotAt(i, box, 5));
      for (let a = 0; a < slots.length; a++) {
        for (let b = a + 1; b < slots.length; b++) {
          const distance = Math.hypot(slots[a].x - slots[b].x, slots[a].y - slots[b].y);
          expect(distance).toBeGreaterThan(slots[a].r + slots[b].r);
        }
      }
    }
  });

  it("throws past the fifth slot when the count picks the five-slot layout", () => {
    expect(() => slotAt(5, DEFAULT, 5)).toThrow(RangeError);
  });
});

describe("tunnelSeed", () => {
  it("gives a colony the same seed every time", () => {
    expect(tunnelSeed("acme/webshop", 42)).toBe(tunnelSeed("acme/webshop", 42));
  });

  it("gives neighbours different ones, so their tunnels are not the same shape", () => {
    expect(tunnelSeed("acme/webshop", 42)).not.toBe(tunnelSeed("acme/webshop", 43));
    expect(tunnelSeed("acme/webshop", 42)).not.toBe(tunnelSeed("acme/design-system", 42));
  });

  it("digs from 0 for a colony started without an issue", () => {
    expect(tunnelSeed("acme/webshop", null)).toBe(tunnelSeed("acme/webshop", 0));
  });
});

describe("tunnelPath", () => {
  const seed = tunnelSeed("acme/webshop", 42);

  it("starts at the mothership's mouth for every slot", () => {
    for (let i = 0; i < MAX_CHAMBERS; i++) {
      expect(tunnelPath(slotAt(i, DEFAULT), DEFAULT, seed).startsWith("M440 104 ")).toBe(true);
    }
  });

  it("lands exactly on the chamber however much the middle wanders", () => {
    for (let i = 0; i < MAX_CHAMBERS; i++) {
      const slot = slotAt(i, DEFAULT);
      const end = tunnelPath(slot, DEFAULT, seed).split(" ").slice(-2);
      expect(Number(end[0])).toBe(slot.x);
      expect(Number(end[1])).toBe(Number((slot.y - slot.r * 0.55).toFixed(0)));
    }
  });

  it("is the same corridor on every render", () => {
    const slot = slotAt(2, DEFAULT);
    expect(tunnelPath(slot, DEFAULT, seed)).toBe(tunnelPath(slot, DEFAULT, seed));
  });

  it("digs a different corridor for a different colony", () => {
    const slot = slotAt(2, DEFAULT);
    expect(tunnelPath(slot, DEFAULT, seed)).not.toBe(tunnelPath(slot, DEFAULT, tunnelSeed("acme/webshop", 43)));
  });

  it("wanders: more than the one hop a straight curve would need", () => {
    const hops = tunnelPath(slotAt(0, DEFAULT), DEFAULT, seed).split("Q").length - 1;
    expect(hops).toBeGreaterThanOrEqual(4);
    expect(hops).toBeLessThanOrEqual(6);
  });
});

describe("branchPaths", () => {
  const seed = tunnelSeed("acme/webshop", 42);

  it("grows no side tunnels before 5 steps", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 0, DEFAULT, seed)).toEqual([]);
  });

  it("grows one side tunnel per 5 steps", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 9, DEFAULT, seed)).toHaveLength(1);
  });

  it("stops at 4 side tunnels", () => {
    expect(branchPaths(slotAt(0, DEFAULT), 100, DEFAULT, seed)).toHaveLength(4);
  });

  it("is stable across calls", () => {
    const slot = slotAt(3, DEFAULT);
    expect(branchPaths(slot, 40, DEFAULT, seed)).toEqual(branchPaths(slot, 40, DEFAULT, seed));
  });

  it("stops a deep chamber's branches short of the bottom edge", () => {
    // Slot 7 is the lowest, so its branches are the ones that would otherwise run off the plot.
    // Only the end point is clamped — the corridor may bow past it on the way and that is fine.
    for (const branch of branchPaths(slotAt(7, DEFAULT), 100, DEFAULT, seed)) {
      const endY = Number(branch.d.split(" ").slice(-1)[0]);
      expect(endY).toBeLessThanOrEqual(DEFAULT.height - 12);
    }
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
