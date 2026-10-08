// The header's model switcher (issue #1051): a chip at the top right naming the install's main
// model with its provider's health, and a popover that hot-switches every model role — install-wide
// ("All orgs") or for one org — for the agent module the scope runs on.
//
// What each role resolves to and where that comes from is GET /api/models/assignments; a switch is
// one POST /api/models/switch, validated as a whole before anything is saved. "Also switch running
// colonies" asks the mothership first (a dry run) how many colonies would restart, then restarts
// them through the quota card's restart path. ⌘K's `/model` opens it too.
//
// Opening it also reads GET /api/models/plans — what is left on each plan in use, with a bar per
// plan and a badge beside every role whose model's plan is out — and GET /api/models/profiles, the
// saved role → model sets a "Use" loads into the draft for the chosen scope and Apply switches to.
//
// The words, the grouping and the request bodies are pure functions, so the tests (no DOM) pin them
// directly and render the panel to static markup from given data.
import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { cx, store, stored } from "../components/ui";
import { untilWords } from "../resetTime";
import type { AutonomyStatus, LeftoverClaude, ModelProvider, ModuleInfo, ModelAssignments, ModelProfile, ModelRoleRow, ModelSource, ModelSwitchReply, ModelSwitchRequest, PlanUsage, SwitchableModel } from "../types";
import { judgeFailing, judgeSaveBody } from "./JudgeModel";
import { ModelSwitcherPanel, type ModelView } from "./ModelMenu";
import { formatResetUtc } from "./ProviderQuotaCard";

/** The window event that opens the switcher (⌘K's `/model`, or anything else that wants it). */
export const SWITCH_MODEL_EVENT = "colonizer:switch-model";

/** Opens the header's model switcher from anywhere in the cockpit. */
export function openModelSwitcher(): void {
  if (typeof window !== "undefined") window.dispatchEvent(new CustomEvent(SWITCH_MODEL_EVENT));
}

/** Whether the ⌘K pane's text is the "Switch model…" command: `/model`, `/models`, or "switch model". */
export function isModelCommand(text: string): boolean {
  const t = text.trim().toLowerCase();
  return /^\/models?$/.test(t) || /^switch models?(…|\.\.\.)?$/.test(t);
}

/** The switcher's scope: every org without an override of its own (the install), or one org. */
export type ModelScope = { kind: "install" } | { kind: "org"; org: string };
export const INSTALL_SCOPE: ModelScope = { kind: "install" };

/** The select value that stands for "no value of this scope's own": an org's inherit, the install's module default. */
export const INHERIT = "__inherit__";

/** `Opus 5.5`, `glm-5`, `Default`: a model's short name for the chip and the recent list. */
export function shortModelName(id: string, models: readonly SwitchableModel[]): string {
  if (!id) return "Default";
  const m = models.find((x) => x.id === id);
  if (!m) return id.includes("/") ? id.slice(id.indexOf("/") + 1) : id;
  if (m.provider === "anthropic") return m.label.replace(/^Claude /, "").replace(/ \(latest\)$/, "");
  return m.label.split(" · ")[0];
}

export type HealthTone = "ok" | "warn" | "err" | "unknown";

/** The dot beside a model: red while its provider is out of quota, amber while degraded, green otherwise. */
export function modelHealth(id: string, models: readonly SwitchableModel[]): HealthTone {
  const m = models.find((x) => x.id === id);
  if (!m) return id ? "unknown" : "ok";
  if (m.out_of_quota) return "err";
  return m.degraded ? "warn" : "ok";
}

export const TONE_DOT: Record<HealthTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", unknown: "bg-faint" };

/** `out of quota until Oct 6, 09:00 UTC`, from the reset's timestamp or its words. */
export function quotaUntil(m: SwitchableModel): string {
  const when = m.reset_unix != null ? formatResetUtc(m.reset_unix) : m.reset_at;
  return when ? `out of quota until ${when}` : "out of quota";
}

