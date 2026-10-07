import { useCallback, useEffect, useId, useState } from "react";
import { errorMessage, useApi, useToast } from "../../context";
import { PROVIDER_CATALOG, type CatalogEntry } from "../../providerCatalog";
import { avgLatencyText, failureRateText, formatAvgLatency, formatFailureRate, formatSince, lastFailureText, quotaExhaustedText, quotaTone, usageHealthTone } from "../../providerHealth";
import { ProviderQuotaCard, QuotaChangeSummary, runQuotaAction } from "../../cockpit/ProviderQuotaCard";
import type { HarnessStatus, ModelOption, ModelProvider, ModelSetting, ProviderHealth, QuotaActionReply, QuotaCard } from "../../types";
import { Badge, Button, Spinner, cx, formatDuration, inputClass, timeAgo } from "../ui";
import { IconChevron, IconNetwork, IconPencil, IconPlus } from "../icons";
import { ProviderMark } from "../providerMark";
import { ADD_PRESETS, WIRE_LABEL, limitLabels, presetLabel, type Editing } from "./providerCatalog";
import { KeyBadge, ProviderForm } from "./ProviderForm";
import { Pane, Code } from "./ui";

export function ProvidersPane({
  providers,
  error,
  setProviders,
  reload,
  claude,
  models,
  onOpenConnections,
  focusId,
  back,
}: {
  providers: ModelProvider[] | null;
  error: string | null;
  setProviders: (update: (list: ModelProvider[] | null) => ModelProvider[] | null) => void;
  reload: () => Promise<void>;
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  onOpenConnections: () => void;
  /** Opens this provider's editor when the pane appears — where a model picker's "Set key" lands. */
  focusId?: string;
  back?: () => void;
}) {
  const api = useApi();
  const addLabelId = useId();
  const [editing, setEditing] = useState<Editing>(() => (focusId ? { mode: "edit", id: focusId } : null));
  useEffect(() => {
    if (focusId) setEditing({ mode: "edit", id: focusId });
  }, [focusId]);
  const [browsing, setBrowsing] = useState(false);
  const [catalogQuery, setCatalogQuery] = useState("");
  const [health, setHealth] = useState<Record<string, HealthView>>({});

  // In-flight and queued counts change as colonies work; refresh them quietly.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.hidden) return;
      void reload();
    }, 5000);
    return () => clearInterval(timer);
  }, [reload]);

  const check = async (id: string) => {
    setHealth((h) => ({ ...h, [id]: { state: "checking" } }));
    try {
      const result = await api.providerHealth(id);
      setHealth((h) => ({ ...h, [id]: { state: "done", result } }));
    } catch (e) {
      setHealth((h) => ({ ...h, [id]: { state: "failed", message: errorMessage(e) } }));
    }
  };

  const upsert = (saved: ModelProvider) =>
    setProviders((list) => {
      const rest = (list ?? []).filter((p) => p.id !== saved.id);
      const index = (list ?? []).findIndex((p) => p.id === saved.id);
      if (index < 0) return [...rest, saved];
      rest.splice(index, 0, saved);
      return rest;
    });

  const addDisabled = !providers || editing !== null;

  return (
    <Pane
      title="Model providers"
      subtitle="Claude, and other Anthropic-compatible endpoints"
      back={back}
      info={
        <>
          <p>
            Claude is the default: a model picked by its plain id, such as <Code>sonnet</Code>, goes to Anthropic. Pick another provider's
            model as <Code>provider/model</Code>, for example <Code>deepseek/deepseek-flash</Code>, wherever you choose an orchestrator,
            subagent or background model.
          </p>
          <p className="text-muted">
            Requests go through the Mothership gateway: colonies never see provider keys, and servers on a private network (LAN, tailnet)
            work without extra setup.
          </p>
        </>
      }
    >
      <div className="space-y-3">
        <ProviderQuotaCards reloadProviders={reload} />
        <div className="space-y-2">
          <ClaudeRow claude={claude} models={models} onOpenConnections={onOpenConnections} />
          {error && <p className="text-body-sm text-err">{error}</p>}
          {!providers && !error && (
            <p className="flex items-center gap-2 py-1 text-body-sm text-muted">
              <Spinner /> Loading providers…
            </p>
          )}
        </div>
        {providers && (
          <div className="space-y-2">
            {providers.length === 0 && editing?.mode !== "new" && (
              <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-4 text-center text-body-sm text-muted">
                No extra providers yet. Claude works without one.
              </p>
            )}
            {providers.map((provider) =>
              editing?.mode === "edit" && editing.id === provider.id ? (
                <ProviderForm
                  key={provider.id}
                  initial={provider}
                  peers={providers}
                  preset={provider.preset}
                  takenIds={[]}
                  onCancel={() => setEditing(null)}
                  onSaved={(saved) => {
                    upsert(saved);
                    setHealth((h) => {
                      const next = { ...h };
                      delete next[saved.id];
                      return next;
                    });
                    setEditing(null);
                  }}
                  onDeleted={(id) => {
                    setProviders((list) => list?.filter((p) => p.id !== id) ?? null);
                    setEditing(null);
                  }}
                />
              ) : (
                <ProviderRow
                  key={provider.id}
                  provider={provider}
                  health={health[provider.id]}
                  disabled={editing !== null}
                  onCheck={() => void check(provider.id)}
                  onEdit={() => setEditing({ mode: "edit", id: provider.id })}
                />
              ),
            )}
            {editing?.mode === "new" && (
              <ProviderForm
                key={`new-${editing.preset}`}
                preset={editing.preset}
                peers={providers}
                takenIds={providers.map((p) => p.id)}
                onCancel={() => setEditing(null)}
                onSaved={(saved) => {
                  upsert(saved);
                  setEditing(null);
                }}
              />
            )}
          </div>
        )}
        <div role="group" aria-labelledby={addLabelId} className="space-y-2">
          <span id={addLabelId} className="block text-small-lg text-muted">
            Add a provider
          </span>
          <div className="grid grid-cols-3 gap-2 sm:grid-cols-6">
            {ADD_PRESETS.map(({ preset, label }) => {
              return (
                <button
                  key={preset}
                  type="button"
                  disabled={addDisabled}
                  onClick={() => setEditing({ mode: "new", preset })}
                  className={cx(
                    "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-border bg-panel px-1.5 py-3",
                    "text-small font-medium text-text transition-colors hover:bg-panel-2",
                    "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
                  )}
                >
                  <ProviderMark preset={preset} name={presetLabel(preset)} size="tile" />
                  <span className="w-full truncate text-center">{label}</span>
                </button>
              );
            })}
            <button
              type="button"
              disabled={addDisabled}
              aria-expanded={browsing}
              onClick={() => setBrowsing((open) => !open)}
              className={cx(
                "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-dashed border-border-strong bg-panel px-1.5 py-3",
                "text-small font-medium text-muted transition-colors hover:bg-panel-2 hover:text-text",
                "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
              )}
            >
              <span aria-hidden="true" className="grid size-11 shrink-0 place-items-center rounded-xl bg-panel-2">
                <IconPlus size={22} />
              </span>
              <span className="w-full truncate text-center">{browsing ? "Close" : "More"}</span>
            </button>
          </div>
          {browsing && <CatalogBrowser query={catalogQuery} onQuery={setCatalogQuery} disabled={addDisabled} onPick={(id) => {
            setBrowsing(false);
            setCatalogQuery("");
            setEditing({ mode: "new", preset: id });
          }} />}
        </div>
        <p className="text-meta-lg leading-snug text-faint">
          Logos and names are the property of their owners. Colonizer is not affiliated with, endorsed by or connected to any of them.
        </p>
      </div>
    </Pane>
  );
}

