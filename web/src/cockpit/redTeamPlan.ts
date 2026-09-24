// The pure half of the red-team wizard and history: turning the operator's local schedule choice into
// the UTC cadence the mothership stores, describing a cadence back in local time, and estimating what
// a run costs from the runs that came before it. Kept apart from the components so it is testable.
import { sessionCost } from "../spend";
import type { RedTeamCadence, RedTeamRun, Session } from "../types";

export const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"] as const;

export type ScheduleChoice =
  | { every: "once" }
  | { every: "weekly"; weekday: number; time: string }
  | { every: "monthly"; day: number; time: string };

function hm(time: string): [number, number] {
  const [h, m] = time.split(":").map((n) => Number.parseInt(n, 10));
  return [Number.isFinite(h) ? h : 0, Number.isFinite(m) ? m : 0];
}

/**
 * The operator's local weekday/day and time as the UTC cadence the server stores. Built from a real
 * local date (the next matching one), so the shift across midnight moves the weekday or day with it.
 */
export function toUtcCadence(choice: ScheduleChoice, now = new Date()): RedTeamCadence | null {
  if (choice.every === "once") return null;
  const [h, m] = hm(choice.time);
  if (choice.every === "weekly") {
    const local = new Date(now);
    const today = (local.getDay() + 6) % 7; // Monday = 0
    local.setDate(local.getDate() + ((choice.weekday - today + 7) % 7));
    local.setHours(h, m, 0, 0);
    return { every: "weekly", weekday: (local.getUTCDay() + 6) % 7, hour: local.getUTCHours(), minute: local.getUTCMinutes() };
  }
  const local = new Date(now.getFullYear(), now.getMonth(), Math.min(choice.day, 28), h, m, 0, 0);
  // Days 29–31 keep their number: the server clamps them to the month's end. The UTC shift is taken
  // from day 28, which exists in every month.
  const shift = local.getUTCDate() - local.getDate();
  const day = Math.min(31, Math.max(1, choice.day + (shift > 1 ? -1 : shift < -1 ? 1 : shift)));
  return { every: "monthly", day, hour: local.getUTCHours(), minute: local.getUTCMinutes() };
}

/** "Every Monday at 09:00" / "Monthly on day 1 at 06:00", in the viewer's local time. */
export function describeCadence(cadence: RedTeamCadence, now = new Date()): string {
  const utc = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), cadence.every === "monthly" ? Math.min(cadence.day, 28) : now.getUTCDate(), cadence.hour, cadence.minute));
  if (cadence.every === "weekly") {
    utc.setUTCDate(utc.getUTCDate() + ((cadence.weekday - ((utc.getUTCDay() + 6) % 7) + 7) % 7));
  }
  const time = utc.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (cadence.every === "weekly") return `Every ${WEEKDAYS[(utc.getDay() + 6) % 7]} at ${time}`;
  const day = cadence.day > 28 ? cadence.day : utc.getDate();
  return `Monthly on day ${day}${cadence.day > 28 ? " (or the month's last)" : ""} at ${time}`;
}

/** What one run cost: its hunters' spend, summed. Null while none of them has a price yet. */
export function runCost(run: RedTeamRun, sessions: Session[]): number | null {
  let total = 0;
  let priced = false;
  for (const hunter of run.hunters) {
    const session = sessions.find((s) => s.id === hunter.session_id);
    const cost = session ? sessionCost(session) : null;
    if (cost != null) {
      total += cost;
      priced = true;
    }
  }
  return priced ? total : null;
}

/**
 * A cost estimate for `repos` runs of `swarm` hunters: the average finished run scaled to the swarm,
 * else the average colony's spend times the hunters. `basis` says which, so the wizard can be honest.
 */
export function estimateCost(
  runs: RedTeamRun[],
  sessions: Session[],
  swarm: number,
  repos: number,
): { low: number; basis: "runs" | "colonies" } | null {
  const perHunter: number[] = [];
  for (const run of runs) {
    if (run.state !== "done" || run.hunters.length === 0) continue;
    const cost = runCost(run, sessions);
    if (cost != null) perHunter.push(cost / run.hunters.length);
  }
  if (perHunter.length > 0) {
    const avg = perHunter.reduce((a, b) => a + b, 0) / perHunter.length;
    return { low: avg * swarm * repos, basis: "runs" };
  }
  const colony = sessions.map(sessionCost).filter((c): c is number => c != null && c > 0);
  if (colony.length === 0) return null;
  const avg = colony.reduce((a, b) => a + b, 0) / colony.length;
  return { low: avg * swarm * repos, basis: "colonies" };
}
