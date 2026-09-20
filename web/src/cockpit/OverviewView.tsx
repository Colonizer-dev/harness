// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". The rail's base button lands here.
//
// The prototype's rows carry a settler count. This one does not: the mothership streams events for a
// single colony at a time, so the only honest per-colony facts here are the ones in the list itself.
//
// Colonies collapse to one compact line apiece. The issue's title — the one fact that varies — waits
// behind a click so the page reads as a roster, not a wall of issue text. The header only toggles its
// row in place; reaching the colony's own view is the explicit "open colony →" action, so the
// overview stays one page and the full session is still one more click deep.
import { useId, useState, type ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconChevron } from "../components/icons";
import { SESSION_STATUS, type Tone, attentionText, cx, isLive, orgOf, sameOrg, timeAgo } from "../components/ui";
import { colonyLabel, needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { headlineFor } from "./feed";
import type { Session } from "../types";

const TONE_VAR: Record<Tone, string> = {
  neutral: "var(--faint)",
  info: "var(--info)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  err: "var(--err)",
  accent: "var(--accent)",
};

const RETURNED = new Set(["pr_opened", "merged", "closed", "no_changes"]);

/** Flip one colony's membership in the expanded set. Pure, so the tests can pin the toggle. */
export function flipExpanded(expanded: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(expanded);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}

/**
 * One colony on the overview. Collapsed it is a single line — status, name, age, status label — with
 * the issue title kept behind the disclosure; clicked, the title and the issue's address open beneath
 * it in place. The header is a plain disclosure button (never a navigation), and the colony's own
 * view stays one explicit "open colony →" away so the overview does not empty out on a stray click.
 */
export function ColonyRow({
  session,
  open,
  onToggle,
  onOpenColony,
}: {
  session: Session;
  /** Whether this colony's detail is revealed. */
  open: boolean;
  onToggle: (id: string) => void;
  onOpenColony: (id: string) => void;
}): ReactElement {
  const detailsId = useId();
  const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
  const edge = TONE_VAR[tone];
  const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
  const address = colonyLabel(session.repo, session.issue);
  const issueUrl = session.issue != null ? `https://github.com/${session.repo}/issues/${session.issue}` : null;
  const attention = session.attention
    ? attentionText(session.attention)
    : session.status === "waiting_for_answer"
      ? "Waiting for your answer"
      : null;

  return (
    <div className="border-b border-border last:border-b-0">
      <button
        type="button"
        onClick={() => onToggle(session.id)}
        aria-expanded={open}
        aria-controls={detailsId}
        className="grid w-full cursor-pointer grid-cols-[8px_minmax(0,1fr)_auto_auto] items-center gap-2.5 px-3.5 py-2.5 text-left hover:bg-panel-2"
      >
        <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: edge }} />
        <span className="min-w-0 truncate text-[13px]">
          {short} <span className="text-faint">· {timeAgo(session.last_activity_at ?? session.updated_at)}</span>
        </span>
        <span className="whitespace-nowrap font-mono text-[11px]" style={{ color: edge }}>
          {SESSION_STATUS[session.status]?.label ?? session.status}
        </span>
        <IconChevron size={13} className={cx("text-faint transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <div id={detailsId} className="flex flex-col gap-1.5 border-t border-border bg-panel-2 px-3.5 pb-3 pt-2.5">
          <div className="text-[13px] font-medium leading-snug">{session.issue_title || short}</div>
          <div className="flex items-center gap-2.5">
            {issueUrl ? (
              <a href={issueUrl} target="_blank" rel="noreferrer" className="font-mono text-[11.5px] text-accent hover:underline">
                {address}
              </a>
            ) : (
              <span className="font-mono text-[11.5px] text-faint">{address}</span>
            )}
            {attention && (
              <span className="flex items-center gap-1.5 whitespace-nowrap text-[11.5px] text-warn">
                <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full bg-warn" />
                {attention}
              </span>
            )}
            <button
              type="button"
              onClick={() => onOpenColony(session.id)}
              className="ml-auto cursor-pointer font-sans text-[12.5px] font-semibold text-accent"
            >
              open colony →
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

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
  // Which rows are unfolded, by session id. The 4 s poll hands this component brand-new session
  // objects, so the set is keyed on the id and lives here — a re-render must not close the row a
  // person is reading.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = (id: string) => setExpanded((rows) => flipExpanded(rows, id));
  const live = sessions.filter((s) => isLive(s.status)).length;
  const need = sessions.filter(needsYou).length;
  const returned = sessions.filter((s) => RETURNED.has(s.status)).length;
  const queued = sessions.filter((s) => s.status === "queued").length;

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-10 pt-7">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-5">
        <div className="flex flex-wrap items-baseline justify-between gap-4">
          <div>
            <div className="mb-1.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">OVERVIEW</div>
            <div className="text-[20px] font-semibold tracking-tight">{headlineFor(need, live)}</div>
          </div>
          <div className="flex gap-4.5 font-mono text-xs text-muted tabular-nums">
            <span>
              <span className="text-accent">{live}</span> live
            </span>
            <span>
              <span className={need > 0 ? "text-warn" : "text-muted"}>{need}</span> need you
            </span>
            <span>
              <span className="text-ok">{returned}</span> returned
            </span>
            <span>
              <span className="text-text">{queued}</span> queued
            </span>
            {cost !== null && <span title="what every colony has spent in total">${cost.toFixed(2)} spent</span>}
          </div>
        </div>

        <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fill,minmax(300px,1fr))]">
          {orgs.map((org) => {
            const mine = sortSessions(sessions.filter((s) => sameOrg(orgOf(s), org.org)));
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
                      {mine.length} {mine.length === 1 ? "colony" : "colonies"} · {org.live} live
                    </span>
                  </span>
                  <span className="whitespace-nowrap font-mono text-[11px] text-accent">open nest →</span>
                </button>

                <div className="flex flex-col">
                  {mine.length === 0 ? (
                    <div className="p-3.5 text-[12px] text-faint">No colonies yet.</div>
                  ) : (
                    mine.map((session) => (
                      <ColonyRow
                        key={session.id}
                        session={session}
                        open={expanded.has(session.id)}
                        onToggle={toggle}
                        onOpenColony={onOpenColony}
                      />
                    ))
                  )}
                </div>
              </section>
            );
          })}
        </div>
      </div>
    </main>
  );
}
