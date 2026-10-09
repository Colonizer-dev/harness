// Plan usage in the model switcher: one compact row per plan the install's roles route to — the
// Claude account and each provider in use — with a bar of used against limit, what is left, and the
// reset. Built only from GET /api/models/plans, which carries what the mothership actually knows: a
// limit it saw (exhausted, the reset, when it hit), the request count through the gateway, the plan
// balance a provider's quota probe answered, and the session and weekly windows the Claude account
// reports itself (issue #1223). Where a plan reports no remaining quota the row
// says so, and shows what is known instead; nothing here is estimated.
//
// The words and the bar are pure functions (`planView`), so the tests pin them without a DOM.
import { resetWords, untilWords } from "../resetTime";
import type { PlanUsage } from "../types";

export type PlanTone = "ok" | "warn" | "err" | "unknown";

export interface PlanView {
  tone: PlanTone;
  /** The right-hand figure: "42.8% left", "2.1M left", "Out · 2 h 10 min", "Session: 62% used · resets 18:41", "no limit reported". */
  figure: string;
  /** Percent of the plan used, for the bar; null when the limit is not known (no bar fill). */
  usedPct: number | null;
  /** "Used by orchestrator and subagents", or null when no role routes here. */
  usedBy: string | null;
  /** The detail lines, each one fact with its source. */
  details: string[];
}

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
const whole = new Intl.NumberFormat("en");

const listWords = (items: string[]): string =>
  items.length <= 1 ? (items[0] ?? "") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;

/** "40 s ago", "5 min ago", "3 h ago", "2 d ago". */
export function agoWords(iso: string, nowMs: number): string {
  const seconds = Math.max(0, Math.floor((nowMs - Date.parse(iso)) / 1000));
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return `${seconds} s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} h ago`;
  return `${Math.floor(seconds / 86_400)} d ago`;
}

const shortDate = (iso: string, timeZone?: string) =>
  new Date(iso).toLocaleDateString("en-GB", { day: "numeric", month: "short", timeZone });

/** A window reading's " · resets …" suffix: " · resets 18:41" within a day, " · resets Thu" further out, "" when none is named. */
const windowResetWords = (unix: number | null, nowMs: number, timeZone?: string): string => {
  if (unix == null || !Number.isFinite(unix)) return "";
  if (unix * 1000 <= nowMs) return " · resetting now";
  const at = new Date(unix * 1000);
  const words =
    at.getTime() - nowMs < 86_400_000
      ? at.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone })
      : at.toLocaleDateString("en-GB", { weekday: "short", timeZone });
  return ` · resets ${words}`;
};

/**
 * What one plan row says. Exhausted wins (a full red bar and the countdown); then a balance with a
 * known limit (the bar and percent left); a balance alone (what is left, no bar); the account's own
 * window readings (the one nearest its cap as the figure, the rest as details); and otherwise what is known —
 * request counts, the last limit hit — labelled as such.
 */
export function planView(plan: PlanUsage, nowMs: number = Date.now(), timeZone?: string): PlanView {
  const details: string[] = [];
  const usedBy = plan.used_by.length ? `Used by ${listWords(plan.used_by)}` : null;
  const balance = plan.balance;
  let tone: PlanTone = "unknown";
  let figure = "no limit reported";
  let usedPct: number | null = null;

  if (plan.exhausted) {
    tone = "err";
    usedPct = 100;
    figure = plan.reset_unix != null && plan.reset_unix * 1000 > nowMs ? `Out · ${untilWords(plan.reset_unix, nowMs)}` : "Out";
    const reset = resetWords(plan, nowMs, timeZone);
    details.push(`Limit reached — ${reset}`);
    // The account fallback is carrying the work (issue #1130): say where Claude's roles run.
    if (plan.fallback) details.push(`Claude out, running on ${plan.fallback.provider_name} (${plan.fallback.model}) until the reset`);
  } else if (balance && balance.error == null && balance.remaining != null) {
    if (balance.pct_left != null && balance.limit != null) {
      usedPct = Math.max(0, Math.min(100, 100 - balance.pct_left));
      tone = balance.pct_left < 10 ? "err" : balance.pct_left < 25 ? "warn" : "ok";
      figure = `${balance.pct_left}% left`;
      details.push(`${compact.format(balance.remaining)} of ${compact.format(balance.limit)} left`);
    } else {
      tone = balance.remaining <= 0 ? "err" : "ok";
      figure = `${compact.format(balance.remaining)} left`;
      details.push("Plan balance; the plan's total isn't reported");
    }
    if (balance.checked_at) details[details.length - 1] += ` · checked ${agoWords(balance.checked_at, nowMs)}`;
  } else if (plan.windows?.length) {
    // The account's own readings (issue #1223): Claude reports its session and weekly usage directly.
    // The figure names the window nearest its cap (a tie keeps the reading's order); the rest are details.
    const windows = plan.windows;
    const lead = windows.reduce((best, w, i) => (w.used_pct > windows[best].used_pct ? i : best), 0);
    const pct = windows.map((w) => Math.round(w.used_pct));
    usedPct = Math.max(0, Math.min(100, Math.max(...pct)));
    tone = usedPct >= 85 ? "warn" : "ok";
    const leadWindow = windows[lead];
    figure = `${leadWindow.label}: ${pct[lead]}% used${windowResetWords(leadWindow.reset_unix, nowMs, timeZone)}`;
    windows.forEach((w, i) => {
      if (i !== lead) details.push(`${w.label}: ${pct[i]}% used${windowResetWords(w.reset_unix, nowMs, timeZone)}`);
    });
    if (plan.windows_checked_at != null) figure += ` · checked ${agoWords(new Date(plan.windows_checked_at * 1000).toISOString(), nowMs)}`;
    if (usedPct >= 85) details.push("Nearly at the limit");
    if (balance?.error) details.push(`Balance check failed: ${balance.error}`);
  } else {
    if (balance?.error) details.push(`Balance check failed: ${balance.error}`);
    else if (plan.kind === "claude") details.push("Claude reports its session and weekly limits only once one is hit");
    else details.push("This provider reports no remaining quota");
  }

  if (plan.requests != null && plan.requests > 0) {
    const since = plan.since ? ` since ${shortDate(plan.since, timeZone)}` : "";
    details.push(`${whole.format(plan.requests)} ${plan.requests === 1 ? "request" : "requests"}${since}`);
  }
  if (!plan.exhausted && plan.last_limit) {
    details.push(`Last limit hit ${agoWords(plan.last_limit.at, nowMs)}`);
  }
  return { tone, figure, usedPct, usedBy, details };
}

/** The plans with the exhausted ones first, then in the mothership's order (Claude, then providers). */
export function sortPlans(plans: readonly PlanUsage[]): PlanUsage[] {
  return plans.map((p, i) => [p, i] as const).sort((a, b) => Number(b[0].exhausted) - Number(a[0].exhausted) || a[1] - b[1]).map(([p]) => p);
}
