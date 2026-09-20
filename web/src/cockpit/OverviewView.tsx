// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". The rail's base button lands here.
//
// The prototype's rows carry a settler count. This one does not: the mothership streams events for a
// single colony at a time, so the only honest per-colony facts here are the ones in the list itself.
import { useState, type ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { SESSION_STATUS, cx, isLive, type Tone, orgOf, sameOrg, timeAgo } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { OVERVIEW_FILTERS, headlineFor, overviewCounts, overviewSessions, type OverviewFilter } from "./feed";
import type { Session } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

/** The counter's number keeps its accent per bucket; the "need you" one dims at zero, as it always did. */
const COUNT_COLOR: Record<OverviewFilter, (count: number) => string> = {
  live: () => "text-accent",
  "need you": (count) => (count > 0 ? "text-warn" : "text-muted"),
  returned: () => "text-ok",
  queued: () => "text-text",
};

export function OverviewView({
  sessions,
  orgs,
  cost,
  onOpenOrg,
  onOpenColony,
}: {
  /** Every colony the mothership knows, unfiltered — this page is the cross-workspace view. */
  sessions: Session[];
  orgs: OrgEntry[];
  cost: number | null;
  onOpenOrg: (org: string) => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  // The filter lives here, not in the cockpit: toggling a counter narrows the page, and a second
  // click on the active one (or the counts themselves) clears it. State is per-visit on purpose.
  const [filter, setFilter] = useState<OverviewFilter | null>(null);
  // Counts come from the whole list, never the filtered one, so they keep moving on the 4s poll.
  const counts = overviewCounts(sessions);
  const shown = overviewSessions(sessions, filter);

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-10 pt-7">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-5">
        <div className="flex flex-wrap items-baseline justify-between gap-4">
          <div>
            <div className="mb-1.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">OVERVIEW</div>
            <div className="text-[20px] font-semibold tracking-tight">{headlineFor(counts["need you"], counts.live)}</div>
          </div>
          <div className="flex flex-wrap items-center justify-end gap-1.5 font-mono text-xs tabular-nums">
            {OVERVIEW_FILTERS.map((name) => {
              const count = counts[name];
              const active = filter === name;
              return (
                <button
                  key={name}
                  type="button"
                  aria-pressed={active}
                  onClick={() => setFilter(active ? null : name)}
                  className={cx(
                    "cursor-pointer rounded-full border px-2.5 py-1 text-[11.5px] font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-ring)]",
                    active ? "border-accent bg-accent-soft text-text" : "border-border text-muted hover:border-accent hover:text-text",
                  )}
                >
                  <span className={COUNT_COLOR[name](count)}>{count}</span> {name}
                </button>
              );
            })}
            {cost !== null && (
              <span className="whitespace-nowrap px-1" title="what every colony has spent in total">
                ${cost.toFixed(2)} spent
              </span>
            )}
          </div>
        </div>

        {filter && shown.length === 0 ? (
          <div className="rounded-2xl border border-border bg-panel px-4 py-3.5 text-[13px] text-muted">
            nothing here under this filter
          </div>
        ) : (
          <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fill,minmax(300px,1fr))]">
            {orgs.map((org) => {
              const mine = sortSessions(shown.filter((s) => sameOrg(orgOf(s), org.org)));
              // Under a filter, an org with no matching colonies drops out entirely.
              if (filter && mine.length === 0) return null;
              return (
                <section key={org.org} className="flex flex-col overflow-hidden rounded-2xl border border-border bg-panel">
                  <button
                    type="button"
                    onClick={() => onOpenOrg(org.org)}
                    className="grid cursor-pointer grid-cols-[30px_minmax(0,1fr)_auto] items-center gap-2.5 border-b border-border px-3.5 py-3 text-left hover:bg-panel-2"
                  >
                    <Avatar name={org.org} src={org.avatar} size={30} rounded="lg" />
                    <span className="min-w-0">
                      <span className="block truncate font-semibold">{org.org}</span>
                      <span className="block font-mono text-[11px] text-faint">
                        {mine.length} {mine.length === 1 ? "colony" : "colonies"} ·{" "}
                        {/* Under a filter the card counts the rows it actually shows; unfiltered, the org's own tally. */}
                        {filter ? mine.filter((s) => isLive(s.status)).length : org.live} live
                      </span>
                    </span>
                    <span className="whitespace-nowrap font-mono text-[11px] text-accent">open nest →</span>
                  </button>

                  <div className="flex flex-col">
                    {mine.length === 0 ? (
                      <div className="p-3.5 text-[12px] text-faint">No colonies yet.</div>
                    ) : (
                      mine.map((session) => {
                        const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
                        const edge = TONE_VAR[tone];
                        const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
                        return (
                          <button
                            key={session.id}
                            type="button"
                            onClick={() => onOpenColony(session.id)}
                            className="grid cursor-pointer grid-cols-[8px_minmax(0,1fr)_auto] items-center gap-2.5 border-b border-border px-3.5 py-2.5 text-left last:border-b-0 hover:bg-panel-2"
                          >
                            <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: edge }} />
                            <span className="min-w-0">
                              <span className="block truncate text-[13px]">{session.issue_title || short}</span>
                              <span className="block truncate font-mono text-[11px] text-faint">
                                {short} · {timeAgo(session.last_activity_at ?? session.updated_at)}
                              </span>
                            </span>
                            <span className="whitespace-nowrap font-mono text-[11px]" style={{ color: edge }}>
                              {SESSION_STATUS[session.status]?.label ?? session.status}
                            </span>
                          </button>
                        );
                      })
                    )}
                  </div>
                </section>
              );
            })}
          </div>
        )}
      </div>
    </main>
  );
}
