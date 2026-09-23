// Per-org dashboard (issue #398): delivery KPIs for one workspace from real data only —
// SpendHistory (launched/returned/spend per day, models per day) plus the session list
// (statuses for the funnel, repos and costs for the drill-down). Anything without a source
// (lead time, CI pass, coverage, latency p50/p95, MTTR) is omitted, not estimated.
import type { ReactElement } from "react";

import { Avatar } from "../components/Avatar";
import { sameOrg } from "../components/ui";
import type { OrgEntry } from "../orgs";
import { formatCost, formatTokens, modelMix, orgCost } from "../spend";
import type { Session, SpendHistory } from "../types";
import { DashBars, KpiTile, type KpiDef } from "./DashChart";
import {
  RANGES,
  DASH_COLORS,
  costPerMerged,
  dailyCosts,
  dailyLaunched,
  dailyReturned,
  formatDelta,
  funnelFor,
  relDelta,
  repoRows,
  slicePeriods,
  sparkPoints,
  sumHistoryCost,
  sumLaunched,
  sumReturned,
  type RangeDays,
} from "./dash";
import { overviewCounts } from "./feed";

export function RangePicker({
  range,
  onRange,
  compare,
  onCompare,
}: {
  range: RangeDays;
  onRange: (range: RangeDays) => void;
  compare: boolean;
  onCompare: () => void;
}): ReactElement {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <div className="flex rounded-[10px] border border-border bg-panel p-[3px]" role="group" aria-label="Range">
        {RANGES.map((v) => (
          <button
            key={v}
            type="button"
            aria-pressed={range === v}
            onClick={() => onRange(v)}
            className={`cursor-pointer rounded-[7px] px-3 py-[5px] font-mono text-[11.5px] ${range === v ? "bg-accent-soft text-accent" : "text-muted hover:text-text"}`}
          >
            {v}d
          </button>
        ))}
      </div>
      <button
        type="button"
        aria-pressed={compare}
        onClick={onCompare}
        className="cursor-pointer rounded-[10px] border border-border bg-panel px-3 py-[7px] text-xs text-muted hover:text-text"
      >
        Compare to previous {range}d
      </button>
    </div>
  );
}

