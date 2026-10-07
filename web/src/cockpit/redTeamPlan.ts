// The pure half of the red-team wizard and history: turning the operator's local schedule choice into
// the UTC cadence the mothership stores, describing a cadence back in local time, and estimating what
// a run costs from the runs that came before it. Kept apart from the components so it is testable.
import { sessionCost } from "../spend";
import type { ChecklistStatus, RedTeamCadence, RedTeamPreset, RedTeamRun, Session } from "../types";

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

/** The presets a run can use, as the wizard offers them. */
export const PRESETS: { id: RedTeamPreset; name: string; blurb: string }[] = [
  {
    id: "general",
    name: "General",
    blurb: "Eight bug-hunting focus areas: edge cases, races, injection, leaks, auth, logic, silent failures and contracts.",
  },
  {
    id: "security",
    name: "Security",
    blurb: "Eight security focus areas, a deterministic pre-scan before launch that hands its leads to the matching hunter, and an operator checklist in the report.",
  },
];

/** The security preset's focus areas, in the mothership's order (redteam.rs); a pre-scan lead's `focus` indexes it. */
export const SECURITY_FOCUSES = [
  "auth on every route",
  "object-level access (IDOR)",
  "sessions, tokens and secrets",
  "input handling and injection",
  "the web boundary",
  "abuse and cost limits",
  "AI and agent safety",
  "failure and leakage",
] as const;

/** A run's preset; runs made before presets read as general. */
export function presetOf(run: { preset?: RedTeamPreset }): RedTeamPreset {
  return run.preset ?? "general";
}

/** How the report labels a checklist item. There is no "passed" state to label: code cannot prove these. */
export const CHECKLIST_LABEL: Record<ChecklistStatus, string> = {
  needs_review: "Needs review",
  not_verifiable: "Not verifiable from the repo",
};

// ---------------------------------------------------------------------------
// The repository list (#1145): which repositories have a run, and the one muted line each row shows.
// ---------------------------------------------------------------------------

const OVER = new Set<RedTeamRun["state"]>(["done", "stopped", "cancelled"]);
const HUNTER_DONE = new Set<Session["status"]>(["publishing", "pr_opened", "merged", "closed", "no_changes", "stopped", "failed"]);

/** `1 colony`, `2 colonies`: the singular for exactly one. */
export function plural(n: number, one: string, many: string = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The repository's active (not over) run, if it has one. The server refuses a second with a 409. */
export function activeRunFor(runs: RedTeamRun[], repo: string): RedTeamRun | null {
  return runs.find((r) => r.repo === repo && !OVER.has(r.state)) ?? null;
}

/** The repository's most recent finished hunt: the latest run that is over and actually launched hunters. */
export function lastHunt(runs: RedTeamRun[], repo: string): RedTeamRun | null {
  let best: RedTeamRun | null = null;
  for (const r of runs) {
    if (r.repo !== repo || !OVER.has(r.state) || (r.hunters.length === 0 && !r.started_at)) continue;
    if (!best || huntTime(r) > huntTime(best)) best = r;
  }
  return best;
}

function huntTime(run: RedTeamRun): number {
  return Date.parse(run.ended_at ?? run.started_at ?? run.created_at) || 0;
}

/** "3 d ago" / "2 h ago" / "12 min ago" / "just now". */
export function ago(ts: string, now: number = Date.now()): string {
  const minutes = Math.max(0, Math.round((now - Date.parse(ts)) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return `${Math.round(hours / 24)} d ago`;
}

/** How many of a run's hunters have ended, out of how many it has (or was sized for). */
export function huntersDone(run: RedTeamRun, sessions: Session[]): { done: number; total: number } {
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const total = run.hunters.length || run.swarm_size;
  const done = run.hunters.filter((h) => {
    const s = byId.get(h.session_id);
    return !s || HUNTER_DONE.has(s.status);
  }).length;
  return { done, total };
}

/** The line a repository's row shows when it has an active run: "run in progress · started 14:02 · 3/8 hunters done". */
export function activeLine(run: RedTeamRun, sessions: Session[]): string {
  const { done, total } = huntersDone(run, sessions);
  const gated = run.state === "armed" || run.state === "waiting";
  const started = run.started_at ?? run.created_at;
  const at = new Date(started).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  return gated
    ? `run queued · waiting for the nest to empty`
    : `run in progress · started ${at} · ${done}/${total} hunters done`;
}

/** The line for a repository with no active run: its red-team history, then when it was last pushed to. */
export function historyLine(runs: RedTeamRun[], repo: string, pushedAt: string | null, now: number = Date.now()): string {
  const last = lastHunt(runs, repo);
  const hunted = last
    ? `last hunted ${ago(last.ended_at ?? last.started_at ?? last.created_at, now)} · ${plural(last.counts.found, "finding")} (${last.counts.filed} filed)`
    : "never hunted";
  return pushedAt ? `${hunted} · pushed ${ago(pushedAt, now)}` : hunted;
}

/**
 * Sort for the picker: active runs last, then never-hunted first, then the oldest hunt first; ties
 * keep their incoming order (the most-pushed first), so the sort is stable.
 */
export function sortForRedTeam<T extends { full_name: string }>(repos: T[], runs: RedTeamRun[]): T[] {
  const rank = (r: T): [number, number] => {
    if (activeRunFor(runs, r.full_name)) return [2, 0];
    const last = lastHunt(runs, r.full_name);
    return last ? [1, huntTime(last)] : [0, 0];
  };
  return repos
    .map((r, i) => ({ r, i, k: rank(r) }))
    .sort((a, b) => a.k[0] - b.k[0] || a.k[1] - b.k[1] || a.i - b.i)
    .map((x) => x.r);
}
