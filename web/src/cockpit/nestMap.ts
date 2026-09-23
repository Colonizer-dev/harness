// The architecture map as a nest (crates/colonizer/src/maps.rs makes the map; NestMapView draws it).
// Pure on purpose, like nest.ts: which component a touched file belongs to, where each chamber sits
// on the plot, and the tunnels an ant walks to reach it are all decided here and pinned by tests.
import type { ArchMap, ArchComponent } from "../types";
import { SURFACE_Y, type NestBox } from "./nest";

/** A component placed on the plot: its chamber's centre and radius, in plot pixels. */
export interface MapChamber {
  id: string;
  x: number;
  y: number;
  r: number;
  component: ArchComponent;
}

export interface MapLayout {
  chambers: MapChamber[];
  byId: Map<string, MapChamber>;
  /** Where the mothership's corridor comes down into the map. */
  mouth: { x: number; y: number };
}

const PAD = 36;
/** Room under the lowest chambers for their labels. */
const LABEL_ROOM = 34;
const MIN_R = 18;
const MAX_R = 54;

/**
 * Scales archify's layout (a top-left `pos` and a `size` per component, in its own units) into the
 * plot below the surface line, keeping its proportions and centring it. A chamber's radius follows
 * its component's size, clamped so a tiny component is still a hole you can click and a huge one
 * does not swallow its neighbours.
 */
export function layoutMap(map: ArchMap, box: NestBox): MapLayout {
  const top = SURFACE_Y + 70;
  const comps = map.components;
  const minX = Math.min(...comps.map((c) => c.pos[0]));
  const minY = Math.min(...comps.map((c) => c.pos[1]));
  const maxX = Math.max(...comps.map((c) => c.pos[0] + c.size[0]));
  const maxY = Math.max(...comps.map((c) => c.pos[1] + c.size[1]));
  const spanX = Math.max(1, maxX - minX);
  const spanY = Math.max(1, maxY - minY);
  const availW = Math.max(1, box.width - PAD * 2);
  const availH = Math.max(1, box.height - top - PAD - LABEL_ROOM);
  const scale = Math.min(availW / spanX, availH / spanY);
  const offX = PAD + (availW - spanX * scale) / 2;
  const offY = top + (availH - spanY * scale) / 2;
  const chambers = comps.map((c) => {
    const r = Math.max(MIN_R, Math.min(MAX_R, (Math.min(c.size[0], c.size[1]) * scale) / 2 + 6));
    return {
      id: c.id,
      x: Math.round(offX + (c.pos[0] - minX + c.size[0] / 2) * scale),
      y: Math.round(offY + (c.pos[1] - minY + c.size[1] / 2) * scale),
      r: Math.round(r),
      component: c,
    };
  });
  return { chambers, byId: new Map(chambers.map((c) => [c.id, c])), mouth: { x: Math.round(box.width / 2), y: SURFACE_Y + 26 } };
}

/**
 * The component a repository path belongs to: a source that is the file itself wins; otherwise the
 * component whose source sits in the deepest directory containing the path (a source naming a
 * directory counts as that directory). `null` when nothing claims it.
 */
