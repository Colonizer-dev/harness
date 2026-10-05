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
}