/**
 * The "Provider out of quota" cards (issue #767) at the top of the providers pane: the same cards
 * the inbox shows, answered here the same way. Refreshed on the pane's own 5 s rhythm.
 */
function ProviderQuotaCards({ reloadProviders }: { reloadProviders: () => Promise<void> }) {
  const api = useApi();
  const toast = useToast();
  const [cards, setCards] = useState<QuotaCard[]>([]);
  // The last switch's "was X → now Y" summary, kept after its card goes (issue #767).
  const [switched, setSwitched] = useState<QuotaActionReply | null>(null);
  const load = useCallback(async () => {
    try {
      setCards((await api.attention()).quota_cards ?? []);
    } catch {
      // An older mothership has no /api/attention: no cards, nothing to say.
    }
  }, [api]);
  useEffect(() => {
    void load();
    const timer = setInterval(() => {
      if (!document.hidden) void load();
    }, 5000);
    return () => clearInterval(timer);
  }, [load]);
  if (cards.length === 0 && !switched) return null;
  return (
    <div className="space-y-2">
      <QuotaChangeSummary reply={switched} onDismiss={() => setSwitched(null)} />
      {cards.map((card) => (
        <ProviderQuotaCard
          key={card.provider}
          card={card}
          onAction={async (provider, body) => {
            const reply = await runQuotaAction(api.quotaAction, (message, tone) => toast(message, tone), provider, body);
            if ((reply?.changes?.length ?? 0) > 0) setSwitched(reply);
            await Promise.all([load(), reloadProviders()]);
          }}
        />
      ))}
    </div>
  );
}

