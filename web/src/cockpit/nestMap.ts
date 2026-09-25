// The architecture map as a nest (crates/colonizer/src/maps.rs makes the map; NestMapView draws it).
// Pure on purpose, like nest.ts: which component a touched file belongs to, where each chamber sits
// on the plot, and the tunnels an ant walks to reach it are all decided here and pinned by tests.
import type { ArchMap, ArchComponent } from "../types";
import { SURFACE_Y, type NestBox } from "./nest";

/**
 * A stored map made safe to draw: a hand-edited or half-written map can miss a list, a position, a
 * size, a name or a component's sources, and the map view must not throw on it. Idempotent.
 */
export function normalizeMap(map: ArchMap): ArchMap {
  const num = (v: unknown, d: number) => (typeof v === "number" && Number.isFinite(v) ? v : d);
  const list = <T,>(v: T[] | undefined | null): T[] => (Array.isArray(v) ? v : []);
  const components = list(map?.components)
    .filter((c) => c && c.id != null)
    .map((c) => ({
      ...c,
      id: String(c.id),
      type: String(c.type ?? ""),
      label: String(c.label ?? c.id),
      pos: [num(c.pos?.[0], 0), num(c.pos?.[1], 0)] as [number, number],
      size: [Math.max(1, num(c.size?.[0], 160)), Math.max(1, num(c.size?.[1], 64))] as [number, number],
      sources: list(c.sources).filter((s) => s && typeof s.path === "string"),
    }));
  return {
    ...map,
    title: String(map?.title ?? ""),
    components,
    connections: list(map?.connections).filter((c) => c && c.from != null && c.to != null),
    boundaries: list(map?.boundaries).map((b) => ({ ...b, label: String(b?.label ?? ""), wraps: list(b?.wraps) })),
  };
}

/** A box on the plot, in plot (world) pixels. */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Where a chamber's name goes: under the hole, or beside it. */
export type LabelSide = "below" | "right";

/** A component placed on the plot: its chamber's centre and radius, and its name's box, in plot pixels. */
export interface MapChamber {
  id: string;
  x: number;
  y: number;
  r: number;
  component: ArchComponent;
  /** The name (and the grey description under it) — never overlapping another chamber's. */
  label: Rect;
  /** Too tight even after spreading out (a degenerate map): the description is left off. */
  compact: boolean;
}

/** A boundary's mound, and where its title sits so it overlaps no other title or chamber. */
export interface MapMound {
  label: string;
  box: Rect;
  title: Rect | null;
}

export interface MapLayout {
  chambers: MapChamber[];
  byId: Map<string, MapChamber>;
  mounds: MapMound[];
  labelSide: LabelSide;
  /** The plot the map is drawn on: at least the viewport, larger when the map needs room to stay legible. */
  width: number;
  height: number;
  /** Where the mothership's corridor comes down into the map. */
  mouth: { x: number; y: number };
}

const PAD = 28;
/** Room between the surface line and the highest mound: the mothership's corridor comes down here. */
const TOP = SURFACE_Y + 46;
const MIN_R = 16;
const MAX_R = 30;
const BASE_R = 21;
const GAP = 12;
/** A name is cut off (with an ellipsis, full text on hover) past this width. */
export const LABEL_MAX_W = 150;
const LABEL_LINE = 17;
const SUB_LINE = 15;
/** Between a hole and its name. */
const LABEL_OFFSET = 6;
/** A map never spreads past this many pixels a side, however degenerate archify's arrangement. */
const MAX_WORLD = 6000;
const TITLE_H = 14;

/** A rough width for text in the cockpit's UI font: enough to keep names apart, CSS truncates the rest. */
export function textWidth(text: string, px: number): number {
  return Math.ceil(text.length * px * 0.56);
}

