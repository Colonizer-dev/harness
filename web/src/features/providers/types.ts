/** GET /api/status `model_providers`: each configured model provider's cumulative requests and the health rule's verdict on it (§6.5) — the one rule the providers screen, the status poll and the notify module all share. */
export interface ModelProviderStatus {
  id: string;
  name: string;
  /** Cumulative requests counted at the gateway; the denominator of `failure_pct`. */
  requests: number;
  /** `failures / requests * 100`, one decimal; 0 with no requests. */
  failure_pct: number;
  /** Mean duration of dispatched requests, time queued excluded; 0 with no requests. */
  avg_latency_ms: number;
  /** Rated and at least 10% of requests failed — the verdict the notify module announces (§6.3). */
  degraded: boolean;
}

/**
 * GET /api/status `quota` (issue #225): whether every routable provider's plan is out, and the
 * earliest reset. `paused` holds the colony queue; `reason` is the queue holder's own words.
 */
export interface StatusQuota {
  paused: boolean;
  reason: string | null;
  /** The earliest reset words, e.g. "09-23 07:54 UTC"; null when no reset was named. */
  reset_at: string | null;
  /** The earliest reset as a unix timestamp; null when no reset was named. */
  reset_unix: number | null;
  /** Every exhausted provider's id. */
  providers: string[];
  /**
   * Which scope the pause covers: the Claude account's own cap (`"account"`) or named exhausted
   * providers (`"provider"`). Null when the queue is not paused; absent from older mothership
   * builds, which the banner derives from `providers` instead (see `quotaPauseKind`).
   */
  kind?: "account" | "provider" | null;
  /**
   * Each exhausted plan's display name and the roles routed to it, e.g.
   * `{ id: "byteplus", name: "BytePlus", used_by: ["subagents", "background"] }`; an account pause
   * carries one `anthropic` entry named "Claude". Absent from older mothership builds, which the
   * banner covers by looking the ids up in the provider catalog.
   */
  provider_details?: QuotaProviderDetail[];
}

/** One exhausted plan in GET /api/status `quota.provider_details`. */
export interface QuotaProviderDetail {
  id: string;
  name: string;
  /** Plain-word roles that route here: `orchestrator`, `subagents`, `background`, `small tasks`, … */
  used_by: string[];
}

/**
 * GET /api/status `account_alerts` (issue #984): a Claude account that needs the owner — a token
 * the API rejected (`needs_sign_in`) or a plan that hit its usage limit (`limited`) — with the
 * colonies waiting on it. Empty when everything is fine.
 */
export interface AccountAlert {
  /** The account's name, e.g. "default". */
  account: string;
  state: "needs_sign_in" | "limited";
  /** The failure class the mothership recorded, e.g. "auth". */
  class: string;
  /** The upstream HTTP status that classified it, e.g. 401. */
  status: number;
  /** When the alert started. */
  since: string;
  /** How many colonies are waiting on this account. */
  waiting: number;
}

/**
 * GET /api/status `github_pause` and GET /api/github/status (issue #1074): the GitHub account's
 * circuit breaker. While GitHub refuses the account, launches, publishes, merges and GitHub writes
 * wait, and a slow probe resumes them once GitHub works again. `{ paused: false }` when all is well.
 */
export interface GitHubPause {
  paused: boolean;
  cause?: "suspended" | "token_revoked" | "secondary_rate_limit";
  /** The cause in words, e.g. "GitHub account suspended". */
  message?: string;
  /** What a person does next, e.g. "contact GitHub support". */
  next_step?: string;
  since?: string;
  /** When the probe asks GitHub again (RFC3339). */
  next_probe_at?: string;
  probes?: number;
  /** What GitHub last said, truncated. */
  detail?: string;
  /** Colonies waiting in the queue. */
  queued?: number;
  /** Autopilot publishes waiting to go out. */
  held_publishes?: number;
  /** Calls refused without reaching GitHub while paused. */
  refused_calls?: number;
  /** Secondary rate limits in the last ten minutes, while not paused. */
  secondary_limits_recent?: number;
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
  thinking_per_mtok?: number;
}

/**
 * Where to read what is left in a prepaid token plan (issue #199). The probe is fetched with the
 * provider's own credential, so the URL must sit on the base URL's origin — scheme, host and port.
 */
