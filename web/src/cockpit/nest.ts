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

export function slotAt(index: number, box: NestBox): Slot {
  const frac = SLOT_FRACTIONS[index];
  if (!frac) throw new RangeError(`no slot ${index}, the nest holds ${SLOT_FRACTIONS.length} chambers`);
  const [fx, fy, baseRadius] = frac;
  return {
    x: Math.round(fx * box.width),
    y: Math.round(SURFACE_Y + 30 + fy * (box.height - SURFACE_Y - 60)),
    r: Math.round(baseRadius * scaleFor(box)),
  };
}

/** The quadratic from the mothership's mouth down to the top edge of a chamber. */
export function tunnelPath(slot: Slot, box: NestBox): string {
  const mx = Math.round(box.width / 2);
  const my = SURFACE_Y;
  const cx = (mx + slot.x) / 2 + (slot.x - mx) * 0.22;
  const cy = (my + 26 + slot.y) / 2 - 10;
  return `M${mx} ${my + 26} Q${cx.toFixed(0)} ${cy.toFixed(0)} ${slot.x} ${slot.y - slot.r * 0.55}`;
}

/** Side tunnels grow with the work done: one per 5 steps, at most 4, splaying away from the centre. */
export function branchPaths(slot: Slot, totalSteps: number, box: NestBox): Branch[] {
  const mx = Math.round(box.width / 2);
  const count = Math.min(4, Math.floor(totalSteps / 5));
  const branches: Branch[] = [];
  for (let k = 0; k < count; k++) {
    const ang = (k % 2 ? -1 : 1) * (0.6 + k * 0.5) + (slot.x < mx ? Math.PI : 0);
    const len = slot.r * (0.9 + k * 0.25);
    const ex = slot.x + Math.cos(ang) * (slot.r + len);
    const ey = slot.y + Math.sin(ang) * 0.6 * (slot.r + len) + 8;
    const sx = (slot.x + Math.cos(ang) * slot.r).toFixed(0);
    const sy = (slot.y + Math.sin(ang) * slot.r * 0.9).toFixed(0);
    const qx = ((slot.x + ex) / 2 + 10).toFixed(0);
    const qy = ((slot.y + ey) / 2 + 14).toFixed(0);
    branches.push({ d: `M${sx} ${sy} Q${qx} ${qy} ${ex.toFixed(0)} ${ey.toFixed(0)}` });
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
