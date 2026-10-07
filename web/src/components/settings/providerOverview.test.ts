import { describe, expect, it } from "vitest";
import type { ModelProvider, ProviderUsageReport } from "../../types";
import { claudePlanText, dailySeries, gaugeOf, modelsSummary, projectRunOut, providerStatus, resetText } from "./providerOverview";

const base = {
  id: "deepseek",
  name: "DeepSeek",
  base_url: "https://api.deepseek.com/anthropic",
  auth: "x-api-key",
  wire: "anthropic",
  has_key: true,
  models: ["a", "b", "c"],
  preset: "deepseek",
  trusted: false,
  timeout_secs: 600,
  max_concurrent: null,
  queue_timeout_secs: null,
  context_tokens: null,
  fallback_model: null,
  in_flight: 0,
  queued: 0,
  usage: { requests: 120, failures: 0, fallbacks: 0, duration_ms: 1000, since: null, last_request_at: null },
  health: { failure_pct: 0, avg_latency_ms: 10, rated: true, degraded: false },
} as unknown as ModelProvider;
const NOW = Date.parse("2026-10-07T12:00:00Z");

describe("providerStatus", () => {
  it("ranks quota out over degraded over unused", () => {
    expect(providerStatus(base).label).toBe("Healthy");
    expect(providerStatus({ ...base, usage: { ...base.usage!, requests: 0 } }).tone).toBe("idle");
    expect(providerStatus({ ...base, health: { ...base.health!, degraded: true, failure_pct: 12 } }).label).toBe("Degraded");
    expect(providerStatus({ ...base, health: { ...base.health!, degraded: true, failure_pct: 40 } }).label).toBe("Failing");
    expect(providerStatus({ ...base, health: { ...base.health!, degraded: true }, quota_exhausted: { reset_at: null, reset_unix: null } }).label).toBe("Quota out");
  });
});

describe("gaugeOf", () => {
  it("handles an unknown balance, a total, no total, and an exhausted plan", () => {
    expect(gaugeOf(base)).toEqual({ pctLeft: null, text: "balance unknown", tone: "idle" });
    expect(gaugeOf({ ...base, balance: { at: "x", remaining: 620, limit: 1000 } })).toMatchObject({ pctLeft: 62, text: "62%", tone: "ok" });
    expect(gaugeOf({ ...base, balance: { at: "x", remaining: 90, limit: 1000 } }).tone).toBe("err");
    expect(gaugeOf({ ...base, balance: { at: "x", remaining: 3_100_000 } })).toMatchObject({ pctLeft: null, text: "3.1M left" });
    expect(gaugeOf({ ...base, balance: { at: "x", remaining: 500, limit: 1000 }, quota_exhausted: { reset_at: null, reset_unix: null } }).pctLeft).toBe(0);
  });
});

describe("resetText", () => {
  it("shows the reset when known and nothing when not", () => {
    expect(resetText(base, NOW)).toBeNull();
    const unix = NOW / 1000 + 3 * 3600 + 12 * 60;
    expect(resetText({ ...base, balance: { at: "x", remaining: 1, reset_unix: unix } }, NOW)).toBe("resets in 3 h 12 min");
    expect(resetText({ ...base, quota_exhausted: { reset_at: "10-08 07:54 UTC", reset_unix: null } }, NOW)).toBe("resets 10-08 07:54 UTC");
  });
});

describe("modelsSummary", () => {
  it("counts enabled and the discovered extras", () => {
    expect(modelsSummary(base)).toBe("3 enabled");
    expect(modelsSummary({ models: ["a"], discovered_models: ["a", "b", "c"] })).toBe("1 enabled · +2 available");
    expect(modelsSummary({ models: [] })).toBe("No models enabled");
  });
});

describe("projectRunOut", () => {
  const hours = (h: number) => new Date(NOW - h * 3_600_000).toISOString();
  it("extends a falling line to zero", () => {
    const p = projectRunOut([{ at: hours(10), remaining: 1000 }, { at: hours(5), remaining: 750 }, { at: hours(0), remaining: 500 }], NOW);
    expect(p.runOutMs).toBeCloseTo(NOW + 10 * 3_600_000, -4);
    expect(p.perDay).toBeCloseTo(-1200, 0);
  });
  it("refuses thin or rising data and starts again after a refill", () => {
    expect(projectRunOut([{ at: hours(1), remaining: 5 }, { at: hours(0), remaining: 4 }], NOW).runOutMs).toBeNull();
    expect(projectRunOut([{ at: hours(10), remaining: 10 }, { at: hours(5), remaining: 20 }, { at: hours(0), remaining: 30 }], NOW).runOutMs).toBeNull();
    const refilled = projectRunOut(
      [{ at: hours(30), remaining: 10 }, { at: hours(20), remaining: 1000 }, { at: hours(10), remaining: 800 }, { at: hours(0), remaining: 600 }],
      NOW,
    );
    expect(refilled.runOutMs).toBeCloseTo(NOW + 30 * 3_600_000, -4);
  });
});

describe("dailySeries", () => {
  it("derives the failure share per day and tolerates no report", () => {
    const report = { daily: [{ date: "a", requests: 10, failures: 5, avg_latency_ms: 100 }, { date: "b", requests: 0, failures: 0, avg_latency_ms: 0 }] } as ProviderUsageReport;
    expect(dailySeries(report, "failure_pct")).toEqual([50, 0]);
    expect(dailySeries(report, "requests")).toEqual([10, 0]);
    expect(dailySeries(null, "requests")).toEqual([]);
  });
});

describe("claudePlanText", () => {
  it("says the subscription is within limits until one is hit", () => {
    expect(claudePlanText(null, NOW).status).toBe("Subscription");
    const plan = { exhausted: true, reset_unix: NOW / 1000 + 7200, reset_at: null } as never;
    expect(claudePlanText(plan, NOW)).toEqual({ tone: "err", status: "Limit reached", reset: "resets in 2 h" });
  });
});