/**
 * Claude, at the top of the list. Not a provider anyone configured here: it is the
 * default the harness falls back to, so it is read-only, has no key field and no
 * endpoint to probe. Its state is the Connections card's, its models the ones
 * `/api/models` lists under `anthropic`.
 */
function ClaudeRow({ claude, models, onOpenConnections }: { claude: HarnessStatus["claude"] | null; models: ModelOption[]; onOpenConnections: () => void }) {
  const own = models.filter((m) => m.provider === "anthropic");
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <ProviderMark preset="anthropic" name="Anthropic" />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-body-lg font-semibold">Anthropic</span>
          <Badge>Built-in</Badge>
          {claude && <Badge tone={claude.configured ? "ok" : "err"}>{claude.configured ? "Connected" : "Not connected"}</Badge>}
        </div>
        <div className="mt-0.5 text-small text-muted">
          {claude?.configured ? [claude.account, claude.source ?? "Connected", "managed in Connections"].filter(Boolean).join(" · ") : "Managed in Connections"}
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {own.length === 0 ? (
            <span className="text-small text-faint">Model list not loaded.</span>
          ) : (
            own.map((model) => (
              <span key={model.id} title={model.label} className="rounded bg-panel-2 px-1.5 py-px font-mono text-meta-lg text-muted">
                {model.id}
              </span>
            ))
          )}
        </div>
        <p className="mt-1.5 text-small text-faint">The default. A model id with no provider prefix, and a provider's fallback, go here.</p>
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" onClick={onOpenConnections}>
          Connections <IconChevron size={13} />
        </Button>
      </div>
    </div>
  );
}

type HealthView = { state: "checking" } | { state: "done"; result: ProviderHealth } | { state: "failed"; message: string };

