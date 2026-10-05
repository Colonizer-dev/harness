// A plan's reset time in words a person reads at a glance: the local clock time and how long until
// it, e.g. "19:51 · in 2 h 10 min". Shared by the quota banner and the model switcher's plan usage.
// `timeZone` exists for the tests; the cockpit always formats in the browser's own zone.

/** "2 h 10 min", "1 d 3 h", "4 min", "under a minute"; "now" once the moment has passed. */
export function untilWords(unix: number, nowMs: number): string {
  const seconds = Math.floor(unix - nowMs / 1000);
  if (seconds <= 0) return "now";
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return hours > 0 ? `${days} d ${hours} h` : `${days} d`;
  if (hours > 0) return minutes > 0 ? `${hours} h ${minutes} min` : `${hours} h`;
  return minutes > 0 ? `${minutes} min` : "under a minute";
}

/**
 * The reset's local clock time: "19:51" today, "Tue 19:51" within the week, "12 Oct 19:51" beyond.
 * Always 24-hour, so a phone in a 12-hour locale still reads one short token.
 */
export function resetClock(unix: number, nowMs: number, timeZone?: string): string {
  const at = new Date(unix * 1000);
  const time = at.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone });
  const day = (d: Date) => d.toLocaleDateString("en-CA", { timeZone });
  const now = new Date(nowMs);
  if (day(at) === day(now)) return time;
  const days = (unix * 1000 - nowMs) / 86_400_000;
  if (days > 0 && days < 6.5) {
    return `${at.toLocaleDateString("en-GB", { weekday: "short", timeZone })} ${time}`;
  }
  return `${at.toLocaleDateString("en-GB", { day: "numeric", month: "short", timeZone })} ${time}`;
}

/**
 * "resets at 19:51 · in 2 h 10 min" from a unix reset; the provider's own words ("resets 09-23
 * 07:54 UTC") when it named no timestamp; "no reset time given" when it named neither.
 */
export function resetWords(
  reset: { reset_unix?: number | null; reset_at?: string | null },
  nowMs: number,
  timeZone?: string,
): string {
  if (reset.reset_unix != null && Number.isFinite(reset.reset_unix)) {
    if (reset.reset_unix * 1000 <= nowMs) return "resetting now";
    const clock = resetClock(reset.reset_unix, nowMs, timeZone);
    // "resets at 19:51" today, "resets Tue 19:51" on another day.
    return `resets ${clock.includes(" ") ? "" : "at "}${clock} · in ${untilWords(reset.reset_unix, nowMs)}`;
  }
  if (reset.reset_at) return `resets ${reset.reset_at}`;
  return "no reset time given";
}
