export interface ModuleProviderInfo {
  id: string;
  name: string;
  description?: string;
  /** Agent rows only: the runner serves the loop MCP tools `loop_next` and `loop_stop` (issue #643). */
  loop_tools?: boolean;
  /** Agent rows only: the module loads skill packs (issue #1164). */
  skill_packs?: boolean;
}

/** A small JSON-Schema subset: an object whose properties are scalar settings. */
export interface SchemaField {
  type?: "string" | "number" | "integer" | "boolean" | "array";
  /** A rendering hint: `plugin-dirs` shows a comma-separated list of plugin names as skillset switches. */
  format?: string;
  /** For `type: "array"`: what the entries are. Only string arrays are supported. */
  items?: { type?: "string" };
  title?: string;
  description?: string;
  enum?: (string | number)[];
  default?: unknown;
  minimum?: number;
  maximum?: number;
}

export interface SettingsSchema {
  type?: "object";
  properties?: Record<string, SchemaField>;
  required?: string[];
}

export interface ModuleInfo {
  kind: string;
  provider: string;
  providers: ModuleProviderInfo[];
  enabled: boolean;
  settings: Record<string, unknown>;
  schema: SettingsSchema | null;
}

/** Why the autonomy judge's last call failed (issue #875), as the mothership classified it. */
export type JudgeFailureKind = "provider_error" | "rate_limited" | "timeout" | "unreachable" | "refused";

/**
 * GET /api/autonomy/status (issue #875): the autonomy judge's recent health, for the Settings
 * status line and the header's warning chip. `last_success` and `last_error` are null until the
 * judge has answered or failed once; `consecutive_failures` counts the run of failures since the
 * last success, and `alerted` says whether the mothership has already told the operator about them.
 */
export interface AutonomyStatus {
  enabled: boolean;
  model: string | null;
  fallback_models: string[];
  /** Set when autonomy is on with no model — the one way "on" is still silent (issue #776). */
  problem?: string | null;
  last_success: { at: string; model: string } | null;
  last_error: { at: string; model: string; kind: JudgeFailureKind; status: number | null; message: string } | null;
  consecutive_failures: number;
  alerted: boolean;
}

/** One webhook delivery as GET /api/notify/deliveries lists it (issue #898): never the body, and the address without its query. */
export interface WebhookDelivery {
  key: string;
  event_id: string;
  event: string;
  target: string;
  url: string;
  colony: string | null;
  attempts: number;
  first_at: string;
  last_at: string;
  /** When the next retry is due; null in the dead letter. */
  next_at: string | null;
  last_error: string;
}

/** GET /api/notify/deliveries (issue #898): what is waiting for a retry, the dead letter (newest first), and the last success. */
export interface WebhookDeliveries {
  pending: WebhookDelivery[];
  dead_letters: WebhookDelivery[];
  last_success_at: string | null;
  max_attempts: number;
}

/**
 * GET /api/observability/status (issue #839): whether OTLP export is on, why not, where each
 * setting came from, the header names (never values) and the exporter add-on's own health.
 */
export interface ObservabilityStatus {
  state: "off" | "invalid" | "no_addon" | "starting" | "running" | "restarting" | "refused" | "failed";
  configured: boolean;
  reason?: string;
  error?: string | null;
  endpoint?: string;
  protocol?: string;
  headers?: { source: "env" | "secret" | "none"; names: string[] };
  exporter?: { state?: string; exported?: number; last_error?: string | null; dropped?: Record<string, number> } | null;
}

/** POST /api/observability/test: what the backend answered for each signal. */
export interface ObservabilityTest {
  ok: boolean;
  error?: string;
  signals?: Record<string, { ok: boolean; status?: number; error?: string; rejected?: number }>;
}
