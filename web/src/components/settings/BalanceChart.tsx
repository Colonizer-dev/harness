// The credits chart on an expanded provider (issue #1204): the remaining plan balance over the last
// days, drawn in the cockpit dashboards' idiom (DashChart: dashed hairline grid, one smoothed line over
// a soft area, a pulsing "now" dot, HTML overlays for dots and the tooltip so nothing distorts), with
// the plan's reset markers and a dashed run-out projection. It only draws what the Mothership read:
// readings, reset events and the reset time a balance reader reported. The projection is a straight
// line through those readings and is labelled as one.
import { useId, useState, type ReactElement } from "react";

import { monotonePath, type ChartPoint } from "../../cockpit/dash";
import { niceStep } from "../../cockpit/DashChart";
import { untilWords } from "../../resetTime";
import type { ProviderBalance, ProviderUsageReport } from "../../types";
import { compactNumber, projectRunOut, type StatusTone } from "./providerOverview";

const DAY = 86_400_000;
const TONE_VAR: Record<StatusTone, string> = { ok: "var(--ok)", warn: "var(--warn)", err: "var(--err)", idle: "var(--faint)" };

const weekday = new Intl.DateTimeFormat("en-GB", { weekday: "short" });
const dayMonth = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short" });
const clock = new Intl.DateTimeFormat("en-GB", { weekday: "short", hour: "2-digit", minute: "2-digit", hourCycle: "h23" });

/** The local midnights inside [t0, t1], for the x axis. */
function dayTicks(t0: number, t1: number): number[] {
  const out: number[] = [];
  const d = new Date(t0);
  d.setHours(24, 0, 0, 0);
  for (let t = d.getTime(); t <= t1; t += DAY) out.push(t);
  return out;
}

