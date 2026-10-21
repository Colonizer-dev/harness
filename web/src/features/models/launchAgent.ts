// The agent and model new colonies start on (the cockpit's top-right menu). The menu lists only the
// models the install has enabled for each installed agent module that can launch (`modules`, not
// `blocked`: its `enabled_models`), grouped by module. The choice is kept in this browser; a launch
// sends `agent_module` and `model_override` (POST /api/sessions), and omits both when no choice has
// been made, so the org's own default applies. For a Claude model on Pi or Claude Code, a chosen
// Claude account goes as `claude_account`; "Org default" (no account chosen) sends none.
import { stored, store } from "../../components/ui";
import type { AgentModuleRoles, ModelAssignments, SwitchableModel } from "./types";

export const LAUNCH_AGENT_KEY = "colonizer.launch.agent";

/** The note shown for Pi on a Claude model: what the run is billed as. */
export const PI_CLAUDE_NOTE = "Billed as extra usage per token on your Claude subscription, not your plan limits.";

/** The picked agent module and model. `model` null keeps the default model for the launch. */
export interface LaunchSelection {
  module: string;
  model: string | null;
  /** The Claude account id for a Claude model on Pi or Claude Code; null is the org's account. */
  account?: string | null;
}

/** A bare Claude model id (`claude-opus-5-5`), as opposed to `<provider>/<model>`. */
export function isClaudeModel(id: string): boolean {
  return id.startsWith("claude-");
}

/** Whether the launch can name a Claude account: a Claude model on Pi or on Claude Code. */
export function takesClaudeAccount(module: string, model: string | null): boolean {
  return model !== null && isClaudeModel(model) && (module === "pi" || module === "claude-code");
}

/** One entry in the menu: an agent module running one model. */
export interface LaunchChoice {
  module: string;
  moduleName: string;
  /** The model id as the launch body sends it: a bare Claude id, or `<provider>/<model>`. */
  model: string;
  /** The model's name in the menu, e.g. "Claude Opus 5.5 (subscription)". */
  modelLabel: string;
  /** Out of quota: listed so the owner sees it, but not pickable. */
  disabled: boolean;
  /** Why it is disabled, in words; null when it can be picked. */
  reason: string | null;
}

/** A heading in the menu, one per installed agent module. */
export interface LaunchGroup {
  module: string;
  name: string;
  choices: LaunchChoice[];
}

/**
 * Each installed agent module that can launch, with the models the install enabled for it (Claude's
 * own first). An enabled id missing from `models` is listed under its raw id; a module with nothing
 * enabled has no group.
 */
export function launchGroups(a: Pick<ModelAssignments, "modules" | "models">): LaunchGroup[] {
  const launchable = a.modules.filter((m: AgentModuleRoles) => m.blocked === null);
  return launchable
    .map((m) => {
      const offered = new Map(a.models.map((x) => [x.id, x]));
      const enabled = [...new Set(m.enabled_models ?? [])].map((id) => offered.get(id) ?? rawModel(id));
      return { module: m.id, name: m.name, choices: enabled.sort(claudeFirst).map((model) => choiceFor(m, model)) };
    })
    .filter((g) => g.choices.length > 0);
}

/** A model the models list does not describe: labelled by its id. */
function rawModel(id: string): SwitchableModel {
  const claude = isClaudeModel(id);
  return {
    id,
    label: id,
    provider: claude ? "anthropic" : (id.includes("/") ? id.slice(0, id.indexOf("/")) : id),
    provider_name: claude ? "Anthropic" : "",
    wire: null,
    failure_pct: 0,
    rated: false,
    degraded: false,
    healthy: true,
    out_of_quota: false,
    reset_at: null,
    reset_unix: null,
  };
}

function claudeFirst(x: SwitchableModel, y: SwitchableModel): number {
  return Number(y.provider === "anthropic") - Number(x.provider === "anthropic");
}

function choiceFor(m: AgentModuleRoles, model: SwitchableModel): LaunchChoice {
  const claude = model.provider === "anthropic";
  return {
    module: m.id,
    moduleName: m.name,
    model: model.id,
    modelLabel: claude ? `${model.label} (subscription)` : model.label,
    disabled: model.out_of_quota,
    reason: model.out_of_quota ? "out of quota" : null,
  };
}

/** The menu's short name for a model, for the pill: `Opus 5.5`, `deepseek-flash`, `Default`. */
export function launchModelName(id: string, models: readonly SwitchableModel[]): string {
  if (!id) return "Default";
  const m = models.find((x) => x.id === id);
  if (!m) return id.includes("/") ? id.slice(id.indexOf("/") + 1) : id;
  if (m.provider === "anthropic") return m.label.replace(/^Claude /, "").replace(/ \(latest\)$/, "");
  return m.label.split(" · ")[0];
}

/**
 * What the pill and the launch dialog say a launch runs on. With no choice made, the install's own
 * agent module and main model; the install's main model is `a.install.roles`' `model` row.
 */
export function launchSummary(sel: LaunchSelection | null, a: Pick<ModelAssignments, "install" | "modules" | "models">): { module: string; name: string; model: string } {
  if (sel) {
    return {
      module: sel.module,
      name: a.modules.find((m) => m.id === sel.module)?.name ?? sel.module,
      model: sel.model ? launchModelName(sel.model, a.models) : "Default",
    };
  }
  const main = a.install.roles.find((r) => r.role === "model")?.value ?? "";
  return {
    module: a.install.module,
    name: a.modules.find((m) => m.id === a.install.module)?.name ?? a.install.module,
    model: launchModelName(main, a.models),
  };
}

/** The menu entry that is checked: the saved choice, or the install's default when none is saved. */
export function checkedChoice(groups: readonly LaunchGroup[], sel: LaunchSelection | null, install: { module: string; model: string }): LaunchChoice | null {
  const find = (module: string, model: string) => groups.find((g) => g.module === module)?.choices.find((c) => c.model === model) ?? null;
  if (sel) return find(sel.module, sel.model ?? "");
  return find(install.module, install.model);
}

/** The launch body fields for a choice. Omitted fields keep the org's default. */
export function launchFields(sel: LaunchSelection | null): { agent_module?: string; model_override?: string; claude_account?: string } {
  if (!sel) return {};
  const fields: { agent_module?: string; model_override?: string; claude_account?: string } = sel.model
    ? { agent_module: sel.module, model_override: sel.model }
    : { agent_module: sel.module };
  if (sel.account && takesClaudeAccount(sel.module, sel.model)) fields.claude_account = sel.account;
  return fields;
}

// The saved choice, read once per page and written through to this browser's storage.
let current: LaunchSelection | null | undefined;
const listeners = new Set<() => void>();

function parse(raw: string | null): LaunchSelection | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Partial<LaunchSelection>;
    if (typeof v.module !== "string" || !v.module) return null;
    const sel: LaunchSelection = { module: v.module, model: typeof v.model === "string" && v.model ? v.model : null };
    if (typeof v.account === "string" && v.account) sel.account = v.account;
    return sel;
  } catch {
    return null;
  }
}

export function getLaunchSelection(): LaunchSelection | null {
  if (current === undefined) current = parse(stored(LAUNCH_AGENT_KEY));
  return current;
}

export function setLaunchSelection(sel: LaunchSelection | null): void {
  current = sel;
  store(LAUNCH_AGENT_KEY, sel ? JSON.stringify(sel) : null);
  for (const listener of listeners) listener();
}

export function subscribeLaunchSelection(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Test-only: forget the cached choice so the next read goes to storage. */
export function resetLaunchSelectionCache(): void {
  current = undefined;
}
