// The providers screen's pure half after it learned to surface a provider's failure rate (issue
// #184): the Mothership computes the numbers and decides `degraded`, so these helpers only format
// what it says and pick a tone from it. Kept free of React and the DOM so vitest can run them as is.
import type { ProviderQuotaState, ProviderUsageHealth } from "./types";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** A failure rate as shown: always one decimal, so a clean 0 reads as measured, not as blank. */
export function formatFailureRate(pct: number): string {
  return `${pct.toFixed(1)}%`;
}

/**
 * An average latency as shown. Under a second it reads like the reachability probe's ms; a whole
 * number of seconds drops the decimal; longer averages follow `formatDuration`'s convention. Unlike
 * `formatDuration` it keeps one decimal, because a 12.8 s mean is the fact this screen exists to show.
 */
export function formatAvgLatency(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) {
    const text = (ms / 1000).toFixed(1);
    return `${text.endsWith(".0") ? text.slice(0, -2) : text}s`;
  }
  const s = Math.round(ms / 1000);
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

/**
 * When a tally started, as it reads after "since": `12 Mar`, with the year once it is not the
 * current one. UTC days, so a given timestamp reads the same on every machine. Null for null or
 * unparseable input.
 */
export function formatSince(since: string | null | undefined, now: number = Date.now()): string | null {
  if (!since) return null;
  const date = new Date(since);
  if (Number.isNaN(date.getTime())) return null;
  const day = `${date.getUTCDate()} ${MONTHS[date.getUTCMonth()]}`;
  return date.getUTCFullYear() === new Date(now).getUTCFullYear() ? day : `${day} ${date.getUTCFullYear()}`;
}

/**
 * The failure-rate segment's text, or null when there is nothing to show: no health reported, or no
 * requests to rate. Unrated it still shows the numbers but says they are too few to judge, so two
 * requests at 50% do not read like a provider failing a third of its traffic.
 */
export function failureRateText(health: ProviderUsageHealth | null | undefined, requests: number): string | null {
  if (!health || requests <= 0) return null;
  const rate = `${formatFailureRate(health.failure_pct)} failed`;
  return health.rated ? rate : `${rate}, too few requests to judge`;
}

/** The average-latency segment's text, under the same guard as `failureRateText`. */
export function avgLatencyText(health: ProviderUsageHealth | null | undefined, requests: number): string | null {
  if (!health || requests <= 0) return null;
  return `${formatAvgLatency(health.avg_latency_ms)} avg`;
}

/**
 * The provider's most recent typed failure as shown: `last failure: unreachable`, the gateway's own
 * failure code (issue #302) reported as given. Null when the Mothership names none.
 */
export function lastFailureText(code: string | null | undefined): string | null {
  if (!code) return null;
  return `last failure: ${code}`;
}

/**
 * The Mothership's verdict as a tone: `err` for degraded, `ok` for rated and fine, and none when it
 * does not rate the provider at all — no data, no verdict.
 */
export function usageHealthTone(health: ProviderUsageHealth | null | undefined): "err" | "ok" | null {
  if (!health?.rated) return null;
  return health.degraded ? "err" : "ok";
}

/**
 * Quota exhaustion as a tone: `err`, so an exhausted provider reads as degraded everywhere the
 * health tone does. Null when the plan is not exhausted.
 */
export function quotaTone(quota: ProviderQuotaState | null | undefined): "err" | null {
  return quota ? "err" : null;
}

/**
 * Quota exhaustion as shown: `quota exhausted, resets 09-23 07:54 UTC`, or `quota exhausted` when
 * the provider named no reset. Null when the plan is not exhausted.
 */
export function quotaExhaustedText(quota: ProviderQuotaState | null | undefined): string | null {
  if (!quota) return null;
  return quota.reset_at ? `quota exhausted, resets ${quota.reset_at}` : "quota exhausted";
}
