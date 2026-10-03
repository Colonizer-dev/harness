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
  /** Out of tokens (provider quota, or the autopilot hold timed out): stopped but resumable (issue #213). */
  | "parked"
  | "stopped"
  | "failed";

export type AttentionReason = "stalled" | "waiting_for_answer" | "nudges_exhausted" | "autopilot_held" | "provider_quota_exhausted" | "hold_timeout" | "model_error";

/** Set by the watchdog or autopilot (§6.3); cleared by the next agent event. */
export interface Attention {
  reason: AttentionReason;
  since: string;
  nudges: number;
  /** Why, in the mothership's own words — the failing checks and where their output went for `autopilot_held` (issue #672). Absent otherwise and on older motherships. */
  detail?: string;
  /** `provider_quota_exhausted` (issue #767): the provider the colony is blocked or parked on. */
  provider?: string;
  /** `provider_quota_exhausted` parked from its card: `"wait"`, with the scheduled resume. */
  action?: "wait";
  resume_unix?: number | null;
  reset_at?: string | null;
}

/** A model the "Provider out of quota" card offers to switch to, with its provider's health (issue #767). */
export interface QuotaAlternative {
  /** A Claude alias/id, or `<provider>/<model>`. */
  id: string;
  label: string;
  /** `anthropic` for Claude's own models. */
  provider: string;
  /** The provider's wire; null for Claude's own models, which any provider can fall back to. */
  wire?: "anthropic" | "openai" | null;
  failure_pct: number;
  rated: boolean;
  degraded: boolean;
  healthy: boolean;
}

/** One colony on a "Provider out of quota" card. */
export interface QuotaCardColony {
  id: string;
  repo: string;
  org: string;
  issue: number | null;
  issue_title: string;
  status: SessionStatus;
  /** Quota answers in a row since its last success; null for a colony only parked on the provider. */
  hits: number | null;
  /** Parked on the provider (by the card's Wait, or on its own). */
  waiting: boolean;
  /** When a Wait scheduled it back; null otherwise. */
  resume_unix: number | null;
}

/** GET /api/attention `quota_cards` (issue #767): one card per provider that ran out of quota. */
export interface QuotaCard {
  provider: string;
  provider_name: string;
  /** The provider's models the colonies run, most used first. */
  models: string[];
  /** e.g. "bailian · qwen3.8-max is out of quota". */
  title: string;
  reset_at: string | null;
  reset_unix: number | null;
  colonies: QuotaCardColony[];
  orgs: string[];
  /** How many of the colonies wait for the reset, and the earliest scheduled resume. */
  waiting: number;
  resume_unix: number | null;
  fallback_model: string | null;
  /** The provider's wire: a remembered fallback on another provider must speak the same one. */
  wire?: "anthropic" | "openai";
  alternatives: QuotaAlternative[];
}

/** POST /api/providers/{id}/quota-action. */
export interface QuotaActionRequest {
  action: "switch" | "wait" | "stop";
  model?: string;
  /** `colonies` (default), `org` (their orgs' model settings too) or `all` (every model role on the
   *  provider install-wide: the agent module's settings and every org's overrides, plus the colonies). */
  scope?: "colonies" | "org" | "all";
  colonies?: string[];
  org?: string;
  /** Save the model as the provider's `fallback_model`: a Claude model, or one on a provider of the same wire. */
  remember?: boolean;
}

/** One setting a quota switch changed, with the value it replaced. */
export interface QuotaChange {
  /** `install` (target: the agent module), `org`, `colony` or `provider` (the remembered fallback). */
  scope: "install" | "org" | "colony" | "provider";
  target: string;
  key: string;
  was: string | null;
  now: string;
}

export interface QuotaActionReply {
  action: string;
  provider: string;
  colonies: string[];
  failed: { id: string; ok: false; error: string }[];
  /** What a switch changed and what it replaced; older builds omit it. */
  changes?: QuotaChange[];
}

/** One line of a colony's recent event history — GET /api/sessions/{id} only (issue #230). */
export interface RecentEvent {
  seq: number;
  ts: string | null;
  type: string;
  summary: string;
}

export type DiagnosisState = "queued" | "booting" | "working" | "waiting_on_human" | "waiting_on_provider" | "stuck";

/** Why this colony is not progressing — GET /api/sessions/{id} only, non-terminal colonies (issue #230). */
export interface Diagnosis {
  state: DiagnosisState;
  text: string;
  resets_at?: string;
}

/** GET /api/status `stall` (issue #230): the queue-wide idle readout; null when nothing is stalled. */
export interface StallInfo {
  idle_secs: number;
  last_event_at: string | null;
  live: number;
  queued: number;
}

export type CiState = "success" | "failure" | "pending" | "no_checks";

export interface Session {
  id: string;
  repo: string;
  /** Repository owner; older mothership builds omit it, see `orgOf`. */
  org?: string;
  /** null for an open session started on a repository without an issue. */
  issue: number | null;
  issue_title: string;
  /** The task in one plain sentence, written by a cheap model (summaries.rs); absent until written. */
  summary?: string | null;
  status: SessionStatus;
  branch: string;
  base: string | null;
  /** The colony this one is stacked on: it branched from that colony's branch instead of the default one, which is what `base` then holds. null for an unstacked colony — absent in live data, since the backend omits the field when there is no parent. */
  parent?: string | null;
  /** The colony this queued one waits for; null when it waits for a parallelism slot instead. Absent in older payloads, which read as a generic queued entry. */
  queued_behind?: string | null;
  /** True while this colony waits in its issue's successor queue: it starts when the holder releases the issue (`queued_behind` names the holder). Absent in older payloads. */
  claim_wait?: boolean;
  /** True when the colony branch has diverged from origin/{base} and needs a rebase. Absent in older payloads. */
  needs_rebase?: boolean;
  /** What launched the colony, when it was not a person: `burn_down` for bug-hunt colonies the burn-down scheduler auto-launched near the token-plan reset (issue #210). Absent otherwise. */
  origin?: string | null;
  worktree: string;
  /** Path of the worktree's git admin dir on the host; null until the worktree was created. */
  git_admin_dir: string | null;
  sandbox: string;
  mesh: { name: string; ip: string | null } | null;
  agent: string;
  autopilot: boolean;
  /** Whether a filed finding from this colony spawns a fix colony; absent until the operator answers, when the publish module's `autofix` setting decides (§6.6). */
  autofix?: boolean;
  /** Whether a fix colony's review-passing pull request merges itself; absent until the operator answers, when the publish module's `automerge` setting decides (§6.6). */
  automerge?: boolean;
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
  /** The microVM this colony boots: vCPUs and memory as the sandbox sized them. Absent on a colony booted before this change. */
  boot_cpus?: number | null;
  /** `8G`-shaped, like the sandbox's `memory` setting. Absent on a colony booted before this change. */
  boot_memory?: string | null;
  /**
   * Where the last launch's time went (docs/protocol.md §4): phases back to back in boot order, their
   * sum at most `total_ms`. While `starting` it holds the phases finished so far; a failed boot keeps
   * the ones it got through.
   */
  boot_timing?: {
    /** Present only once the boot finished: a boot under way or stopped part way has no total. */
    total_ms?: number;
    phases: { name: string; ms: number }[];
  } | null;
  cleaned_up: boolean;
  /** True opts this colony's worktree out of automatic reclamation (issue #223). */
  keep_worktree: boolean;
  created_at: string;
  /** When the PR merged (GitHub mergedAt, or when the mothership saw the flip); omitted when absent. */
  merged_at?: string | null;
  /** When the pull request was opened (GitHub's createdAt); absent until the PR watcher reads it. */
  pr_opened_at?: string | null;
  /** The pull request's checks in one word, as last read by the PR watcher. */
  ci_state?: CiState | null;
  /** The files the colony's pull request changed (first 500); absent until read from GitHub. */
  changed_paths?: string[];
  updated_at: string;
  last_activity_at?: string | null;
  attention?: Attention | null;
  /**
   * True while the colony is `failed` and nobody has opened it since (issue #744): the badge and
   * the mothership's attention count include it until POST /api/sessions/{id}/seen marks it looked
   * at, which also pushes "resolved" to every other device. Older mothership builds omit the field.
   */
  unseen_failure?: boolean;
  /**
   * Set while the colony is paused with its question outstanding (issue #562): the microVM is
   * stopped and it holds no parallelism slot, but `status` stays `waiting_for_answer` and the
   * question stays answerable exactly as before. The answer re-boots the colony with priority;
   * older mothership builds omit the field.
   */
  suspended?: { at: string; snapshot: string | null; reason: string; path: string } | null;
  /**
   * Set while the colony is parked for tokens (issue #213): the provider's quota ran out, or the
   * autopilot hold timed out, so the mothership stopped the microVM and freed its parallel slot
   * until tokens return. `status` reads `parked`, the worktree is kept, and Resume
   * (POST /api/sessions/{id}/resume) brings the colony back. `resets_at` is the provider's own
   * reset time when it named one. Older mothership builds omit the whole field.
   */
  parked?: { at: string; reason: string; resets_at?: string; vm_kept: boolean } | null;
  /**
   * The answer held between the user sending it and the colony's re-boot (issue #562), present only
   * while one is stored: which question it answers, the prompt as asked, and — once the queue has
   * it — when it was answered (RFC3339), the restore-order key. Older mothership builds omit the
   * whole field.
   */
  pending_answer?: { question_id: string; prompt: string; answered_at?: string } | null;
  /**
   * A warm-up of this suspended colony's question is under way (issue #701): the mothership is
   * booting it ahead of your answer. `suspended` stays set while warming, so an answer still holds
   * as for any suspended colony; all three fields clear once the answer is delivered. Older
   * mothership builds omit the whole field.
   */
  prewarm?: { requested_at: string; started_at?: string | null; ready_at?: string | null } | null;
  /** The supply-chain target this colony was launched to fix (issue #673); absent for a colony launched against none. */
  supply_chain?: { package: string; advisory: string } | null;
  /**
   * Set when a same-repository colony's pull request merged over this one's work (issue #673).
   * While it stands unkept the queue and the resume route hold the colony; absent for a colony no
   * merge covered, and `pr` is omitted when the merged pull request's URL carries no number.
   */
  superseded?: {
    by: string;
    pr_url: string;
    pr?: number;
    title: string;
    reason: "supply_chain" | "issue" | "files";
    at: string;
    kept: boolean;
  } | null;
  /** Why the colony is not progressing — single-session GET only (issue #230). */
  diagnosis?: Diagnosis | null;
  /** Last ≤20 events, oldest first — single-session GET only (issue #230). */
  recent_events?: RecentEvent[] | null;
}

/** GET /api/burn-down state: where the weekly-token-plan scheduler's burn-down is (issue #210). */
export type BurnDownState =
  | "disabled"
  | "unconfigured"
  | "unknown_allowance"
  | "outside_window"
  | "burning"
  | "at_reserve";

/**
 * GET /api/burn-down: the burn-down scheduler's read on the weekly token plan — how long until the
 * reset, and whether the estimated allowance has been spent down to the reserve. `now` is the
 * backend's clock, so the frontend can measure drift between its own time and the scheduler's.
 */
export interface BurnDownStatus {
  /** The scheduler is switched on and configured. */
  enabled: boolean;
  state: BurnDownState;
  /** Always true — the allowance is a measured-window estimate, never a real plan limit. */
  estimate: boolean;
  /** RFC3339, the backend's read of when it answered. */
  now: string;
  next_reset: string | null;
  window_start: string | null;
  spent_usd: number;
  /** null = the operator has not set an estimate. */
  allowance_usd: number | null;
  remaining_usd: number | null;
  reserve_usd: number | null;
  colonies: { live: number; queued: number; total: number };
  launches_needed: number | null;
  launches_done: number;
}

