// Per-org dashboard (issue #398), in the Cockpit Dashboards v3 layout: back link, title and meta,
// repository pill tabs, the KPI strip, colony outcomes beside the delivery funnel, spend by model
// beside the token mix, the repositories table and the org's colonies. Backed only by real data —
// Session (statuses, created_at, cost, repo), /api/spend/history (per-org per-day
// launched/returned/cost/models) and optional cumulative provider tallies. Anything without a
// source (coverage, time to recover, per-day latency, lead-time histogram) is named as unmeasured
// once, never guessed. Lead time, PR cycle time and CI pass rate come from delivery.ts.
import { Fragment, useContext, useEffect, useState, type KeyboardEvent, type ReactElement, type ReactNode } from "react";
import { ApiContext } from "../context";
import { inPackage, packageRows, OUTSIDE, UNKNOWN, type PackageRow } from "./monorepo";

import { SESSION_STATUS, sameOrg, timeAgo } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { formatCost, formatTokens, modelMix, orgCost } from "../spend";
import type { RepoPackages, Session, SessionStatus, SpendHistory } from "../types";
import type { LiveConnection } from "../liveStream";
import { AreaChart, ChartSection, ColonyRow, DashLegend, KpiStrip, OrgTile, PillTab, Rules, Section, type KpiDef } from "./DashChart";
import { isBumped, isFlashed, type LiveEvents } from "./liveEvents";
import {
  changeFailRate,
  costPerMerged,
  dailyFailRate,
  dailyLaunched,
  dailyMerged,
  dayCost,
  dayKeyOf,
  deltaTone,
  formatDelta,
  formatPts,
  funnelFor,
  mergedAtOf,
  modelColorFor,
  relDelta,
  repoRows,
  shortDayLabel,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumTokens,
  type ProviderErrorSnapshot,
  type RangeDays,
} from "./dash";
import { deliveryKpis } from "./delivery";
import { PackagesView } from "./PackagesView";
import { COLONY_LIST_ALL, colonyListMatches } from "./ColonyFilters";
import { FilterSelect, Pagination, SearchBox, optionsBy } from "./ListControls";
import { PAGE_SIZE, usePagedFilter } from "./paging";
import { overviewCounts } from "./feed";

/** Kept here (rather than imported from dash) so existing importers keep working. */
export type { ProviderErrorSnapshot };

function formatLatency(ms: number): string {
  if (ms >= 1000) return `${parseFloat((ms / 1000).toFixed(2))}s`;
  return `${Math.round(ms)}ms`;
}

const shortRepo = (repo: string): string => repo.split("/")[1] ?? repo;

type Outcome = "merged" | "pr" | "nochg" | "failed" | "stopped";

/** A colony's current outcome bucket, or null while it is still in progress (live statuses have
 *  no outcome yet and sit out of the per-day stacks — the sub-line says how many). */
function outcomeOf(session: Session): Outcome | null {
  switch (session.status) {
    case "merged":
      return "merged";
    case "pr_opened":
    case "closed":
      return "pr";
    case "no_changes":
      return "nochg";
    case "failed":
      return "failed";
    case "stopped":
      return "stopped";
    default:
      return null;
  }
}

const OUTCOMES: ReadonlyArray<{ key: Outcome; label: string; color: string }> = [
  { key: "merged", label: "Merged", color: "var(--out-merged)" },
  { key: "pr", label: "PR open", color: "var(--out-pr)" },
  { key: "nochg", label: "No changes", color: "var(--out-nochg)" },
  { key: "failed", label: "Failed", color: "var(--out-failed)" },
  { key: "stopped", label: "Stopped", color: "var(--out-stopped)" },
];

