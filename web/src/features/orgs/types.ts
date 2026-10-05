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
  /**
   * The org layer of the exec policy (issue #924): JSON text shaped like the install's `exec_policy`
   * module setting and a repository's `.colonizer/exec-policy.json`. It sits between the two, and
   * layers only narrow. Absent, null or blank adds no org layer.
   */
  exec_policy?: string | null;
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
  | "remote.device_approve"
  | "remote.device_revoke"
  | "remote.require_github"
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