/** A picker option: `Claude Opus 5.5`, `ds4-flash · Strix Halo — degraded, 29.4% failing`, `… — out of quota until …`. */
export function modelOptionLabel(m: SwitchableModel): string {
  if (m.out_of_quota) return `${m.label} — ${quotaUntil(m)}`;
  if (m.degraded) return `${m.label} — degraded, ${m.failure_pct}% failing`;
  if (m.rated) return `${m.label} — ${m.failure_pct}% failing`;
  return m.label;
}

export interface ModelGroup {
  provider: string;
  name: string;
  models: SwitchableModel[];
}

/** The models by provider, Claude's own first, each group in offer order. */
export function groupModels(models: readonly SwitchableModel[]): ModelGroup[] {
  const groups: ModelGroup[] = [];
  for (const m of models) {
    let g = groups.find((x) => x.provider === m.provider);
    if (!g) groups.push((g = { provider: m.provider, name: m.provider_name, models: [] }));
    g.models.push(m);
  }
  return groups.sort((a, b) => Number(b.provider === "anthropic") - Number(a.provider === "anthropic"));
}

/** Where a value comes from, as the row says it. */
export function sourceLabel(source: ModelSource, scope: ModelScope): string {
  if (source === "org") return "org override";
  if (source === "install") return scope.kind === "org" ? "install default" : "set install-wide";
  return "module default";
}

export interface ScopeRow extends ModelRoleRow {
  /** Whether this scope can set it: per org, only the roles an org can override. */
  editable: boolean;
}

export interface ScopeView {
  module: string;
  moduleSource: "org" | "install";
  rows: ScopeRow[];
}

/**
 * What the popover shows for a scope: its agent module (the draft's pick, when there is one; `""`
 * is an org back on the install's) and one row per role that module declares, resolved as the
 * mothership resolves them — the org's override, the install's settings for the install's own
 * module, else the module's default.
 */
export function scopeView(a: ModelAssignments, scope: ModelScope, moduleDraft?: string): ScopeView {
  const installRows = a.install.roles;
  const rolesOf = (module: string) => a.modules.find((m) => m.id === module)?.roles ?? [];
  const fromInstall = (module: string): ModelRoleRow[] =>
    rolesOf(module).map((r) => {
      const own = module === a.install.module ? installRows.find((x) => x.role === r.role) : undefined;
      return own ? { ...own } : { ...r, value: "", source: "default" as const };
    });
  if (scope.kind === "install") {
    const module = moduleDraft || a.install.module;
    const rows = module === a.install.module ? installRows : fromInstall(module);
    return { module, moduleSource: "install", rows: rows.map((r) => ({ ...r, editable: true })) };
  }
  const org = a.orgs.find((o) => o.org === scope.org);
  const current = org?.module ?? a.install.module;
  const module = moduleDraft === undefined ? current : moduleDraft || a.install.module;
  const moduleSource = moduleDraft === undefined ? (org?.module_source ?? "install") : moduleDraft ? "org" : "install";
  let rows: ModelRoleRow[];
  if (org && module === current) rows = org.roles;
  else
    rows = fromInstall(module).map((r) => {
      // The org's own overrides stay with it across a module change.
      const own = org?.roles.find((x) => x.role === r.role && x.source === "org");
      return own ? { ...r, value: own.value, source: "org" as const } : r;
    });
  return { module, moduleSource, rows: rows.map((r) => ({ ...r, editable: r.org_settable })) };
}

/** The unsaved edits: the module pick (absent: unchanged) and role → model (`null`: clear). */
export interface ModelDraft {
  module?: string;
  roles: Record<string, string | null>;
}

export const EMPTY_DRAFT: ModelDraft = { roles: {} };

export function draftDirty(draft: ModelDraft): boolean {
  return draft.module !== undefined || Object.keys(draft.roles).length > 0;
}

/** The select's value for a row: the draft's, else the scope's own value, else INHERIT. */
export function rowSelectValue(row: ScopeRow, scope: ModelScope, draft: ModelDraft): string {
  if (row.role in draft.roles) return draft.roles[row.role] ?? INHERIT;
  const own = scope.kind === "org" ? row.source === "org" : row.source === "install";
  return own ? row.value : INHERIT;
}