export function OrgDashboard({
  org,
  sessions,
  history,
  range,
  compare,
  onBack,
  providers = [],
  initialRepo = null,
  initialPackages,
  initialPackage = null,
  toolbar,
  events,
  onOpenColony,
}: {
  org: OrgEntry;
  /** This org's visible sessions (already filtered by the caller). */
  sessions: Session[];
  history: SpendHistory | null;
  range: RangeDays;
  compare: boolean;
  onBack: () => void;
  /** Cumulative provider tallies; empty (the default) leaves API error rate and the latency
   *  caption as honest empty states. */
  providers?: ProviderErrorSnapshot[];
  /** The repo filter to start on. Null in production — the tests pin the filtered state through
   *  it because static markup cannot click. */
  initialRepo?: string | null;
  /** Monorepo detections by repository, pinned by tests; fetched from the API otherwise. */
  initialPackages?: Record<string, RepoPackages>;
  /** The package row to start filtered on (with `initialRepo`); tests only. */
  initialPackage?: string | null;
  /** The realtime feed's connection (issue #446); the header shows it now, so this is unread. */
  connection?: LiveConnection;
  /** The range/compare toolbar, which the caller owns; drawn at the title row's right. */
  toolbar?: ReactNode;
  /** What moved since the last push, for the colonies' row flash and cost highlight. */
  events?: LiveEvents;
  onOpenColony?: (id: string) => void;
}): ReactElement {
  // The repository filter scopes the whole dashboard below the header: every session-backed
  // figure reads `scoped`. Spend history is per org per day, so it — and the ghost line drawn
  // from it — stays org-wide; the spend panel's sub-line says so while a repo is selected.
  const [repo, setRepo] = useState<string | null>(initialRepo);
  // A monorepo's package filter narrows the repository filter further: `pkg` is a package path (or
  // the outside / not-read row key) within `repo`.
  const [pkg, setPkg] = useState<string | null>(initialPackage);
  const detections = useRepoPackages(sessions, initialPackages);
  const [codeTab, setCodeTab] = useState<"repositories" | "packages">("repositories");
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(initialRepo && initialPackage ? [initialRepo] : []));
  const repoScoped = repo ? sessions.filter((s) => sameOrg(s.repo, repo)) : sessions;
  const pkgDetection = repo ? detections[repo] : undefined;
  const scoped = repo && pkg && pkgDetection ? repoScoped.filter((s) => inPackage(s, pkg, pkgDetection)) : repoScoped;

  const { current, previous } = slicePeriods(history, range);
  const days = current.map((d) => d.day);
  const inRange = new Set(days);
  const launched = scoped.filter((s) => inRange.has(dayKeyOf(s.created_at))).length;
  const merged = scoped.filter((s) => s.status === "merged").length;
  const failed = scoped.filter((s) => s.status === "failed").length;
  const inProgress = scoped.filter((s) => outcomeOf(s) === null).length;
  const counts = overviewCounts(sessions);
  const repos = repoRows(sessions);
  const queued = counts.queued;

  // --- KPI tiles (the render's eight, in its order) ---
  const rollup = orgCost(org.spend);
  const periodSpend = sumHistoryCost(current, org.org);
  const prevSpend = sumHistoryCost(previous, org.org);
  const cpp = costPerMerged(rollup ?? periodSpend, merged);
  // In-range windowed figures for the sparkline + previous-period delta: merged per day via
  // dailyMerged (bucketed by merge date, falling back to created_at), the failure snapshot over the
  // history-day window (merged by merge date, failed by created date), and cost per merged PR off
  // the period spend. The tile values stay the dashboard's own readings (see the sub-lines); only
  // spark/delta read the window.
  const mergedDaily = dailyMerged(scoped, days);
  const mergedPrevDaily = previous.length > 0 ? dailyMerged(scoped, previous.map((d) => d.day)) : [];
  const mergedTotal = (ns: number[]) => ns.reduce((t, n) => t + n, 0);
  const mergedDelta = compare && previous.length > 0 ? relDelta(mergedTotal(mergedDaily), mergedTotal(mergedPrevDaily)) : null;
  const winOf = (ds: typeof current) =>
    ds.length > 0 ? { from: Date.parse(`${ds[0].day}T00:00:00`), to: Date.parse(`${ds[ds.length - 1].day}T00:00:00`) + 86_400_000 } : null;
  const curWin = winOf(current);
  const prevWin = winOf(previous);
  const failCurW = curWin ? changeFailRate(scoped, curWin.from, curWin.to) : { rate: null as number | null, failed: 0, decided: 0 };
  const failPrevW = prevWin ? changeFailRate(scoped, prevWin.from, prevWin.to) : { rate: null as number | null, failed: 0, decided: 0 };
  const failDelta = compare && failCurW.rate != null && failPrevW.rate != null ? failCurW.rate - failPrevW.rate : null;
  const cppCur = costPerMerged(periodSpend, mergedTotal(mergedDaily));
  const cppPrev = costPerMerged(prevSpend, mergedTotal(mergedPrevDaily));
  const cppDelta = compare && previous.length > 0 ? relDelta(cppCur, cppPrev) : null;
  const cppDaily = days.map((_, i) => costPerMerged(dayCost(current[i], org.org), mergedDaily[i] ?? 0));
  const totalReq = providers.reduce((n, p) => n + p.requests, 0);
  const totalFail = providers.reduce((n, p) => n + p.failures, 0);
  const errRate = totalReq > 0 ? totalFail / totalReq : null;
  const since = providers.map((p) => p.since).find((s) => s != null)?.slice(0, 10) ?? null;
  // The API error rate is cumulative, not range-bound, so it rides in the strip's footnote as a
  // snapshot rather than sitting among the ranged tiles (the v3 placement).
  const errNote =
    errRate != null
      ? `API error rate ${(errRate * 100).toFixed(2)}% of ${formatTokens(totalReq)} calls${since ? ` since ${since}` : ""} (cumulative)`
      : "API error rate: no data source yet";
  const delivery = deliveryKpis(scoped, curWin, prevWin, days, compare && previous.length > 0);
  const kpis: KpiDef[] = [
    {
      label: "Merged PRs",
      value: String(merged),
      delta: mergedDelta != null ? formatDelta(mergedDelta) : undefined,
      deltaTone: deltaTone(mergedDelta),
      spark: mergedDaily.length > 0 ? sparkPoints(mergedDaily) : undefined,
      sub: scoped.length > 0 ? `${Math.round((merged / scoped.length) * 100)}% of ${scoped.length} ${scoped.length === 1 ? "colony" : "colonies"}` : "no colonies in scope",
      hint: "sessions with status merged, GET /api/sessions — the delta reads merged in range vs the previous period (bucketed by merge date, falling back to created_at)",
    },
    delivery[0],
    delivery[1],
    {
      label: "Change failure rate",
      ...(scoped.length > 0
        ? {
            value: `${((failed / scoped.length) * 100).toFixed(1)}%`,
            delta: failDelta != null ? formatPts(failDelta) : undefined,
            deltaTone: deltaTone(failDelta, "down"),
            spark: days.length > 0 ? sparkPoints(dailyFailRate(scoped, days)) : undefined,
            sub: `${failed} failed of ${scoped.length}`,
          }
        : { value: "—", emptyNote: "no colonies in scope" }),
      hint: "failed sessions ÷ colonies in scope, GET /api/sessions — the spark and delta read the in-range window (merged by merge day, failed by created day)",
    },
    { label: "Time to recover", value: "—", unmeasured: true, hint: "needs failure → recovery timestamps; the API serves none" },
    delivery[2],
    {
      label: "Cost per merged PR",
      ...(merged > 0
        ? {
            value: formatCost(cpp),
            delta: cppDelta != null ? formatDelta(cppDelta) : undefined,
            deltaTone: deltaTone(cppDelta, "down"),
            spark: days.length > 0 ? sparkPoints(cppDaily) : undefined,
            sub: `${merged} merged`,
          }
        : { value: "—", emptyNote: "nothing merged yet" }),
      hint: "org spend rollup ÷ merged sessions, GET /api/orgs + GET /api/sessions — the delta reads period spend ÷ merged in range vs the previous period",
    },
    {
      label: "Spend",
      value: formatCost(periodSpend),
      valueNum: periodSpend ?? undefined,
      formatNum: (n) => formatCost(n),
      delta: compare && previous.length > 0 && relDelta(periodSpend, prevSpend) != null ? formatDelta(relDelta(periodSpend, prevSpend)) : undefined,
      deltaTone: deltaTone(compare ? relDelta(periodSpend, prevSpend) : null, "down"),
      spark: days.length > 0 ? sparkPoints(current.map((d) => dayCost(d, org.org))) : undefined,
      sub: `${formatTokens(sumTokens(current, org.org))} tokens`,
      hint: "measured spend in range, GET /api/spend/history",
    },
    { label: "Coverage", value: "—", unmeasured: true, hint: "the API serves no coverage" },
  ];

  // --- Colony outcomes per day: sessions bucketed by merge day when merged, by launch
  //     day otherwise, carrying their current status. ---
  const perDay = new Map<string, Record<Outcome, number>>();
  for (const s of scoped) {
    const outcome = outcomeOf(s);
    if (outcome === null) continue;
    const day = dayKeyOf(outcome === "merged" ? mergedAtOf(s) : s.created_at);
    if (!inRange.has(day)) continue;
    let row = perDay.get(day);
    if (!row) perDay.set(day, (row = { merged: 0, pr: 0, nochg: 0, failed: 0, stopped: 0 }));
    row[outcome] += 1;
  }
  const outcomeSeries = OUTCOMES.map((o) => ({ label: o.label, color: o.color, values: days.map((day) => perDay.get(day)?.[o.key] ?? 0) }));
  const prevLaunched = dailyLaunched(previous, org.org);
  // Zero-launch days are gaps, not baseline anchors: joining through them would sawtooth the ghost.
  const ghost = compare && !repo && previous.length > 0 ? days.map((_, i) => prevLaunched[i] || null) : undefined;
  const outcomeSub =
    `${launched} launched in range, ${merged} merged overall · merged by merge day, the rest by launch day · current status` +
    (inProgress > 0 ? ` · ${inProgress} in progress excluded` : "") +
    (compare && !repo && previous.length > 0 ? ` · dashed line is launches in the previous ${range}d` : "");

  // --- Delivery funnel: launched → PR opened → (no CI step: the API serves none) → merged. ---
  const funnel = funnelFor(scoped);
  const funnelNote = funnel.launched > 0 ? `${Math.round((funnel.merged / funnel.launched) * 100)}% of colonies end in a merged PR` : "no colonies in scope";

  // --- Spend per day by model (org-wide: history is per org, never per repo). ---
  const tokByModel = new Map<string, number>();
  const costByDay = new Map<string, Map<string, number>>();
  const tokensByDay = new Map<string, Map<string, number>>();
  let pricedModelCost = 0;
  for (const d of current) {
    const o = d.orgs.find((e) => sameOrg(e.org, org.org));
    if (!o) continue;
    const costs = new Map<string, number>();
    const tokens = new Map<string, number>();
    for (const m of o.models) {
      tokByModel.set(m.model, (tokByModel.get(m.model) ?? 0) + m.tokens);
      costs.set(m.model, m.cost_usd ?? 0);
      tokens.set(m.model, m.tokens);
      pricedModelCost += m.cost_usd ?? 0;
    }
    costByDay.set(d.day, costs);
    tokensByDay.set(d.day, tokens);
  }
  // Claude Code reports cost per colony, not per model, and a routed provider without prices has
  // none at all: the org total is known while almost every model's cost is null. Drawing cost per
  // model would then show an empty chart beside a real total, so it draws tokens per model instead
  // and says why.
  const modelsPriced = pricedModelCost > 0;
  const topModels = [...tokByModel.entries()].sort((a, b) => b[1] - a[1]).slice(0, 4).map(([model]) => model);
  const prevCosts = previous.map((d) => d.orgs.filter((e) => sameOrg(e.org, org.org)).reduce((n, e) => n + (e.cost_usd ?? e.routed_cost_usd ?? 0), 0));
  const spendGhost = compare && previous.length > 0 ? days.map((_, i) => prevCosts[i] || null) : undefined;
  const spendSub =
    periodSpend != null
      ? `$${periodSpend.toFixed(2)} over ${range}d · $${(periodSpend / range).toFixed(2)} per day${repo ? " · org-wide: spend history is per org" : ""}`
      : `no measured spend in range${repo ? " · spend history is per org" : ""}`;

  // --- Model API latency: no per-day series exists — the panel keeps the p50/p95 shape empty,
  //     with the cumulative per-provider average as the caption when the caller passes tallies. ---
  const latCaption =
    providers.filter((p) => p.requests > 0 && p.avgLatencyMs != null).length > 0
      ? providers
          .filter((p) => p.requests > 0 && p.avgLatencyMs != null)
          .map((p) => `${p.name} avg ${formatLatency(p.avgLatencyMs as number)} · ${formatTokens(p.requests)} requests`)
          .join(" · ")
      : "no per-day latency series";

  // --- Token usage / model mix: the org's cumulative rollup (not range-bound). ---
  const spendTokens = org.spend ? org.spend.tokens.input + org.spend.tokens.output + org.spend.tokens.cache_read + org.spend.tokens.cache_write : null;
  const mix = modelMix(org.spend?.models, 4);
  const costByModel = new Map((org.spend?.models ?? []).map((m) => [m.model, m.cost_usd]));
  const mixTotal = mix.shown.reduce((n, m) => n + m.tokens, 0);

  const colonies = sortSessions(scoped);
  const colonyList = usePagedFilter(colonies, { filters: COLONY_LIST_ALL, match: colonyListMatches });
  const nowMs = Date.now();
  const labels = days.map(shortDayLabel);
  const pctOf = (v: number) => (funnel.launched > 0 ? `${Math.round((v / funnel.launched) * 100)}%` : "");
  const shareOf = (v: number) => (funnel.launched > 0 ? (v / funnel.launched) * 100 : 0);
  const pickRepo = (r: string) => {
    setPkg(null);
    setRepo((cur) => (cur !== null && sameOrg(cur, r) && pkg === null ? null : r));
  };
  const pickPackage = (r: string, key: string) => {
    const same = repo !== null && sameOrg(repo, r) && pkg === key;
    setRepo(r);
    setPkg(same ? null : key);
  };
  const toggleExpanded = (r: string) =>
    setExpanded((cur) => {
      const next = new Set(cur);
      if (next.has(r)) next.delete(r);
      else next.add(r);
      return next;
    });
  const pkgLabel = (key: string) =>
    key === OUTSIDE ? "outside packages" : key === UNKNOWN ? "files not read" : (pkgDetection?.packages.find((p) => p.path === key)?.name ?? key);

  return (
    <div className="flex flex-col gap-10">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <button type="button" onClick={onBack} className="mb-3 cursor-pointer border-0 bg-transparent p-0 text-[13px] text-muted hover:text-text">
            ← All workspaces
          </button>
          <h1 className="m-0 flex items-center gap-3 text-[30px] font-semibold leading-[1.15] tracking-[-0.035em]">
            <OrgTile org={org.org} avatar={org.avatar} size={28} />
            <span className="truncate">{org.org}</span>
          </h1>
          {org.description && <p data-org-description className="m-0 mt-2 max-w-[640px] text-[14px] leading-snug text-text/80 [text-wrap:pretty]">{org.description}</p>}
          <div className="mt-2 text-[14px] text-muted">
            {counts.live} live · {counts["need you"]} need you · {queued} queued · {repos.length} {repos.length === 1 ? "repo" : "repos"}
            {repo ? ` · filtered to ${shortRepo(repo)}${pkg ? ` / ${pkgLabel(pkg)}` : ""}` : ""}
          </div>
        </div>
        {toolbar}
      </div>

      <div role="group" aria-label="Repository" className="-mt-4 flex flex-wrap gap-1.5">
        <PillTab active={repo === null} label="all" count={sessions.length} title="Show every repository" onClick={() => setRepo(null)} />
        {repos.map((r) => (
          <PillTab key={r.repo} active={repo !== null && sameOrg(repo, r.repo)} label={shortRepo(r.repo)} count={r.colonies} title={r.repo} onClick={() => pickRepo(r.repo)} />
        ))}
      </div>

      <KpiStrip items={kpis} note={errNote} />

      <ChartSection
        title="Colony outcomes"
        legend={<DashLegend items={[...OUTCOMES, ...(ghost ? [{ label: `launches, prev ${range}d`, color: "transparent", dashed: true }] : [])]} />}
        chart={<AreaChart series={outcomeSeries} labels={labels} ghost={ghost} format={(v) => String(Math.round(v))} readTitle={`Last ${range} days`} />}
        foot={outcomeSub}
        sideTitle="Delivery funnel"
        side={[
          { label: "Colonies launched", value: funnel.launched, note: pctOf(funnel.launched), share: shareOf(funnel.launched), color: "var(--out-stopped)" },
          { label: "PR opened", value: funnel.prOpened, note: pctOf(funnel.prOpened), share: shareOf(funnel.prOpened), color: "var(--out-pr)" },
          { label: "CI green", value: "—", note: "no data source yet", share: 0, color: "var(--panel-3)", title: "the API serves no CI results" },
          { label: "Merged", value: funnel.merged, note: pctOf(funnel.merged), share: shareOf(funnel.merged), color: "var(--out-merged)" },
        ]}
        sideFoot={funnelNote}
      />

      <ChartSection
        title={modelsPriced ? "Spend by model" : "Tokens by model"}
        legend={topModels.length > 0 ? <DashLegend items={topModels.map((m) => ({ label: m.split("/").pop() ?? m, color: modelColorFor(m) }))} /> : undefined}
        chart={
          <AreaChart
            series={topModels.map((model) => ({
              label: model.split("/").pop() ?? model,
              color: modelColorFor(model),
              values: days.map((d) => (modelsPriced ? costByDay : tokensByDay).get(d)?.get(model) ?? 0),
            }))}
            labels={labels}
            ghost={modelsPriced ? spendGhost : undefined}
            format={(v) => (modelsPriced ? formatCost(v) : formatTokens(v))}
            formatY={(v) => (modelsPriced ? (v >= 1000 ? `$${(v / 1000).toFixed(1)}k` : `$${Math.round(v)}`) : formatTokens(v))}
            readTitle={`Last ${range} days`}
            emptyNote="no model spend in range"
          />
        }
        foot={`${spendSub}${modelsPriced ? "" : " · per-model prices unknown (Claude Code reports cost per colony; set prices on routed providers), so the chart shows tokens"} · ${latCaption}`}
        sideTitle="Token mix"
        side={mix.shown.map((m) => ({
          label: m.model,
          value: formatTokens(m.tokens),
          note: mixTotal > 0 ? `${Math.round((m.tokens / mixTotal) * 100)}%` : "—",
          share: mixTotal > 0 ? (m.tokens / mixTotal) * 100 : 0,
          color: modelColorFor(m.model),
          title: `${m.model} · ${formatCost(costByModel.get(m.model) ?? null)}`,
        }))}
        sideFoot={
          mix.shown.length > 0
            ? `${spendTokens != null ? `${formatTokens(spendTokens)} tokens · ` : ""}${formatCost(rollup ?? periodSpend)} · org total${mix.more > 0 ? ` · +${mix.more} more` : ""}`
            : "no measured usage"
        }
      />

      <Section
        title={codeTab === "packages" ? "Packages" : "Repositories"}
        meta={codeTab === "packages" ? "published · dependencies · supply chain" : `${range}d · Click a row to filter the dashboard`}
        right={
          <div role="tablist" aria-label="repositories or packages" className="flex rounded-lg border border-border p-0.5">
            {(["repositories", "packages"] as const).map((t) => (
              <button
                key={t}
                type="button"
                role="tab"
                aria-selected={codeTab === t}
                onClick={() => setCodeTab(t)}
                className={`cursor-pointer rounded-md border-0 px-3 py-1 text-[12.5px] ${codeTab === t ? "bg-panel-3 text-text" : "bg-transparent text-muted hover:text-text"}`}
              >
                {t === "repositories" ? "Repositories" : "Packages"}
              </button>
            ))}
          </div>
        }
      >
        {codeTab === "packages" ? (
          <PackagesView org={org.org} onOpenColony={onOpenColony} />
        ) : (
        <Rules>
          {repos.length === 0 ? (
            <div className="py-3.5 text-[13px] text-faint">No colonies right now.</div>
          ) : (
            <div className="overflow-x-auto">
              <div className="min-w-[640px]">
                <div className={`${REPO_GRID} border-b border-border py-2.5 text-[12.5px] text-muted`}>
                  <span>Repository</span>
                  <span className="text-right">Colonies</span>
                  <span>Merge rate</span>
                  <span className="text-right">Fail</span>
                  <span className="text-right">Spend</span>
                  <span className="text-right">$ / PR</span>
                </div>
                {repos.map((r) => {
                  const active = repo !== null && sameOrg(repo, r.repo) && pkg === null;
                  const mine = sessions.filter((s) => sameOrg(s.repo, r.repo));
                  const failedHere = mine.filter((s) => s.status === "failed").length;
                  const detection = detections[r.repo];
                  const mono = detection?.monorepo === true;
                  const open = mono && expanded.has(r.repo);
                  return (
                    <Fragment key={r.repo}>
                      <RepoTableRow
                        name={shortRepo(r.repo)}
                        title={`Filter the dashboard to ${r.repo}`}
                        active={active}
                        colonies={r.colonies}
                        rate={r.rate}
                        failed={failedHere}
                        merged={r.merged}
                        spend={r.spend}
                        onClick={() => pickRepo(r.repo)}
                        mono={
                          mono
                            ? {
                                open,
                                count: detection.packages.length,
                                tool: detection.tool,
                                onToggle: () => toggleExpanded(r.repo),
                              }
                            : undefined
                        }
                      />
                      {open &&
                        packageRows(mine, detection).map((p) => (
                          <PackageTableRow
                            key={p.key}
                            row={p}
                            active={repo !== null && sameOrg(repo, r.repo) && pkg === p.key}
                            onClick={() => pickPackage(r.repo, p.key)}
                          />
                        ))}
                    </Fragment>
                  );
                })}
              </div>
            </div>
          )}
        </Rules>
        )}
      </Section>

      <Section
        title="Colonies"
        meta={String(colonies.length)}
        right={
          colonies.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              <SearchBox value={colonyList.query} onChange={colonyList.setQuery} placeholder="Search colonies…" label="search colonies" className="w-full sm:w-52" />
              <FilterSelect
                label="status"
                allLabel="Any status"
                value={colonyList.filters.status}
                onChange={(v) => colonyList.setFilters({ status: v })}
                options={optionsBy(colonies, (s) => s.status, (k) => SESSION_STATUS[k as SessionStatus]?.label ?? k)}
              />
              <FilterSelect label="repository" allLabel="All repositories" value={colonyList.filters.repo} onChange={(v) => colonyList.setFilters({ repo: v })} options={optionsBy(colonies, (s) => s.repo, shortRepo)} />
              <FilterSelect label="agent" allLabel="All agents" value={colonyList.filters.agent} onChange={(v) => colonyList.setFilters({ agent: v })} options={optionsBy(colonies, (s) => s.agent)} />
            </div>
          ) : undefined
        }
      >
        <Rules>
          {colonies.length === 0 ? (
            <div className="py-3.5 text-[13px] text-faint">No colonies right now.</div>
          ) : colonyList.total === 0 ? (
            <div className="py-3.5 text-[13px] text-muted">
              nothing matches this search and these filters ·{" "}
              <button type="button" onClick={colonyList.reset} className="cursor-pointer border-0 bg-transparent p-0 font-medium text-text underline underline-offset-[3px]">
                clear filters ×
              </button>
            </div>
          ) : (
            <div className="overflow-x-auto">
              <div className="min-w-[640px]">
                {colonyList.rows.map((s) => (
                  <ColonyRow
                    key={s.id}
                    session={s}
                    showOrg={false}
                    age={timeAgo(s.last_activity_at ?? s.updated_at)}
                    flashed={events ? isFlashed(events, s.id, nowMs) : false}
                    bumped={events ? isBumped(events, s.id, nowMs) : false}
                    onOpen={onOpenColony}
                  />
                ))}
              </div>
            </div>
          )}
          {colonyList.total > PAGE_SIZE && <Pagination view={colonyList} onPage={colonyList.setPage} noun="colonies" className="-mt-px border-t border-border py-2.5" />}
        </Rules>
      </Section>

      <div className="-mt-6 text-[12.5px] text-faint">
        {formatTokens(sumTokens(current, org.org))} tokens in range · per-day latency and coverage have no API to read from (the API serves no coverage).
      </div>
    </div>
  );
}

