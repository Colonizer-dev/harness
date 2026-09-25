// The panel beside the nest on the home view while no colony is picked: what this workspace's
// colonies are doing — state, attention, cost, and what moved last — read off the same scoped list
// the nest draws. Everything is computed from that list; nothing here fetches.
//
// With no workspace chosen (the org filter cleared) it is titled "All workspaces" and aggregates
// across every one of them: it deliberately mirrors whatever the nest shows in that scope, rather
// than picking a workspace of its own.
import { type ReactElement } from "react";

import { SESSION_STATUS, attentionText, timeAgo, type Tone } from "../components/ui";
import { colonyLabel, needsYou } from "../notifications";
import { formatCost, sessionCost, sumCosts } from "../spend";
import type { Session } from "../types";
import { OrgTile } from "./DashChart";
import { overviewCounts } from "./feed";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

/** How many recent-activity rows the panel keeps. */
export const RECENT_LIMIT = 5;

export interface NestDashboardSummary {
  /** Colonies whose microVM is up (`isLive`), the same set the cockpit's live count reads. */
  live: number;
  needYou: number;
  queued: number;
  /** Pull-request round-trips finished, the overview's RETURNED bucket. */
  returned: number;
  /** Failed or stopped colonies — the two terminal states worth a line of their own. */
  ended: number;
  total: number;
  /** What these colonies have spent in total; null when nothing has ever been measured. */
  spend: number | null;
  /** The needs-you colonies, longest-waiting first. */
  attention: Session[];
  /** The last colonies to move, newest first. */
  recent: Session[];
}