export interface ModelTokens {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  thinking_tokens: number;
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
  /** Set by the mothership's issue list when the issue is an epic (sub-issues, an `epic` label, or a title marking it): why, and how many sub-issues. A launch on it answers 409 without `allow_epic`. */
  epic?: { reason: string; sub_issues: number } | null;
}

/** One issue Colonize drafted from free text, shown for a confirm or an edit before it is filed. */
export interface IssueDraft {
  title: string;
  body: string;
}

/** POST /api/colonize/draft: `model` is null (and `note` says why) when the text came back as its own draft. */
export interface IssueDrafts {
  issues: IssueDraft[];
  model: string | null;
  note?: string;
  /** The Source module's include labels, which filing adds so the filtered list still offers the issue. Absent from older motherships. */
  labels?: string[];
}

/** POST /api/repos/{owner}/{repo}/issues: the issue just filed; `number` is null if gh's answer named none. */
export interface CreatedIssue {
  repo: string;
  number: number | null;
  title: string;
  url: string;
  /** The Source labels the issue carries. Absent from older motherships. */
  labels?: string[];
  /** Source labels it could not be given (missing on the repository and not creatable); the issue is filed anyway. */
  labels_skipped?: string[];
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
  /** `{ ok: true }` alone until there is a storage alert: a failed disk write, which can recover, or colony records lost at startup, which cannot (see `StorageHealth.kind`). `ok` is the current write verdict, not a latch: a failure sets it false and the next write through sets it true again with `recovered_at`. The disk-space readings below ride every poll regardless (issue #220); older mothership builds omit the whole object. */
  storage?: StorageHealth;
  /** Aggregate reclamation counts from the same poll (issue #223); older mothership builds omit it. */
  reclaim?: { reclaimable: number; unpushed: number };
  /** The machine facts a colony's first minute depends on (issue #129); older mothership builds omit it. */
  runtime?: RuntimeInfo;
  /** The machine every colony in the overview boots on (issue #205); older mothership builds omit it. */
  host?: HostInfo | null;
  /** One entry per configured model provider, so the status poll can answer "is it the provider?" without the providers screen; older mothership builds omit it. */
  model_providers?: ModelProviderStatus[];
  /** Quota exhaustion across providers (issue #225); older mothership builds omit it. */
  quota?: StatusQuota | null;
  /** "Provider out of quota" cards (issue #767), the same list GET /api/attention serves; older builds omit it. */
  quota_cards?: QuotaCard[];
  /** Queue-wide stall readout (issue #230); null when nothing is stalled, omitted by older builds. */
  stall?: StallInfo | null;
  /** The shared anti-spam ledger's tallies (issue #311): what notify and the autonomous judge delivered, held for the digest, or dropped, by class, with the limits in force. Counts by class only — no colony ids. Older mothership builds omit it. */
  ledger?: LedgerStatus | null;
}

/** GET /api/status `ledger` (issue #311): the anti-spam ledger's running tallies. */
export interface LedgerStatus {
  /** Per class (e.g. `question`, `provider_degraded`, `judge`): how many candidates were delivered, held for the digest, or dropped. */
  counters: Record<string, { delivered: number; digested: number; dropped: number }>;
  /** Candidates held since the last digest line went out, by class. */
  pending_digest: Record<string, number>;
  /** When the last digest line was delivered; null until the first one. */
  last_digest: string | null;
  /** 1 when a corrupt `ledger.json` was quarantined aside at startup, else 0. */
  quarantined: number;
  limits: { notify: LedgerLimits; judge: LedgerLimits };
}

/** The rules one ledger claimant lives under; see `LedgerStatus`. */
export interface LedgerLimits {
  quiet_hours: { start: number; end: number } | null;
  per_hour: number;
  per_day: number;
  topic_cooldown_minutes: number;
  dedup_window_hours: number;
  topic_daily_cap: number;
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
  /**
   * Which operating system the mothership runs on, as the host distro tooling reports it. Additive
   * display info only — `platform` stays the supported/unsupported gate. Older mothership builds
   * omit the whole field.
   */
  os?: OsInfo;
}

/**
 * GET /api/status `runtime.os` (issue #208): which operating system the mothership runs on.
 * Additive display info only — `platform` remains the supported/unsupported gate. Older mothership
 * builds omit the whole block.
 */
export interface OsInfo {
  /** The family key, one of: ubuntu, debian, fedora, rhel, centos, rocky, almalinux, arch, omarchy, manjaro, endeavouros, nixos, alpine, opensuse, linux, apple, unknown. */
  vendor: string;
  /** Display name, e.g. "Ubuntu", "macOS". */
  name: string;
  /** e.g. "24.04", "14.5"; null when there is no version to report. */
  version: string | null;
  /** Raw os-release ID, Linux only; null off Linux and where the probe found none. */
  id: string | null;
}

/**
 * GET /api/status `host` (issue #205): the machine every colony boots on, re-probed on each status
 * poll. Every measurable is optional and omitted — never null, never zero-filled — when the host
 * cannot read it, so the overview never draws a number the mothership did not measure.
 */
export interface HostInfo {
  /** Stable per-install host id (uuid). It keys the host: a second machine can be summed into the
   * overview later instead of being mistaken for this one (issue #205). */
  id: string;
  /** Omitted when unmeasurable. */
  hostname?: string;
  cpu_cores?: number;
  memory_total_bytes?: number;
  memory_used_bytes?: number;
  /** 1, 5 and 15 minute load averages; the overview shows the first. */
  load?: [number, number, number];
  uptime_secs?: number;
  disk_total_bytes?: number;
  disk_used_bytes?: number;
  disk_free_bytes?: number;
  /** When the probe ran, RFC3339; always present. */
  checked_at: string;
  /** Always present. */
  microvms_live: number;
  /** Always present. */
  microvms_ceiling: number;
  /** Whether this host can boot a microVM at all; omitted on non-Linux. false means colonies cannot start here, which reads as idle rather than broken. */
  kvm_ok?: boolean;
}

/** Whether a fleet host answered the mothership's live poll (issue #231). */
export type FleetHostHealth = "online" | "unreachable";

/**
 * GET /api/hosts (issue #231): every host the mothership knows about — itself, always first and
 * always `online`, plus each peer configured via `COLONIZER_FLEET_PEERS`, polled live on every
 * request. An unreachable peer never disappears from the list: it keeps its last-known cached
 * stats (with `health: "unreachable"`) once it has answered before, or comes back as a bare
 * placeholder — `id`/`name` are its configured URL, `platform`/`os` are `""`, everything else is
 * `null`/`0` — if it has never been reached at all.
 */
export interface FleetHost {
  id: string;
  name: string;
  platform: string;
  os: string;
  /** Absent on an older peer build, or a peer never reached. */
  version: string | null;
  slots_in_use: number;
  slots_ceiling: number;
  queue_depth: number;
  /** Absent when the peer has never reported it. */
  disk_free_bytes: number | null;
  /** The disk's size (issue #764); absent when unknown. */
  disk_total_bytes?: number;
  /** RFC3339; null when the peer has never answered. */
  last_heartbeat: string | null;
  health: FleetHostHealth;
}

// ---------------------------------------------------------------------------
// Fleet pairing (issue #686): GET/POST /api/fleet… (docs/fleet.md). A mothership joins another's
// fleet like phone pairing: the owner mints a single-use invite, the joiner redeems it, both
// screens show the same six-digit confirmation code, and the owner approves what they see.
// ---------------------------------------------------------------------------

/** GET /api/fleet `role`: where this mothership stands — a fleet owner with members, a member of someone else's fleet, or in neither. */
export type FleetRole = "owner" | "member" | "none";

/** One open invite. The code itself is shown once at creation (POST /api/fleet/invites) and stored only as a hash. */
export interface FleetInvite {
  id: string;
  /** RFC3339-ish; an invite lives 15 minutes. */
  expires_at: string;
}

/** A machine that redeemed an invite and now waits for the owner's decision. */
export interface FleetPending {
  id: string;
  /** The name the joiner gave itself. */
  name: string;
  /** The joiner's own URL, when it gave one; null when it did not. */
  url: string | null;
  /** The six digits both screens must show, "123 456". */
  confirm_code: string;
  expires_at: string;
  status: "pending" | "approved" | "rejected";
}

/** GET /api/fleet member `health.state` (issue #764): the worst of what the owner observed; `unknown` until a poll has checked the member. */
export type FleetMemberHealthState = "ok" | "unknown" | "degraded" | "stopped";

/**
 * GET /api/fleet member `health` (issue #764): one state, and when it is not `ok`, why and what to do.
 * `code` is a stable key (`token_revoked`, `no_heartbeat`, `unreachable`, `disk_full`, `unwatched`, `not_checked`, …);
 * `reason` and `hint` are for showing verbatim, e.g. "No heartbeat for 12 min" / "the machine may be asleep".
 */
export interface FleetMemberHealth {
  state: FleetMemberHealthState;
  code: string | null;
  reason: string | null;
  hint: string | null;
  /** Something worth knowing that is not a fault, whatever the state: "History sync off". Absent from an older owner. */
  note?: string | null;
}

/** A mothership that joined this one's fleet; it hosts colonies and sees the fleet view. */
export interface FleetMember {
  id: string;
  name: string;
  url: string | null;
  joined_at: string;
  /** Absent from owners built before issue #764. */
  health?: FleetMemberHealth;
}

/** GET /api/fleet `membership`: this mothership's place in the fleet it joined. */
export interface FleetMembership {
  owner_url: string;
  member_id: string;
  joined_at: string;
  /** Whether this machine's operator consented to pushing its history to the owner (issue #762). Off at every join. */
  history_sync: boolean;
}

/** GET /api/fleet/sync/preview (issue #762): what turning the history push on would send — read from the same collection the push sends. */
export interface FleetSyncPreview {
  owner_url: string;
  /** Finished colonies, one row each. */
  colonies: number;
  /** Their log files, and those files' bytes. */
  payloads: number;
  payload_bytes: number;
  /** Logs over the size limit: named on their row, never sent. */
  omitted_payloads: number;
  row_bytes: number;
  total_bytes: number;
  /** What the owner has not acknowledged yet. */
  pending_colonies: number;
  pending_bytes: number;
  includes: string;
  excludes: string;
}

/** GET /api/fleet/sync's `status`: where the history push stands. */
export type FleetSyncState = "idle" | "synced" | "backoff" | "unauthorized" | "removed" | "error" | "consent_required";

/** GET /api/fleet/sync, and POST /api/fleet/sync/consent's answer. */
export interface FleetSyncStatus {
  member: boolean;
  consent: boolean;
  enabled: boolean;
  status: FleetSyncState;
  detail: string | null;
  acknowledged: number;
  retired: { id: string; error: string; attempts: number; at: string }[];
  last_drain_at: string | null;
  last_synced_at: string | null;
  next_attempt_at: string | null;
}

// Fleet history (issue #762, the owner's view): what members pushed with history sync on, read
// back from <data_dir>/fleet-ingest/ by GET /api/fleet/history… (docs/fleet.md). Owner-only.

/** A synced colony's record: the allowlist projection the member sent (`ImportedSession`). */
export interface FleetHistoryRecord {
  /** `<origin_host>:<original_id>`. */
  id: string;
  origin_host: string;
  original_id: string;
  repo: string;
  org: string;
  issue: number | null;
  issue_title: string;
  status: SessionStatus;
  branch: string;
  base?: string | null;
  pr_url: string | null;
  pr_opened_at?: string | null;
  merged_at: string | null;
  summary: string | null;
  error: string | null;
  cost_usd: number | null;
  model_tier?: string | null;
  agent: string;
  created_at: string;
  updated_at: string;
}

