// The Model providers list (issue #1204): one quiet row per provider that opens to its details.
// Collapsed, a row is the logo, the name, a status dot, a credits gauge and when it resets; the
// credits chart, the usage sparklines, the models, the wiring and the connection details wait
// behind the row, one short sentence each, in the same card language as the rest of Settings.
import { useEffect, useState, type ReactNode } from "react";

import { Sparkline } from "../../cockpit/DashChart";
import { sparkPoints } from "../../cockpit/dash";
import { useApi } from "../../context";
import type { HarnessStatus, ModelOption, ModelProvider, ModelSetting, PlanUsage, ProviderUsageReport } from "../../types";
import { IconChevron, IconNetwork, IconPencil } from "../icons";
import { ProviderMark } from "../providerMark";
import { Badge, Button, Spinner, cx, timeAgo } from "../ui";
import { formatAvgLatency } from "../../providerHealth";
import { BalanceChart } from "./BalanceChart";
import { KeyBadge } from "./ProviderForm";
import { HealthStatus, type HealthView } from "./HealthStatus";
import { WIRE_LABEL, limitLabels } from "./providerCatalog";
import { claudePlanText, dailySeries, gaugeOf, modelsSummary, offeredModels, providerStatus, resetText, resetUnixOf, type StatusTone } from "./providerOverview";

const DOT: Record<StatusTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", idle: "bg-faint" };
const FILL: Record<StatusTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", idle: "bg-faint" };
const TEXT: Record<StatusTone, string> = { ok: "text-ok", warn: "text-warn", err: "text-err", idle: "text-muted" };

/** The model settings by the names the model form gives them. */
const SETTING_LABEL: Record<ModelSetting, string> = {
  model: "Orchestrator model",
  subagent_model: "Subagent model",
  background_model: "Background model",
  model_low: "Model for small tasks",
  model_high: "Model for large tasks",
};

/** A thin meter of what is left in a plan; dashed when the balance is unknown. */
function GaugeBar({ pct, tone, label }: { pct: number | null; tone: StatusTone; label: string }) {
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct ?? undefined}
      aria-valuetext={pct == null ? "balance unknown" : `${Math.round(pct)}% left`}
      className={cx("h-1.5 w-full overflow-hidden rounded-full", pct == null ? "border border-dashed border-border-strong" : "bg-panel-3")}
    >
      {pct != null && <div className={cx("h-full rounded-full transition-[width] duration-500", FILL[tone])} style={{ width: `${Math.max(pct, 2)}%` }} />}
    </div>
  );
}

/** One expandable row. The summary is the button; its details render only while open. */
function Shell({
  id,
  mark,
  name,
  tag,
  statusLabel,
  tone,
  gauge,
  reset,
  open,
  onToggle,
  children,
}: {
  id: string;
  mark: ReactNode;
  name: string;
  tag?: ReactNode;
  statusLabel: string;
  tone: StatusTone;
  gauge: { pct: number | null; text: string; tone: StatusTone };
  reset: string | null;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  const panel = `provider-${id}-panel`;
  return (
    <div data-provider={id} data-open={open} className="min-w-0">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panel}
        onClick={onToggle}
        className="flex w-full cursor-pointer items-center gap-3 px-3.5 py-3 text-left transition-colors hover:bg-panel-2"
      >
        {mark}
        <span className="min-w-0 flex-1">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate text-body-lg font-semibold">{name}</span>
            <span className="hidden sm:contents">{tag}</span>
          </span>
          {/* Phone: the gauge and the reset sit under the name instead of in their own columns. */}
          <span className="mt-1.5 flex items-start gap-2.5 sm:hidden">
            {gauge.pct != null && (
              <span className="mt-[7px] w-12 shrink-0">
                <GaugeBar pct={gauge.pct} tone={gauge.tone} label={`${name} plan left`} />
              </span>
            )}
            <span className={cx("min-w-0 text-small leading-5 tabular-nums", TEXT[gauge.tone])}>{[gauge.text, reset].filter(Boolean).join(" · ")}</span>
            {tag && <span className="ml-auto shrink-0">{tag}</span>}
          </span>
        </span>
        <span className="hidden w-[132px] shrink-0 items-center gap-2.5 sm:flex">
          {gauge.pct != null && (
            <span className="w-16 shrink-0">
              <GaugeBar pct={gauge.pct} tone={gauge.tone} label={`${name} plan left`} />
            </span>
          )}
          <span className={cx("truncate text-small tabular-nums", gauge.pct == null ? "text-faint" : TEXT[gauge.tone])}>{gauge.text}</span>
        </span>
        <span className="hidden w-[130px] shrink-0 truncate text-small tabular-nums text-muted sm:block">{reset ?? ""}</span>
        <span className="flex shrink-0 items-center gap-1.5" title={statusLabel}>
          <span aria-hidden="true" className={cx("size-2 rounded-full", DOT[tone], tone === "err" && "shadow-[0_0_0_3px_var(--err-soft)]")} />
          <span className={cx("hidden w-[84px] whitespace-nowrap text-small md:inline", TEXT[tone])}>{statusLabel}</span>
        </span>
        <IconChevron size={15} className={cx("shrink-0 text-faint transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <div id={panel} className="space-y-5 border-t border-border bg-panel-2/40 px-4 py-4">
          {children}
        </div>
      )}
    </div>
  );
}