export function BalanceChart({
  report,
  tone,
  resetUnix,
  nowMs,
}: {
  report: ProviderUsageReport;
  tone: StatusTone;
  /** When the plan refills (unix seconds), if known; drawn as a dashed marker ahead of "now". */
  resetUnix: number | null;
  nowMs: number;
}): ReactElement {
  const uid = useId().replace(/:/g, "");
  const [hover, setHover] = useState<number | null>(null);
  const samples: ProviderBalance[] = report.balance;
  const color = TONE_VAR[tone];
  const span = report.days * DAY;
  const t0 = nowMs - span;
  const projection = projectRunOut(samples, nowMs);
  const resetMs = resetUnix != null && resetUnix * 1000 > nowMs ? resetUnix * 1000 : null;
  // A run-out only matters if the plan does not refill first.
  const runOutMs = projection.runOutMs != null && (resetMs == null || projection.runOutMs < resetMs) ? projection.runOutMs : null;
  const ahead = [resetMs, runOutMs].filter((t): t is number => t != null);
  const t1 = nowMs + (ahead.length ? Math.min(Math.max(...ahead) - nowMs + span * 0.04, span * 0.3) : span * 0.03);
  const X = (t: number) => ((t - t0) / (t1 - t0)) * 100;

  const peak = Math.max(1, ...samples.map((s) => Math.max(s.remaining, s.limit ?? 0)));
  const step = niceStep(peak / 2);
  const max = step * 2;
  const Y = (v: number) => Math.max(0, Math.min(100, 100 - (v / max) * 100));

  const pts: ChartPoint[] = samples.map((s) => ({ x: X(Date.parse(s.at)), y: Y(s.remaining) }));
  const last = samples[samples.length - 1];
  // Hold the line level out to "now": the plan keeps the balance of its last reading until the next.
  const drawn = last && Date.parse(last.at) < nowMs - 60_000 ? [...pts, { x: X(nowMs), y: Y(last.remaining) }] : pts;
  const line = monotonePath(drawn);
  const area = drawn.length > 1 ? `${line} L${drawn[drawn.length - 1].x.toFixed(2)},100 L${drawn[0].x.toFixed(2)},100 Z` : "";
  const nowPt = drawn[drawn.length - 1];

  const projTo: ChartPoint | null =
    runOutMs != null && last
      ? { x: X(Math.min(runOutMs, t1)), y: runOutMs <= t1 ? 100 : Y(Math.max(0, last.remaining * (1 - (t1 - Date.parse(last.at)) / (runOutMs - Date.parse(last.at))))) }
      : null;

  const resets = report.events.filter((e) => e.kind === "reset" || e.kind === "recovered").map((e) => Date.parse(e.at));
  const exhausted = report.events.filter((e) => e.kind === "exhausted").map((e) => Date.parse(e.at));
  const ticks = dayTicks(t0, nowMs);
  const labelEvery = report.days > 10 ? Math.ceil(ticks.length / 6) : 1;

  const hs = hover != null ? samples[hover] : null;
  const readout = hs
    ? [clock.format(Date.parse(hs.at)), `${compactNumber(hs.remaining)}${hs.limit ? ` of ${compactNumber(hs.limit)}` : ""} left`]
    : [
        `Last ${report.days} days`,
        last ? `${compactNumber(last.remaining)}${last.limit ? ` of ${compactNumber(last.limit)}` : ""} left` : "no readings",
        resetMs != null ? `resets in ${untilWords(resetMs / 1000, nowMs)}` : null,
        runOutMs != null ? `at this pace, out in ~${untilWords(runOutMs / 1000, nowMs)}` : projection.perDay != null && projection.perDay < 0 ? null : null,
      ].filter((v): v is string => v != null);

  const nearest = (clientX: number, rect: DOMRect) => {
    if (!samples.length) return null;
    const t = t0 + ((clientX - rect.left) / rect.width) * (t1 - t0);
    let best = 0;
    samples.forEach((s, i) => {
      if (Math.abs(Date.parse(s.at) - t) < Math.abs(Date.parse(samples[best].at) - t)) best = i;
    });
    return best;
  };

  return (
    <div>
      <div className="flex min-h-5 flex-wrap items-baseline gap-x-3 gap-y-0.5 text-body-sm tabular-nums text-muted">
        {readout.map((text, i) => (
          <span key={i} className={i === 0 ? "font-medium text-text" : undefined}>
            {text}
          </span>
        ))}
      </div>
      <div className="mt-3 flex gap-2">
        <div aria-hidden="true" className="flex h-[148px] w-9 shrink-0 flex-col justify-between text-right font-mono text-meta leading-none text-faint">
          {[2, 1, 0].map((k) => (
            <span key={k}>{compactNumber(step * k)}</span>
          ))}
        </div>
        <div className="min-w-0 flex-1">
          <div
            role="img"
            aria-label={`Plan balance over the last ${report.days} days${last ? `, ${compactNumber(last.remaining)} left` : ""}`}
            className="dash-overlay relative h-[148px]"
            onMouseMove={(e) => setHover(nearest(e.clientX, e.currentTarget.getBoundingClientRect()))}
            onMouseLeave={() => setHover(null)}
          >
            <div aria-hidden="true" className="absolute inset-x-0 top-0 border-t border-dashed border-border" />
            <div aria-hidden="true" className="absolute inset-x-0 top-1/2 border-t border-dashed border-border" />
            <div aria-hidden="true" className="absolute inset-x-0 bottom-0 border-t border-border" />
            {/* "Now": everything right of it is the future. */}
            <div aria-hidden="true" className="absolute inset-y-0 border-l border-dashed border-border-strong" style={{ left: `${X(nowMs)}%` }} />
            {resets.map((t) => (
              <div key={`r${t}`} aria-hidden="true" title="Plan refilled" className="absolute inset-y-0 border-l border-dashed" style={{ left: `${X(t)}%`, borderColor: "var(--info, var(--faint))" }}>
                {resets.length <= 3 && <span className="absolute -left-px top-0 -translate-x-1/2 -translate-y-full whitespace-nowrap font-mono text-meta text-faint">reset</span>}
              </div>
            ))}
            {exhausted.map((t) => (
              <div key={`x${t}`} aria-hidden="true" title="Plan ran out" className="absolute bottom-0 h-2 border-l-2" style={{ left: `${X(t)}%`, borderColor: "var(--err)" }} />
            ))}
            {resetMs != null && (
              <div aria-hidden="true" className="absolute inset-y-0 border-l border-dashed" style={{ left: `${X(resetMs)}%`, borderColor: "var(--ok)" }}>
                <span className="absolute right-1.5 top-1 whitespace-nowrap font-mono text-meta text-ok">resets</span>
              </div>
            )}
            <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true" className="v3-reveal absolute inset-0 h-full w-full overflow-visible">
              <defs>
                <linearGradient id={`${uid}-a`} x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0" stopColor={color} stopOpacity={0.34} />
                  <stop offset="1" stopColor={color} stopOpacity={0.02} />
                </linearGradient>
              </defs>
              {area && <path d={area} fill={`url(#${uid}-a)`} />}
              {line && <path d={line} fill="none" stroke={color} strokeWidth={1.75} strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />}
              {projTo && nowPt && (
                <line x1={nowPt.x} y1={nowPt.y} x2={projTo.x} y2={projTo.y} stroke="var(--warn)" strokeWidth={1.5} strokeDasharray="4 4" vectorEffect="non-scaling-stroke">
                  <title>Projected run-out: a straight line through the readings</title>
                </line>
              )}
            </svg>
            {nowPt && hover == null && (
              <span aria-hidden="true" className="v3-now-dot pointer-events-none absolute h-2 w-2 -translate-x-1/2 -translate-y-1/2 rounded-full" style={{ left: `${nowPt.x}%`, top: `${nowPt.y}%`, background: color }} />
            )}
            {projTo && runOutMs != null && runOutMs <= t1 && (
              <span aria-hidden="true" className="pointer-events-none absolute bottom-1 -translate-x-full whitespace-nowrap pr-1 font-mono text-meta text-warn" style={{ left: `${projTo.x}%` }}>
                out
              </span>
            )}
            {hover != null && pts[hover] && (
              <>
                <div aria-hidden="true" className="pointer-events-none absolute inset-y-0 w-px bg-border-strong" style={{ left: `${pts[hover].x}%` }} />
                <span
                  aria-hidden="true"
                  className="pointer-events-none absolute box-border h-[9px] w-[9px] -translate-x-1/2 -translate-y-1/2 rounded-full border-2 bg-bg"
                  style={{ left: `${pts[hover].x}%`, top: `${pts[hover].y}%`, borderColor: color }}
                />
              </>
            )}
            {samples.length === 0 && <div className="absolute inset-0 grid place-items-center text-small text-faint">No readings in this window yet</div>}
          </div>
          <div aria-hidden="true" className="relative mt-2 h-[16px] font-mono text-meta text-faint">
            {ticks.map((t, i) =>
              i % labelEvery === 0 ? (
                <span key={t} className="absolute -translate-x-1/2 whitespace-nowrap" style={{ left: `${X(t)}%` }}>
                  {report.days > 10 ? dayMonth.format(t) : weekday.format(t)}
                </span>
              ) : null,
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