/** One log a synced colony carries; `omitted` was too large to travel. */
export interface FleetHistoryPayload {
  name: string;
  sha256: string;
  bytes: number;
  omitted?: boolean;
}

/** One synced colony in GET /api/fleet/history. `key` (`<member_id>/<row id>`) is its cursor. */
export interface FleetHistoryEntry {
  key: string;
  member_id: string;
  member_name: string;
  /** The member was removed from the fleet; its history stays. */
  member_removed: boolean;
  id: string;
  received_at: string;
  record: FleetHistoryRecord;
  payloads: FleetHistoryPayload[];
}

/** Totals over the filtered history; `cost_usd` is null when no row carried a cost. */
export interface FleetHistoryTotals {
  colonies: number;
  merged: number;
  cost_usd: number | null;
}

/** GET /api/fleet/history's filters and page; every field optional. Dates are YYYY-MM-DD or RFC 3339, on the colony's finish. */
export interface FleetHistoryQuery {
  member?: string;
  repo?: string;
  status?: string;
  since?: string;
  until?: string;
  limit?: number;
  cursor?: string;
}

/** GET /api/fleet/history: one page, newest finish first, the totals over every filtered row, and the filter options. */
export interface FleetHistoryPage {
  colonies: FleetHistoryEntry[];
  next_cursor: string | null;
  stats: {
    total: FleetHistoryTotals;
    members: (FleetHistoryTotals & { member_id: string; name: string; removed: boolean })[];
    repos: (FleetHistoryTotals & { repo: string })[];
  };
  members: { id: string; name: string; removed: boolean }[];
  repos: string[];
  retention_days: number;
}

/** GET /api/fleet/history/{member}/{row_id}: the entry, and each log with whether the owner holds it. */
export interface FleetHistoryDetail extends FleetHistoryEntry {
  logs: (FleetHistoryPayload & { omitted: boolean; stored: boolean })[];
}

/** GET /api/fleet `joining`: a join this mothership started and has not finished; both screens show `confirm_code` until the owner decides. */
export interface FleetJoining {
  owner_url: string;
  confirm_code: string;
  started_at: string;
}

/** GET /api/fleet: everything the Fleet settings pane renders, in one view. */
export interface FleetState {
  role: FleetRole;
  invites: FleetInvite[];
  pending: FleetPending[];
  members: FleetMember[];
  membership: FleetMembership | null;
  joining: FleetJoining | null;
}

/** POST /api/fleet/invites' answer: the invite code, shown exactly once — the registry keeps only its hash. */
export interface CreatedFleetInvite {
  id: string;
  code: string;
  expires_at: string;
}

/** POST /api/fleet/join: what the join form collects. `name` and `url` are optional labels for the owner's member list. */
export interface FleetJoinRequest {
  owner_url: string;
  code: string;
  name?: string;
  url?: string;
}

/** POST /api/fleet/join/confirm's answer: `pending` until the owner decides, then `joined` — or the decision, bad news both other ways. */
export type FleetJoinStatus = "joined" | "pending" | "rejected" | "expired";

/** GET /api/status `storage`: whether the mothership can still write its own files (sessions.json, colony event logs). */
export interface StorageHealth {
  /** False while writes are failing; true when every write was confirmed, or once one succeeds after a failure (then `recovered_at` is set). Always true for load damage. */
  ok: boolean;
  /**
   * Which alert this is (issue #371). `write`: a disk write failed; it recovers once one goes through.
   * `load_damage`: sessions.json was unreadable or partly damaged at startup, so colony records were
   * lost; `ok` only says writes work, it never recovers, and `message` names the `.corrupt-` copy.
   * Older motherships omit it: read a missing kind as `write`.
   */
  kind?: "write" | "load_damage" | null;
  /** The underlying write error (for load damage: what was lost and where the original went), for showing verbatim. Kept after a recovery: the gap it reports still happened. */
  message?: string | null;
  /** When the latest failure was recorded (for load damage: when startup found it); same representation as a harness_log `ts`. */
  ts?: string | null;
  /** Failed writes since the mothership started; a recovery does not reset it. A load_damage alert always carries 1, which is not a write count. */
  failures?: number | null;
  /** When a write first succeeded after the latest failure; null while writes are still failing. Absent from older motherships, whose alert stays until a restart. */
  recovered_at?: string | null;
  /** Free bytes on the data dir's volume at the queue's last check (issue #220); null when there is no reading yet or df failed. Absent on older motherships. */
  free_bytes?: number | null;
  /** Warn threshold in free bytes; 0 means the warning is off. Absent on older motherships. */
  warn_free_bytes?: number;
  /** Floor in free bytes; 0 means the floor is off. Below it the queue stops starting new colonies (`admission_paused`). Absent on older motherships. */
  min_free_bytes?: number;
  /** Free space is below the warn threshold (or the floor). Absent on older motherships. */
  low_disk?: boolean;
  /**
   * Free space is below the floor: the queue is not starting new colonies. Running colonies keep
   * running and admission resumes on its own when space returns; the pause itself deletes nothing.
   * Below the floor the reclaim sweep still reclaims finished colonies whose work is already pushed;
   * unpushed work is never deleted. Absent on older motherships.
   */
  admission_paused?: boolean;
}

/** GET /api/storage: disk usage and what automatic reclamation can (and pointedly will not) take (issues #223, #220). */
export interface StorageSummary {
  enabled: boolean;
  retention_secs: number;
  min_free_bytes: number;
  warn_free_bytes: number;
  /** Free bytes on the data dir's volume; null when there is no reading yet or df failed. */
  free_bytes: number | null;
  /** Free space is below the floor: the queue is not starting new colonies (running ones keep running). */
  admission_paused: boolean;
  /** Data-dir usage by category. `archive_bytes` is the log archive under `<data_dir>/archive`; `microsandbox_bytes` is microsandbox's whole home directory (holding the shared OCI image cache) — informational, never offered for cleanup; null when unmeasured. */
  totals: { worktrees_bytes: number; repos_bytes: number; sessions_bytes: number; archive_bytes: number; microsandbox_bytes: number | null };
  /** Finished colonies whose work is pushed (a PR, or no_changes) and not yet cleaned up; `due` means past the auto-reclaim retention window. */
  reclaimable: Array<{ id: string; status: SessionStatus; pr_url: string | null; bytes: number; updated_at: string; due: boolean }>;
  /** Terminal colonies with no PR: listed for a person, never auto-deleted. */
  unpushed: Array<{ id: string; status: SessionStatus; bytes: number; updated_at: string }>;
  /** Worktree directories with no colony behind them, and what the sweep will do. */
  orphans: Array<{ path: string; bytes: number; action: string }>;
}

/**
 * One archived colony's logs under `<data_dir>/archive` (issue #496). The backend's sidecar record
 * carries every key on every entry — absent reads as `null`, never a missing field — and sends
 * several more (`org`, `pr_url`, `cost_usd`, `model_usage`, `model_tier`, `agent`, `created_at`,
 * `updated_at`, `mothership`, `fingerprint`) that nothing here reads.
 */
export interface ArchiveEntry {
  session: string;
  repo: string;
  /** null for a colony started on a repository without an issue. */
  issue: number | null;
  title: string;
  status: string;
  bundle: string;
  bytes: number;
  archived_at: string;
  revision: number;
}

/** GET /api/archive (issue #496): the whole log archive, bundles included. */
export interface ArchiveListing {
  root: string;
  count: number;
  bytes: number;
  entries: ArchiveEntry[];
}

/** POST /api/archive/retention (issue #496): preview a cleanup pass (`dry_run: true`) or apply one. */
export interface RetentionRequest {
  keep_days: number | null;
  max_gb: number | null;
  /** Bundles that are the only copy are never removed unless this is set. */
  allow_single_copy: boolean;
  dry_run: boolean;
  /** On apply, the previewed bundle list; the server answers 409 when the archive no longer matches. */
  expect: string[] | null;
}

/** POST /api/archive/retention's answer (issue #496): what the pass takes, or would take. */
export interface RetentionPlan {
  dry_run: boolean;
  remove: Array<{ bundle: string; session: string; bytes: number; archived_at: string }>;
  count: number;
  bytes: number;
  /** Bundles held back because they are the only copy and `allow_single_copy` was false. */
  kept_single_copy: number;
}

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
}

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
    backup: string | null;
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

/** The anonymous usage batch, exactly what a sender transmits: Cratefield's module-telemetry payload (Cratefield/harness#413), the grammar the collector on the other side parses. Built whatever the switch says. */
export interface UsageBatch {
  schema: number;
  /** 32 lowercase hex — the per-on-period usage id without its dashes. All zeros while reporting is off or held off by the environment: a batch the sender refuses to post. */
  install: string;
  client: { kind: string; version: string; platform: string; arch: string };
  /** The declared modules this batch speaks for: the mothership, and nothing else it composes. */
  modules: string[];
  /**
   * One event per observation; each name's `.`-separated parts come from a closed vocabulary, so the
   * label a field used to carry rides in the name (`colonies.parallel_now.2-3`, `boot.vm-boot.5-15s`,
   * `setting.agent.model`). Outcome, error class, duration and count stay at their neutral values: a
   * usage batch is a set of observations, not runs. docs/usage-data.md maps field by field.
   */
  events: { name: string; outcome: "ok" | "error" | "cancelled"; error: string; duration: string; count: number }[];
}