/** A titled block inside an open row, with the one line that says what it shows. */
function Block({ title, help, children }: { title: string; help: string; children: ReactNode }) {
  return (
    <section className="min-w-0">
      <h4 className="text-meta-lg font-medium uppercase tracking-wide text-muted">{title}</h4>
      <p className="mb-2.5 mt-0.5 text-small leading-snug text-faint">{help}</p>
      {children}
    </section>
  );
}

function Chips({ items, extra = [], limit = 6 }: { items: string[]; extra?: string[]; limit?: number }) {
  const [all, setAll] = useState(false);
  const shown = all ? items : items.slice(0, limit);
  return (
    <div className="flex flex-wrap items-center gap-1">
      {shown.map((model) => (
        <span key={model} className="rounded bg-panel-3 px-1.5 py-px font-mono text-meta-lg text-muted">
          {model}
        </span>
      ))}
      {all &&
        extra.map((model) => (
          <span key={model} className="rounded border border-dashed border-border-strong px-1.5 py-px font-mono text-meta-lg text-faint">
            {model}
          </span>
        ))}
      {(items.length > limit || extra.length > 0) && (
        <button type="button" onClick={() => setAll((v) => !v)} className="cursor-pointer rounded px-1.5 py-px text-meta-lg text-muted underline decoration-dotted underline-offset-2 hover:text-text">
          {all ? "Show fewer" : items.length > limit ? `+${items.length - limit + extra.length} more` : `+${extra.length} available`}
        </button>
      )}
    </div>
  );
}

/** The usage report for an open row, refreshed once a minute while it stays open. */
function useUsage(id: string, enabled: boolean, days = 7) {
  const api = useApi();
  const [report, setReport] = useState<ProviderUsageReport | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const load = () =>
      api
        .providerUsage(id, days)
        .then((r) => live && (setReport(r), setFailed(false)))
        .catch(() => live && setFailed(true));
    void load();
    const timer = setInterval(() => !document.hidden && void load(), 60_000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [api, id, enabled, days]);
  return { report, failed };
}

function Stat({ label, value, sub, values, color }: { label: string; value: string; sub?: string; values: number[]; color: string }) {
  return (
    <div className="min-w-0 rounded-lg border border-border bg-panel px-3 py-2.5">
      <div className="text-meta-lg text-faint">{label}</div>
      <div className="mt-0.5 flex items-baseline gap-1.5">
        <span className="text-body-lg font-medium tabular-nums">{value}</span>
        {sub && <span className="text-small text-faint">{sub}</span>}
      </div>
      <Sparkline points={sparkPoints(values, 100, 28, 1)} color={color} height={26} />
    </div>
  );
}

