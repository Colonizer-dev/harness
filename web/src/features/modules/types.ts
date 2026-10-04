export interface ModuleProviderInfo {
  id: string;
  name: string;
  description?: string;
  /** Agent rows only: the runner serves the loop MCP tools `loop_next` and `loop_stop` (issue #643). */
  loop_tools?: boolean;
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