/** GET /api/telemetry/usage, and of a successful PUT. The batch is built whatever the switch says, so it can be read in full — and it is the same value the sender posts, at most once a day when an endpoint is named. */
export interface UsageStatus {
  /** Already resolved: true when reporting is on — including when nobody has answered, since it is on by default — false once declined or held off by the environment. */
  enabled: boolean;
  /** An environment variable keeping it off whatever Settings says (COLONIZER_TELEMETRY, DO_NOT_TRACK or CI). */
  blocked_by: string | null;
  /** The payload schema the batch speaks; the same value as `batch.schema`. */
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
  /** Set when the probe answered in a way that is still healthy — an anthropic-wire endpoint that serves no /v1/models ("no model list"). */
  note: string | null;
  /** The plan balance the provider's quota probe read; absent when the provider has no probe configured. */
  quota?: { remaining: number | null; error: string | null } | null;
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

// ---------------------------------------------------------------------------
// Org workspaces (§6.3)
// ---------------------------------------------------------------------------

/** Every field is optional; missing or null inherits the global module setting. */
export interface OrgSettings {
  agent?: {
    /** Which installed agent module this org's colonies launch on; null inherits the mothership's choice. */
    module?: string | null;
    model?: string | null;
    subagent_model?: string | null;
    background_model?: string | null;
    /** Skillsets this org switches on (`true`) or off (`false`); unnamed ones follow the global switches. */
    skillsets?: Record<string, boolean> | null;
  } | null;
  max_parallel?: number | null;
  /** Live colonies one repository of this org may run at once; null inherits the global per-repository limit. */
  repo_max_parallel?: number | null;
  /**
   * Repositories of this org (full `owner/name`) whose superseded colonies' pull requests Colonizer
   * may close on GitHub when another colony's pull request merges over them (issue #673). Empty —
   * the default — only marks the colonies superseded and leaves their pull requests open.
   */
  close_superseded_prs?: string[];
  /** Dollars one colony of this org may spend on models in total; 0 opts out of the global budget. */
  budget_usd?: number | null;
  /** The most disk one colony of this org may leave on the host, like `16G`; 0 opts out of the global quota. */
  host_disk?: string | null;
  /** The sandbox stack this org's colonies boot, pinning what the global `preset` would otherwise choose; `null` inherits. */
  stack?: string | null;
  /** `deja` is the recall toggle; null or absent inherits the install setting. */
  memory?: { enabled?: boolean | null; deja?: boolean | null } | null;
  watchdog?: { enabled?: boolean | null; stall_minutes?: number | null; max_nudges?: number | null } | null;
  /**
   * Off keeps the org out of the workspace list and stops new colonies starting there; its existing
   * colonies stay listed and resumable. Absent and null mean on, like every field above.
   */
  enabled?: boolean | null;
  /**
   * Whether this org's colonies may consult Jev at any decision point (issue #582). False turns every
   * point off for the org's colonies — no network call — while absent, null or true follows the
   * module settings, point by point.
   */
  jev?: boolean | null;
}

export interface OrgInfo {
  org: string;
  colonies: { live: number; total: number };
  pending_memory: number;
  settings: OrgSettings;
  /** The org's GitHub avatar. Absent when unknown — an org that only appears in the colony list has none. */
  avatar_url?: string;
  /** The org's GitHub description. Absent when it has none, or on a mothership that does not send it. */
  description?: string;
  /**
   * True for a newly-appeared org the operator has not decided about yet; it is not a workspace
   * until then. Optional so an older mothership that never sends it simply has no pending orgs.
   */
  awaiting_decision?: boolean;
  /**
   * The org's token and dollar tallies plus its top models (issue #209). Optional: a mothership
   * from before it measured org spend sends none, and the overview falls back to the colony list.
   */
  spend?: OrgSpend;
}

/** Token tallies, per the per-turn `model_usage` but summed across the org. */
export interface SpendTokens {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
}

/** One model's share of an org's spend; the server sends them sorted by tokens descending. */
export interface ModelSpend {
  model: string;
  tokens: number;
  /** null = the model was never priced (routed through an unpriced provider). */
  cost_usd: number | null;
}

/** GET /api/orgs `spend`: the org's measured spend and what earned it. */
export interface OrgSpend {
  /** Never measured (a subscription-account org): null, and rendered as "—", not "$0.00". */
  cost_usd: number | null;
  routed_cost_usd: number | null;
  tokens: SpendTokens;
  models: ModelSpend[];
}

/** GET /api/spend/history: one org's tallies for one day. */
export interface SpendOrgDay {
  org: string;
  cost_usd: number | null;
  routed_cost_usd: number | null;
  tokens: SpendTokens;
  models: ModelSpend[];
  launched: number;
  returned: number;
}

/** GET /api/spend/history: one day across the orgs that had activity. */
export interface SpendDay {
  /** "YYYY-MM-DD" */
  day: string;
  orgs: SpendOrgDay[];
}

/** GET /api/spend/history: days ascending, only days with activity included. */
export interface SpendHistory {
  days: SpendDay[];
}

// ---------------------------------------------------------------------------
// Shared memory (§6.2–6.3)
// ---------------------------------------------------------------------------

/** `key` is "" for global, the org for `org`, and `owner/repo` for `repo`. */
export type MemoryScope = "global" | "org" | "repo";

/** `origin` is where in the colony a note came from ("orchestrator", "subagent", "background", …).
 * Older proposals omit it; treat those as "orchestrator". */
export type MemorySource = { session_id: string; repo: string; origin?: string } | { user: true };

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

/** The voice module's active speech-to-text service (GET /api/voice). Never the key. */
export interface VoiceStatus {
  /** `browser` is the browser's own recogniser; anything else is a service the Mothership calls. */
  provider: string;
  name: string;
  model: string;
  /** ISO-639-1, or empty for auto-detect. */
  language: string;
  /** The service can be used now: a key is set (or not needed) and a base URL is known. Always true for `browser`. */
  configured: boolean;
  has_key: boolean;
  /** `saved`, the env var's name, or `provider:<id>` when a model provider's key is reused. */
  source: string | null;
  key_optional: boolean;
  max_seconds: number;
  max_bytes: number;
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

/**
 * Who set a recorded line in motion (issue #312): an envelope field beside `seq`/`ts`/`agent` on every
 * line of `events.jsonl` and `harness.jsonl`, and on each stream event. Absent on lines recorded before
 * the field existed, which readers infer as before (a `watchdog-` message id, an answered question).
 * Exception: a `memory_proposal`'s `origin` names the proposer (§6.2), not the envelope — those lines
 * are never stamped (docs/agent-events.schema.json `#/$defs/origin`).
 */
export const ORIGINS = [
  "user",
  "agent",
  "subagent",
  "watchdog",
  "autonomy",
  "burn_down",
  "redteam",
  "notify",
  "system",
] as const;
export type Origin = (typeof ORIGINS)[number];

interface Sequenced {
  seq?: number;
  ts?: string;
  agent?: AgentRef;
  origin?: Origin;
}

export type AgentEventBody =
  | { type: "status"; state: AgentState; detail?: string | null }
  | { type: "user_message"; id: string; text: string }
  | { type: "assistant_text_delta"; message_id: string; block_index: number; delta: string }
  | { type: "assistant_text"; message_id: string; block_index: number; text: string }
  | { type: "thinking"; message_id: string; block_index: number; text: string }
  | { type: "tool_call"; message_id: string; tool_call_id: string; name: string; input: Record<string, unknown> }
  | { type: "tool_result"; tool_call_id: string; output: string; is_error: boolean }
  /**
   * `risk` is the question's risk class (§2 rules), which routes the autonomy judge's ceiling (§6.2b);
   * absent means workspace_write, and any value reads as above every ceiling until judged.
   */
  | {
      type: "question";
      question_id: string;
      message_id?: string;
      risk?: "read_only" | "workspace_write" | "publish_affecting" | "credential_adjacent";
      questions: Question[];
    }
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
  | { type: "log"; level: LogLevel; message: string }
  /** The model the colony's next turns use: sent at start with no `previous`, then after each `set_model` that took. */
  | { type: "model_changed"; model: string; previous: string | null }
  /**
   * A proposed shared-memory note (docs/protocol.md §6.2). Absent or null scope means repo; absent
   * tags mean none. The wire's `origin` on this one event names the proposer — "orchestrator",
   * "subagent:<name>", "background:<name>" (§3 Origins) — not the envelope's, so it is not declared
   * here: it would collide with `Sequenced`'s envelope `origin`, and the stream reads only the
   * watermark off this body.
   */
  | { type: "memory_proposal"; scope?: MemoryScope | null; title: string; content: string; tags?: string[] }
  /** A confirmed problem outside the task (§6.6), which the mothership files as a GitHub issue. */
  | { type: "finding"; title: string; body: string; evidence: string }
  /**
   * Jev compaction's per-chunk decisions for one pass (#475), shadow telemetry the harness grades
   * into its data-dir-wide `jev_ladder.jsonl`; the web types it but renders nothing.
   */
  | {
      type: "jev_ladder";
      applied: boolean;
      pre_tokens?: number;
      post_tokens?: number;
      trigger?: string;
      decisions: Array<{
        tool_call_id: string;
        tool: string;
        action: "keep" | "drop_result" | "drop_call";
        keep_call?: number;
        keep_result?: number;
      }>;
    }
  /**
   * The agent reached for a path the path policy masks or write-protects (docs/path-policy.md,
   * #647). Reporting only — the mount enforced before this ran. The harness turns it into a colony
   * log line and a History entry per distinct (access, path); the stream types it and renders
   * nothing of its own.
   */
  | { type: "path_policy"; access: "read" | "write"; policy: "masked" | "protected"; path: string; tool?: string }
  /**
   * The mothership's independent verdict on a completion claim (§6.3, Autopilot): tests re-run in a
   * fresh checkout and the git state read directly, never the agent's own account. Host-generated,
   * like the finding-chain events, so the runner-event schema does not list it.
   */
  | {
      type: "verification";
      /** `inconclusive` (issue #672): a check failed on the colony's work but fails on the merge-base too, so it is not this colony's doing — autopilot publishes anyway. */
      verdict: "confirmed" | "contradicted" | "inconclusive" | "unverifiable";
      by_declaration: boolean;
      summary: string;
      contradictions: string[];
      /** Observations that do not change the verdict, e.g. a described path missing beside ones that are there. Absent on events recorded before it existed. */
      advisories?: string[];
      /** Checks that failed on the merge-base as well, one reviewer-ready clause each (issue #672). Absent on events recorded before it existed. */
      inconclusive?: string[];
      command: string | null;
      command_source:
        | "config"
        | "packageManager"
        | "bun.lock"
        | "bun.lockb"
        | "pnpm-lock.yaml"
        | "yarn.lock"
        | "package-lock.json"
        | "npm-shrinkwrap.json"
        | "package.json"
        | "Cargo.toml"
        | "Makefile"
        | null;
      exit_code: number | null;
      tests_ms: number | null;
      commits: number;
      files_changed: string[];
      snapshot: string | null;
      ms: number;
    };

export type AgentEvent = Sequenced & AgentEventBody;

export type ServerFrame =
  | AgentEvent
  | { type: "session"; session: Session }
  | { type: "harness_log"; level: LogLevel; message: string; ts: string; origin?: Origin }
  | { type: "memory_proposed"; proposal: MemoryProposal }
  | { type: "run_epoch"; epoch: number }
  /** The backlog replay is complete; everything after it is live. */
  | { type: "replay_done"; seq: number };

export type ClientCommand =
  | { type: "user_message"; text: string }
  | { type: "answer"; question_id: string; answers: Answers; response: string | null }
  | { type: "interrupt" }
  /** Switches the model for the colony's next turns, keeping the conversation; `model_changed` confirms it. */
  | { type: "set_model"; model: string };

export interface NewSessionRequest {
  repo: string;
  /** Omit to start an open session on the repository. */
  issue?: number;
  title?: string;
  instructions?: string;
  autopilot?: boolean;
  /** Whether a filed finding from this colony spawns a fix colony; omitted uses the publish module's `autofix` setting (§6.6). */
  autofix?: boolean;
  /** Whether a fix colony's review-passing pull request merges itself; omitted uses the publish module's `automerge` setting (§6.6). */
  automerge?: boolean;
  /** Start a colony on an issue another colony already holds; the mothership answers 409 without it. */
  allow_duplicate?: boolean;
  /** The supply-chain target this colony fixes, `{package, advisory}` (issue #673): a second live colony of the same repository for one target is a 409 naming the holder unless `allow_duplicate` is set. */
  supply_chain?: { package: string; advisory: string };
  /** Start a colony on an epic anyway; the mothership answers 409 without it, listing the epic's open sub-issues. */
  allow_epic?: boolean;
  /** Queue the colony for an issue another colony already holds instead of refusing: it comes back `queued` (`claim_wait`, `queued_behind` naming the holder) and starts when the holder releases. `allow_duplicate` wins if both are set; a remote conflict still answers 409. */
  queue_behind_holder?: boolean;
  /** Stack the new colony on another's branch: the parent session's id, which becomes `parent` and whose branch becomes `base`. Launching a stack is API-only; no form picker yet. */
  after?: string;
  /** Opt in to overlap-aware queueing: queue behind a live same-repo colony that's already touching files, instead of developing against the same paths at once. Off by default. */
  serialize?: boolean;
  /** Who is launching when it is not the launch form: `chat` marks a conversation turned into a colony, `colonize` a hand-off from the Colonize pane; the activity log records both as such. */
  origin?: string;
}

/**
 * One commit a colony wrote (GET /api/sessions/{id}/commits, issue #765): where the link points now,
 * the shas it pointed at before a rebase or amend re-pointed it, and whether it was kept unmatched —
 * a squash or a rewrite left no single commit with the same patch-id, so the link was not guessed.
 */
export interface CommitLink {
  sha: string;
  previous: string[];
  orphaned: boolean;
  agent_session?: string;
  recorded_at: string;
}

/**
 * One line of a colony's finding ledger (GET /api/sessions/{id}/findings). The ledger is append-only:
 * as a finding moves validated → filed (or rejected/duplicate) → fix_colony → review → merged, a new
 * line is written and nothing is rewritten, so one finding — keyed by `title` — is the several lines
 * that mention it. The present is the last line's state; `error` and `rejected` lines carry a
 * `reason`, and `cockpit/findings.ts` folds the lines into one chain per title.
 */
export interface FindingRecord {
  session: string;
  repo?: string;
  title: string;
  state: "validated" | "rejected" | "filed" | "duplicate" | "fix_colony" | "review" | "merged" | "blocked" | "error";
  ts?: string;
  reason?: string;
  severity?: "low" | "medium" | "high" | "critical";
  issue?: string;
  duplicate_of?: string;
  fix_session?: string;
  review_session?: string;
  verdict?: "pass" | "fail";
  pr?: string;
}

/** One file a colony changed, from GET /api/sessions/{id}/diff (issue #611): its path and line counts. */
export interface SessionDiffFile {
  path: string;
  added: number;
  removed: number;
}

/**
 * Everything a colony changed against its base branch (GET /api/sessions/{id}/diff, issue #611):
 * the per-file counts, their totals, and the unified diff text, capped at 200 KiB (`truncated` says
 * when). The cockpit's pull request card reads only `files`; the CLI and MCP read the text.
 */
export interface SessionDiff {
  id: string;
  repo: string;
  base: string | null;
  files: SessionDiffFile[];
  added: number;
  removed: number;
  diff: string;
  truncated: boolean;
}

// ---------------------------------------------------------------------------
// Red-team runs (issue #212): a swarm of hunter colonies raiding one repository
// ---------------------------------------------------------------------------

/**
 * Ordered lifecycle of a red-team run (issue #212). `armed` and `waiting` are gated —
 * the run is live but not raiding until the nest empties — `running`/`draining` are
 * raiding, and `done`/`stopped` are terminal.
 */
export type RedTeamState = "armed" | "waiting" | "running" | "draining" | "done" | "stopped";

export interface RedTeamHunter {
  session_id: string;
  title: string;
  module: string;
  /** The module's pinned release; null when the module has no version of its own. */
  version: string | null;
  focus: string;
}

/**
 * The synthesis step (issue #309): once a run is `done`, the mothership launches one colony that
 * merges every hunter's findings into a single deduplicated, severity-ranked report.
 */
export type RedTeamSynthesisState = "pending" | "running" | "done" | "failed";

export interface RedTeamSynthesis {
  state: RedTeamSynthesisState;
  /** The current (newest) synthesis colony. */
  session_id: string | null;
  /** Host path of the newest successful merged report. */
  report: string | null;
  /** Why it failed. */
  reason: string | null;
  /** Earlier synthesis session ids, oldest first. */
  superseded: string[];
}

/** Which focus list and briefing a run's hunters get: the bug hunt, or the security hunt with a pre-scan. */
export type RedTeamPreset = "general" | "security";

/** A security pre-scan lead: a deterministic heuristic hit, never a confirmed vulnerability. */
export interface PreScanLead {
  /** `P1`, `P2`, …; hunters and the synthesis cite it. */
  id: string;
  check: string | null;
  /** Index into the security preset's eight focus areas. */
  focus: number;
  path: string;
  line: number | null;
  commit: string | null;
  message: string;
}

/** Operator checklist item states. There is deliberately no "passed": code cannot prove these. */
export type ChecklistStatus = "needs_review" | "not_verifiable";

export interface ChecklistItem {
  id: string;
  title: string;
  status: ChecklistStatus;
  evidence: string;
}

/** A security run's pre-scan, run on the host mirror before the hunters launch. */
export interface PreScan {
  ran_at: string | null;
  commit: string | null;
  /** `gitleaks` when the host had it installed, `builtin` for the fallback, empty when it could not run. */
  secret_scanner: string;
  notes: string[];
  leads: PreScanLead[];
  checklist: ChecklistItem[];
}

/** GET /api/redteam/runs: one swarm against one repository. */
export interface RedTeamRun {
  id: string;
  repo: string;
  org: string;
  state: RedTeamState;
  swarm_size: number;
  modules: string[];
  /** Whether the swarm may merge its finds; off by default, so a raid never touches main. */
  autofix: boolean;
  hunters: RedTeamHunter[];
  /** `merged` is the distinct defects after the synthesis dedup; null until a synthesis finishes. */
  counts: { found: number; validated: number; rejected: number; filed: number; merged: number | null };
  /** The run's synthesis step; null until one has launched (the mothership does it at `done`). */
  synthesis: RedTeamSynthesis | null;
  created_at: string;
  started_at: string | null;
  ended_at: string | null;
  /** The server's reason for holding an armed run at the gate; null while none applies. */
  gate_reason: string | null;
  /** Who hunts: `swarm` (colony hunters). Absent from runs made before hunters were named. */
  hunter?: string;
  /** The hunters' orchestrator / subagent models when named; null uses the agent defaults. */
  model?: string | null;
  subagent_model?: string | null;
  /** The schedule that started this run, if one did. */
  schedule_id?: string | null;
  /** Absent from runs made before presets: read as general. */
  preset?: RedTeamPreset;
  /** A security run's pre-scan; null for general runs and until a security run launches. */
  prescan?: PreScan | null;
}

/** POST /api/redteam/runs. `arm: true` starts gated, waiting for the nest to empty. */
export interface StartRedTeamRunRequest {
  repo: string;
  swarm_size?: number;
  modules?: string[];
  autofix?: boolean;
  arm?: boolean;
  hunter?: string;
  model?: string | null;
  subagent_model?: string | null;
  /** `general` when unset. */
  preset?: RedTeamPreset;
}

/** When a red-team schedule fires, in UTC. `weekday` 0 = Monday; a monthly `day` past the month's end fires on its last day. */
export type RedTeamCadence =
  | { every: "weekly"; weekday: number; hour: number; minute: number }
  | { every: "monthly"; day: number; hour: number; minute: number };

/** A recurring red-team run (GET /api/redteam/schedules). */
export interface RedTeamSchedule {
  id: string;
  org: string;
  repos: string[];
  hunter: string;
  swarm_size: number;
  model: string | null;
  subagent_model: string | null;
  autofix: boolean;
  /** Absent from schedules saved before presets: read as general. */
  preset?: RedTeamPreset;
  cadence: RedTeamCadence;
  enabled: boolean;
  next_run_at: string;
  last_run_at: string | null;
  last_result: string | null;
  created_at: string;
}

/** POST /api/redteam/schedules, and PUT /api/redteam/schedules/{id} (a full replace). */
export interface NewRedTeamSchedule {
  org: string;
  repos: string[];
  hunter?: string;
  swarm_size?: number;
  model?: string | null;
  subagent_model?: string | null;
  autofix?: boolean;
  preset?: RedTeamPreset;
  cadence: RedTeamCadence;
  enabled?: boolean;
}

/**
 * When a loop runs, in UTC (loops.rs, schedule.rs). `self_paced`: each run names the next
 * (loop_next), else a day later. `every_days` runs whole days apart at one time of day; the server
 * refuses days outside 1–365.
 */
export type LoopCadence =
  | RedTeamCadence
  | { every: "interval"; minutes: number }
  | { every: "daily"; hour: number; minute: number }
  | { every: "every_days"; days: number; hour: number; minute: number }
  | { every: "self_paced" };

/** A scheduled colony (GET /api/loops). */
export interface Loop {
  id: string;
  name: string;
  org: string;
  repo: string;
  prompt: string;
  cadence: LoopCadence;
  /** What a run starts: a colony from `prompt` (the default), the repository's architecture map, or —
   * for the one built-in loop, id `disk-cleanup` — the mothership's own disk cleanup. */
  kind?: LoopKind;
  /** Map loops only: repositories still queued this cycle; `owner/*` is re-listed every run. */
  pending?: string[];
  /** The built-in disk-cleanup loop only: its settings, run history and attention item. */
  disk_cleanup?: DiskCleanupState;
  tz_offset_minutes: number;
  model: string | null;
  subagent_model: string | null;
  autopilot: boolean;
  max_runs: number | null;
  end_at: string | null;
  enabled: boolean;
  /** Null once the loop has ended. */
  next_run_at: string | null;
  runs: number;
  last_run: { session: string; at: string } | null;
  /** The last thing it did or was told: a skip, the colony's chosen next run, why it ended. */
  last_note: string | null;
  ended_reason: string | null;
  created_at: string;
}

export type LoopKind = "colony" | "map" | "disk_cleanup";

/** What the built-in disk-cleanup loop may clean, each with its own switch. */
export type DiskCleanupCategory = "build_output" | "worktrees" | "microvms" | "archives" | "host_paths";

/** The disk-cleanup loop's settings (PUT /api/loops/disk-cleanup's `disk_cleanup`). */
export interface DiskCleanupSettings {
  /** Run early when free space is under this percent of the disk; 0 is off. */
  trigger_free_pct: number;
  build_output: boolean;
  stopped_after_days: number;
  worktrees: boolean;
  microvms: boolean;
  archives: boolean;
  archive_keep_days: number;
  archive_max_gb: number | null;
  /** Owner only, off by default: Cargo target/ dirs under `extra_paths`. */
  host_paths: boolean;
  extra_paths: string[];
  host_min_age_days: number;
}

export interface DiskCleanupCategoryReport {
  category: DiskCleanupCategory;
  enabled: boolean;
  items: { path: string; bytes: number | null; colony?: string }[];
  count: number;
  /** Freed, or in a dry run, would be freed. */
  bytes: number;
  held?: { path: string; reason: string }[];
  failed?: string[];
  note?: string;
}

/** POST /api/loops/disk-cleanup/run-now[?dry_run=1], and each entry of the loop's history. */
export interface DiskCleanupReport {
  at: string;
  dry_run: boolean;
  trigger: "schedule" | "low_disk" | "manual" | string;
  bytes: number;
  categories: DiskCleanupCategoryReport[];
  free_bytes_after?: number;
  used_pct_after?: number;
  attention?: string;
}

export interface DiskCleanupState {
  settings: DiskCleanupSettings;
  /** Real runs, newest first. */
  history: DiskCleanupReport[];
  attention: string | null;
  /** When a dry run was last shown; null until the owner has seen one. */
  previewed_at: string | null;
}

// The built-in "Dependencies & supply chain" loop (GET/PUT /api/supply-chain-loop,
// POST /api/supply-chain-loop/run; supply_chain_loop.rs).

export type SupplySeverity = "critical" | "high" | "moderate" | "low";
export type SupplyFindingKind = "vulnerability" | "yanked" | "unmaintained" | "deprecated" | "license" | "outdated";

/** The loop's settings: off, with an empty allowlist, until the operator opts in. */
export interface SupplyChainSettings {
  enabled: boolean;
  /** Orgs (`acme`) and repositories (`acme/app`) opted in. */
  allow: string[];
  /** Daily by default; `interval` no tighter than 60 minutes. */
  cadence: LoopCadence;
  max_per_repo: number;
  max_per_run: number;
  cooldown_hours: number;
  /** The least severe finding that is dispatched; everything is reported. */
  min_severity: SupplySeverity;
  /** Also report direct dependencies a major version or more behind (never dispatched). */
  outdated: boolean;
  /** Check lockfiles no host scanner reads with the mothership's own OSV lookup. */
  builtin: boolean;
  autopilot: boolean;
}

export interface SupplyFinding {
  ecosystem: string;
  package: string;
  version: string | null;
  kind: SupplyFindingKind;
  severity: SupplySeverity;
  id: string | null;
  title: string;
  fixed: string | null;
  fix_available: boolean;
  fix_via?: string | null;
  major_bump: boolean;
  url: string | null;
  lockfile: string;
  scanner: string;
}

export interface SupplyRepoReport {
  repo: string;
  sha: string | null;
  scanners: string[];
  findings: SupplyFinding[];
  notes: string[];
  /** Files nothing checked, and what to install. */
  missing: string[];
  error: string | null;
}

export interface SupplyAttention {
  repo: string;
  ecosystem: string;
  package: string;
  version: string | null;
  id: string | null;
  severity: SupplySeverity;
  reason: string;
}

export interface SupplyChainReport {
  id: string;
  started_at: string;
  finished_at: string;
  dry_run: boolean;
  trigger: "schedule" | "manual";
  blocked: boolean;
  repos: SupplyRepoReport[];
  counts: Partial<Record<SupplySeverity, number>>;
  dispatched: { repo: string; ecosystem: string; session: string | null; title: string; findings: number; worst: SupplySeverity }[];
  skipped: { repo: string; ecosystem: string | null; reason: string; findings: number }[];
  attention: SupplyAttention[];
  note: string | null;
}

export interface SupplyChainRun {
  id: string;
  at: string;
  trigger: string;
  counts: Partial<Record<SupplySeverity, number>>;
  dispatched: number;
  skipped: number;
  attention: number;
  summary: string;
}

export interface SupplyChainLoop {
  name: string;
  settings: SupplyChainSettings;
  next_run_at: string | null;
  running: boolean;
  /** Which scanners the mothership's host has installed. */
  scanners: Record<string, boolean>;
  /** Whether COLONIZER_NO_EXTERNAL_EFFECTS holds every dispatch. */
  blocked: boolean;
  last_report: SupplyChainReport | null;
  history: SupplyChainRun[];
  attention: SupplyAttention[];
}

/** POST /api/loops, and PUT /api/loops/{id} (a full replace). */
export interface NewLoop {
  name: string;
  repo: string;
  prompt: string;
  cadence: LoopCadence;
  /** Colony loops (the default) or map loops; a map loop's `repo` may be `owner/*`. `disk_cleanup`
   * only on the built-in loop's own PUT. */
  kind?: LoopKind;
  tz_offset_minutes?: number;
  model?: string | null;
  subagent_model?: string | null;
  autopilot?: boolean;
  max_runs?: number | null;
  end_at?: string | null;
  enabled?: boolean;
  /** The built-in disk-cleanup loop only; left out, its settings are kept. */
  disk_cleanup?: DiskCleanupSettings;
}

// The built-in "TypeScript: remove any" loop (GET/PUT /api/ts-any-loop,
// POST /api/ts-any-loop/run; ts_any_loop.rs).

export type TsAnyForm = "annotation" | "as" | "angle" | "type_argument" | "array" | "array_generic" | "record" | "generic_default" | "other";
export type TsAnyMethod = "typescript" | "token_scan";

/** The loop's settings: off, with an empty allowlist, until the operator opts in. */
export interface TsAnySettings {
  enabled: boolean;
  /** Orgs (`acme`) and repositories (`acme/app`) opted in. */
  allow: string[];
  /** Daily by default; `interval` no tighter than 60 minutes. */
  cadence: LoopCadence;
  /** Occurrences given to one colony (20 by default). */
  batch_cap: number;
  max_per_run: number;
  cooldown_hours: number;
  /** Also count implicit any (only with the repository's own TypeScript). */
  implicit: boolean;
  /** Install from the lockfile, offline, when node_modules is absent. */
  offline_install: boolean;
  autopilot: boolean;
}

export interface TsAnyModuleCount {
  module: string;
  explicit: number;
  files: number;
}

export interface TsAnyFileCount {
  path: string;
  module: string;
  explicit: number;
  implicit?: number | null;
  as_casts: number;
  suppressions: number;
}

export interface TsAnyRepoReport {
  repo: string;
  sha: string | null;
  typescript: boolean;
  method: TsAnyMethod | null;
  method_note: string | null;
  ts_version: string | null;
  total: number;
  implicit: number | null;
  as_casts: number;
  suppressions: number;
  ts_files: number;
  forms: Partial<Record<TsAnyForm, number>>;
  modules: TsAnyModuleCount[];
  files: TsAnyFileCount[];
  /** Earlier real runs' totals, newest first. */
  previous: number[];
  notes: string[];
  error: string | null;
}

export interface TsAnyAttention {
  repo: string;
  module: string;
  session: string;
  pr_url: string | null;
  problems: string[];
  reason: string;
}

export interface TsAnyReport {
  id: string;
  started_at: string;
  finished_at: string;
  dry_run: boolean;
  trigger: "schedule" | "manual";
  blocked: boolean;
  repos: TsAnyRepoReport[];
  total: number;
  dispatched: { repo: string; module: string; session: string | null; title: string; occurrences: number; module_total: number }[];
  skipped: { repo: string; module: string | null; reason: string }[];
  checks: { session: string; repo: string; module: string; pr_url: string | null; flagged: boolean; summary: string }[];
  attention: TsAnyAttention[];
  note: string | null;
}

export interface TsAnyRun {
  id: string;
  at: string;
  trigger: string;
  total: number;
  totals: Record<string, number>;
  dispatched: number;
  skipped: number;
  flagged: number;
  summary: string;
}

export interface TsAnyLoop {
  name: string;
  settings: TsAnySettings;
  next_run_at: string | null;
  running: boolean;
  /** Whether node is on the host (the repository's own TypeScript needs it). */
  node: boolean;
  /** Whether COLONIZER_NO_EXTERNAL_EFFECTS holds every dispatch. */
  blocked: boolean;
  last_report: TsAnyReport | null;
  /** Newest first. */
  history: TsAnyRun[];
  attention: TsAnyAttention[];
  /** Totals per repository, oldest first. */
  trend: Record<string, { at: string; total: number; sha: string | null }[]>;
}

/** GET /api/hunters/{id}/probe: whether an external hunter is installed and could run here. */
export interface HunterProbe {
  manifest: { id: string; name: string; description: string; homepage: string; licence: string; available: boolean; needs_docker: boolean };
  installed: string | null;
  probe: { runtime_ok: boolean; docker_ok: boolean; ready: boolean; detail: string };
}

/** GET /api/repos/{owner}/{repo}/packages: whether the repository is a monorepo, and its packages. */
export interface RepoPackages {
  monorepo: boolean;
  tool: string | null;
  packages: { name: string; path: string }[];
}

// ---------------------------------------------------------------------------
// Saved secrets (GET /api/secrets): where each key lives, never its value
// ---------------------------------------------------------------------------

export type SecretLocation = "keychain" | "file" | "env" | "unset";
export type SecretGroup = "providers" | "connections" | "integrations" | "colonies";

/** What a colony gets of a secret: nothing, the gateway's use of it, or a per-host substitution. */
export interface SecretColonyAccess {
  kind: "gateway" | "injected" | "none";
  /** For `injected`: the hosts msb swaps the placeholder for the value on (TLS only). */
  hosts: string[];
}

/** Which colonies a colony secret is given to. */
export type ColonySecretScope = { kind: "all" } | { kind: "org"; org: string } | { kind: "repo"; repo: string };

/** POST /api/secrets/colony. `value` is required for a new secret. */
export interface ColonySecretRequest {
  env: string;
  hosts: string[];
  scope: ColonySecretScope;
  value?: string;
}

export interface SecretRow {
  /** Stable id, e.g. `provider-keys:zai`; the path segment for PUT/DELETE/move. */
  id: string;
  label: string;
  group: SecretGroup;
  used_by: string;
  icon: string;
  location: SecretLocation;
  /** The environment variable that can also supply it, if any. */
  env: string | null;
  env_set: boolean;
  updated_at: string | null;
  /** False for secrets managed elsewhere (environment-only, or read by the CLI off disk). */
  editable: boolean;
  /** How it reaches colonies; absent from an older mothership. */
  colonies?: SecretColonyAccess;
}

export interface KeychainHealth {
  available: boolean;
  /** "macOS Keychain", "Secret Service", or "none". */
  backend: string;
  reason: string | null;
  checked_at: string | null;
}

export interface SecretsListing {
  keychain: KeychainHealth;
  secrets: SecretRow[];
}

/** One component of a repository's architecture map (GET /api/maps/{owner}/{repo}, from an archify
 *  architecture diagram): archify's own layout (`pos` top-left, `size`), and the repository files
 *  it lives in. */
export interface ArchComponent {
  id: string;
  type: string;
  label: string;
  sublabel?: string | null;
  pos: [number, number];
  size: [number, number];
  sources: { path: string; line?: number; label?: string }[];
}

/** The fields of an archify architecture diagram the cockpit draws, as the mothership stores them. */
export interface ArchMap {
  title: string;
  subtitle?: string | null;
  components: ArchComponent[];
  connections: { from: string; to: string; label?: string }[];
  boundaries: { label: string; wraps: string[] }[];
}

/** GET /api/maps/{owner}/{repo}: the stored map, if any, and the newest mapping colony, if any. */
/** One tool call a colony made on a file (GET /api/maps/{owner}/{repo}/file). */
export interface MapFileActivity {
  ts: string;
  tool: string;
  summary: string;
  /** The subagent (settler) the call ran in, when the event says. */
  agent: string | null;
}

/** A live colony on one file: what it did there, and its diff of it. */
export interface MapFileColony {
  id: string;
  title: string;
  issue: number | null;
  status: SessionStatus;
  mode: "changing" | "reading";
  activity: MapFileActivity[];
  diff: string | null;
  diff_truncated: boolean;
}

/** GET /api/maps/{owner}/{repo}/file?path=…: every live colony changing or reading one file. */
export interface MapFileDetail {
  repo: string;
  path: string;
  colonies: MapFileColony[];
}

export interface RepoMap {
  repo: string;
  map: { repo: string; revision: string | null; generated_at: string; session: string; map: ArchMap } | null;
  mapping: { id: string; status: SessionStatus; created_at: string } | null;
}

/** GET /api/touched: the files each live colony's worktree has changed, keyed by session id. */
export interface TouchedFiles {
  sessions: Record<string, string[]>;
  /** The files each live colony's recent tool calls looked at, newest first; absent from an older mothership. */
  reading?: Record<string, string[]>;
}

/** GET /api/repos/{owner}/{repo}/meta: what the repository picker shows about a repository. */
export interface RepoMeta {
  full_name: string;
  description: string | null;
  homepage: string | null;
  stars: number;
  primary_language: string | null;
  languages: { name: string; bytes: number; percent: number }[];
  /** 52 weekly commit counts, oldest first; empty while GitHub is still computing them. */
  commits_weekly: number[];
  stats_pending: boolean;
  contributors: { login: string; avatar_url: string; contributions: number }[];
  pushed_at: string | null;
  html_url: string | null;
}

// ---------------------------------------------------------------------------
// The Code page (code.rs): a repository read from the mothership's bare clone
// ---------------------------------------------------------------------------

/** GET /api/repos/{o}/{r}/loc: lines of code by language at the default branch. */
export interface RepoLoc {
  ref: string;
  sha: string;
  total: number;
  by_language: { name: string; files: number; code: number; blank: number }[];
}

/** GET /api/repos/{o}/{r}/coverage: line coverage from CI artifacts, or why there is none. */
export type RepoCoverage =
  | { measured: true; percent: number; format: string; file: string; artifact: string; run: number; at?: string }
  | { measured: false; reason: string };

/** GET /api/repos/{o}/{r}/git-summary. */
export interface RepoGitSummary {
  repo: string;
  branches: number;
  open_prs: number | null;
  release: { tagName: string; name: string; publishedAt: string } | null;
  latest_tag: string | null;
}

export interface RepoBranch {
  name: string;
  sha: string;
  date: string;
  author: string;
  message: string;
  default: boolean;
  protected: boolean;
  colony: boolean;
  ahead: number;
  behind: number;
  pr: { number: number; title: string; url: string; isDraft: boolean } | null;
}

export interface RepoBranches {
  repo: string;
  default: string;
  branches: RepoBranch[];
}

export interface RepoTree {
  repo: string;
  ref: string;
  sha: string;
  paths: string[];
  truncated: boolean;
}

export interface RepoBlob {
  path: string;
  ref: string;
  sha: string;
  size: number;
  binary: boolean;
  too_large: boolean;
  text: string | null;
}

export interface FileCommit {
  sha: string;
  author: string;
  date: string;
  message: string;
}

export interface RepoBlame {
  path: string;
  ref: string;
  sha: string;
  commits: Record<string, { author?: string; time?: number; summary?: string }>;
  /** Per line (0-based index = line - 1), the commit that last touched it. */
  lines: string[];
}

export interface EditsRequest {
  base?: string;
  branch: string;
  message: string;
  title: string;
  body: string;
  files: { path: string; content: string }[];
}

/** An autosaved edit on the mothership (never on GitHub until a pull request is confirmed). */
export interface Draft {
  ref: string;
  path: string;
  content: string;
  base_sha: string;
  saved_at: string;
}

/** GET/POST /api/login-item: whether the mothership starts at login (a LaunchAgent or systemd user unit). */
export interface LoginItemStatus {
  platform: "macos" | "linux" | "unsupported";
  installed: boolean;
  enabled: boolean;
  pid: number | null;
  definition: string;
  binary: string;
  log: string;
  note: string | null;
}

// ---------------------------------------------------------------------------
// Web push (issue #516): the mothership pushes to phones via GET/POST/DELETE /api/push
// ---------------------------------------------------------------------------

/** One event a device can be told about; a key the prefs omit means "the default". */
export type PushEventKind =
  | "question"
  | "pull_request"
  | "needs_rebase"
  | "failed"
  | "attention"
  | "provider_degraded"
  | "digest";

/** Per-device delivery prefs (issue #743), as PATCH takes and the summary answers. */
export interface PushPrefs {
  events: Partial<Record<PushEventKind, boolean>>;
  /** A sound may accompany a question's push; every other event is silent. */
  question_sound: boolean;
  /** A question's notification may offer answer buttons (issue #742). */
  answer_actions: boolean;
  /** Pushes set the installed app's badge to the needs-you count (issue #744). */
  badge: boolean;
  /** Repositories the device hears about, entries "org" or "org/repo"; empty means all. */
  scope: string[];
  /** Minutes since local midnight; start may wrap past midnight, never equals end. Null is off. */
  quiet: { start: number; end: number } | null;
  /** A question's push breaks through quiet hours when nothing else may. */
  questions_break_quiet: boolean;
  /** The device's IANA timezone, as it reported itself; null until a save that knows it. */
  tz: string | null;
  /** Minutes east of UTC (the sign of JS `getTimezoneOffset()`, negated). */
  utc_offset: number;
}

/** One enrolled device, as GET /api/push/subscriptions answers and POST returns. */
export interface PushSubscriptionSummary {
  id: string;
  label: string;
  /** Unix seconds. */
  created_at: number;
  /** The push service's host (e.g. fcm.googleapis.com); the full endpoint never reaches the list. */
  endpoint_host: string;
  /** Unix seconds of the last presence report; null until the first one. */
  last_seen: number | null;
  prefs: PushPrefs;
  /** The paired phone (issue #746) that subscribed this device, if one did; revoking it drops this subscription. */
  phone?: string | null;
}

/** POST /api/push/subscriptions: the browser's `PushSubscription.toJSON()` plus a device label. */
export interface PushSubscribeBody {
  label: string;
  endpoint: string;
  keys: { p256dh: string; auth: string };
}

/** PATCH /api/push/subscriptions/{id}: rename the device and/or replace its prefs wholesale. */
export interface PushSubscriptionPatch {
  label?: string;
  prefs?: PushPrefs;
}

/** POST /api/push/presence: where this tab is, and whether it can take the notification itself. */
export interface PushPresenceBody {
  endpoint: string;
  /** The colony this tab has open, or null when none — a push for it can be suppressed. */
  colony: string | null;
  focused: boolean;
  tz?: string;
  utc_offset?: number;
}

// ---------------------------------------------------------------------------
// Remote access (issue #535): GET/PUT /api/remote, POST /api/remote/reset
// (docs/protocol.md §6.10), plus the relay's pairing view the cockpit mirrors
// ---------------------------------------------------------------------------

/** The switch, the tunnel host and the live link, as all three /api/remote endpoints answer. */
export interface RemoteStatus {
  enabled: boolean;
  /** e.g. `h4xk2q7mzt5pw3nd6vrc.my.colonizer.dev`; null until the first enable. The link is `https://<host>`. */
  host: string | null;
  /** True only while the switch is on and the tunnel's handshake has succeeded. */
  connected: boolean;
  /** RFC3339, only while connected: when the current tunnel came up. */
  since: string | null;
  /** True when the relay closed the tunnel because a newer one took this link over; it stays that way until a re-enable or reset dials again. */
  replaced: boolean;
}

/** One pairing code waiting at the relay (services/relay/src/worker.js `pairingView`). */
export interface RemotePairingRequest {
  /** Six digits. */
  code: string;
  github_login: string;
  /** When the code stops working, in unix seconds — the encoding the relay pins for timestamps. */
  expires_at: number;
}

/** GET /api/remote/pairing: the owner binding and the pending codes, mirrored from the relay. */
export interface RemotePairing {
  owner: { github_login: string } | null;
  pending: RemotePairingRequest[];
}

// ---------------------------------------------------------------------------
// Add your phone (issue #746): /api/phone — a single-use invite a phone scans,
// a code confirmed in the local cockpit, and a revocable credential per phone
// ---------------------------------------------------------------------------

/** One place the cockpit is reachable from, in the mothership's preference order (relay → tailnet → lan). */
export interface PhoneOrigin {
  kind: "relay" | "tailnet" | "lan";
  /** `scheme://host[:port]`, no trailing slash — the base the invite link is built on. */
  url: string;
  /** Whether the mothership thinks a phone can reach this origin right now. */
  reachable: boolean;
  /** False for a plain-http origin: the phone can pair, but not install the app or get notifications. */
  secure: boolean;
  /** Why the origin is (un)usable, when the mothership has something to say about it. */
  note: string | null;
}

/** POST /api/phone/invites: a single-use invite — a ticket to ask, never a credential — and where a phone might open it. */
export interface PhoneInvite {
  code: string;
  /** RFC3339: when the invite stops working. */
  expires_at: string;
  ttl_secs: number;
  origins: PhoneOrigin[];
}

/** A paired phone, with its own credential; revoking it signs that phone out alone. */
export interface PairedPhone {
  id: string;
  label: string;
  paired_at: string;
}

/** A phone that opened an invite and shows a code, waiting for it to be typed here. */
export interface PendingPhone {
  id: string;
  label: string;
  expires_at: string;
}

/** GET /api/phone. */
export interface Phones {
  devices: PairedPhone[];
  pending: PendingPhone[];
  /** The same ranked origins an invite answers with (bare origins, no code, no credential), so a
   * bookmark can name the network address without minting an invite. Older motherships omit it. */
  origins?: PhoneOrigin[];
}

// ---------------------------------------------------------------------------
// Scoped API tokens (issue #646): GET/POST /api/tokens, DELETE /api/tokens/{id}
// (docs/cli.md, "Scoped API tokens")
// ---------------------------------------------------------------------------

/** How much a token may do, ordered so `read` < `operate` < `launch` — each adds to the last.
 * `fleet` sits outside that ladder: the lowest scope there is, admitted only on the fleet routes
 * (`GET /api/hosts` and `POST /api/fleet/peer/leave`, docs/fleet.md), and minted by fleet pairing
 * rather than created by hand. */
export type ApiTokenScope = "fleet" | "read" | "operate" | "launch";

/** One token's metadata, as GET /api/tokens answers: never the secret, never its hash. */
export interface ApiTokenMeta {
  id: string;
  name: string;
  scope: ApiTokenScope;
  /** The GitHub owners the token stays inside; empty means no limit of this kind. */
  orgs: string[];
  /** The `owner/repo` repositories the token stays inside; empty means no limit of this kind. */
  repos: string[];
  /** The most colonies it may keep unfinished; absent when uncapped. */
  max_concurrent?: number;
  /** The most model spend its colonies may run up per UTC day; absent when uncapped. */
  budget_usd_per_day?: number;
  /** RFC3339. */
  created_at: string;
  /** RFC3339; absent until its first use, and refreshed at most once a minute, in memory only. */
  last_used_at?: string;
}

/** POST /api/tokens: what the cockpit's create form collects. */
export interface NewApiToken {
  name: string;
  scope: ApiTokenScope;
  orgs?: string[];
  repos?: string[];
  max_concurrent?: number;
  budget_usd_per_day?: number;
}

/** POST /api/tokens' answer: the plaintext, shown exactly once, next to the metadata. */
export interface CreatedApiToken extends ApiTokenMeta {
  token: string;
}

// ---------------------------------------------------------------------------
// Chat: a direct conversation with a model, no colony (GET/POST /api/chat, docs/protocol.md)
// ---------------------------------------------------------------------------

export interface ChatMeta {
  id: string;
  title: string;
  model: string;
  system?: string;
  max_tokens: number;
  /** The workspace its spend is filed under; absent files it under the `chat` pseudo-org. */
  workspace?: string;
  created_at: string;
  updated_at: string;
  /** Kept at the top of the list; absent from an older mothership. */
  pinned?: boolean;
  /** 0–1; absent leaves it to the provider. */
  temperature?: number;
  /** The persona preset the system prompt came from, a label only. */
  persona?: string;
  /** The title is still the automatic one; the first reply replaces it with a generated one. */
  auto_title?: boolean;
  forked_from?: { chat: string; message: string };
}

/** What an attachment left on the message it came with: never the content, only what it was. An image
 * also carries its stored reference, so it can be shown and sent to the model again. */
export interface ChatAttachmentNote {
  kind: string;
  label: string;
  sha?: string;
  mime?: string;
  width?: number;
  height?: number;
  bytes?: number;
}

/** A stored chat image (POST /api/chat/attachments), content-addressed by its sha256. */
export interface ChatImageRef {
  sha: string;
  mime: string;
  width: number;
  height: number;
  bytes: number;
}

/** Persona preset edits and notes on replies, kept on the mothership (GET /api/chat/prefs). */
export interface ChatPrefs {
  /** Preset id → the system prompt saved to it. */
  personas: Record<string, string>;
  /** Reply message id → the operator's note on it. */
  feedback: Record<string, string>;
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  ts: string;
  model?: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd?: number;
  stopped: boolean;
  error?: string;
  parent_id?: string;
  first_token_ms?: number;
  latency_ms?: number;
  attachments?: ChatAttachmentNote[];
  /** A compare reply not picked yet; the model's history leaves it out. */
  candidate?: boolean;
  lane?: number;
}

export interface ChatProvider {
  id: string;
  name: string;
  models: string[];
  preset?: string;
  wire?: "anthropic" | "openai";
  has_key?: boolean;
  pricing?: { input_per_mtok: number; output_per_mtok: number } | null;
}

export interface ChatModels {
  /** The cheap default (the summaries' model), or null when none is reachable. */
  default: string | null;
  /** Plain Claude models: usable only with an Anthropic API key or an Anthropic provider. */
  claude: { available: boolean; reason: string | null };
  providers: ChatProvider[];
}

/** Something attached to a message (docs/protocol.md, "Chat attachments"). */
export type ChatAttachment =
  | { kind: "colony"; id: string }
  | { kind: "file"; repo: string; path: string; ref?: string }
  | { kind: "map"; repo: string }
  | { kind: "map_component"; repo: string; component: string }
  | { kind: "snippet"; label?: string; text: string }
  /** A stored image (`sha`), or older clients' inline base64 `data`, which the mothership stores first. */
  | { kind: "image"; sha: string; name?: string }
  | { kind: "image"; media_type: string; data: string; name?: string }
  | { kind: "colonies_today"; org?: string }
  | { kind: "merged_prs"; org?: string; days?: number };

/** One line of the streamed reply to POST /api/chat/{id}/messages (or /compare, tagged by `lane`). */
export type ChatStreamEvent = (
  | { type: "delta"; text: string }
  | { type: "done"; message: ChatMessage; chat?: ChatMeta }
  | { type: "error"; message: string; message_record?: ChatMessage }
) & { lane?: number };

export interface ChatSendRequest {
  content?: string;
  regenerate?: boolean;
  /** Answer with this model once; the conversation keeps its own. */
  model?: string;
  context?: { colony?: string; file?: { repo: string; path: string; ref?: string } };
  attachments?: ChatAttachment[];
}

export interface ChatCompareRequest {
  content: string;
  models: [string, string];
  attachments?: ChatAttachment[];
}

export type ChatPatch = Partial<Pick<ChatMeta, "title" | "model" | "system" | "max_tokens" | "workspace" | "pinned" | "persona">> & {
  /** A negative temperature clears it. */
  temperature?: number;
};

// ---------------------------------------------------------------------------
// Packages (GET /api/orgs/{org}/packages/*): published, dependencies, supply chain
// ---------------------------------------------------------------------------

/** A scan that has not landed yet: ask again in a few seconds. */
export interface ScanPending {
  status: "scanning";
  message: string;
}

/** What the mothership adds to an answer served from its cache: when it was computed, and whether
 *  a refresh is running behind it. */
export interface CacheInfo {
  cached_at?: string;
  refreshing?: boolean;
}

export type Ecosystem = "npm" | "cargo" | "pypi" | "go" | "dart" | "swift";

export interface ScannedRepo {
  repo: string;
  sha?: string;
  error?: string;
  lockfiles?: string[];
  skipped?: string[];
  defined?: number;
}

export interface RegistryInfo {
  latest: string | null;
  published_at: string | null;
  created_at?: string | null;
  downloads: number | null;
  downloads_period?: string;
  url: string;
}

export interface PublishedPackage {
  ecosystem: Ecosystem;
  name: string;
  version: string | null;
  repo: string;
  path: string;
  private: boolean;
  registry: string | null;
  status: "published" | "unpublished" | "private";
  /** The repository's version is ahead of the registry's latest. */
  unreleased_changes: boolean;
  published: RegistryInfo | null;
}

export interface GithubPackage {
  name: string;
  type: string;
  visibility: string;
  versions: number | null;
  updated_at: string | null;
  url: string | null;
  repo: string | null;
}

export interface PackagesPublished extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  packages: PublishedPackage[];
  github_packages: { packages: GithubPackage[]; note: string | null };
}

export interface Advisory {
  id: string;
  summary?: string | null;
  severity: string;
  fixed?: string | null;
  url?: string;
}

export interface DependencyVersion {
  version: string;
  behind: boolean;
  users: { repo: string; path: string }[];
  vulns: Advisory[];
}

export interface Dependency {
  ecosystem: Ecosystem;
  name: string;
  direct: boolean | null;
  dev: boolean;
  latest: string | null;
  outdated: boolean;
  vulnerable: boolean;
  drift: boolean;
  versions: DependencyVersion[];
}

export interface PackagesDependencies extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  ecosystems: { ecosystem: Ecosystem; direct: number; transitive: number }[];
  totals: { direct: number; transitive: number; outdated: number; vulnerable: number };
  packages: Dependency[];
}