/** A row's pick: back to the scope's own value drops the edit; INHERIT clears; anything else sets. */
export function pickRole(draft: ModelDraft, row: ScopeRow, scope: ModelScope, value: string): ModelDraft {
  const roles = { ...draft.roles };
  const own = scope.kind === "org" ? row.source === "org" : row.source === "install";
  const current = own ? row.value : INHERIT;
  if (value === current) delete roles[row.role];
  else roles[row.role] = value === INHERIT ? null : value;
  return { ...draft, roles };
}

/** The module pick: back to the scope's current module drops it. Roles the new module lacks go. */
export function pickModule(draft: ModelDraft, a: ModelAssignments, scope: ModelScope, module: string): ModelDraft {
  // An org's "" is "use the install's"; its own pick is the module it names.
  const org = scope.kind === "org" ? a.orgs.find((o) => o.org === scope.org) : undefined;
  const now = scope.kind === "install" ? a.install.module : org?.module_source === "org" ? org.module : "";
  const sameAsNow = module === now;
  const next: ModelDraft = { roles: {} };
  if (!sameAsNow) next.module = module;
  const roles = scopeView(a, scope, next.module).rows.map((r) => r.role);
  for (const [role, value] of Object.entries(draft.roles)) if (roles.includes(role)) next.roles[role] = value;
  return next;
}

/** The switch request for a scope's draft. */
export function switchRequest(scope: ModelScope, draft: ModelDraft, apply: "new" | "running", dryRun = false): ModelSwitchRequest {
  return {
    scope: scope.kind,
    ...(scope.kind === "org" ? { org: scope.org } : null),
    ...(draft.module !== undefined ? { module: draft.module } : null),
    roles: { ...draft.roles },
    apply,
    ...(dryRun ? { dry_run: true } : null),
  };
}

/** The confirm step's question for a running switch. */
export function confirmLine(affected: number): string {
  if (affected === 0) return "No running colony in this scope is on these models; only new colonies change.";
  return affected === 1 ? "Restart 1 running colony on the new models?" : `Restart ${affected} running colonies on the new models?`;
}

/** The toast after a switch: what moved and what restarted. */
export function switchSummary(reply: ModelSwitchReply): string {
  const settings = reply.changes.filter((c) => c.scope !== "colony").length;
  const where = reply.org ?? "all orgs";
  const parts = [settings === 0 ? `nothing changed for ${where}` : `${settings} setting${settings === 1 ? "" : "s"} switched for ${where}`];
  if (reply.colonies.length) parts.push(`${reply.colonies.length} ${reply.colonies.length === 1 ? "colony" : "colonies"} restarting`);
  if (reply.failed.length) parts.push(`${reply.failed.length} could not restart`);
  return parts.join(" · ");
}

/** The role that names the model Claude's roles run on while the account is out (issue #1130). */
export const ACCOUNT_FALLBACK_ROLE = "account_fallback_model";

/**
 * The groups a role's select offers: every model, except for the account fallback, which must be a
 * model on another provider — a Claude model would be out with the account.
 */
export function groupsForRole(role: string, groups: readonly ModelGroup[]): ModelGroup[] {
  return role === ACCOUNT_FALLBACK_ROLE ? groups.filter((g) => g.provider !== "anthropic") : [...groups];
}

/** The "no value" option's words for a row: the account fallback's is "off", not "module default". */
export function inheritOptionLabel(role: string, fallbackLabel: string): string {
  return role === ACCOUNT_FALLBACK_ROLE ? "Off — wait for the reset" : fallbackLabel;
}

/** The line under the account fallback's select, so it reads as what it does. */
export const ACCOUNT_FALLBACK_NOTE =
  "When the Claude plan runs out, roles that use Claude run on this model until the reset, then go back to Claude by themselves. Restricted tasks need a trusted provider.";

