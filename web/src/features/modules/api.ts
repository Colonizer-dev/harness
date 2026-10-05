// Modules API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type { AutonomyStatus, ModuleInfo, ObservabilityStatus, ObservabilityTest, WebhookDeliveries } from "./types";

export interface SaveModuleRequest {
  provider: string;
  enabled: boolean;
  settings: Record<string, unknown>;
  /**
   * PUT /api/modules/autonomy (issue #875) refuses a save whose judge model fails a test call
   * unless this is true: the "Save anyway" the pane offers alongside the provider's error.
   */
  save_anyway?: boolean;
}

export interface ModulesApi {
  modules(): Promise<ModuleInfo[]>;
  saveModule(kind: string, body: SaveModuleRequest): Promise<ModuleInfo>;
  /** GET /api/autonomy/status (issue #875): the judge's recent health, for Settings and the header chip. */
  autonomyStatus(): Promise<AutonomyStatus>;
  /** GET /api/observability/status (issue #839): export on or off, why, and the add-on's health. */
  observabilityStatus(): Promise<ObservabilityStatus>;
  /** POST /api/observability/test: one test log record and metric point to the saved backend. */
  observabilityTest(): Promise<ObservabilityTest>;
  /** GET /api/notify/deliveries (issue #898): webhook retries waiting, the dead letter and the last success. */
  webhookDeliveries(): Promise<WebhookDeliveries>;
  /** POST /api/notify/dead-letters/{key}/replay: one attempt now; `delivered` false with the receiver's error when it still fails. */
  replayDeadLetter(key: string): Promise<{ delivered: boolean; error: string | null }>;
  /** DELETE /api/notify/dead-letters/{key}: discards one dead letter. */
  discardDeadLetter(key: string): Promise<unknown>;
}

export const modulesHttp: ModulesApi = {
  modules: () => request("/api/modules"),
  saveModule: (kind, body) => put(`/api/modules/${enc(kind)}`, body),
  autonomyStatus: () => request("/api/autonomy/status"),
  observabilityStatus: () => request("/api/observability/status"),
  observabilityTest: () => post("/api/observability/test"),
  webhookDeliveries: () => request("/api/notify/deliveries"),
  replayDeadLetter: (key) => post(`/api/notify/dead-letters/${enc(key)}/replay`),
  discardDeadLetter: (key) => del(`/api/notify/dead-letters/${enc(key)}`),
};