export type RiskSeverity = "critical" | "high" | "moderate" | "low";

export interface SupplyRisk {
  severity: RiskSeverity;
  kind: string;
  ecosystem: Ecosystem;
  name: string;
  version: string | null;
  reason: string;
  fix: { available: boolean; version?: string | null };
  url: string;
  direct: boolean;
  via: string[];
  users: { repo: string; path: string }[];
}

export interface SupplyChain extends CacheInfo {
  org: string;
  scanned_at: string;
  repos: ScannedRepo[];
  counts: Partial<Record<RiskSeverity, number>>;
  fixable: number;
  risks: SupplyRisk[];
  note: string;
}

/** The closed set of kinds an activity line carries (crates/colonizer/src/activity.rs `KINDS`). */
export type ActivityKind =
  | "outcome.pr_opened"
  | "outcome.merged"
  | "outcome.closed"
  | "outcome.no_changes"
  | "outcome.stopped"
  | "outcome.failed"
  | "outcome.question"
  | "colony.launch"
  | "colony.stop"
  | "colony.resume"
  | "colony.delete"
  | "colony.publish"
  | "colony.catch_up"
  | "colony.cleanup"
  | "colony.retain"
  | "colony.answer"
  | "chat.colony"
  | "chat.issue"
  | "colonize.issue"
  | "colonize.colony"
  | "decision.shadow"
  | "decision.act"
  | "decision.fallback"
  | "loop.create"
  | "loop.update"
  | "loop.pause"
  | "loop.resume"
  | "loop.delete"
  | "loop.run_now"
  | "loop.docs"
  | "redteam.start"
  | "redteam.stop"
  | "redteam.schedule"
  | "redteam.unschedule"
  | "remote.enable"
  | "remote.disable"
  | "remote.reset"
  | "remote.pair"
  | "remote.pair_reject"
  | "remote.unpair"
  | "workspace.enable"
  | "workspace.disable"
  | "workspace.settings"
  | "settings.save"
  | "settings.remove"
  | "memory.review"
  | "memory.note"
  | "burn_down.stop"
  | "app.update"
  | "map.create";

