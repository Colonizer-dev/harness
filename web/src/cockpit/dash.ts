// Dashboard stats for the cockpit overview + org dashboard (issue #398): pure derivations over
// SpendHistory and the session list. Every figure keeps its source — history days carry launched /
// returned / cost per org per day, sessions carry status / cost / repo — so anything without one
// (lead time, CI pass, coverage, latency, …) simply has no helper here and is never rendered.
import { orgOf, sameOrg, type Tone } from "../components/ui";
import { sessionCost, sumCosts } from "../spend";
import type { ModelProviderStatus, Session, SpendDay, SpendHistory, SpendOrgDay } from "../types";

export type RangeDays = 7 | 30 | 90;
export const RANGES: readonly RangeDays[] = [7, 30, 90];

/** Status-tone → CSS var, shared by the overview and org colony rows so the two cannot drift. */
export const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

export function sortedDays(history: SpendHistory | null): SpendDay[] {
  return [...(history?.days ?? [])].sort((a, b) => (a.day < b.day ? -1 : a.day > b.day ? 1 : 0));
}

/** Current = last `range` days; previous = the `range` before those (shorter when history is thin). */
export function slicePeriods(history: SpendHistory | null, range: number): { current: SpendDay[]; previous: SpendDay[] } {
  const days = sortedDays(history);
  const current = days.slice(Math.max(0, days.length - range));
  return { current, previous: days.slice(0, days.length - current.length).slice(-range) };
}

export function orgDayCost(day: SpendOrgDay): number | null {
  return sumCosts([day.cost_usd, day.routed_cost_usd]);
}

function orgDays(day: SpendDay, org?: string): SpendOrgDay[] {
  return org ? day.orgs.filter((o) => sameOrg(o.org, org)) : day.orgs;
}

/** One day's measured spend, null when no org in scope measured any. */
export function dayCost(day: SpendDay, org?: string): number | null {
  return sumCosts(orgDays(day, org).map(orgDayCost));
}

function sumOrg(days: SpendDay[], org: string | undefined, pick: (o: SpendOrgDay) => number): number {
  return days.reduce((n, d) => n + orgDays(d, org).reduce((m, o) => m + pick(o), 0), 0);
}

function dailyOrg(days: SpendDay[], org: string | undefined, pick: (o: SpendOrgDay) => number): number[] {
  return days.map((d) => orgDays(d, org).reduce((n, o) => n + pick(o), 0));
}

export function sumHistoryCost(days: SpendDay[], org?: string): number | null {
  return sumCosts(days.map((d) => dayCost(d, org)));
}

export function sumLaunched(days: SpendDay[], org?: string): number {
  return sumOrg(days, org, (o) => o.launched);
}

export function sumReturned(days: SpendDay[], org?: string): number {
  return sumOrg(days, org, (o) => o.returned);
}

export function sumTokens(days: SpendDay[], org?: string): number {
  return sumOrg(days, org, (o) => o.tokens.input + o.tokens.output + o.tokens.cache_read + o.tokens.cache_write);
}

export function dailyCosts(days: SpendDay[], org?: string): (number | null)[] {
  return days.map((d) => dayCost(d, org));
}

export function dailyLaunched(days: SpendDay[], org?: string): number[] {
  return dailyOrg(days, org, (o) => o.launched);
}

export function dailyReturned(days: SpendDay[], org?: string): number[] {
  return dailyOrg(days, org, (o) => o.returned);
}

/** Relative change current vs previous; null when there is no previous figure to stand on. */
export function relDelta(cur: number | null, prev: number | null): number | null {
  if (cur == null || prev == null || prev === 0) return null;
  const d = (cur - prev) / Math.abs(prev);
  return Number.isFinite(d) ? d : null;
}

/**
 * The delta chip for a compared figure: the usual +/-%, or undefined when there is nothing to
 * compare against — a previous period with nothing in it gets no chip (a percentage over zero means
 * nothing, and the Compare switch's tooltip already says the previous period is empty).
 */
export function compareDelta(cur: number | null, prev: number | null): { text: string; d: number | null } | undefined {
  if (prev == null || prev === 0) return undefined;
  const d = relDelta(cur, prev);
  return d == null ? undefined : { text: formatDelta(d), d };
}

