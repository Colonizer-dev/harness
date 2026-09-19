// What the inbox and the history timeline show, derived from the colony list alone.
//
// The prototype drew both from an event log. The mothership keeps no such log for the browser —
// `GET /api/sessions` is the whole story — so every entry here is a reading of a colony's *current*
// state stamped with its `updated_at`, not a record of something that happened. That is why there is
// one entry per colony rather than one per transition: inventing a history the API cannot support
// would put words in the mothership's mouth.
import { needsYou } from "../notifications";
import { colonyLabel } from "../notifications";
import { orgOf } from "../components/ui";
import type { Session } from "../types";

export type FeedKind = "question" | "returned" | "failed" | "launched" | "queued" | "stopped";

export interface FeedEntry {
  /** The colony this reads; the row opens it. */
  id: string;
  kind: FeedKind;
  /** One plain line, in the colony's own voice. */
  text: string;
  /** `owner/repo #12`, the address the notifications use. */
  label: string;
  /** The colony's `updated_at`, which is as close to "when" as the API gets. */
  at: string;
  prUrl: string | null;
}

/** The timeline's filter pills. */
export type HistoryFilter = "all" | "launches" | "questions" | "returned";

const KIND_FOR_FILTER: Record<Exclude<HistoryFilter, "all">, readonly FeedKind[]> = {
  launches: ["launched", "queued", "stopped"],
  questions: ["question"],
  returned: ["returned", "failed"],
};

/** Which kind of line a colony is on right now. `needsYou` wins over status: that is the one thing worth interrupting for. */
export function feedKind(session: Session): FeedKind {
  if (needsYou(session)) return "question";
  switch (session.status) {
    case "pr_opened":
    case "merged":
    case "closed":
    case "no_changes":
      return "returned";
    case "failed":
      return "failed";
    case "stopped":
      return "stopped";
    case "queued":
      return "queued";
    default:
      return "launched";
  }
}

function textFor(session: Session, kind: FeedKind): string {
  const short = session.repo.split("/")[1] ?? session.repo;
  const at = session.issue != null ? `${short}#${session.issue}` : short;
  if (kind === "question") {
    // The watchdog's reason is more useful than "waiting" when it is the one that flagged the colony.
    if (session.attention?.reason === "stalled") return `${at} has stopped making progress`;
    if (session.attention?.reason === "nudges_exhausted") return `${at} is still stalled after being nudged`;
    if (session.attention?.reason === "autopilot_held") return `${at} finished, and autopilot is holding the pull request`;
    return `${at} is waiting on your answer`;
  }
  switch (session.status) {
    case "pr_opened":
      return `${at} returned a pull request`;
    case "merged":
      return `${at} was merged`;
    case "closed":
      return `${at} had its pull request closed`;
    case "no_changes":
      return `${at} finished with nothing to change`;
    case "failed":
      return `${at} failed`;
    case "stopped":
      return `${at} was stopped · the worktree is kept`;
    case "queued":
      // A stacked colony queues on its parent, not on a parallelism slot — the one thing worth
      // saying differently about the same status.
      return session.parent ? `${at} waits for the colony it is stacked on` : `${at} waits for a free slot`;
    case "publishing":
      return `${at} is opening its pull request`;
    case "starting":
      return `${at} is digging in`;
    default:
      return `${at} is working`;
  }
}

export function feedEntry(session: Session): FeedEntry {
  const kind = feedKind(session);
  return {
    id: session.id,
    kind,
    text: textFor(session, kind),
    label: colonyLabel(session.repo, session.issue),
    at: session.updated_at,
    prUrl: session.pr_url,
  };
}

/** Newest first; ties fall through to `id` so the order holds between polls. */
export function feedEntries(sessions: Session[]): FeedEntry[] {
  return sessions
    .map(feedEntry)
    .sort((a, b) => Date.parse(b.at) - Date.parse(a.at) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

export function matchesFilter(kind: FeedKind, filter: HistoryFilter): boolean {
  return filter === "all" || KIND_FOR_FILTER[filter].includes(kind);
}

const MONTHS = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"] as const;

/**
 * The heading a timestamp sits under. Built from the local calendar date rather than an elapsed
 * count, so an entry from 23:50 last night says YESTERDAY at 00:10 rather than "8h".
 * Spelled out here instead of through toLocaleDateString so the label cannot shift with the locale.
 */
export function dayLabel(at: string, now: Date): string {
  const then = new Date(at);
  if (Number.isNaN(then.getTime())) return "EARLIER";
  const midnight = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((midnight(now) - midnight(then)) / 86_400_000);
  if (days <= 0) return "TODAY";
  if (days === 1) return "YESTERDAY";
  return `${then.getDate()} ${MONTHS[then.getMonth()]}`;
}

export interface HistoryRow {
  /** The day heading this entry opens, or null when it sits under the one above. */
  day: string | null;
  entry: FeedEntry;
}

/** The timeline: filtered, newest first, each entry told whether it starts a new day. */
export function historyRows(sessions: Session[], filter: HistoryFilter, now: Date): HistoryRow[] {
  let last: string | null = null;
  return feedEntries(sessions)
    .filter((entry) => matchesFilter(entry.kind, filter))
    .map((entry) => {
      const label = dayLabel(entry.at, now);
      const day = label === last ? null : label;
      last = label;
      return { day, entry };
    });
}

/** How many colonies of each org are waiting on a person; the rail's and header's dots read this. */
export function needCountByOrg(sessions: Session[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const session of sessions) {
    if (!needsYou(session)) continue;
    const org = orgOf(session);
    counts[org] = (counts[org] ?? 0) + 1;
  }
  return counts;
}

/**
 * The overview's one-line summary: what wants a person first, then what is simply running.
 * Here rather than in the view because it is the only prose in it that has rules.
 */
export function headlineFor(need: number, live: number): string {
  if (need > 0) return `${need} ${need === 1 ? "colony needs" : "colonies need"} you · ${live} working`;
  return live > 0 ? `All quiet · ${live} working` : "All quiet";
}