/**
 * One line of the mothership's activity log (GET /api/activity, docs/protocol.md §6.9): a colony's
 * outcome, recorded once at the transition, or something a person did through the API. Names what
 * changed — never a secret's value.
 */
export interface ActivityEntry {
  seq: number;
  ts: string;
  /** A kind this build knows, or a newer one it does not (drawn as a generic action). */
  kind: ActivityKind | (string & {});
  /** `you` (whoever holds the API token) or `colony`. */
  actor: "you" | "colony" | (string & {});
  /** How `you` reached the mothership: the browser (`cockpit`) or a bearer token (`api`). */
  via?: "cockpit" | "api" | null;
  org?: string | null;
  repo?: string | null;
  issue?: number | null;
  colony?: string | null;
  target?: string | null;
  /** The cockpit place that shows the target: a settings section id, `secrets`, `loops`, `redteam`, `memory`. */
  section?: string | null;
  title?: string | null;
  pr_url?: string | null;
  detail?: string | null;
}

export interface ActivityPage {
  entries: ActivityEntry[];
  /** Pass as `before` for the next (older) page; null on the last. */
  next_before: number | null;
  /** Lines the read could not parse; they are skipped, and said so. */
  skipped: number;
}

export interface ActivityQuery {
  before?: number;
  limit?: number;
  kind?: string;
  actor?: "you" | "colony";
  org?: string;
  repo?: string;
  q?: string;
}

