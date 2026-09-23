// Shared dashboard primitives for the cockpit (issue #398, next-gen pass issue #445): card shapes,
// KPI tiles, stacked bars, line charts, share bars, chips and the range picker, all following the
// Claude Design "Cockpit Dashboards" reference (docs/design/cockpit-dashboards/). Single-series
// charts draw in accent-orange shades, multi-series in the --chart-1..5 ramp (index.css); the
// caller picks the colour, so semantic series (failed outcomes, status strips) keep theirs.
// Rendering is a hybrid that never distorts: the SVG (viewBox 0 0 100 100,
// preserveAspectRatio="none") carries only lines, areas and gridlines — strokes hold their width
// via vector-effect — while dots, bars, crosshair and tooltips are HTML overlays positioned in %,
// so circles stay circular at any width. No measuring, no ResizeObserver: static markup renders
// the full geometry, which is also what the renderToStaticMarkup tests assert on.
import { useId, type ReactElement, type ReactNode } from "react";

import { chartRuns, chartY, joinedPoints, monotoneArea, monotonePath, RANGES, type ChartPoint, type RangeDays } from "./dash";
import { TweenedValue } from "./Live";

export interface BarSeries {
  label: string;
  color: string;
  values: number[];
}

/** The design's mono eyebrow: 10.5px, tracked out, uppercase by convention — pass ALREADY-UPPER text. */
export function Eyebrow({ children }: { children: ReactNode }): ReactElement {
  return <div className="font-mono text-[10.5px] tracking-[0.12em] text-faint">{children}</div>;
}

/** Right-side legend for a panel header: one colour dot per entry, or the caller's icon —
 *  the overview passes each org's avatar tile so hue is never the only identity. */
export function DashLegend({ items }: { items: Array<{ label: string; color: string; icon?: ReactNode }> }): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      {items.map((item) => (
        <span key={item.label} className="inline-flex items-center gap-1.5 text-[11.5px] text-muted">
          {item.icon ?? <span aria-hidden="true" className="h-2 w-2 rounded-[2px]" style={{ background: item.color }} />}
          {item.label}
        </span>
      ))}
    </div>
  );
}

/** One dashboard card: eyebrow title, an optional sans subtitle, an optional right-side legend. */
export function DashPanel({
  title,
  sub,
  legend,
  children,
  className,
}: {
  title: string;
  sub?: string;
  legend?: ReactNode;
  children: ReactNode;
  className?: string;
}): ReactElement {
  return (
    <section className={`min-w-0 rounded-2xl border border-border bg-panel p-4 ${className ?? ""}`}>
      <div className="mb-2 flex flex-wrap items-start justify-between gap-x-3 gap-y-1">
        <div className="min-w-0">
          <Eyebrow>{title}</Eyebrow>
          {sub && <div className="mt-1 text-[13px] text-muted">{sub}</div>}
        </div>
        {legend}
      </div>
      {children}
    </section>
  );
}

/** One KPI tile: value, previous-period delta and a sparkline area. Shared by the overview
 *  strip and the org dashboard so the two cannot drift apart. */
export interface KpiDef {
  label: string;
  value: string;
  /** Numeric twin of `value`: when present the tile tweens between pushes (issue #446) instead
   *  of snapping. Static markup shows `value`, so keep the two in agreement. */
  valueNum?: number;
  /** Formats the tweened number; defaults to rounding. */
  formatNum?: (n: number) => string;
  /** Absent while the compare toggle is off or a snapshot has no previous period. */
  delta?: string;
  /** Colours the delta (good → ok, bad → err, flat/absent → faint); pair with deltaTone(). */
  deltaTone?: "good" | "bad" | "flat";
  /** Renders a ▲/▼ glyph before the delta; the sign itself stays in the delta string. */
  deltaDir?: "up" | "down";
  spark?: string;
  /** Sparkline stroke: accent when the tone is good, err when bad, faint with no tone. */
  sparkColor?: string;
  sub?: string;
  /** Honest empty state: renders "—" plus this mono line (e.g. "no data source yet") in the same
   *  card shape, instead of value/spark/delta. */
  emptyNote?: string;
  hint: string;
}

const DELTA_CLASS = { good: "text-ok", bad: "text-err", flat: "text-faint" } as const;

