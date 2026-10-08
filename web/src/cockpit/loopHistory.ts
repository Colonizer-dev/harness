// The pure half of the Loops page's redesign (issue #1199): how a loop's history becomes the 7-day
// strip and the detail charts, how a run's items are grouped by outcome with identical reasons merged
// into one line, and the plain-language rewrites of the reasons GitHub gives. Kept apart from the
// components so it is testable without a DOM.
import type { LoopHistory, LoopHistoryBucket, LoopHistoryRun, LoopOutcome } from "../types";

/** The ids the server keeps the built-in loops' history under. */
export const BUILTIN_HISTORY_ID = {
  mergeTrain: "merge-train",
  supplyChain: "supply-chain",
  tsAny: "ts-any",
  docs: "docs",
  diskCleanup: "disk-cleanup",
} as const;

/** The words and colours of an outcome, the same on a pill, a bar and a legend. */
export const OUTCOME: Record<LoopOutcome, { label: string; color: string; tone: "ok" | "warn" | "err" | "neutral" | "info" }> = {
  ok: { label: "OK", color: "var(--ok)", tone: "ok" },
  partial: { label: "Partial", color: "var(--warn)", tone: "warn" },
  failed: { label: "Failed", color: "var(--err)", tone: "err" },
  skipped: { label: "Skipped", color: "var(--faint)", tone: "neutral" },
  running: { label: "Running", color: "var(--info)", tone: "info" },
};

/** The order outcomes stack in, calmest at the bottom. */
export const OUTCOME_ORDER: LoopOutcome[] = ["ok", "partial", "failed", "skipped", "running"];

/** "$4.20", "$0.04", "<$0.01" and "$0" for nothing. */
export function money(usd: number): string {
  if (usd <= 0) return "$0";
  if (usd < 0.01) return "<$0.01";
  return `$${usd >= 100 ? Math.round(usd) : usd.toFixed(2)}`;
}

/** "1 run" / "24 runs". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] as const;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"] as const;

/** A bucket's day as "Mon 5 Oct". `YYYY-MM-DD` is read as a calendar date, never shifted by a zone. */
export function dayLabel(day: string): string {
  const [y, m, d] = day.split("-").map(Number);
  const date = new Date(Date.UTC(y, m - 1, d));
  return `${DAYS[date.getUTCDay()]} ${d} ${MONTHS[m - 1]}`;
}

/** "5 Oct", for a chart's axis. */
export function dayShort(day: string): string {
  const [, m, d] = day.split("-").map(Number);
  return `${d} ${MONTHS[m - 1]}`;
}

/** The weekday's first letter, for the strip's foot. */
export function dayInitial(day: string): string {
  const [y, m, d] = day.split("-").map(Number);
  return DAYS[new Date(Date.UTC(y, m - 1, d)).getUTCDay()][0];
}

/** What one bucket says, for a tooltip and a screen reader. */
export function describeBucket(b: LoopHistoryBucket): string {
  if (b.runs === 0) return `${dayLabel(b.day)}: no runs`;
  const parts = OUTCOME_ORDER.filter((o) => b[o] > 0).map((o) => `${b[o]} ${OUTCOME[o].label.toLowerCase()}`);
  return `${dayLabel(b.day)}: ${plural(b.runs, "run")} (${parts.join(", ")})${b.cost_usd > 0 ? `, ${money(b.cost_usd)}` : ""}`;
}

/** One bar of the 7-day strip. */
export interface StripBar {
  key: string;
  /** Segments, bottom first: an outcome and how many runs of it. A per-run bar has one, of 1. */
  segments: { outcome: LoopOutcome; n: number }[];
  /** 0..1 of the strip's height. */
  height: number;
  /** 0..1: how much of the strip's busiest day's spend this bar carries. */
  cost: number;
  label: string;
}

/** The strip's bars for a 7-day history: always one bar per day, stacked by the day's mix of outcomes, so every card reads the same. */
export function stripBars(history: LoopHistory): { mode: "day"; bars: StripBar[] } {
  const most = Math.max(1, ...history.buckets.map((b) => b.runs));
  const topCost = Math.max(0, ...history.buckets.map((b) => b.cost_usd));
  return {
    mode: "day",
    bars: history.buckets.map((b) => ({
      key: b.day,
      segments: OUTCOME_ORDER.filter((o) => b[o] > 0).map((o) => ({ outcome: o, n: b[o] })),
      height: b.runs / most,
      cost: topCost > 0 ? b.cost_usd / topCost : 0,
      label: describeBucket(b),
    })),
  };
}

