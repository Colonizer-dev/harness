// Issue-claim helpers (issue #827): the client-side mirror of the mothership's issue-holding rules.
import type { Session, SessionStatus } from "../sessions/types";
import type { Issue } from "./types";

/**
 * Whether a colony in this status holds its issue against a second launch — the cockpit mirror
 * of the mothership's `holds_issue` (crates/colonizer/src/sessions.rs): queued, live,
 * publishing, or with its pull request still open. Stopped, failed, no_changes, merged and
 * closed leave the issue free for a retry.
 */
export function holdsIssue(status: SessionStatus): boolean {
  return (
    status === "queued" ||
    status === "starting" ||
    status === "running" ||
    status === "waiting_for_answer" ||
    status === "idle" ||
    status === "publishing" ||
    status === "pr_opened"
  );
}

/**
 * The colony already holding `(repo, issue)`, if any — the launch that POST /api/sessions would
 * refuse with a 409. The mirror of the mothership's `issue_held_by`: the first holding colony that
 * is not a `claim_wait` waiter, else — once the holder is gone and only waiters remain — the oldest
 * waiter by `created_at` (issue #321), never a later-arriving waiter.
 */
export function heldByFor(sessions: Session[], repo: string, issue: number): Session | null {
  const holding = (s: Session) => s.repo === repo && s.issue === issue && holdsIssue(s.status);
  return (
    sessions.find((s) => holding(s) && !s.claim_wait) ??
    sessions.filter(holding).sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))[0] ??
    null
  );
}

/** The client-side mirror of the mothership's epic check (epic.rs), for lists: the list's own `epic` marker, else an `epic` label or a title ending "(epic)" / starting "Epic:". */
export function isEpic(issue: Pick<Issue, "title" | "labels" | "epic">): boolean {
  if (issue.epic) return true;
  const title = issue.title.trim().toLowerCase();
  return issue.labels.some((l) => l.name.trim().toLowerCase() === "epic") || title.endsWith("(epic)") || title.startsWith("epic:");
}

/** The marker an epic carries in an issue list: "Epic · 5 sub-issues", or just "Epic"; null for any other issue. */
export function epicMarker(issue: Pick<Issue, "title" | "labels" | "epic">): string | null {
  if (!isEpic(issue)) return null;
  const n = issue.epic?.sub_issues ?? 0;
  return n > 0 ? `Epic · ${n} sub-issue${n === 1 ? "" : "s"}` : "Epic";
}

/** The issues of a batch launch another colony already holds, in the order given — each one a 409 waiting to happen. */
export function heldInBatch(sessions: Session[], repo: string, issues: Iterable<number>): number[] {
  return [...issues].filter((issue) => heldByFor(sessions, repo, issue) !== null);
}

/** The `claim_wait` colonies waiting on `(repo, issue)`, oldest first — the issue's successor queue. */
export function claimWaitersFor(sessions: Session[], repo: string, issue: number): Session[] {
  return sessions
    .filter((s) => s.repo === repo && s.issue === issue && s.claim_wait && s.status === "queued")
    .sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at));
}

/**
 * Where a colony stands in its issue's successor queue — 1 for the oldest waiter, the side the
 * mothership takes over first. null for anything not a queued `claim_wait` colony, which reads as
 * an ordinary queued entry rather than a line position.
 */
export function claimWaitPosition(sessions: Session[], session: Session): number | null {
  if (!session.claim_wait || session.status !== "queued" || session.issue === null) return null;
  const at = claimWaitersFor(sessions, session.repo, session.issue).findIndex((s) => s.id === session.id);
  return at < 0 ? null : at + 1;
}