function UsageBlock({ provider, report, failed }: { provider: ModelProvider; report: ProviderUsageReport | null; failed: boolean }) {
  const requests = dailySeries(report, "requests");
  const total = requests.reduce((a, b) => a + b, 0);
  const failures = (report?.daily ?? []).reduce((a, d) => a + d.failures, 0);
  const weighted = (report?.daily ?? []).reduce((a, d) => a + d.avg_latency_ms * d.requests, 0);
  const usage = provider.usage;
  const since = usage?.since ? new Date(usage.since).toLocaleDateString("en-GB", { day: "numeric", month: "short" }) : null;
  const lastUsed = timeAgo(usage?.last_request_at);
  const lifetime = usage && usage.requests > 0 ? [`${usage.requests.toLocaleString()} requests${since ? ` since ${since}` : ""}`, lastUsed ? `last used ${lastUsed}` : null].filter(Boolean).join(" · ") : "Never used";
  return (
    <Block title="Usage" help={`Requests, failures and time per request, day by day over the last ${report?.days ?? 7} days.`}>
      {!report ? (
        <p className="flex items-center gap-2 text-small text-faint">{failed ? "Usage history isn't available from this Mothership." : <><Spinner className="size-3" /> Reading usage…</>}</p>
      ) : total === 0 ? (
        <p className="text-small text-faint">No requests in the last {report.days} days.</p>
      ) : (
        <div className="grid grid-cols-3 gap-2">
          <Stat label="Requests" value={total.toLocaleString()} values={requests} color="var(--chart-1)" />
          <Stat
            label="Failed"
            value={`${((failures / total) * 100).toFixed(1)}%`}
            values={dailySeries(report, "failure_pct")}
            color={failures / total >= 0.1 ? "var(--err)" : "var(--faint)"}
          />
          <Stat label="Avg time" value={formatAvgLatency(weighted / total)} values={dailySeries(report, "avg_latency_ms")} color="var(--info)" />
        </div>
      )}
      <p className="mt-2 text-small text-faint">{lifetime}</p>
    </Block>
  );
}

function CreditsBlock({ provider, report, failed, onEdit, nowMs }: { provider: ModelProvider; report: ProviderUsageReport | null; failed: boolean; onEdit: () => void; nowMs: number }) {
  const gauge = gaugeOf(provider);
  const hasReader = Boolean(provider.quota);
  return (
    <Block title="Credits" help={hasReader ? "What is left in the plan over the last 7 days, and when it refills." : "How much of the plan is left and when it resets."}>
      {report?.has_balance ? (
        <BalanceChart report={report} tone={gauge.tone} resetUnix={resetUnixOf(provider)} nowMs={nowMs} />
      ) : (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-lg border border-dashed border-border-strong px-3.5 py-3">
          <p className="min-w-0 flex-1 basis-56 text-small leading-snug text-muted">
            {failed
              ? "Credits history isn't available from this Mothership."
              : hasReader
                ? "No reading yet. The balance is read when you press Check, and every half hour after."
                : "Balance unknown: add a plan balance reader to see what is left."}
          </p>
          {!hasReader && (
            <Button size="sm" onClick={onEdit}>
              <IconPencil size={13} /> Add a reader
            </Button>
          )}
        </div>
      )}
    </Block>
  );
}

