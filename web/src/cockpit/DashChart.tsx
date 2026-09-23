// One small shared SVG chart for the dashboards (issue #398): stacked bars per day with an
// optional dashed previous-period ghost. Plain inline SVG, CSS-only hover via <title> — no
// library, no JS tooltip — using only index.css tokens so it follows light/dark automatically.
import type { ReactElement } from "react";

export interface BarSeries {
  label: string;
  color: string;
  values: number[];
}

const H = 32;
const W = 100;

/** One KPI tile: value, previous-period delta and an optional sparkline. Shared by the
 *  overview strip and the org dashboard so the two cannot drift apart. */
export interface KpiDef {
  label: string;
  value: string;
  /** Absent while the compare toggle is off or a snapshot has no previous period. */
  delta?: string;
  spark?: string;
  sub?: string;
  hint: string;
}

export function KpiTile({ label, value, delta, spark, sub, hint }: KpiDef): ReactElement {
  return (
    <div title={hint} className="flex min-w-0 flex-col gap-1.5 rounded-[14px] border border-border bg-panel px-3.5 py-3">
      <div className="truncate font-mono text-[10.5px] tracking-[0.12em] text-faint">{label}</div>
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">{value}</span>
        {delta && <span className="font-mono text-[11px] text-faint">{delta}</span>}
      </div>
      {spark ? (
        <svg viewBox="0 0 100 28" preserveAspectRatio="none" className="block h-7 w-full" aria-hidden="true">
          <polyline points={spark} fill="none" stroke="var(--accent)" strokeWidth={1.5} strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
        </svg>
      ) : null}
      {sub && <div className="font-mono text-[11px] text-faint">{sub}</div>}
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

export function DashBars({
  series,
  labels,
  ghost,
  format,
}: {
  series: BarSeries[];
  /** One x label per day (the "YYYY-MM-DD"), used in the hover titles. */
  labels: string[];
  /** Previous-period daily totals, aligned by index; null = gap. */
  ghost?: (number | null)[];
  format: (value: number) => string;
}): ReactElement {
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const totals = Array.from({ length: n }, (_, i) => series.reduce((t, s) => t + (s.values[i] ?? 0), 0));
  const max = Math.max(...totals, ...(ghost ?? []).map((v) => v ?? 0), 0);
  if (n === 0 || max <= 0) {
    return <div className="py-6 text-center font-mono text-[11px] text-faint">no data in range</div>;
  }
  const slot = W / n;
  const bw = Math.max(0.8, Math.min(6, slot * 0.62));
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" className="block h-28 w-full">
      {[0.25, 0.5, 0.75, 1].map((f) => (
        <line key={f} x1={0} x2={W} y1={H * (1 - f)} y2={H * (1 - f)} stroke="var(--border)" strokeWidth={0.2} strokeDasharray="1 1.4" vectorEffect="non-scaling-stroke" />
      ))}
      {totals.map((_, i) => {
        let y = H;
        const x = (i * slot + (slot - bw) / 2).toFixed(2);
        return (
          <g key={i}>
            {series.map((s) => {
              const v = s.values[i] ?? 0;
              if (!(v > 0)) return null;
              const h = (v / max) * (H - 2);
              y -= h;
              return (
                <rect key={s.label} x={x} y={y.toFixed(2)} width={bw.toFixed(2)} height={Math.max(h, 0.4).toFixed(2)} fill={s.color}>
                  <title>{`${labels[i] ?? ""} · ${s.label}: ${format(v)}`}</title>
                </rect>
              );
            })}
          </g>
        );
      })}
      {ghost && ghost.some((v) => v != null && v > 0) && (
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
      )}
    </svg>
  );
}
