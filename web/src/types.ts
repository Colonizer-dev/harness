// Types for the harness browser API (docs/protocol.md §4, §6.3) and the agent event vocabulary (§2–3).

export type SessionStatus =
  | "queued"
  | "starting"
  | "running"
  | "waiting_for_answer"
  | "idle"
  | "publishing"
  | "pr_opened"
  | "merged"
  | "closed"
  | "no_changes"
  | "stopped"
  | "failed";

export type AttentionReason = "stalled" | "waiting_for_answer" | "nudges_exhausted" | "autopilot_held";

/** Set by the watchdog or autopilot (§6.3); cleared by the next agent event. */
export interface Attention {
  reason: AttentionReason;
  since: string;
  nudges: number;
}

export interface Session {
  id: string;
  repo: string;
  /** Repository owner; older mothership builds omit it, see `orgOf`. */
  org?: string;
  /** null for an open session started on a repository without an issue. */
  issue: number | null;
  issue_title: string;
  status: SessionStatus;
  branch: string;
  base: string | null;
  worktree: string;
  sandbox: string;
  mesh: { name: string; ip: string | null } | null;
  agent: string;
  autopilot: boolean;
  pr_url: string | null;
  error: string | null;
  /** Claude models only; routed models are counted in `model_usage` as tokens. */
  cost_usd: number | null;
  /** Cumulative tokens per model, as of the last turn end. */
  model_usage?: Record<string, ModelTokens> | null;
  cleaned_up: boolean;
  created_at: string;
  updated_at: string;
  last_activity_at?: string | null;
  attention?: Attention | null;
}

export interface ModelTokens {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
}

export interface Repo {
  full_name: string;
  description: string | null;
  private: boolean;
  fork: boolean;
  archived: boolean;
  open_issues_count: number;
  pushed_at: string | null;
  has_issues?: boolean;
}

export interface Issue {
  number: number;
  title: string;
  body: string | null;
  labels: { name: string; color: string }[];
  author: { login: string } | null;
  updatedAt: string;
  url: string;
}

export interface HarnessStatus {
  github: { connected: boolean; login?: string; name?: string | null; source?: string; error?: string };
  claude: { configured: boolean; source: string | null; kind: string | null };
  sandbox: {
    msb_version: string | null;
    image: string;
    cpus?: number;
    memory?: string;
    max_parallel?: number;
    claude_bin?: string | null;
    claude_bin_error?: string | null;
  };
  mesh?: {
    enabled: boolean;
    provider?: string;
    state?: string;
    harness_ip?: string | null;
    nodes?: number;
    error?: string | null;
  } | null;
}

export interface ModuleProviderInfo {
  id: string;
  name: string;
  description?: string;
}