export function componentForPath(path: string, components: readonly ArchComponent[]): string | null {
  const clean = path.replace(/^\.\//, "");
  let best: { id: string; score: number } | null = null;
  for (const c of components) {
    for (const s of c.sources) {
      const src = s.path.replace(/^\.\//, "").replace(/\/$/, "");
      let score = 0;
      if (src === clean) score = 10_000 + src.length;
      else if (clean.startsWith(`${src}/`)) score = src.length;
      else {
        const dir = src.includes("/") ? src.slice(0, src.lastIndexOf("/")) : "";
        if (dir && clean.startsWith(`${dir}/`)) score = dir.length;
      }
      if (score > 0 && (!best || score > best.score)) best = { id: c.id, score };
    }
  }
  return best?.id ?? null;
}

/** Touched files per component, busiest first: where a colony's ants go. */
export function componentsForFiles(files: readonly string[], components: readonly ArchComponent[]): { id: string; files: string[] }[] {
  const hits = new Map<string, string[]>();
  for (const f of files) {
    const id = componentForPath(f, components);
    if (id) hits.set(id, [...(hits.get(id) ?? []), f]);
  }
  return [...hits.entries()].map(([id, fs]) => ({ id, files: fs })).sort((a, b) => b.files.length - a.files.length || a.id.localeCompare(b.id));
}

// --- Tunnels ----------------------------------------------------------------------------------

interface Dig {
  start: [number, number];
  hops: { c: [number, number]; e: [number, number] }[];
}

function rnd(seed: number): (n: number) => number {
  return (n) => {
    const v = Math.sin(seed + n * 12.9898) * 43758.5453;
    return v - Math.floor(v);
  };
}

/** A seed from two ids, the same whichever way round they come. */
export function edgeSeed(a: string, b: string): number {
  const key = [a, b].sort().join("→");
  let h = 7;
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) % 233_280;
  return h;
}

/** A wobbling corridor from a to b, as quadratic hops, the last landing exactly on b. */
function dig(a: [number, number], b: [number, number], seed: number): Dig {
  const r = rnd(seed);
  const len = Math.hypot(b[0] - a[0], b[1] - a[1]);
  const segs = Math.max(2, Math.min(5, Math.round(len / 90)));
  const amp = Math.min(34, 10 + len * 0.08);
  const hops: Dig["hops"] = [];
  let p = a;
  for (let q = 1; q <= segs; q++) {
    const t = q / segs;
    const e: [number, number] =
      q === segs ? b : [a[0] + (b[0] - a[0]) * t + (r(q) - 0.5) * amp, a[1] + (b[1] - a[1]) * t + (r(q + 40) - 0.5) * amp * 0.7];
    const c: [number, number] = [(p[0] + e[0]) / 2 + (r(q + 80) - 0.5) * amp, (p[1] + e[1]) / 2 + (r(q + 120) - 0.5) * amp * 0.6];
    hops.push({ c, e });
    p = e;
  }
  return { start: a, hops };
}

function reversed(d: Dig): Dig {
  const points = [d.start, ...d.hops.map((h) => h.e)];
  const hops = d.hops
    .map((h, i) => ({ c: h.c, e: points[i] }))
    .reverse();
  return { start: points[points.length - 1], hops };
}

const f = (n: number) => n.toFixed(0);
function toD(parts: Dig[]): string {
  if (!parts.length) return "";
  let d = `M${f(parts[0].start[0])} ${f(parts[0].start[1])}`;
  for (const part of parts) for (const h of part.hops) d += ` Q${f(h.c[0])} ${f(h.c[1])} ${f(h.e[0])} ${f(h.e[1])}`;
  return d;
}

/** The dug corridor between two chambers, drawn from the one sorted first so both ends agree. */
function edgeDig(layout: MapLayout, from: string, to: string): Dig | null {
  const a = layout.byId.get(from);
  const b = layout.byId.get(to);
  if (!a || !b) return null;
  const [first, second] = from < to ? [a, b] : [b, a];
  const d = dig([first.x, first.y], [second.x, second.y], edgeSeed(from, to));
  return from < to ? d : reversed(d);
}

/** Every connection as a tunnel path, once per pair. */
export function tunnelPaths(map: ArchMap, layout: MapLayout): { key: string; d: string; from: string; to: string }[] {
  const seen = new Set<string>();
  const out: { key: string; d: string; from: string; to: string }[] = [];
  for (const c of map.connections) {
    const key = [c.from, c.to].sort().join("|");
    if (c.from === c.to || seen.has(key)) continue;
    seen.add(key);
    const d = edgeDig(layout, c.from, c.to);
    if (d) out.push({ key, d: toD([d]), from: c.from, to: c.to });
  }
  return out;
}

/**
 * Where the mothership's corridor enters the map: the component with no incoming connection that
 * starts the most, else the one nearest the surface.
 */
export function entryComponent(map: ArchMap, layout: MapLayout): string | null {
  if (!layout.chambers.length) return null;
  const incoming = new Set(map.connections.map((c) => c.to));
  const outgoing = (id: string) => map.connections.filter((c) => c.from === id).length;
  const roots = layout.chambers.filter((c) => !incoming.has(c.id) && outgoing(c.id) > 0);
  const pool = roots.length ? roots : layout.chambers;
  return [...pool].sort((a, b) => outgoing(b.id) - outgoing(a.id) || a.y - b.y || a.x - b.x)[0].id;
}

/** The chambers between `from` and `to` along connections (either direction), shortest first. */
export function routeBetween(map: ArchMap, from: string, to: string): string[] | null {
  if (from === to) return [from];
  const next = new Map<string, string[]>();
  for (const c of map.connections) {
    next.set(c.from, [...(next.get(c.from) ?? []), c.to]);
    next.set(c.to, [...(next.get(c.to) ?? []), c.from]);
  }
  const prev = new Map<string, string>([[from, from]]);
  const queue = [from];
  while (queue.length) {
    const at = queue.shift()!;
    for (const n of next.get(at) ?? []) {
      if (prev.has(n)) continue;
      prev.set(n, at);
      if (n === to) {
        const path = [to];
        while (path[0] !== from) path.unshift(prev.get(path[0])!);
        return path;
      }
      queue.push(n);
    }
  }
  return null;
}

/** The mothership's corridor down to the entry chamber. */
export function mouthPath(layout: MapLayout, entry: string): string {
  const e = layout.byId.get(entry);
  if (!e) return "";
  return toD([dig([layout.mouth.x, layout.mouth.y], [e.x, e.y - e.r * 0.6], edgeSeed("mothership", entry))]);
}

/**
 * The path an ant walks: down the mothership's corridor to the entry chamber, then along the
 * tunnels to its target — the same curves the tunnels are drawn with, so it stays inside them. A
 * target no connection reaches gets its own corridor straight from the mouth.
 */
export function antRoute(map: ArchMap, layout: MapLayout, target: string): string {
  const t = layout.byId.get(target);
  if (!t) return "";
  const entry = entryComponent(map, layout);
  const route = entry ? routeBetween(map, entry, target) : null;
  if (!entry || !route) return toD([dig([layout.mouth.x, layout.mouth.y], [t.x, t.y], edgeSeed("mothership", target))]);
  const e = layout.byId.get(entry)!;
  const parts: Dig[] = [dig([layout.mouth.x, layout.mouth.y], [e.x, e.y - e.r * 0.6], edgeSeed("mothership", entry))];
  // From the corridor's end into the entry chamber's centre, then hop chamber to chamber.
  parts.push({ start: [e.x, e.y - e.r * 0.6], hops: [{ c: [e.x, e.y - e.r * 0.3], e: [e.x, e.y] }] });
  for (let i = 1; i < route.length; i++) {
    const d = edgeDig(layout, route[i - 1], route[i]);
    if (d) parts.push(d);
  }
  return toD(parts);
}

/** A soft mound behind a boundary's chambers: their bounding box, padded. */
export function boundaryBox(layout: MapLayout, wraps: readonly string[], pad = 22): { x: number; y: number; w: number; h: number } | null {
  const members = wraps.map((id) => layout.byId.get(id)).filter((c): c is MapChamber => Boolean(c));
  if (!members.length) return null;
  const x0 = Math.min(...members.map((c) => c.x - c.r)) - pad;
  const y0 = Math.min(...members.map((c) => c.y - c.r)) - pad - 14;
  const x1 = Math.max(...members.map((c) => c.x + c.r)) + pad;
  const y1 = Math.max(...members.map((c) => c.y + c.r)) + pad + 16;
  return { x: Math.round(x0), y: Math.round(y0), w: Math.round(x1 - x0), h: Math.round(y1 - y0) };
}
