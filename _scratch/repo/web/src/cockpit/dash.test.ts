// Pure dashboard stats (issue #398): pin the period slicing, the history sums and the
// session derivations directly — no DOM, no clock.
import { describe, expect, it } from "vitest";

import type { Session, SpendDay, SpendHistory } from "../types";
import {
  changeFailRate,
  costPerMerged,
  dailyCosts,
  dailyFailRate,
  dailyMerged,
  formatDelta,
  funnelFor,
  mergedAtOf,
  mergedInWindow,
  providerSnapshots,
  relDelta,
  repoRows,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumLaunched,
  sumReturned,
} from "./dash";

function orgDay(org: string, launched: number, returned: number, cost: number | null): SpendDay["orgs"][number] {
  return { org, cost_usd: cost, routed_cost_usd: null, tokens: { input: 1, output: 1, cache_read: 0, cache_write: 0 }, models: [], launched, returned };
}

const day = (d: string, orgs: SpendDay["orgs"]): SpendDay => ({ day: d, orgs });
const history = (days: SpendDay[]): SpendHistory => ({ days });

function session(overrides: Partial<Session> = {}): Session {
  return { id: "s1", repo: "acme/webshop", org: "acme", issue: 1, issue_title: "t", status: "running", branch: "b", base: "main", parent: null, worktree: "/w", git_admin_dir: null, sandbox: "s", mesh: null, agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd: 1.5, routed_cost_usd: null, cleaned_up: false, keep_worktree: false, created_at: "2026-09-18T09:00:00Z", updated_at: "2026-09-18T09:10:00Z", attention: null, ...overrides };
}

describe("slicePeriods", () => {
  const h = history(["01", "02", "03", "04", "05", "06"].map((d) => day(`2026-09-${d}`, [orgDay("acme", 1, 0, 1)])));
  it("takes the current period off the end and the previous one before it", () => {
    const { current, previous } = slicePeriods(h, 2);
    expect(current.map((d) => d.day)).toEqual(["2026-09-05", "2026-09-06"]);
    expect(previous.map((d) => d.day)).toEqual(["2026-09-03", "2026-09-04"]);
  });
  it("leaves the previous period empty when the history is thinner than two ranges", () => {
    const { current, previous } = slicePeriods(h, 7);
    expect(current).toHaveLength(6);
    expect(previous).toEqual([]);
  });
  it("is empty on both sides with no history", () => {
    expect(slicePeriods(null, 30)).toEqual({ current: [], previous: [] });
  });
});

describe("history sums", () => {
  const h = history([
    day("2026-09-01", [orgDay("acme", 2, 1, 1.5), orgDay("beta", 1, 1, null)]),
    day("2026-09-02", [orgDay("acme", 0, 3, 0.5)]),
  ]);
  it("sums launched and returned across days, scoped by org", () => {
    expect(sumLaunched(h.days)).toBe(3);
    expect(sumLaunched(h.days, "acme")).toBe(2);
    expect(sumReturned(h.days, "beta")).toBe(1);
  });
  it("sums measured spend, ignoring unmeasured days rather than zeroing them", () => {
    expect(sumHistoryCost(h.days)).toBe(2);
    expect(sumHistoryCost(h.days, "beta")).toBeNull();
    expect(dailyCosts(h.days, "beta")).toEqual([null, null]);
  });
});

describe("deltas", () => {
  it("is null with no previous figure, so the tile reads — instead of ±∞", () => {
    expect(relDelta(5, 0)).toBeNull();
    expect(relDelta(null, 4)).toBeNull();
    expect(relDelta(6, 4)).toBeCloseTo(0.5);
    expect(formatDelta(null)).toBe("—");
    expect(formatDelta(0.5)).toBe("+50%");
    expect(formatDelta(-0.034)).toBe("-3.4%");
  });
  it("draws a flat baseline when nothing was measured", () => {
    expect(sparkPoints([null, null])).toContain(",28");
    expect(sparkPoints([])).toBe("");
  });
});

describe("funnelFor and repoRows", () => {
  it("reads launch → PR → merged off current statuses", () => {
    const f = funnelFor([session({ id: "a", status: "running" }), session({ id: "b", status: "pr_opened" }), session({ id: "c", status: "merged" }), session({ id: "d", status: "closed" })]);
    expect(f).toEqual({ launched: 4, prOpened: 3, merged: 1 });
  });
  it("groups sessions by repo with merge rate and measured spend", () => {
    const rows = repoRows([
      session({ id: "a", repo: "acme/webshop", status: "merged", cost_usd: 2 }),
      session({ id: "b", repo: "acme/webshop", status: "failed", cost_usd: null, routed_cost_usd: null }),
      session({ id: "c", repo: "acme/api", status: "running", cost_usd: null, routed_cost_usd: null }),
    ]);
    expect(rows[0]).toMatchObject({ repo: "acme/webshop", colonies: 2, merged: 1, rate: 0.5, spend: 2 });
    expect(rows[1]).toMatchObject({ repo: "acme/api", rate: 0, spend: null });
    expect(costPerMerged(2, 1)).toBe(2);
    expect(costPerMerged(null, 1)).toBeNull();
    expect(costPerMerged(2, 0)).toBeNull();
  });
});