/** The strip's one-line total: "24 runs · 2 failed · $4.20". */
export function stripTotals(history: LoopHistory): string {
  const t = history.totals;
  if (t.runs === 0) return "no runs this week";
  const bits = [plural(t.runs, "run")];
  if (t.failed > 0) bits.push(`${t.failed} failed`);
  if (t.partial > 0) bits.push(`${t.partial} partial`);
  bits.push(money(t.cost_usd));
  return bits.join(" · ");
}

/** A chart's series for a range: runs by outcome, per day, in stacking order. */
export function outcomeSeries(history: LoopHistory): { label: string; color: string; values: number[] }[] {
  return (["ok", "partial", "failed", "skipped"] as const).map((o) => ({ label: OUTCOME[o].label, color: OUTCOME[o].color, values: history.buckets.map((b) => b[o]) }));
}

/** The run a card calls "last": the newest, in words. */
export function lastRunLine(run: LoopHistoryRun | null): string {
  return run ? run.summary : "not run yet";
}

// --- run details, grouped by outcome ------------------------------------------------------------

/** One thing a run did to one subject (a pull request, a repository, a finding). */
export interface DetailItem {
  /** The group it files under. */
  group: string;
  repo: string;
  /** "#212", or the repository itself for a repository-level item. */
  ref?: { text: string; url?: string; /** The text stands alone: no repository in front of it. */ bare?: boolean };
  title?: string;
  /** The colony this item started or belongs to. */
  colony?: string;
  reason: string;
}

export interface DetailGroupDef {
  key: string;
  label: string;
  tone: "ok" | "warn" | "err" | "neutral" | "info" | "accent";
}

/** One merged line: `count` items that share a repository and a reason. */
export interface DetailLine {
  repo: string;
  reason: ReasonText;
  items: DetailItem[];
}

export interface DetailGroup extends DetailGroupDef {
  count: number;
  lines: DetailLine[];
}

/** A reason in plain words, and where to go to fix it when there is somewhere. */
export interface ReasonText {
  text: string;
  /** The reason as GitHub or the loop said it, for a tooltip. */
  raw: string;
  fix?: { label: string; href: string };
}

/** Rewrites the reasons that are really one problem: a billing lock reads the same on every pull request. */
export function friendlyReason(raw: string, repo: string): ReasonText {
  const org = repo.split("/")[0];
  if (/spending limit|payments have failed|billing/i.test(raw) && /actions|job|check/i.test(raw)) {
    return { text: "GitHub Actions is blocked (billing)", raw, fix: org ? { label: "Fix billing", href: `https://github.com/organizations/${org}/settings/billing` } : undefined };
  }
  return { text: raw.replace(/\s+/g, " ").trim(), raw };
}

/**
 * Groups a run's items by outcome, in the order of `defs`, and merges those that share a repository
 * and a reason into one line. A repository-level item (no `ref`) merges across repositories instead,
 * so "3 repositories: the cooldown holds" is one line, not three.
 */
export function groupItems(items: readonly DetailItem[], defs: readonly DetailGroupDef[]): DetailGroup[] {
  return defs
    .map((def) => {
      const lines = new Map<string, DetailLine>();
      for (const item of items.filter((i) => i.group === def.key)) {
        const reason = friendlyReason(item.reason, item.repo);
        const key = item.ref ? `${item.repo}\u0000${reason.text}` : `\u0000${reason.text}`;
        const line = lines.get(key);
        if (line) line.items.push(item);
        else lines.set(key, { repo: item.ref ? item.repo : "", reason, items: [item] });
      }
      const list = [...lines.values()].sort((a, b) => b.items.length - a.items.length);
      return { ...def, count: list.reduce((n, l) => n + l.items.length, 0), lines: list };
    })
    .filter((g) => g.count > 0);
}

/** A merged line in words: "11 PRs in kontinuum-ai/kontinuum", "3 repositories". A single item has no summary line. */
export function lineSubject(line: DetailLine, unit: { one: string; many: string }): string {
  const n = line.items.length;
  if (line.repo) return `${n} ${n === 1 ? unit.one : unit.many} in ${line.repo}`;
  return `${n} ${n === 1 ? "repository" : "repositories"}`;
}
