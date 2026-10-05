// The header's model switcher (issue #1051): a chip at the top right naming the install's main
// model with its provider's health, and a popover that hot-switches every model role — install-wide
// ("All orgs") or for one org — for the agent module the scope runs on.
//
// What each role resolves to and where that comes from is GET /api/models/assignments; a switch is
// one POST /api/models/switch, validated as a whole before anything is saved. "Also switch running
// colonies" asks the mothership first (a dry run) how many colonies would restart, then restarts
// them through the quota card's restart path. ⌘K's `/model` opens it too.
//
// The words, the grouping and the request bodies are pure functions, so the tests (no DOM) pin them
// directly and render the panel to static markup from given data.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { errorMessage, useApi, useToast } from "../context";
import { cx, store, stored } from "../components/ui";
import type { ModelAssignments, ModelRoleRow, ModelSource, ModelSwitchReply, ModelSwitchRequest, SwitchableModel } from "../types";
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

const TONE_DOT: Record<HealthTone, string> = { ok: "bg-ok", warn: "bg-warn", err: "bg-err", unknown: "bg-faint" };

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

type Stage = { step: "edit" } | { step: "counting" } | { step: "confirm"; affected: string[] } | { step: "applying" };

export interface ModelSwitcherProps {
  /** For the tests, which render without effects: the assignments in hand and the popover open. */
  initialAssignments?: ModelAssignments;
  initialOpen?: boolean;
  initialScope?: ModelScope;
  initialDraft?: ModelDraft;
  initialApply?: "new" | "running";
  initialStage?: Stage;
  initialRecent?: string[];
  /** The cockpit's chosen workspace: the popover opens on it. */
  selectedOrg?: string | null;
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
  const root = useRef<HTMLDivElement>(null);

  const load = useCallback(() => {
    api
      .modelAssignments()
      .then(setAssignments)
      .catch(() => {});
  }, [api]);

  useEffect(() => load(), [load]);
  useEffect(() => {
    if (open) load();
  }, [open, load]);

  const show = useCallback(() => {
    setOpen(true);
    setError(null);
    setStage({ step: "edit" });
    setDraft(EMPTY_DRAFT);
    setScope(props.selectedOrg ? { kind: "org", org: props.selectedOrg } : INSTALL_SCOPE);
  }, [props.selectedOrg]);