function Facts({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-small">
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-faint">{k}</dt>
          <dd className="min-w-0 text-muted [overflow-wrap:anywhere]">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function ProviderListRow({
  provider,
  health,
  disabled,
  open,
  onToggle,
  onCheck,
  onEdit,
}: {
  provider: ModelProvider;
  health?: HealthView;
  disabled: boolean;
  open: boolean;
  onToggle: () => void;
  onCheck: () => void;
  onEdit: () => void;
}) {
  const nowMs = Date.now();
  const status = providerStatus(provider);
  const gauge = gaugeOf(provider);
  const { report, failed } = useUsage(provider.id, open);
  const limits = limitLabels(provider);
  const running = provider.in_flight ?? 0;
  const queued = provider.queued ?? 0;
  const usedBy = provider.used_by ?? [];
  const extra = offeredModels(provider).filter((m) => !provider.models.includes(m));
  return (
    <Shell
      id={provider.id}
      mark={<ProviderMark preset={provider.preset} name={provider.name} baseUrl={provider.base_url} />}
      name={provider.name}
      tag={
        running > 0 || queued > 0 ? (
          <Badge tone={queued > 0 ? "warn" : "info"} pulse={running > 0}>
            {[running > 0 && `${running} running`, queued > 0 && `${queued} queued`].filter(Boolean).join(" · ")}
          </Badge>
        ) : null
      }
      statusLabel={status.label}
      tone={status.tone}
      gauge={{ pct: gauge.pctLeft, text: gauge.text, tone: gauge.tone }}
      reset={resetText(provider, nowMs)}
      open={open}
      onToggle={onToggle}
    >
      <CreditsBlock provider={provider} report={report} failed={failed} onEdit={onEdit} nowMs={nowMs} />
      <UsageBlock provider={provider} report={report} failed={failed} />
      <Block title="Models" help="Enabled models can be picked as provider/model wherever you choose one.">
        <p className="mb-1.5 text-body-sm">{modelsSummary(provider)}</p>
        {provider.models.length > 0 ? <Chips items={provider.models} extra={extra} /> : <p className="text-small text-faint">None listed; type model ids where you pick a model.</p>}
      </Block>
      <Block title="Used by" help="The model settings that send work here.">
        {usedBy.length === 0 ? (
          <p className="text-small text-faint">No model setting points here, so it stays idle.</p>
        ) : (
          <div className="flex flex-wrap gap-1.5">
            {usedBy.map((setting) => (
              <Badge key={setting}>{SETTING_LABEL[setting]}</Badge>
            ))}
          </div>
        )}
      </Block>
      <Block title="Connection" help="Where the Mothership sends requests, and how.">
        <Facts
          rows={[
            ["Address", <span key="u" className="font-mono">{provider.base_url}</span>],
            ["Protocol", WIRE_LABEL[provider.wire]],
            ["Key", <KeyBadge key="k" provider={provider} />],
            ...(limits.length ? ([["Limits", limits.join(" · ")]] as [string, ReactNode][]) : []),
            ["ID", <span key="i" className="font-mono">{provider.id}</span>],
          ]}
        />
        {health && (
          <div className="mt-3">
            <HealthStatus health={health} degraded={provider.health?.degraded} />
          </div>
        )}
        <div className="mt-3 flex flex-wrap gap-1.5">
          <Button size="sm" disabled={health?.state === "checking"} onClick={onCheck} title="Check that the Mothership can reach this provider">
            {health?.state === "checking" ? <Spinner className="size-3" /> : <IconNetwork size={13} />} Check
          </Button>
          <Button size="sm" disabled={disabled} onClick={onEdit}>
            <IconPencil size={13} /> Edit
          </Button>
        </div>
      </Block>
    </Shell>
  );
}

/**
 * Claude, the first row. Not a provider anyone configured here: the default the harness falls back
 * to, so read-only. Its limit state and reset come from the account quota in /api/models/plans.
 */
export function ClaudeListRow({
  claude,
  models,
  plan,
  open,
  onToggle,
  onOpenConnections,
}: {
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  plan: PlanUsage | null;
  open: boolean;
  onToggle: () => void;
  onOpenConnections: () => void;
}) {
  const nowMs = Date.now();
  const own = models.filter((m) => m.provider === "anthropic");
  const state = claudePlanText(plan, nowMs);
  const connected = claude?.configured;
  const tone: StatusTone = connected === false ? "err" : state.tone === "idle" ? (connected ? "ok" : "idle") : state.tone;
  const label = connected === false ? "Not connected" : state.status === "Limit reached" ? state.status : "Connected";
  const { report } = useUsage("anthropic", open);
  const last = plan?.last_limit;
  return (
    <Shell
      id="anthropic"
      mark={<ProviderMark preset="anthropic" name="Anthropic" />}
      name="Anthropic"
      tag={<Badge>Built-in</Badge>}
      statusLabel={label}
      tone={tone}
      gauge={{ pct: state.tone === "err" ? 0 : null, text: state.tone === "err" ? "0%" : "subscription", tone: state.tone === "err" ? "err" : "idle" }}
      reset={state.reset}
      open={open}
      onToggle={onToggle}
    >
      <Block title="Usage window" help="Claude reports its session and weekly limits only once one is hit.">
        <p className="text-body-sm">
          {plan?.exhausted ? <span className="text-err">Limit reached{state.reset ? `, ${state.reset}` : ""}.</span> : "Within its limits right now."}
        </p>
        <p className="mt-1 text-small text-faint">
          {last ? `Last limit hit ${timeAgo(last.at) ?? "recently"}.` : "No limit hit since this Mothership started keeping track."}
          {report && report.events.length > 0 ? ` ${report.events.filter((e) => e.kind === "exhausted").length} in the last ${report.days} days.` : ""}
        </p>
      </Block>
      <Block title="Models" help="The default. A model id with no provider prefix, and a provider's fallback, go here.">
        {own.length === 0 ? <p className="text-small text-faint">Model list not loaded.</p> : <Chips items={own.map((m) => m.id)} limit={8} />}
      </Block>
      <Block title="Connection" help="The subscription is signed in under Connections, not here.">
        <Facts rows={[["Account", claude?.configured ? [claude.account, claude.source ?? "Connected"].filter(Boolean).join(" · ") : "Not connected"]]} />
        <div className="mt-3">
          <Button size="sm" onClick={onOpenConnections}>
            Connections <IconChevron size={13} />
          </Button>
        </div>
      </Block>
    </Shell>
  );
}

