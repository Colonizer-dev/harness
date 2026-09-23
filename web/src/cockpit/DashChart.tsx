// Shared dashboard primitives for the cockpit (issue #398): card shapes, KPI tiles, stacked bars,
// line charts, share bars, chips and the range picker, all following the Claude Design "Cockpit
// Dashboards" reference (docs/design/cockpit-dashboards/). Plain inline SVG with native <title>
// hovers — no library, no JS tooltips — using only index.css tokens so everything follows
// light/dark automatically. OverviewView and OrgDashboard compose these; they own no chart code.
import type { ReactElement, ReactNode } from "react";

import { RANGES, type RangeDays } from "./dash";

export interface BarSeries {
  label: string;
  color: string;
  values: number[];
}

/** The design's mono eyebrow: 10.5px, tracked out, uppercase by convention — pass ALREADY-UPPER text. */
export function Eyebrow({ children }: { children: ReactNode }): ReactElement {
  return <div className="font-mono text-[10.5px] tracking-[0.12em] text-faint">{children}</div>;
}

/** Right-side legend for a panel header: one colour dot per entry. */
export function DashLegend({ items }: { items: Array<{ label: string; color: string }> }): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      {items.map((item) => (
        <span key={item.label} className="inline-flex items-center gap-1.5 text-[11.5px] text-muted">
          <span aria-hidden="true" className="h-2 w-2 rounded-[2px]" style={{ background: item.color }} />
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

export function KpiTile({ label, value, delta, deltaTone, deltaDir, spark, sparkColor, sub, emptyNote, hint }: KpiDef): ReactElement {
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
        <span className="text-2xl font-semibold tabular-nums tracking-tight">{value}</span>
        {delta && (
          <span className={`font-mono text-[11px] ${DELTA_CLASS[deltaTone ?? "flat"]}`}>
            {deltaDir === "up" ? "▲ " : deltaDir === "down" ? "▼ " : ""}
            {delta}
          </span>
        )}
      </div>
      {spark ? (
        <svg viewBox="0 0 100 28" preserveAspectRatio="none" className="block h-7 w-full" aria-hidden="true">
          <polygon points={`0,28 ${spark} 100,28`} fill={stroke} opacity={0.12} />
          <polyline points={spark} fill="none" stroke={stroke} strokeWidth={1.5} strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
        </svg>
      ) : null}
      {sub && <div className="font-mono text-[11px] text-faint">{sub}</div>}
    </div>
  );
}

const H = 32;
const W = 100;

/** Dashed gridlines shared by the bar and line charts (the design draws dashed rules). */
function Gridlines(): ReactElement {
  return (
    <g>
      {[0.25, 0.5, 0.75, 1].map((f) => (
        <line key={f} x1={0} x2={W} y1={H * (1 - f)} y2={H * (1 - f)} stroke="var(--border)" strokeWidth={0.2} strokeDasharray="1 1.4" vectorEffect="non-scaling-stroke" />
      ))}
    </g>
  );
}

/** Chart frame: an optional mono y-axis gutter, the SVG, and sparse x labels pinned to their
 *  column centres. The gutter stretches to the SVG's height, so the 5 labels read max..0. */
function ChartFrame({
  max,
  formatY,
  xLabels,
  n,
  labelledBy,
  children,
}: {
  max: number;
  formatY?: (value: number) => string;
  xLabels?: string[];
  n: number;
  labelledBy: string;
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
          <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={labelledBy} className="block h-28 w-full">
            {children}
          </svg>
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

/** Points for a dashed ghost line on the same scale; null days are gaps the line skips. */
function ghostPoints(ghost: (number | null)[], max: number): string {
  const pts: string[] = [];
  ghost.forEach((v, i) => {
    if (v == null || max <= 0) return;
    pts.push(`${((i + 0.5) / ghost.length) * W},${(H - (v / max) * (H - 2)).toFixed(1)}`);
  });
  return pts.join(" ");
}

/** Ghost overlay: the previous period's daily totals as a dashed faint line. */
function Ghost({ ghost, max }: { ghost?: (number | null)[]; max: number }): ReactElement | null {
  if (!ghost || !ghost.some((v) => v != null && v > 0)) return null;
  return (
    <polyline
      points={ghostPoints(ghost, max)}
      fill="none"
      stroke="var(--faint)"
      strokeWidth={1.25}
      strokeDasharray="1.6 1.2"
      strokeLinecap="round"
      vectorEffect="non-scaling-stroke"
    >
      <title>previous period daily total</title>
    </polyline>
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
  const totals = Array.from({ length: n }, (_, i) => series.reduce((t, s) => t + (s.values[i] ?? 0), 0));
  const max = Math.max(...totals, ...(ghost ?? []).map((v) => v ?? 0), 0);
  if (n === 0 || max <= 0) {
    return <div className="py-6 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  // The rounded cap sits on each column's topmost segment — the last series with a value there.
  const topOf = totals.map((_, i) => {
    let top = -1;
    series.forEach((s, si) => {
      if ((s.values[i] ?? 0) > 0) top = si;
    });
    return top;
  });
  const slot = W / n;
  const bw = Math.max(0.8, Math.min(6, slot * 0.62));
  const cap = Math.min(1.2, bw / 2).toFixed(2);
  return (
    <ChartFrame max={max} formatY={formatY} xLabels={xLabels} n={n} labelledBy="stacked bars per day">
      <Gridlines />
      {totals.map((_, i) => {
        let y = H;
        const x = (i * slot + (slot - bw) / 2).toFixed(2);
        return (
          <g key={i}>
            {series.map((s, si) => {
              const v = s.values[i] ?? 0;
              if (!(v > 0)) return null;
              const h = (v / max) * (H - 2);
              y -= h;
              return (
                <rect
                  key={s.label}
                  x={x}
                  y={y.toFixed(2)}
                  width={bw.toFixed(2)}
                  height={Math.max(h, 0.4).toFixed(2)}
                  rx={si === topOf[i] ? cap : 0}
                  fill={s.color}
                >
                  <title>{`${labels[i] ?? ""} · ${s.label}: ${format(v)}`}</title>
                </rect>
              );
            })}
          </g>
        );
      })}
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

/** Splits a gappy series into drawable runs; each run is a "x,y x,y …" point list. */
function lineRuns(values: (number | null)[], max: number): string[] {
  const runs: string[] = [];
  let run: string[] = [];
  values.forEach((v, i) => {
    if (v == null || !Number.isFinite(v) || max <= 0) {
      if (run.length > 0) runs.push(run.join(" "));
      run = [];
      return;
    }
    run.push(`${(((i + 0.5) / values.length) * W).toFixed(1)},${Math.min(H, Math.max(0, H - (v / max) * (H - 2))).toFixed(1)}`);
  });
  if (run.length > 0) runs.push(run.join(" "));
  return runs;
}

/** One multi-series line chart for p50/p95-style series: area bands, dashed reference lines,
 *  an end dot per series, and the same empty state as the bars. */
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
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const max = Math.max(0, ...series.flatMap((s) => s.values.map((v) => v ?? 0)));
  if (n === 0 || max <= 0) {
    return <div className="py-6 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  return (
    <ChartFrame max={max} formatY={formatY} xLabels={xLabels} n={n} labelledBy="lines per day">
      <Gridlines />
      {series.map((s) => {
        const runs = lineRuns(s.values, max);
        if (runs.length === 0) return null;
        const last = s.values.map((v, i) => ({ v, i })).filter((p) => p.v != null);
        const end = last[last.length - 1];
        return (
          <g key={s.label}>
            {s.fill &&
              !s.dashed &&
              runs.map((run, ri) => {
                const xs = run.split(" ").map((pt) => pt.split(",")[0]);
                return <polygon key={ri} points={`${xs[0]},${H} ${run} ${xs[xs.length - 1]},${H}`} fill={s.color} opacity={0.12} />;
              })}
            {runs.map((run, ri) => (
              <polyline
                key={ri}
                points={run}
                fill="none"
                stroke={s.color}
                strokeWidth={s.dashed ? 1.25 : 1.5}
                strokeDasharray={s.dashed ? "1.6 1.2" : undefined}
                strokeLinecap="round"
                strokeLinejoin="round"
                vectorEffect="non-scaling-stroke"
              >
                {ri === 0 && <title>{`${s.label}: ${end.v != null ? format(end.v) : "—"} (${labels[end.i] ?? ""})`}</title>}
              </polyline>
            ))}
            {end.v != null && !s.dashed && (
              <circle
                cx={(((end.i + 0.5) / n) * W).toFixed(1)}
                cy={Math.min(H, Math.max(0, H - (end.v / max) * (H - 2))).toFixed(1)}
                r={1.4}
                fill="var(--panel)"
                stroke={s.color}
                strokeWidth={0.8}
              />
            )}
          </g>
        );
      })}
    </ChartFrame>
  );
}

/** One horizontal stacked strip (the org cards' live/returned strip, the model-mix strip):
 *  segments share the bar by value, each with a native hover title. */
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
      {segments.map((s) =>
        s.value > 0 ? (
          <div key={s.label} className="h-full" style={{ width: `${(s.value / total) * 100}%`, background: s.color }}>
            <title>{`${s.label}: ${format(s.value)}`}</title>
          </div>
        ) : null,
      )}
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
