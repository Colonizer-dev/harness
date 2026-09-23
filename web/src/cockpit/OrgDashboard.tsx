// Per-org dashboard (issue #398): the Claude Design "Cockpit Dashboards" org render, backed
// only by real data — Session (statuses, created_at, cost, repo), /api/spend/history (per-org
// per-day launched/returned/cost/models) and optional cumulative provider tallies. Anything
// without a source (lead time, PR cycle time, CI pass/CI-green, coverage, time to recover, per-day
// latency, lead-time histogram) renders an honest empty state in the same shape, never a guess.
import { useState, type ReactElement } from "react";

import { Avatar, initialOf } from "../components/Avatar";
import { SESSION_STATUS, sameOrg, timeAgo, type Tone } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { formatCost, formatTokens, modelMix, orgCost, sessionCost } from "../spend";
import type { Session, SpendHistory } from "../types";
import { DashBars, DashLegend, DashLine, DashPanel, FilterChip, KpiTile, ShareBar, type KpiDef } from "./DashChart";
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
  orgColorFor,
  relDelta,
  repoRows,
  shortDayLabel,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumTokens,
  TONE_VAR,
  type ProviderErrorSnapshot,
  type RangeDays,
} from "./dash";
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
}): ReactElement {
  // The repository filter scopes the whole dashboard below the header: every session-backed
  // figure reads `scoped`. Spend history is per org per day, so it — and the ghost line drawn
  // from it — stays org-wide; the spend panel's sub-line says so while a repo is selected.
  const [repo, setRepo] = useState<string | null>(initialRepo);
  const scoped = repo ? sessions.filter((s) => sameOrg(s.repo, repo)) : sessions;

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
  const kpis: KpiDef[] = [
    {
      label: "MERGED PRS",
      value: String(merged),
      delta: mergedDelta != null ? formatDelta(mergedDelta) : undefined,
      deltaTone: deltaTone(mergedDelta),
      deltaDir: (mergedDelta ?? 0) < 0 ? "down" : "up",
      spark: mergedDaily.length > 0 ? sparkPoints(mergedDaily) : undefined,
      sub: scoped.length > 0 ? `${Math.round((merged / scoped.length) * 100)}% of ${scoped.length} ${scoped.length === 1 ? "colony" : "colonies"}` : "no colonies in scope",
      hint: "sessions with status merged, GET /api/sessions — the delta reads merged in range vs the previous period (bucketed by merge date, falling back to created_at)",
    },
    { label: "LEAD TIME", value: "—", emptyNote: "no data source yet", hint: "needs issue-picked-up → PR-opened timestamps; the API serves none" },
    { label: "PR CYCLE TIME", value: "—", emptyNote: "no data source yet", hint: "needs PR-opened timestamps; the API serves merged_at but no PR-opened time" },
    {
      label: "CHANGE FAILURE RATE",
      ...(scoped.length > 0
        ? {
            value: `${((failed / scoped.length) * 100).toFixed(1)}%`,
            delta: failDelta != null ? formatPts(failDelta) : undefined,
            deltaTone: deltaTone(failDelta, "down"),
            deltaDir: (failDelta ?? 0) < 0 ? "down" : "up",
            spark: days.length > 0 ? sparkPoints(dailyFailRate(scoped, days)) : undefined,
            sub: `${failed} failed of ${scoped.length}`,
          }
        : { value: "—", emptyNote: "no colonies in scope" }),
      hint: "failed sessions ÷ colonies in scope, GET /api/sessions — the spark and delta read the in-range window (merged by merge day, failed by created day)",
    },
    { label: "TIME TO RECOVER", value: "—", emptyNote: "no data source yet", hint: "needs failure → recovery timestamps; the API serves none" },
    { label: "CI PASS RATE", value: "—", emptyNote: "no data source yet", hint: "the API serves no CI results" },
    {
      label: "COST PER MERGED PR",
      ...(merged > 0
        ? {
            value: formatCost(cpp),
            delta: cppDelta != null ? formatDelta(cppDelta) : undefined,
            deltaTone: deltaTone(cppDelta, "down"),
            deltaDir: (cppDelta ?? 0) < 0 ? "down" : "up",
            spark: days.length > 0 ? sparkPoints(cppDaily) : undefined,
            sub: `${merged} merged`,
          }
        : { value: "—", emptyNote: "nothing merged yet" }),
      hint: "org spend rollup ÷ merged sessions, GET /api/orgs + GET /api/sessions — the delta reads period spend ÷ merged in range vs the previous period",
    },
    {
      label: "API ERROR RATE",
      ...(errRate != null
        ? { value: `${(errRate * 100).toFixed(2)}%`, sub: `${formatTokens(totalReq)} calls${since ? ` · since ${since}` : ""}` }
        : { value: "—", emptyNote: "no data source yet" }),
      hint: "cumulative provider failures ÷ requests, GET /api/status model_providers (not range-bound)",
    },
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
  for (const d of current) {
    const o = d.orgs.find((e) => sameOrg(e.org, org.org));
    if (!o) continue;
    const costs = new Map<string, number>();
    for (const m of o.models) {
      tokByModel.set(m.model, (tokByModel.get(m.model) ?? 0) + m.tokens);
      costs.set(m.model, m.cost_usd ?? 0);
    }
    costByDay.set(d.day, costs);
  }
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

  return (
    <div className="flex flex-col gap-5">
      <div>
        <button type="button" onClick={onBack} className="cursor-pointer font-mono text-[11.5px] text-accent hover:underline">
          ← overview
        </button>
        <div className="mt-2 flex items-center gap-3">
          {org.avatar ? (
            <Avatar name={org.org} src={org.avatar} size={44} rounded="xl" />
          ) : (
            <span
              aria-hidden="true"
              className="grid h-[44px] w-[44px] shrink-0 select-none place-items-center rounded-xl text-[18px] font-bold leading-none"
              style={{ background: orgColorFor(org.org), color: "var(--term-bg)" }}
            >
              {initialOf(org.org)}
            </span>
          )}
          <div className="min-w-0">
            <div className="mb-1 font-mono text-[10.5px] tracking-[0.12em] text-faint">ORG DASHBOARD</div>
            <div className="truncate text-[22px] font-semibold tracking-tight">{org.org}</div>
            <div className="font-mono text-[11.5px] text-muted">
              {counts.live} live · {counts["need you"]} need you · {queued} queued · {repos.length} {repos.length === 1 ? "repo" : "repos"}
            </div>
          </div>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1.5">
        <span className="mr-1.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">REPOSITORY</span>
        <FilterChip active={repo === null} count={sessions.length} label="All" title="Show every repository" onClick={() => setRepo(null)} />
        {repos.map((r) => (
          <FilterChip
            key={r.repo}
            active={repo !== null && sameOrg(repo, r.repo)}
            count={r.colonies}
            label={shortRepo(r.repo)}
            title={r.repo}
            onClick={() => setRepo((cur) => (cur !== null && sameOrg(cur, r.repo) ? null : r.repo))}
          />
        ))}
      </div>

      <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(200px,1fr))]">
        {kpis.map((k) => (
          <KpiTile key={k.label} {...k} />
        ))}
      </div>

      <div className="flex flex-wrap gap-3.5">
        <DashPanel title="COLONY OUTCOMES PER DAY" sub={outcomeSub} legend={<DashLegend items={[...OUTCOMES]} />} className="flex-[2_1_480px]">
          <DashBars
            series={outcomeSeries}
            labels={days}
            ghost={ghost}
            format={(v) => String(Math.round(v))}
            formatY={(v) => String(Math.round(v))}
            xLabels={days.map(shortDayLabel)}
          />
        </DashPanel>
        <DashPanel title="DELIVERY FUNNEL" sub="From issue picked up to PR merged" className="flex flex-[1_1_300px] flex-col gap-3.5">
          {(
            [
              { label: "Colonies launched", value: funnel.launched, color: "var(--out-stopped)" },
              { label: "PR opened", value: funnel.prOpened, color: "var(--out-pr)" },
            ] as const
          ).map((step) => (
            <div key={step.label} className="flex flex-col gap-1.5">
              <div className="flex items-baseline justify-between gap-2 text-[13px]">
                <span>{step.label}</span>
                <span className="font-mono text-xs tabular-nums">
                  {step.value} <span className="text-faint">{funnel.launched > 0 ? `${Math.round((step.value / funnel.launched) * 100)}%` : ""}</span>
                </span>
              </div>
              <div className="h-2.5 overflow-hidden rounded-full bg-panel-3">
                <div className="h-full rounded-full" style={{ width: `${funnel.launched > 0 ? (step.value / funnel.launched) * 100 : 0}%`, background: step.color }} />
              </div>
            </div>
          ))}
          <div className="flex flex-col gap-1.5" title="the API serves no CI results">
            <div className="flex items-baseline justify-between gap-2 text-[13px]">
              <span>CI green</span>
              <span className="font-mono text-xs tabular-nums">—</span>
            </div>
            <div className="h-2.5 overflow-hidden rounded-full bg-panel-3" />
            <div className="font-mono text-[11px] text-faint">no data source yet</div>
          </div>
          <div className="flex flex-col gap-1.5">
            <div className="flex items-baseline justify-between gap-2 text-[13px]">
              <span>Merged</span>
              <span className="font-mono text-xs tabular-nums">
                {funnel.merged} <span className="text-faint">{funnel.launched > 0 ? `${Math.round((funnel.merged / funnel.launched) * 100)}%` : ""}</span>
              </span>
            </div>
            <div className="h-2.5 overflow-hidden rounded-full bg-panel-3">
              <div className="h-full rounded-full" style={{ width: `${funnel.launched > 0 ? (funnel.merged / funnel.launched) * 100 : 0}%`, background: "var(--out-merged)" }} />
            </div>
          </div>
          <div className="mt-auto border-t border-border pt-3 font-mono text-[11px] text-faint">{funnelNote}</div>
        </DashPanel>
      </div>

      <div className="flex flex-wrap gap-3.5">
        <DashPanel
          title="SPEND PER DAY · BY MODEL"
          sub={spendSub}
          legend={topModels.length > 0 ? <DashLegend items={topModels.map((m) => ({ label: m.split("/").pop() ?? m, color: modelColorFor(m) }))} /> : undefined}
          className="flex-[2_1_480px]"
        >
          {topModels.length > 0 ? (
            <DashBars
              series={topModels.map((model) => ({ label: model.split("/").pop() ?? model, color: modelColorFor(model), values: days.map((d) => costByDay.get(d)?.get(model) ?? 0) }))}
              labels={days}
              ghost={spendGhost}
              format={(v) => formatCost(v)}
              formatY={(v) => `$${Math.round(v)}`}
              xLabels={days.map(shortDayLabel)}
            />
          ) : (
            <div className="py-6 text-center font-mono text-[11px] text-faint">no model spend in range</div>
          )}
        </DashPanel>
        <DashPanel
          title="MODEL API LATENCY"
          sub={latCaption}
          legend={<DashLegend items={[{ label: "p50", color: "var(--lat-p50)" }, { label: "p95", color: "var(--lat-p95)" }]} />}
          className="flex-[1_1_300px]"
        >
          <DashLine
            series={[
              { label: "p50", color: "var(--lat-p50)", values: days.map(() => null), fill: true },
              { label: "p95", color: "var(--lat-p95)", values: days.map(() => null), fill: true },
            ]}
            labels={days}
            format={(v) => formatLatency(v)}
            formatY={(v) => formatLatency(v)}
            xLabels={days.map(shortDayLabel)}
          />
        </DashPanel>
      </div>

      <div className="grid gap-3.5 [grid-template-columns:repeat(auto-fit,minmax(min(100%,420px),1fr))]">
        <DashPanel title="LEAD TIME DISTRIBUTION" sub="Issue picked up → PR opened">
          <div className="grid grid-cols-6 gap-2" aria-hidden="true">
            {["<15m", "15–30m", "30–60m", "1–2h", "2–4h", "4h+"].map((label) => (
              <div key={label} className="text-center font-mono text-[10.5px] text-faint">
                {label}
              </div>
            ))}
          </div>
          <div className="py-8 text-center font-mono text-[11px] text-faint">no data source yet · no pickup or PR-opened timestamps in the API</div>
        </DashPanel>
        <DashPanel
          title="TOKEN USAGE · MODEL MIX"
          sub={
            spendTokens != null
              ? `${formatTokens(spendTokens)} tokens · ${formatCost(rollup ?? periodSpend)} · org total`
              : "no measured usage"
          }
        >
          {mix.shown.length > 0 ? (
            <div className="flex flex-col gap-2.5">
              <ShareBar
                segments={mix.shown.map((m) => ({ label: m.model, color: modelColorFor(m.model), value: m.tokens }))}
                format={(v) => `${formatTokens(v)} tokens`}
                label="token usage by model"
              />
              {mix.shown.map((m) => (
                <div key={m.model} className="grid grid-cols-[minmax(0,1fr)_64px_64px_44px] items-center gap-2.5 text-[12.5px]">
                  <span className="flex min-w-0 items-center gap-2">
                    <span aria-hidden="true" className="h-2 w-2 shrink-0 rounded-[2px]" style={{ background: modelColorFor(m.model) }} />
                    <span title={m.model} className="truncate font-mono text-[11.5px]">
                      {m.model}
                    </span>
                  </span>
                  <span className="text-right font-mono text-[11.5px] text-muted">{formatTokens(m.tokens)}</span>
                  <span className="text-right font-mono text-[11.5px]">{formatCost(costByModel.get(m.model) ?? null)}</span>
                  <span className="text-right font-mono text-[11.5px] text-faint">{mixTotal > 0 ? `${Math.round((m.tokens / mixTotal) * 100)}%` : "—"}</span>
                </div>
              ))}
              {mix.more > 0 && <div className="font-mono text-[11px] text-faint">+{mix.more} more</div>}
            </div>
          ) : (
            <div className="py-8 text-center font-mono text-[11px] text-faint">no data source yet</div>
          )}
        </DashPanel>
      </div>

      <section className="overflow-hidden rounded-2xl border border-border bg-panel">
        <div className="flex flex-wrap items-baseline justify-between gap-2 border-b border-border px-4 py-3.5">
          <span className="font-mono text-[10.5px] tracking-[0.12em] text-faint">REPOSITORIES · {range}d</span>
          <span className="text-xs text-faint">Click a row to filter the dashboard</span>
        </div>
        {repos.length === 0 ? (
          <div className="px-4 py-3.5 text-[13px] text-faint">No colonies right now.</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[860px] border-collapse text-[13px] tabular-nums">
              <thead>
                <tr className="border-b border-border font-mono text-[10px] tracking-[0.08em] text-faint">
                  <th className="px-4 py-2 text-left font-medium">REPO</th>
                  <th className="px-4 py-2 text-right font-medium">COLONIES</th>
                  <th className="px-4 py-2 text-left font-medium">MERGE RATE</th>
                  <th className="px-4 py-2 text-right font-medium">LEAD TIME</th>
                  <th className="px-4 py-2 text-right font-medium">PR CYCLE</th>
                  <th className="px-4 py-2 text-right font-medium">CI PASS</th>
                  <th className="px-4 py-2 text-right font-medium">COVERAGE</th>
                  <th className="px-4 py-2 text-right font-medium">SPEND</th>
                  <th className="px-4 py-2 text-right font-medium">$/PR</th>
                </tr>
              </thead>
              <tbody>
                {repos.map((r) => {
                  const active = repo !== null && sameOrg(repo, r.repo);
                  return (
                    <tr
                      key={r.repo}
                      onClick={() => setRepo((cur) => (cur !== null && sameOrg(cur, r.repo) ? null : r.repo))}
                      title={`Filter the dashboard to ${r.repo}`}
                      className={`cursor-pointer border-b border-border last:border-b-0 hover:bg-panel-2 ${active ? "bg-accent-soft" : ""}`}
                    >
                      <td className="max-w-[220px] truncate px-4 py-2.5 font-semibold" title={r.repo}>
                        {shortRepo(r.repo)}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs">{r.colonies}</td>
                      <td className="px-4 py-2.5">
                        <span className="flex items-center gap-2">
                          <span className="h-[5px] min-w-[60px] flex-1 overflow-hidden rounded-full bg-panel-3">
                            <span className="block h-full rounded-full bg-ok" style={{ width: `${Math.round(r.rate * 100)}%` }} />
                          </span>
                          <span className="w-9 text-right font-mono text-xs">{`${Math.round(r.rate * 100)}%`}</span>
                        </span>
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-faint" title="no data source yet">
                        —
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-faint" title="no data source yet">
                        —
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-faint" title="the API serves no CI results">
                        —
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-faint" title="the API serves no coverage">
                        —
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs">{formatCost(r.spend)}</td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">{formatCost(costPerMerged(r.spend, r.merged))}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="overflow-hidden rounded-2xl border border-border bg-panel">
        <div className="border-b border-border px-4 py-3.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">COLONIES · {colonies.length}</div>
        {colonies.length === 0 ? (
          <div className="px-4 py-3.5 text-[13px] text-faint">No colonies right now.</div>
        ) : (
          <div className="overflow-x-auto">
            <div className="min-w-[640px]">
              {colonies.map((s) => {
                const meta = SESSION_STATUS[s.status] ?? { label: s.status, tone: "neutral" as Tone };
                const dot = TONE_VAR[meta.tone];
                const short = `${shortRepo(s.repo)}${s.issue != null ? `#${s.issue}` : ""}`;
                return (
                  <div key={s.id} className="grid grid-cols-[10px_minmax(0,3fr)_150px_80px_72px] items-center gap-3 border-b border-border px-4 py-2.5 text-[13px] last:border-b-0">
                    <span aria-hidden="true" className="h-2 w-2 rounded-full" style={{ background: dot }} />
                    <span className="min-w-0 truncate">
                      {short} <span className="text-faint">{s.issue_title}</span>
                    </span>
                    <span className="truncate font-mono text-[11px]" style={{ color: dot }}>
                      {meta.label}
                    </span>
                    <span className="text-right font-mono text-[11px] text-faint">{timeAgo(s.last_activity_at ?? s.updated_at)}</span>
                    <span className="text-right font-mono text-[11px] tabular-nums">{formatCost(sessionCost(s))}</span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </section>

      <div className="font-mono text-[11px] text-faint">
        {sumTokens(current, org.org)} tokens in range · figures without a source stay "—": lead time, PR cycle time, CI pass rate, time to recover,
        per-day latency, lead-time distribution and coverage have no API to read from.
      </div>
    </div>
  );
}
