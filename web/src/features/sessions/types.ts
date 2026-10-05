import type { BoundaryRecord } from "../events/types";

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

export type AttentionReason =
  | "stalled"
  | "waiting_for_answer"
  | "nudges_exhausted"
  | "autopilot_held"
  | "provider_quota_exhausted"
  | "hold_timeout"
  | "model_error"
  /** The watchdog's control-defeat signature fired (issue #609); `signature`, `detail` and `evidence` say why. */
  | "control_defeat";

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
  /** `control_defeat` (issue #609): which pattern fired — `repeated_denial`, `ask_bypass`, `deny_then_reach`, `publish_rewrite`. */
  signature?: string;
  /** `control_defeat`: the boundary events that are its evidence. */
  evidence?: BoundaryRecord[];
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

/**
 * One model setting the boot resolved away from what it named because the gateway would have refused
 * the model for this colony's sensitivity class (issue #704) — a restricted colony's subagent model
 * on an untrusted provider, for instance.
 */
export interface ModelSubstitution {
  /** The model setting's name: `model`, `subagent_model`, `background_model` or `small_model`. */
  setting: string;
  /** The model the setting named, which the gateway would have refused. */
  from: string;
  /** The eligible model the colony runs on instead, or "the orchestrator's model" when cleared. */
  to: string;
  /** Why the gateway would have refused `from`, e.g. `"zai" is not marked trusted`. */
  reason: string;
}

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
  /** Why the fleet scheduler put this colony where it runs, in the scheduler's own words — e.g. "archlinux: 3 free slots" or "pinned to box-2" (issue #688). Absent when no reason was given. */
  placement?: string | null;
  worktree: string;
  /** Path of the worktree's git admin dir on the host; null until the worktree was created. */
  git_admin_dir: string | null;
  sandbox: string;
  mesh: { name: string; ip: string | null } | null;
  /** The guest-local port a dev-server preview is proxied from (`/api/previews/{id}/`), set by the owner; absent when no preview is open. */
  preview_port?: number;
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
  /** Every package/advisory pair a supply-chain loop colony was dispatched to fix (issue #832); absent otherwise. */
  supply_chain_targets?: { package: string; advisory: string }[];
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
  /**
   * Model settings the boot replaced with an eligible one because the gateway would have refused
   * what they named for this colony's sensitivity class (issue #704); absent when every model
   * cleared the bar. Shown on the colony view so what it really runs on is not hidden.
   */
  model_substitutions?: ModelSubstitution[];
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
  /** Pin the colony to a fleet member by id or name (issue #688). Omitting it, or naming this host, launches here; naming another member is refused with 409 until cross-member launch lands (#298). */
  host?: string;
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

/**
 * Who already holds the work a launch was refused for: the 409 body's `duplicate` (duplicates.rs,
 * issue #832). `colony` is absent for a remote claim found only by its branch or label; `host` is
 * set only for another mothership's claim; `queueable` says Wait behind the holder would work.
 */
export interface DuplicateHolder {
  kind: "issue" | "supply_chain" | "remote_claim";
  colony: string | null;
  host: string | null;
  status: string | null;
  pr_url: string | null;
  issue: number | null;
  /** The held work in words: `#7`, `lodash / ghsa-1`. */
  what: string;
  queueable: boolean;
}
