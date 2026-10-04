// The mock's per-call state slice for the providers feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { ModelProvider } from "../../types";
import { ago } from "../../mockShared";
import type { MockState } from "../../mockState";

export type ProvidersMockState = {
    providers: ModelProvider[];
    DEFAULT_LIMITS: {
        timeout_secs: number;
        max_concurrent: null;
        queue_timeout_secs: null;
        context_tokens: null;
        fallback_model: null;
    };
    zeroUsage: () => {
        requests: number;
        failures: number;
        fallbacks: number;
        duration_ms: number;
        since: null;
        last_request_at: null;
    };
    zeroHealth: () => {
        failure_pct: number;
        avg_latency_ms: number;
        rated: boolean;
        degraded: boolean;
    };
    LIMIT_RANGES: ["context_tokens" | "fallback_model" | "max_concurrent" | "queue_timeout_secs" | "timeout_secs", number, number][];
    ANTHROPIC_MODELS: [string, string][];
};

export function installProvidersMockState(ms: MockState): void {
  ms.DEFAULT_LIMITS = { timeout_secs: 600, max_concurrent: null, queue_timeout_secs: null, context_tokens: null, fallback_model: null };
  ms.zeroUsage = () => ({ requests: 0, failures: 0, fallbacks: 0, duration_ms: 0, since: null, last_request_at: null });
  // What the Mothership computes for a tally with no requests: numbers of zero, and nothing rated.
  ms.zeroHealth = () => ({ failure_pct: 0, avg_latency_ms: 0, rated: false, degraded: false });
  ms.providers = [
    {
      id: "deepseek",
      name: "DeepSeek",
      base_url: "https://api.deepseek.com/anthropic",
      auth: "x-api-key",
      wire: "anthropic",
      has_key: true,
      models: ["deepseek-flash", "deepseek-v4-pro"],
      preset: "deepseek",
      // Vetted for restricted-sensitivity work, so the Trusted switch shows on (#472).
      trusted: true,
      // Priced, so routed spend and the budget can be exercised; strix and lab stay unpriced ($0).
      pricing: { input_per_mtok: 0.27, output_per_mtok: 1.1, cache_read_per_mtok: 0.07, cache_write_per_mtok: 0.27 },
      // A prepaid plan with a balance endpoint, so the health line shows "… left in plan" (issue #199).
      quota: { url: "https://api.deepseek.com/plan", pointer: "/data/remaining_tokens" },
      ...ms.DEFAULT_LIMITS,
      in_flight: 0,
      queued: 0,
      // Wired only to the subagent model and the small-task tier, and no colony has ever
      // delegated: the issue #39 state.
      usage: ms.zeroUsage(),
      health: ms.zeroHealth(),
      used_by: ["subagent_model", "model_low"],
    },
    {
      id: "strix",
      name: "Strix Halo",
      base_url: "http://strix.tail4c2e.ts.net:8080",
      auth: "none",
      wire: "anthropic",
      has_key: false,
      models: ["ds4-flash"],
      preset: "local",
      trusted: false,
      timeout_secs: 900,
      max_concurrent: 1,
      queue_timeout_secs: null,
      context_tokens: 131072,
      fallback_model: "sonnet",
      in_flight: 1,
      queued: 2,
      // The orchestrator model for acme, so it sees a steady stream of requests — and, issue #184's
      // report, it fails 29.4% of them at 12.8 s apiece: rated, well past the 10% mark.
      usage: {
        requests: 32_689,
        failures: 9_599,
        fallbacks: 12,
        duration_ms: 418_419_200,
        since: "2026-03-12T09:00:00Z",
        last_request_at: ago(2),
      },
      health: { failure_pct: 29.4, avg_latency_ms: 12_800, rated: true, degraded: true },
      used_by: ["model"],
    },
    {
      id: "lab",
      name: "Lab vLLM",
      base_url: "http://10.0.4.20:8000",
      auth: "bearer",
      wire: "anthropic",
      has_key: true,
      models: ["qwen3-coder"],
      preset: "custom",
      trusted: false,
      ...ms.DEFAULT_LIMITS,
      max_concurrent: 4,
      in_flight: 0,
      queued: 0,
      // Reachable, but no model setting points at it, so nothing ever will.
      usage: ms.zeroUsage(),
      health: ms.zeroHealth(),
      used_by: [],
    },
  ];
  ms.LIMIT_RANGES = [
    ["timeout_secs", 30, 3600],
    ["max_concurrent", 1, 64],
    ["queue_timeout_secs", 1, 3600],
    ["context_tokens", 1024, 2_000_000],
  ];
  ms.ANTHROPIC_MODELS = [
    ["opus", "Claude Opus (latest)"],
    ["sonnet", "Claude Sonnet (latest)"],
    ["haiku", "Claude Haiku (latest)"],
    ["fable", "Claude Fable (latest)"],
    ["claude-opus-5-5", "Claude Opus 5.5"],
    ["claude-opus-5", "Claude Opus 5"],
    ["claude-sonnet-5", "Claude Sonnet 5"],
    ["claude-haiku-4-5", "Claude Haiku 4.5"],
  ];
}