function overlaps(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

function overlapArea(a: Rect, b: Rect): number {
  const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return w > 0 && h > 0 ? w * h : 0;
}

interface Node {
  c: ArchComponent;
  /** Centre in archify's units. */
  ux: number;
  uy: number;
  r: number;
  lw: number;
  lh: number;
  /** The chamber-plus-name box's extents around the centre, per label side. */
  ext: Record<LabelSide, { l: number; r: number; t: number; b: number }>;
}

function nodesOf(map: ArchMap): Node[] {
  const comps = map.components;
  const areas = comps.map((c) => Math.max(1, c.size[0] * c.size[1])).sort((a, b) => a - b);
  const median = areas[Math.floor(areas.length / 2)] ?? 1;
  const seen = new Set<string>();
  return comps.map((c) => {
    let ux = c.pos[0] + c.size[0] / 2;
    let uy = c.pos[1] + c.size[1] / 2;
    // Two components on the very same spot can never be pulled apart by scaling: nudge the later one.
    while (seen.has(`${ux},${uy}`)) {
      ux += Math.max(1, c.size[0] / 2);
      uy += Math.max(1, c.size[1] / 2);
    }
    seen.add(`${ux},${uy}`);
    const r = Math.round(Math.max(MIN_R, Math.min(MAX_R, BASE_R * Math.sqrt(Math.max(1, c.size[0] * c.size[1]) / median))));
    const lw = Math.min(LABEL_MAX_W, Math.max(textWidth(c.label, 12.5), c.sublabel ? textWidth(c.sublabel, 10.5) : 0) + 4);
    const lh = LABEL_LINE + (c.sublabel ? SUB_LINE : 0);
    const half = Math.max(r, lw / 2);
    const side = Math.max(r, lh / 2);
    return {
      c,
      ux,
      uy,
      r,
      lw,
      lh,
      ext: {
        below: { l: -half, r: half, t: -r, b: r + LABEL_OFFSET + lh },
        right: { l: -r, r: r + LABEL_OFFSET + 2 + lw, t: -side, b: side },
      },
    };
  });
}

/**
 * The smallest spread (pixels per archify unit, per axis) at or above the viewport-filling one that
 * keeps every chamber-plus-name box clear of every other: each pair must be apart on at least one
 * axis, and the cheaper axis is stretched when it is not. `stuck` holds pairs no spread can part.
 */
function spread(nodes: Node[], side: LabelSide, sx0: number, sy0: number, capX: number, capY: number): { sx: number; sy: number; stuck: Set<number> } {
  let sx = sx0;
  let sy = sy0;
  const stuck = new Set<number>();
  for (let pass = 0; pass < 64; pass++) {
    let changed = false;
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const [a, b] = nodes[i].ux <= nodes[j].ux ? [nodes[i], nodes[j]] : [nodes[j], nodes[i]];
        const [p, q] = nodes[i].uy <= nodes[j].uy ? [nodes[i], nodes[j]] : [nodes[j], nodes[i]];
        const dx = b.ux - a.ux;
        const dy = q.uy - p.uy;
        const needX = a.ext[side].r - b.ext[side].l + GAP;
        const needY = p.ext[side].b - q.ext[side].t + GAP;
        if (dx * sx >= needX - 0.01 || dy * sy >= needY - 0.01) continue;
        const wantX = dx > 0 ? needX / dx : Infinity;
        const wantY = dy > 0 ? needY / dy : Infinity;
        const okX = wantX <= capX;
        const okY = wantY <= capY;
        if (!okX && !okY) {
          stuck.add(i).add(j);
          continue;
        }
        if (okX && (!okY || wantX / sx <= wantY / sy)) sx = wantX;
        else sy = wantY;
        changed = true;
      }
    }
    if (!changed) break;
  }
  return { sx, sy, stuck };
}

/** How nested a boundary is: one mound wrapping another gets a wider margin, so their edges and titles part. */
function nesting(boundaries: ArchMap["boundaries"], present: Set<string>): number[] {
  const sets = boundaries.map((b) => new Set(b.wraps.filter((id) => present.has(id))));
  return sets.map((s, i) =>
    sets.filter((o, j) => j !== i && o.size > 0 && o.size < s.size && [...o].every((id) => s.has(id))).length,
  );
}

