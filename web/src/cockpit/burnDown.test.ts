// The burn-down card (issue #210) formats status fields and decides when to show itself; these
// helpers are pure, so what matters is that every state reads the way the card shows it, that a
// countdown handles a missing reset, and that a stopped-but-recently-used scheduler stays visible.
import { describe, expect, it } from "vitest";

import { formatCountdown, msToReset, shouldShow, stateLabel, stateTone, usd } from "./burnDown";
import type { BurnDownState, BurnDownStatus } from "../types";

function status(overrides: Partial<BurnDownStatus> = {}): BurnDownStatus {
  return {
    enabled: true,
    state: "burning",
    estimate: true,
    now: "2026-09-18T10:00:00Z",
    next_reset: "2026-09-19T09:00:00Z",
    window_start: null,
    spent_usd: 140,
    allowance_usd: 200,
    remaining_usd: 60,
    reserve_usd: 10,
    colonies: { live: 1, queued: 1, total: 3 },
    launches_needed: 6,
    launches_done: 3,
    ...overrides,
  };
}

describe("msToReset", () => {
  const now = new Date("2026-09-18T10:00:00Z");

  it("measures to the next reset", () => {
    expect(msToReset(status(), now)).toBe(23 * 3_600_000);
  });

  it("is null with no reset, so the card has nothing to count down", () => {
    expect(msToReset(status({ next_reset: null }), now)).toBeNull();
    expect(msToReset(status({ next_reset: "not a date" }), now)).toBeNull();
  });

  it("clamps a reset already passed to 0, not a negative span", () => {
    expect(msToReset(status({ next_reset: "2026-09-18T09:00:00Z" }), now)).toBe(0);
  });
});

describe("formatCountdown", () => {
  it("shows days and hours", () => {
    expect(formatCountdown((2 * 86_400_000) + (5 * 3_600_000) + 1_834_700)).toBe("2d 5h");
  });

  it("shows hours and minutes", () => {
    expect(formatCountdown((3 * 3_600_000) + (12 * 60_000))).toBe("3h 12m");
  });

  it("says <1m inside the last minute", () => {
    expect(formatCountdown(0)).toBe("<1m");
    expect(formatCountdown(45_000)).toBe("<1m");
  });

  it("renders a dash with no reset to count down to", () => {
    expect(formatCountdown(null)).toBe("—");
  });
});

describe("stateLabel and stateTone", () => {
  const cases: [BurnDownState, string, "neutral" | "info" | "ok" | "warn" | "err" | "accent"][] = [
    ["disabled", "Off", "neutral"],
    ["unconfigured", "Needs repos", "warn"],
    ["unknown_allowance", "Allowance unknown", "warn"],
    ["outside_window", "Waiting for window", "neutral"],
    ["burning", "Burning down", "accent"],
    ["at_reserve", "At reserve", "ok"],
  ];
  for (const [state, label, tone] of cases) {
    it(`reads ${state} as "${label}"`, () => {
      expect(stateLabel(state)).toBe(label);
      expect(stateTone(state)).toBe(tone);
    });
  }

  it("falls back to the raw state for a state the card does not know", () => {
    expect(stateLabel("some_new_state" as BurnDownState)).toBe("some_new_state");
  });
});

describe("shouldShow", () => {
  it("shows while the scheduler is on, even with no colonies yet", () => {
    expect(shouldShow(status({ enabled: true, colonies: { live: 0, queued: 0, total: 0 } }))).toBe(true);
  });

  it("stays visible after a stop while colonies remain, until the reset clears it", () => {
    expect(shouldShow(status({ enabled: false }))).toBe(true);
  });

  it("hides once there is nothing on and nothing to look at", () => {
    expect(shouldShow(status({ enabled: false, colonies: { live: 0, queued: 0, total: 0 } }))).toBe(false);
  });
});

describe("usd", () => {
  it("formats dollars with two decimals", () => {
    expect(usd(60)).toBe("$60.00");
    expect(usd(9.5)).toBe("$9.50");
    expect(usd(0)).toBe("$0.00");
  });
});