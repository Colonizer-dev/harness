// Shared dashboard primitives for the cockpit, in the Cockpit Dashboards v3 idiom (Claude Design
// import): no cards — sections are a quiet 14px heading over content held between two hairlines.
// The pieces here are the KPI strip, the stacked smoothed-area chart with its side column, the
// segmented tabs, the range toolbar, the share strip and the colony row, shared by the overview
// and the org dashboard so the two cannot drift apart.
//
// Rendering never distorts: the chart SVG (viewBox 0 0 100 100, preserveAspectRatio="none")
// carries only areas and lines — strokes hold their width via vector-effect — while dots, the
// crosshair and the tooltip are HTML overlays positioned in %, so circles stay circular at any
// width. No measuring: static markup renders the full geometry, which the tests assert on.
import { useId, useState, type CSSProperties, type ReactElement, type ReactNode } from "react";

import { Avatar, initialOf } from "../components/Avatar";
import { SESSION_STATUS, isLive, orgOf, type Tone } from "../components/ui";
import { sessionCost } from "../spend";
import type { Session } from "../types";
import { monotonePath, orgColorFor, RANGES, type ChartPoint, type RangeDays } from "./dash";
import { LiveCost, TweenedValue } from "./Live";

/** A section: the 14px heading with its faint meta, an optional right slot, and the body. */
export function Section({
  title,
  meta,
  right,
  children,
  id,
}: {
  title: string;
  meta?: ReactNode;
  right?: ReactNode;
  children: ReactNode;
  id?: string;
}): ReactElement {
  return (
    <section id={id} className="min-w-0">
      <div className="mb-3 flex flex-wrap items-baseline justify-between gap-x-5 gap-y-2">
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2.5">
          <h2 className="m-0 text-[14px] font-medium text-text">{title}</h2>
          {meta != null && meta !== "" && <span className="text-[13px] text-faint">{meta}</span>}
        </div>
        {right}
      </div>
      {children}
    </section>
  );
}

/** The two hairlines every section body sits between. */
export function Rules({ children, className }: { children: ReactNode; className?: string }): ReactElement {
  return <div className={`overflow-hidden border-y border-border ${className ?? ""}`}>{children}</div>;
}

/** Legend entries for a section heading: a small square per series. */
export function DashLegend({ items }: { items: Array<{ label: string; color: string; icon?: ReactNode; dashed?: boolean }> }): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-x-3.5 gap-y-1 text-[12.5px] text-muted">
      {items.map((item) => (
        <span key={item.label} className="inline-flex items-center gap-1.5">
          {item.icon ??
            (item.dashed ? (
              <span aria-hidden="true" className="h-0 w-3 border-t border-dashed border-muted" />
            ) : (
              <span aria-hidden="true" className="h-2 w-2 rounded-[2px]" style={{ background: item.color }} />
            ))}
          {item.label}
        </span>
      ))}
    </div>
  );
}

/** One KPI: value, previous-period delta, sparkline, sub-line. KPIs with no data source are not
 *  tiles at all in v3 — the strip lists them once in its footnote (see KpiStrip). */
export interface KpiDef {
  label: string;
  value: string;
  /** Numeric twin of `value`: when present the value tweens between pushes instead of snapping.
   *  Static markup shows the target, so keep the two in agreement. */
  valueNum?: number;
  /** Formats the tweened number; defaults to rounding. */
  formatNum?: (n: number) => string;
  /** Absent while the compare toggle is off or a snapshot has no previous period. */
  delta?: string;
  /** Colours the delta (good → ok, bad → err, flat/absent → faint); pair with deltaTone(). */
  deltaTone?: "good" | "bad" | "flat";
  spark?: string;
  sub?: string;
  /** No data source: the strip names it in the footnote instead of drawing an empty tile. */
  unmeasured?: boolean;
  /** A measured KPI with nothing to read yet ("nothing merged yet"): "—" plus this line. */
  emptyNote?: string;
  hint: string;
}

const DELTA_CLASS = { good: "text-ok", bad: "text-err", flat: "text-faint" } as const;

