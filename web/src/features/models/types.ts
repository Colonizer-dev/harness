// The header's model switcher (issue #1051): which model each role of each agent module runs on,
// install-wide and per org, and the switch that moves them.
import type { QuotaChange } from "../sessions/types";

/** Where a role's value comes from: an org's override, the install's agent settings, or the module's own default. */
export type ModelSource = "org" | "install" | "default";

/** One model role as it resolves for a scope: GET /api/models/assignments. */
export interface ModelRoleRow {
  /** The setting's key: `model`, `subagent_model`, `background_model`, `summary_model`, `small_model`, `model_low`, `model_high`. */
  role: string;
  /** The module schema's title for it, e.g. "Orchestrator model". */
  title: string;
  /** What it resolves to; empty is the agent's own default. */
  value: string;
  source: ModelSource;
  /** Whether an org can override it (orchestrator, subagent, background); the rest are install-wide only. */
  org_settable: boolean;
}

export interface InstallModelAssignment {
  module: string;
  roles: ModelRoleRow[];
}

export interface OrgModelAssignment {
  org: string;
  module: string;
  /** `org` when the org picked its own agent module, `install` when it follows the install's. */
  module_source: "org" | "install";
  roles: ModelRoleRow[];
}

/** An installed agent module and the model roles its module.json declares. */
export interface AgentModuleRoles {
  id: string;
  name: string;
  roles: { role: string; title: string; org_settable: boolean }[];
  /** Why it cannot launch on this install, in a launch refusal's words; null when it can. */
  blocked: string | null;
}

/** A model on offer, with its provider's health and quota. */
export interface SwitchableModel {
  id: string;
  label: string;
  /** `anthropic` for Claude's own models, else the provider id. */
  provider: string;
  provider_name: string;
  wire: "anthropic" | "openai" | null;
  failure_pct: number;
  rated: boolean;
  degraded: boolean;
  healthy: boolean;
  /** The provider's plan (or the Claude account's cap) is out: listed, but not pickable. */
  out_of_quota: boolean;
  reset_at: string | null;
  reset_unix: number | null;
}

export interface ModelAssignments {
  install: InstallModelAssignment;
  orgs: OrgModelAssignment[];
  modules: AgentModuleRoles[];
  models: SwitchableModel[];
}

/** POST /api/models/switch. */
export interface ModelSwitchRequest {
  scope: "install" | "org";
  org?: string;
  /** The scope's agent module; omitted keeps it, `""` returns an org to the install's. */
  module?: string;
  /** Role → model; null clears it (an org's override, or the install's back to the module default). */
  roles: Record<string, string | null>;
  /** `new` (default): colonies started from now on. `running`: also restart the scope's colonies on the new models. */
  apply?: "new" | "running";
  /** Plan only: which colonies would restart, nothing changed. */
  dry_run?: boolean;
  /** Also clear the Claude names the switch leaves in colony and org overrides (the "clear these too" option). */
  clear_leftovers?: boolean;
}

/**
 * The Claude model names a switch leaves behind in per-colony launch overrides and org overrides
 * (issue #1130): they still route to the Claude account after the roles moved to another provider.
 */
export interface LeftoverClaude {
  colonies: { id: string; role: string; model: string }[];
  orgs: { org: string; role: string; model: string }[];
  /** True when the request asked to clear them and they are gone. */
  cleared: boolean;
}

export interface ModelSwitchReply {
  dry_run: boolean;
  scope: "install" | "org";
  org: string | null;
  module: string;
  changes: QuotaChange[];
  /** The colonies the switch moves (a dry run's count to confirm). */
  affected: string[];
  /** The colonies restarted. */
  colonies: string[];
  failed: { id: string; ok: false; error?: string }[];
  /** Absent from older mothership builds. */
  leftover_claude?: LeftoverClaude;
}

/** What the mothership knows about one plan in use: GET /api/models/plans. Nothing is estimated. */
export interface PlanUsage {
  /** `anthropic` for the Claude account, else the provider id. */
  id: string;
  name: string;
  kind: "claude" | "provider";
  /** Plain-word roles routed here: `orchestrator`, `subagents`, `background`, `small tasks`, … */
  used_by: string[];
  /** Out right now: a limit was hit and its reset is still ahead. */
  exhausted: boolean;
  reset_at: string | null;
  reset_unix: number | null;
  /** The last limit the gateway saw, current or lapsed: when it hit and the reset it named. */
  last_limit: { at: string; reset_at: string | null; reset_unix: number | null } | null;
  /** Requests through the gateway since `since`; null for the Claude account, which it does not proxy. */
  requests: number | null;
  failures: number | null;
  last_request_at: string | null;
  since: string | null;
  /** The provider's plan-balance probe (issue #199); null when none is configured. */
  /** While the Claude account is out and its fallback carries the work: where its roles run (issue #1130). */
  fallback?: { model: string; provider_name: string } | null;
  balance: {
    remaining: number | null;
    /** The plan's total, when the probe names one (`quota.limit_pointer`). */
    limit: number | null;
    /** Derived only when both `remaining` and `limit` are known. */
    pct_left: number | null;
    error: string | null;
    checked_at: string | null;
  } | null;
  /** The account's window readings (issue #1223): "Session" and "Week" with percent used; null when nothing reported. */
  windows?: { label: string; used_pct: number; reset_unix: number | null }[] | null;
  /** Unix seconds the window reading was taken; null when unknown. */
  windows_checked_at?: number | null;
}

export interface ModelPlans {
  plans: PlanUsage[];
  checked_at: string;
}

/** A saved model profile, or a starter derived from what the install has configured. */
export interface ModelProfile {
  id: string;
  name: string;
  /** The agent module it was saved from: a hint only, applying never changes a scope's module. */
  module: string | null;
  /** Role → model id; `""` is the module's own default. */
  roles: Record<string, string>;
  /** A starter: derived, never stored, not renamed or deleted. */
  builtin: boolean;
  created_at?: string;
  updated_at?: string;
}

export interface ModelProfileBody {
  name?: string;
  module?: string | null;
  roles?: Record<string, string>;
}
