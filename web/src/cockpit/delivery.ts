// Delivery KPIs off the session list: lead time (colony launched → merged), PR cycle time (pull
// request opened → merged) and CI pass rate (settled check verdicts). The mothership records
// `pr_opened_at` and `ci_state` from the PR watcher's `gh pr view`; a colony without them simply
// does not count, and a figure with no samples says so instead of showing a zero.
import type { Session } from "../types";
import type { KpiDef } from "./DashChart";
import { deltaTone, formatDelta, formatPts, mergedAtOf, relDelta, sparkPoints } from "./dash";

export interface Window {
  from: number;
  to: number;
}

const HOUR = 3_600_000;

function within(ts: string | null | undefined, win: Window): boolean {
  if (!ts) return false;
  const t = Date.parse(ts);
  return !Number.isNaN(t) && t >= win.from && t < win.to;
}

/** The middle value; the mean of the two middle ones for an even count; null when empty. */
export function median(values: number[]): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** "42m", "5.3h", "2.1d" — a duration in milliseconds, at the unit that reads best. */
export function formatSpan(ms: number): string {
  if (ms < HOUR) return `${Math.max(1, Math.round(ms / 60_000))}m`;
  if (ms < 48 * HOUR) return `${(ms / HOUR).toFixed(1)}h`;
  return `${(ms / (24 * HOUR)).toFixed(1)}d`;
}

/** Launch → merge, per merged colony, in ms; merged in `win` when given. */
export function leadTimes(sessions: Session[], win?: Window): number[] {
  return sessions
    .filter((s) => s.status === "merged" && s.merged_at && (!win || within(s.merged_at, win)))
    .map((s) => Date.parse(s.merged_at as string) - Date.parse(s.created_at))
    .filter((ms) => Number.isFinite(ms) && ms >= 0);
}

/** PR opened → merge, per merged colony that has a PR-opened time, in ms. */
export function cycleTimes(sessions: Session[], win?: Window): number[] {
  return sessions
    .filter((s) => s.status === "merged" && s.merged_at && s.pr_opened_at && (!win || within(s.merged_at, win)))
    .map((s) => Date.parse(s.merged_at as string) - Date.parse(s.pr_opened_at as string))
    .filter((ms) => Number.isFinite(ms) && ms >= 0);
}

export interface CiRate {
  rate: number | null;
  passed: number;
  settled: number;
}

/** Settled check verdicts (success / failure) over colonies whose PR is dated in `win`: by merge
 *  time when merged, by PR-opened time otherwise. Pending and no-checks colonies do not count. */
export function ciPassRate(sessions: Session[], win?: Window): CiRate {
  const settled = sessions.filter(
    (s) =>
      (s.ci_state === "success" || s.ci_state === "failure") &&
      (!win || within(s.status === "merged" ? mergedAtOf(s) : (s.pr_opened_at ?? s.created_at), win)),
  );
  const passed = settled.filter((s) => s.ci_state === "success").length;
  return { rate: settled.length > 0 ? passed / settled.length : null, passed, settled: settled.length };
}

function dayWindow(day: string): Window {
  const from = Date.parse(`${day}T00:00:00`);
  return { from, to: from + 24 * HOUR };
}

/** The three delivery tiles for a KPI strip. `prev` is the previous period, read only when `compare`. */
export function deliveryKpis(sessions: Session[], win: Window | null, prev: Window | null, days: string[], compare: boolean): KpiDef[] {
  const cur = win ?? undefined;
  const lead = leadTimes(sessions, cur);
  const cycle = cycleTimes(sessions, cur);
  const ci = ciPassRate(sessions, cur);
  const prevLead = compare && prev ? median(leadTimes(sessions, prev)) : null;
  const prevCycle = compare && prev ? median(cycleTimes(sessions, prev)) : null;
  const prevCi = compare && prev ? ciPassRate(sessions, prev).rate : null;
  const leadMed = median(lead);
  const cycleMed = median(cycle);
  const perDay = (f: (w: Window) => number | null) => (days.length > 0 ? sparkPoints(days.map((d) => f(dayWindow(d)))) : undefined);
  const leadDelta = relDelta(leadMed, prevLead);
  const cycleDelta = relDelta(cycleMed, prevCycle);
  const ciDelta = ci.rate != null && prevCi != null ? ci.rate - prevCi : null;
  const samples = (n: number) => `median of ${n} merged`;
  return [
    leadMed == null
      ? { label: "Lead time", value: "—", emptyNote: "nothing merged in range yet", hint: "colony launched → pull request merged, median over colonies merged in range (created_at → merged_at)" }
      : {
          label: "Lead time",
          value: formatSpan(leadMed),
          delta: leadDelta != null ? formatDelta(leadDelta) : undefined,
          deltaTone: deltaTone(leadDelta, "down"),
          spark: perDay((w) => median(leadTimes(sessions, w))),
          sub: samples(lead.length),
          hint: "colony launched → pull request merged, median over colonies merged in range (created_at → merged_at)",
        },
    cycleMed == null
      ? { label: "PR cycle time", value: "—", emptyNote: "no merged colonies with PR times yet", hint: "pull request opened → merged, median over merged colonies with a recorded PR-opened time (pr_opened_at → merged_at)" }
      : {
          label: "PR cycle time",
          value: formatSpan(cycleMed),
          delta: cycleDelta != null ? formatDelta(cycleDelta) : undefined,
          deltaTone: deltaTone(cycleDelta, "down"),
          spark: perDay((w) => median(cycleTimes(sessions, w))),
          sub: samples(cycle.length),
          hint: "pull request opened → merged, median over merged colonies with a recorded PR-opened time (pr_opened_at → merged_at)",
        },
    ci.rate == null
      ? { label: "CI pass rate", value: "—", emptyNote: "no settled checks in range yet", hint: "colony pull requests whose checks passed ÷ those whose checks settled (ci_state success / failure)" }
      : {
          label: "CI pass rate",
          value: `${(ci.rate * 100).toFixed(0)}%`,
          delta: ciDelta != null ? formatPts(ciDelta) : undefined,
          deltaTone: deltaTone(ciDelta),
          spark: perDay((w) => ciPassRate(sessions, w).rate),
          sub: `${ci.passed} of ${ci.settled} passed`,
          hint: "colony pull requests whose checks passed ÷ those whose checks settled (ci_state success / failure)",
        },
  ];
}
