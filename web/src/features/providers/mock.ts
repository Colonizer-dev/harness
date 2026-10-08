// The `providers` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, now, sleep } from "../../mockShared";
import type { DownloadableSkillset, ModelOption, ModelProvider, ProviderUsageReport } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { ProvidersApi } from "./api";

export const GRAFT_BYTES = 84_213_760;
/** The skillset the mock offers second: a much smaller tarball. */
export const UNDERSTAND_ANYTHING_BYTES = 3_145_728;

/**
 * Every mock download, by skillset name. Each row advances on its own, exactly as the real
 * endpoints do — one status per skillset, not one for "the" skillset.
 */
const mockDownloads: Record<string, DownloadableSkillset> = {
  graft: { name: "graft", release: "0.19.0-1", installed_release: null, state: "idle", bytes: 0, total: null, started_at: null, finished_at: null, error: null },
  "understand-anything": { name: "understand-anything", release: "v2.9.0", installed_release: null, state: "idle", bytes: 0, total: null, started_at: null, finished_at: null, error: null },
};

const downloadBytes = (name: string) => (name === "graft" ? GRAFT_BYTES : UNDERSTAND_ANYTHING_BYTES);

/** Advances one mock download: about four seconds from start to installed. */
export function tickGraft(name = "graft"): DownloadableSkillset {
  const status = mockDownloads[name] ?? mockDownloads.graft;
  if (status.state !== "downloading" || !status.started_at) return status;
  const total = downloadBytes(name);
  const bytes = Math.min(total, Math.round(((Date.now() - Date.parse(status.started_at)) / 4000) * total));
  mockDownloads[name] =
    bytes >= total
      ? { ...status, state: "installed", installed_release: status.release, bytes, total, finished_at: new Date().toISOString() }
      : { ...status, bytes, total };
  return mockDownloads[name];
}

/** A small seeded generator, so the demo's history is the same on every load. */
function seeded(seed: string): () => number {
  let h = 2166136261;
  for (const c of seed) h = Math.imul(h ^ c.charCodeAt(0), 16777619);
  return () => {
    h = Math.imul(h ^ (h >>> 15), 2246822507);
    h = Math.imul(h ^ (h >>> 13), 3266489909);
    h ^= h >>> 16;
    return (h >>> 0) / 4294967296;
  };
}

/**
 * GET /api/providers/{id}/usage?days= for the mock: per-day requests scaled to the provider's own
 * tally, and — for a provider with a balance reader — a sawtooth of readings whose current cycle ends
 * at the reader's reset time and lands on the balance the list shows.
 */
function usageReport(p: ModelProvider | null, id: string, days: number): ProviderUsageReport {
  const rand = seeded(id);
  const DAY = 86_400_000;
  const nowMs = Date.now();
  const total = p?.usage?.requests ?? 0;
  const failPct = (p?.health?.failure_pct ?? 0) / 100;
  const weights = Array.from({ length: days }, (_, i) => 0.55 + rand() * 0.9 + (i === days - 1 ? -0.2 : 0));
  const wsum = weights.reduce((a, b) => a + b, 0);
  const perDay = total > 0 ? Math.max(1, total / Math.max(30, days * 4)) * days : 0;
  const daily = weights.map((w, i) => {
    const date = new Date(nowMs - (days - 1 - i) * DAY).toISOString().slice(0, 10);
    const requests = Math.round((perDay * w) / wsum);
    const failures = Math.round(requests * Math.max(0, failPct * (0.6 + rand() * 0.8)));
    return { date, requests, failures, avg_latency_ms: requests ? Math.round((p?.health?.avg_latency_ms ?? 2000) * (0.8 + rand() * 0.4)) : 0 };
  });
  const b = p?.balance;
  const balance: ProviderUsageReport["balance"] = [];
  const events: ProviderUsageReport["events"] = [];
  if (b?.limit && b.reset_unix) {
    const limit = b.limit;
    const resetMs = b.reset_unix * 1000;
    const r = b.remaining / limit;
    const P = id === "byteplus" ? 5 * DAY : 7 * DAY;
    const frac = (t: number) => ((((t - (resetMs - P)) / P) % 1) + 1) % 1;
    const nowFrac = frac(nowMs);
    const nowCycleStart = resetMs - P;
    const curve = (f: number, top: number) => Math.min(1, top) * Math.pow(f, 0.7);
    const topNow = (1 - r) / Math.pow(nowFrac, 0.7);
    for (let t = nowMs - days * DAY; t <= nowMs; t += 2 * 3600_000) {
      const cycleStart = nowCycleStart - Math.ceil((nowCycleStart - t) / P) * P;
      const f = frac(t);
      const top = cycleStart >= nowCycleStart ? topNow : 0.8 + rand() * 0.2;
      balance.push({ at: new Date(t).toISOString(), remaining: Math.round(limit * (1 - curve(f, top))), limit });
      if (f < 2 * 3600_000 / P && t > nowMs - days * DAY + 3 * 3600_000) events.push({ at: new Date(cycleStart).toISOString(), kind: "reset" });
    }
    balance.push({ at: new Date(nowMs).toISOString(), remaining: b.remaining, limit, reset_unix: b.reset_unix });
  }
  const report: ProviderUsageReport = { provider: id, days, daily, balance, events, has_balance: balance.length > 0 };
  if (p?.quota_exhausted) report.exhausted = { reset_at: p.quota_exhausted.reset_at ?? null, reset_unix: p.quota_exhausted.reset_unix ?? null };
  return report;
}