  useEffect(() => {
    window.addEventListener(SWITCH_MODEL_EVENT, show);
    return () => window.removeEventListener(SWITCH_MODEL_EVENT, show);
  }, [show]);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !root.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [open]);

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

  if (!assignments) return null;
  const main = assignments.install.roles.find((r) => r.role === "model")?.value ?? "";
  const name = shortModelName(main, assignments.models);
  const tone = modelHealth(main, assignments.models);

  return (
    <div ref={root} className="relative shrink-0">
      <button
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Models · main model ${name}`}
        title="Switch models · /model in ⌘K"
        onClick={() => (open ? setOpen(false) : show())}
        className="inline-flex h-8 max-w-[11rem] cursor-pointer items-center gap-1.5 rounded-full border border-border bg-transparent px-2.5 text-[12.5px] font-medium text-text transition-colors hover:border-border-strong hover:bg-panel-2"
      >
        <span aria-hidden="true" data-health={tone} className={cx("size-1.5 shrink-0 rounded-full", TONE_DOT[tone])} />
        <span className="truncate">{name}</span>
        <span aria-hidden="true" className="text-[10px] text-faint">▾</span>
      </button>
      {open && (
        <ModelSwitcherPanel
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
          onClose={() => setOpen(false)}
        />
      )}
    </div>
  );
}

export interface ModelSwitcherPanelProps {
  assignments: ModelAssignments;
  scope: ModelScope;
  draft: ModelDraft;
  apply: "new" | "running";
  stage: Stage;
  error: string | null;
  recent: string[];
  onScope: (scope: ModelScope) => void;
  onDraft: (draft: ModelDraft) => void;
  onApplyMode: (apply: "new" | "running") => void;
  onApply: () => void;
  onConfirm: () => void;
  onCancel: () => void;
  onRecent: (id: string) => void;
  onClose: () => void;
}

const SOURCE_TONE: Record<ModelSource, string> = {
  org: "bg-accent-soft text-accent",
  install: "bg-panel-2 text-muted",
  default: "bg-panel-2 text-faint",
};

export function ModelSwitcherPanel(p: ModelSwitcherPanelProps): ReactElement {
  const { assignments: a, scope, draft, stage } = p;
  const view = scopeView(a, scope, draft.module);
  const groups = useMemo(() => groupModels(a.models), [a.models]);
  const installModule = a.modules.find((m) => m.id === a.install.module);
  const busy = stage.step === "counting" || stage.step === "applying";
  const mainNow = view.rows.find((r) => r.role === "model")?.value ?? "";
  const recent = recentChoices(p.recent, mainNow);
  const select = "w-full min-w-0 rounded-md border border-border bg-panel-2 px-2 py-1 text-[12.5px] text-text disabled:opacity-60";
  const scopeValue = scope.kind === "org" ? `org:${scope.org}` : "install";
  const moduleValue = scope.kind === "org" && view.moduleSource === "install" ? "" : view.module;
  const blocked = a.modules.find((m) => m.id === view.module)?.blocked ?? null;

  return (
    <div
      role="dialog"
      aria-label="Switch models"
      className="absolute right-0 top-10 z-50 flex max-h-[min(80vh,640px)] w-[380px] flex-col overflow-y-auto rounded-xl border border-border-strong bg-panel p-3 shadow-[0_16px_48px_rgb(0_0_0/0.35)] max-sm:fixed max-sm:inset-x-4 max-sm:top-14 max-sm:w-auto"
    >
      <div className="mb-2 flex items-center justify-between">
        <h2 className="m-0 text-[13px] font-semibold text-text">Models</h2>
        <button type="button" aria-label="Close" onClick={p.onClose} className="cursor-pointer border-0 bg-transparent text-[15px] text-faint hover:text-text">
          ×
        </button>
      </div>

      <label className="mb-2 block text-[11.5px] text-muted">
        Scope
        <select
          aria-label="scope"
          value={scopeValue}
          onChange={(e) => p.onScope(e.target.value === "install" ? INSTALL_SCOPE : { kind: "org", org: e.target.value.slice(4) })}
          className={cx(select, "mt-0.5")}
        >
          <option value="install">All orgs (install default)</option>
          {a.orgs.map((o) => (
            <option key={o.org} value={`org:${o.org}`}>
              {o.org}
              {o.roles.some((r) => r.source === "org") || o.module_source === "org" ? " · overrides" : ""}
            </option>
          ))}
        </select>
      </label>

      <label className="mb-1 block text-[11.5px] text-muted">
        Agent
        <select
          aria-label="agent module"
          value={moduleValue}
          disabled={busy}
          onChange={(e) => p.onDraft(pickModule(draft, a, scope, e.target.value))}
          className={cx(select, "mt-0.5")}
        >
          {scope.kind === "org" && <option value="">Use install default ({installModule?.name ?? a.install.module})</option>}
          {a.modules.map((m) => (
            <option key={m.id} value={m.id} disabled={m.blocked !== null && m.id !== view.module} title={m.blocked ?? undefined}>
              {m.name}
              {m.blocked ? " — can't launch here" : ""}
            </option>
          ))}
        </select>
      </label>
      <p className="m-0 mb-2 text-[11px] text-faint">
        {scope.kind === "org" ? (view.moduleSource === "org" ? "This org's own pick" : "From the install") : "The install's agent module"} · new colonies only
      </p>
      {blocked && <p className="m-0 mb-2 text-[11.5px] text-warn">{blocked}</p>}

      <div role="group" aria-label="model roles" className="space-y-2">
        {view.rows.map((row) => {
          const value = rowSelectValue(row, scope, draft);
          const known = value === INHERIT || a.models.some((m) => m.id === value);
          const inheritLabel =
            scope.kind === "org"
              ? `Use install default${row.source !== "org" && row.value ? ` (${shortModelName(row.value, a.models)})` : ""}`
              : "Module default";
          return (
            <div key={row.role} data-role={row.role}>
              <div className="mb-0.5 flex items-center justify-between gap-2 text-[11.5px]">
                <span className="text-muted">{row.title}</span>
                <span className={cx("rounded-full px-1.5 py-px text-[10.5px]", SOURCE_TONE[row.source])}>
                  {row.role in draft.roles ? "changed" : row.editable ? sourceLabel(row.source, scope) : "install-wide only"}
                </span>
              </div>
              <select
                aria-label={`${row.title} model`}
                value={value}
                disabled={!row.editable || busy}
                onChange={(e) => p.onDraft(pickRole(draft, row, scope, e.target.value))}
                className={select}
              >
                <option value={INHERIT}>{inheritLabel}</option>
                {!known && <option value={value}>{value}</option>}
                {groups.map((g) => (
                  <optgroup key={g.provider} label={g.name}>
                    {g.models.map((m) => (
                      <option key={m.id} value={m.id} disabled={m.out_of_quota}>
                        {modelOptionLabel(m)}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
            </div>
          );
        })}
      </div>

      <fieldset className="m-0 mt-3 border-0 p-0">
        <legend className="mb-1 p-0 text-[11.5px] text-muted">Apply to</legend>
        {(
          [
            ["new", "New colonies only"],
            ["running", "Also switch running colonies"],
          ] as const
        ).map(([mode, label]) => (
          <label key={mode} className="mr-3 inline-flex items-center gap-1.5 text-[12.5px] text-text">
            <input type="radio" name="model-apply" value={mode} checked={p.apply === mode} disabled={busy} onChange={() => p.onApplyMode(mode)} />
            {label}
          </label>
        ))}
      </fieldset>

      {p.error && (
        <p role="alert" className="m-0 mt-2 whitespace-pre-line text-[12px] text-err">
          {p.error}
        </p>
      )}

      {stage.step === "confirm" ? (
        <div role="alertdialog" aria-label="confirm the switch" className="mt-3 rounded-lg border border-border bg-panel-2 p-2">
          <p className="m-0 text-[12.5px] text-text">{confirmLine(stage.affected.length)}</p>
          <div className="mt-2 flex justify-end gap-2">
            <button type="button" onClick={p.onCancel} className="cursor-pointer rounded-md border border-border bg-transparent px-2.5 py-1 text-[12.5px] text-muted hover:text-text">
              Cancel
            </button>
            <button type="button" onClick={p.onConfirm} className="cursor-pointer rounded-md border-0 bg-accent px-2.5 py-1 text-[12.5px] font-semibold text-on-accent hover:brightness-110">
              {stage.affected.length ? `Switch and restart ${stage.affected.length}` : "Switch"}
            </button>
          </div>
        </div>
      ) : (
        <div className="mt-3 flex justify-end">
          <button
            type="button"
            disabled={!draftDirty(draft) || busy}
            onClick={p.onApply}
            className="cursor-pointer rounded-md border-0 bg-accent px-3 py-1.5 text-[12.5px] font-semibold text-on-accent hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {stage.step === "counting" ? "Counting colonies…" : stage.step === "applying" ? "Switching…" : "Apply"}
          </button>
        </div>
      )}

      {recent.length > 0 && (
        <div className="mt-3 border-t border-border pt-2">
          <div className="mb-1 text-[11.5px] text-muted">Recent · main model, new colonies</div>
          <div className="flex flex-wrap gap-1.5">
            {recent.map((id) => {
              const m = a.models.find((x) => x.id === id);
              return (
                <button
                  key={id}
                  type="button"
                  disabled={busy || m?.out_of_quota === true}
                  title={m?.out_of_quota ? quotaUntil(m) : `Switch the main model to ${id}`}
                  onClick={() => p.onRecent(id)}
                  className="inline-flex cursor-pointer items-center gap-1 rounded-full border border-border bg-transparent px-2 py-0.5 text-[12px] text-text hover:border-border-strong disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <span aria-hidden="true" className={cx("size-1.5 rounded-full", TONE_DOT[modelHealth(id, a.models)])} />
                  {shortModelName(id, a.models)}
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
