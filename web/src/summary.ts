// A colony's task as the cockpit shows it: the model-written one-line summary when there is one,
// else the issue title, else the caller's fallback ("open session", "no title yet", …). The full
// issue title goes in a tooltip wherever the summary stands in for it.
import type { Session } from "./types";

type Titled = Pick<Session, "issue_title"> & { summary?: string | null };

/** What a colony is doing, in one line: summary, else issue title, else `fallback`. */
export function taskLine(session: Titled, fallback: string): string {
  const summary = session.summary?.trim();
  if (summary) return summary;
  return session.issue_title || fallback;
}

/** The tooltip for a task line: the full issue title, when the summary is what is shown. */
export function taskTooltip(session: Titled): string | undefined {
  return session.summary?.trim() && session.issue_title ? session.issue_title : undefined;
}