/** One colony's pull request and where it stands in its repository's merge train (issue #671). */
export interface MergeTrainPr {
  session: string;
  pr_url: string;
  title: string;
  status: "next" | "waiting_ci" | "needs_rebase" | "waiting" | "skipped" | "merged";
  /** Why the pull request stands where it does — always set, for every status. */
  reason: string;
}

/** Per-repository merge-train state: the base branch's CI, the train's last merge and the queued pull requests. */
export interface MergeTrainRepo {
  repo: string;
  /** `on` while the train drives the repository, `off` before opt-in, `denied` when the org sits on `merge_train_deny_orgs`. */
  state: "on" | "off" | "denied";
  /** The repository's default branch; null when the train is off or denied here, or it could not be read. */
  base: string | null;
  base_ci: "green" | "pending" | "failing" | "unknown";
  /** When the mothership last looked; null before the first pass. */
  checked_at: string | null;
  /** The train's most recent merge; null until it merged one. */
  last_merge: { pr_url: string; at: string } | null;
  prs: MergeTrainPr[];
}

/** GET /api/merge-train (issue #671): empty until a repository opts in. */
export interface MergeTrainStatus {
  repos: MergeTrainRepo[];
}

/** The merge-train loop's settings (issue #754): off, hourly, and no repository opted in by default. */
export interface MergeLoopSettings {
  enabled: boolean;
  cadence: LoopCadence;
  /** Opted-in `owner` or `owner/repo` entries; empty merges nowhere. */
  allow: string[];
  /** `owner` or `owner/repo` entries never merged in (upstream-review-only forks), whatever `allow` says. */
  never: string[];
  max_merges: number;
  /** Per-repository caps that replace `max_merges` there. */
  repo_max_merges: Record<string, number>;
  cooldown_secs: number;
  ci_wait_minutes: number;
  ci_poll_secs: number;
  /** Check names re-run once when they are all that fails; a trailing `*` matches a prefix. */
  flaky_checks: string[];
  self_heal: boolean;
  revert_on_red: boolean;
  redo_on_conflict: boolean;
  max_api_calls: number;
  min_call_gap_ms: number;
  /** Colony ids held out of the loop. */
  held: string[];
}