/** The whole panel's reading, from the scoped sessions alone — pure so the tests can pin it. */
export function nestDashboard(sessions: Session[]): NestDashboardSummary {
  // The four buckets are the overview's own counts, so the tiles can never disagree with it.
  const counts = overviewCounts(sessions);
  return {
    live: counts.live,
    needYou: counts["need you"],
    queued: counts.queued,
    returned: counts.returned,
    ended: sessions.filter((s) => s.status === "failed" || s.status === "stopped").length,
    total: sessions.length,
    spend: sumCosts(sessions.map(sessionCost)),
    // Longest-waiting first: the watchdog's own `since` when it set one, else the colony's last move.
    attention: sessions
      .filter(needsYou)
      .sort((a, b) => Date.parse(a.attention?.since ?? a.updated_at) - Date.parse(b.attention?.since ?? b.updated_at)),
    // Newest first by the colony's own activity clock; ties fall through to `id` like feedEntries,
    // so the order holds between polls.
    recent: [...sessions]
      .sort(
        (a, b) =>
          Date.parse(b.last_activity_at ?? b.updated_at) - Date.parse(a.last_activity_at ?? a.updated_at) ||
          (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
      )
      .slice(0, RECENT_LIMIT),
  };
}

function Fact({ label, value }: { label: string; value: string }): ReactElement {
  return (
    <div className="bg-bg px-3 py-2.5">
      <div className="text-[12px] text-muted lowercase first-letter:uppercase">{label}</div>
      <div className="mt-0.5 truncate text-[15px] font-semibold tracking-[-0.01em] text-text tabular-nums">{value}</div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactElement | ReactElement[] }): ReactElement {
  return (
    <div>
      <div className="mb-2 text-[13px] font-medium text-text lowercase first-letter:uppercase">{title}</div>
      {children}
    </div>
  );
}

export function NestDashboard({
  org,
  avatar,
  sessions,
  maxParallel,
  onSelect,
  onHide,
}: {
  /** The chosen workspace, or null for every workspace (the nest's own scope, whatever it is). */
  org: string | null;
  /** The workspace's avatar from /api/orgs; null falls back to the tile's initial. */
  avatar: string | null;
  /** Already filtered exactly as the nest's list is. */
  sessions: Session[];
  /** What the machine runs at once (`sandbox.max_parallel`); unknown reads the bare count. */
  maxParallel: number | null;
  /** Pick a colony: the inspector takes this panel's place. */
  onSelect: (id: string) => void;
  onHide: () => void;
}): ReactElement {
  const summary = nestDashboard(sessions);
  const states: { label: string; n: number; tone: Tone }[] = [
    { label: "Working", n: summary.live, tone: "info" },
    { label: "Need you", n: summary.needYou, tone: "accent" },
    { label: "Queued", n: summary.queued, tone: "neutral" },
    { label: "Returned", n: summary.returned, tone: "ok" },
    { label: "Failed / stopped", n: summary.ended, tone: "err" },
  ];
  return (
    <aside aria-label="workspace dashboard" className="flex w-[300px] shrink-0 flex-col overflow-hidden border-l border-border bg-bg">
      <div className="flex items-start gap-3 border-b border-border px-4 pb-3 pt-4">
        {org ? (
          <OrgTile org={org} avatar={avatar} size={34} />
        ) : (
          <span className="grid h-[34px] w-[34px] shrink-0 place-items-center rounded-full border border-border text-accent">
            <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M12 2.8 20 7.4v9.2L12 21.2 4 16.6V7.4z" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinejoin="round" />
              <circle cx="12" cy="12" r="2.6" fill="currentColor" />
            </svg>
          </span>
        )}
        <div className="min-w-0 flex-1">
          <div className="truncate font-mono text-[11.5px] text-muted">nest dashboard</div>
          <div className="mt-0.5 truncate text-[15px] font-semibold leading-tight tracking-[-0.01em]">{org ?? "All workspaces"}</div>
        </div>
        <button
          type="button"
          onClick={onHide}
          aria-label="hide dashboard"
          className="grid h-[26px] w-[26px] shrink-0 cursor-pointer place-items-center rounded-full text-faint hover:bg-panel-2 hover:text-text"
        >
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </div>

      <div className="scroll-thin flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4 pt-3.5">
        {sessions.length === 0 ? (
          <div className="rounded-md bg-panel-2 px-3 py-2 text-[12.5px] leading-snug text-muted">
            no colonies here yet — launch one and its state, cost and activity land here.
          </div>
        ) : (
          <>
            <div className="grid shrink-0 grid-cols-2 gap-px overflow-hidden border-y border-border bg-border">
              <Fact label="WORKING" value={maxParallel != null ? `${summary.live} / ${maxParallel}` : String(summary.live)} />
              <Fact label="NEED YOU" value={String(summary.needYou)} />
              <Fact label="QUEUED" value={String(summary.queued)} />
              <Fact label="SPENT" value={formatCost(summary.spend)} />
            </div>

            <Section title="NEEDS YOU">
              <div className="flex flex-col gap-0.5">
                {summary.attention.length === 0 ? (
                  <div className="rounded-md bg-panel-2 px-3 py-2 text-[12px] text-faint">nothing needs you here right now</div>
                ) : (
                  summary.attention.map((s) => (
                    <button
                      key={s.id}
                      type="button"
                      onClick={() => onSelect(s.id)}
                      className="cursor-pointer rounded-md bg-panel-2 px-2.5 py-2 text-left transition-colors hover:bg-panel-2/70"
                    >
                      <span className="block truncate text-[12.5px] font-medium">{colonyLabel(s.repo, s.issue)}</span>
                      <span className="block truncate text-[11.5px] text-warn">
                        {s.attention ? attentionText(s.attention) : "waiting for your answer"}
                      </span>
                    </button>
                  ))
                )}
              </div>
            </Section>

            <Section title="BY STATE">
              <div className="flex flex-col gap-1">
                {states.map((state) => (
                  <div key={state.label} className="flex items-center gap-2.5 text-[12px]">
                    <span aria-hidden="true" className="h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: TONE_VAR[state.tone] }} />
                    <span className="min-w-0 flex-1 truncate text-muted">{state.label}</span>
                    <span className="shrink-0 font-mono text-[11px] tabular-nums">{state.n}</span>
                  </div>
                ))}
              </div>
            </Section>

            <Section title="RECENT ACTIVITY">
              <div className="flex flex-col gap-0.5">
                {summary.recent.map((s) => (
                  <button
                    key={s.id}
                    type="button"
                    onClick={() => onSelect(s.id)}
                    title={s.issue_title}
                    className="-mx-1 flex cursor-pointer items-baseline gap-2 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-panel-2"
                  >
                    <span className="min-w-0 flex-1 truncate text-[12.5px]">{colonyLabel(s.repo, s.issue)}</span>
                    <span className="shrink-0 text-[11px]" style={{ color: TONE_VAR[SESSION_STATUS[s.status]?.tone ?? "neutral"] }}>
                      {SESSION_STATUS[s.status]?.label ?? s.status}
                    </span>
                    <span className="shrink-0 font-mono text-[10.5px] text-faint tabular-nums">{timeAgo(s.last_activity_at ?? s.updated_at)}</span>
                    <span className="w-11 shrink-0 text-right font-mono text-[11px] text-muted tabular-nums">{formatCost(sessionCost(s))}</span>
                  </button>
                ))}
              </div>
            </Section>
          </>
        )}
      </div>
    </aside>
  );
}
