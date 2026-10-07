// A colony's commits (issue #765): the links the mothership keeps through rebase, amend and
// force-push. A link it could not re-point unambiguously is kept and marked orphaned, never guessed.
import type { ReactElement } from "react";

import { Badge } from "../components/ui";
import type { CommitLink } from "../types";

export const ORPHANED_TOOLTIP =
  "A squash or rewrite left no single commit with this change, so the match was ambiguous; the link was kept rather than guessed.";

export function CommitLinks({ commits }: { commits: CommitLink[] }): ReactElement {
  if (commits.length === 0) {
    return <div className="rounded-md bg-panel-2 px-3 py-2 text-small text-faint">no commits recorded yet</div>;
  }
  return (
    <div className="flex flex-col gap-1.5">
      {commits.map((c) => (
        <div key={c.sha} className="flex items-center gap-2.5 text-small text-muted">
          <span className="font-mono text-meta text-text" title={c.sha}>
            {c.sha.slice(0, 7)}
          </span>
          {c.previous.length > 0 && (
            <span className="min-w-0 flex-1 truncate font-mono text-meta text-faint" title={c.previous.join(" → ")}>
              was {c.previous[c.previous.length - 1].slice(0, 7)}
            </span>
          )}
          {c.orphaned && (
            <Badge tone="warn" title={ORPHANED_TOOLTIP} className="ml-auto">
              orphaned
            </Badge>
          )}
        </div>
      ))}
    </div>
  );
}
