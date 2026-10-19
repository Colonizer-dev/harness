// Modules API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, put, request } from "../../http";
import type {
  AutonomyStatus,
  ModuleInfo,
  ObservabilityAddonInstall,
  ObservabilityAddonRemove,
  ObservabilityStatus,
  ObservabilityTest,
  WebhookDeliveries,
} from "./types";

export interface SaveModuleRequest {
  provider: string;
  enabled: boolean;
  settings: Record<string, unknown>;
  /**
   * PUT /api/modules/autonomy (issue #875) refuses a save whose judge model fails a test call
   * unless this is true: the "Save anyway" the pane offers alongside the provider's error.
   */
  save_anyway?: boolean;
  /** PUT /api/modules/observability refuses to turn on conversation content or thinking unless the
   *  pane showed the confirmation and the operator accepted (#855). Validated, never persisted. */
  confirm_content?: boolean;
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
  /** PUT /api/observability/addon (issue #1060): installs the exporter add-on and tries to enable it. */
  observabilityInstall(): Promise<ObservabilityAddonInstall>;
  /** DELETE /api/observability/addon (issue #1060): removes the add-on; `purge` also drops its files. */
  observabilityRemove(purge: boolean): Promise<ObservabilityAddonRemove>;
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
  observabilityInstall: () => put("/api/observability/addon", {}),
  observabilityRemove: (purge) => del(`/api/observability/addon${purge ? "?purge=1" : ""}`),
  webhookDeliveries: () => request("/api/notify/deliveries"),
  replayDeadLetter: (key) => post(`/api/notify/dead-letters/${enc(key)}/replay`),
  discardDeadLetter: (key) => del(`/api/notify/dead-letters/${enc(key)}`),
};
