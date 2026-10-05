// A plan's reset in words: local clock time plus a countdown, the provider's own words without a
// timestamp, and a plain "none" without either. Formatted in UTC here so the test is zone-proof.
import { describe, expect, it } from "vitest";

import { resetClock, resetWords, untilWords } from "./resetTime";

const at = (iso: string) => Date.parse(iso);
const unix = (iso: string) => at(iso) / 1000;

describe("untilWords", () => {
  it("reads hours and minutes, days and hours, minutes, and now", () => {
    const now = at("2026-10-05T17:41:58Z");
    expect(untilWords(unix("2026-10-05T19:51:58Z"), now)).toBe("2 h 10 min");
    expect(untilWords(unix("2026-10-05T19:41:58Z"), now)).toBe("2 h");
    expect(untilWords(unix("2026-10-07T20:41:58Z"), now)).toBe("2 d 3 h");
    expect(untilWords(unix("2026-10-05T17:46:58Z"), now)).toBe("5 min");
    expect(untilWords(unix("2026-10-05T17:42:10Z"), now)).toBe("under a minute");
    expect(untilWords(unix("2026-10-05T17:00:00Z"), now)).toBe("now");
  });
});

describe("resetClock", () => {
  it("is the bare time today, the weekday this week, and the date beyond", () => {
    const now = at("2026-10-05T17:41:58Z"); // a Monday
    expect(resetClock(unix("2026-10-05T19:51:58Z"), now, "UTC")).toBe("19:51");
    expect(resetClock(unix("2026-10-06T07:00:00Z"), now, "UTC")).toBe("Tue 07:00");
    expect(resetClock(unix("2026-10-20T07:00:00Z"), now, "UTC")).toBe("20 Oct 07:00");
  });
});

describe("resetWords", () => {
  const now = at("2026-10-05T17:41:58Z");

  it("gives the local time and the countdown for a timestamp", () => {
    expect(resetWords({ reset_unix: unix("2026-10-05T19:51:58Z") }, now, "UTC")).toBe("resets at 19:51 · in 2 h 10 min");
    expect(resetWords({ reset_unix: unix("2026-10-06T07:00:00Z") }, now, "UTC")).toBe("resets Tue 07:00 · in 13 h 18 min");
  });

  it("falls back to the provider's words, then to saying there are none", () => {
    expect(resetWords({ reset_unix: null, reset_at: "7am (UTC)" }, now)).toBe("resets 7am (UTC)");
    expect(resetWords({}, now)).toBe("no reset time given");
    expect(resetWords({ reset_unix: unix("2026-10-05T17:00:00Z") }, now)).toBe("resetting now");
  });
});