/** The repositories table's grid, shared by its header and rows. */
const REPO_GRID = "grid grid-cols-[minmax(0,1.4fr)_72px_minmax(0,1.4fr)_64px_84px_72px] items-center gap-4";

/** Fetches each repository's monorepo detection once per mount (the mothership caches it). Tests
 *  pin the map instead: static markup has no ApiContext. */
function useRepoPackages(sessions: Session[], pinned?: Record<string, RepoPackages>): Record<string, RepoPackages> {
  const api = useContext(ApiContext);
  const [found, setFound] = useState<Record<string, RepoPackages>>(pinned ?? {});
  const repos = [...new Set(sessions.map((s) => s.repo))].sort().join(",");
  useEffect(() => {
    if (pinned || !api) return;
    let cancelled = false;
    for (const r of repos.split(",").filter(Boolean)) {
      api
        .repoPackages(r)
        .then((d) => {
          if (!cancelled) setFound((cur) => ({ ...cur, [r]: d }));
        })
        .catch(() => {});
    }
    return () => {
      cancelled = true;
    };
  }, [api, repos, pinned]);
  return found;
}

function failCell(failed: number, merged: number): { text: string; tone: string; title: string } {
  const decided = failed + merged;
  const fr = decided > 0 ? failed / decided : null;
  return {
    text: fr != null ? `${(fr * 100).toFixed(1)}%` : "—",
    tone: fr != null && fr > 0.08 ? "text-err" : "text-muted",
    title: fr != null ? `${failed} failed of ${decided} decided (merged+failed)` : "nothing decided",
  };
}

