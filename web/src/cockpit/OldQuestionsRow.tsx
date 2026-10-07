// One row for every question that was abandoned more than 72 hours ago (issue #1140): they stay
// in the colony list, but a page of three-day-old entries is noise, so Needs you folds them into
// a single line with Dismiss all, which marks each failure seen.
import { useContext, useState, type ReactElement } from "react";

import { ApiContext } from "../context";
import type { Session } from "../types";

export function OldQuestionsRow({ sessions, compact = false }: { sessions: Session[]; compact?: boolean }): ReactElement | null {
  const api = useContext(ApiContext);
  const [busy, setBusy] = useState(false);
  if (sessions.length === 0) return null;
  const dismissAll = async () => {
    if (!api) return;
    setBusy(true);
    try {
      await Promise.allSettled(sessions.map((s) => api.seenSession(s.id)));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div
      className={`flex items-center gap-2.5 border-y border-border ${compact ? "px-4 py-3" : "py-3.5"} text-body-sm text-muted`}
      data-testid="old-questions"
    >
      <span aria-hidden="true" className="h-[7px] w-[7px] shrink-0 rounded-full bg-faint" />
      <span className="min-w-0 flex-1">
        {sessions.length} old question{sessions.length === 1 ? "" : "s"} · unanswered for over 3 days
      </span>
      <button
        type="button"
        disabled={busy || !api}
        onClick={() => void dismissAll()}
        className="shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-2.5 py-1 text-small-lg font-medium text-text hover:bg-panel-2 disabled:cursor-default disabled:opacity-60"
      >
        Dismiss all
      </button>
    </div>
  );
}
