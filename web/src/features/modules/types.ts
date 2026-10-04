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
