// A launch refused as a duplicate (issue #832): who already holds the work, with a link to that
// colony or its pull request — the mothership's 409 `duplicate`, shown where the launch was made.
// The way past it, Allow duplicate, is the launch form's own checkbox beside this notice.
import type { ReactElement } from "react";

import type { DuplicateHolder } from "../types";

/** Relative time formatter for RFC3339 timestamps. */
function relativeTime(rfc3339: string): string {
  try {
    const date = new Date(rfc3339);
    const now = new Date();
    const seconds = Math.floor((now.getTime() - date.getTime()) / 1000);

    if (seconds < 60) return "just now";
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
    if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
    if (seconds < 604800) return `${Math.floor(seconds / 86400)}d ago`;
    return date.toLocaleDateString();
  } catch {
    return rfc3339;
  }
}

/** The holder in one plain sentence, for a toast or a screen reader. */
export function describeHolder(holder: DuplicateHolder): string {
  const who = holder.colony ? `colony ${holder.colony}` : "a colony";
  const where = holder.host ? ` on ${holder.host}` : "";
  const state = holder.pr_url ? "its pull request is open" : holder.status ? `it is ${holder.status.replace(/_/g, " ")}` : null;
  return `${holder.what} is already being done by ${who}${where}${state ? ` (${state})` : ""}.`;
}

export function DuplicateNotice({
  holder,
  onOpen,
  onTakeOver,
}: {
  holder: DuplicateHolder;
  onOpen?: () => void;
  onTakeOver?: () => void;
}): ReactElement {
  const remote = holder.kind === "remote_claim";
  const isStale = remote && holder.stale_hint;

  if (isStale && onTakeOver) {
    // Show "label-only claim" action for claims with no branch or PR
    const claimedBy = holder.claimed_by ? `@${holder.claimed_by}` : "unknown account";
    const claimedWhen = holder.claimed_at ? relativeTime(holder.claimed_at) : "unknown time";
    return (
      <div role="status" className="rounded-lg border border-warn/40 bg-warn-soft px-2.5 py-2 text-small-lg text-muted [overflow-wrap:anywhere]">
        <p className="mb-2">
          <span className="font-medium text-text">{holder.what}</span> is claimed by{" "}
          <span className="font-mono">{claimedBy}</span> ({claimedWhen}).
        </p>
        <p>
          No branch or pull request; the claim may be stale. Press{" "}
          <button
            type="button"
            onClick={onTakeOver}
            className="cursor-pointer font-medium text-accent hover:underline"
          >
            Take over
          </button>{" "}
          to proceed, or{" "}
          <span className="font-medium">Allow duplicate</span> to ignore it.
        </p>
      </div>
    );
  }

  return (
    <p role="status" className="rounded-lg border border-warn/40 bg-warn-soft px-2.5 py-2 text-small-lg text-muted [overflow-wrap:anywhere]">
      <span className="font-medium text-text">{holder.what}</span> is already being done by{" "}
      {holder.colony && onOpen && !remote ? (
        <button type="button" onClick={onOpen} className="cursor-pointer font-mono text-accent hover:underline">
          {holder.colony}
        </button>
      ) : holder.colony ? (
        <span className="font-mono">{holder.colony}</span>
      ) : (
        "a colony"
      )}
      {holder.host ? <> on {holder.host}</> : null}
      {holder.claimed_by && remote ? (
        <>
          {" "}
          (claimed by <span className="font-mono">@{holder.claimed_by}</span>
          {holder.claimed_at ? ` ${relativeTime(holder.claimed_at)}` : ""})
        </>
      ) : null}
      {holder.status || holder.pr_url ? " (" : null}
      {holder.status ? holder.status.replace(/_/g, " ") : null}
      {holder.status && holder.pr_url ? ", " : null}
      {holder.pr_url ? (
        <a href={holder.pr_url} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
          pull request
        </a>
      ) : null}
      {holder.status || holder.pr_url ? ")" : null}. Check <span className="font-medium">Allow duplicate</span> to start another anyway
      {holder.queueable ? (
        <>
          , or <span className="font-medium">Wait behind the holder</span>
        </>
      ) : null}
      .
    </p>
  );
}