export function formatDelta(d: number | null): string {
  if (d == null) return "—";
  const pct = Math.abs(d * 100);
  return `${d > 0 ? "+" : d < 0 ? "-" : ""}${pct >= 10 ? pct.toFixed(0) : pct.toFixed(1)}%`;
}

/** Sparkline points in a 100×28 box; unmeasured days sit on the baseline, never as zeroes. */
export function sparkPoints(values: (number | null)[], w = 100, h = 28, window = 7): string {
  if (values.length === 0) return "";
  // A trailing rolling mean (the design's 7-day roll): per-day counts are mostly 0s and 1s, and
  // drawn raw they read as a comb of spikes rather than a trend.
  const raw = values.map((v) => (v != null && Number.isFinite(v) ? v : 0));
  const span = Math.max(1, Math.min(window, raw.length));
  const nums = raw.map((_, i) => {
    const from = Math.max(0, i - span + 1);
    // Divided by the full window even at the start, so the first days are not inflated by a
    // short denominator.
    return raw.slice(from, i + 1).reduce((a, b) => a + b, 0) / span;
  });
  const max = Math.max(...nums);
  if (max <= 0) return values.map((_, i) => `${((i / Math.max(1, values.length - 1)) * w).toFixed(1)},${h}`).join(" ");
  return nums.map((v, i) => `${((i / Math.max(1, values.length - 1)) * w).toFixed(1)},${(h - 2 - (v / max) * (h - 4)).toFixed(1)}`).join(" ");
}

/** Launch → PR → merged funnel, read off current session statuses (a snapshot, not a history). */
export function funnelFor(sessions: Session[]): { launched: number; prOpened: number; merged: number } {
  let prOpened = 0;
  let merged = 0;
  for (const s of sessions) {
    if (s.status === "merged") {
      merged += 1;
      prOpened += 1;
    } else if (s.status === "pr_opened" || s.status === "closed") prOpened += 1;
  }
  return { launched: sessions.length, prOpened, merged };
}

export interface RepoRow {
  repo: string;
  colonies: number;
  merged: number;
  rate: number;
  spend: number | null;
}

/** Sessions grouped by repo: colonies, merged count, merge rate, measured spend. */
export function repoRows(sessions: Session[]): RepoRow[] {
  const by = new Map<string, Session[]>();
  for (const s of sessions) {
    const list = by.get(s.repo) ?? [];
    list.push(s);
    by.set(s.repo, list);
  }
  return [...by.entries()]
    .map(([repo, list]) => {
      const merged = list.filter((s) => s.status === "merged").length;
      return {
        repo,
        colonies: list.length,
        merged,
        rate: merged / list.length,
        spend: sumCosts(list.map(sessionCost)),
      };
    })
    .sort((a, b) => b.colonies - a.colonies || a.repo.localeCompare(b.repo));
}

/** Spend ÷ merged PRs; null when unmeasured or nothing merged yet. */
export function costPerMerged(spend: number | null, merged: number): number | null {
  if (spend == null || merged <= 0) return null;
  return spend / merged;
}

/** Deterministic hue for a name (FNV-1a into 0..359): the same org always draws the same
 *  colour, across reloads and across themes. */
export function orgHue(name: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < name.length; i++) {
    h ^= name.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0) % 360;
}

/** Per-org colour in the design's form, oklch(0.72 0.17 <hue>) — theme-independent like the
 *  reference, so it reads the same in light and dark. */
export function orgColorFor(name: string): string {
  return `oklch(0.72 0.17 ${orgHue(name)})`;
}

/** Orange ramp slot for a multi-series chart (issue #445): series take `var(--chart-N)` by
 *  series index, so org identity is carried by the avatar in legends/chips — never by hue.
 *  Per-theme values live in index.css, readable in light and dark. */
export function chartColor(index: number): string {
  return `var(--chart-${(index % 5) + 1})`;
}

/** A point in chart viewBox units: x spans 0..100 across the series, y spans 0..100 top to
 *  bottom (an SVG-native frame, so % overlays and the SVG agree without measuring). */
export interface ChartPoint {
  x: number;
  y: number;
}

