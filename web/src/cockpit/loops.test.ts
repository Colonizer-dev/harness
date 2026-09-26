import { describe, expect, it } from "vitest";
import { describeLoopCadence, intervalWords, nameFromPrompt, parseInterval, parseLoopCommand, relative, toLocalChoice, toUtcLoopCadence } from "./loops";

describe("loops", () => {
  it("parses intervals the way people type them", () => {
    expect(parseInterval("15m")).toBe(15);
    expect(parseInterval("2h")).toBe(120);
    expect(parseInterval("1.5h")).toBe(90);
    expect(parseInterval("1d")).toBe(1440);
    expect(parseInterval("90 minutes")).toBe(90);
    expect(parseInterval("check")).toBeNull();
  });

  it("reads /loop like Claude Code: an interval, or self-paced without one", () => {
    expect(parseLoopCommand("/loop 1h check CI on main and fix flakes")).toEqual({
      cadence: { every: "interval", minutes: 60 },
      prompt: "check CI on main and fix flakes",
      error: null,
    });
    expect(parseLoopCommand("/loop keep the docs in step with the code")).toEqual({
      cadence: { every: "self_paced" },
      prompt: "keep the docs in step with the code",
      error: null,
    });
    expect(parseLoopCommand("/loop 5m poll")?.error).toMatch(/15 minutes/);
    expect(parseLoopCommand("/loop")?.error).toMatch(/Say what the loop should do/);
    expect(parseLoopCommand("fix the build")).toBeNull();
  });

  it("reads whole-day intervals past a week as every-N-days, anchored at this UTC time", () => {
    const now = new Date(Date.UTC(2026, 8, 24, 3, 5));
    expect(parseLoopCommand("/loop 14d check the map", now)).toEqual({
      cadence: { every: "every_days", days: 14, hour: 3, minute: 5 },
      prompt: "check the map",
      error: null,
    });
    for (const days of [30, 60]) {
      expect(parseLoopCommand(`/loop ${days}d sweep the backlog`, now)?.cadence).toEqual({ every: "every_days", days, hour: 3, minute: 5 });
    }
    // Up to and including a week it is still the minute interval it always was.
    expect(parseLoopCommand("/loop 7d sweep the backlog", now)?.cadence).toEqual({ every: "interval", minutes: 10080 });
    expect(parseLoopCommand("/loop 366d sweep the backlog", now)?.error).toMatch(/365/);
  });

  it("describes cadences in words", () => {
    expect(describeLoopCadence({ every: "interval", minutes: 15 })).toBe("every 15 minutes");
    expect(describeLoopCadence({ every: "interval", minutes: 120 })).toBe("every 2 hours");
    expect(describeLoopCadence({ every: "self_paced" })).toMatch(/self-paced/);
    expect(intervalWords(1440)).toBe("day");
  });

  it("round-trips a daily local time through UTC", () => {
    const now = new Date(2026, 8, 24, 12, 0);
    const utc = toUtcLoopCadence({ every: "daily", time: "09:30" }, now);
    expect(utc.every).toBe("daily");
    expect(toLocalChoice(utc, now)).toEqual({ every: "daily", time: "09:30" });
    expect(toUtcLoopCadence({ every: "interval", minutes: 3 })).toEqual({ every: "interval", minutes: 15 });
  });

  it("round-trips an every-N-days local time through UTC, crossing midnight when the zone pushes it", () => {
    const now = new Date(2026, 8, 24, 0, 30);
    const utc = toUtcLoopCadence({ every: "every_days", days: 14, time: "23:30" }, now);
    // The stored UTC time is the local one shifted by the zone, wrapping past midnight as needed.
    const shift = -now.getTimezoneOffset();
    const minutes = (23 * 60 + 30 + shift + 1440) % 1440;
    expect(utc).toEqual({ every: "every_days", days: 14, hour: Math.floor(minutes / 60), minute: minutes % 60 });
    expect(toLocalChoice(utc, now)).toEqual({ every: "every_days", days: 14, time: "23:30" });
    expect(describeLoopCadence(utc, now)).toBe("every 14 days at 23:30");
    expect(describeLoopCadence({ every: "every_days", days: 1, hour: 0, minute: 0 }, now)).toMatch(/every 1 day at /);
  });

  it("names a loop from its prompt and says how far off a run is", () => {
    expect(nameFromPrompt("Triage new issues\nand more")).toBe("Triage new issues");
    expect(nameFromPrompt("x".repeat(80)).endsWith("…")).toBe(true);
    const now = Date.UTC(2026, 8, 24, 12, 0);
    expect(relative(new Date(now + 3 * 3600_000).toISOString(), now)).toBe("in 3h");
    expect(relative(new Date(now - 20 * 60_000).toISOString(), now)).toBe("20m ago");
    expect(relative(null, now)).toBe("—");
  });
});