export function providersMock(ms: MockState): ProvidersApi {
  return {
    providerUsage: async (id, days = 7) => {
      const provider = ms.providers.find((p) => p.id === id) ?? null;
      if (!provider && id !== "anthropic") throw new ApiError("no such provider", 404);
      await sleep(180);
      return clone(usageReport(provider, id, days));
    },
    skillset: async (name) => {
      if (!mockDownloads[name]) throw new ApiError("no such skillset", 404);
      return clone(tickGraft(name));
    },
    skillsetDownload: async (name) => {
      if (!mockDownloads[name]) throw new ApiError("no such skillset", 404);
      const status = mockDownloads[name];
      if (status.state === "idle" || status.state === "failed") {
        mockDownloads[name] = { ...status, state: "downloading", bytes: 0, total: downloadBytes(name), started_at: new Date().toISOString(), finished_at: null, error: null };
      }
      return clone(mockDownloads[name]);
    },
    plugins: () =>
      ms.later(() => ({
    local_root: "/home/you/.local/share/colonizer/plugins",
    plugins: [
      {
        name: "ecc",
        description: "Harness-native ECC plugin for engineering teams - 68 agents, 286 skills, 94 legacy command shims",
        version: "2.2.1",
        source: "vendored" as const,
        shadows_vendored: false,
        skills: 286,
        agents: 68,
        commands: 94,
      },
      {
        name: "team-skills",
        description: "House style, release checklist and the incident runbook",
        version: "0.3.0",
        source: "local" as const,
        shadows_vendored: false,
        skills: 4,
        agents: 0,
        commands: 1,
      },
      // Once downloaded, graft is an ordinary local skillset.
      ...(tickGraft("graft").state === "installed"
        ? [{ name: "graft", description: "A code map of the colony's repository (graft by Nanonets)", version: "0.19.0", source: "local" as const, shadows_vendored: false, skills: 1, agents: 0, commands: 0 }]
        : []),
      ...(tickGraft("understand-anything").state === "installed"
        ? [{ name: "understand-anything", description: "A knowledge graph of the colony's repository (understand-anything by Egonex)", version: "2.9.0", source: "local" as const, shadows_vendored: false, skills: 9, agents: 0, commands: 9 }]
        : []),
    ],
    downloadable: Object.keys(mockDownloads).map((name) => clone(tickGraft(name))),
      })),
    providers: () => ms.later(() => ms.providers),
    saveProvider: async (id, body) => {
      await sleep(250);
      if (!/^[a-z0-9][a-z0-9-]{0,31}$/.test(id) || id === "anthropic") {
    throw new ApiError('provider ids are lowercase letters, digits and dashes, and can\'t be "anthropic"', 400);
      }
      if (!body.name.trim()) throw new ApiError("provider name must be 1-60 characters", 400);
      if (!/^https?:\/\/[^\s/]+/.test(body.base_url.trim())) throw new ApiError("base URL must be an http(s) URL like https://api.deepseek.com/anthropic", 400);
      for (const [key, min, max] of ms.LIMIT_RANGES) {
    const value = body[key];
    if (value != null && (!Number.isInteger(value) || (value as number) < min || (value as number) > max)) {
      throw new ApiError(`${key} must be between ${min} and ${max}`, 400);
    }
      }
      const fallback = body.fallback_model?.trim() || null;
      if (fallback && (fallback.includes("/") || !/^[a-z0-9][a-z0-9.-]*$/.test(fallback))) {
    throw new ApiError("fallback_model must be an Anthropic model id or alias like sonnet", 400);
      }
      for (const rate of Object.values(body.pricing ?? {})) {
    if (!Number.isFinite(rate) || rate < 0) {
      throw new ApiError("pricing rates must be dollar amounts per million tokens, zero or more", 400);
    }
      }
      const existing = ms.providers.find((p) => p.id === id);
      const has_key = body.api_key === undefined ? (existing?.has_key ?? false) : body.api_key.trim() !== "";
      const provider: ModelProvider = {
    id,
    name: body.name.trim(),
    base_url: body.base_url.trim().replace(/\/+$/, ""),
    auth: body.auth,
    wire: body.wire ?? existing?.wire ?? "anthropic",
    has_key,
    models: body.models.map((m) => m.trim()).filter(Boolean),
    preset: body.preset ?? existing?.preset ?? "custom",
    timeout_secs: body.timeout_secs ?? ms.DEFAULT_LIMITS.timeout_secs,
    max_concurrent: body.max_concurrent ?? null,
    queue_timeout_secs: body.queue_timeout_secs ?? null,
    context_tokens: body.context_tokens ?? null,
    fallback_model: fallback,
    // Omitted keeps the saved rates, like the key; the Mothership treats an all-0 object the same as none.
    pricing: body.pricing ?? existing?.pricing ?? null,
    // Omitted keeps the saved probe; an empty URL clears it, like the Mothership.
    quota:
      body.quota === undefined
        ? existing?.quota ?? null
        : body.quota.url.trim()
          ? { url: body.quota.url.trim(), pointer: body.quota.pointer.trim(), ...(body.quota.reset_pointer ? { reset_pointer: body.quota.reset_pointer } : null) }
          : null,
    // Omitted keeps the saved mark; the model map and disabled tools follow the same convention.
    trusted: body.trusted ?? existing?.trusted ?? false,
    model_map: body.model_map ?? existing?.model_map ?? {},
    disabled_tools: body.disabled_tools ?? existing?.disabled_tools ?? [],
    in_flight: existing?.in_flight ?? 0,
    queued: existing?.queued ?? 0,
    usage: existing?.usage ?? ms.zeroUsage(),
    health: existing?.health ?? ms.zeroHealth(),
    used_by: existing?.used_by ?? [],
      };
      if (existing) Object.assign(existing, provider);
      else ms.providers.push(provider);
      return clone(provider);
    },
    deleteProvider: async (id) => {
      const index = ms.providers.findIndex((p) => p.id === id);
      if (index < 0) throw new ApiError("no such provider", 404);
      ms.providers.splice(index, 1);
      return { ok: true };
    },
    attention: async () => ({ quota_cards: [] }),
    quotaAction: async (provider, body) => {
      if (!ms.providers.some((p) => p.id === provider)) throw new ApiError("no such provider", 404);
      return { action: body.action, provider, colonies: [], failed: [] };
    },
    providerHealth: async (id) => {
      const provider = ms.providers.find((p) => p.id === id);
      if (!provider) throw new ApiError("no such provider", 404);
      // strix is a local server that is switched off; custom endpoints answer but have no /v1/models,
      // which the Mothership reports as a note (still healthy) for anthropic wire and an error for openai.
      await sleep(provider.id === "strix" ? 2200 : 700);
      const checked_at = now();
      if (provider.id === "strix") {
    return { reachable: false, status: null, latency_ms: null, models: [], error: "connect timed out after 5 s", note: null, checked_at };
      }
      if (provider.preset === "custom") {
    return provider.wire === "anthropic"
      ? { reachable: true, status: 404, latency_ms: 38, models: [], error: null, note: "no model list", checked_at }
      : { reachable: true, status: 404, latency_ms: 38, models: [], error: "GET /v1/models returned 404", note: null, checked_at };
      }
      return {
        reachable: true,
        status: 200,
        latency_ms: 42,
        models: provider.models.length ? provider.models : ["ds4-flash"],
        error: null,
        note: null,
        // The plan balance rides along once the provider has a quota probe configured (issue #199).
        ...(provider.quota ? { quota: { remaining: 12_345_678, error: null } } : null),
        checked_at,
      };
    },
    testProvider: async (id) => {
      const provider = ms.providers.find((p) => p.id === id);
      if (!provider) throw new ApiError("no such provider", 404);
      await sleep(600);
      const model = provider.models[0] ?? null;
      if (!model) return { ok: false, url: null, status: null, model: null, latency_ms: null, error: "list at least one model to test with" };
      const base = provider.base_url.replace(/\/+$/, "");
      const path = provider.wire === "openai" ? "/chat/completions" : "/messages";
      const url = /\/v\d+$/.test(base) ? `${base}${path}` : `${base}/v1${path}`;
      // Custom endpoints in the demo stand for a base URL that misses the provider's API root.
      return provider.preset === "custom"
        ? { ok: false, url, status: 404, model, latency_ms: 41, error: `HTTP 404 (upstream answered 404 at ${url}; check the provider's base URL)` }
        : { ok: true, url, status: 200, model, latency_ms: 380, error: null };
    },
    models: () =>
      ms.later((): ModelOption[] => [
    ...ms.ANTHROPIC_MODELS.map(([id, label]) => ({ id, label, provider: "anthropic" })),
    ...ms.providers.flatMap((p) => p.models.map((model) => ({ id: `${p.id}/${model}`, label: `${model} · ${p.name}`, provider: p.id }))),
      ])
  };
}
