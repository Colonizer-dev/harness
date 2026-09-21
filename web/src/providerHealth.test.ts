// The failure-rate helpers' contract (issue #184): what the Mothership says is formatted, never
// recomputed — `degraded` and `rated` are taken as given, at and around the 10% boundary included.
import { describe, expect, it } from "vitest";

import { avgLatencyText, failureRateText, formatAvgLatency, formatFailureRate, formatSince, quotaExhaustedText, quotaTone, usageHealthTone } from "./providerHealth";
import type { ProviderUsageHealth } from "./types";

const NOW = Date.parse("2026-09-19T08:12:00Z");

function health(overrides: Partial<ProviderUsageHealth> = {}): ProviderUsageHealth {
  return { failure_pct: 0, avg_latency_ms: 0, rated: false, degraded: false, ...overrides };
}

// Issue #184's report: a provider failing 29.4% of 32,689 requests at 12.8 s apiece.
const DEGRADED = health({ failure_pct: 29.4, avg_latency_ms: 12_800, rated: true, degraded: true });

describe("formatFailureRate", () => {
  it("keeps the one decimal the wire promises", () => {
    expect(formatFailureRate(29.4)).toBe("29.4%");
  });

  it("shows a clean zero as measured, not blank", () => {
    expect(formatFailureRate(0)).toBe("0.0%");
  });

  it("does not round the extremes away", () => {
    expect(formatFailureRate(100)).toBe("100.0%");
    expect(formatFailureRate(0.04)).toBe("0.0%");
  });
});

describe("formatAvgLatency", () => {
  it("reads the issue's 12.8 s average as the fact it is", () => {
    expect(formatAvgLatency(12_800)).toBe("12.8s");
  });

  it("keeps one decimal below a minute and drops a whole one", () => {
    expect(formatAvgLatency(1_234)).toBe("1.2s");
    expect(formatAvgLatency(45_000)).toBe("45s");
  });

  it("reads sub-second like the reachability probe", () => {
    expect(formatAvgLatency(0)).toBe("0 ms");
    expect(formatAvgLatency(840)).toBe("840 ms");
  });

  it("follows formatDuration's convention for long averages", () => {
    expect(formatAvgLatency(90_000)).toBe("1m 30s");
    expect(formatAvgLatency(3_600_000)).toBe("1h 0m");
  });
});

describe("formatSince", () => {
  it("reads a day and month for the current year", () => {
    expect(formatSince("2026-03-12T09:00:00Z", NOW)).toBe("12 Mar");
  });

  it("adds the year once the tally started in an earlier one", () => {
    expect(formatSince("2024-03-12T09:00:00Z", NOW)).toBe("12 Mar 2024");
  });

  it("is null for a null or unparseable start", () => {
    expect(formatSince(null, NOW)).toBeNull();
    expect(formatSince(undefined, NOW)).toBeNull();
    expect(formatSince("not a date", NOW)).toBeNull();
  });
});

describe("failureRateText", () => {
  it("is nothing without health or without requests", () => {
    expect(failureRateText(undefined, 32_689)).toBeNull();
    expect(failureRateText(DEGRADED, 0)).toBeNull();
  });

  it("states the issue's rate plainly when the Mothership rates it", () => {
    expect(failureRateText(DEGRADED, 32_689)).toBe("29.4% failed");
  });

  it("shows an unrated sample's numbers without presenting them as a verdict", () => {
    expect(failureRateText(health({ failure_pct: 50 }), 2)).toBe("50.0% failed, too few requests to judge");
  });
});

describe("avgLatencyText", () => {
  it("shows the average only when something was dispatched", () => {
    expect(avgLatencyText(DEGRADED, 32_689)).toBe("12.8s avg");
    expect(avgLatencyText(DEGRADED, 0)).toBeNull();
    expect(avgLatencyText(undefined, 32_689)).toBeNull();
  });
});

describe("usageHealthTone", () => {
  it("tones degraded err right at the 10% boundary and ok just under it", () => {
    expect(usageHealthTone(health({ failure_pct: 10, rated: true, degraded: true }))).toBe("err");
    expect(usageHealthTone(health({ failure_pct: 9.9, rated: true, degraded: false }))).toBe("ok");
  });

  it("gives an unrated provider no verdict, whatever its raw percentage", () => {
    expect(usageHealthTone(health({ failure_pct: 50, rated: false, degraded: false }))).toBeNull();
    expect(usageHealthTone(undefined)).toBeNull();
  });
});

describe("quotaTone", () => {
  it("tones an exhausted plan err and a healthy one nothing", () => {
    expect(quotaTone({ reset_at: "09-23 07:54 UTC", reset_unix: 1_789_000_000 })).toBe("err");
    expect(quotaTone({ reset_at: null, reset_unix: null })).toBe("err");
    expect(quotaTone(null)).toBeNull();
    expect(quotaTone(undefined)).toBeNull();
  });
});

describe("quotaExhaustedText", () => {
  it("names the reset when the provider gave one", () => {
    expect(quotaExhaustedText({ reset_at: "09-23 07:54 UTC", reset_unix: 1_789_000_000 })).toBe(
      "quota exhausted, resets 09-23 07:54 UTC",
    );
  });

  it("reads plain exhaustion without a reset, and nothing without exhaustion", () => {
    expect(quotaExhaustedText({ reset_at: null, reset_unix: null })).toBe("quota exhausted");
    expect(quotaExhaustedText(null)).toBeNull();
    expect(quotaExhaustedText(undefined)).toBeNull();
  });
});
