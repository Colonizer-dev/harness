// The `modules` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, clone, sleep } from "../../mockShared";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { ModulesApi } from "./api";
import type { ObservabilityStatus, WebhookDelivery } from "./types";

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
    // Export is off until the module is saved and enabled; the mock never sends anything. Enabled,
    // it shows every card state at once (issue #855): one signal per exporter state, so the health
    // card, the provenance line and the degraded badge all have something to read in mock mode.
    observabilityStatus: () =>
      ms.later((): ObservabilityStatus => {
        const module = ms.modules.find((m) => m.kind === "observability");
        if (!module?.enabled) return { state: "off", configured: false, reason: "the observability module is not configured" };
        const endpoint = String(module.settings?.endpoint ?? "");
        const nowSecs = Math.floor(Date.now() / 1000);
        return {
          state: "running",
          configured: true,
          endpoint,
          protocol: String(module.settings?.protocol ?? "http/protobuf"),
          service_name: "colonizer-mothership",
          provenance: { endpoint: "module", protocol: "default", service_name: "env:OTEL_EXPORTER_OTLP_SERVICE_NAME" },
          restarts: 0,
          headers: { source: "secret", names: ["Authorization"] },
          exporter: {
            state: "running",
            endpoint,
            heartbeat_unix: nowSecs - 15,
            last_success_unix: nowSecs - 90,
            last_error: null,
            next_retry_unix: nowSecs + 20,
            consecutive_failures: 0,
            exported: 1284,
            bytes_sent: 8_900_000,
            backlog_bytes: 12_300,
            dropped: {},
            export_failures: 0,
            signals: {
              logs: { state: "ok", endpoint, consecutive_failures: 0, last_success_unix: nowSecs - 60, last_error: null, records_sent: 812, bytes_sent: 5_100_000 },
              traces: { state: "backing_off", endpoint, consecutive_failures: 2, last_success_unix: nowSecs - 240, last_error: "connection refused", records_sent: 302, bytes_sent: 2_800_000 },
              metrics: { state: "auth_failed", endpoint, consecutive_failures: 4, last_success_unix: nowSecs - 600, last_error: "HTTP 401", records_sent: 170, bytes_sent: 1_000_000 },
              profiles: { state: "off", endpoint, consecutive_failures: 0, last_success_unix: null, last_error: null, records_sent: 0, bytes_sent: 0 },
            },
          },
        };
      }),
    // One failing signal (a 401) so the test-result hint line is demonstrable in mock mode (#855).
    observabilityTest: async () => {
      await sleep(400);
      return {
        ok: false,
        signals: {
          logs: { ok: true, rejected: 0 },
          metrics: { ok: true, rejected: 0 },
          traces: { ok: false, auth_failed: true, status: 401, error: "HTTP 401 Unauthorized" },
        },
      };
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