/** ViewBox y for a value on a 0..max scale: the max sits `topPad` off the top edge, zero on
 *  the baseline at 100. Values below zero clamp to the frame instead of escaping it (bars
 *  ignore non-positive segments outright); a non-positive max collapses to the baseline. */
export function chartY(v: number, max: number, topPad = 2): number {
  if (!(max > 0)) return 100;
  return Math.min(100, Math.max(0, 100 - (v / max) * (100 - topPad)));
}

/** Splits a gappy series into drawable runs of viewBox points: null/NaN/unmeasured days end
 *  the current run, so the line breaks rather than zeroing through a gap. `topPad` keeps the
 *  max value off the frame's top edge; the baseline sits at y=100. */
export function chartRuns(values: (number | null)[], max: number, topPad = 2): ChartPoint[][] {
  const runs: ChartPoint[][] = [];
  let run: ChartPoint[] = [];
  const n = values.length;
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v) || max <= 0 || n === 0) {
      if (run.length > 0) runs.push(run);
      run = [];
      return;
    }
    run.push({
      x: colX(i, n),
      y: chartY(v, max, topPad),
    });
  });
  if (run.length > 0) runs.push(run);
  return runs;
}

/** The previous period's points with gaps joined, not split: the ghost dashed line connects
 *  across unmeasured days exactly like the old polyline did, so sparse history still draws a
 *  visible line instead of vanishing one-point subpaths. */
export function joinedPoints(values: (number | null)[], max: number, topPad = 2): ChartPoint[] {
  const pts: ChartPoint[] = [];
  const n = values.length;
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v)) return;
    pts.push({ x: colX(i, n), y: chartY(v, max, topPad) });
  });
  return pts;
}

const coord = (v: number): string => (Math.round(v * 10) / 10).toString();

/** Monotone cubic (Fritsch–Carlson) SVG path through the points: the curve passes through
 *  every point and never overshoots monotone data, unlike straight polylines or Catmull-Rom.
 *  Single points emit a bare moveto (the caller dots them); pairs emit a straight segment. */
export function monotonePath(pts: ChartPoint[]): string {
  if (pts.length === 0) return "";
  if (pts.length === 1) return `M${coord(pts[0].x)},${coord(pts[0].y)}`;
  if (pts.length === 2) return `M${coord(pts[0].x)},${coord(pts[0].y)}L${coord(pts[1].x)},${coord(pts[1].y)}`;
  const n = pts.length;
  const h: number[] = [];
  const m: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    const dx = pts[i + 1].x - pts[i].x;
    h.push(dx);
    m.push(dx === 0 ? 0 : (pts[i + 1].y - pts[i].y) / dx);
  }
  const t: number[] = new Array(n);
  t[0] = m[0];
  t[n - 1] = m[n - 2];
  for (let i = 1; i < n - 1; i++) {
    if (m[i - 1] === 0 || m[i] === 0 || (m[i - 1] < 0) !== (m[i] < 0)) t[i] = 0;
    else {
      // Weighted harmonic mean of the neighbouring secants, then the Carlson limiter so a
      // steep jump beside a flat stretch cannot bow the curve past either endpoint.
      const w1 = 2 * h[i] + h[i - 1];
      const w2 = h[i] + 2 * h[i - 1];
      t[i] = (w1 + w2) / (w1 / m[i - 1] + w2 / m[i]);
    }
  }
  for (let i = 0; i < n - 1; i++) {
    if (m[i] === 0) continue;
    const a = t[i] / m[i];
    const b = t[i + 1] / m[i];
    const s = a * a + b * b;
    if (s > 9) {
      const tau = 3 / Math.sqrt(s);
      t[i] = tau * a * m[i];
      t[i + 1] = tau * b * m[i];
    }
  }
  let d = `M${coord(pts[0].x)},${coord(pts[0].y)}`;
  for (let i = 0; i < n - 1; i++) {
    const c1x = pts[i].x + h[i] / 3;
    const c1y = pts[i].y + t[i] * h[i] / 3;
    const c2x = pts[i + 1].x - h[i] / 3;
    const c2y = pts[i + 1].y - t[i + 1] * h[i] / 3;
    d += `C${coord(c1x)},${coord(c1y)} ${coord(c2x)},${coord(c2y)} ${coord(pts[i + 1].x)},${coord(pts[i + 1].y)}`;
  }
  return d;
}

