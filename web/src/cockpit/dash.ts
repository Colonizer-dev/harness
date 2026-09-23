// Dashboard stats for the cockpit overview + org dashboard (issue #398): pure derivations over
// SpendHistory and the session list. Every figure keeps its source — history days carry launched /
// returned / cost per org per day, sessions carry status / cost / repo — so anything without one
// (lead time, CI pass, coverage, latency, …) simply has no helper here and is never rendered.
import { sameOrg } from "../components/ui";
import { sessionCost, sumCosts } from "../spend";
import type { Session, SpendDay, SpendHistory, SpendOrgDay } from "../types";

export type RangeDays = 7 | 30 | 90;
export const RANGES: readonly RangeDays[] = [7, 30, 90];

/** Series colours, cycling: index.css tokens only, so they follow light/dark automatically. */
export const DASH_COLORS = ["var(--accent)", "var(--ok)", "var(--info)", "var(--warn)", "var(--err)"];

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

export function formatDelta(d: number | null): string {
  if (d == null) return "—";
  const pct = Math.abs(d * 100);
  return `${d > 0 ? "+" : d < 0 ? "-" : ""}${pct >= 10 ? pct.toFixed(0) : pct.toFixed(1)}%`;
}

/** Sparkline points in a 100×28 box; unmeasured days sit on the baseline, never as zeroes. */
export function sparkPoints(values: (number | null)[], w = 100, h = 28): string {
  if (values.length === 0) return "";
  const nums = values.map((v) => v ?? 0);
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
