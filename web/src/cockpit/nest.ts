// Nest geometry: where the chambers sit, how the tunnels bend, how the surface grass scatters.
// The arithmetic is lifted verbatim from the prototype's renderVals() so the React port and these
// tests share one source of truth — the numbers are the design's, not re-derived.

export interface NestBox {
  width: number;
  height: number;
}

export interface Slot {
  x: number;
  y: number;
  r: number;
}

/** A chamber the view builds on a slot, once it knows which colony sits there. */
export interface Chamber extends Slot {
  id: string;
  index: number;
}

/** A tunnel the view draws from tunnelPath, once it knows which colony it serves. */
export interface Tunnel {
  id: string;
  d: string;
}

export interface Branch {
  d: string;
}

export interface GrassTuft {
  left: number;
  top: number;
  height: number;
  rotation: number;
}

/** The ground line, and the mothership's y. */
export const SURFACE_Y = 78;

/** The plot never gets smaller than this, and a 0×0 reading (not laid out yet) falls back to the prototype's defaults. */
export function normalizeBox(width: number, height: number): NestBox {
  return { width: Math.max(520, width || 880), height: Math.max(360, height || 470) };
}

/** Chambers and labels shrink on a narrow plot but never enough to become unreadable. */
export function scaleFor(box: NestBox): number {
  return Math.max(0.62, Math.min(1.1, Math.min(box.width / 900, (box.height - SURFACE_Y) / 400)));
}

/** [fx, fy, baseRadius] per slot: fraction of the box, fraction of the underground, blob radius at scale 1. */
export const SLOT_FRACTIONS: readonly (readonly [number, number, number])[] = [
  [0.5, 0.42, 68],
  [0.22, 0.3, 74],
  [0.78, 0.29, 74],
  [0.31, 0.76, 76],
  [0.69, 0.77, 76],
  [0.075, 0.58, 58],
  [0.925, 0.57, 58],
  [0.5, 0.92, 58],
];

export const MAX_CHAMBERS = SLOT_FRACTIONS.length;

/**
 * A machine only digs `sandbox.max_parallel` colonies at once (5 by default), so the nest draws
 * only that many chambers. Unknown capacity (null/undefined/NaN) reads as the default 5;
 * anything else is floored, then clamped to what the nest can hold.
 */
export const DEFAULT_CHAMBERS = 5;

export function chamberCount(capacity: number | null | undefined): number {
  if (capacity == null || Number.isNaN(capacity)) return DEFAULT_CHAMBERS;
  const floored = Math.floor(capacity);
  if (!Number.isFinite(floored)) return DEFAULT_CHAMBERS;
  return Math.min(MAX_CHAMBERS, Math.max(1, floored));
}

/**
 * The roomier layout, used when the machine runs 5 or fewer colonies at once: the same
 * [fx, fy, baseRadius] shape as SLOT_FRACTIONS, with bigger chambers spread further apart.
 */
export const FIVE_SLOT_FRACTIONS: readonly (readonly [number, number, number])[] = [
  [0.5, 0.4, 84],
  [0.2, 0.32, 80],
  [0.8, 0.32, 80],
  [0.32, 0.8, 80],
  [0.68, 0.8, 80],
];

export function slotAt(index: number, box: NestBox, count: number = MAX_CHAMBERS): Slot {
  const fractions = count <= FIVE_SLOT_FRACTIONS.length ? FIVE_SLOT_FRACTIONS : SLOT_FRACTIONS;
  const frac = fractions[index];
  if (!frac) throw new RangeError(`no slot ${index}, the nest holds ${fractions.length} chambers`);
  const [fx, fy, baseRadius] = frac;
  return {
    x: Math.round(fx * box.width),
    y: Math.round(SURFACE_Y + 30 + fy * (box.height - SURFACE_Y - 60)),
    r: Math.round(baseRadius * scaleFor(box)),
  };
}

/**
 * A colony's own seed. Tunnels are dug, not drawn: each one wobbles its own way, and this is what
 * makes that shape the colony's — the same every render, different from its neighbour's.
 * An open colony has no issue number, so it digs from 0.
 */
