// The timeline: every colony in the workspace, newest move first, grouped by day.
//
// This is a standing of the colony list, not a log — `updated_at` is the only "when" the API gives,
// so a colony appears once, on the line it is on now. The filters narrow that set; they do not
// reach back for transitions nobody recorded.
import { useState, type ReactElement } from "react";

import type { Session } from "../types";
import { historyRows, type HistoryFilter } from "./feed";
import { KIND_DOT } from "./InboxView";

const FILTERS: HistoryFilter[] = ["all", "launches", "questions", "returned"];

export function HistoryView({
  sessions,
  org,
  onOpenColony,
}: {
  sessions: Session[];
  org: string | null;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const [filter, setFilter] = useState<HistoryFilter>("all");
  const rows = historyRows(sessions, filter, new Date());

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-20 pt-10">
      <div className="mx-auto w-full max-w-[1080px]">
        <div className="mb-8 flex flex-wrap items-end justify-between gap-4">
          <div>
            <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">History</h1>
            <div className="mt-2 text-[14px] text-muted">{org ?? "All workspaces"}</div>
          </div>
          <div role="group" aria-label="history filter" className="flex flex-wrap gap-0.5 rounded-lg border border-border p-0.5">
            {FILTERS.map((name) => (
              <button
                key={name}
                type="button"
                aria-pressed={filter === name}
                onClick={() => setFilter(name)}
                className={`cursor-pointer rounded-md border-0 px-2.5 py-1 text-[12.5px] ${filter === name ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text"}`}
              >
                {name}
              </button>
            ))}
          </div>
        </div>

        {rows.length === 0 ? (
          <div className="border-y border-border py-3.5 text-[13px] text-muted">
            nothing here yet under this filter
          </div>
        ) : (
          <div className="relative pl-5.5">
            <div aria-hidden="true" className="absolute bottom-1.5 left-[5px] top-1.5 w-px bg-border" />
            {rows.map(({ day, entry }) => (
              <div key={entry.id}>
                {day && (
                  <div className="relative -ml-5.5 bg-bg pb-2 pt-4 text-[13px] font-medium text-muted">
                    {day}
                  </div>
                )}
                <button
                  type="button"
                  onClick={() => onOpenColony(entry.id)}
                  className="relative grid w-full cursor-pointer grid-cols-[52px_minmax(0,1fr)_auto] items-baseline gap-3 rounded-md border-0 bg-transparent py-2 pr-3 text-left text-text hover:bg-panel-2"
                >
                  <span
                    aria-hidden="true"
                    className="absolute -left-[21px] top-3.5 h-[7px] w-[7px] rounded-full shadow-[0_0_0_3px_var(--bg)]"
                    style={{ background: KIND_DOT[entry.kind] }}
                  />
                  <span className="font-mono text-[11px] text-faint">{clock(entry.at)}</span>
                  <span className="min-w-0">
                    <span className="block truncate text-[13.5px]">{entry.text}</span>
                    <span className="mt-0.5 block truncate font-mono text-[11px] text-faint">{entry.label}</span>
                  </span>
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </main>
  );
}

/**
 * The wall-clock time the entry sits at. Forced to 24h: a locale that appends AM/PM wraps the
 * column onto a second line, and the timeline reads as a log anyway.
 */
function clock(at: string): string {
  const when = new Date(at);
  if (Number.isNaN(when.getTime())) return "";
  return when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}
