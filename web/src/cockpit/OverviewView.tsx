// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". The rail's base button lands here.
//
// The prototype's rows carry a settler count. This one does not: the mothership streams events for a
// single colony at a time, so the only honest per-colony facts here are the ones in the list itself.
//
// Colonies collapse to one compact line apiece. The issue's title — the one fact that varies — waits
// behind a click so the page reads as a roster, not a wall of issue text. The header only toggles its
// row in place; reaching the colony's own view is the explicit "open colony →" action, so the
// overview stays one page and the full session is still one more click deep. The colony's machine
// facts (the microVM it booted, its mesh address, its agent) live in the same disclosure: they are
// per-colony detail, and the collapsed line stays the roster.
//
// The counters at the top double as filters: a click narrows the roster to that bucket, a second
// click on the active one clears it. The buckets themselves live in feed.ts, so the number a counter
// shows and the rows its filter reveals can never disagree.
//
// Above the roster sits the host strip (issue #205): the one machine every listed colony boots on —
// its name, how many microVMs are live against the ceiling, its cores, load, memory, disk and
// uptime — so "why is nothing starting" is answerable without leaving the page. It is page-level by
// nature, orthogonal to the counters and the filter: the filter narrows the roster, never the host.
import { useId, useState, type ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconAlert, IconChevron, IconCpu, IconMemory, IconServer } from "../components/icons";
import { SESSION_STATUS, type Tone, attentionText, cx, isLive, orgOf, sameOrg, timeAgo } from "../components/ui";
import { colonyLabel, needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { FleetPanel } from "./FleetPanel";
import { OVERVIEW_FILTERS, headlineFor, overviewCounts, overviewSessions, type OverviewFilter } from "./feed";
import { colonyFacts, hostFacts } from "./host";
import type { FleetHost, HostInfo, Session } from "../types";

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

/** Flip one colony's membership in the expanded set. Pure, so the tests can pin the toggle. */
export function flipExpanded(expanded: ReadonlySet<string>, id: string): Set<string> {
  const next = new Set(expanded);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  return next;
}

/**
 * One colony on the overview. Collapsed it is a single line — status, name, age, status label — with
 * the issue title kept behind the disclosure; clicked, the title, the colony's machine facts and the
 * issue's address open beneath it in place. The header is a plain disclosure button (never a
 * navigation), and the colony's own view stays one explicit "open colony →" away so the overview
 * does not empty out on a stray click.
 */
export function ColonyRow({
  session,
  open,
  onToggle,
  onOpenColony,
  onSelect,
}: {
  session: Session;
  /** Whether this colony's detail is revealed. */
  open: boolean;
  onToggle: (id: string) => void;
  onOpenColony: (id: string) => void;
  /** Puts the colony into the inspector, whose pane can answer a waiting question directly. */
  onSelect: (session: Session) => void;
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
  // The colony's machine facts (issue #205): the microVM it booted, the boot itself, its mesh
  // address and its agent — only the ones that exist, so a pre-#205 colony shows just its agent.
  const meta = colonyFacts(session);

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
          {meta.length > 0 && (
            <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 font-mono text-[11px] text-faint">
              {meta.map((fact, i) => (
                <span key={i} title={fact.title} className="inline-flex items-center gap-1 whitespace-nowrap">
                  {fact.icon === "cpu" && <IconCpu size={11} className="shrink-0" />}
                  {fact.value}
                </span>
              ))}
            </div>
          )}
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
            {needsYou(session) && (
              <button
                type="button"
                onClick={() => onSelect(session)}
                className="ml-auto cursor-pointer whitespace-nowrap font-sans text-[12.5px] font-semibold text-warn hover:underline"
              >
                answer in the pane →
              </button>
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
  host,
  fleet,
  onOpenOrg,
  onOpenColony,
  onSelect,
}: {
  /** Every colony the mothership knows, unfiltered — this page is the cross-workspace view. */
  sessions: Session[];
  orgs: OrgEntry[];
  cost: number | null;
  /** The machine every listed colony boots on, polled with the status; a mothership before issue #205 sends none. */
  host?: HostInfo | null;
  /** Self plus every peer configured via COLONIZER_FLEET_PEERS (issue #231); absent or empty renders no fleet panel. */
  fleet?: FleetHost[];
  onOpenOrg: (org: string) => void;
  onOpenColony: (id: string) => void;
  /** Puts a chosen colony into the cockpit's inspector; its pane can answer a waiting question. */
  onSelect: (session: Session) => void;
}): ReactElement {
  // The filter lives here, not in the cockpit: toggling a counter narrows the page, and a second
  // click on the active one (or the counts themselves) clears it. State is per-visit on purpose.
  const [filter, setFilter] = useState<OverviewFilter | null>(null);
  // Which rows are unfolded, by session id. The 4 s poll hands this component brand-new session
  // objects, so the set is keyed on the id and lives here — a re-render must not close the row a
  // person is reading. Filtering narrows which rows render but never touches this set, so a row
  // stays expanded across a filter switch and is still open when its bucket comes back.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = (id: string) => setExpanded((rows) => flipExpanded(rows, id));
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

        {host && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-xl border border-border bg-panel px-3.5 py-2">
            <span
              title={host.hostname ? `${host.hostname} · host ${host.id}` : `host ${host.id}`}
              className="font-mono text-[11px] font-semibold"
            >
              {host.hostname || host.id.slice(0, 8)}
            </span>
            {hostFacts(host).map((fact, i) => (
              <span key={i} title={fact.title} className="inline-flex items-center gap-1 font-mono text-[11px] text-faint">
                {fact.icon === "server" && <IconServer size={12} className="shrink-0" />}
                {fact.icon === "cpu" && <IconCpu size={12} className="shrink-0" />}
                {fact.icon === "memory" && <IconMemory size={12} className="shrink-0" />}
                {fact.value}
              </span>
            ))}
            {host.kvm_ok === false && (
              <span
                title="KVM is unavailable: this host cannot boot microVMs, so no colony can start here"
                className="inline-flex items-center gap-1 font-mono text-[11px] text-warn"
              >
                <IconAlert size={12} className="shrink-0" />
                no KVM
              </span>
            )}
            <span title="when the mothership last probed the machine" className="ml-auto font-mono text-[11px] text-faint">
              checked {timeAgo(host.checked_at)}
            </span>
          </div>
        )}

        <FleetPanel hosts={fleet ?? []} />

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
                      mine.map((session) => (
                        <ColonyRow
                          key={session.id}
                          session={session}
                          open={expanded.has(session.id)}
                          onToggle={toggle}
                          onOpenColony={onOpenColony}
                          onSelect={onSelect}
                        />
                      ))
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