function place(map: ArchMap, view: NestBox, side: LabelSide): MapLayout {
  const nodes = nodesOf(map);
  const cxs = nodes.map((n) => n.ux);
  const cys = nodes.map((n) => n.uy);
  const minUx = Math.min(...cxs);
  const minUy = Math.min(...cys);
  const spanX = Math.max(...cxs) - minUx;
  const spanY = Math.max(...cys) - minUy;
  const extW = Math.max(...nodes.map((n) => n.ext[side].r - n.ext[side].l));
  const extH = Math.max(...nodes.map((n) => n.ext[side].b - n.ext[side].t));
  const depth = nesting(map.boundaries, new Set(nodes.map((n) => n.c.id)));
  const moundRoom = (Math.max(0, ...depth) + 1) * 16 + 24 + TITLE_H;
  // Fill the viewport first; spread() only ever widens that.
  const availW = Math.max(1, view.width - PAD * 2 - extW - (map.boundaries.length ? 2 * moundRoom : 0));
  const availH = Math.max(1, view.height - TOP - PAD - extH - (map.boundaries.length ? 2 * moundRoom : 0));
  const sx0 = spanX > 0 ? availW / spanX : 1;
  const sy0 = spanY > 0 ? availH / spanY : 1;
  const spreadOut = spread(nodes, side, sx0, sy0, spanX > 0 ? MAX_WORLD / spanX : 1, spanY > 0 ? MAX_WORLD / spanY : 1);
  const { stuck } = spreadOut;

  const build = (sx: number, sy: number) => {
    const chambers: MapChamber[] = nodes.map((n, i) => {
      const x = (n.ux - minUx) * sx;
      const y = (n.uy - minUy) * sy;
      const label =
        side === "below"
          ? { x: x - n.lw / 2, y: y + n.r + LABEL_OFFSET, w: n.lw, h: n.lh }
          : { x: x + n.r + LABEL_OFFSET + 2, y: y - n.lh / 2, w: n.lw, h: n.lh };
      return { id: n.c.id, x, y, r: n.r, component: n.c, label, compact: stuck.has(i) };
    });
    const byId = new Map(chambers.map((c) => [c.id, c]));
    const mounds: MapMound[] = [];
    map.boundaries.forEach((b, i) => {
      const box = moundBox(byId, b.wraps, 16 + depth[i] * 16, b.label ? TITLE_H + 6 : 0);
      if (box) mounds.push({ label: b.label, box, title: null });
    });
    const boxes = [...chambers.flatMap((c) => [chamberRect(c), c.label]), ...mounds.map((m) => m.box)];
    const minX = Math.min(...boxes.map((r) => r.x));
    const minY = Math.min(...boxes.map((r) => r.y));
    const contentW = Math.max(...boxes.map((r) => r.x + r.w)) - minX;
    const contentH = Math.max(...boxes.map((r) => r.y + r.h)) - minY;
    return { chambers, byId, mounds, minX, minY, contentW, contentH };
  };

  // Fill the whole viewport — at the zoom the map will be shown at — on both axes: an axis with room
  // to spare is stretched (which only ever parts chambers further) until the map meets the edges.
  let { sx, sy } = spreadOut;
  let built = build(sx, sy);
  for (let pass = 0; pass < 3; pass++) {
    const w = built.contentW + PAD * 2;
    const h = TOP + built.contentH + PAD;
    const k = Math.min(1, view.width / w, view.height / h);
    const targetW = view.width / k;
    const targetH = view.height / k;
    let grew = false;
    if (spanX > 0 && w < targetW - 1) {
      const fixed = built.contentW - spanX * sx;
      const next = (targetW - PAD * 2 - fixed) / spanX;
      if (next > sx * 1.001) (sx = Math.min(next, MAX_WORLD / spanX)), (grew = true);
    }
    if (spanY > 0 && h < targetH - 1) {
      const fixed = built.contentH - spanY * sy;
      const next = (targetH - TOP - PAD - fixed) / spanY;
      if (next > sy * 1.001) (sy = Math.min(next, MAX_WORLD / spanY)), (grew = true);
    }
    if (!grew) break;
    built = build(sx, sy);
  }
  const { chambers, byId, mounds, minX, minY, contentW, contentH } = built;

  // Shift everything so the highest mound sits under the corridor, and centre a map narrower than the viewport.
  const width = Math.round(Math.max(view.width, contentW + PAD * 2));
  const height = Math.round(Math.max(view.height, TOP + contentH + PAD));
  const ox = (width - contentW) / 2 - minX;
  const oy = TOP - minY;
  for (const c of chambers) {
    c.x = Math.round(c.x + ox);
    c.y = Math.round(c.y + oy);
    c.label = { x: Math.round(c.label.x + ox), y: Math.round(c.label.y + oy), w: c.label.w, h: c.label.h };
  }
  for (const m of mounds) m.box = { x: Math.round(m.box.x + ox), y: Math.round(m.box.y + oy), w: Math.round(m.box.w), h: Math.round(m.box.h) };
  placeTitles(mounds, chambers);
  return { chambers, byId, mounds, labelSide: side, width, height, mouth: { x: Math.round(width / 2), y: SURFACE_Y + 26 } };
}

