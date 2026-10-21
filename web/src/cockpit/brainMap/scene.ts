// The brain map's scene, as pure functions (no DOM, no canvas): a seeded 3D layout of the brain's
// nodes inside a brain-shaped ellipsoid, the camera's projection, picking, and where each colony's
// ant stands and walks. BrainMapView.tsx draws what these return.
import type { Session } from "../../features/sessions/types";
import type { BrainMapConnected } from "../../features/brainMap/types";

/** The ellipsoid the brain fills: left-right, up-down, front-back (world units). */
export const BRAIN_AXES = { x: 1, y: 0.72, z: 0.9 } as const;
/** The camera's distance from the origin, in world units. */
const CAMERA_DISTANCE = 3.2;

/** Each kind's colour, the livingbrain.wiki brain's dark-theme tokens (its app.css `--t-*`). */
export const KIND_COLOUR: Readonly<Record<string, string>> = {
  project: "oklch(0.79 0.075 295)",
  person: "oklch(0.8 0.075 235)",
  decision: "oklch(0.85 0.085 85)",
  customer: "oklch(0.8 0.08 25)",
  system: "oklch(0.8 0.07 190)",
  topic: "oklch(0.82 0.06 140)",
  glossary: "oklch(0.82 0.06 140)",
  radar: "oklch(0.82 0.06 140)",
  "coding-agents": "oklch(0.86 0.16 162)",
  session: "oklch(0.76 0.09 320)",
  github: "oklch(0.85 0.01 250)",
  source: "oklch(0.78 0.08 55)",
  fact: "oklch(0.62 0.02 210)",
};
export const DEFAULT_KIND_COLOUR = "oklch(0.82 0.06 140)";

export function kindColour(kind: string): string {
  return KIND_COLOUR[kind] ?? DEFAULT_KIND_COLOUR;
}