export function KpiTile({ label, value, valueNum, formatNum, delta, deltaTone, deltaDir, spark, sparkColor, sub, emptyNote, hint }: KpiDef): ReactElement {
  if (emptyNote != null) {
    return (
      <div title={hint} className="flex min-w-0 flex-col gap-1.5 rounded-[14px] border border-border bg-panel px-3.5 py-3">
        <div className="truncate font-mono text-[10.5px] tracking-[0.12em] text-faint">{label}</div>
        <div className="text-2xl font-semibold tabular-nums">—</div>
        <div className="font-mono text-[11px] text-faint">{emptyNote}</div>
      </div>
    );
  }
  const stroke = sparkColor ?? (deltaTone === "bad" ? "var(--err)" : deltaTone === "good" ? "var(--accent)" : "var(--faint)");
  return (
    <div title={hint} className="flex min-w-0 flex-col gap-1.5 rounded-[14px] border border-border bg-panel px-3.5 py-3">
      <div className="truncate font-mono text-[10.5px] tracking-[0.12em] text-faint">{label}</div>
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums tracking-tight">
          {valueNum != null ? <TweenedValue value={valueNum} format={formatNum} /> : value}
        </span>
        {delta && (
          <span className={`font-mono text-[11px] ${DELTA_CLASS[deltaTone ?? "flat"]}`}>
            {deltaDir === "up" ? "▲ " : deltaDir === "down" ? "▼ " : ""}
            {delta}
          </span>
        )}
      </div>
      {spark ? <Sparkline points={spark} color={stroke} /> : null}
      {sub && <div className="font-mono text-[11px] text-faint">{sub}</div>}
    </div>
  );
}

/** Back from the "x,y x,y …" spark string (the 100×28 box sparkPoints emits) to points. */
function parseSpark(points: string): ChartPoint[] {
  return points
    .split(" ")
    .map((pt) => pt.split(",").map(Number))
    .filter((pair): pair is [number, number] => pair.length === 2 && pair.every((v) => Number.isFinite(v)))
    .map(([x, y]) => ({ x, y }));
}

/** One compact sparkline with the full next-gen look: gradient area, smoothed glowing line and
 *  a pulsing last dot. The SVG keeps the historic 100×28 box and carries only the line and
 *  area; the dot is an HTML overlay so it stays circular. Shared by KpiTile and the overview's
 *  per-workspace card so the two cannot drift apart. */
export function Sparkline({ points, color }: { points: string; color: string }): ReactElement | null {
  const gid = `spark-${useId().replace(/:/g, "")}`;
  const pts = parseSpark(points);
  if (pts.length === 0) return null;
  const last = pts[pts.length - 1];
  return (
    <div className="relative block h-7 w-full">
      <svg viewBox="0 0 100 28" preserveAspectRatio="none" aria-hidden="true" className="absolute inset-0 block h-full w-full">
        <defs>
          <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={color} stopOpacity={0.35} />
            <stop offset="100%" stopColor={color} stopOpacity={0} />
          </linearGradient>
        </defs>
        {pts.length > 1 && <path d={monotoneArea(pts, 28)} fill={`url(#${gid})`} />}
        {pts.length > 1 ? (
          <path
            key={points}
            d={monotonePath(pts)}
            fill="none"
            stroke={color}
            strokeWidth={1.5}
            strokeLinecap="round"
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
            pathLength={1}
            strokeDasharray={1}
            className="dash-draw"
            style={{ filter: `drop-shadow(0 1px 3px color-mix(in srgb, ${color} 60%, transparent))` }}
          />
        ) : null}
      </svg>
      <span
        aria-hidden="true"
        className="dash-pulse absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full"
        style={{ left: `${last.x}%`, top: `${(last.y / 28) * 100}%`, background: color, border: "1.5px solid var(--panel)" }}
      />
    </div>
  );
}

/** Dashed gridlines shared by the bar and line charts (the design draws dashed rules). */
function Gridlines(): ReactElement {
  return (
    <g>
      {[75, 50, 25, 0].map((y) => (
        <line key={y} x1={0} x2={100} y1={y} y2={y} stroke="var(--border)" strokeWidth={0.35} strokeDasharray="1.2 1.6" vectorEffect="non-scaling-stroke" />
      ))}
    </g>
  );
}

/** Chart frame: an optional mono y-axis gutter beside a responsive plot. The plot is a fixed
 *  height — 170px on narrow screens, 240px at desktop widths — with an SVG underlay (gridlines,
 *  areas, lines) and an HTML overlay (bars, dots, crosshair, tooltips) as siblings, so nothing
 *  drawn in screen space ever stretches. Sparse x labels pin to their column centres below. */