function chamberRect(c: { x: number; y: number; r: number }): Rect {
  return { x: c.x - c.r, y: c.y - c.r, w: c.r * 2, h: c.r * 2 };
}

/** A mound's box: its chambers and their names, padded, with room on top for its title. */
function moundBox(byId: Map<string, MapChamber>, wraps: readonly string[], pad: number, titleRoom: number): Rect | null {
  const members = wraps.map((id) => byId.get(id)).filter((c): c is MapChamber => Boolean(c));
  if (!members.length) return null;
  const rects = members.flatMap((c) => [chamberRect(c), c.label]);
  const x0 = Math.min(...rects.map((r) => r.x)) - pad;
  const y0 = Math.min(...rects.map((r) => r.y)) - pad - titleRoom;
  const x1 = Math.max(...rects.map((r) => r.x + r.w)) + pad;
  const y1 = Math.max(...rects.map((r) => r.y + r.h)) + pad;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/**
 * Each mound's title, at the first spot along its top edge, its bottom edge or just outside it that is
 * clear of every chamber, name and title already placed (else the least-covered spot), cut to the mound's width.
 */
function placeTitles(mounds: MapMound[], chambers: MapChamber[]): void {
  const taken: Rect[] = chambers.flatMap((c) => [chamberRect(c), c.label]);
  for (const m of mounds) {
    if (!m.label) continue;
    const { x, y, w, h } = m.box;
    const tw = Math.min(Math.max(24, w - 24), textWidth(m.label, 10.5) + Math.ceil(m.label.length * 1.05) + 4);
    const top = y + 6;
    const bottom = y + h - TITLE_H - 6;
    // Inside along the top, then along the bottom, then just outside the mound's edge.
    const rows = [top, bottom, y - TITLE_H - 3, y + h + 3];
    const cols = [x + (w - tw) / 2, x + 12, x + w - 12 - tw];
    const spots: Rect[] = rows.flatMap((ry) => cols.map((rx) => ({ x: rx, y: ry, w: tw, h: TITLE_H })));
    const cost = (r: Rect) => taken.reduce((sum, t) => sum + overlapArea(r, t), 0);
    const best = spots.find((r) => !taken.some((t) => overlaps(r, t))) ?? [...spots].sort((a, b) => cost(a) - cost(b))[0];
    m.title = { x: Math.round(best.x), y: Math.round(best.y), w: Math.round(best.w), h: TITLE_H };
    taken.push(m.title);
  }
}

/**
 * Lays archify's arrangement out on a plot at least as big as the viewport: archify's positions
 * (a top-left `pos` and a `size` per component, in its own units) are scaled per axis to fill the
 * viewport, then spread further only as far as keeps every chamber and its name clear of every
 * other — so a dense map gets a larger plot (panned and zoomed to fit) rather than overlapping text.
 * Names go under the holes or beside them, whichever needs less zooming out to fit.
 */
export function layoutMap(map: ArchMap, viewIn: NestBox): MapLayout {
  // A plot not laid out yet (hidden, or measured before mount) reads 0×0: lay out for a sensible size instead.
  const view = { width: sane(viewIn.width, 960), height: sane(viewIn.height, 560) };
  map = normalizeMap(map);
  if (!map.components.length) {
    return { chambers: [], byId: new Map(), mounds: [], labelSide: "below", width: view.width, height: view.height, mouth: { x: Math.round(view.width / 2), y: SURFACE_Y + 26 } };
  }
  const below = place(map, view, "below");
  const right = place(map, view, "right");
  // Beside wins only when it clearly needs less zooming out; under is the nest's usual look.
  return fitView(right, view).k > fitView(below, view).k * 1.05 ? right : below;
}

/** Every pair of chamber names (or chambers) that overlap: empty for a laid-out map. */
export function labelCollisions(layout: MapLayout): [string, string][] {
  const out: [string, string][] = [];
  const cs = layout.chambers;
  for (let i = 0; i < cs.length; i++)
    for (let j = i + 1; j < cs.length; j++) {
      const a = [chamberRect(cs[i]), cs[i].label];
      const b = [chamberRect(cs[j]), cs[j].label];
      if (a.some((x) => b.some((y) => overlaps(x, y)))) out.push([cs[i].id, cs[j].id]);
    }
  return out;
}

/** Mound titles that overlap each other or any chamber or name. */
export function titleCollisions(layout: MapLayout): string[] {
  const out: string[] = [];
  const titles = layout.mounds.filter((m) => m.title);
  const things = layout.chambers.flatMap((c) => [chamberRect(c), c.label]);
  titles.forEach((m, i) => {
    if (things.some((t) => overlaps(m.title!, t)) || titles.some((o, j) => j !== i && overlaps(m.title!, o.title!))) out.push(m.label);
  });
  return out;
}

// --- Pan and zoom -----------------------------------------------------------------------------

/** How the plot is shown in the viewport: screen = plot × k + (x, y). */
export interface MapView {
  k: number;
  x: number;
  y: number;
}

export const MAX_ZOOM = 2.5;
/** Below this zoom the grey descriptions are too small to read and are left off. */
export const SUBLABEL_MIN_ZOOM = 0.72;
/** Below this zoom names are left off too (hover or tap a chamber for its name). */
export const LABEL_MIN_ZOOM = 0.38;

function sane(n: number, fallback: number): number {
  return Number.isFinite(n) && n >= 1 ? n : fallback;
}

/** The whole plot in the viewport, never enlarged, centred across and from the top. */
export function fitView(plot: { width: number; height: number }, viewIn: NestBox): MapView {
  const view = { width: sane(viewIn.width, 1), height: sane(viewIn.height, 1) };
  const k = Math.max(0.02, Math.min(1, view.width / sane(plot.width, 1), view.height / sane(plot.height, 1)));
  return { k, x: (view.width - plot.width * k) / 2, y: 0 };
}

/** Keeps the plot on screen: a plot smaller than the viewport stays inside it, a larger one covers it. */
export function clampView(v: MapView, plot: { width: number; height: number }, view: NestBox): MapView {
  const minK = Math.min(fitView(plot, view).k, 1) * 0.75;
  const k = Math.max(minK, Math.min(MAX_ZOOM, v.k));
  const axis = (pos: number, size: number, room: number) => {
    const s = size * k;
    return s <= room ? Math.max(0, Math.min(room - s, pos)) : Math.max(room - s, Math.min(0, pos));
  };
  return { k, x: axis(v.x, plot.width, view.width), y: axis(v.y, plot.height, view.height) };
}

/** Zooms by `factor` about a viewport point, keeping the plot point under it still. */
export function zoomAt(v: MapView, factor: number, px: number, py: number, plot: { width: number; height: number }, view: NestBox): MapView {
  const k = clampView({ ...v, k: v.k * factor }, plot, view).k;
  const ratio = k / v.k;
  return clampView({ k, x: px - (px - v.x) * ratio, y: py - (py - v.y) * ratio }, plot, view);
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

/** A soft mound behind a boundary's chambers: their boxes (names included), padded. */
export function boundaryBox(layout: MapLayout, wraps: readonly string[], pad = 22): Rect | null {
  const r = moundBox(layout.byId, wraps, pad, TITLE_H);
  return r && { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.w), h: Math.round(r.h) };
}