export function KpiTile({ label, value, valueNum, formatNum, delta, deltaTone, spark, sub, emptyNote, hint }: KpiDef): ReactElement {
  const stroke = deltaTone === "bad" ? "var(--err)" : "var(--chart-1)";
  return (
    <div title={hint} className="flex min-w-0 flex-col gap-1.5 px-5 pb-4 pt-5 shadow-[-1px_0_0_var(--border),0_-1px_0_var(--border)]">
      <div className="truncate text-[13px] text-muted">{label}</div>
      <div className="flex min-w-0 items-baseline gap-2">
        <span className="whitespace-nowrap text-[28px] font-semibold tabular-nums tracking-[-0.03em]">
          {emptyNote != null ? "—" : valueNum != null ? <TweenedValue value={valueNum} format={formatNum} /> : value}
        </span>
        {delta && emptyNote == null && <span className={`whitespace-nowrap text-[12.5px] ${DELTA_CLASS[deltaTone ?? "flat"]}`}>{delta}</span>}
      </div>
      {spark && emptyNote == null ? <Sparkline points={spark} color={stroke} /> : <div aria-hidden="true" className="mt-1 h-6" />}
      <div className="truncate text-[12.5px] text-faint">{emptyNote ?? sub ?? ""}</div>
    </div>
  );
}

/** The KPI strip: measured tiles between hairlines, and one line naming what is not measured. */
export function KpiStrip({ items, note }: { items: KpiDef[]; note?: string }): ReactElement {
  const shown = items.filter((k) => !k.unmeasured);
  const missing = items.filter((k) => k.unmeasured).map((k) => k.label);
  const footnote = [note, missing.length > 0 ? `${joinList(missing)} ${missing.length === 1 ? "is" : "are"} not measured yet — no data source.` : null]
    .filter(Boolean)
    .join(" · ");
  return (
    <div>
      <Rules>
        <div className="grid [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
          {shown.map((k) => (
            <KpiTile key={k.label} {...k} />
          ))}
        </div>
      </Rules>
      {footnote && <div className="mt-2.5 text-[12.5px] text-faint [text-wrap:pretty]">{footnote}</div>}
    </div>
  );
}

function joinList(items: string[]): string {
  if (items.length <= 1) return items.join("");
  const [first, ...rest] = items;
  return `${[first, ...rest.slice(0, -1)].join(", ")} and ${rest[rest.length - 1]}`;
}

/** Back from the "x,y x,y …" spark string (the 100×28 box sparkPoints emits) to points. */
function parseSpark(points: string): ChartPoint[] {
  return points
    .split(" ")
    .map((pt) => pt.split(",").map(Number))
    .filter((pair): pair is [number, number] => pair.length === 2 && pair.every((v) => Number.isFinite(v)))
    .map(([x, y]) => ({ x, y }));
}

/** The v3 sparkline: one smoothed line with a soft glow, no fill. Keeps the 100×28 box sparkPoints emits. */
export function Sparkline({ points, color, height = 24 }: { points: string; color: string; height?: number }): ReactElement | null {
  const pts = parseSpark(points);
  if (pts.length < 2) return <div aria-hidden="true" className="mt-1" style={{ height }} />;
  return (
    <svg viewBox="0 0 100 28" preserveAspectRatio="none" aria-hidden="true" className="mt-1 block w-full overflow-visible" style={{ height }}>
      <path
        key={points}
        d={monotonePath(pts)}
        fill="none"
        stroke={color}
        strokeWidth={1.5}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
        // Revealed left to right by a clip, not a dash offset: a dashed draw-in on a
        // non-scaling stroke renders as broken segments in Chrome.
        className="spark-reveal"
        style={{ filter: `drop-shadow(0 0 2px ${color})` }}
      />
    </svg>
  );
}

