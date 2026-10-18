// The Model providers page's pure half (issue #1204): what a collapsed row says (status, gauge,
// reset), what the credits chart projects, and how the models are summarised. Kept free of React so
// vitest runs it as is. Nothing here guesses: a balance the Mothership never read is "unknown", a
// reset it was never told is absent, and the run-out is a straight line through real readings.
import { untilWords } from "../../resetTime";
import type { ModelProvider, PlanUsage, ProviderBalance, ProviderUsageReport } from "../../types";
import { presetDraft } from "./providerCatalog";

export type StatusTone = "ok" | "warn" | "err" | "idle";

export interface ProviderStatus {
  tone: StatusTone;
  /** One or two words: "Healthy", "Degraded", "Failing", "Quota out", "Unused". */
  label: string;
}

/** The row's status dot: quota out first, then the gateway's own health verdict, then whether it was ever used. */
export function providerStatus(provider: ModelProvider): ProviderStatus {
  if (provider.quota_exhausted) return { tone: "err", label: "Quota out" };
  const health = provider.health;
  if (health?.degraded) return health.failure_pct >= 25 ? { tone: "err", label: "Failing" } : { tone: "warn", label: "Degraded" };
  if (!provider.has_key && provider.auth !== "none") return { tone: "warn", label: "No key" };
  if ((provider.usage?.requests ?? 0) === 0) return { tone: "idle", label: "Unused" };
  return { tone: "ok", label: "Healthy" };
}

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
export const compactNumber = (n: number): string => compact.format(n);

export interface Gauge {
  /** 0..100 left in the plan; null when the balance or its total is unknown. */
  pctLeft: number | null;
  /** "62%", "3.1M left", or "balance unknown". */
  text: string;
  tone: StatusTone;
}

/** The collapsed row's credits gauge. An exhausted plan reads as empty whatever the last reading said. */
export function gaugeOf(provider: ModelProvider): Gauge {
  if (provider.quota_exhausted) return { pctLeft: 0, text: "0%", tone: "err" };
  const b = provider.balance;
  if (!b) return { pctLeft: null, text: "balance unknown", tone: "idle" };
  if (b.limit != null && b.limit > 0) {
    const pct = Math.max(0, Math.min(100, (b.remaining / b.limit) * 100));
    return { pctLeft: pct, text: `${Math.round(pct)}%`, tone: pct < 10 ? "err" : pct < 25 ? "warn" : "ok" };
  }
  return { pctLeft: null, text: `${compactNumber(b.remaining)} left`, tone: b.remaining <= 0 ? "err" : "ok" };
}

/** When the plan refills as unix seconds: the exhausted record's reset first, else the balance reader's. */
export function resetUnixOf(provider: ModelProvider): number | null {
  return provider.quota_exhausted?.reset_unix ?? provider.balance?.reset_unix ?? null;
}

/** "resets in 3 h 12 min", the provider's own words when it named none as a timestamp, or null when no reset is known. */
export function resetText(provider: ModelProvider, nowMs: number): string | null {
  const unix = resetUnixOf(provider);
  if (unix != null) return unix * 1000 > nowMs ? `resets in ${untilWords(unix, nowMs)}` : "resetting now";
  const words = provider.quota_exhausted?.reset_at;
  return words ? `resets ${words}` : null;
}

/**
 * What the endpoint offers (issue #1167): the models discovered from its /v1/models once one came
 * back non-empty, otherwise the preset's or catalogue entry's known models — an endpoint that serves
 * no list (Alibaba's /apps/anthropic) still offers what the catalogue names.
 */
export function offeredModels(provider: Pick<ModelProvider, "discovered_models" | "preset">): string[] {
  return provider.discovered_models?.length ? provider.discovered_models : presetDraft(provider.preset).models;
}

/** "3 enabled · +5 available": the enabled models, and how many more the endpoint offers (issue #1167). */
export function modelsSummary(provider: Pick<ModelProvider, "models" | "discovered_models" | "preset">): string {
  const enabled = provider.models.length;
  const extra = offeredModels(provider).filter((m) => !provider.models.includes(m)).length;
  const head = enabled === 0 ? "No models enabled" : `${enabled} enabled`;
  return extra > 0 ? `${head} · +${extra} available` : head;
}