export function OrgDashboard({
  org,
  sessions,
  history,
  range,
  compare,
  onBack,
}: {
  org: OrgEntry;
  /** This org's visible sessions (already filtered by the caller). */
  sessions: Session[];
  history: SpendHistory | null;
  range: RangeDays;
  compare: boolean;
  onBack: () => void;
}): ReactElement {
  const { current, previous } = slicePeriods(history, range);
  const launched = sumLaunched(current, org.org);
  const returned = sumReturned(current, org.org);
  const spend = sumHistoryCost(current, org.org);
  const counts = overviewCounts(sessions);
  const merged = sessions.filter((s) => s.status === "merged").length;
  const rollup = orgCost(org.spend);
  const cpp = costPerMerged(rollup ?? spend, merged);
  const funnel = funnelFor(sessions);
  const repos = repoRows(sessions);
  const mix = modelMix(org.spend?.models, 4);

  const kpis: KpiDef[] = [
    { label: "LAUNCHED", value: String(launched), delta: compare ? formatDelta(relDelta(launched, sumLaunched(previous, org.org))) : undefined, spark: sparkPoints(dailyLaunched(current, org.org)), hint: "colonies launched per day, GET /api/spend/history" },
    { label: "RETURNED", value: String(returned), delta: compare ? formatDelta(relDelta(returned, sumReturned(previous, org.org))) : undefined, spark: sparkPoints(dailyReturned(current, org.org)), hint: "colonies that returned per day, GET /api/spend/history" },
    { label: "MERGED", value: String(merged), hint: "sessions with status merged, GET /api/sessions" },
    { label: "SPEND", value: formatCost(spend), delta: compare ? formatDelta(relDelta(spend, sumHistoryCost(previous, org.org))) : undefined, spark: sparkPoints(dailyCosts(current, org.org)), hint: "measured spend in range, GET /api/spend/history" },
    { label: "COST / MERGED PR", value: formatCost(cpp), hint: "org spend rollup ÷ merged sessions" },
  ];

  // Spend/day by model, aggregated in one pass: tokens pick the top models, cost/day fills the stacks.
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
  const days = current.map((d) => d.day);
  const prevCosts = dailyCosts(previous, org.org);
  const ghost = compare && previous.length > 0 ? current.map((_, i) => prevCosts[i] ?? null) : undefined;

  const funnelRows: [string, number][] = [["Colonies launched", funnel.launched], ["PR opened", funnel.prOpened], ["Merged", funnel.merged]];

  return (
    <div className="flex flex-col gap-5">
      <div>
        <button type="button" onClick={onBack} className="cursor-pointer font-mono text-[11.5px] text-accent hover:underline">
          ← overview
        </button>
        <div className="mt-2 flex items-center gap-3">
          <Avatar name={org.org} src={org.avatar} size={40} rounded="xl" />
          <div>
            <div className="mb-1 font-mono text-[10.5px] tracking-[0.12em] text-faint">ORG DASHBOARD</div>
            <div className="text-[20px] font-semibold tracking-tight">{org.org}</div>
            <div className="font-mono text-[11.5px] text-muted">
              {counts.live} live · {counts["need you"]} need you · {counts.queued} queued
            </div>
          </div>
        </div>
      </div>

      <div className="grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
        {kpis.map((k) => (
          <KpiTile key={k.label} label={k.label} value={k.value} delta={k.delta} spark={k.spark} hint={k.hint} />
        ))}
      </div>

      <div className="flex flex-wrap gap-3.5">
        <section className="min-w-0 flex-[2_1_320px] rounded-2xl border border-border bg-panel p-4">
          <div className="mb-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">OUTCOMES PER DAY · LAUNCHED VS RETURNED</div>
          <DashBars
            series={[
              { label: "launched", color: "var(--info)", values: dailyLaunched(current, org.org) },
              { label: "returned", color: "var(--ok)", values: dailyReturned(current, org.org) },
            ]}
            labels={days}
            ghost={ghost}
            format={(v) => String(Math.round(v))}
          />
        </section>
        <section className="min-w-0 flex-[2_1_320px] rounded-2xl border border-border bg-panel p-4">
          <div className="mb-2 font-mono text-[10.5px] tracking-[0.12em] text-faint">SPEND PER DAY · BY MODEL</div>
          {topModels.length > 0 ? (
            <DashBars
              series={topModels.map((model, i) => ({ label: model.split("/").pop() ?? model, color: DASH_COLORS[i % DASH_COLORS.length], values: days.map((d) => costByDay.get(d)?.get(model) ?? 0) }))}
              labels={days}
              ghost={ghost}
              format={(v) => formatCost(v)}
            />
          ) : (
            <div className="py-6 text-center font-mono text-[11px] text-faint">no model spend in range</div>
          )}
        </section>
        <section className="flex min-w-0 flex-[1_1_220px] flex-col gap-3 rounded-2xl border border-border bg-panel p-4">
          <div className="font-mono text-[10.5px] tracking-[0.12em] text-faint">DELIVERY FUNNEL</div>
          {funnelRows.map(([label, value]) => (
            <div key={label} className="flex flex-col gap-1.5">
              <div className="flex items-baseline justify-between gap-2 text-[13px]">
                <span>{label}</span>
                <span className="font-mono text-xs tabular-nums">{value}</span>
              </div>
              <div className="h-2.5 overflow-hidden rounded-full bg-panel-3">
                <div className="h-full rounded-full bg-accent" style={{ width: `${funnel.launched > 0 ? (value / funnel.launched) * 100 : 0}%` }} />
              </div>
            </div>
          ))}
          {mix.shown.length > 0 && (
            <div className="mt-auto border-t border-border pt-3 font-mono text-[11px] text-faint">
              {mix.shown.map((m) => `${m.model.split("/").pop()} ${formatTokens(m.tokens)}`).join(" · ")}
              {mix.more > 0 && ` · +${mix.more} more`}
            </div>
          )}
        </section>
      </div>

      <section className="overflow-hidden rounded-2xl border border-border bg-panel">
        <div className="border-b border-border px-4 py-3.5 font-mono text-[10.5px] tracking-[0.12em] text-faint">REPOSITORIES</div>
        {repos.length === 0 ? (
          <div className="px-4 py-3.5 text-[13px] text-faint">No colonies right now.</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[520px] border-collapse text-[13px] tabular-nums">
              <thead>
                <tr className="border-b border-border font-mono text-[10px] tracking-[0.08em] text-faint">
                  <th className="px-4 py-2 text-left font-medium">REPO</th>
                  <th className="px-4 py-2 text-right font-medium">COLONIES</th>
                  <th className="px-4 py-2 text-right font-medium">MERGED</th>
                  <th className="px-4 py-2 text-right font-medium">MERGE RATE</th>
                  <th className="px-4 py-2 text-right font-medium">SPEND</th>
                  <th className="px-4 py-2 text-right font-medium">$/PR</th>
                </tr>
              </thead>
              <tbody>
                {repos.map((r) => (
                  <tr key={r.repo} className="border-b border-border last:border-b-0">
                    <td className="truncate px-4 py-2.5 font-semibold">{r.repo}</td>
                    <td className="px-4 py-2.5 text-right font-mono text-xs">{r.colonies}</td>
                    <td className="px-4 py-2.5 text-right font-mono text-xs">{r.merged}</td>
                    <td className="px-4 py-2.5 text-right font-mono text-xs">{`${Math.round(r.rate * 100)}%`}</td>
                    <td className="px-4 py-2.5 text-right font-mono text-xs">{formatCost(r.spend)}</td>
                    <td className="px-4 py-2.5 text-right font-mono text-xs text-muted">{formatCost(costPerMerged(r.spend, r.merged))}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