describe("merged_at bucketing", () => {
  const from = Date.parse("2026-09-10T00:00:00Z");
  const to = Date.parse("2026-09-11T00:00:00Z");
  // Created a month ago but merged today: the merge date decides, not the creation date.
  const lateMerge = session({ id: "late", status: "merged", created_at: "2026-08-10T09:00:00Z", updated_at: "2026-09-10T09:00:00Z", merged_at: "2026-09-10T09:00:00Z" });

  it("mergedAtOf prefers merged_at and falls back to created_at", () => {
    expect(mergedAtOf(lateMerge)).toBe("2026-09-10T09:00:00Z");
    expect(mergedAtOf(session({ status: "merged" }))).toBe("2026-09-18T09:00:00Z");
    expect(mergedAtOf(session({ status: "merged", merged_at: null }))).toBe("2026-09-18T09:00:00Z");
  });

  it("mergedInWindow counts by merge date: created long ago but merged today counts today", () => {
    expect(mergedInWindow([lateMerge], from, to).map((s) => s.id)).toEqual(["late"]);
    expect(mergedInWindow([lateMerge], Date.parse("2026-08-10T00:00:00Z"), Date.parse("2026-08-11T00:00:00Z"))).toEqual([]);
  });

  it("falls back to created_at when merged_at is absent", () => {
    const noStamp = session({ id: "old", status: "merged", created_at: "2026-09-10T09:00:00Z", updated_at: "2026-09-10T09:00:00Z" });
    expect(mergedInWindow([noStamp], from, to).map((s) => s.id)).toEqual(["old"]);
  });

  it("dailyMerged buckets merged sessions by merge day", () => {
    expect(dailyMerged([lateMerge], ["2026-08-10", "2026-09-10"])).toEqual([0, 1]);
  });

  it("per-day rate is the in-range merged count divided by days in range", () => {
    const days = ["2026-09-10", "2026-09-11", "2026-09-12"];
    const inRange = mergedInWindow([lateMerge], Date.parse("2026-09-10T00:00:00Z"), Date.parse("2026-09-13T00:00:00Z"));
    expect(inRange.length / days.length).toBeCloseTo(1 / 3);
  });

  it("changeFailRate reads merged by merge date and failed by created date", () => {
    const failed = session({ id: "f1", status: "failed", created_at: "2026-09-10T09:00:00Z", updated_at: "2026-09-10T09:00:00Z" });
    const rate = changeFailRate([lateMerge, failed], from, to);
    expect(rate).toMatchObject({ failed: 1, decided: 2 });
    expect(rate.rate).toBeCloseTo(0.5);
    // A merge outside the window leaves only the failure decided.
    const staleMerge = session({ id: "m2", status: "merged", created_at: "2026-09-10T09:00:00Z", updated_at: "2026-09-12T09:00:00Z", merged_at: "2026-09-12T09:00:00Z" });
    expect(changeFailRate([staleMerge, failed], from, to)).toMatchObject({ failed: 1, decided: 1 });
    expect(dailyFailRate([lateMerge, failed], ["2026-09-10"])).toEqual([0.5]);
  });
});

describe("providerSnapshots", () => {
  it("maps GET /api/status model_providers onto the org dashboard's tallies", () => {
    const snaps = providerSnapshots([
      { id: "strix", name: "Strix Halo", requests: 32_689, failure_pct: 29.4, avg_latency_ms: 12_800, degraded: true },
      { id: "lab", name: "Lab vLLM", requests: 0, failure_pct: 0, avg_latency_ms: 0, degraded: false },
    ]);
    // Failures re-derive from the rounded pct, so the displayed rate reads back exactly.
    expect(snaps[0]).toMatchObject({ name: "Strix Halo", requests: 32_689, avgLatencyMs: 12_800 });
    expect(snaps[0].failures / snaps[0].requests).toBeCloseTo(0.294, 3);
    // No requests: zero latency reads as unmeasured, not 0ms.
    expect(snaps[1]).toMatchObject({ requests: 0, failures: 0, avgLatencyMs: null });
    expect(snaps[1].since).toBeUndefined();
  });
  it("reads empty without a status payload", () => {
    expect(providerSnapshots(null)).toEqual([]);
    expect(providerSnapshots(undefined)).toEqual([]);
  });
});
