// A launch refused as a duplicate (issue #832): who already holds the work, with a link to that
// colony or its pull request — the mothership's 409 `duplicate`, shown where the launch was made.
// The way past it, Allow duplicate, is the launch form's own checkbox beside this notice.
import type { ReactElement } from "react";

import type { DuplicateHolder } from "../types";

/** The holder in one plain sentence, for a toast or a screen reader. */
export function describeHolder(holder: DuplicateHolder): string {
  const who = holder.colony ? `colony ${holder.colony}` : "a colony";
  const where = holder.host ? ` on ${holder.host}` : "";
  const state = holder.pr_url ? "its pull request is open" : holder.status ? `it is ${holder.status.replace(/_/g, " ")}` : null;
  return `${holder.what} is already being done by ${who}${where}${state ? ` (${state})` : ""}.`;
}

export function DuplicateNotice({ holder, onOpen }: { holder: DuplicateHolder; onOpen?: () => void }): ReactElement {
  const remote = holder.kind === "remote_claim";
  return (
    <p role="status" className="rounded-lg border border-warn/40 bg-warn-soft px-2.5 py-2 text-[12.5px] text-muted [overflow-wrap:anywhere]">
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