/** A small plain trend line for table rows (the workspaces table's spend trend). */
export function TrendLine({ values }: { values: (number | null)[] }): ReactElement {
  const v = values.map((x) => (x == null || !Number.isFinite(x) ? 0 : x));
  const mn = Math.min(...v, 0);
  const mx = Math.max(...v, 0);
  const span = mx - mn || 1;
  const pts = v.map((x, i) => `${((i / Math.max(1, v.length - 1)) * 100).toFixed(1)},${(19 - ((x - mn) / span) * 18).toFixed(1)}`).join(" ");
  return (
    <svg viewBox="0 0 100 20" preserveAspectRatio="none" aria-hidden="true" className="block h-5 w-full">
      <polyline points={pts} fill="none" stroke="var(--muted)" strokeWidth={1.25} strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

// --- The stacked smoothed-area chart -------------------------------------------------------

export interface AreaSeries {
  label: string;
  color: string;
  values: number[];
}

/** The largest "nice" step (1, 2, 2.5, 5 × 10ⁿ) at or above x — four of them make the y axis. */
export function niceStep(x: number): number {
  if (!(x > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(x));
  for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= x) return m * p;
  return 10 * p;
}

/** Stacked area geometry, in viewBox units: per series its top line and closed band, plus the
 *  column tops the hover dots sit on. Exported for the tests. */
export function stackAreas(series: AreaSeries[], n: number, max: number): Array<{ line: string; area: string; tops: ChartPoint[] }> {
  const X = (i: number) => ((i + 0.5) / n) * 100;
  const Y = (v: number) => Math.max(0, Math.min(100, 100 - (v / max) * 100));
  const acc = Array<number>(n).fill(0);
  return series.map((s) => {
    const bottom = acc.map((v, i) => ({ x: X(i), y: Y(v) }));
    for (let i = 0; i < n; i++) acc[i] += s.values[i] ?? 0;
    const tops = acc.map((v, i) => ({ x: X(i), y: Y(v) }));
    const line = monotonePath(tops);
    const back = monotonePath([...bottom].reverse()).replace(/^M/, "L");
    return { line, area: `${line} ${back} Z`, tops };
  });
}

/** Glides a path between data pushes where the browser can (Chromium/Firefox interpolate the
 *  CSS `d` property); elsewhere the attribute simply swaps. */
const glide = (d: string): CSSProperties => ({ d: `path("${d}")`, transition: "d 700ms cubic-bezier(.2,.8,.2,1)" }) as CSSProperties;

export function AreaChart({
  series,
  labels,
  xLabels,
  ghost,
  format,
  formatY,
  readTitle,
  emptyNote = "no data in range",
  highlight = null,
}: {
  series: AreaSeries[];
  /** One label per column for the tooltip and read-out (e.g. "Sep 3"). */
  labels: string[];
  /** Axis labels, one per column; drawn sparsely (~6). Defaults to `labels`. */
  xLabels?: string[];
  /** Previous-period daily totals, aligned by index: the dashed ghost line. */
  ghost?: (number | null)[];
  format: (v: number) => string;
  formatY?: (v: number) => string;
  /** The read-out's title while nothing is hovered ("Last 30 days"). */
  readTitle: string;
  emptyNote?: string;
  /** A series label to bring forward (the side row under the pointer); the rest step back. */
  highlight?: string | null;
}): ReactElement {
  const uid = useId().replace(/:/g, "");
  const dim = (label: string) => highlight != null && label !== highlight;
  const [hover, setHover] = useState<number | null>(null);
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const totals = Array.from({ length: n }, (_, i) => series.reduce((t, s) => t + (s.values[i] ?? 0), 0));
  const top = Math.max(0, ...totals, ...(ghost ?? []).map((v) => v ?? 0));
  if (n === 0 || top <= 0) {
    return <div className="py-10 text-center text-[12.5px] text-faint">{emptyNote}</div>;
  }
  const step = niceStep(top / 4);
  const max = step * 4;
  const geo = stackAreas(series, n, max);
  const X = (i: number) => ((i + 0.5) / n) * 100;
  const hv = hover != null && hover < n ? hover : null;
  const fmtY = formatY ?? ((v: number) => String(Math.round(v)));
  const every = Math.ceil(n / 6);
  const axis = xLabels ?? labels;
  const nowTop = geo.length > 0 ? geo[geo.length - 1].tops[n - 1] : null;
  const nowColor = series.length > 0 ? series[series.length - 1].color : "var(--chart-1)";
  const read =
    hv != null
      ? series.map((s) => ({ label: s.label, color: s.color, value: format(s.values[hv] ?? 0) }))
      : series.map((s) => ({ label: s.label, color: s.color, value: format(s.values.reduce((t, v) => t + (v ?? 0), 0)) }));
  if (hv != null && ghost) read.push({ label: "prev", color: "var(--muted)", value: ghost[hv] != null ? format(ghost[hv] as number) : "—" });
  const ghostPts = ghost
    ? ghost
        .map((v, i) => (v == null ? null : `${X(i).toFixed(1)},${(100 - (v / max) * 100).toFixed(1)}`))
        .filter((p): p is string => p !== null)
        .join(" ")
    : "";

  return (
    <div>
      <div className="flex min-h-5 flex-wrap items-baseline gap-x-4 gap-y-1 text-[13px] tabular-nums text-muted">
        <span className="font-medium text-text">{hv != null ? labels[hv] : readTitle}</span>
        {read.map((r) => (
          <span key={r.label} className="inline-flex items-center gap-1.5">
            <span aria-hidden="true" className="h-[7px] w-[7px] rounded-[2px]" style={{ background: r.color }} />
            {r.label} <span className="text-text">{r.value}</span>
          </span>
        ))}
      </div>
      <div className="mt-4 flex gap-2.5">
        <div aria-hidden="true" className="flex h-[200px] w-[34px] shrink-0 flex-col justify-between text-right font-mono text-[11px] leading-none text-faint">
          {[4, 3, 2, 1, 0].map((k) => (
            <span key={k}>{fmtY(step * k)}</span>
          ))}
        </div>
        <div className="min-w-0 flex-1">
          <div role="img" aria-label={`${series.map((s) => s.label).join(", ")} per day`} onMouseLeave={() => setHover(null)} className="dash-overlay relative h-[200px]">
            <div aria-hidden="true" className="absolute inset-x-0 top-0 border-t border-dashed border-border" />
            <div aria-hidden="true" className="absolute inset-x-0 top-1/2 border-t border-dashed border-border" />
            <div aria-hidden="true" className="absolute inset-x-0 bottom-0 border-t border-border" />
            {/* Re-keyed on the column count, so a range change replays the grow-in. */}
            <svg key={n} viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" className="v3-reveal absolute inset-0 h-full w-full overflow-visible">
              <defs>
                {series.map((s, si) => (
                  <linearGradient key={s.label} id={`${uid}-a${si}`} x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0" stopColor={s.color} stopOpacity={0.42} />
                    <stop offset="1" stopColor={s.color} stopOpacity={0.03} />
                  </linearGradient>
                ))}
              </defs>
              {geo.map((g, si) => (
                <path
                  key={`a${si}`}
                  d={g.area}
                  fill={`url(#${uid}-a${si})`}
                  style={{ ...glide(g.area), opacity: dim(series[si].label) ? 0.15 : 1, transition: "opacity 200ms ease" }}
                />
              ))}
              {geo.map((g, si) => (
                <path
                  key={`l${si}`}
                  d={g.line}
                  fill="none"
                  stroke={series[si].color}
                  strokeWidth={highlight === series[si].label ? 2.75 : 1.75}
                  strokeLinejoin="round"
                  strokeLinecap="round"
                  vectorEffect="non-scaling-stroke"
                  style={{ ...glide(g.line), opacity: dim(series[si].label) ? 0.25 : 1, transition: "opacity 200ms ease" }}
                />
              ))}
            </svg>
            {ghostPts && (
              <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" className="v3-fade-late pointer-events-none absolute inset-0 h-full w-full overflow-visible">
                <polyline points={ghostPts} fill="none" stroke="var(--faint)" strokeWidth={1.25} strokeDasharray="3 3" vectorEffect="non-scaling-stroke">
                  <title>previous period daily total</title>
                </polyline>
              </svg>
            )}
            <div className="absolute inset-0 flex">
              {Array.from({ length: n }, (_, i) => (
                <div
                  key={i}
                  tabIndex={0}
                  aria-label={`${labels[i] ?? ""}: ${series.map((s) => `${s.label} ${format(s.values[i] ?? 0)}`).join(", ")}`}
                  onMouseEnter={() => setHover(i)}
                  onFocus={() => setHover(i)}
                  onBlur={() => setHover(null)}
                  className="h-full flex-1 focus-visible:outline-none"
                />
              ))}
            </div>
            {nowTop && hv == null && (
              <span
                aria-hidden="true"
                className="v3-now-dot pointer-events-none absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full transition-[top] duration-700"
                style={{ left: `${nowTop.x}%`, top: `${nowTop.y}%`, background: nowColor }}
              />
            )}
            {hv != null && (
              <>
                <div
                  aria-hidden="true"
                  className="pointer-events-none absolute inset-y-0 w-px bg-[linear-gradient(to_bottom,transparent,var(--border-strong)_20%,var(--border-strong))]"
                  style={{ left: `${X(hv)}%` }}
                />
                {geo.map((g, si) => (
                  <span
                    key={si}
                    aria-hidden="true"
                    className="pointer-events-none absolute box-border h-[9px] w-[9px] -translate-x-1/2 -translate-y-1/2 rounded-full border-2 bg-bg"
                    style={{ left: `${X(hv)}%`, top: `${g.tops[hv].y}%`, borderColor: series[si].color }}
                  />
                ))}
                <div
                  className="v3-pop pointer-events-none absolute top-0 z-[5] min-w-[160px] rounded-[10px] border border-border-strong px-[11px] py-[9px] text-[12.5px] shadow-[0_10px_30px_rgb(0_0_0/0.3)]"
                  style={{ left: `${X(hv)}%`, transform: X(hv) > 62 ? "translateX(calc(-100% - 14px))" : "translateX(14px)" }}
                >
                  <div className="mb-1.5 font-medium">{labels[hv]}</div>
                  {[...series.map((s) => ({ label: s.label, color: s.color, value: format(s.values[hv] ?? 0) })), ...(ghost ? [{ label: "previous", color: "var(--faint)", value: ghost[hv] != null ? format(ghost[hv] as number) : "—" }] : [])]
                    .reverse()
                    .map((r) => (
                      <div key={r.label} className="flex items-center gap-2 py-0.5">
                        <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full" style={{ background: r.color }} />
                        <span className="flex-1 whitespace-nowrap text-muted">{r.label}</span>
                        <span className="tabular-nums">{r.value}</span>
                      </div>
                    ))}
                </div>
              </>
            )}
          </div>
          <div aria-hidden="true" className="relative mt-2 h-[18px] font-mono text-[11px] text-faint">
            {axis.map((label, i) =>
              i % every === 0 ? (
                <span key={i} className="absolute -translate-x-1/2 whitespace-nowrap" style={{ left: `${X(i)}%` }}>
                  {label}
                </span>
              ) : null,
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** One row of a chart's side column: label, value + note, and a 4px share bar. */
export interface SideRow {
  label: string;
  value: ReactNode;
  note?: string;
  /** Bar width, 0..100. */
  share: number;
  color: string;
  title?: string;
  onClick?: () => void;
  /** A mark before the label, e.g. the org's logo. */
  icon?: ReactNode;
  /** A hover / focus card for the row: who this is and what the number means. */
  card?: ReactNode;
}

/** A chart section: the area chart on the left, its side column (share, funnel, mix) on the
 *  right, divided by a hairline, all between the section's two rules. */
export function ChartSection({
  title,
  legend,
  chart,
  foot,
  sideTitle,
  side,
  sideFoot,
  sideLimit,
}: {
  title: string;
  legend?: ReactNode;
  /** The chart, or a function of the side row under the pointer so the chart can bring it forward. */
  chart: ReactNode | ((hot: string | null) => ReactNode);
  foot?: ReactNode;
  sideTitle: string;
  side: SideRow[];
  sideFoot?: ReactNode;
  /** Show this many side rows until "Show all" is pressed. */
  sideLimit?: number;
}): ReactElement {
  const [hot, setHot] = useState<string | null>(null);
  const [all, setAll] = useState(false);
  const limited = sideLimit != null && side.length > sideLimit;
  const rows = limited && !all ? side.slice(0, sideLimit) : side;
  return (
    <Section title={title} right={legend}>
      <Rules className="flex flex-wrap">
        <div className="min-w-0 flex-[2_1_460px] py-5 pr-6">
          {typeof chart === "function" ? chart(hot) : chart}
          {foot && <div className="mt-3 text-[12.5px] text-faint">{foot}</div>}
        </div>
        <div className="flex min-w-0 flex-[1_1_240px] flex-col gap-0.5 py-5 pl-6 shadow-[-1px_0_0_var(--border)]">
          <div className="mb-2 text-[13px] text-muted">{sideTitle}</div>
          {rows.map((row) => {
            const body = (
              <>
                <span className="flex w-full items-center justify-between gap-3">
                  <span className="flex min-w-0 items-center gap-2">
                    {row.icon}
                    <span className="min-w-0 truncate text-[13.5px]">{row.label}</span>
                  </span>
                  <span className="whitespace-nowrap text-[13px] tabular-nums">
                    {row.value} {row.note && <span className="text-faint">{row.note}</span>}
                  </span>
                </span>
                <span className="block h-1 w-full overflow-hidden rounded-sm bg-panel-3">
                  <span className="block h-full rounded-sm transition-[width] duration-700" style={{ width: `${Math.max(0, Math.min(100, row.share))}%`, background: row.color }} />
                </span>
              </>
            );
            const isHot = hot === row.label;
            const hover = {
              onMouseEnter: () => setHot(row.label),
              onMouseLeave: () => setHot((h) => (h === row.label ? null : h)),
              onFocus: () => setHot(row.label),
              onBlur: () => setHot((h) => (h === row.label ? null : h)),
            };
            const card = row.card && isHot && (
              <div
                role="tooltip"
                className="v3-pop pointer-events-none absolute right-[calc(100%+12px)] top-1/2 z-20 w-[260px] -translate-y-1/2 animate-[ck-in_140ms_ease-out_both] rounded-xl border border-border-strong p-3 text-left shadow-[0_16px_48px_rgb(0_0_0/0.35)]"
              >
                {row.card}
              </div>
            );
            const rowClass = `relative flex flex-col gap-2 rounded-md px-1.5 py-2.5 -mx-1.5 transition-[background-color,opacity] ${hot != null && !isHot ? "opacity-55" : ""} ${isHot ? "bg-panel-2" : ""}`;
            return row.onClick ? (
              <button
                key={row.label}
                type="button"
                title={row.card ? undefined : row.title}
                aria-label={row.title ? `${row.label}: ${row.title}` : undefined}
                onClick={row.onClick}
                {...hover}
                className={`${rowClass} cursor-pointer border-0 bg-transparent text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent`}
              >
                {body}
                {card}
              </button>
            ) : (
              <div key={row.label} title={row.card ? undefined : row.title} tabIndex={row.card ? 0 : undefined} {...hover} className={rowClass}>
                {body}
                {card}
              </div>
            );
          })}
          {(limited || sideFoot) && (
            <div className="mt-auto flex flex-wrap items-center justify-between gap-2 pt-3 text-[12.5px] text-faint">
              {limited && (
                <button
                  type="button"
                  aria-expanded={all}
                  onClick={() => setAll((a) => !a)}
                  className="cursor-pointer rounded-md border border-border bg-transparent px-2.5 py-1 text-[12.5px] text-muted hover:border-border-strong hover:text-text"
                >
                  {all ? "Show less" : `Show all ${side.length} workspaces`}
                </button>
              )}
              {sideFoot && <span>{sideFoot}</span>}
            </div>
          )}
        </div>
      </Rules>
    </Section>
  );
}

/** One horizontal stacked strip (a model mix): segments share the bar by value, each with a hover title. */
export function ShareBar({
  segments,
  format,
  label,
}: {
  segments: Array<{ label: string; color: string; value: number }>;
  format: (value: number) => string;
  label: string;
}): ReactElement {
  const total = segments.reduce((t, s) => t + s.value, 0);
  if (!(total > 0)) {
    return <div className="py-2 text-center text-[12.5px] text-faint">no data in range</div>;
  }
  return (
    <div className="flex h-1 overflow-hidden rounded-sm bg-panel-3" role="img" aria-label={label}>
      <div key={segments.map((s) => s.value).join(",")} className="dash-grow-x flex h-full w-full">
        {segments.map((s) =>
          s.value > 0 ? (
            <div key={s.label} className="h-full" style={{ width: `${(s.value / total) * 100}%`, background: s.color }}>
              <title>{`${s.label}: ${format(s.value)}`}</title>
            </div>
          ) : null,
        )}
      </div>
    </div>
  );
}

/** A small status note ("queued · stalled"): tone-coloured text, no pill. */
export function StatusChip({ tone = "neutral", children }: { tone?: Tone; children: ReactNode }): ReactElement {
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap text-[12.5px]" style={{ color: TONE_COLOR[tone] }}>
      {children}
    </span>
  );
}

const TONE_COLOR: Record<Tone, string> = {
  neutral: "var(--muted)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

/** One segment of a SegTabs control. */
export interface SegTab {
  key: string;
  label: string;
  count?: number;
  active: boolean;
  onClick: () => void;
  /** The count speaks up in warn (needs you, a stalled queue). */
  urgent?: boolean;
  title?: string;
}

/** The v3 segmented control: a hairline box with the active segment filled. */
export function SegTabs({ items, label }: { items: SegTab[]; label: string }): ReactElement {
  return (
    <div role="group" aria-label={label} className="flex flex-wrap gap-0.5 rounded-lg border border-border p-0.5">
      {items.map((t) => (
        <button
          key={t.key}
          type="button"
          aria-pressed={t.active}
          title={t.title}
          onClick={t.onClick}
          className={`cursor-pointer whitespace-nowrap rounded-md border-0 px-2.5 py-1 text-[12.5px] ${t.active ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text"}`}
        >
          {t.label}
          {t.count != null && <span className={t.urgent && t.count > 0 ? "text-warn" : "text-faint"}> {t.count}</span>}
        </button>
      ))}
    </div>
  );
}

/** A rounded pill tab (the org dashboard's repository filter). */
export function PillTab({ active, label, count, title, onClick }: { active: boolean; label: string; count: number; title?: string; onClick: () => void }): ReactElement {
  return (
    <button
      type="button"
      aria-pressed={active}
      title={title}
      onClick={onClick}
      className={`cursor-pointer whitespace-nowrap rounded-full border px-3 py-[5px] text-[13px] ${active ? "border-text bg-panel-3 text-text" : "border-border bg-transparent text-muted hover:text-text"}`}
    >
      {label} <span className="text-faint">{count}</span>
    </button>
  );
}

/** The dashboard toolbar: the 7d/30d/90d segmented range plus the Compare switch. The caller owns the state. */
export function RangePicker({
  range,
  onRange,
  compare,
  onCompare,
}: {
  range: RangeDays;
  onRange: (range: RangeDays) => void;
  compare: boolean;
  onCompare: () => void;
}): ReactElement {
  return (
    <div className="flex items-center gap-2">
      <div className="flex rounded-lg border border-border p-0.5" role="group" aria-label="Range">
        {RANGES.map((v) => (
          <button
            key={v}
            type="button"
            aria-pressed={range === v}
            onClick={() => onRange(v)}
            className={`cursor-pointer rounded-md border-0 px-3 py-[5px] text-[13px] ${range === v ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text"}`}
          >
            {v}d
          </button>
        ))}
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={compare}
        title={`compare to the previous ${range}d`}
        onClick={onCompare}
        className={`flex cursor-pointer items-center gap-2 rounded-lg border border-border bg-transparent px-3 py-[7px] text-[13px] hover:border-border-strong ${compare ? "text-text" : "text-muted"}`}
      >
        <Switch on={compare} />
        Compare
      </button>
    </div>
  );
}

/** The v3 switch: a small track with the knob at the end it points to. */
export function Switch({ on }: { on: boolean }): ReactElement {
  return (
    <span
      aria-hidden="true"
      className={`box-border flex h-4 w-[26px] rounded-lg p-0.5 transition-colors duration-200 ${on ? "justify-end bg-text" : "justify-start bg-border-strong"}`}
    >
      <span className="h-3 w-3 rounded-full bg-bg" />
    </span>
  );
}

/** The live dot a colony row leads with: status-toned, breathing while the colony's microVM is up. */
export function StatusDot({ session }: { session: Session }): ReactElement {
  const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
  const live = session.status === "running" || session.status === "starting";
  return (
    <span
      aria-hidden="true"
      className={`h-[7px] w-[7px] rounded-full transition-colors duration-500 ${live ? "v3-live-dot" : ""}`}
      style={{ background: TONE_COLOR[tone] }}
    />
  );
}

/** The colonies table's grid, shared by the header row and every row. */
export const COLONY_GRID = "grid grid-cols-[10px_minmax(0,1fr)_minmax(0,120px)_130px_72px_64px] items-center gap-3.5";

/** One colony row: dot, title + repo#issue, org, status, age, spend. Flashes when its status just
 *  moved and lights its cost when it just rose (liveEvents). */
export function ColonyRow({
  session,
  age,
  flashed,
  bumped,
  onOpen,
  showOrg = true,
}: {
  session: Session;
  age: string;
  flashed: boolean;
  bumped: boolean;
  onOpen?: (id: string) => void;
  showOrg?: boolean;
}): ReactElement {
  const meta = SESSION_STATUS[session.status] ?? { label: session.status, tone: "neutral" as Tone };
  const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
  return (
    <div
      className={`${COLONY_GRID} -mt-px border-t border-border py-3 transition-colors duration-[1200ms] ${flashed ? "v3-flash" : ""}`}
      data-live={isLive(session.status) || undefined}
    >
      <StatusDot session={session} />
      <button
        type="button"
        onClick={() => onOpen?.(session.id)}
        title={session.issue_title || short}
        className="min-w-0 cursor-pointer truncate border-0 bg-transparent p-0 text-left text-[13.5px] text-text hover:opacity-80"
      >
        {session.issue_title || short} <span className="font-mono text-[12px] text-faint">{short}</span>
      </button>
      <span className="min-w-0 truncate text-[13px] text-faint">{showOrg ? orgOf(session) : ""}</span>
      <span className="truncate text-[13px]" style={{ color: TONE_COLOR[meta.tone] }}>
        {meta.label}
      </span>
      <span className="text-right text-[12.5px] text-faint">{age}</span>
      <span className={`text-right text-[13px] tabular-nums transition-colors duration-700 ${bumped ? "text-accent" : "text-text"}`}>
        <LiveCost value={sessionCost(session)} />
      </span>
    </div>
  );
}

/** An org's avatar when /api/orgs knows one, else its initial on its deterministic colour —
 *  the same tile everywhere (cards, queue, table, legends, chips), so the hue fallback reads as
 *  one identity. A broken image falls back to the lettermark too, via Avatar's fallback. */
export function OrgTile({ org, avatar, size = 22 }: { org: string; avatar?: string | null; size?: number }): ReactElement {
  const radius = "50%";
  return (
    <Avatar
      name={org}
      src={avatar ?? undefined}
      size={size}
      rounded="full"
      fallback={
        <span
          aria-hidden="true"
          className="grid shrink-0 select-none place-items-center font-mono font-medium"
          style={{ width: size, height: size, borderRadius: radius, fontSize: Math.max(9, Math.round(size * 0.4)), background: orgColorFor(org), color: "var(--term-bg)" }}
        >
          {initialOf(org)}
        </span>
      }
    />
  );
}