/** The gradient area under a smoothed run: the line path closed down to the baseline. */
export function monotoneArea(pts: ChartPoint[], baseline = 100): string {
  const line = monotonePath(pts);
  if (!line || pts.length < 2) return "";
  return `${line}L${coord(pts[pts.length - 1].x)},${baseline}L${coord(pts[0].x)},${baseline}Z`;
}

/** Per-model colour: the design's three pinned hues by name fragment, anything else hashed
 *  like an org so a new model still draws deterministically. */
export function modelColorFor(model: string): string {
  const id = model.toLowerCase();
  if (id.includes("deepseek")) return "var(--model-deepseek)";
  if (id.includes("opus")) return "var(--model-opus)";
  if (id.includes("muse-spark") || id.includes("muse_spark")) return "var(--model-spark)";
  return orgColorFor(model);
}

export type DeltaTone = "good" | "bad" | "flat";

/** Colours a period delta: flat (faint) when null or under half a point, else good/bad with
 *  `goodWhen` deciding which direction reads as good (spend and failure rates pass "down"). */
export function deltaTone(delta: number | null, goodWhen: "up" | "down" = "up"): DeltaTone {
  if (delta == null || !Number.isFinite(delta) || Math.abs(delta) < 0.005) return "flat";
  return (goodWhen === "up") === delta > 0 ? "good" : "bad";
}

// ---------------------------------------------------------------------------
// Overview derivations (issue #398): everything the OVERVIEW screen reads off the
// session list. Merged PRs bucket by merged_at (falling back to created_at when the
// mothership omits it); failed sessions bucket by created_at. Lead time / PR cycle
// time / CI pass rate live in delivery.ts, and the change-failure rate is a
// snapshot reading (failed ÷ decided in the window, merged read by merge time and
// failed by created_at), never a history.
// ---------------------------------------------------------------------------

function windowMs(ts: string, fromMs: number, toMs: number): boolean {
  const t = Date.parse(ts);
  return !Number.isNaN(t) && t >= fromMs && t < toMs;
}

/** A merged session's merge time: merged_at when the mothership serves one, else created_at. */
export function mergedAtOf(s: Session): string {
  return s.merged_at ?? s.created_at;
}

/** Sessions with status merged in [fromMs, toMs), bucketed by merge time (merged_at,
 *  falling back to created_at when absent). */
export function mergedInWindow(sessions: Session[], fromMs: number, toMs: number): Session[] {
  return sessions.filter((s) => s.status === "merged" && windowMs(mergedAtOf(s), fromMs, toMs));
}

/** The local-calendar "YYYY-MM-DD" of a Date — the same day key the mock's spend
 *  history is built on, so session buckets line up with history days. */
