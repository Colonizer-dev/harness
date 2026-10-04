// The `providers` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { clone, now, sleep } from "../../mockShared";
import type { DownloadableSkillset, ModelOption, ModelProvider } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { ProvidersApi } from "./api";

export let mockGraft: DownloadableSkillset = { name: "graft", release: "0.19.0-1", installed_release: null, state: "idle", bytes: 0, total: null, started_at: null, finished_at: null, error: null };
export const GRAFT_BYTES = 84_213_760;
/** Advances the mock graft download: about four seconds from start to installed. */
export function tickGraft(): DownloadableSkillset {
  if (mockGraft.state === "downloading" && mockGraft.started_at) {
    const bytes = Math.min(GRAFT_BYTES, Math.round(((Date.now() - Date.parse(mockGraft.started_at)) / 4000) * GRAFT_BYTES));
    mockGraft =
      bytes >= GRAFT_BYTES
        ? { ...mockGraft, state: "installed", installed_release: mockGraft.release, bytes, total: GRAFT_BYTES, finished_at: new Date().toISOString() }
        : { ...mockGraft, bytes, total: GRAFT_BYTES };
  }
  return mockGraft;
}

export function providersMock(ms: MockState): ProvidersApi {
  return {
    graftSkillset: async () => clone(tickGraft()),
    graftDownload: async () => {
      if (mockGraft.state === "idle" || mockGraft.state === "failed") {
        mockGraft = { ...mockGraft, state: "downloading", bytes: 0, total: GRAFT_BYTES, started_at: new Date().toISOString(), finished_at: null, error: null };
      }
      return clone(mockGraft);
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
      ...(tickGraft().state === "installed"
        ? [{ name: "graft", description: "A code map of the colony's repository (graft by Nanonets)", version: "0.19.0", source: "local" as const, shadows_vendored: false, skills: 1, agents: 0, commands: 0 }]
        : []),
    ],
    downloadable: [clone(tickGraft())],
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
          ? { url: body.quota.url.trim(), pointer: body.quota.pointer.trim() }
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
    models: () =>
      ms.later((): ModelOption[] => [
    ...ms.ANTHROPIC_MODELS.map(([id, label]) => ({ id, label, provider: "anthropic" })),
    ...ms.providers.flatMap((p) => p.models.map((model) => ({ id: `${p.id}/${model}`, label: `${model} · ${p.name}`, provider: p.id }))),
      ])
  };
}