export function HealthStatus({ health, degraded }: { health: HealthView; degraded?: boolean }) {
  if (health.state === "checking") {
    return (
      <span role="status" className="flex items-center gap-1.5 text-small text-muted">
        <Spinner className="size-3" /> Checking from the Mothership…
      </span>
    );
  }
  let tone: "ok" | "warn" | "err";
  let text: string;
  let title: string | undefined;
  if (health.state === "failed") {
    tone = "err";
    text = `Check failed: ${health.message}`;
  } else {
    const r = health.result;
    const latency = r.latency_ms != null ? `${Math.round(r.latency_ms)} ms` : null;
    const models = r.models.length ? `${r.models.length} model${r.models.length === 1 ? "" : "s"}` : null;
    // The plan balance rides along on the probe when the provider has a quota URL configured; its
    // failure is its own clause and never changes the reachability verdict (issue #199).
    const quota = r.quota ? r.quota.error ?? (r.quota.remaining != null ? `${r.quota.remaining.toLocaleString("en-US")} left in plan` : null) : null;
    // A note marks a non-2xx the Mothership judged healthy (an anthropic-wire endpoint with no
    // /v1/models), so it skips the HTTP warning.
    if (!r.reachable) {
      tone = "err";
      text = `Unreachable${r.error ? `: ${r.error}` : ""}`;
    } else if (!r.note && r.status != null && (r.status < 200 || r.status > 299)) {
      tone = "warn";
      text = [`HTTP ${r.status}`, latency, r.error, quota].filter(Boolean).join(" · ");
    } else {
      tone = "ok";
      // A passing probe is one request; say so next to a provider failing a share of its real traffic.
      text = ["Reachable", latency, models ?? r.note, quota, degraded ? "but failing real traffic" : null].filter(Boolean).join(" · ");
    }
    title = [
      r.models.length ? `Models: ${r.models.join(", ")}` : r.note ? "The endpoint does not list its models; requests route normally." : null,
      r.checked_at ? `Checked ${new Date(r.checked_at).toLocaleTimeString()}` : null,
    ]
      .filter(Boolean)
      .join("\n");
  }
  return (
    <span
      role="status"
      title={title || undefined}
      className={cx("flex items-start gap-1.5 text-small font-medium", tone === "ok" ? "text-ok" : tone === "warn" ? "text-warn" : "text-err")}
    >
      <span className="mt-[5px] size-1.5 shrink-0 rounded-full bg-current" />
      <span className="min-w-0 [overflow-wrap:anywhere]">{text}</span>
    </span>
  );
}

/** Model settings by the names the model form labels them, for the usage line. */
const MODEL_SETTING_LABEL: Record<ModelSetting, string> = {
  model: "Orchestrator model",
  subagent_model: "Subagent model",
  background_model: "Background model",
  model_low: "Model for small tasks",
  model_high: "Model for large tasks",
};

/**
 * Why a provider wired only to subagent or background work can look idle, as one clause: the
 * orchestrator does nearly all of a colony's work, subagent traffic only appears when a colony
 * delegates (rare) and background calls are small auxiliary jobs. Null when the provider is on
 * the main path, or when no setting points at it — that case gets its own, sharper wording.
 */
function idleWiringNote(usedBy: ModelSetting[]): string | null {
  if (usedBy.length === 0 || usedBy.includes("model")) return null;
  const names = usedBy.map((setting) => MODEL_SETTING_LABEL[setting]);
  const settings =
    names.length === 1
      ? `the ${names[0]} setting`
      : `the ${names.slice(0, -1).join(", ")} and ${names[names.length - 1]} settings`;
  return `only wired to ${settings} — the Orchestrator model does nearly all of a colony's work, so it can look idle`;
}

/** The failure-rate segment's tooltip: the Mothership owns the rule, the line only reports it. */
const FAILURE_RATE_TITLE =
  "Failures as a share of the requests counted at the gateway. The Mothership calls a provider degraded at 10% or more of at least 50 requests; below that the rate is shown without a verdict.";
const AVG_LATENCY_TITLE = "Mean time of the requests dispatched to this provider, time spent queued excluded.";

/**
 * The cumulative usage line on a provider card. Its most important job is telling "never used"
 * from "used and working" at a glance: `Reachable` alone once read as "in use" when it only meant
 * the endpoint answered. An absent `usage` means the Mothership didn't report one — not the same
 * thing as never used. Wiring notes explain, they don't advise — an idle provider is not broken.
 */
