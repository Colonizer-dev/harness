// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". Follows the Claude Design "Cockpit Dashboards
// v3" reference: title and meta line, the KPI strip, the needs-you queue, merged-per-day as stacked
// areas beside each workspace's share, the workspaces table, the colonies table, and the system
// strip — flat sections between hairlines, no cards.
//
// Design elements with NO data source behind them (reported, never faked):
// - Change failure rate: no failure history — derived from current session statuses as
//   failed ÷ (merged + failed) in the window (merged by merge date, failed by created
//   date), with the basis in the sub-line.
// - Merged PRs (count, per-day areas, workspaces table): bucketed by merged_at, falling
//   back to created_at when the mothership omits it, said out loud in the sub-lines.
// - "Manage workspaces": lives in the header's switcher (it opens org settings); this view has no
//   org-settings opener of its own.
// - "Answer": deep-links to the colony through onOpenColony; no inline answering backend here.
import { HackerIcon } from "./HackerIcon";
import { useMemo, useState, type ReactElement } from "react";

import type { SectionId } from "../components/SettingsDialog";
import { formatDuration, isLive, orgOf, sameOrg, stored, timeAgo } from "../components/ui";
import { needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { HIDE_EMPTY_ORGS_KEY, hideEmptyOrgEntries, parseHideEmptyOrgs } from "../orgs";
import { formatCost, formatTokens, orgCost, sumCosts } from "../spend";
import { useSpendHistory } from "../useSpendHistory";
import { BurnDownCard } from "./BurnDownCard";
import { AreaChart, ChartSection, ColonyRow, KpiStrip, OrgTile, RangePicker, Rules, Section, TrendLine, type KpiDef } from "./DashChart";
import { deliveryKpis } from "./delivery";
import { FleetPanel } from "./FleetPanel";
import { isBumped, isFlashed, useLiveEvents } from "./liveEvents";
import { OrgDashboard } from "./OrgDashboard";
import {
  changeFailRate,
  dailyCosts,
  dailyFailRate,
  dailyMerged,
  dayKeyOfDate,
  chartColor,
  deltaTone,
  formatPts,
  formatWait,
  mergedInWindow,
  compareDelta,
  shortDayLabel,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumTokens,
  waitingMs,
  waitingSince,
  type ProviderErrorSnapshot,
  type RangeDays,
} from "./dash";
import { heldSlots, matchesOverviewFilter, overviewCounts, overviewVisibleSessions, queueStalled, type OverviewFilter } from "./feed";
import { hostFacts } from "./host";
import { RedTeamWizard } from "./RedTeamWizard";
import { RedTeamHistory } from "./RedTeamHistory";
import { StoragePanel } from "./StoragePanel";
import { useOpenQuestions } from "./questions";
import type { FleetHost, HostInfo, RedTeamRun, Session, StartRedTeamRunRequest, StatusQuota, StorageSummary } from "../types";
import type { LiveConnection } from "../liveStream";
import { IssuesButton, type IssuesActions } from "./IssuesHandoff";
import { taskLine, taskTooltip } from "../summary";
import { ColonyFilterHeader, NO_FILTERS, applyColonyFilters, filtersActive, type ColonyFilters } from "./ColonyFilters";

/** The last `range` local-calendar days, ascending — the x axis of every per-day series.
 *  Walks the calendar (not fixed 24h steps) so a DST transition cannot duplicate or skip a day. */
function rangeDays(range: number, nowMs: number): string[] {
  const base = new Date(nowMs);
  base.setHours(0, 0, 0, 0);
  return Array.from({ length: range }, (_, i) => {
    const d = new Date(base);
    d.setDate(base.getDate() - (range - 1 - i));
    return dayKeyOfDate(d);
  });
}

/** Sort key for the colonies table: needs-you first (longest wait first), then live by
 *  recency, then everything else by recency. */
function colonyRank(session: Session): number {
  if (needsYou(session)) return 0;
  if (isLive(session.status)) return 1;
  return 2;
}

function sortColonies(list: Session[]): Session[] {
  return [...list].sort((a, b) => {
    const rank = colonyRank(a) - colonyRank(b);
    if (rank !== 0) return rank;
    if (colonyRank(a) === 0) return waitingMs(b) - waitingMs(a) || a.id.localeCompare(b.id);
    return b.updated_at.localeCompare(a.updated_at) || a.id.localeCompare(b.id);
  });
}


const COLONY_LIMIT = 10;

export function OverviewView({
  sessions,
  orgs,
  cost,
  host,
  fleet,
  runs = [],
  initialFilter = null,
  quota = null,
  quotaBannerVisible = false,
  providers = [],
  liveStorage = null,
  onStart,
  onStop,
  onOpenColony,
  onOpenSettings,
  scopeOrg,
  onScopeOrg,
  issues,
}: {
  /** Every colony the mothership knows, unfiltered — this page is the cross-workspace view. */
  sessions: Session[];
  orgs: OrgEntry[];
  cost: number | null;
  /** The machine every listed colony boots on, polled with the status; a mothership before issue #205 sends none. */
  host?: HostInfo | null;
  /** Self plus every peer configured via COLONIZER_FLEET_PEERS (issue #231); absent or empty renders no fleet panel. */
  fleet?: FleetHost[];
  /** Red-team runs (issue #212): each workspace row's history lists its own. */
  runs?: RedTeamRun[];
  /** The bucket filter to start on. Null in production — the tests pin the filtered states through it because static markup cannot click. */
  initialFilter?: OverviewFilter | null;
  /** Quota exhaustion across providers (issue #225); a paused queue banners the page. */
  quota?: StatusQuota | null;
  /** True while the cockpit's global quota banner shows (issue #404): the page's own scoped
    * queue-paused banner hides, so a paused queue banners exactly once. Defaults to false, which
    * keeps the scoped banner for every caller that renders this page without the global one. */
  quotaBannerVisible?: boolean;
  /** Cumulative provider tallies, mapped from GET /api/status `model_providers` by the caller;
   *  passed through to the in-place org dashboard. Empty stays empty, never zero. */
  providers?: ProviderErrorSnapshot[];
  /** The realtime feed's connection (issue #446); the header shows it, the org dashboard too. */
  connection?: LiveConnection;
  /** A storage frame the stream pushed; the storage panel shows it instead of its own fetch. */
  liveStorage?: StorageSummary | null;
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onStop?: (id: string) => Promise<void>;
  onOpenColony: (id: string) => void;
  /** Opens settings at a section; threaded to the storage panel's gear button. Absent in tests. */
  onOpenSettings?: (section: SectionId) => void;
  /** The GitHub issues hand-off, shown as a button above the range toolbar; absent in tests. */
  issues?: IssuesActions;
  /** The cockpit's workspace scope: set, it opens that org's dashboard in place; null is the
   *  overview. Omitted (tests), the page keeps the choice itself. */
  scopeOrg?: string | null;
  /** Changes the cockpit's scope — the sidebar's switcher and this page stay one control. */
  onScopeOrg?: (org: string | null) => void;
}): ReactElement {
  // Dashboard toolbar state (issue #398): the range scopes the history-backed KPIs and charts;
  // the compare toggle adds previous-period deltas and the ghost line. Client state, per visit.
  const [range, setRange] = useState<RangeDays>(30);
  const [compare, setCompare] = useState(true);
  // The org whose dashboard replaces the overview body in place; null is the overview itself.
  const [localDashOrg, setLocalDashOrg] = useState<string | null>(null);
  const dashOrg = scopeOrg !== undefined ? scopeOrg : localDashOrg;
  const setDashOrg = onScopeOrg ?? setLocalDashOrg;
  // The colonies table's own filters: one status bucket plus one org. Null is unfiltered.
  // The colonies table's filters, set from its header row (ColonyFilters.tsx).
  const [filters, setFilters] = useState<ColonyFilters>(() => ({ ...NO_FILTERS, statuses: new Set(initialFilter ? [initialFilter] : []) }));
  const filtered = filtersActive(filters);
  // The red-team wizard or history open for one workspace row; null is neither.
  const [redTeam, setRedTeam] = useState<{ org: string; view: "wizard" | "history" } | null>(null);
  const [showAll, setShowAll] = useState(false);
  // What moved since the last push: flashes the row whose status changed, lights a risen cost.
  const events = useLiveEvents(sessions);
  const questions = useOpenQuestions(sessions);
  // The page covers exactly the visible workspaces — never the whole list. Counting switched-off
  // orgs while their colonies have no row is the divergence behind issue #246.
  const visibleSessions = overviewVisibleSessions(sessions, orgs);
  const counts = overviewCounts(visibleSessions);
  // The Workspace-settings "hide orgs with no colonies" toggle is client-side state: read it on
  // render so flipping it in the dialog shows on the next poll.
  const workspaces = hideEmptyOrgEntries(orgs, parseHideEmptyOrgs(stored(HIDE_EMPTY_ORGS_KEY)));
  // Held slots (issue #217): idle colonies whose PR autopilot holds occupy parallel slots without
  // doing work. When every slot-occupying colony is held and something queues, nothing can drain
  // until a hold times out — the queued tab must read as stalled, never as a healthy busy queue.
  const held = heldSlots(visibleSessions);
  const stalled = queueStalled(visibleSessions);
  // Colonies the counts deliberately do not include: their org is switched off. They are named
  // in the scope line below instead of being silently hidden.
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

  // Twice the range is fetched so the compare toggle has a previous period to stand on.
  const spendHistory = useSpendHistory(range * 2);
  const { current, previous } = useMemo(() => slicePeriods(spendHistory, range), [spendHistory, range]);

  // Prefer the server's per-org rollups when it reports them, so the total can never disagree with
  // the rows it sits above; only fall back to the sessions-derived `cost` while any org carries no
  // server spend — an older mothership, or an org the rollup has never measured a dollar for.
  const headerCost =
    orgs.length > 0 && orgs.every((o) => o.spend !== undefined)
      ? sumCosts(orgs.map((o) => orgCost(o.spend)))
      : cost;

  // Range windows over wall-clock time for the session-derived figures. Merged sessions
  // bucket by merge date (merged_at, falling back to created_at); failed ones by created_at.
  const nowMs = Date.now();
  const fromMs = nowMs - range * 86_400_000;
  const prevFromMs = fromMs - range * 86_400_000;
  const days = rangeDays(range, nowMs);
  const prevDays = rangeDays(range, fromMs - 1);
  const dayLabels = days.map(shortDayLabel);

  const needList = visibleSessions.filter(needsYou).sort((a, b) => waitingMs(b) - waitingMs(a) || a.id.localeCompare(b.id));

  // The KPI strip: merged, change failure and spend are measured; live colonies is the moment's
  // count; lead time, PR cycle time and CI pass rate come from the PR watcher's timestamps and checks.
  const mergedCur = mergedInWindow(visibleSessions, fromMs, nowMs);
  const mergedPrev = mergedInWindow(visibleSessions, prevFromMs, fromMs);
  const mergedCmp = compare ? compareDelta(mergedCur.length, mergedPrev.length) : undefined;
  const mergedDelta = mergedCmp?.d ?? null;
  const failCur = changeFailRate(visibleSessions, fromMs, nowMs);
  const failPrev = changeFailRate(visibleSessions, prevFromMs, fromMs);
  const failDelta = compare && failCur.rate != null && failPrev.rate != null ? failCur.rate - failPrev.rate : null;
  const periodSpend = sumHistoryCost(current);
  const spendCmp = compare ? compareDelta(periodSpend, sumHistoryCost(previous)) : undefined;
  const spendDelta = spendCmp?.d ?? null;
  const kpis: KpiDef[] = [
    {
      label: "Merged PRs",
      value: String(mergedCur.length),
      valueNum: mergedCur.length,
      delta: mergedCmp?.text,
      deltaTone: deltaTone(mergedDelta),
      spark: sparkPoints(dailyMerged(visibleSessions, days)),
      sub: `${(mergedCur.length / range).toFixed(1)} per day · by merge date`,
      hint: "sessions with status merged in range, GET /api/sessions — bucketed by merged_at, falling back to created_at when absent",
    },
    failCur.rate == null
      ? { label: "Change failure rate", value: "—", emptyNote: "nothing decided in range", hint: "failed ÷ (merged + failed) in range, GET /api/sessions" }
      : {
          label: "Change failure rate",
          value: `${(failCur.rate * 100).toFixed(1)}%`,
          delta: failDelta != null ? formatPts(failDelta) : undefined,
          deltaTone: deltaTone(failDelta, "down"),
          spark: sparkPoints(dailyFailRate(visibleSessions, days)),
          sub: `${failCur.failed} failed of ${failCur.decided} decided (merged+failed)`,
          hint: "failed ÷ (merged + failed) in range, GET /api/sessions — merged by merge date, failed by created date: a snapshot reading, not a history",
        },
    {
      label: "Spend",
      value: formatCost(periodSpend),
      valueNum: periodSpend ?? undefined,
      formatNum: (n) => formatCost(n),
      delta: spendCmp?.text,
      deltaTone: deltaTone(spendDelta, "down"),
      spark: sparkPoints(dailyCosts(current)),
      sub: `${formatTokens(sumTokens(current))} tokens`,
      hint: "measured spend in range, GET /api/spend/history",
    },
    {
      label: "Live colonies",
      value: String(counts.live),
      valueNum: counts.live,
      sub: `${counts.queued} queued${held.count > 0 ? ` · ${held.count} held` : ""}`,
      hint: "starting, working, idle or waiting on you, right now",
    },
    ...deliveryKpis(visibleSessions, { from: fromMs, to: nowMs }, { from: prevFromMs, to: fromMs }, days, compare),
  ];

  // Merged per day, stacked by workspace in ramp order (org identity rides on the avatars, never
  // on hue alone); the ghost is the previous period's daily total when compare is on.
  const mergedSeries = workspaces.map((o, i) => ({ label: o.org, color: chartColor(i), values: dailyMerged(visibleSessions, days, o.org) }));
  const ghostTotals = prevDays.map((day) => dailyMerged(visibleSessions, [day]).reduce((t, v) => t + v, 0));
  // Compare on with an empty previous period still draws: a flat dashed zero line, labelled as such,
  // so the switch visibly does something instead of silently showing nothing.
  const prevEmpty = ghostTotals.every((v) => v === 0) && (sumHistoryCost(previous) ?? 0) === 0;
  const ghost = compare ? ghostTotals : undefined;

  // Per workspace: merged share, the in-range failure reading and the measured rollup.
  const compared = workspaces.map((o, i) => {
    const mine = visibleSessions.filter((s) => sameOrg(orgOf(s), o.org));
    const merged = mergedInWindow(mine, fromMs, nowMs).length;
    const fail = changeFailRate(visibleSessions, fromMs, nowMs, o.org);
    return { org: o.org, avatar: o.avatar, color: chartColor(i), mine, merged, fail, spend: orgCost(o.spend), need: mine.filter(needsYou).length };
  });
  const mergedAll = compared.reduce((t, c) => t + c.merged, 0);

  // The colonies table: its header filters, needs-you longest-wait first, capped at ten.
  const tableSessions = sortColonies(applyColonyFilters(visibleSessions, filters, nowMs));
  const tableShown = showAll ? tableSessions : tableSessions.slice(0, COLONY_LIMIT);
  const rangePicker = (
    <RangePicker range={range} onRange={setRange} compare={compare} onCompare={() => setCompare((c) => !c)} emptyPrevious={prevEmpty} />
  );
  // The issues hand-off sits above the range controls, at the title row's right.
  const toolbar = issues ? (
    <div className="flex flex-col items-end gap-2.5">
      <IssuesButton variant="header" {...issues} />
      {rangePicker}
    </div>
  ) : (
    rangePicker
  );

  // An org dashboard replaces the overview body in place; the header above stays put.
  const dashEntry = dashOrg ? workspaces.find((o) => sameOrg(o.org, dashOrg)) : undefined;
  if (dashEntry) {
    return (
      <main className="cockpit min-h-0 overflow-y-auto px-6 pb-20 pt-10">
        <div className="mx-auto w-full max-w-[1080px]">
          <OrgDashboard
            org={dashEntry}
            sessions={visibleSessions.filter((s) => sameOrg(orgOf(s), dashEntry.org))}
            history={spendHistory}
            range={range}
            compare={compare}
            providers={providers}
            toolbar={toolbar}
            events={events}
            onOpenColony={onOpenColony}
            onBack={() => setDashOrg(null)}
          />
        </div>
      </main>
    );
  }

  const fleetHosts = fleet ?? [];
  const fleetOnline = fleetHosts.filter((h) => h.health === "online").length;
  const clearFilters = () => {
    setFilters(NO_FILTERS);
    setShowAll(false);
  };

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-20 pt-10">
      <div className="mx-auto flex w-full max-w-[1080px] flex-col gap-10">
        {quota?.paused && !quotaBannerVisible ? (
          <div role="status" className="-mb-4 border-y border-warn/40 py-2.5 text-[13px] text-warn">
            Queue paused — {quota.reason ?? "every provider's quota is exhausted"}
          </div>
        ) : null}

        <div className="flex flex-wrap items-end justify-between gap-4">
          <div className="min-w-0">
            <h1 className="m-0 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">Overview</h1>
            <div className="mt-2 text-[14px] text-muted">
              {needList.length} {needList.length === 1 ? "colony needs" : "colonies need"} you · {counts.live} live · {counts.queued} queued across {workspaces.length}{" "}
              {workspaces.length === 1 ? "workspace" : "workspaces"}
              {headerCost !== null && <> · {formatCost(headerCost)} spent</>}
            </div>
          </div>
          {toolbar}
        </div>

        {(filtered || hiddenOrgs.length > 0) && (
          <div className="-mt-6 flex flex-wrap items-center gap-x-3 gap-y-1 text-[12.5px] text-faint" role="status">
            {filtered && (
              <span>
                colonies filtered · showing {tableSessions.length} of {visibleSessions.length}
                <button type="button" onClick={clearFilters} className="ml-2 cursor-pointer border-0 bg-transparent p-0 text-muted underline underline-offset-[3px] hover:text-text">
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

        <KpiStrip items={kpis} />

        {needList.length > 0 && (
          <Section id="sec-inbox" title="Needs you" meta={`${needList.length} waiting · oldest first`}>
            <Rules>
              {needList.map((session) => {
                const org = orgOf(session);
                const waitMs = waitingMs(session, nowMs);
                const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
                return (
                  <div
                    key={session.id}
                    className={`-mt-px grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-4 border-t border-border py-3 transition-colors duration-[1200ms] ${isFlashed(events, session.id, nowMs) ? "v3-flash" : ""}`}
                  >
                    <span className="flex min-w-0 flex-col gap-0.5">
                      <span className="truncate text-[14px]" title={taskTooltip(session)}>{taskLine(session, short)}</span>
                      {questions[session.id] && <span className="line-clamp-2 text-[13px] text-warn [text-wrap:pretty]">{questions[session.id]}</span>}
                      <span className="text-[12.5px] text-faint">
                        <span className="font-mono text-[12px]">{short}</span> · {org}
                      </span>
                    </span>
                    <span className={`text-[13px] tabular-nums ${waitMs > 2 * 3_600_000 ? "text-err" : "text-warn"}`} title={waitingSince(session)}>
                      {formatWait(waitMs)}
                    </span>
                    <button
                      type="button"
                      onClick={() => onOpenColony(session.id)}
                      className="cursor-pointer rounded-md border-0 bg-text px-3 py-1.5 text-[13px] font-medium text-bg hover:opacity-85"
                    >
                      Answer
                    </button>
                  </div>
                );
              })}
            </Rules>
          </Section>
        )}

        <ChartSection
          title="Merged PRs per day"
          legend={
            ghost ? (
              <span className="inline-flex items-center gap-1.5 text-[12.5px] text-muted">
                <span aria-hidden="true" className="h-0 w-3 border-t border-dashed border-muted" />
                prev {range}d{prevEmpty ? " · no activity" : ""}
              </span>
            ) : undefined
          }
          legendIcons
          chart={(hot) => (
            <AreaChart
              highlight={hot}
              seriesReadout={false}
              series={mergedSeries}
              labels={dayLabels}
              ghost={ghost}
              format={(v) => String(Math.round(v))}
              formatY={(v) => (Math.abs(v - Math.round(v)) < 1e-9 ? String(Math.round(v)) : v.toFixed(1))}
              readTitle={`Last ${range} days`}
              emptyNote="no merged PRs in range"
            />
          )}
          foot={`${mergedCur.length} merged in ${range}d · by merge date${ghost ? ` · dashed: previous ${range}d${prevEmpty ? " (no activity)" : ""}` : ""}`}
          sideTitle="Share by workspace"
          sideLimit={SHARE_LIMIT}
          side={[...compared]
            .sort((a, b) => b.merged - a.merged)
            .map((c) => {
              const share = mergedAll > 0 ? (c.merged / mergedAll) * 100 : 0;
              const entry = workspaces.find((o) => sameOrg(o.org, c.org));
              return {
                label: c.org,
                value: c.merged,
                note: mergedAll > 0 ? `${Math.round(share)}%` : "—",
                share,
                color: c.color,
                title: `open the ${c.org} dashboard`,
                onClick: () => setDashOrg(c.org),
                icon: <OrgTile org={c.org} avatar={c.avatar} size={18} />,
                card: (
                  <ShareCard
                    org={c.org}
                    avatar={c.avatar}
                    color={c.color}
                    description={entry?.description}
                    merged={c.merged}
                    share={mergedAll > 0 ? share : null}
                    range={range}
                    live={entry?.live ?? 0}
                    total={entry?.total ?? 0}
                    need={c.need}
                  />
                ),
              };
            })}
          sideFoot={hiddenOrgs.length > 0 ? `+ ${hiddenOrgs.length} hidden ${hiddenOrgs.length === 1 ? "org" : "orgs"} not counted` : "All workspaces shown"}
        />

        <Section title="Workspaces" meta={String(workspaces.length)}>
          <Rules>
            <div className="overflow-x-auto">
              <div className="min-w-[760px]">
                <div className={`${WS_GRID} border-b border-border py-2.5 text-[12.5px] text-muted`}>
                  <span>Name</span>
                  <span className="text-right">Colonies</span>
                  <span className="text-right">Need</span>
                  <span className="text-right">Merged</span>
                  <span className="text-right">Fail</span>
                  <span className="text-right">Spend</span>
                  <span>Trend</span>
                  <span className="sr-only">Actions</span>
                </div>
                {compared.map((c) => (
                  <div
                    key={c.org}
                    role="button"
                    tabIndex={0}
                    onClick={() => setDashOrg(c.org)}
                    onKeyDown={(e) => {
                      if (e.target !== e.currentTarget) return;
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setDashOrg(c.org);
                      }
                    }}
                    title={`open the ${c.org} dashboard`}
                    className={`${WS_GRID} -mt-px w-full cursor-pointer border-0 border-t border-solid border-border bg-transparent py-3.5 text-left text-[13.5px] tabular-nums text-text hover:bg-panel-2 focus-visible:outline-2 focus-visible:outline-accent`}
                  >
                    <span className="flex min-w-0 items-center gap-2.5">
                      <OrgTile org={c.org} avatar={c.avatar} size={22} />
                      <span className="truncate font-medium">{c.org}</span>
                    </span>
                    <span className="text-right text-muted">{c.mine.length}</span>
                    <span className={`text-right ${c.need > 0 ? "text-warn" : "text-faint"}`}>{c.need > 0 ? `${c.need} need you` : "—"}</span>
                    <span className="text-right">{c.merged}</span>
                    <span
                      className={`text-right ${c.fail.rate != null && c.fail.rate > 0.08 ? "text-err" : "text-muted"}`}
                      title={c.fail.rate != null ? `${c.fail.failed} failed of ${c.fail.decided} decided (merged+failed)` : "nothing decided in range"}
                    >
                      {c.fail.rate != null ? `${(c.fail.rate * 100).toFixed(1)}%` : "—"}
                    </span>
                    <span className="text-right">{formatCost(c.spend)}</span>
                    <TrendLine values={dailyCosts(current, c.org)} />
                    <RedTeamActions
                      org={c.org}
                      live={runs.filter((r) => sameOrg(r.org, c.org) && ["armed", "waiting", "running", "draining"].includes(r.state)).length}
                      onStart={() => setRedTeam({ org: c.org, view: "wizard" })}
                      onHistory={() => setRedTeam({ org: c.org, view: "history" })}
                    />
                  </div>
                ))}
              </div>
            </div>
          </Rules>
        </Section>

        <Section
          id="sec-colonies"
          title="Colonies"
          meta={String(tableSessions.length)}
          right={
            filtered ? (
              <button type="button" onClick={clearFilters} className="cursor-pointer border-0 bg-transparent p-0 text-[12.5px] text-muted underline underline-offset-[3px] hover:text-text">
                clear filters
              </button>
            ) : undefined
          }
        >
          {held.count > 0 && (
            <div role="status" title="idle colonies holding parallel slots while autopilot holds their pull request" className={`mb-3 text-[12.5px] ${stalled ? "text-warn" : "text-faint"}`}>
              {held.count} held{held.oldestAgeMs != null ? ` · oldest ${formatDuration(held.oldestAgeMs)}` : ""}
            </div>
          )}
          <Rules>
            {visibleSessions.length > 0 && (
              <div className="overflow-x-auto">
                <div className="min-w-[680px]">
                  <ColonyFilterHeader
                    filters={filters}
                    onChange={(next) => {
                      setFilters(next);
                      setShowAll(false);
                    }}
                    sessions={visibleSessions}
                    workspaces={workspaces.map((w) => ({ org: w.org, avatar: w.avatar ?? null }))}
                    counts={counts}
                    stalledQueue={stalled}
                  />
                </div>
              </div>
            )}
            {tableSessions.length === 0 ? (
              filtered ? (
                <div className="py-3.5 text-[13px] text-muted">
                  <div>
                    nothing matches these filters
                    {visibleSessions.length > 0 && <> · {visibleSessions.length} in other bucket{visibleSessions.length === 1 ? "" : "s"}</>}
                    {hiddenOrgs.length > 0 && <> · + {hiddenParts.join(" · ")} in hidden {hiddenOrgs.length === 1 ? "org" : "orgs"} ({hiddenOrgs.join(", ")})</>}
                  </div>
                  <button type="button" onClick={clearFilters} className="mt-1.5 cursor-pointer border-0 bg-transparent p-0 font-medium text-text underline underline-offset-[3px]">
                    clear filters ×
                  </button>
                </div>
              ) : (
                <div className="py-3.5 text-[13px] text-faint">No colonies in these workspaces yet.</div>
              )
            ) : (
              <div className="overflow-x-auto">
                <div className="min-w-[680px]">
                  {tableShown.map((session) => (
                    <ColonyRow
                      key={session.id}
                      session={session}
                      age={session.status === "queued" ? `queued ${formatWait(nowMs - Date.parse(session.created_at))}` : timeAgo(session.last_activity_at ?? session.updated_at)}
                      flashed={isFlashed(events, session.id, nowMs)}
                      bumped={isBumped(events, session.id, nowMs)}
                      onOpen={onOpenColony}
                      orgAvatar={workspaces.find((w) => sameOrg(w.org, orgOf(session)))?.avatar ?? null}
                    />
                  ))}
                </div>
              </div>
            )}
            {tableSessions.length > COLONY_LIMIT && (
              <button
                type="button"
                onClick={() => setShowAll((v) => !v)}
                className="w-full cursor-pointer border-0 border-t border-solid border-border bg-transparent py-3 text-left text-[13px] text-muted hover:bg-panel-2 hover:text-text"
              >
                {showAll ? "Show fewer" : `Show all ${tableSessions.length} colonies`}
              </button>
            )}
          </Rules>
        </Section>

        <div className="flex flex-wrap gap-x-6 gap-y-2 text-[12.5px] text-faint">
          {host && (
            <span title="the machine every listed colony boots on">
              Host {host.hostname || host.id.slice(0, 8)}
              {hostFacts(host).slice(1).map((fact, i) => (
                <span key={i} title={fact.title}> · {fact.value}</span>
              ))}
            </span>
          )}
          <span className="inline-flex items-center gap-1.5">
            {fleetHosts.length === 0 ? (
              "no fleet data"
            ) : (
              <>
                <span aria-hidden="true" className={`h-1.5 w-1.5 rounded-full ${fleetOnline === fleetHosts.length ? "bg-ok" : "bg-err"}`} />
                {fleetHosts.length === 1 ? "1 host online · no peers configured" : `${fleetOnline} of ${fleetHosts.length} hosts online`}
              </>
            )}
          </span>
        </div>

        <div className="flex flex-col gap-4">
          <BurnDownCard />
          <FleetPanel hosts={fleetHosts} />
          <StoragePanel onOpenColony={onOpenColony} onOpenSettings={onOpenSettings} liveStorage={liveStorage} />
        </div>
      </div>
      <RedTeamWizard
        org={redTeam?.org ?? null}
        open={redTeam?.view === "wizard"}
        sessions={sessions}
        runs={runs}
        onStart={onStart}
        onClose={() => setRedTeam((r) => (r?.view === "wizard" ? null : r))}
        onDone={() => {}}
        onOpenHistory={(org) => setRedTeam({ org, view: "history" })}
      />
      <RedTeamHistory
        org={redTeam?.org ?? null}
        open={redTeam?.view === "history"}
        sessions={sessions}
        runs={runs}
        onStop={onStop}
        onOpenColony={onOpenColony}
        onClose={() => setRedTeam((r) => (r?.view === "history" ? null : r))}
        onNew={(org) => setRedTeam({ org, view: "wizard" })}
      />
    </main>
  );
}

/** A workspace row's red-team buttons: start one (the wizard), or see what ran (the history). */
function RedTeamActions({ org, live, onStart, onHistory }: { org: string; live: number; onStart: () => void; onHistory: () => void }): ReactElement {
  const stop = (fn: () => void) => (e: React.MouseEvent | React.KeyboardEvent) => {
    e.stopPropagation();
    fn();
  };
  return (
    <span className="flex items-center justify-end gap-1" onKeyDown={(e) => e.stopPropagation()}>
      <button
        type="button"
        onClick={stop(onStart)}
        aria-label={`start a red team on ${org}`}
        title="Hunt for bugs with a red-team swarm"
        className="inline-flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-md border border-border bg-panel px-2 py-1 text-[12px] text-text hover:border-border-strong hover:bg-panel-2"
      >
        <HackerIcon size={14} />
        Red team
        {live > 0 && <span className="rounded-full bg-accent px-1.5 text-[10.5px] tabular-nums text-on-accent">{live}</span>}
      </button>
      <button
        type="button"
        onClick={stop(onHistory)}
        aria-label={`red-team history for ${org}`}
        title="Red-team history and schedules"
        className="grid size-7 cursor-pointer place-items-center rounded-md border-0 bg-transparent text-muted hover:bg-panel-3 hover:text-text"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1" />
          <path d="M3.5 4.5v4h4" />
          <path d="M12 7.5V12l3 2" />
        </svg>
      </button>
    </span>
  );
}

/** The workspaces table's grid, shared by its header and rows. */
const WS_GRID = "grid grid-cols-[minmax(0,1.6fr)_64px_84px_64px_64px_84px_minmax(48px,1fr)_auto] items-center gap-4";

/** How many workspaces "Share by workspace" lists before "Show all". */
const SHARE_LIMIT = 5;

/** The hover card on a "Share by workspace" row: who the org is and what its number means. */
function ShareCard({
  org,
  avatar,
  color,
  description,
  merged,
  share,
  range,
  live,
  total,
  need,
}: {
  org: string;
  avatar: string | null;
  color: string;
  description?: string;
  merged: number;
  share: number | null;
  range: number;
  live: number;
  total: number;
  need: number;
}): ReactElement {
  return (
    <div className="flex flex-col gap-2.5">
      <div className="flex items-center gap-2.5">
        <OrgTile org={org} avatar={avatar} size={32} />
        <div className="min-w-0">
          <div className="truncate text-[13.5px] font-semibold text-text">{org}</div>
          {description ? (
            <div className="line-clamp-2 text-[12px] leading-snug text-muted">{description}</div>
          ) : (
            <div className="text-[12px] text-faint">No GitHub description</div>
          )}
        </div>
      </div>
      <div className="grid grid-cols-3 gap-2 border-t border-border pt-2.5 tabular-nums">
        <div>
          <div className="text-[15px] font-semibold text-text">{merged}</div>
          <div className="text-[11px] text-faint">merged · {range}d</div>
        </div>
        <div>
          <div className="text-[15px] font-semibold" style={{ color }}>
            {share == null ? "—" : `${Math.round(share)}%`}
          </div>
          <div className="text-[11px] text-faint">of all merged</div>
        </div>
        <div>
          <div className="text-[15px] font-semibold text-text">
            {live}
            <span className="text-[12px] font-normal text-faint">/{total}</span>
          </div>
          <div className="text-[11px] text-faint">live / colonies</div>
        </div>
      </div>
      {need > 0 && <div className="text-[12px] text-warn">{need} need you</div>}
      <div className="text-[11.5px] text-faint">Click to open the {org} dashboard</div>
    </div>
  );
}