/** True when a switch left Claude names in colony or org overrides, and they are not cleared yet. */
export function hasLeftovers(left: LeftoverClaude | null | undefined): left is LeftoverClaude {
  return !!left && !left.cleared && left.colonies.length + left.orgs.length > 0;
}

/** "2 colonies and 1 org override still name a Claude model: opus (2), sonnet (acme)." */
export function leftoverLine(left: LeftoverClaude): string {
  const colonies = left.colonies.length;
  const orgs = left.orgs.length;
  const parts: string[] = [];
  if (colonies) parts.push(`${colonies} ${colonies === 1 ? "colony" : "colonies"}`);
  if (orgs) parts.push(`${orgs} org ${orgs === 1 ? "override" : "overrides"}`);
  const names = [...new Set([...left.colonies.map((c) => c.model), ...left.orgs.map((o) => o.model)])];
  return `${parts.join(" and ")} still ${colonies + orgs === 1 ? "names" : "name"} a Claude model (${names.join(", ")}), which uses the Claude plan.`;
}

/** The request that clears the leftovers a switch reported, for the same scope. */
export function clearLeftoversRequest(scope: ModelScope): ModelSwitchRequest {
  return {
    scope: scope.kind,
    ...(scope.kind === "org" ? { org: scope.org } : null),
    roles: {},
    apply: "new",
    clear_leftovers: true,
  };
}

/** The recent list: model ids, newest first, five at most, kept in this browser only. */
export const RECENT_KEY = "colonizer.models.recent";
export const RECENT_MAX = 5;

export function pushRecent(list: readonly string[], id: string): string[] {
  if (!id) return [...list];
  return [id, ...list.filter((x) => x !== id)].slice(0, RECENT_MAX);
}

