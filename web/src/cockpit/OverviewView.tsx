// Overview: every workspace and every colony on one page, for when the question is "what is going on
// everywhere" rather than "what is this nest doing". Follows the Claude Design "Cockpit Dashboards"
// reference (docs/design/cockpit-dashboards/, issue #398): headline, six KPI tiles, the needs-you
// queue, merged-per-day bars beside the workspaces-compared table, workspace cards, the colonies
// table and the system strip.
//
// Design elements with NO data source behind them (reported, never faked):
// - "Auto-answer with Jev" toggle and "Let Jev answer" buttons: no auto-answer backend exists.
//   Omitted; "Answer →" deep-links to the colony through the existing onOpenColony path.
// - Lead time, PR cycle time, CI pass rate KPIs: no PR timestamps, no CI data. KpiTile empty states.
// - Change failure rate: no failure history — derived from current session statuses as
//   failed ÷ (merged + failed) among sessions created in the window, with the basis in the sub-line.
// - Merged PRs (count, per-day bars, compared table): the mothership records no merge timestamp,
//   so everything "merged per day" is bucketed by created_at, said out loud in the sub-lines.
// - "Manage workspaces": this view receives no settings opener, so the buttons are omitted.
import { useMemo, useState, type ReactElement } from "react";

import { initialOf } from "../components/Avatar";
import { SESSION_STATUS, formatDuration, isLive, orgOf, sameOrg, stored, timeAgo } from "../components/ui";
import { needsYou } from "../notifications";
import type { OrgEntry } from "../orgs";
import { HIDE_EMPTY_ORGS_KEY, hideEmptyOrgEntries, parseHideEmptyOrgs } from "../orgs";
import { formatCost, formatTokens, modelMix, orgCost, sessionCost, sumCosts } from "../spend";
import { useSpendHistory } from "../useSpendHistory";
import { BurnDownCard } from "./BurnDownCard";
import { DashBars, DashLegend, DashPanel, Eyebrow, FilterChip, KpiTile, RangePicker, ShareBar, StatusChip, type KpiDef } from "./DashChart";
import { FleetPanel } from "./FleetPanel";
import { OrgDashboard } from "./OrgDashboard";
import {
  changeFailRate,
  dailyCosts,
  dailyFailRate,
  dailyMerged,
  dayKeyOfDate,
  deltaTone,
  formatDelta,
  formatPts,
  formatWait,
  mergedInWindow,
  orgColorFor,
  orgRepos,
  relDelta,
  shortDayLabel,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumTokens,
  TONE_VAR,
  waitingMs,
  waitingSince,
  type ProviderErrorSnapshot,
  type RangeDays,
} from "./dash";
import { headlineFor, OVERVIEW_FILTERS, heldSlots, matchesOverviewFilter, overviewCounts, overviewVisibleSessions, queueStalled, type OverviewFilter } from "./feed";
import { hostFacts } from "./host";
import { RedTeamCard } from "./RedTeamCard";
import { StoragePanel } from "./StoragePanel";
import type { FleetHost, HostInfo, RedTeamRun, Session, StartRedTeamRunRequest, StatusQuota } from "../types";

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

/** An org's initial on its deterministic colour. The tile hue is theme-independent (like the
 *  reference), so the near-black token reads on it in both themes. */
