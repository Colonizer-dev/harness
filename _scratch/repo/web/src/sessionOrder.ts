// Sidebar ordering: what the colony wants from you first, then recency inside each group.
import { needsYou } from "./notifications";
import type { Session, SessionStatus } from "./types";

/**
 * Sort group per status, lowest first: needs an answer, then live colonies, then a PR in
 * flight, then the queue, then everything finished. Exhaustive over SessionStatus, so a new
 * status has to pick a group instead of silently sorting last.
 */
const RANK: Record<SessionStatus, number> = {
  waiting_for_answer: 0,
  starting: 1,
  running: 1,
  idle: 1,
  publishing: 2,
  queued: 3,
  pr_opened: 4,
  merged: 4,
  closed: 4,
  no_changes: 4,
  stopped: 4,
  failed: 4,
};

/** A colony that needs a person joins the unanswered at the top, whatever its status — same predicate the notifications read. */
export function sessionRank(session: Session): number {
  return needsYou(session) ? 0 : RANK[session.status];
}

/** Newest first inside a group; ties fall through to `id` so the order holds between polls. */
export function sortSessions(sessions: Session[]): Session[] {
  return [...sessions].sort(
    (a, b) =>
      sessionRank(a) - sessionRank(b) ||
      Date.parse(b.updated_at) - Date.parse(a.updated_at) ||
      (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}