function RateBar({ rate }: { rate: number }): ReactElement {
  return (
    <span className="flex items-center gap-2.5">
      <span className="h-1 flex-1 overflow-hidden rounded-sm bg-panel-3">
        <span className="block h-full transition-[width] duration-700" style={{ width: `${Math.round(rate * 100)}%`, background: "var(--chart-1)" }} />
      </span>
      <span className="w-10 text-right">{`${Math.round(rate * 100)}%`}</span>
    </span>
  );
}

/** A repository's row; a monorepo's carries a chevron that opens its package rows. */
function RepoTableRow({
  name,
  title,
  active,
  colonies,
  rate,
  failed,
  merged,
  spend,
  onClick,
  mono,
}: {
  name: string;
  title: string;
  active: boolean;
  colonies: number;
  rate: number;
  failed: number;
  merged: number;
  spend: number | null;
  onClick: () => void;
  mono?: { open: boolean; count: number; tool: string | null; onToggle: () => void };
}): ReactElement {
  const fail = failCell(failed, merged);
  return (
    <div
      role="button"
      tabIndex={0}
      aria-pressed={active}
      onClick={onClick}
      onKeyDown={(e: KeyboardEvent<HTMLDivElement>) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onClick();
        }
      }}
      title={title}
      className={`${REPO_GRID} -mt-px w-full cursor-pointer border-0 border-t border-solid border-border py-3.5 text-left text-[13.5px] tabular-nums text-text hover:bg-panel-2 focus-visible:outline-2 focus-visible:outline-accent ${active ? "bg-panel-2" : "bg-transparent"}`}
    >
      <span className="flex min-w-0 items-center gap-2">
        {mono && (
          <button
            type="button"
            aria-expanded={mono.open}
            aria-label={`${mono.open ? "hide" : "show"} the ${mono.count} packages in ${name}`}
            onClick={(e) => {
              e.stopPropagation();
              mono.onToggle();
            }}
            className="-ml-1 grid size-5 shrink-0 cursor-pointer place-items-center rounded border-0 bg-transparent text-muted hover:bg-panel-3 hover:text-text"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className={`transition-transform ${mono.open ? "rotate-90" : ""}`}>
              <path d="m9 6 6 6-6 6" />
            </svg>
          </button>
        )}
        <span className="min-w-0 truncate font-mono text-[13px]">{name}</span>
        {mono && (
          <span className="shrink-0 rounded-full border border-border px-1.5 py-px text-[11px] text-muted" title={mono.tool ? `detected from ${mono.tool}` : undefined}>
            monorepo · {mono.count} packages
          </span>
        )}
      </span>
      <span className="text-right text-muted">{colonies}</span>
      <RateBar rate={rate} />
      <span className={`text-right ${fail.tone}`} title={fail.title}>
        {fail.text}
      </span>
      <span className="text-right">{formatCost(spend)}</span>
      <span className="text-right text-muted">{formatCost(costPerMerged(spend, merged))}</span>
    </div>
  );
}