/** One row of the edit form's model checklist (issue #1167). */
export interface ModelChoice {
  model: string;
  /** Ticked: the model is enabled, so it can be picked as provider/model. */
  enabled: boolean;
  /** The endpoint listed it for the first time at the last discovery. */
  isNew: boolean;
  /** Enabled, but the endpoint's own list no longer names it. Kept listed and ticked; nothing drops it behind a setting's back. */
  gone: boolean;
}

/**
 * The edit form's checklist (issue #1167): every model the endpoint offers, plus any enabled model
 * the offer left out — unchecking one is the only way it leaves `models`. `models` is the form's
 * live list, not the saved one, so a tick survives a re-render and a chip removed above unticks here.
 */
export function modelChoices(
  provider: Pick<ModelProvider, "discovered_models" | "new_models" | "preset">,
  models: string[],
): ModelChoice[] {
  const discovered = provider.discovered_models ?? [];
  const fresh = provider.new_models ?? [];
  return [...new Set([...offeredModels(provider), ...models])].map((model) => ({
    model,
    enabled: models.includes(model),
    isNew: fresh.includes(model),
    gone: discovered.length > 0 && models.includes(model) && !discovered.includes(model),
  }));
}

/** The enabled list after one checklist checkbox flips: ticking adds, unticking removes. */
export function toggleModel(models: string[], model: string, on: boolean): string[] {
  return on ? (models.includes(model) ? models : [...models, model]) : models.filter((m) => m !== model);
}

export interface Projection {
  /** The projected moment the balance reaches zero, in ms; null when it is not falling or the data is too thin. */
  runOutMs: number | null;
  /** Remaining units per day at the fitted rate (negative while falling). */
  perDay: number | null;
}

/**
 * A least-squares line through the readings since the last reset or refill (a reading higher than
 * its predecessor starts a new run), extended to zero. Needs three readings over at least an hour,
 * and a falling line; otherwise there is nothing honest to project.
 */
export function projectRunOut(samples: ProviderBalance[], nowMs: number): Projection {
  const pts = samples.map((s) => ({ t: Date.parse(s.at), v: s.remaining })).filter((p) => Number.isFinite(p.t));
  let start = 0;
  for (let i = 1; i < pts.length; i++) if (pts[i].v > pts[i - 1].v) start = i;
  const run = pts.slice(start);
  if (run.length < 3 || run[run.length - 1].t - run[0].t < 3_600_000) return { runOutMs: null, perDay: null };
  const n = run.length;
  const mt = run.reduce((a, p) => a + p.t, 0) / n;
  const mv = run.reduce((a, p) => a + p.v, 0) / n;
  const num = run.reduce((a, p) => a + (p.t - mt) * (p.v - mv), 0);
  const den = run.reduce((a, p) => a + (p.t - mt) ** 2, 0);
  if (den === 0) return { runOutMs: null, perDay: null };
  const slope = num / den; // units per ms
  if (slope >= 0) return { runOutMs: null, perDay: slope * 86_400_000 };
  const last = run[run.length - 1];
  const at = last.t + last.v / -slope;
  return { runOutMs: at > nowMs ? at : null, perDay: slope * 86_400_000 };
}

/** A per-day series for a sparkline: requests, failure share (percent) or average latency (ms). */
export function dailySeries(report: ProviderUsageReport | null, pick: "requests" | "failure_pct" | "avg_latency_ms"): number[] {
  return (report?.daily ?? []).map((d) =>
    pick === "failure_pct" ? (d.requests === 0 ? 0 : (d.failures / d.requests) * 100) : d[pick],
  );
}

/** What the Claude subscription row says, from its plan in GET /api/models/plans. */
export function claudePlanText(plan: PlanUsage | null | undefined, nowMs: number): { tone: StatusTone; status: string; reset: string | null } {
  if (!plan) return { tone: "idle", status: "Subscription", reset: null };
  if (plan.exhausted) {
    const reset = plan.reset_unix != null && plan.reset_unix * 1000 > nowMs ? `resets in ${untilWords(plan.reset_unix, nowMs)}` : plan.reset_at ? `resets ${plan.reset_at}` : null;
    return { tone: "err", status: "Limit reached", reset };
  }
  return { tone: "ok", status: "Within limits", reset: null };
}