function UsageLine({ provider }: { provider: ModelProvider }) {
  const usage = provider.usage;
  const requests = usage?.requests ?? 0;
  const failures = usage?.failures ?? 0;
  const fallbacks = usage?.fallbacks ?? 0;
  const lastUsed = timeAgo(usage?.last_request_at);
  const total = formatDuration(usage?.duration_ms ?? 0);
  const usedBy = provider.used_by;
  const wiring = idleWiringNote(usedBy ?? []);
  const health = provider.health;
  const tone = usageHealthTone(health);
  const rate = failureRateText(health, requests);
  const avg = avgLatencyText(health, requests);
  const lastFailure = lastFailureText(health?.last_failure);
  const since = formatSince(usage?.since);

  const segments: { text: string; title?: string; tone?: "warn" | "err" | "lift" }[] = [];
  if (!usage) {
    segments.push({ text: "No usage recorded — this Mothership doesn't report usage", tone: "lift" });
  } else if (requests === 0) {
    segments.push({ text: "Never used", tone: "lift" });
  } else {
    if (rate)
      segments.push({
        text: rate,
        title: FAILURE_RATE_TITLE,
        tone: tone === "err" ? "err" : undefined,
      });
    if (avg) segments.push({ text: avg, title: AVG_LATENCY_TITLE });
    if (lastFailure)
      segments.push({
        text: lastFailure,
        title: "The typed failure code of this provider's most recent failed request, as the gateway recorded it in its audit log.",
        tone: "warn",
      });
    segments.push({ text: `${requests.toLocaleString()} ${requests === 1 ? "request" : "requests"}${since ? ` since ${since}` : ""}` });
    if (lastUsed) segments.push({ text: `last used ${lastUsed}` });
    if (failures > 0)
      segments.push({
        text: `${failures.toLocaleString()} failed${fallbacks > 0 ? `, ${fallbacks.toLocaleString()} of them got a Claude fallback` : ""}`,
        title:
          "Failed means no usable response came back — a queue timeout, an unreachable provider, a request timeout, an upstream status of 400 or more, or an OpenAI-wire body that failed or never finished. A failure part-way through a streamed body is not counted. The fallbacks are a prediction, not an observation: the Mothership answered those with a fallback response, which the colony's router retries on Claude.",
        tone: "warn",
      });
    if (total !== "0s") segments.push({ text: `${total} total` });
  }
  if (usedBy && usedBy.length === 0) {
    segments.push({ text: "no model setting points at it as configured", tone: requests === 0 ? "lift" : undefined });
  } else if (wiring) {
    segments.push({ text: wiring });
  }
  if (provider.quota_exhausted) {
    segments.push({ text: quotaExhaustedText(provider.quota_exhausted) ?? "quota exhausted", tone: quotaTone(provider.quota_exhausted) ?? undefined });
  }

  return (
    <div className="mt-1 text-small text-faint" title="Counted at the Mothership gateway since it first kept tally; the counts survive a restart.">
      {segments.map((segment, index) => (
        <span key={index}>
          {index > 0 && " · "}
          <span
            title={segment.title}
            className={cx(
              segment.tone === "warn" && "text-warn",
              segment.tone === "err" && "font-medium text-err",
              segment.tone === "lift" && "font-medium text-muted",
            )}
          >
            {segment.text}
          </span>
        </span>
      ))}
    </div>
  );
}

