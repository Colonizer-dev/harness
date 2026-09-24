// What the inbox shows, derived from the colony list alone: every entry is a reading of a colony's
// *current* state stamped with its `updated_at`, one entry per colony. History does not read these:
// `updated_at` moves on every housekeeping write, so it reads the activity log instead (history.ts).
import { needsYou } from "../notifications";
import { colonyLabel } from "../notifications";
import { isLive, occupiesSlot, orgOf, sameOrg } from "../components/ui";
import type { Session, SessionStatus } from "../types";

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

/** Which kind of line a colony is on right now. `needsYou` wins over status: that is the one thing worth interrupting for. */
export function feedKind(session: Session): FeedKind {
  if (needsYou(session)) return "question";
  // The same round-trip statuses the overview's RETURNED bucket counts, read from that one set so
  // the timeline and the counter can never name a different "returned".
  if (RETURNED.has(session.status)) return "returned";
  switch (session.status) {
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

export function textFor(session: Session, kind: FeedKind): string {
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

// ---------------------------------------------------------------------------
// The overview's buckets. The counters at the top of OVERVIEW and the click-filters
// read these same predicates, so the number a counter shows can never disagree with
// the colonies the corresponding filter reveals.
// ---------------------------------------------------------------------------

/** The four buckets the overview counts and filters on. */
export type OverviewFilter = "live" | "need you" | "returned" | "queued";

/** The order the counters render in. */
export const OVERVIEW_FILTERS: readonly OverviewFilter[] = ["live", "need you", "returned", "queued"];

/** "Returned" as the overview draws it: the statuses that end a pull-request round-trip. */
export const RETURNED: ReadonlySet<SessionStatus> = new Set(["pr_opened", "merged", "closed", "no_changes"]);

/** Whether a colony belongs to the overview bucket `filter`. */
export function matchesOverviewFilter(session: Session, filter: OverviewFilter): boolean {
  switch (filter) {
    case "live":
      return isLive(session.status);
    case "need you":
      return needsYou(session);
    case "returned":
      return RETURNED.has(session.status);
    case "queued":
      return session.status === "queued";
  }
}

/** A colony holding a parallel slot without doing work: idle while autopilot holds its pull request. */
export function isHeld(session: Session): boolean {
  return session.status === "idle" && session.attention?.reason === "autopilot_held";
}

export interface HeldSlots {
  count: number;
  /** The earliest `attention.since` among the held colonies, or null when none is held. */
  oldestSince: string | null;
  /** `now` minus `oldestSince`, or null when none is held. */
  oldestAgeMs: number | null;
}

/**
 * How many parallel slots idle-but-held colonies occupy, and how long the longest has waited.
 * `now` is a parameter so the tests can pin the age.
 */
export function heldSlots(sessions: Session[], now: number | Date = Date.now()): HeldSlots {
  let count = 0;
  let oldest: string | null = null;
  for (const session of sessions) {
    const attention = session.attention;
    if (session.status !== "idle" || attention?.reason !== "autopilot_held") continue;
    count += 1;
    if (attention.since && (oldest === null || Date.parse(attention.since) < Date.parse(oldest))) oldest = attention.since;
  }
  const at = typeof now === "number" ? now : now.getTime();
  const oldestAgeMs = oldest === null || Number.isNaN(Date.parse(oldest)) ? null : Math.max(0, at - Date.parse(oldest));
  return { count, oldestSince: oldest, oldestAgeMs };
}

/**
 * Whether the queue cannot drain: something waits for a slot, at least one colony occupies a
 * slot, and every slot-occupying colony is held — so no slot is doing work and nothing will free
 * one until a hold times out and parks its colony. Reads `occupiesSlot`, not `isLive`: a colony
 * mid-publish holds its slot though its microVM is gone, and an active publish will free one.
 */
export function queueStalled(sessions: Session[]): boolean {
  if (!sessions.some((session) => session.status === "queued")) return false;
  const occupying = sessions.filter((session) => occupiesSlot(session.status));
  return occupying.length > 0 && occupying.every(isHeld);
}

/** The overview's counts over the whole list — a filter narrows the page, never these, so they keep updating on the 4s poll while one is active. */
export function overviewCounts(sessions: Session[]): Record<OverviewFilter, number> {
  const counts: Record<OverviewFilter, number> = { live: 0, "need you": 0, returned: 0, queued: 0 };
  for (const session of sessions) {
    counts.live += matchesOverviewFilter(session, "live") ? 1 : 0;
    counts["need you"] += matchesOverviewFilter(session, "need you") ? 1 : 0;
    counts.returned += matchesOverviewFilter(session, "returned") ? 1 : 0;
    counts.queued += matchesOverviewFilter(session, "queued") ? 1 : 0;
  }
  return counts;
}

/** The rows the overview shows: everything when `filter` is null (a second click clears it), else exactly the bucket's colonies. */
export function overviewSessions(sessions: Session[], filter: OverviewFilter | null): Session[] {
  return filter ? sessions.filter((s) => matchesOverviewFilter(s, filter)) : sessions;
}

/**
 * The colonies the overview may show: the page renders one card per entry of its `orgs` prop
 * (the visible workspaces), so a colony whose org is not among them — a switched-off org — has
 * no card. The view counts this set and names the rest, instead of showing bare global numbers
 * over a list that cannot contain them (issue #246).
 */
export function overviewVisibleSessions(sessions: Session[], orgs: readonly { org: string }[]): Session[] {
  return sessions.filter((session) => orgs.some((entry) => sameOrg(orgOf(session), entry.org)));
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

/**
 * How many colonies of each org are waiting on a person; the rail's and header's dots read this.
 * Keyed by the lowercased org, as sameOrg compares: a colony's org and the /api/orgs spelling of
 * the same org need not agree on case. Read it through needFor.
 */
export function needCountByOrg(sessions: Session[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const session of sessions) {
    if (!needsYou(session)) continue;
    const org = orgOf(session).toLowerCase();
    counts[org] = (counts[org] ?? 0) + 1;
  }
  return counts;
}

/** One org's count from needCountByOrg, whatever the case of the name it is asked with. */
export function needFor(counts: Record<string, number>, org: string): number {
  return counts[org.toLowerCase()] ?? 0;
}

/**
 * The overview's one-line summary: what wants a person first, then what is simply running.
 * Here rather than in the view because it is the only prose in it that has rules.
 */
export function headlineFor(need: number, live: number): string {
  if (need > 0) return `${need} ${need === 1 ? "colony needs" : "colonies need"} you · ${live} working`;
  return live > 0 ? `All quiet · ${live} working` : "All quiet";
}