/** A small seeded PRNG (mulberry32), so a layout is the same every time for the same graph. */
export function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A string's FNV-1a hash, for seeding an ant from its colony id. */
export function hashString(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/**
 * The nodes' positions, three numbers each, inside the brain's ellipsoid. A shell start, then a
 * fixed number of cooling steps: springs along the links, sampled repulsion between nodes, and the
 * ellipsoid as a hard limit. Deterministic for a given graph and seed.
 */
export function layoutBrain(count: number, links: readonly (readonly [number, number])[], seed = 11): Float32Array {
  const pos = new Float32Array(count * 3);
  if (count === 0) return pos;
  const random = rng(seed);
  const { x: A, y: B, z: C } = BRAIN_AXES;
  for (let i = 0; i < count; i += 1) {
    const u = random() * 2 - 1;
    const theta = random() * Math.PI * 2;
    const s = Math.sqrt(1 - u * u);
    const r = 0.55 + 0.45 * Math.cbrt(random());
    pos[i * 3] = A * s * Math.cos(theta) * r;
    pos[i * 3 + 1] = B * u * r;
    pos[i * 3 + 2] = C * s * Math.sin(theta) * r;
  }
  if (count === 1) return pos;
  const spacing = Math.cbrt(((4 / 3) * Math.PI * A * B * C) / count);
  const rest = spacing * 1.1;
  const repel = spacing * spacing * 0.6;
  const samples = Math.min(12, count - 1);
  const ticks = 220;
  const vel = new Float32Array(count * 3);
  for (let tick = 0; tick < ticks; tick += 1) {
    const cool = 0.15 + 0.85 * (1 - tick / ticks);
    for (let i = 0; i < count; i += 1) {
      for (let k = 0; k < samples; k += 1) {
        const j = Math.floor(random() * count);
        if (j === i) continue;
        const dx = pos[i * 3] - pos[j * 3];
        const dy = pos[i * 3 + 1] - pos[j * 3 + 1];
        const dz = pos[i * 3 + 2] - pos[j * 3 + 2];
        const d2 = dx * dx + dy * dy + dz * dz + 1e-6;
        if (d2 > spacing * spacing * 16) continue;
        const d = Math.sqrt(d2);
        const f = (repel / d2) * cool;
        vel[i * 3] += (dx / d) * f;
        vel[i * 3 + 1] += (dy / d) * f;
        vel[i * 3 + 2] += (dz / d) * f;
      }
    }
    for (const [a, b] of links) {
      const dx = pos[b * 3] - pos[a * 3];
      const dy = pos[b * 3 + 1] - pos[a * 3 + 1];
      const dz = pos[b * 3 + 2] - pos[a * 3 + 2];
      const d = Math.sqrt(dx * dx + dy * dy + dz * dz) + 1e-6;
      const f = ((d - rest) / d) * 0.08 * cool;
      vel[a * 3] += dx * f;
      vel[a * 3 + 1] += dy * f;
      vel[a * 3 + 2] += dz * f;
      vel[b * 3] -= dx * f;
      vel[b * 3 + 1] -= dy * f;
      vel[b * 3 + 2] -= dz * f;
    }
    for (let i = 0; i < count; i += 1) {
      for (let c = 0; c < 3; c += 1) {
        const step = Math.max(-spacing, Math.min(spacing, vel[i * 3 + c] * 0.5));
        pos[i * 3 + c] += step;
        vel[i * 3 + c] *= 0.6;
      }
      const x = pos[i * 3] / A, y = pos[i * 3 + 1] / B, z = pos[i * 3 + 2] / C;
      const e = Math.sqrt(x * x + y * y + z * z);
      if (e > 0.97) {
        const k = 0.97 / e;
        pos[i * 3] *= k;
        pos[i * 3 + 1] *= k;
        pos[i * 3 + 2] *= k;
      }
    }
  }
  return pos;
}

/** The camera: the brain turns by `yaw` (around up) and `pitch` (around the side axis); `zoom` scales. */
export interface Camera {
  yaw: number;
  pitch: number;
  zoom: number;
}

export const DEFAULT_CAMERA: Camera = { yaw: 0.5, pitch: 0.28, zoom: 1 };

export interface Projected {
  x: number;
  y: number;
  /** Depth: larger is farther from the viewer. */
  depth: number;
  /** The perspective scale at this depth. */
  scale: number;
}

/** A world point, turned by the camera and projected onto a `width` x `height` canvas (CSS pixels). */
export function projectPoint(
  x: number,
  y: number,
  z: number,
  camera: Camera,
  width: number,
  height: number,
): Projected {
  const cy = Math.cos(camera.yaw);
  const sy = Math.sin(camera.yaw);
  const x1 = x * cy + z * sy;
  const z1 = -x * sy + z * cy;
  const cp = Math.cos(camera.pitch);
  const sp = Math.sin(camera.pitch);
  const y1 = y * cp - z1 * sp;
  const z2 = y * sp + z1 * cp;
  const scale = CAMERA_DISTANCE / (CAMERA_DISTANCE + z2);
  const focal = Math.min(width, height) * 0.42 * camera.zoom;
  return { x: width / 2 + x1 * scale * focal, y: height / 2 + y1 * scale * focal, depth: z2, scale };
}

/** Each node's drawn radius in CSS pixels: larger for busier nodes, shrinking with depth. */
export function nodeRadius(degree: number, maxDegree: number, scale: number): number {
  const busy = maxDegree > 0 ? Math.sqrt(degree / maxDegree) : 0;
  return Math.max(1.2, Math.min(9, (1.6 + 3.4 * busy) * scale));
}

/** The nearest node or ant under a point, within `slop` pixels of its drawn edge; null for none. */
export function nearest<T extends { x: number; y: number; r: number }>(items: readonly T[], px: number, py: number, slop = 6): T | null {
  let best: T | null = null;
  let bestD = Infinity;
  for (const item of items) {
    const d = Math.hypot(item.x - px, item.y - py) - item.r;
    if (d <= slop && d < bestD) {
      best = item;
      bestD = d;
    }
  }
  return best;
}

/** The statuses an ant is drawn for: the colonies still in play. Finished ones have no ant. */
const ANT_TONE: Partial<Record<Session["status"], { tone: AntTone; moving: boolean }>> = {
  starting: { tone: "info", moving: true },
  running: { tone: "info", moving: true },
  publishing: { tone: "info", moving: true },
  waiting_for_answer: { tone: "accent", moving: false },
  blocked: { tone: "warn", moving: false },
  queued: { tone: "muted", moving: false },
  idle: { tone: "muted", moving: false },
  parked: { tone: "warn", moving: false },
};

export type AntTone = "info" | "accent" | "warn" | "muted";

/** Whether a colony in this status is drawn as an ant at all. */
export function isAntStatus(status: Session["status"]): boolean {
  return ANT_TONE[status] !== undefined;
}

export interface Ant {
  /** The colony's id. */
  id: string;
  title: string;
  repo: string;
  status: Session["status"];
  /** The node it stands on. */
  node: number;
  tone: AntTone;
  /** Walks along an edge from its node; false for one that sits still and is drawn dimmer. */
  moving: boolean;
  /** A seed for the ant's walk and its place around its node. */
  seed: number;
}

export const MAX_ANTS = 200;

/**
 * The colonies drawn as ants, each on the node its repository matched (the nest when none did).
 * A colony that is suspended, or whose repository is not in the answer, still gets an ant at the nest.
 */
export function antPlan(sessions: readonly Session[], answer: BrainMapConnected): Ant[] {
  const ants: Ant[] = [];
  for (const session of sessions) {
    const meta = ANT_TONE[session.status];
    if (!meta) continue;
    const matched = answer.matches[session.repo];
    const node = matched ?? answer.nest;
    if (node == null) continue;
    ants.push({
      id: session.id,
      title: session.summary || session.issue_title || session.repo,
      repo: session.repo,
      status: session.status,
      node,
      tone: meta.tone,
      moving: meta.moving && !(session.suspended != null || session.pending_answer != null),
      seed: hashString(session.id),
    });
    if (ants.length >= MAX_ANTS) break;
  }
  return ants;
}

/** Each node's neighbours, from the links: what an ant walking from it can walk to. */
export function adjacency(count: number, links: readonly (readonly [number, number])[]): number[][] {
  const out: number[][] = Array.from({ length: count }, () => []);
  for (const [a, b] of links) {
    out[a].push(b);
    out[b].push(a);
  }
  return out;
}

/**
 * Where an ant is in the world at `time` (seconds). A moving ant ping-pongs along one edge from its
 * node to a neighbour; one with no neighbour, or a still one, stays by its node, offset around it.
 */
export function antWorld(ant: Ant, time: number, positions: Float32Array, neighbours: readonly number[][], out: Float32Array, at: number): void {
  const n = ant.node;
  const nx = positions[n * 3];
  const ny = positions[n * 3 + 1];
  const nz = positions[n * 3 + 2];
  const around = (ant.seed % 360) * (Math.PI / 180);
  const ring = 0.035;
  const list = neighbours[n] ?? [];
  if (!ant.moving || list.length === 0) {
    out[at] = nx + Math.cos(around) * ring;
    out[at + 1] = ny;
    out[at + 2] = nz + Math.sin(around) * ring;
    return;
  }
  const other = list[ant.seed % list.length];
  const speed = 0.12 + ((ant.seed >>> 8) % 100) / 1000;
  const phase = (time * speed + ((ant.seed >>> 16) % 1000) / 1000) % 1;
  const s = 0.5 - 0.5 * Math.cos(phase * Math.PI * 2);
  out[at] = nx + (positions[other * 3] - nx) * s + Math.cos(around) * 0.02;
  out[at + 1] = ny + (positions[other * 3 + 1] - ny) * s;
  out[at + 2] = nz + (positions[other * 3 + 2] - nz) * s + Math.sin(around) * 0.02;
}
