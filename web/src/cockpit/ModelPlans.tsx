// Plan usage in the model switcher: one compact row per plan the install's roles route to — the
// Claude account and each provider in use — with a bar of used against limit, what is left, and the
// reset. Built only from GET /api/models/plans, which carries what the mothership actually knows: a
// limit it saw (exhausted, the reset, when it hit), the request count through the gateway, and the
// plan balance a provider's quota probe answered. Where a plan reports no remaining quota the row
// says so, and shows what is known instead; nothing here is estimated.
//
// The words and the bar are pure functions (`planView`), so the tests pin them without a DOM.
import type { ReactElement } from "react";

import { cx } from "../components/ui";
import { resetWords, untilWords } from "../resetTime";
import type { PlanUsage } from "../types";

export type PlanTone = "ok" | "warn" | "err" | "unknown";

export interface PlanView {
  tone: PlanTone;
  /** The right-hand figure: "42.8% left", "2.1M left", "Out · 2 h 10 min", "no limit reported". */
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

/**
 * What one plan row says. Exhausted wins (a full red bar and the countdown); then a balance with a
 * known limit (the bar and percent left); a balance alone (what is left, no bar); and otherwise
 * what is known — request counts, the last limit hit — labelled as such.
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

const FILL: Record<PlanTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", unknown: "bg-faint" };
const FIGURE: Record<PlanTone, string> = { ok: "text-muted", warn: "text-warn", err: "text-err font-semibold", unknown: "text-faint" };

/** One plan: name and figure, the bar, and its detail line. */
export function PlanRow({ plan, nowMs }: { plan: PlanUsage; nowMs?: number }): ReactElement {
  const v = planView(plan, nowMs);
  const label = `${plan.name}: ${v.figure}. ${[v.usedBy, ...v.details].filter(Boolean).join(". ")}`;
  return (
    <li data-plan={plan.id} data-tone={v.tone} className={cx("rounded-lg px-2 py-1.5", plan.exhausted ? "bg-err-soft" : "bg-panel-2")} aria-label={label}>
      <div className="flex items-baseline justify-between gap-2 text-[12.5px]">
        <span className="min-w-0 truncate font-semibold text-text">{plan.name}</span>
        <span className={cx("shrink-0 tabular-nums text-[11.5px]", FIGURE[v.tone])}>{v.figure}</span>
      </div>
      <div
        role="meter"
        aria-label={`${plan.name} plan used`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={v.usedPct ?? undefined}
        aria-valuetext={v.usedPct == null ? "limit not reported" : `${Math.round(v.usedPct)}% used`}
        className={cx("mt-1 h-1.5 w-full overflow-hidden rounded-full", v.usedPct == null ? "border border-dashed border-border bg-transparent" : "bg-panel-3")}
      >
        {v.usedPct != null && <div className={cx("h-full rounded-full", FILL[v.tone])} style={{ width: `${Math.max(v.usedPct, 2)}%` }} />}
      </div>
      <p className="m-0 mt-1 text-[11px] leading-snug text-faint">
        {[v.usedBy, ...v.details].filter(Boolean).join(" · ")}
      </p>
    </li>
  );
}

/** The plans section of the popover: a loading line, an empty line, or the rows. */
export function PlanList({ plans, error, nowMs }: { plans: PlanUsage[] | null; error?: string | null; nowMs?: number }): ReactElement {
  return (
    <section aria-label="plan usage" className="mb-3">
      <div className="mb-1 flex items-baseline justify-between text-[11.5px] text-muted">
        <span>Plans in use</span>
        {plans && plans.some((p) => p.exhausted) && <span className="text-err">{plans.filter((p) => p.exhausted).length} out</span>}
      </div>
      {error ? (
        <p className="m-0 text-[11.5px] text-faint">Plan usage unavailable: {error}</p>
      ) : plans === null ? (
        <p className="m-0 text-[11.5px] text-faint">Reading plan usage…</p>
      ) : plans.length === 0 ? (
        <p className="m-0 text-[11.5px] text-faint">No plan in use reports anything yet.</p>
      ) : (
        <ul className="m-0 list-none space-y-1.5 p-0">
          {sortPlans(plans).map((p) => (
            <PlanRow key={p.id} plan={p} nowMs={nowMs} />
          ))}
        </ul>
      )}
    </section>
  );
}