export function tunnelSeed(repo: string, issue: number | null): number {
  return ((issue ?? 0) * 9301 + repo.length * 49297) % 233280;
}

/** The seeded fract hash the wobble draws every offset from. */
function rndFor(seed: number): (n: number) => number {
  return (n) => {
    const v = Math.sin(seed + n * 12.9898) * 43758.5453;
    return v - Math.floor(v);
  };
}

/**
 * A corridor from (ax, ay) to (bx, by) in `segs` quadratic hops, each knocked off the straight line
 * by up to `amp`. The last hop always lands exactly on the target, so a tunnel still meets its
 * chamber however much the middle wanders.
 */
function wobble(
  ax: number,
  ay: number,
  bx: number,
  by: number,
  segs: number,
  amp: number,
  rnd: (n: number) => number,
): string {
  let d = `M${ax.toFixed(0)} ${ay.toFixed(0)}`;
  let px = ax;
  let py = ay;
  for (let q = 1; q <= segs; q++) {
    const t = q / segs;
    const nx = ax + (bx - ax) * t;
    const ny = ay + (by - ay) * t;
    const ex = q === segs ? bx : nx + (rnd(q) - 0.5) * amp;
    const ey = q === segs ? by : ny + (rnd(q + 40) - 0.5) * amp * 0.7;
    const cx = (px + ex) / 2 + (rnd(q + 80) - 0.5) * amp;
    const cy = (py + ey) / 2 + (rnd(q + 120) - 0.5) * amp * 0.6;
    d += ` Q${cx.toFixed(0)} ${cy.toFixed(0)} ${ex.toFixed(0)} ${ey.toFixed(0)}`;
    px = ex;
    py = ey;
  }
  return d;
}

/** The corridor from the mothership's mouth down to the top edge of a chamber. */
export function tunnelPath(slot: Slot, box: NestBox, seed: number): string {
  const rnd = rndFor(seed);
  const mx = Math.round(box.width / 2);
  return wobble(
    mx,
    SURFACE_Y + 26,
    slot.x,
    slot.y - slot.r * 0.55,
    4 + Math.floor(rnd(1) * 3),
    34 + rnd(2) * 30,
    rnd,
  );
}

/**
 * Side tunnels grow with the work done: one per 5 steps, at most 4, each dug off at its own angle.
 * They are kept clear of the bottom edge so a deep chamber's branches stay on the plot.
 */
export function branchPaths(slot: Slot, totalSteps: number, box: NestBox, seed: number): Branch[] {
  const rnd = rndFor(seed);
  const mx = Math.round(box.width / 2);
  const count = Math.min(4, Math.floor(totalSteps / 5));
  const branches: Branch[] = [];
  for (let k = 0; k < count; k++) {
    const ang =
      rnd(k + 200) * 1.9 -
      0.95 +
      (k % 2 ? Math.PI * 0.5 : -Math.PI * 0.5) * (rnd(k + 260) > 0.5 ? 1 : 0.3) +
      (slot.x < mx ? Math.PI : 0);
    const len = slot.r * (0.7 + rnd(k + 300) * 0.9);
    const ex = slot.x + Math.cos(ang) * (slot.r + len);
    const ey = Math.min(box.height - 12, slot.y + Math.abs(Math.sin(ang)) * 0.6 * (slot.r + len) + 12);
    branches.push({
      d: wobble(slot.x + Math.cos(ang) * slot.r * 0.85, slot.y + Math.sin(ang) * slot.r * 0.75, ex, ey, 3, 22, rnd),
    });
  }
  return branches;
}

/** 34 tufts scattered along the ground line by a fixed fract hash, so they never jitter between renders. */
export function surfaceGrass(box: NestBox): GrassTuft[] {
  return Array.from({ length: 34 }, (_, i) => {
    const r = Math.sin(i * 12.9898) * 43758.5453;
    const fr = r - Math.floor(r);
    return {
      left: Math.round((i / 34) * box.width + fr * 18),
      top: SURFACE_Y - 6 - Math.round(fr * 8),
      height: 6 + Math.round(fr * 8),
      rotation: Math.round((fr - 0.5) * 40),
    };
  });
}