function OrgTile({ org, size = 22 }: { org: string; size?: number }): ReactElement {
  return (
    <span
      aria-hidden="true"
      className="grid shrink-0 select-none place-items-center font-bold"
      style={{ width: size, height: size, borderRadius: size <= 22 ? 6 : 8, fontSize: Math.max(10, Math.round(size * 0.42)), background: orgColorFor(org), color: "var(--term-bg)" }}
    >
      {initialOf(org)}
    </span>
  );
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
  onStart,
  onStop,
  onOpenColony,
}: {
  /** Every colony the mothership knows, unfiltered — this page is the cross-workspace view. */
  sessions: Session[];
  orgs: OrgEntry[];
  cost: number | null;
  /** The machine every listed colony boots on, polled with the status; a mothership before issue #205 sends none. */
  host?: HostInfo | null;
  /** Self plus every peer configured via COLONIZER_FLEET_PEERS (issue #231); absent or empty renders no fleet panel. */
  fleet?: FleetHost[];
  /** Red-team runs (issue #212): the card lists them. */
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
  onStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onStop?: (id: string) => Promise<void>;
  onOpenColony: (id: string) => void;
}): ReactElement {
  // Dashboard toolbar state (issue #398): the range scopes the history-backed KPIs and charts;
  // the compare toggle adds previous-period deltas and the ghost line. Client state, per visit.
  const [range, setRange] = useState<RangeDays>(30);
  const [compare, setCompare] = useState(true);
  // The org whose dashboard replaces the overview body in place; null is the overview itself.
  // Local state, so Cockpit.tsx stays untouched.
  const [dashOrg, setDashOrg] = useState<string | null>(null);
  // The colonies table's own filters: one status bucket plus one org. Null is unfiltered.
  const [colonyFilter, setColonyFilter] = useState<OverviewFilter | null>(initialFilter ?? null);
  const [orgFilter, setOrgFilter] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);
  // The page renders one card per entry of `orgs` (the visible workspaces), so the counters cover
  // exactly that set — never the whole list. Counting switched-off orgs in the chips while their
  // colonies have no card is the divergence behind issue #246: bare global numbers over a list
  // that cannot show them.
  const visibleSessions = overviewVisibleSessions(sessions, orgs);
  const counts = overviewCounts(visibleSessions);
  // The Workspace-settings "hide orgs with no colonies" toggle is client-side state: read it on
  // render so flipping it in the dialog shows on the next poll.
  const workspaces = hideEmptyOrgEntries(orgs, parseHideEmptyOrgs(stored(HIDE_EMPTY_ORGS_KEY)));
  // Held slots (issue #217): idle colonies whose PR autopilot holds occupy parallel slots without
  // doing work. When every slot-occupying colony is held and something queues, nothing can drain
  // until a hold times out — the queued chip must read as stalled, never as a healthy busy queue.
  const held = heldSlots(visibleSessions);
  const stalled = queueStalled(visibleSessions);
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

  // Twice the range is fetched so the compare toggle has a previous period to stand on.
  const spendHistory = useSpendHistory(range * 2);
  const { current, previous } = useMemo(() => slicePeriods(spendHistory, range), [spendHistory, range]);

  // Prefer the server's per-org rollups when it reports them, so the header can never disagree with
  // the rows it sits above; only fall back to the sessions-derived `cost` while any org carries no
  // server spend — an older mothership, or an org the rollup has never measured a dollar for.
  const headerCost =
    orgs.length > 0 && orgs.every((o) => o.spend !== undefined)
      ? sumCosts(orgs.map((o) => orgCost(o.spend)))
      : cost;

  // Range windows over wall-clock time for the session-derived figures. There is no merge
  // timestamp, so "merged in range" means created in range — bucketed by created_at.
  const nowMs = Date.now();
  const fromMs = nowMs - range * 86_400_000;
  const prevFromMs = fromMs - range * 86_400_000;
  const days = rangeDays(range, nowMs);
  const prevDays = rangeDays(range, fromMs - 1);
  const xLabels = days.map(shortDayLabel);

  const needList = visibleSessions.filter(needsYou).sort((a, b) => waitingMs(b) - waitingMs(a) || a.id.localeCompare(b.id));
  const working = visibleSessions.filter((s) => s.status === "running" || s.status === "starting").length;

  // Six KPI tiles: merged and spend are real, change-failure is a snapshot reading, and lead
  // time / PR cycle time / CI pass rate have no data source — honest empty states, same card shape.
  const mergedCur = mergedInWindow(visibleSessions, fromMs, nowMs);
  const mergedPrev = mergedInWindow(visibleSessions, prevFromMs, fromMs);
  const mergedDelta = compare ? relDelta(mergedCur.length, mergedPrev.length) : null;
  const failCur = changeFailRate(visibleSessions, fromMs, nowMs);
  const failPrev = changeFailRate(visibleSessions, prevFromMs, fromMs);
  const failDelta = compare && failCur.rate != null && failPrev.rate != null ? failCur.rate - failPrev.rate : null;
  const periodSpend = sumHistoryCost(current);
  const spendDelta = compare ? relDelta(periodSpend, sumHistoryCost(previous)) : null;
  const kpis: KpiDef[] = [
    {
      label: "MERGED PRS",
      value: String(mergedCur.length),
      delta: mergedDelta != null ? formatDelta(mergedDelta) : undefined,
      deltaTone: deltaTone(mergedDelta),
      deltaDir: (mergedDelta ?? 0) < 0 ? "down" : "up",
      spark: sparkPoints(dailyMerged(visibleSessions, days)),
      sub: `${(mergedCur.length / range).toFixed(1)} per day · by created_at, no merge date`,
      hint: "sessions with status merged created in range, GET /api/sessions — the merge date is not recorded, so they bucket by created_at",
    },
    { label: "LEAD TIME", value: "—", emptyNote: "no data source yet", hint: "issue picked up → PR opened: no PR timestamps are served" },
    { label: "PR CYCLE TIME", value: "—", emptyNote: "no data source yet", hint: "PR opened → merged: no PR timestamps are served" },
    failCur.rate == null
      ? { label: "CHANGE FAILURE RATE", value: "—", emptyNote: "nothing decided in range", hint: "failed ÷ (merged + failed) among sessions created in range, GET /api/sessions" }
      : {
          label: "CHANGE FAILURE RATE",
          value: `${(failCur.rate * 100).toFixed(1)}%`,
          delta: failDelta != null ? formatPts(failDelta) : undefined,
          deltaTone: deltaTone(failDelta, "down"),
          deltaDir: (failDelta ?? 0) < 0 ? "down" : "up",
          spark: sparkPoints(dailyFailRate(visibleSessions, days)),
          sub: `${failCur.failed} failed of ${failCur.decided} decided (merged+failed)`,
          hint: "failed ÷ (merged + failed) among sessions created in range, GET /api/sessions — a snapshot reading, not a history",
        },
    { label: "CI PASS RATE", value: "—", emptyNote: "no data source yet", hint: "checks on colony PRs: no CI data is served" },
    {
      label: "SPEND",
      value: formatCost(periodSpend),
      delta: spendDelta != null ? formatDelta(spendDelta) : undefined,
      deltaTone: deltaTone(spendDelta, "down"),
      deltaDir: (spendDelta ?? 0) < 0 ? "down" : "up",
      spark: sparkPoints(dailyCosts(current)),
      sub: `${formatTokens(sumTokens(current))} tokens`,
      hint: "measured spend in range, GET /api/spend/history",
    },
  ];

  // Merged per day, stacked by workspace in deterministic org colours; the ghost is the previous
  // period's daily total when compare is on.
  const mergedSeries = workspaces.map((o) => ({ label: o.org, color: orgColorFor(o.org), values: dailyMerged(visibleSessions, days, o.org) }));
  const ghostTotals = prevDays.map((day) => dailyMerged(visibleSessions, [day]).reduce((t, v) => t + v, 0));
  const ghost = compare && ghostTotals.some((v) => v > 0) ? ghostTotals.map((v) => (v > 0 ? v : null)) : undefined;

  // Workspaces compared: merged share plus the in-range failure reading and the measured rollup.
  const compared = workspaces.map((o) => {
    const merged = mergedInWindow(visibleSessions.filter((s) => sameOrg(orgOf(s), o.org)), fromMs, nowMs).length;
    const fail = changeFailRate(visibleSessions, fromMs, nowMs, o.org);
    return { org: o.org, merged, fail, spend: orgCost(o.spend) };
  });
  const mergedAll = compared.reduce((t, c) => t + c.merged, 0);

  // The colonies table: bucket + org filters, needs-you longest-wait first, capped at ten.
  const tableSessions = sortColonies(
    visibleSessions.filter(
      (s) => (!colonyFilter || matchesOverviewFilter(s, colonyFilter)) && (!orgFilter || sameOrg(orgOf(s), orgFilter)),
    ),
  );
  const tableShown = showAll ? tableSessions : tableSessions.slice(0, COLONY_LIMIT);

  // An org dashboard replaces the overview body in place; the header above stays put.
  const dashEntry = dashOrg ? workspaces.find((o) => sameOrg(o.org, dashOrg)) : undefined;
  if (dashEntry) {
    return (
      <main className="cockpit min-h-0 overflow-y-auto px-6 pb-10 pt-7">
        <div className="mx-auto flex w-full max-w-[1240px] flex-col gap-5">
          <RangePicker range={range} onRange={setRange} compare={compare} onCompare={() => setCompare((c) => !c)} />
          <OrgDashboard
            org={dashEntry}
            sessions={visibleSessions.filter((s) => sameOrg(orgOf(s), dashEntry.org))}
            history={spendHistory}
            range={range}
            compare={compare}
            providers={providers}
            onBack={() => setDashOrg(null)}
          />
        </div>
      </main>
    );
  }

  const fleetHosts = fleet ?? [];
  const fleetOnline = fleetHosts.filter((h) => h.health === "online").length;

  return (
    <main className="cockpit min-h-0 overflow-y-auto px-6 pb-10 pt-7">
      <div className="mx-auto flex w-full max-w-[1240px] flex-col gap-5">
        {quota?.paused && !quotaBannerVisible ? (
          <div role="status" className="rounded-md border border-warn bg-warn-soft px-3 py-2 text-sm text-warn">
            Queue paused — {quota.reason ?? "every provider's quota is exhausted"}
          </div>
        ) : null}
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div>
            <div className="mb-1.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">OVERVIEW · {workspaces.length} WORKSPACES</div>
            <div className="text-[22px] font-semibold tracking-tight">{headlineFor(needList.length, working)}</div>
          </div>
          <RangePicker range={range} onRange={setRange} compare={compare} onCompare={() => setCompare((c) => !c)} />
        </div>

        {(colonyFilter || orgFilter || hiddenOrgs.length > 0) && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11.5px] text-muted" role="status">
            {(colonyFilter || orgFilter) && (
              <span>
                filter {colonyFilter ? `"${colonyFilter}"` : ""}{colonyFilter && orgFilter ? " · " : ""}{orgFilter ? `org "${orgFilter}"` : ""} · showing {tableSessions.length} of {visibleSessions.length}
                <button
                  type="button"
                  onClick={() => { setColonyFilter(null); setOrgFilter(null); setShowAll(false); }}
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

        <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(180px,1fr))]">
          {kpis.map((k) => (
            <KpiTile key={k.label} {...k} />
          ))}
        </div>

        {needList.length > 0 && (
          <section className="overflow-hidden rounded-2xl border border-border bg-panel">
            <div className="flex items-center gap-2.5 border-b border-border bg-warn-soft px-4 py-3">
              <span aria-hidden="true" className="h-[7px] w-[7px] rounded-full bg-warn" />
              <span className="font-mono text-[10.5px] tracking-[0.12em] text-warn">NEEDS YOU · {needList.length}</span>
              <span className="text-xs text-muted">colonies paused on a question, oldest first</span>
            </div>
            {needList.map((session) => {
              const org = orgOf(session);
              const waitMs = waitingMs(session, nowMs);
              const wait = formatWait(waitMs);
              const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
              return (
                <div key={session.id} className="grid grid-cols-[22px_minmax(0,1fr)_auto_auto] items-center gap-3 border-t border-border px-4 py-2.5 first:border-t-0">
                  <OrgTile org={org} />
                  <span className="min-w-0">
                    <span className="block truncate text-[13px]">
                      {short} <span className="text-muted">· {session.issue_title || short}</span>
                    </span>
                    <span className="block font-mono text-[11px] text-faint">
                      {org} · waiting {wait}
                    </span>
                  </span>
                  <span className={`font-mono text-[11px] ${waitMs > 2 * 3_600_000 ? "text-err" : "text-warn"}`}>{wait}</span>
                  <button
                    type="button"
                    onClick={() => onOpenColony(session.id)}
                    title={waitingSince(session)}
                    className="cursor-pointer whitespace-nowrap rounded-lg border border-accent bg-accent-soft px-2.5 py-[5px] text-xs font-semibold text-accent hover:bg-accent hover:text-on-accent"
                  >
                    Answer →
                  </button>
                </div>
              );
            })}
          </section>
        )}

        <div className="flex flex-wrap gap-3.5">
          <DashPanel
            title="MERGED PRS PER DAY · BY WORKSPACE"
            sub={`${mergedCur.length} merged in ${range}d${compare ? ` · dashed line is the previous ${range}d` : ""} · by created_at, no merge date`}
            legend={<DashLegend items={mergedSeries.map((s) => ({ label: s.label, color: s.color }))} />}
            className="min-w-0 flex-[2_1_560px]"
          >
            {mergedAll > 0 || mergedSeries.some((s) => s.values.some((v) => v > 0)) ? (
              <DashBars series={mergedSeries} labels={days} ghost={ghost} format={(v) => String(Math.round(v))} formatY={(v) => (Math.abs(v - Math.round(v)) < 1e-9 ? String(Math.round(v)) : "")} xLabels={xLabels} />
            ) : (
              <div className="py-6 text-center font-mono text-[11px] text-faint">no merged PRs in range</div>
            )}
          </DashPanel>
          <section className="flex min-w-0 flex-[1_1_320px] flex-col overflow-hidden rounded-2xl border border-border bg-panel">
            <div className="px-4 pb-1 pt-4">
              <Eyebrow>WORKSPACES COMPARED</Eyebrow>
              <div className="mt-1 text-[13px] text-muted">Share of merged PRs, {range}d</div>
            </div>
            <div className="grid grid-cols-[minmax(0,1fr)_58px_62px_70px] gap-2 border-b border-border px-4 py-1.5 font-mono text-[10px] tracking-[0.08em] text-faint">
              <span>ORG</span>
              <span className="text-right">MERGED</span>
              <span className="text-right">FAIL %</span>
              <span className="text-right">SPEND</span>
            </div>
            {compared.map((c) => (
              <button
                key={c.org}
                type="button"
                onClick={() => setDashOrg(c.org)}
                className="grid cursor-pointer grid-cols-[minmax(0,1fr)_58px_62px_70px] items-center gap-2 border-b border-border px-4 py-2.5 text-left tabular-nums last:border-b-0 hover:bg-panel-2"
              >
                <span className="min-w-0">
                  <span className="block truncate text-[13px] font-semibold">{c.org}</span>
                  <span className="mt-1.5 block h-1 overflow-hidden rounded-full bg-panel-3">
                    <span className="block h-full rounded-full" style={{ width: `${mergedAll > 0 ? (c.merged / mergedAll) * 100 : 0}%`, background: orgColorFor(c.org) }} title={`share of merged: ${c.merged} of ${mergedAll}`} />
                  </span>
                </span>
                <span className="text-right font-mono text-xs">{c.merged}</span>
                <span className={`text-right font-mono text-xs ${c.fail.rate != null && c.fail.rate > 0.08 ? "text-err" : "text-muted"}`} title={c.fail.rate != null ? `${c.fail.failed} failed of ${c.fail.decided} decided (merged+failed)` : "nothing decided in range"}>
                  {c.fail.rate != null ? `${(c.fail.rate * 100).toFixed(1)}%` : "—"}
                </span>
                <span className="text-right font-mono text-xs">{formatCost(c.spend)}</span>
              </button>
            ))}
            <div className="mt-auto px-4 py-2.5 font-mono text-[11px] text-faint">
              {hiddenOrgs.length > 0 ? `+ ${hiddenOrgs.length} hidden ${hiddenOrgs.length === 1 ? "org" : "orgs"} not counted` : "All workspaces shown"}
            </div>
          </section>
        </div>

        <div className="mt-1 font-mono text-[10.5px] tracking-[0.12em] text-faint">WORKSPACES</div>
        <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fill,minmax(min(100%,340px),1fr))]">
          {workspaces.map((org) => {
            const mine = visibleSessions.filter((s) => sameOrg(orgOf(s), org.org));
            const need = mine.filter(needsYou).length;
            const live = mine.filter((s) => isLive(s.status)).length;
            const queued = mine.filter((s) => s.status === "queued").length;
            const returned = mine.filter((s) => ["pr_opened", "merged", "closed", "no_changes"].includes(s.status)).length;
            const merged = mergedInWindow(mine, fromMs, nowMs).length;
            const fail = changeFailRate(mine, fromMs, nowMs);
            const spend = orgCost(org.spend);
            const tokens = sumTokens(current, org.org);
            const mix = modelMix(org.spend?.models, 1);
            const topModel = mix.shown.length > 0 ? `${mix.shown[0].model} ${formatTokens(mix.shown[0].tokens)}` : "no model usage";
            return (
              <section key={org.org} className="flex min-w-0 flex-col overflow-hidden rounded-2xl border border-border bg-panel">
                <button
                  type="button"
                  onClick={() => setDashOrg(org.org)}
                  className="grid cursor-pointer grid-cols-[30px_minmax(0,1fr)_auto] items-center gap-2.5 px-4 pb-3 pt-3.5 text-left hover:bg-panel-2"
                >
                  <OrgTile org={org.org} size={30} />
                  <span className="min-w-0">
                    <span className="block truncate font-semibold">{org.org}</span>
                    <span className="block font-mono text-[11px] text-faint">
                      {mine.length} {mine.length === 1 ? "colony" : "colonies"} · {live} live · {orgRepos(mine, org.org)} {orgRepos(mine, org.org) === 1 ? "repo" : "repos"}
                    </span>
                  </span>
                  {need > 0 && <StatusChip tone="warn">{need} need you</StatusChip>}
                </button>
                <div className="px-4">
                  <ShareBar
                    segments={[
                      { label: `${need} need you`, color: "var(--warn)", value: need },
                      { label: `${live - need} working`, color: "var(--info)", value: Math.max(0, live - need) },
                      { label: `${queued} queued`, color: "var(--faint)", value: queued },
                      { label: `${returned} returned`, color: "var(--ok)", value: returned },
                    ]}
                    format={(v) => String(v)}
                    label={`${org.org} colonies by status`}
                  />
                  <div className="flex flex-wrap gap-x-3 gap-y-0.5 pt-2 font-mono text-[10.5px] text-faint">
                    {[
                      { label: `${need} need you`, color: "var(--warn)", show: need > 0 },
                      { label: `${Math.max(0, live - need)} working`, color: "var(--info)", show: live - need > 0 },
                      { label: `${queued} queued`, color: "var(--faint)", show: queued > 0 },
                      { label: `${returned} returned`, color: "var(--ok)", show: returned > 0 },
                    ].filter((e) => e.show).map((e) => (
                      <span key={e.label} className="inline-flex items-center gap-1.5">
                        <span aria-hidden="true" className="h-1.5 w-1.5 rounded-full" style={{ background: e.color }} />
                        {e.label}
                      </span>
                    ))}
                  </div>
                </div>
                <div className="grid grid-cols-3 gap-2.5 px-4 py-3.5">
                  {[
                    { label: "MERGED", value: String(merged) },
                    { label: "SPEND", value: formatCost(spend) },
                    { label: "FAIL %", value: fail.rate != null ? `${(fail.rate * 100).toFixed(1)}%` : "—" },
                  ].map((stat) => (
                    <div key={stat.label} className="min-w-0">
                      <div className="font-mono text-[10px] tracking-[0.1em] text-faint">{stat.label}</div>
                      <div className="mt-0.5 truncate text-[16px] font-semibold tabular-nums">{stat.value}</div>
                    </div>
                  ))}
                </div>
                <div className="px-4 pb-3">
                  <svg viewBox="0 0 100 30" preserveAspectRatio="none" className="block h-[30px] w-full" aria-hidden="true">
                    <polygon points={`0,30 ${sparkPoints(dailyCosts(current, org.org))} 100,30`} fill={orgColorFor(org.org)} opacity={0.16} />
                    <polyline points={sparkPoints(dailyCosts(current, org.org))} fill="none" stroke={orgColorFor(org.org)} strokeWidth={1.5} strokeLinejoin="round" vectorEffect="non-scaling-stroke" />
                  </svg>
                  <div className="mt-1 flex justify-between font-mono text-[10px] text-faint">
                    <span>daily spend · {range}d</span>
                    <span>{formatTokens(tokens)} tokens</span>
                  </div>
                </div>
                <div className="mt-auto flex items-center justify-between gap-2 border-t border-border px-4 py-2.5 font-mono text-[11px]">
                  <span className="min-w-0 truncate text-faint">{topModel}</span>
                  <button type="button" onClick={() => setDashOrg(org.org)} className="cursor-pointer whitespace-nowrap text-accent hover:underline">
                    dashboard →
                  </button>
                </div>
              </section>
            );
          })}
        </div>

        <section className="overflow-hidden rounded-2xl border border-border bg-panel">
          <div className="flex flex-wrap items-center justify-between gap-2.5 border-b border-border px-4 py-3.5">
            <span className="font-mono text-[10.5px] tracking-[0.12em] text-faint">COLONIES · {tableSessions.length}</span>
            <div className="flex flex-wrap items-center gap-1.5">
              {OVERVIEW_FILTERS.map((name) => {
                const count = counts[name];
                const active = colonyFilter === name;
                const stalledQueue = name === "queued" && stalled;
                const pick = () => { setColonyFilter(active ? null : name); setShowAll(false); };
                // A stalled queue keeps the warn-bordered treatment the header chips always had —
                // FilterChip has no tone, so this one stays a bespoke button in the same shape.
                return stalledQueue ? (
                  <button
                    key={name}
                    type="button"
                    aria-pressed={active}
                    onClick={pick}
                    title="every slot-occupying colony is held — the queue cannot drain until a hold times out"
                    className="cursor-pointer whitespace-nowrap rounded-full border border-warn bg-warn-soft px-2.5 py-1 text-[11.5px] font-medium text-warn"
                  >
                    <span className="text-warn">{count}</span> queued · stalled
                  </button>
                ) : (
                  <FilterChip
                    key={name}
                    active={active}
                    count={count}
                    label={name}
                    onClick={pick}
                  />
                );
              })}
              <span aria-hidden="true" className="mx-1 h-4 w-px bg-border" />
              {[{ label: "All orgs", org: null as string | null }, ...workspaces.map((o) => ({ label: o.org, org: o.org as string | null }))].map((chip) => {
                const active = orgFilter === chip.org;
                return (
                  <button
                    key={chip.label}
                    type="button"
                    aria-pressed={active}
                    onClick={() => { setOrgFilter(active ? null : chip.org); setShowAll(false); }}
                    className={`cursor-pointer whitespace-nowrap rounded-full border px-2.5 py-1 text-[11.5px] ${
                      active ? "border-accent bg-accent-soft text-text" : "border-border text-muted hover:border-accent hover:text-text"
                    }`}
                  >
                    {chip.label}
                  </button>
                );
              })}
              {held.count > 0 && (
                <span
                  role="status"
                  title="idle colonies holding parallel slots while autopilot holds their pull request"
                  className="whitespace-nowrap px-1 font-mono text-[11px] text-muted"
                >
                  {held.count} held{held.oldestAgeMs != null ? ` · oldest ${formatDuration(held.oldestAgeMs)}` : ""}
                </span>
              )}
              {headerCost !== null && (
                <span className="whitespace-nowrap px-1 font-mono text-[11px] tabular-nums text-muted" title="what every colony has spent in total">
                  {formatCost(headerCost)} spent
                </span>
              )}
            </div>
          </div>
          {tableSessions.length === 0 ? (
            colonyFilter || orgFilter ? (
            <div className="px-4 py-3.5 text-[13px] text-muted">
              <div>
                nothing under {colonyFilter ? `"${colonyFilter}"` : "this filter"}
                {visibleSessions.length > 0 && (
                  <> · {visibleSessions.length} in other bucket{visibleSessions.length === 1 ? "" : "s"}</>
                )}
                {hiddenOrgs.length > 0 && (
                  <> · + {hiddenParts.join(" · ")} in hidden {hiddenOrgs.length === 1 ? "org" : "orgs"} ({hiddenOrgs.join(", ")})</>
                )}
              </div>
              <button
                type="button"
                onClick={() => { setColonyFilter(null); setOrgFilter(null); setShowAll(false); }}
                className="mt-1.5 cursor-pointer font-semibold text-accent hover:underline"
              >
                clear filter ×
              </button>
            </div>
            ) : (
              <div className="px-4 py-3.5 text-[13px] text-faint">No colonies in these workspaces yet.</div>
            )
          ) : (
            <div className="overflow-x-auto">
              <div className="min-w-[720px]">
                <div className="grid grid-cols-[10px_minmax(0,2.6fr)_minmax(0,1.1fr)_150px_80px_72px] gap-3 border-b border-border px-4 py-2 font-mono text-[10px] tracking-[0.08em] text-faint">
                  <span />
                  <span>COLONY</span>
                  <span>ORG</span>
                  <span>STATUS</span>
                  <span className="text-right">UPDATED</span>
                  <span className="text-right">SPENT</span>
                </div>
                {tableShown.map((session) => {
                  const tone = SESSION_STATUS[session.status]?.tone ?? "neutral";
                  const short = `${session.repo.split("/")[1] ?? session.repo}${session.issue != null ? `#${session.issue}` : ""}`;
                  return (
                    <div key={session.id} className="grid grid-cols-[10px_minmax(0,2.6fr)_minmax(0,1.1fr)_150px_80px_72px] items-center gap-3 border-b border-border px-4 py-[9px] text-[13px] last:border-b-0">
                      <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: TONE_VAR[tone] }} />
                      <button type="button" onClick={() => onOpenColony(session.id)} className="min-w-0 cursor-pointer truncate text-left hover:text-accent" title={session.issue_title || short}>
                        <span className="font-medium">{short}</span> <span className="text-faint">{session.issue_title}</span>
                      </button>
                      <span className="min-w-0 truncate text-muted">{orgOf(session)}</span>
                      <span className="truncate font-mono text-[11px]" style={{ color: TONE_VAR[tone] }}>
                        {SESSION_STATUS[session.status]?.label ?? session.status}
                      </span>
                      <span className="text-right font-mono text-[11px] text-faint">
                        {session.status === "queued" ? `queued ${formatWait(nowMs - Date.parse(session.created_at))}` : timeAgo(session.last_activity_at ?? session.updated_at)}
                      </span>
                      <span className="text-right font-mono text-[11px] tabular-nums">{formatCost(sessionCost(session))}</span>
                    </div>
                  );
                })}
              </div>
            </div>
          )}
          {tableSessions.length > COLONY_LIMIT && (
            <button
              type="button"
              onClick={() => setShowAll((v) => !v)}
              className="w-full cursor-pointer border-0 bg-transparent px-4 py-2.5 text-[12.5px] font-semibold text-accent hover:bg-panel-2"
            >
              {showAll ? "Show fewer" : `Show all ${tableSessions.length} colonies`}
            </button>
          )}
        </section>

        <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(min(100%,260px),1fr))]">
          {host && (
            <div className="flex flex-col gap-1 rounded-xl border border-border bg-panel px-3.5 py-2.5">
              <Eyebrow>HOST</Eyebrow>
              <div className="font-mono text-[11.5px] text-muted">
                <span className="font-semibold text-text">{host.hostname || host.id.slice(0, 8)}</span>
                {hostFacts(host).slice(1).map((fact, i) => (
                  <span key={i} title={fact.title}> · {fact.value}</span>
                ))}
              </div>
            </div>
          )}
          <div className="flex flex-col gap-1 rounded-xl border border-border bg-panel px-3.5 py-2.5">
            <Eyebrow>FLEET</Eyebrow>
            <div className="font-mono text-[11.5px] text-muted">
              {fleetHosts.length === 0 ? (
                "no fleet data"
              ) : (
                <span className="inline-flex items-center gap-1.5">
                  <span aria-hidden="true" className={`h-[7px] w-[7px] rounded-full ${fleetOnline === fleetHosts.length ? "bg-ok" : "bg-err"}`} />
                  {fleetHosts.length === 1 ? "1 host online · no peers configured" : `${fleetOnline} of ${fleetHosts.length} hosts online`}
                </span>
              )}
            </div>
          </div>
          <BurnDownCard />
        </div>

        <FleetPanel hosts={fleetHosts} />

        <StoragePanel onOpenColony={onOpenColony} />

        <RedTeamCard runs={runs} sessions={sessions} onStart={onStart} onStop={onStop} onOpenColony={onOpenColony} />
      </div>
    </main>
  );
}