export type MergeLoopAction = "merged" | "updated" | "rebased" | "red" | "rerun" | "needs_redo" | "redo_dispatched" | "waiting" | "skipped";

export interface MergeLoopItem {
  session: string;
  pr_url: string;
  title: string;
  action: MergeLoopAction;
  reason: string;
}

export interface MergeLoopRepoReport {
  repo: string;
  /** Main's CI as the run last read it, in words. */
  main: string;
  paused: string | null;
  /** What the run did about a red main. */
  heal: string[];
  items: MergeLoopItem[];
}

/** One run's report: merged, updated (CI running), red, redo dispatched, skipped — each with its reason. */
export interface MergeLoopReport {
  started_at: string | null;
  finished_at: string | null;
  dry_run: boolean;
  /** The kill switch (COLONIZER_NO_EXTERNAL_EFFECTS) turned a real run into this dry run. */
  forced_dry_run: boolean;
  /** Why the run stopped early: GitHub pushed back (403/429), or the call budget ran out. */
  stopped: string | null;
  api_calls: number;
  summary: string;
  lines: string[];
  repos: MergeLoopRepoReport[];
}

/** GET /api/merge-train/loop. */
export interface MergeLoopView {
  settings: MergeLoopSettings;
  next_run_at: string | null;
  running: boolean;
  writes_blocked: boolean;
  repos: Record<string, { paused: string | null; needs_redo: Record<string, string> }>;
  last_report: MergeLoopReport | null;
  /** Newest first. */
  history: MergeLoopReport[];
}
