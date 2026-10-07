// The `modules` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, clone, sleep } from "../../mockShared";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { ModulesApi } from "./api";
import type { WebhookDelivery } from "./types";

let mockDeadLetters: WebhookDelivery[] = [
  {
    key: "evt_5b1f0c9e7a2d4e6f8a0b1c2d3e4f5a6b.owner",
    event_id: "evt_5b1f0c9e7a2d4e6f8a0b1c2d3e4f5a6b",
    event: "pull_request",
    target: "owner",
    url: "https://hooks.example.com/colonizer",
    colony: null,
    attempts: 6,
    first_at: ago(120),
    last_at: ago(105),
    next_at: null,
    last_error: "the webhook answered 503 Service Unavailable",
  },
];

export function modulesMock(ms: MockState): ModulesApi {
  return {
    modules: () => ms.later(() => ms.modules),
    saveModule: async (kind, body) => {
      await sleep(250);
      const module = ms.modules.find((m) => m.kind === kind);
      if (!module) throw new ApiError("unknown module kind", 404);
      if (!module.providers.some((p) => p.id === body.provider)) throw new ApiError("unknown provider", 400);
      Object.assign(module, { provider: body.provider, enabled: body.enabled, settings: body.settings });
      return clone(module);
    },
    // The autonomy judge's health (issue #875): a healthy judge with a recent answer, so the
    // Settings status line and the header chip have something to read in mock mode.
    autonomyStatus: () =>
      ms.later(() => ({
        enabled: true,
        model: "deepseek/deepseek-flash",
        fallback_models: ["anthropic/claude-haiku-4-5"],
        last_success: { at: ago(2), model: "deepseek/deepseek-flash" },
        last_error: null,
        consecutive_failures: 0,
        alerted: false,
      })),
    // Export is off until the module is saved and enabled; the mock never sends anything.
    observabilityStatus: () =>
      ms.later(() => {
        const module = ms.modules.find((m) => m.kind === "observability");
        return module?.enabled
          ? { state: "running" as const, configured: true, endpoint: String(module.settings?.endpoint ?? ""), headers: { source: "none" as const, names: [] } }
          : { state: "off" as const, configured: false, reason: "the observability module is not configured" };
      }),
    observabilityTest: async () => {
      await sleep(400);
      return { ok: true, signals: { logs: { ok: true, rejected: 0 }, metrics: { ok: true, rejected: 0 } } };
    },
    // Webhook deliveries (issue #898): one dead letter, so the Settings panel has a row in mock mode.
    webhookDeliveries: () =>
      ms.later(() => ({
        pending: [],
        dead_letters: mockDeadLetters,
        last_success_at: ago(30),
        max_attempts: 6,
      })),
    replayDeadLetter: async (key) => {
      await sleep(300);
      mockDeadLetters = mockDeadLetters.filter((d) => d.key !== key);
      return { delivered: true, error: null };
    },
    discardDeadLetter: async (key) => {
      await sleep(150);
      mockDeadLetters = mockDeadLetters.filter((d) => d.key !== key);
      return { discarded: key };
    },
  };
}