function ProviderRow({
  provider,
  health,
  disabled,
  onCheck,
  onEdit,
}: {
  provider: ModelProvider;
  health?: HealthView;
  disabled: boolean;
  onCheck: () => void;
  onEdit: () => void;
}) {
  const running = provider.in_flight ?? 0;
  const queued = provider.queued ?? 0;
  const limits = limitLabels(provider);
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <ProviderMark preset={provider.preset} name={provider.name} baseUrl={provider.base_url} />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-body-lg font-semibold">{provider.name}</span>
          <span className="font-mono text-small text-faint">{provider.id}</span>
          <KeyBadge provider={provider} />
          {provider.wire === "openai" && (
            <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
              {WIRE_LABEL.openai}
            </Badge>
          )}
          {(running > 0 || queued > 0) && (
            <Badge tone={queued > 0 ? "warn" : "info"} pulse={running > 0}>
              {[running > 0 && `${running} running`, queued > 0 && `${queued} queued`].filter(Boolean).join(" · ")}
            </Badge>
          )}
          {provider.health?.degraded && (
            <Badge
              tone="err"
              title={`${formatFailureRate(provider.health.failure_pct)} of requests failed, ${formatAvgLatency(provider.health.avg_latency_ms)} on average. The Mothership rates a provider degraded past 10% failed.`}
            >
              Degraded
            </Badge>
          )}
        </div>
        <div className="mt-0.5 font-mono text-small text-muted [overflow-wrap:anywhere]">{provider.base_url}</div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {provider.models.length === 0 ? (
            <span className="text-small text-faint">No models listed; type model IDs where you pick a model.</span>
          ) : (
            provider.models.map((model) => (
              <span key={model} className="rounded bg-panel-2 px-1.5 py-px font-mono text-meta-lg text-muted">
                {model}
              </span>
            ))
          )}
        </div>
        {limits.length > 0 && <div className="mt-1 text-small text-faint">{limits.join(" · ")}</div>}
        <UsageLine provider={provider} />
        {health && (
          <div className="mt-2">
            <HealthStatus health={health} degraded={provider.health?.degraded} />
          </div>
        )}
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" disabled={health?.state === "checking"} onClick={onCheck} title="Check that the Mothership can reach this provider">
          {health?.state === "checking" ? <Spinner className="size-3" /> : <IconNetwork size={13} />} Check
        </Button>
        <Button size="sm" disabled={disabled} onClick={onEdit}>
          <IconPencil size={13} /> Edit
        </Button>
      </div>
    </div>
  );
}

/** The host part of a base URL, which is what tells two endpoints apart in a list. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * The long tail of Anthropic-compatible endpoints, searchable. Kept behind "More" because six
 * vendors cover almost everyone and seventy would bury them.
 */
function CatalogBrowser({
  query,
  onQuery,
  disabled,
  onPick,
}: {
  query: string;
  onQuery: (value: string) => void;
  disabled: boolean;
  onPick: (id: string) => void;
}) {
  const needle = query.trim().toLowerCase();
  const matches = PROVIDER_CATALOG.filter(
    (entry: CatalogEntry) =>
      !needle || entry.name.toLowerCase().includes(needle) || entry.base_url.toLowerCase().includes(needle),
  );
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel p-2.5">
      <input
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        placeholder="Search providers"
        aria-label="Search providers"
        autoFocus
        className={inputClass}
      />
      <ul className="scroll-thin max-h-64 space-y-0.5 overflow-y-auto">
        {matches.map((entry) => (
          <li key={entry.id}>
            <button
              type="button"
              disabled={disabled}
              onClick={() => onPick(entry.id)}
              className="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-45"
            >
              <ProviderMark preset={entry.id} name={entry.name} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-body-sm font-medium">{entry.name}</span>
                <span className="block truncate font-mono text-meta-lg text-faint">{hostOf(entry.base_url)}</span>
              </span>
              {entry.wire === "openai" && <Badge tone="info">{WIRE_LABEL.openai}</Badge>}
            </button>
          </li>
        ))}
        {matches.length === 0 && <li className="px-2 py-3 text-body-sm text-faint">Nothing matches that.</li>}
      </ul>
      <p className="px-1 text-meta-lg leading-snug text-faint">
        {PROVIDER_CATALOG.length} endpoints, from the cc-switch catalogue. Colonizer neither vets nor endorses them, and many resell
        access rather than run the model themselves.
      </p>
    </div>
  );
}
