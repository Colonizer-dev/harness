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
  /** The colony this one is stacked on: it branched from that colony's branch instead of the default one, which is what `base` then holds. null for an unstacked colony — absent in live data, since the backend omits the field when there is no parent. */
  parent?: string | null;
  worktree: string;
  /** Path of the worktree's git admin dir on the host; null until the worktree was created. */
  git_admin_dir: string | null;
  sandbox: string;
  mesh: { name: string; ip: string | null } | null;
  agent: string;
  autopilot: boolean;
  pr_url: string | null;
  /** How far the last publish attempt got; absent when no publish has made progress. */
  publish_stage?: "committed" | "pushed" | "pr_opened";
  error: string | null;
  /** Claude models only, as the agent itself reports them; routed models are counted in `model_usage` as tokens. */
  cost_usd: number | null;
  /** Cumulative tokens per model, as of the last turn end. */
  model_usage?: Record<string, ModelTokens> | null;
  /** What the gateway recorded for responses it routed to other providers, on top of `cost_usd`. */
  routed_cost_usd?: number | null;
  /** What the colony leaves on the host — its worktree plus its session files — as last measured. */
  host_disk_bytes?: number | null;
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
  github: { connected: boolean; login?: string; name?: string | null; avatar_url?: string | null; source?: string; error?: string };
  claude: { configured: boolean; source: string | null; kind: string | null; account?: string | null; account_note?: string | null; saved_at?: string | null; expires_at?: string | null; expires_estimated?: boolean };
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
    /** A fact about the platform, not a fault: shown plainly, never as an error. */
    detail?: string | null;
    error?: string | null;
  } | null;
  /** Set by the first failed disk write and sticky until the mothership restarts; older mothership builds omit it. */
  storage?: StorageHealth;
  /** The machine facts a colony's first minute depends on (issue #129); older mothership builds omit it. */
  runtime?: RuntimeInfo;
}

/** GET /api/status `runtime` (issue #129): what kind of machine the mothership runs on, and what it can reach. The mothership re-probes all of it; the frontend only reads. */
export interface RuntimeInfo {
  /** `linux-x86_64`, `darwin-arm64`, or `other` for a platform Setup must call unsupported. */
  platform: string;
  /** Whether `/dev/kvm` is readable and writable by the mothership. Linux only; null on other platforms. */
  kvm: { ok: boolean; error: string | null } | null;
  /** Git on the mothership's PATH; colonies use it. */
  git: { ok: boolean; version?: string; error?: string | null };
  /** The GitHub CLI on the mothership's PATH; the installer and colonies use it. */
  gh: { ok: boolean; version?: string; error?: string | null };
  /** The Claude Code binary on the host, used for subscription login; null when not found. */
  host_claude_bin: string | null;
  /** Why the host binary is missing, when it is. */
  host_claude_bin_error: string | null;
}

