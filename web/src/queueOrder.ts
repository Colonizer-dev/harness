import type { Session } from "./types";

/**
 * The order the mothership starts queued colonies in (issue #1156): highest priority first, the
 * older colony within a priority. Within one org the org's own `queue_priority` is the same for
 * every colony, so a colony's own `priority` (absent is 0) is all that tells them apart. The
 * starvation guard is the server's call and is not guessed at here.
 */
export function queueOrder(sessions: readonly Session[]): Session[] {
  return sessions
    .filter((s) => s.status === "queued")
    .sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0) || Date.parse(a.created_at) - Date.parse(b.created_at));
}

/** The queued colony that starts next in this list, or null when none is queued. */
export function nextUp(sessions: readonly Session[]): Session | null {
  return queueOrder(sessions)[0] ?? null;
}

/** `repo#issue` (or the bare repository name for an open colony), the short name the cockpit uses for a colony. */
export function shortName(session: Session): string {
  const repo = session.repo.split("/")[1] ?? session.repo;
  return session.issue != null ? `${repo}#${session.issue}` : repo;
}