export function dayKeyOfDate(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** The local-calendar "YYYY-MM-DD" of a timestamp — the same day key the mock's spend
 *  history is built on, so session buckets line up with history days. */
export function dayKeyOf(ts: string): string {
  return dayKeyOfDate(new Date(ts));
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "2026-09-03" → "Sep 3", for the chart's sparse x labels. */
export function shortDayLabel(day: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!m) return day;
  return `${MONTHS[Number(m[2]) - 1] ?? ""} ${Number(m[3])}`;
}

/** Merged sessions per day over `days` (ascending "YYYY-MM-DD"), bucketed by merge
 *  time (merged_at, falling back to created_at), optionally scoped to one org. */
export function dailyMerged(sessions: Session[], days: string[], org?: string): number[] {
  return days.map((day) => sessions.filter((s) => s.status === "merged" && dayKeyOf(mergedAtOf(s)) === day && (!org || sameOrg(orgOf(s), org))).length);
}

export interface FailRate {
  /** failed ÷ (merged + failed) in the window — merged read by merge time, failed by
   *  created_at; null when nothing was decided. */
  rate: number | null;
  failed: number;
  /** merged + failed: the snapshot basis, named in the sub-line wherever it renders. */
  decided: number;
}

/** The honest change-failure reading: failed ÷ decided in the window, with merged
 *  sessions bucketed by merge time and failed ones by created_at. */
export function changeFailRate(sessions: Session[], fromMs: number, toMs: number, org?: string): FailRate {
  const inScope = (status: Session["status"]) =>
    sessions.filter(
      (s) =>
        s.status === status &&
        windowMs(status === "merged" ? mergedAtOf(s) : s.created_at, fromMs, toMs) &&
        (!org || sameOrg(orgOf(s), org)),
    );
  const failed = inScope("failed").length;
  const decided = failed + inScope("merged").length;
  return { rate: decided > 0 ? failed / decided : null, failed, decided };
}

/** Per-day failure rate over `days` (null = nothing decided that day, a gap — never a zero).
 *  Merged sessions bucket by merge day, failed ones by created day. */
export function dailyFailRate(sessions: Session[], days: string[], org?: string): (number | null)[] {
  return days.map((day) => {
    const list = sessions.filter((s) => dayKeyOf(s.status === "merged" ? mergedAtOf(s) : s.created_at) === day && (!org || sameOrg(orgOf(s), org)));
    const decided = list.filter((s) => s.status === "merged" || s.status === "failed").length;
    if (decided === 0) return null;
    return list.filter((s) => s.status === "failed").length / decided;
  });
}

/** When a colony started waiting: the watchdog flag's `since`, else the last update. */
export function waitingSince(session: Session): string {
  return session.attention?.since ?? session.updated_at;
}

/** Milliseconds a colony has waited, clamped at zero; unparseable stamps read as zero. */
export function waitingMs(session: Session, nowMs: number = Date.now()): number {
  const t = Date.parse(waitingSince(session));
  return Number.isNaN(t) ? 0 : Math.max(0, nowMs - t);
}

/** Compact wait/age: "45s", "22m", "7h", "3d"; unmeasurable reads as "—", never "NaNd". */
export function formatWait(ms: number): string {
  if (!Number.isFinite(ms)) return "—";
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** A rate delta in points ("1.3 pts"), the design's unit for failure-rate moves. */
export function formatPts(d: number | null): string {
  if (d == null || !Number.isFinite(d)) return "—";
  return `${(Math.abs(d) * 100).toFixed(1)} pts`;
}

/** Distinct repositories with sessions in scope — the workspace card's repo count. */
export function orgRepos(sessions: Session[], org: string): number {
  return new Set(sessions.filter((s) => sameOrg(orgOf(s), org)).map((s) => s.repo)).size;
}

/** Cumulative provider tallies for the org dashboard's API-error tile and latency caption —
 *  GET /api/status `model_providers`, mapped by the caller. The view fetches nothing itself,
 *  like every dashboard surface; absent means those two figures stay empty. */
export interface ProviderErrorSnapshot {
  name: string;
  requests: number;
  failures: number;
  /** Mean dispatched-request duration; null when the mothership never measured one. */
  avgLatencyMs?: number | null;
  /** When the tally started; null when the mothership doesn't say. */
  since?: string | null;
}

/** Maps GET /api/status `model_providers` onto ProviderErrorSnapshot. The status payload
 *  carries no failure count — only `failure_pct`, rounded to one decimal by the mothership —
 *  so failures are re-derived from it (the displayed rate reads back exactly); `avg_latency_ms`
 *  is 0 with no requests, which maps to null; `since` is not served, so it stays absent. */
export function providerSnapshots(from: readonly ModelProviderStatus[] | null | undefined): ProviderErrorSnapshot[] {
  return (from ?? []).map((p) => ({
    name: p.name,
    requests: p.requests,
    failures: Math.round((p.requests * (p.failure_pct ?? 0)) / 100),
    avgLatencyMs: p.avg_latency_ms > 0 ? p.avg_latency_ms : null,
  }));
}

/** Where day `i` of `n` sits across a chart, in percent: edge to edge, so the first day is on the
 *  left edge and the last on the right edge rather than half a column in from each. A single day
 *  sits in the middle. */
export function colX(i: number, n: number): number {
  return n <= 1 ? 50 : (i / (n - 1)) * 100;
}