export function loadRecent(): string[] {
  try {
    const parsed: unknown = JSON.parse(stored(RECENT_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string").slice(0, RECENT_MAX) : [];
  } catch {
    return [];
  }
}

/** The recent models other than the scope's current main, for one-click switching back. */
export function recentChoices(recent: readonly string[], currentMain: string): string[] {
  return recent.filter((id) => id && id !== currentMain);
}

/**
 * The scope's current selection as a profile's roles: each role's draft pick, else what it resolves
 * to now. `""` is the module default (an inherited role with nothing set).
 */
export function selectionRoles(view: ScopeView, draft: ModelDraft): Record<string, string> {
  const roles: Record<string, string> = {};
  for (const row of view.rows) roles[row.role] = row.role in draft.roles ? (draft.roles[row.role] ?? "") : row.value;
  return roles;
}

/**
 * A profile loaded into the draft for a scope: each role the scope's module declares and the scope
 * can set takes the profile's model (`""` clears it back to the inherited value); the rest are
 * skipped and named, e.g. the install-wide roles under one org. Apply then switches as usual.
 */
export function profileDraft(
  a: ModelAssignments,
  scope: ModelScope,
  draft: ModelDraft,
  profile: Pick<ModelProfile, "roles">,
): { draft: ModelDraft; applied: string[]; skipped: string[] } {
  const view = scopeView(a, scope, draft.module);
  let next: ModelDraft = { ...draft, roles: { ...draft.roles } };
  const applied: string[] = [];
  const skipped: string[] = [];
  for (const [role, model] of Object.entries(profile.roles)) {
    const row = view.rows.find((r) => r.role === role);
    if (!row || !row.editable) {
      skipped.push(row?.title ?? role);
      continue;
    }
    next = pickRole(next, row, scope, model === "" ? INHERIT : model);
    applied.push(role);
  }
  return { draft: next, applied, skipped };
}

/** The note after "Use": what loaded, what the scope could not take, and that Apply switches. */
export function profileNote(name: string, applied: number, skipped: string[]): string {
  const loaded = applied === 0 ? `Nothing in “${name}” applies here` : `Loaded “${name}” — Apply to switch`;
  return skipped.length ? `${loaded}. Skipped (not settable here): ${skipped.join(", ")}.` : `${loaded}.`;
}

/** The badge beside a role whose model's plan is out: "BytePlus out · 2 h 10 min". */
export function roleQuotaBadge(modelId: string, models: readonly SwitchableModel[], nowMs: number = Date.now()): { text: string; title: string } | null {
  const m = models.find((x) => x.id === modelId);
  if (!m?.out_of_quota) return null;
  const name = m.provider === "anthropic" ? "Claude" : m.provider_name;
  const left = m.reset_unix != null && m.reset_unix * 1000 > nowMs ? ` · ${untilWords(m.reset_unix, nowMs)}` : "";
  return { text: `${name} out${left}`, title: `${name} plan exhausted: ${quotaUntil(m)}` };
}

export type Stage = { step: "edit" } | { step: "counting" } | { step: "confirm"; affected: string[] } | { step: "applying" };

export interface ModelSwitcherProps {
  /** For the tests, which render without effects: the assignments in hand and the popover open. */
  initialAssignments?: ModelAssignments;
  initialOpen?: boolean;
  initialScope?: ModelScope;
  initialDraft?: ModelDraft;
  initialApply?: "new" | "running";
  initialStage?: Stage;
  /** For the tests: the menu starts on this list instead of the root. */
  initialView?: ModelView;
  initialRecent?: string[];
  initialPlans?: PlanUsage[];
  initialProfiles?: ModelProfile[];
  initialLeftovers?: LeftoverClaude;
  /** The cockpit's chosen workspace: the popover opens on it. */
  selectedOrg?: string | null;
  /** The autonomy judge's health (the cockpit's poll): a warning on the chip while it is failing. */
  judge?: AutonomyStatus | null;
  /** For the tests: the autonomy module and providers in hand. */
  initialAutonomy?: ModuleInfo;
  initialProviders?: ModelProvider[];
}

export function ModelSwitcher(props: ModelSwitcherProps): ReactElement | null {
  const api = useApi();
  const toast = useToast();
  const [assignments, setAssignments] = useState<ModelAssignments | null>(props.initialAssignments ?? null);
  const [open, setOpen] = useState(props.initialOpen ?? false);
  const [scope, setScope] = useState<ModelScope>(props.initialScope ?? INSTALL_SCOPE);
  const [draft, setDraft] = useState<ModelDraft>(props.initialDraft ?? EMPTY_DRAFT);
  const [apply, setApply] = useState<"new" | "running">(props.initialApply ?? "new");
  const [stage, setStage] = useState<Stage>(props.initialStage ?? { step: "edit" });
  const [error, setError] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>(() => props.initialRecent ?? loadRecent());
  const [plans, setPlans] = useState<PlanUsage[] | null>(props.initialPlans ?? null);
  const [plansError, setPlansError] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<ModelProfile[] | null>(props.initialProfiles ?? null);
  const [profileMessage, setProfileMessage] = useState<{ text: string; tone: "info" | "err" } | null>(null);
  const [profileBusy, setProfileBusy] = useState(false);
  const [leftovers, setLeftovers] = useState<LeftoverClaude | null>(props.initialLeftovers ?? null);
  const [autonomy, setAutonomy] = useState<ModuleInfo | null>(props.initialAutonomy ?? null);
  const [providers, setProviders] = useState<ModelProvider[]>(props.initialProviders ?? []);
  const [judgeStatus, setJudgeStatus] = useState<AutonomyStatus | null>(props.judge ?? null);
  const [judgeSaving, setJudgeSaving] = useState(false);
  const [judgeError, setJudgeError] = useState<{ message: string; model: string } | null>(null);
  const chip = useRef<HTMLButtonElement>(null);

  useEffect(() => setJudgeStatus(props.judge ?? null), [props.judge]);

  const loadJudge = useCallback(() => {
    api.modules().then((list) => setAutonomy(list.find((m) => m.kind === "autonomy") ?? null)).catch(() => {});
    api.providers().then(setProviders).catch(() => {});
    api.autonomyStatus().then(setJudgeStatus).catch(() => {});
  }, [api]);

  const saveJudge = async (model: string, saveAnyway = false) => {
    if (!autonomy) return;
    setJudgeSaving(true);
    setJudgeError(null);
    try {
      const saved = await api.saveModule("autonomy", { ...judgeSaveBody(autonomy, model), ...(saveAnyway ? { save_anyway: true } : {}) });
      setAutonomy(saved);
      toast({ title: "Judge model switched", body: model, kind: "success" });
      api.autonomyStatus().then(setJudgeStatus).catch(() => {});
    } catch (e) {
      // The judge's save runs a live test call; its refusal is shown here with the "Save anyway".
      setJudgeError({ message: errorMessage(e), model });
    } finally {
      setJudgeSaving(false);
    }
  };

  const loadExtras = useCallback(() => {
    setPlansError(null);
    api
      .modelPlans()
      .then((reply) => setPlans(reply.plans))
      .catch((e: unknown) => setPlansError(errorMessage(e)));
    api
      .modelProfiles()
      .then((reply) => setProfiles(reply.profiles))
      .catch((e: unknown) => setProfileMessage({ text: errorMessage(e), tone: "err" }));
  }, [api]);

  const load = useCallback(() => {
    api
      .modelAssignments()
      .then(setAssignments)
      .catch(() => {});
  }, [api]);

  useEffect(() => load(), [load]);
  useEffect(() => {
    if (!open) return;
    load();
    loadExtras();
    loadJudge();
    // The countdowns and balances move while the popover stays open: re-read them every 30 s.
    const timer = window.setInterval(loadExtras, 30_000);
    return () => window.clearInterval(timer);
  }, [open, load, loadExtras, loadJudge]);

  const show = useCallback(() => {
    setOpen(true);
    setError(null);
    setStage({ step: "edit" });
    setDraft(EMPTY_DRAFT);
    setProfileMessage(null);
    setScope(props.selectedOrg ? { kind: "org", org: props.selectedOrg } : INSTALL_SCOPE);
  }, [props.selectedOrg]);

  useEffect(() => {
    window.addEventListener(SWITCH_MODEL_EVENT, show);
    return () => window.removeEventListener(SWITCH_MODEL_EVENT, show);
  }, [show]);

  const remember = (ids: string[]) => {
    setRecent((list) => {
      const next = ids.reduce((acc, id) => pushRecent(acc, id), list);
      store(RECENT_KEY, JSON.stringify(next));
      return next;
    });
  };

  const run = async (body: ModelSwitchRequest) => {
    if (!assignments) return;
    const before = scopeView(assignments, scope).rows.find((r) => r.role === "model")?.value ?? "";
    setStage({ step: "applying" });
    setError(null);
    try {
      const reply = await api.switchModels(body);
      const now = body.roles.model;
      remember(now ? [before, now] : [before]);
      toast({ title: "Models switched", body: switchSummary(reply), kind: reply.failed.length ? "error" : "success" });
      // The Claude names the switch left in overrides stay on screen until cleared or dismissed.
      setLeftovers(hasLeftovers(reply.leftover_claude) ? reply.leftover_claude : null);
      setDraft(EMPTY_DRAFT);
      setStage({ step: "edit" });
      load();
    } catch (e) {
      setError(errorMessage(e));
      setStage({ step: "edit" });
    }
  };

  const onApply = async () => {
    if (apply === "new") return run(switchRequest(scope, draft, "new"));
    setStage({ step: "counting" });
    setError(null);
    try {
      const plan = await api.switchModels(switchRequest(scope, draft, "running", true));
      setStage({ step: "confirm", affected: plan.affected });
    } catch (e) {
      setError(errorMessage(e));
      setStage({ step: "edit" });
    }
  };

  const profileAction = async (action: () => Promise<string>): Promise<boolean> => {
    setProfileBusy(true);
    try {
      const said = await action();
      setProfileMessage({ text: said, tone: "info" });
      const reply = await api.modelProfiles();
      setProfiles(reply.profiles);
      return true;
    } catch (e) {
      setProfileMessage({ text: errorMessage(e), tone: "err" });
      return false;
    } finally {
      setProfileBusy(false);
    }
  };

  if (!assignments) return null;
  const main = assignments.install.roles.find((r) => r.role === "model")?.value ?? "";
  const name = shortModelName(main, assignments.models);
  const tone = modelHealth(main, assignments.models);

  return (
    <div className="relative shrink-0">
      <button
        ref={chip}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Models · main model ${name}${judgeFailing(judgeStatus) ? " · judge failing" : ""}`}
        title="Switch models · /model in ⌘K"
        onClick={() => (open ? setOpen(false) : show())}
        className="inline-flex h-8 max-w-[11rem] cursor-pointer items-center gap-1.5 rounded-full border border-border bg-transparent px-2.5 text-small-lg font-medium text-text transition-colors hover:border-border-strong hover:bg-panel-2"
      >
        <span aria-hidden="true" data-health={tone} className={cx("size-1.5 shrink-0 rounded-full", TONE_DOT[tone])} />
        <span className="truncate">{name}</span>
        {judgeFailing(judgeStatus) && (
          <span data-judge-warning title="The autonomy judge is failing — open to switch it" className="shrink-0 font-semibold text-warn">
            !
          </span>
        )}
        <span aria-hidden="true" className="text-micro-lg text-faint">▾</span>
      </button>
      {open && (
        <ModelSwitcherPanel
          anchor={chip}
          initialView={props.initialView}
          assignments={assignments}
          scope={scope}
          draft={draft}
          apply={apply}
          stage={stage}
          error={error}
          recent={recent}
          onScope={(s) => {
            setScope(s);
            setDraft(EMPTY_DRAFT);
            setStage({ step: "edit" });
            setError(null);
          }}
          onDraft={(d) => {
            setDraft(d);
            setStage({ step: "edit" });
          }}
          onApplyMode={setApply}
          onApply={() => void onApply()}
          onConfirm={() => void run(switchRequest(scope, draft, "running"))}
          onCancel={() => setStage({ step: "edit" })}
          onRecent={(id) => void run(switchRequest(scope, { roles: { model: id } }, "new"))}
          judge={{
            module: autonomy,
            providers,
            status: judgeStatus,
            models: assignments.models,
            saving: judgeSaving,
            error: judgeError?.message ?? null,
            onPick: (model) => void saveJudge(model),
            onSaveAnyway: judgeError ? () => void saveJudge(judgeError.model, true) : undefined,
          }}
          leftovers={leftovers}
          onClearLeftovers={() => void run(clearLeftoversRequest(scope))}
          onDismissLeftovers={() => setLeftovers(null)}
          onClose={() => setOpen(false)}
          plans={plans}
          plansError={plansError}
          profiles={profiles}
          profileNote={profileMessage}
          profileBusy={profileBusy}
          onUseProfile={(profile) => {
            const loaded = profileDraft(assignments, scope, draft, profile);
            setDraft(loaded.draft);
            setStage({ step: "edit" });
            setProfileMessage({ text: profileNote(profile.name, loaded.applied.length, loaded.skipped), tone: "info" });
          }}
          onSaveProfile={(name) =>
            profileAction(async () => {
              const view = scopeView(assignments, scope, draft.module);
              const saved = await api.createModelProfile({ name, module: view.module, roles: selectionRoles(view, draft) });
              return `Saved “${saved.name}” for every device.`;
            })
          }
          onRenameProfile={(profile, name) =>
            profileAction(async () => {
              const renamed = await api.updateModelProfile(profile.id, { name });
              return `Renamed to “${renamed.name}”.`;
            })
          }
          onDeleteProfile={(profile) =>
            profileAction(async () => {
              await api.deleteModelProfile(profile.id);
              return `Deleted “${profile.name}”.`;
            })
          }
        />
      )}
    </div>
  );
}

export { ModelSwitcherPanel };
