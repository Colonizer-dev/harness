// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". The rail's base button lands here.
//
// The prototype's rows carry a settler count. This one does not: the mothership streams events for a
// single colony at a time, so the only honest per-colony facts here are the ones in the list itself.
import { useId, useMemo, useState, type ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { IconAlert, IconChevron, IconCpu, IconMemory, IconServer } from "../components/icons";
import { SESSION_STATUS, type Tone, attentionText, cx, isLive, orgOf, sameOrg, timeAgo } from "../components/ui";
import { colonyLabel, needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { isActive, isRaiding } from "../redTeam";
import { sortSessions } from "../sessionOrder";
import { formatCost, orgCost, sumCosts } from "../spend";
import { useSpendHistory } from "../useSpendHistory";
import { BurnDownCard } from "./BurnDownCard";
import { FleetPanel } from "./FleetPanel";
import { headlineFor, OVERVIEW_FILTERS, matchesOverviewFilter, overviewCounts, overviewSessions, overviewVisibleSessions, type OverviewFilter } from "./feed";
import { colonyFacts, hostFacts } from "./host";
import { OrgSpend } from "./OrgSpend";
import { RedAnts } from "./RedAnts";
import { RedTeamCard } from "./RedTeamCard";
import type { FleetHost, HostInfo, RedTeamRun, Session, SpendOrgDay, StartRedTeamRunRequest } from "../types";

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
  runs = [],
  initialFilter = null,
  onStart,
  onStop,
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
  /** Red-team runs (issue #212): the card lists them, and a raid paints ants over its org's card. */
  runs?: RedTeamRun[];
  /** The bucket filter to start on. Null in production — the tests pin the filtered states through it because static markup cannot click. */
  initialFilter?: OverviewFilter | null;
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onStop?: (id: string) => Promise<void>;
  onOpenOrg: (org: string) => void;
  onOpenColony: (id: string) => void;
  /** Puts a chosen colony into the cockpit's inspector; its pane can answer a waiting question. */
  onSelect: (session: Session) => void;
}): ReactElement {
  // The filter lives here, not in the cockpit: toggling a counter narrows the page, and a second
  // click on the active one (or the counts themselves) clears it. State is per-visit on purpose.
  const [filter, setFilter] = useState<OverviewFilter | null>(initialFilter ?? null);
  // Which rows are unfolded, by session id. The 4 s poll hands this component brand-new session
  // objects, so the set is keyed on the id and lives here — a re-render must not close the row a
  // person is reading. Filtering narrows which rows render but never touches this set, so a row
  // stays expanded across a filter switch and is still open when its bucket comes back.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = (id: string) => setExpanded((rows) => flipExpanded(rows, id));
  // The page renders one card per entry of `orgs` (the visible workspaces), so the counters cover
  // exactly that set — never the whole list. Counting switched-off orgs in the chips while their
  // colonies have no card is the divergence behind issue #246: bare global numbers over a list
  // that cannot show them. The chips keep moving on the 4s poll; a bucket filter narrows the
  // cards, never these, and each chip navigates to what it counts.
  const visibleSessions = overviewVisibleSessions(sessions, orgs);
  const counts = overviewCounts(visibleSessions);
  const shown = overviewSessions(visibleSessions, filter);
  // Colonies the chips deliberately do not count: their org is switched off, so no card can show
  // them. They are named in the scope line below instead of being silently hidden.
  const hiddenSessions = sessions.filter((session) => !visibleSessions.includes(session));
  const hiddenCounts = overviewCounts(hiddenSessions);
  const hiddenOrgs = [...new Set(
    hiddenSessions
      .filter((session) => matchesOverviewFilter(session, "live") || matchesOverviewFilter(session, "need you"))
      .map((session) => orgOf(session))
      .filter((org) => org !== ""),
  )].sort((a, b) => a.localeCompare(b));
  const hiddenParts = [
    hiddenCounts.live > 0 ? `${hiddenCounts.live} live` : null,
    hiddenCounts["need you"] > 0 ? `${hiddenCounts["need you"]} need you` : null,
  ].filter((part): part is string => part !== null);

  // One raid per org colours its card; the newest active run wins when several target it.
  const raidFor = new Map<string, RedTeamRun>();
  for (const run of runs) {
    if (!isActive(run)) continue;
    const org = run.org || run.repo.split("/")[0];
    if (!raidFor.has(org)) raidFor.set(org, run);
  }
  const runForOrg = (org: OrgEntry["org"]) => raidFor.get(org);

  // The daily spend history for the sparklines, loaded once (see useSpendHistory). Per org it is
  // aligned to the full day span: a day the org has no entry becomes an empty (zero-height) slot.
  const spendHistory = useSpendHistory();
  const daysByOrg = useMemo(() => {
    const span = spendHistory?.days ?? [];
    const byOrg = new Map<string, { day: string; org: SpendOrgDay | undefined }[]>();
    for (const day of span) {
      for (const orgDay of day.orgs) {
        const list = byOrg.get(orgDay.org) ?? [];
        list.push({ day: day.day, org: orgDay });
        byOrg.set(orgDay.org, list);
      }
    }
    // Align each org to the full day span: a day the response lists with no entry for this org
    // becomes an empty (zero-height) slot on its sparkline.
    for (const [org, days] of byOrg) {
      const orgByDay = new Map(days.map((d) => [d.day, d.org]));
      byOrg.set(org, span.map((day) => ({ day: day.day, org: orgByDay.get(day.day) })));
    }
    return byOrg;
  }, [spendHistory]);

  // Prefer the server's per-org rollups when it reports them, so the header can never disagree with
  // the rows it sits above; only fall back to the sessions-derived `cost` while any org carries no
  // server spend — an older mothership, or an org the rollup has never measured a dollar for.
  const headerCost =
    orgs.length > 0 && orgs.every((o) => o.spend !== undefined)
      ? sumCosts(orgs.map((o) => orgCost(o.spend)))
      : cost;

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
            {headerCost !== null && (
              <span className="whitespace-nowrap px-1" title="what every colony has spent in total">
                {formatCost(headerCost)} spent
              </span>
            )}
          </div>
        </div>

        {(filter || hiddenOrgs.length > 0) && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11.5px] text-muted" role="status">
            {filter && (
              <span>
                filter &quot;{filter}&quot; · showing {shown.length} of {visibleSessions.length}
                <button
                  type="button"
                  onClick={() => setFilter(null)}
                  className="ml-2 cursor-pointer text-accent hover:underline"
                >
                  clear ×
                </button>
              </span>
            )}
            {hiddenOrgs.length > 0 && (
              <span>
                + {hiddenParts.join(" · ")} in hidden {hiddenOrgs.length === 1 ? "org" : "orgs"} ({hiddenOrgs.join(", ")}) —
                re-enable {hiddenOrgs.length === 1 ? "it" : "them"} in the org switcher
              </span>
            )}
          </div>
        )}

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

        <BurnDownCard />

        {filter && shown.length === 0 ? (
          <div className="rounded-2xl border border-border bg-panel px-4 py-3.5 text-[13px] text-muted">
            <div>
              nothing under &quot;{filter}&quot; in these workspaces
              {visibleSessions.length > 0 && (
                <> · {visibleSessions.length} in other bucket{visibleSessions.length === 1 ? "" : "s"}</>
              )}
              {hiddenOrgs.length > 0 && (
                <> · + {hiddenParts.join(" · ")} in hidden {hiddenOrgs.length === 1 ? "org" : "orgs"} ({hiddenOrgs.join(", ")})</>
              )}
            </div>
            <button
              type="button"
              onClick={() => setFilter(null)}
              className="mt-1.5 cursor-pointer font-semibold text-accent hover:underline"
            >
              clear filter ×
            </button>
          </div>
        ) : (
          <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fill,minmax(300px,1fr))]">
            {orgs.map((org) => {
              const mine = sortSessions(shown.filter((s) => sameOrg(orgOf(s), org.org)));
              const raid = runForOrg(org.org);
              // Under a filter, an org with no matching colonies drops out entirely.
              if (filter && mine.length === 0) return null;
              return (
                <section
                  key={org.org}
                  className="relative flex flex-col overflow-hidden rounded-2xl border border-border bg-panel"
                >
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

                  <OrgSpend spend={org.spend} history={daysByOrg.get(org.org)} />

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
                  {/* The raid's ants march over this org's card; the layer never takes clicks. */}
                  {raid && <RedAnts mode={isRaiding(raid) ? "raiding" : "waiting"} count={raid.swarm_size} />}
                </section>
              );
            })}
          </div>
        )}

        <RedTeamCard runs={runs} sessions={sessions} onStart={onStart} onStop={onStop} onOpenColony={onOpenColony} />
      </div>
    </main>
  );
}