function ChartFrame({
  max,
  formatY,
  xLabels,
  n,
  labelledBy,
  overlay,
  children,
}: {
  max: number;
  formatY?: (value: number) => string;
  xLabels?: string[];
  n: number;
  labelledBy: string;
  /** HTML layer: bars, dots, hover zones — positioned in %, immune to SVG stretching. */
  overlay?: ReactNode;
  /** SVG layer: gridlines, gradient areas, smoothed lines. */
  children: ReactNode;
}): ReactElement {
  const step = Math.max(1, Math.ceil(n / 7));
  return (
    <div>
      <div className="flex gap-1.5">
        {formatY && (
          <div aria-hidden="true" className="flex w-9 shrink-0 flex-col justify-between py-0.5 text-right font-mono text-[10px] leading-none text-faint">
            {[1, 0.75, 0.5, 0.25, 0].map((f) => (
              <span key={f}>{formatY(max * f)}</span>
            ))}
          </div>
        )}
        <div className="min-w-0 flex-1">
          <div role="img" aria-label={labelledBy} className="dash-chart relative h-[170px] w-full md:h-[240px]">
            <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" className="absolute inset-0 block h-full w-full">
              {children}
            </svg>
            {overlay}
          </div>
          {xLabels && (
            <div aria-hidden="true" className="relative mt-1 h-3.5 font-mono text-[10px] text-faint">
              {xLabels.map((label, i) =>
                i % step === 0 ? (
                  <span key={i} className="absolute -translate-x-1/2 whitespace-nowrap" style={{ left: `${((i + 0.5) / n) * 100}%` }}>
                    {label}
                  </span>
                ) : null,
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/** Ghost overlay: the previous period's daily totals as a dashed faint line, joining across
 *  unmeasured days (the old polyline semantics) and smoothed like the real series. A lone
 *  point draws as a bare moveto, so callers dot it in the HTML overlay via GhostLoneDot. */
function Ghost({ ghost, max }: { ghost?: (number | null)[]; max: number }): ReactElement | null {
  if (!ghost || !ghost.some((v) => v != null && v > 0)) return null;
  const pts = joinedPoints(ghost, max);
  if (pts.length === 0) return null;
  return (
    <path
      d={monotonePath(pts)}
      fill="none"
      stroke="var(--faint)"
      strokeWidth={1.5}
      strokeDasharray="2.2 1.8"
      strokeLinecap="round"
      vectorEffect="non-scaling-stroke"
      opacity={0.8}
    >
      <title>previous period daily total</title>
    </path>
  );
}

/** The HTML dot for a one-point ghost: an SVG circle would stretch into an ellipse, and a bare
 *  moveto draws nothing — so the overlay dots it. Multi-point ghosts need nothing. */
function GhostLoneDot({ ghost, max }: { ghost?: (number | null)[]; max: number }): ReactElement | null {
  if (!ghost) return null;
  const pts = joinedPoints(ghost, max);
  if (pts.length !== 1) return null;
  return (
    <span
      aria-hidden="true"
      className="absolute h-1.5 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full"
      style={{ left: `${pts[0].x}%`, top: `${pts[0].y}%`, background: "var(--faint)" }}
    />
  );
}

export function DashBars({
  series,
  labels,
  ghost,
  format,
  formatY,
  xLabels,
}: {
  series: BarSeries[];
  /** One x label per day (the "YYYY-MM-DD"), used in the hover titles. */
  labels: string[];
  /** Previous-period daily totals, aligned by index; null = gap. */
  ghost?: (number | null)[];
  format: (value: number) => string;
  /** Renders the mono y-axis gutter (5 ticks, max..0); absent leaves just the gridlines. */
  formatY?: (value: number) => string;
  /** One date label per column, drawn sparsely (~7) under the chart. */
  xLabels?: string[];
}): ReactElement {
  const n = Math.max(0, ...series.map((s) => s.values.length));
  // One pass: each column's total plus its topmost segment (the last series with a value there),
  // which takes the rounded cap.
  const totals: number[] = Array(n).fill(0);
  const topOf: number[] = Array(n).fill(-1);
  series.forEach((s, si) => {
    s.values.forEach((v, i) => {
      if ((v ?? 0) > 0) {
        totals[i] += v as number;
        topOf[i] = si;
      }
    });
  });
  const max = Math.max(...totals, ...(ghost ?? []).map((v) => v ?? 0), 0);
  if (n === 0 || max <= 0) {
    return <div className="py-6 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  // The grow-in replays on mount; later data changes glide through the height transition instead
  // of a whole-stack remount.
  return (
    <ChartFrame
      max={max}
      formatY={formatY}
      xLabels={xLabels}
      n={n}
      labelledBy="stacked bars per day"
      overlay={
        <div className="dash-grow-y dash-overlay absolute inset-0">
          <GhostLoneDot ghost={ghost} max={max} />
          <div className="flex h-full w-full items-stretch">
            {totals.map((total, i) => {
              const parts = series
                .map((s) => ({ s, v: s.values[i] ?? 0 }))
                .filter(({ v }) => v > 0)
                .map(({ s, v }) => `${s.label}: ${format(v)}`);
              return (
              <div
                key={i}
                tabIndex={0}
                title={[labels[i] ?? "", ...parts].filter((p) => p !== "").join(" · ")}
                className="group relative h-full min-w-0 flex-1"
              >
                <div className="absolute bottom-0 flex flex-col justify-end" style={{ left: "20%", right: "20%", minWidth: 2, height: `${(total / max) * 100}%` }}>
                  {series.map((s, si) => {
                    const v = s.values[i] ?? 0;
                    if (!(v > 0)) return null;
                    return (
                      <div
                        key={s.label}
                        className={`w-full transition-[height] duration-500 ${si === topOf[i] ? "rounded-t-[4px]" : ""}`}
                        style={{
                          height: `${(v / total) * 100}%`,
                          minHeight: 2,
                          background: `linear-gradient(to top, ${s.color}, color-mix(in srgb, ${s.color} 45%, white))`,
                        }}
                      />
                    );
                  })}
                </div>
                <div
                  className={`pointer-events-none absolute bottom-full z-10 mb-1.5 whitespace-nowrap rounded-lg border border-border bg-panel px-2 py-1 font-mono text-[10.5px] opacity-0 shadow transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 ${
                    i / n > 0.7 ? "right-0" : i / n < 0.3 ? "left-0" : "left-1/2 -translate-x-1/2"
                  }`}
                >
                  <div className="font-semibold text-text">{labels[i] ?? ""}</div>
                  {series.map((s) => {
                    const v = s.values[i] ?? 0;
                    if (!(v > 0)) return null;
                    return (
                      <div key={s.label} className="flex items-center gap-1.5 text-muted">
                        <span aria-hidden="true" className="h-1.5 w-1.5 rounded-[2px]" style={{ background: s.color }} />
                        {s.label}: <span className="text-text">{format(v)}</span>
                      </div>
                    );
                  })}
                </div>
              </div>
              );
            })}
          </div>
        </div>
      }
    >
      <Gridlines />
      <Ghost ghost={ghost} max={max} />
    </ChartFrame>
  );
}

export interface LineSeries {
  label: string;
  color: string;
  /** null = unmeasured gap; the line breaks rather than zeroing through it. */
  values: (number | null)[];
  /** Previous-period or reference series: dashed, no area fill. */
  dashed?: boolean;
  /** Fills the area under the line at 0.12 opacity, like the design's latency bands. */
  fill?: boolean;
}

/** One multi-series line chart for p50/p95-style series: gradient bands, smoothed glowing
 *  lines, dashed reference series, a pulsing dot on each series' last point, and a hover
 *  crosshair with a tooltip naming the day and every measured value. Null days split the line
 *  into runs, exactly like the ghost. */
export function DashLine({
  series,
  labels,
  format,
  formatY,
  xLabels,
}: {
  series: LineSeries[];
  /** One x label per point (the "YYYY-MM-DD"), used in the hover titles. */
  labels: string[];
  format: (value: number) => string;
  /** Renders the mono y-axis gutter (5 ticks, max..0); absent leaves just the gridlines. */
  formatY?: (value: number) => string;
  /** One date label per point, drawn sparsely (~7) under the chart. */
  xLabels?: string[];
}): ReactElement {
  const uid = useId().replace(/:/g, "");
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const max = Math.max(0, ...series.flatMap((s) => s.values.map((v) => v ?? 0)));
  if (n === 0 || max <= 0) {
    return <div className="py-6 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  // Re-keying the lines on the data replays the draw-in; identical data keeps its key and stays put.
  const sig = series.map((s) => s.values.map((v) => (v == null ? "" : String(v))).join(",")).join("|");
  const runsBy = series.map((s) => chartRuns(s.values, max));
  return (
    <ChartFrame
      max={max}
      formatY={formatY}
      xLabels={xLabels}
      n={n}
      labelledBy="lines per day"
      overlay={
        <div className="dash-overlay absolute inset-0">
          {series.map((s, si) => {
            const runs = runsBy[si];
            if (runs.length === 0 || s.dashed) return null;
            const end = runs[runs.length - 1][runs[runs.length - 1].length - 1];
            return (
              <span
                key={s.label}
                aria-hidden="true"
                className="dash-pulse absolute h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full"
                style={{ left: `${end.x}%`, top: `${end.y}%`, background: s.color, border: "2px solid var(--panel)" }}
              />
            );
          })}
          {series.map((s, si) =>
            runsBy[si].flatMap((run, ri) => {
              // A one-point run draws as a bare moveto — invisible — so it gets a small dot. The
              // last point of a solid series already has the pulsing end dot, so it is skipped.
              if (run.length !== 1 || (!s.dashed && ri === runsBy[si].length - 1)) return [];
              const p = run[0];
              return (
                <span
                  key={`${s.label}-lone-${ri}`}
                  aria-hidden="true"
                  className="absolute h-1.5 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full"
                  style={{ left: `${p.x}%`, top: `${p.y}%`, background: s.color }}
                />
              );
            }),
          )}
          {Array.from({ length: n }, (_, i) => {
            const measured = series.filter((s) => {
              const v = s.values[i];
              return v != null && Number.isFinite(v);
            });
            if (measured.length === 0) return null;
            return (
              <div
                key={i}
                tabIndex={0}
                className="group absolute bottom-0 top-0"
                style={{ left: `${(i / n) * 100}%`, width: `${100 / n}%` }}
                title={`${labels[i] ?? ""} · ${measured.map((s) => `${s.label}: ${format(s.values[i] as number)}`).join(" · ")}`}
              >
                <div aria-hidden="true" className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-border-strong opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100" />
                {measured.map((s) => (
                  <span
                    key={s.label}
                    aria-hidden="true"
                    className="absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
                    style={{ left: "50%", top: `${chartY(s.values[i] as number, max)}%`, background: s.color, border: "1.5px solid var(--panel)" }}
                  />
                ))}
                <div
                  className={`pointer-events-none absolute top-2 z-10 whitespace-nowrap rounded-lg border border-border bg-panel px-2 py-1 font-mono text-[10.5px] opacity-0 shadow transition-opacity group-hover:opacity-100 group-focus-within:opacity-100 ${
                    i / n > 0.6 ? "right-1" : "left-1"
                  }`}
                >
                  <div className="font-semibold text-text">{labels[i] ?? ""}</div>
                  {measured.map((s) => (
                    <div key={s.label} className="flex items-center gap-1.5 text-muted">
                      <span aria-hidden="true" className="h-1.5 w-1.5 rounded-[2px]" style={{ background: s.color }} />
                      {s.label}: <span className="text-text">{format(s.values[i] as number)}</span>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      }
    >
      <Gridlines />
      <defs>
        {series.map((s, si) =>
          s.fill && !s.dashed ? (
            <linearGradient key={s.label} id={`${uid}-area-${si}`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={s.color} stopOpacity={0.35} />
              <stop offset="100%" stopColor={s.color} stopOpacity={0} />
            </linearGradient>
          ) : null,
        )}
      </defs>
      {series.map((s, si) => {
        const runs = runsBy[si];
        if (runs.length === 0) return null;
        return (
          <g key={s.label}>
            {s.fill &&
              !s.dashed &&
              runs.map((run, ri) => <path key={ri} d={monotoneArea(run)} fill={`url(#${uid}-area-${si})`} />)}
            {runs.map((run, ri) =>
              s.dashed ? (
                <path
                  key={ri}
                  d={monotonePath(run)}
                  fill="none"
                  stroke={s.color}
                  strokeWidth={1.5}
                  strokeDasharray="2.2 1.8"
                  strokeLinecap="round"
                  vectorEffect="non-scaling-stroke"
                  opacity={0.8}
                />
              ) : (
                <path
                  key={`${sig}-${ri}`}
                  d={monotonePath(run)}
                  fill="none"
                  stroke={s.color}
                  strokeWidth={2}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  vectorEffect="non-scaling-stroke"
                  pathLength={1}
                  strokeDasharray={1}
                  className="dash-draw"
                  style={{ filter: `drop-shadow(0 1px 5px color-mix(in srgb, ${s.color} 60%, transparent))` }}
                />
              ),
            )}
          </g>
        );
      })}
    </ChartFrame>
  );
}

/** One horizontal stacked strip (the org cards' live/returned strip, the model-mix strip):
 *  segments share the bar by value, each with a native hover title, a top-light gradient and a
 *  grow-in keyed on the data. */
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
    return <div className="py-2 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  return (
    <div className="flex h-2.5 overflow-hidden rounded-full bg-panel-3" role="img" aria-label={label}>
      <div key={segments.map((s) => s.value).join(",")} className="dash-grow-x flex h-full w-full">
        {segments.map((s) =>
          s.value > 0 ? (
            <div
              key={s.label}
              className="h-full"
              style={{ width: `${(s.value / total) * 100}%`, background: `linear-gradient(180deg, color-mix(in srgb, ${s.color} 72%, white), ${s.color})` }}
            >
              <title>{`${s.label}: ${format(s.value)}`}</title>
            </div>
          ) : null,
        )}
      </div>
    </div>
  );
}

/** A small status pill ("1 need you", "Queued · stalled"): tone-coloured text on a tone-tinted edge. */
export function StatusChip({ tone = "neutral", children }: { tone?: "neutral" | "info" | "ok" | "warn" | "err" | "accent"; children: ReactNode }): ReactElement {
  const color =
    tone === "neutral"
      ? "var(--muted)"
      : tone === "info"
        ? "var(--info)"
        : tone === "ok"
          ? "var(--ok)"
          : tone === "warn"
            ? "var(--warn)"
            : tone === "err"
              ? "var(--err)"
              : "var(--accent)";
  return (
    <span
      className="inline-flex items-center gap-1.5 whitespace-nowrap rounded-full border bg-panel px-2.5 py-0.5 font-mono text-[11px]"
      style={{ color, borderColor: `color-mix(in srgb, ${color} 45%, transparent)` }}
    >
      {children}
    </span>
  );
}

/** One toggleable filter pill with a count ("4 need you"): the overview counters and the colonies
 *  table's org filter share this shape. Static markup only — the caller owns the state. */
export function FilterChip({
  active,
  count,
  label,
  title,
  onClick,
}: {
  active: boolean;
  count: number;
  label: string;
  title?: string;
  onClick: () => void;
}): ReactElement {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      title={title}
      className={`cursor-pointer whitespace-nowrap rounded-full border px-2.5 py-1 text-[11.5px] font-medium ${
        active ? "border-accent bg-accent-soft text-text" : "border-border text-muted hover:border-accent hover:text-text"
      }`}
    >
      <span className={active ? "text-accent" : "text-muted"}>{count}</span> {label}
    </button>
  );
}

/** The shared dashboard toolbar: the 7d/30d/90d segmented range plus the "compare to previous"
 *  toggle (moved here from OrgDashboard so both screens share one). Client state lives with the
 *  caller; this only renders. */
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
    <div className="flex flex-wrap items-center gap-2">
      <div className="flex rounded-[10px] border border-border bg-panel p-[3px]" role="group" aria-label="Range">
        {RANGES.map((v) => (
          <button
            key={v}
            type="button"
            aria-pressed={range === v}
            onClick={() => onRange(v)}
            className={`cursor-pointer rounded-[7px] px-3 py-[5px] font-mono text-[11.5px] ${range === v ? "bg-accent-soft text-accent" : "text-muted hover:text-text"}`}
          >
            {v}d
          </button>
        ))}
      </div>
      <button
        type="button"
        aria-pressed={compare}
        onClick={onCompare}
        className={`flex cursor-pointer items-center gap-2 rounded-[10px] border px-3 py-[7px] text-xs ${
          compare ? "border-accent bg-accent-soft text-text" : "border-border bg-panel text-muted hover:text-text"
        }`}
      >
        <span aria-hidden="true" className="h-2 w-2 rounded-[2px]" style={{ background: "var(--accent)" }} />
        Compare to previous {range}d
      </button>
    </div>
  );
}