/** GET /api/status `storage`: whether the mothership can still write its own files (sessions.json, colony event logs). */
export interface StorageHealth {
  ok: boolean;
  /** The underlying write error, for showing verbatim. */
  message?: string | null;
  /** When the failure was recorded; same representation as a harness_log `ts`. */
  ts?: string | null;
  /** Failed writes since the mothership started. */
  failures?: number | null;
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

/**
 * Dollars per million tokens, the rates the gateway prices a provider's routed usage at (§6.5). A rate
 * left out, `0`, or no pricing at all still counts tokens but contributes $0 to `routed_cost_usd`.
 */
export interface ProviderPricing {
  input_per_mtok?: number;
  output_per_mtok?: number;
  cache_read_per_mtok?: number;
  cache_write_per_mtok?: number;
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
  /** null = unpriced: routed tokens are counted but their spend counts as $0. */
  pricing?: ProviderPricing | null;
  /** Live counts across all colonies. */
  in_flight: number;
  queued: number;
  /**
   * Cumulative tallies since the Mothership first kept them; survive a restart. Optional: a
   * Mothership from before it kept tally sends neither this nor `used_by`.
   */
  usage?: ProviderUsage;
  /** The model settings currently routed here; empty means none are, so it stays idle. */
  used_by?: ModelSetting[];
}

/** Cumulative per-provider tallies, kept across restarts. */
export interface ProviderUsage {
  /** Requests dispatched upstream; a gateway-refused request does not count. */
  requests: number;
  /**
   * Requests with no usable response: a gateway fallback error (queue timeout, unreachable,
   * timeout), an upstream status >= 400, or an openai-wire body that failed or never finished.
   * A failure part-way through a streamed body is not counted. A subset of `requests`.
   */
  failures: number;
  /**
   * Failures the Mothership answered with a fallback response, which the colony's router will
   * retry on Claude. A prediction, not an observation. A subset of `failures`.
   */
  fallbacks: number;
  /** Cumulative wall-clock time, including streaming the response body. */
  duration_ms: number;
  /** When the last request was dispatched; null if never. */
  last_request_at: string | null;
}

/**
 * The model settings whose resolved value can route to a provider. The two tier settings are the
 * Mothership's per-task model routing: it reads them itself and strips their env vars from a
 * colony's environment, so only the tier actually in use is probed at boot.
 */
export type ModelSetting =
  | "model"
  | "subagent_model"
  | "background_model"
  | "model_low"
  | "model_high";

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
  /** Omitted keeps the saved rates, like the key; an all-`0` object clears them in effect. */
  pricing?: ProviderPricing;
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

/** GET /api/version: what this mothership was built from. */
export interface BuildInfo {
  version: string;
  commit: string | null;
  dirty: boolean;
  built_at: string;
  release: string | null;
}

/** GET /api/update: the installed build, and the latest release if the check is on. */
export interface UpdateStatus {
  enabled: boolean;
  blocked_by: string | null;
  installed: BuildInfo;
  latest: { version: string; url: string; notes: string; published_at: string | null } | null;
  available: boolean;
  last_checked: string | null;
  error: string | null;
  /// Whether this install can update itself, and why not if it cannot.
  can_apply: { ok: boolean; reason: string | null };
  apply: {
    phase: "idle" | "installing" | "restarting" | "failed";
    version: string | null;
    started_at: string | null;
    error: string | null;
    log: string;
    colonies: { id: string; repo: string; outcome: string }[];
  };
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

/** The anonymous usage batch, exactly what a sender would transmit. Built whatever the switch says, and nothing is sent yet. */
export interface UsageBatch {
  payload_version: number;
  /** Random per on-period; null while reporting is off, so nothing here can be tied to this machine. */
  usage_id: string | null;
  harness_version: string;
  platform: string;
  colonies: {
    parallel_now: string;
    terminal: { pr_opened: string; no_changes: string; stopped: string; failed: string };
  };
  sandbox: { preset: string; image_changed_from_default: boolean };
  autopilot: { enabled: boolean; held: string };
  /** `<kind>.<key>` for every declared setting this install carries — names only, never values. */
  settings_set: string[];
  boot_ms: { phase: string; bucket: string }[];
  providers: string;
  /** Failures and attention reasons under the harness's own labels, bucketed like every count. */
  error_kinds: Record<string, string>;
}

/** GET /api/telemetry/usage, and of a successful PUT. The batch is built whatever the switch says, so it can be read in full. */
export interface UsageStatus {
  /** Already resolved: true when reporting is on — including when nobody has answered, since it is on by default — false once declined or held off by the environment. */
  enabled: boolean;
  /** An environment variable keeping it off whatever Settings says (COLONIZER_TELEMETRY, DO_NOT_TRACK or CI). */
  blocked_by: string | null;
  payload_version: number;
  batch: UsageBatch;
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
  /** Dollars one colony of this org may spend on models in total; 0 opts out of the global budget. */
  budget_usd?: number | null;
  /** The most disk one colony of this org may leave on the host, like `16G`; 0 opts out of the global quota. */
  host_disk?: string | null;
  memory?: { enabled?: boolean | null } | null;
  watchdog?: { enabled?: boolean | null; stall_minutes?: number | null; max_nudges?: number | null } | null;
  /**
   * Off keeps the org out of the workspace list and stops new colonies starting there; its existing
   * colonies stay listed and resumable. Absent and null mean on, like every field above.
   */
  enabled?: boolean | null;
}

export interface OrgInfo {
  org: string;
  colonies: { live: number; total: number };
  pending_memory: number;
  settings: OrgSettings;
  /** The org's GitHub avatar. Absent when unknown — an org that only appears in the colony list has none. */
  avatar_url?: string;
  /**
   * True for a newly-appeared org the operator has not decided about yet; it is not a workspace
   * until then. Optional so an older mothership that never sends it simply has no pending orgs.
   */
  awaiting_decision?: boolean;
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
  /** Where approved notes live: `files` on the Mothership, or `mem0`. */
  provider?: string;
  notes: MemoryNote[];
  proposals: MemoryProposal[];
}

/** Whether a mem0 key is set and where from. The API never returns the key. */
export interface Mem0Status {
  has_key: boolean;
  source: "saved" | "MEM0_API_KEY" | null;
  /** mem0 is the memory module's saved provider. */
  active: boolean;
}

export interface Mem0Check {
  ok: boolean;
  error?: string;
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
      /** Colony-cumulative totals as of this turn, not this turn's own usage (docs/protocol.md §4). */
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
  /** Start a colony on an issue another colony already holds; the mothership answers 409 without it. */
  allow_duplicate?: boolean;
  /** Stack the new colony on another's branch: the parent session's id, which becomes `parent` and whose branch becomes `base`. Launching a stack is API-only; no form picker yet. */
  after?: string;
}
