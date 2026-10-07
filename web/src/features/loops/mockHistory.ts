// The in-browser mock's per-loop run history (`?mock=1`, issue #1199): ninety days of runs per loop,
// generated once from a seeded rhythm so a reload draws the same chart, and shaped by the same
// bucketing the server does (loop_history.rs): a zero-filled bucket per local day, newest-first runs,
// and the latest run whatever the range.
import { rhythm } from "../../mockShared";
import type { LoopHistory, LoopHistoryBucket, LoopHistoryRun, LoopOutcome } from "./types";

const DAY_MS = 86_400_000;

/** How one mock loop behaves: how often it runs, how its runs go, and what a dispatched colony costs. */
export interface MockLoopShape {
  id: string;
  /** Minutes between runs. */
  every: number;
  /** Relative weights of ok, partial, failed, skipped. */
  weights: [number, number, number, number];
  /** Chance a run dispatches a colony, and the dollars it costs (low, high). */
  dispatch: number;
  cost: [number, number];
  summary: Record<LoopOutcome, string[]>;
  seed: number;
}

const OUTCOMES: LoopOutcome[] = ["ok", "partial", "failed", "skipped"];

/** Every run of one loop for the last 90 days, oldest first. */
export function mockRuns(shape: MockLoopShape, now = Date.now()): LoopHistoryRun[] {
  const rand = rhythm(shape.seed);
  const runs: LoopHistoryRun[] = [];
  const total = shape.weights.reduce((a, b) => a + b, 0);
  // A failure comes in streaks, as real ones do (a billing lock, a bad deploy): bad days are drawn
  // first, then each run on a bad day leans toward failing.
  const badDays = new Set<number>();
  for (let d = 0; d < 90; d++) if (rand() < 0.08) badDays.add(d);
  const step = shape.every * 60_000;
  const start = now - 90 * DAY_MS;
  for (let at = start + Math.floor(rand() * step); at <= now - 60_000; at += step) {
    const day = Math.floor((now - at) / DAY_MS);
    let pick = rand() * total;
    let outcome: LoopOutcome = "skipped";
    for (let i = 0; i < 4; i++) {
      pick -= shape.weights[i];
      if (pick < 0) {
        outcome = OUTCOMES[i];
        break;
      }
    }
    if (badDays.has(day) && rand() < 0.7) outcome = rand() < 0.6 ? "failed" : "partial";
    const dispatched = outcome !== "skipped" && outcome !== "failed" && rand() < shape.dispatch ? 1 + Math.floor(rand() * 2) : 0;
    const cost = dispatched ? dispatched * (shape.cost[0] + rand() * (shape.cost[1] - shape.cost[0])) : 0;
    const lines = shape.summary[outcome];
    runs.push({
      at: new Date(at).toISOString(),
      trigger: "schedule",
      outcome,
      summary: lines[Math.floor(rand() * lines.length)],
      counts: dispatched ? { dispatched } : {},
      colonies: Array.from({ length: dispatched }, (_, i) => `mock-${shape.id}-${Math.floor(at / 1000)}-${i}`),
      cost_usd: Math.round(cost * 100) / 100,
    });
  }
  return runs;
}

const pad = (n: number) => String(n).padStart(2, "0");
const dayKey = (ms: number, tzMinutes: number) => {
  const d = new Date(ms + tzMinutes * 60_000);
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
};

/** The server's answer for `days` days: a bucket per local day, zero-filled, and the runs inside the range. */
export function buildHistory(id: string, runs: readonly LoopHistoryRun[], days: number, tzMinutes: number, now = Date.now()): LoopHistory {
  const buckets: LoopHistoryBucket[] = [];
  for (let i = days - 1; i >= 0; i--) {
    buckets.push({ day: dayKey(now - i * DAY_MS, tzMinutes), runs: 0, ok: 0, partial: 0, failed: 0, skipped: 0, running: 0, colonies: 0, cost_usd: 0 });
  }
  const index = new Map(buckets.map((b, i) => [b.day, i]));
  const inRange: LoopHistoryRun[] = [];
  for (const r of runs) {
    const at = index.get(dayKey(new Date(r.at).getTime(), tzMinutes));
    if (at === undefined) continue;
    const b = buckets[at];
    b.runs += 1;
    b[r.outcome] += 1;
    b.colonies += r.colonies.length;
    b.cost_usd += r.cost_usd;
    inRange.push(r);
  }
  const sum = (key: Exclude<keyof LoopHistoryBucket, "day">) => buckets.reduce((t, b) => t + b[key], 0);
  return {
    id,
    days,
    from: buckets[0].day,
    to: buckets[buckets.length - 1].day,
    retention_days: 90,
    totals: { runs: sum("runs"), ok: sum("ok"), partial: sum("partial"), failed: sum("failed"), skipped: sum("skipped"), running: sum("running"), colonies: sum("colonies"), cost_usd: sum("cost_usd") },
    buckets,
    last: runs.length ? runs[runs.length - 1] : null,
    runs: [...inRange].reverse().slice(0, 200),
  };
}
