import { describe, expect, it } from "vitest";
import { buildHistory, mockRuns } from "../features/loops/mockHistory";
import type { LoopHistory, LoopHistoryRun } from "../types";
import { BUILTIN_HISTORY_ID, dayLabel, describeBucket, friendlyReason, groupItems, lineSubject, money, outcomeSeries, stripBars, stripTotals } from "./loopHistory";

const NOW = Date.parse("2026-10-07T12:00:00Z");
const run = (at: string, outcome: LoopHistoryRun["outcome"], cost = 0): LoopHistoryRun => ({ at, trigger: "schedule", outcome, summary: `a ${outcome} run`, counts: {}, colonies: cost ? ["c"] : [], cost_usd: cost });

describe("run details, grouped", () => {
  const billing = "GitHub Actions did not start the checks: the job was not started because recent account payments have failed or your spending limit needs to be increased.";
  const item = (n: number, repo = "kontinuum-ai/kontinuum", reason = billing) => ({ group: "waiting", repo, ref: { text: `#${n}`, url: `https://github.com/${repo}/pull/${n}` }, title: `PR ${n}`, reason });

  it("merges identical reasons in one repository into one line, with the billing link", () => {
    const groups = groupItems(Array.from({ length: 11 }, (_, i) => item(i)), [{ key: "waiting", label: "Waiting", tone: "neutral" }]);
    expect(groups).toHaveLength(1);
    expect(groups[0].count).toBe(11);
    expect(groups[0].lines).toHaveLength(1);
    const [line] = groups[0].lines;
    expect(lineSubject(line, { one: "PR", many: "PRs" })).toBe("11 PRs in kontinuum-ai/kontinuum");
    expect(line.reason.text).toBe("GitHub Actions is blocked (billing)");
    expect(line.reason.fix?.href).toBe("https://github.com/organizations/kontinuum-ai/settings/billing");
  });

  it("keeps different repositories and different reasons apart, and the biggest line first", () => {
    const groups = groupItems([item(1, "a/b", "waiting behind #9"), item(2, "a/b", "waiting behind #9"), item(3, "c/d", "waiting behind #9"), item(4, "a/b", "its checks are running")], [{ key: "waiting", label: "Waiting", tone: "neutral" }]);
    expect(groups[0].lines.map((l) => `${l.repo}:${l.items.length}`)).toEqual(["a/b:2", "c/d:1", "a/b:1"]);
  });

  it("merges repository-level items across repositories, files them in the order given and drops empty groups", () => {
    const groups = groupItems(
      [
        { group: "skipped", repo: "a/x", reason: "the cooldown holds" },
        { group: "skipped", repo: "b/y", reason: "the cooldown holds" },
        { group: "failed", repo: "c/z", reason: "could not read it" },
      ],
      [
        { key: "failed", label: "Failed", tone: "err" },
        { key: "ok", label: "OK", tone: "ok" },
        { key: "skipped", label: "Skipped", tone: "neutral" },
      ],
    );
    expect(groups.map((g) => g.key)).toEqual(["failed", "skipped"]);
    expect(lineSubject(groups[1].lines[0], { one: "PR", many: "PRs" })).toBe("2 repositories");
  });

  it("leaves a reason that is not a known problem as it was said", () => {
    const r = friendlyReason("  squash-merged:   behind main by 0 ", "a/b");
    expect(r.text).toBe("squash-merged: behind main by 0");
    expect(r.fix).toBeUndefined();
  });
});

describe("the 7-day strip", () => {
  const days = (runs: LoopHistoryRun[]): LoopHistory => buildHistory("loop_a", runs, 7, 0, NOW);

  it("draws a bar per day, stacked by outcome, with the day's spend", () => {
    const h = days([run("2026-10-07T09:00:00Z", "ok", 1.5), run("2026-10-07T10:00:00Z", "failed"), run("2026-10-05T10:00:00Z", "partial", 0.5)]);
    const { mode, bars } = stripBars(h);
    expect(mode).toBe("day");
    expect(bars).toHaveLength(7);
    expect(bars[6].segments).toEqual([
      { outcome: "ok", n: 1 },
      { outcome: "failed", n: 1 },
    ]);
    expect(bars[6].height).toBe(1);
    expect(bars[6].cost).toBe(1);
    expect(bars[4].cost).toBeCloseTo(1 / 3);
    expect(bars[0].segments).toEqual([]);
    expect(bars[6].label).toBe("Wed 7 Oct: 2 runs (1 ok, 1 failed), $1.50");
    expect(stripTotals(h)).toBe("3 runs · 1 failed · 1 partial · $2.00");
  });

  it("draws a bar per day even for an hourly loop, split by the day's mix of outcomes", () => {
    const hourly = Array.from({ length: 24 }, (_, i) => run(`2026-10-06T${String(i).padStart(2, "0")}:00:00Z`, i < 3 ? "failed" : i < 8 ? "skipped" : "ok"));
    const { mode, bars } = stripBars(days(hourly));
    expect(mode).toBe("day");
    expect(bars).toHaveLength(7);
    expect(bars[5].segments).toEqual([
      { outcome: "ok", n: 16 },
      { outcome: "failed", n: 3 },
      { outcome: "skipped", n: 5 },
    ]);
    expect(bars[5].height).toBe(1);
    expect(bars[6].segments).toEqual([]);
  });

  it("says so when nothing ran, and charts outcomes per day", () => {
    const h = days([]);
    expect(stripTotals(h)).toBe("no runs this week");
    expect(describeBucket(h.buckets[0])).toBe("Thu 1 Oct: no runs");
    expect(outcomeSeries(days([run("2026-10-07T09:00:00Z", "ok")])).map((s) => s.label)).toEqual(["OK", "Partial", "Failed", "Skipped"]);
    expect(dayLabel("2026-10-07")).toBe("Wed 7 Oct");
    expect(money(0)).toBe("$0");
    expect(money(0.004)).toBe("<$0.01");
    expect(money(12.5)).toBe("$12.50");
  });
});

describe("the mock's history", () => {
  it("covers ninety days for every built-in loop, zero-filled per day, in the server's shape", () => {
    const runs = mockRuns({ id: "x", every: 60, weights: [1, 1, 1, 1], dispatch: 0.5, cost: [1, 2], seed: 7, summary: { ok: ["a"], partial: ["b"], failed: ["c"], skipped: ["d"], running: [] } }, NOW);
    expect(runs.length).toBeGreaterThan(2000);
    const h = buildHistory("x", runs, 90, 0, NOW);
    expect(h.buckets).toHaveLength(90);
    expect(h.totals.runs).toBe(h.buckets.reduce((n, b) => n + b.runs, 0));
    expect(h.totals.runs).toBe(h.totals.ok + h.totals.partial + h.totals.failed + h.totals.skipped);
    expect(h.runs.length).toBeLessThanOrEqual(200);
    expect(buildHistory("x", runs, 7, 0, NOW).totals.runs).toBeLessThan(h.totals.runs);
    expect(Object.values(BUILTIN_HISTORY_ID)).toEqual(["merge-train", "supply-chain", "ts-any", "docs", "disk-cleanup"]);
  });
});