/** A small JSON-Schema subset: an object whose properties are scalar settings. */
export interface SchemaField {
  type?: "string" | "number" | "integer" | "boolean";
  /** A rendering hint: `plugin-dirs` shows a comma-separated list of plugin names as skillset switches. */
  format?: string;
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

// ---------------------------------------------------------------------------
// Model providers (§6.3)
// ---------------------------------------------------------------------------

export type ProviderAuth = "x-api-key" | "bearer" | "none";
/** A built-in preset ("deepseek", "local", "custom", …) or an id from the provider catalogue. */
export type ProviderPreset = string;
/** The protocol the endpoint speaks. `anthropic` is proxied as-is; `openai` is translated by the gateway. */
export type ProviderWire = "anthropic" | "openai";

/** Gateway settings the Mothership applies to every request to a provider. */
export interface ProviderLimits {
  /** Effective request timeout (default 600). */
  timeout_secs: number;
  /** null = unlimited. */
  max_concurrent: number | null;
  /** null = same as `timeout_secs`. */
  queue_timeout_secs: number | null;
  context_tokens: number | null;
  /** An Anthropic model id or alias used when the provider fails. */
  fallback_model: string | null;
}

export interface ModelProvider extends ProviderLimits {
  id: string;
  name: string;
  base_url: string;
  auth: ProviderAuth;
  wire: ProviderWire;
  has_key: boolean;
  models: string[];
  preset: ProviderPreset;
  /** Live counts across all colonies. */
  in_flight: number;
  queued: number;
}

export interface SaveProviderRequest {
  name: string;
  base_url: string;
  auth: ProviderAuth;
  /** Omitted means `anthropic`. */
  wire?: ProviderWire;
  models: string[];
  preset?: ProviderPreset;
  /** Omitted keeps the saved key; `""` removes it. */
  api_key?: string;
  /** For each limit, null uses the default. */
  timeout_secs?: number | null;
  max_concurrent?: number | null;
  queue_timeout_secs?: number | null;
  context_tokens?: number | null;
  fallback_model?: string | null;
}

/** GET /api/providers/{id}/health */
/** A background pull of the colony image. No percentage: msb reports none when piped. */
export type PullState = "idle" | "cached" | "pulling" | "done" | "failed";

export interface PullStatus {
  image: string;
  state: PullState;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** GET /api/headroom: the Headroom bundle a colony runs Headroom from, downloaded when it is switched on. */
export type HeadroomState = "idle" | "installed" | "downloading" | "unpacking" | "failed" | "unavailable";

export interface HeadroomStatus {
  /** The pinned bundle release; null when none is published for this machine's architecture. */
  release: string | null;
  state: HeadroomState;
  bytes: number;
  total: number | null;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** GET /api/telemetry: the live map on colonizer.dev (docs/telemetry.md). */
export interface TelemetryStatus {
  /** null until the user has answered. */
  enabled: boolean | null;
  /** An environment variable keeping it off whatever Settings says (DO_NOT_TRACK or COLONIZER_TELEMETRY). */
  blocked_by: string | null;
  endpoint: string;
  map_url: string;
  last_sent_at: string | null;
  last_error: string | null;
  /** Exactly what the next heartbeat carries; install_id is null until the live map is first switched on. */
  heartbeat: { install_id: string | null; version: string; platform: string; colonies: number };
}

export interface ProviderHealth {
  reachable: boolean;
  /** HTTP status of the probe, when a response arrived. */
  status: number | null;
  latency_ms: number | null;
  models: string[];
  error: string | null;
  checked_at: string;
}

export interface ModelOption {
  id: string;
  label: string;
  provider: string;
}

// ---------------------------------------------------------------------------
// Skillsets: plugin directories a colony can load (§6.3)
// ---------------------------------------------------------------------------

export interface PluginDir {
  name: string;
  description: string | null;
  version: string | null;
  /** `vendored` ships with the app; `local` is in the mothership's plugins folder. */
  source: "vendored" | "local";
  /** A local copy of a vendored plugin, loaded instead of it. */
  shadows_vendored: boolean;
  skills: number;
  agents: number;
  commands: number;
}

/** GET /api/plugins */
export interface PluginListing {
  /** Where an operator puts their own plugin directories. */
  local_root: string;
  plugins: PluginDir[];
}

// ---------------------------------------------------------------------------
// Org workspaces (§6.3)
// ---------------------------------------------------------------------------

/** Every field is optional; missing or null inherits the global module setting. */
export interface OrgSettings {
  agent?: {
    model?: string | null;
    subagent_model?: string | null;
    background_model?: string | null;
    /** Skillsets this org switches on (`true`) or off (`false`); unnamed ones follow the global switches. */
    skillsets?: Record<string, boolean> | null;
  } | null;
  max_parallel?: number | null;
  memory?: { enabled?: boolean | null } | null;
  watchdog?: { enabled?: boolean | null; stall_minutes?: number | null; max_nudges?: number | null } | null;
}

export interface OrgInfo {
  org: string;
  colonies: { live: number; total: number };
  pending_memory: number;
  settings: OrgSettings;
}

// ---------------------------------------------------------------------------
// Shared memory (§6.2–6.3)
// ---------------------------------------------------------------------------

/** `key` is "" for global, the org for `org`, and `owner/repo` for `repo`. */
export type MemoryScope = "global" | "org" | "repo";

export type MemorySource = { session_id: string; repo: string } | { user: true };

export interface MemoryNote {
  id: string;
  scope: MemoryScope;
  key: string;
  title: string;
  content: string;
  tags: string[];
  created_at: string;
  source: MemorySource;
}

export interface MemoryProposal extends MemoryNote {
  status: "pending";
}

export interface MemoryListing {
  scope: MemoryScope;
  key: string;
  notes: MemoryNote[];
  proposals: MemoryProposal[];
}

export interface NewNoteRequest {
  scope: MemoryScope;
  key: string;
  title: string;
  content: string;
}

// ---------------------------------------------------------------------------
// Claude login and the event vocabulary
// ---------------------------------------------------------------------------

export type LoginState = "idle" | "starting" | "awaiting_code" | "verifying" | "done" | "error";

export interface LoginView {
  state: LoginState;
  url: string | null;
  message: string | null;
}

export interface QuestionOption {
  label: string;
  description?: string;
  preview?: string | null;
}

export interface Question {
  question: string;
  header: string;
  multi_select: boolean;
  options: QuestionOption[];
}

export type Answers = Record<string, string | string[]>;

export type AgentState = "idle" | "working" | "waiting_for_answer" | "error" | "exited";

export type LogLevel = "info" | "warn" | "error";

/** The subagent that produced an event. Absent on the orchestrator's own events. */
export interface AgentRef {
  /** The Task tool call that started it, which is also its identity for the run. */
  id: string;
  /** The subagent type, or its task description when the type was not named. */
  name: string;
  description?: string | null;
}

interface Sequenced {
  seq?: number;
  ts?: string;
  agent?: AgentRef;
}

export type AgentEventBody =
  | { type: "status"; state: AgentState; detail?: string | null }
  | { type: "user_message"; id: string; text: string }
  | { type: "assistant_text_delta"; message_id: string; block_index: number; delta: string }
  | { type: "assistant_text"; message_id: string; block_index: number; text: string }
  | { type: "thinking"; message_id: string; block_index: number; text: string }
  | { type: "tool_call"; message_id: string; tool_call_id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; tool_call_id: string; output: string; is_error: boolean }
  | { type: "question"; question_id: string; message_id?: string; questions: Question[] }
  | { type: "question_answered"; question_id: string; answers: Answers; response?: string | null }
  | {
      type: "turn_end";
      is_error: boolean;
      result: string | null;
      cost_usd: number | null;
      duration_ms: number | null;
      model_usage?: Record<string, ModelTokens>;
    }
  | { type: "log"; level: LogLevel; message: string };

export type AgentEvent = Sequenced & AgentEventBody;

export type ServerFrame =
  | AgentEvent
  | { type: "session"; session: Session }
  | { type: "harness_log"; level: LogLevel; message: string; ts: string }
  | { type: "memory_proposed"; proposal: MemoryProposal };

export type ClientCommand =
  | { type: "user_message"; text: string }
  | { type: "answer"; question_id: string; answers: Answers; response: string | null }
  | { type: "interrupt" };

export interface NewSessionRequest {
  repo: string;
  /** Omit to start an open session on the repository. */
  issue?: number;
  title?: string;
  instructions?: string;
  autopilot?: boolean;
}