/** One package of an expanded monorepo, indented under its repository. */
function PackageTableRow({ row, active, onClick }: { row: PackageRow; active: boolean; onClick: () => void }): ReactElement {
  const fail = failCell(row.failed, row.merged);
  const special = row.path === null;
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      title={
        special
          ? row.key === OUTSIDE
            ? "Colonies that changed files outside every package (root config, docs, CI)"
            : "Colonies whose pull-request file list has not been read yet"
          : `Filter the dashboard to ${row.path} — a colony that touched several packages counts in each`
      }
      className={`${REPO_GRID} -mt-px w-full cursor-pointer border-0 border-t border-dashed border-border py-2.5 text-left text-[13px] tabular-nums text-text hover:bg-panel-2 ${active ? "bg-panel-2" : "bg-transparent"}`}
    >
      <span className="flex min-w-0 items-center gap-2 pl-7">
        <span aria-hidden="true" className="h-3 w-2 shrink-0 border-b border-l border-border-strong" />
        <span className={`min-w-0 truncate ${special ? "italic text-muted" : "font-mono text-[12.5px]"}`}>{row.name}</span>
        {row.path && <span className="min-w-0 truncate text-[11.5px] text-faint">{row.path}</span>}
      </span>
      <span className="text-right text-muted">{row.colonies}</span>
      <RateBar rate={row.rate} />
      <span className={`text-right ${fail.tone}`} title={fail.title}>
        {fail.text}
      </span>
      <span className="text-right">{formatCost(row.spend)}</span>
      <span className="text-right text-muted">{formatCost(costPerMerged(row.spend, row.merged))}</span>
    </button>
  );
}