export interface ProviderQuotaProbe {
  url: string;
  /** Non-empty RFC 6901 JSON pointer naming the remaining-token number in the answer, like `/data/remaining`. */
  pointer: string;
  /** Optional RFC 6901 pointer naming the plan's total in the same answer, so used vs. limit can be drawn. */
  limit_pointer?: string;
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
  /** null = no probe: the first sign of an exhausted plan stays the colonies failing over. */
  quota?: ProviderQuotaProbe | null;
  /**
   * Whether the operator vetted this connection to carry restricted-sensitivity work — secrets,
   * `.env` files, infra config (issue #472). Defaults to false: a connection is not trusted with a
   * colony's secrets just because it is configured.
   */
  trusted: boolean;
  /** Canonical model id → the name sent on the wire (issue #295). Empty serves any canonical as-is. */
  model_map?: Record<string, string>;
  /** Claude Code tool names the gateway strips from every request through this connection (issue #295). */
  disabled_tools?: string[];
  /** Live counts across all colonies. */
  in_flight: number;
  queued: number;
  /**
   * Cumulative tallies since the Mothership first kept them; survive a restart. Optional: a
   * Mothership from before it kept tally sends neither this nor `used_by`.
   */
  usage?: ProviderUsage;
  /** The Mothership's read on `usage`. Optional: a Mothership from before it computed health sends neither this nor `usage`. */
  health?: ProviderUsageHealth;
  /** The Mothership's quota record for this provider (issue #225); absent when the plan is not exhausted. */
  quota_exhausted?: ProviderQuotaState | null;
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
  /** When the tally for this provider started; null when the Mothership doesn't say. */
  since: string | null;
}

/**
 * What the Mothership reads out of a provider's `usage`: the failure rate and average latency over
 * that tally, and whether it rates the provider degraded. The rule lives on the Mothership, so the
 * UI renders these as given instead of recomputing them from the raw counts.
 */
export interface ProviderUsageHealth {
  /** `failures / requests * 100`, one decimal; 0.0 with no requests. */
  failure_pct: number;
  /** Mean duration of dispatched requests, time queued excluded; 0 with no requests. */
  avg_latency_ms: number;
  /** Enough data to judge: `requests >= 50`. */
  rated: boolean;
  /** `rated` and at least 10% of requests failed. */
  degraded: boolean;
  /**
   * The typed failure code of the provider's most recent failed request (issue #302: the gateway's
   * per-request audit). Optional: a Mothership from before it kept it sends nothing here.
   */
  last_failure?: string | null;
}

/** A provider's quota record (issue #225): when its plan refills, as words and as a timestamp. */
export interface ProviderQuotaState {
  reset_at: string | null;
  reset_unix: number | null;
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
  /** Omitted keeps the saved probe; an empty `url` removes it. The credential is sent to this URL. */
  quota?: ProviderQuotaProbe;
  /** For each limit, null uses the default. */
  timeout_secs?: number | null;
  max_concurrent?: number | null;
  queue_timeout_secs?: number | null;
  context_tokens?: number | null;
  fallback_model?: string | null;
  /** Whether the connection may carry restricted-sensitivity work (issue #472). Omitted keeps the saved mark. */
  trusted?: boolean;
  /** Canonical model id → wire name (issue #295); omitted keeps the saved map, `{}` clears it. */
  model_map?: Record<string, string>;
  /** Claude Code tools stripped through this connection (issue #295); omitted keeps the list, `[]` clears it. */
  disabled_tools?: string[];
}

export interface ProviderHealth {
  reachable: boolean;
  /** HTTP status of the probe, when a response arrived. */
  status: number | null;
  latency_ms: number | null;
  models: string[];
  error: string | null;
  /** Set when the probe answered in a way that is still healthy — an anthropic-wire endpoint that serves no /v1/models ("no model list"). */
  note: string | null;
  /** The plan balance the provider's quota probe read; absent when the provider has no probe configured. */
  quota?: { remaining: number | null; error: string | null } | null;
  checked_at: string;
}

/** `POST /api/providers/{id}/test`: a one-token request through the colony's own route (issue #1018). */
export interface ProviderTestResult {
  ok: boolean;
  /** The URL the request hit, redacted (no userinfo, no query); null when none was sent. */
  url: string | null;
  /** The upstream's HTTP status; null when no response arrived. */
  status: number | null;
  /** The model it tested with: the provider's first listed one. */
  model: string | null;
  latency_ms: number | null;
  error: string | null;
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

/** Where a downloadable skillset is: `local` means the operator's own directory holds its name. */
export type DownloadState = "idle" | "installed" | "downloading" | "unpacking" | "failed" | "unavailable" | "local";

/** GET /api/plugins/graft, and one entry of GET /api/plugins `downloadable`: a skillset the mothership downloads on request. */
export interface DownloadableSkillset {
  name: string;
  /** The pinned bundle release; null when this build pins none for this machine's architecture. */
  release: string | null;
  /** The release on disk when it differs from the pinned one. */
  installed_release: string | null;
  state: DownloadState;
  bytes: number;
  total: number | null;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** GET /api/plugins */
export interface PluginListing {
  /** Where an operator puts their own plugin directories. */
  local_root: string;
  plugins: PluginDir[];
  /** Skillsets that are downloaded on request; absent on a mothership that offers none. */
  downloadable?: DownloadableSkillset[];
}
