// The model menu (issue #1228): the header chip's panel, rebuilt as a compact Spotlight. A search box
// ("Switch model…"), then Roles (orchestrator, helper, background, judge: each row its current model
// and a health dot), Providers (what is left on each plan, and when it resets) and Quick switches
// ("Everything to MiniMax", "Use Claude until it resets"), with the saved profiles and the models you
// used lately beneath. ↵ on a role opens the model list for it; picking one edits the draft, and the
// card at the foot applies it (new colonies, or the running ones too after a counted confirm) through
// the same POST /api/models/switch as before. The judge saves on pick through PUT /api/modules/autonomy
// with its other settings merged in, never replaced.
import { useMemo, useState, type ReactElement, type ReactNode, type RefObject } from "react";

import { cx } from "../components/ui";
import type { LeftoverClaude, ModelAssignments, ModelProfile, PlanUsage, SwitchableModel } from "../types";
import { JudgeSectionProps, currentJudgeModel, judgeAlternative, judgeFailing, judgeOptions, judgeTone, judgeToneWords } from "./JudgeModel";
import { planView, sortPlans, type PlanTone } from "./ModelPlans";
import { profileSummary } from "./ModelProfiles";
import {
  ACCOUNT_FALLBACK_NOTE,
  ACCOUNT_FALLBACK_ROLE,
  INHERIT,
  INSTALL_SCOPE,
  confirmLine,
  draftDirty,
  groupModels,
  groupsForRole,
  hasLeftovers,
  inheritOptionLabel,
  leftoverLine,
  modelHealth,
  modelOptionLabel,
  pickModule,
  pickRole,
  quotaUntil,
  recentChoices,
  roleQuotaBadge,
  rowSelectValue,
  scopeView,
  shortModelName,
  sourceLabel,
  type HealthTone,
  type ModelDraft,
  type ModelScope,
  type ScopeRow,
  type Stage,
} from "./ModelSwitcher";
import { DotTile, SpotlightPanel, type PanelRow, type PanelSection } from "./spotlight/Panel";
import { Key, MOD } from "./spotlight/Parts";

const TONE_COLOR: Record<HealthTone, string> = { ok: "var(--ok)", warn: "var(--warn)", err: "var(--err)", unknown: "var(--faint)" };
const PLAN_COLOR: Record<PlanTone, string> = { ok: "text-ok", warn: "text-warn", err: "text-err", unknown: "text-faint" };

/** Where the menu is: the root list, or a list picking one thing. */
export type ModelView =
  | { kind: "root" }
  | { kind: "role"; role: string }
  | { kind: "provider"; provider: string }
  | { kind: "judge" }
  | { kind: "scope" }
  | { kind: "module" }
  | { kind: "profiles" }
  | { kind: "profile-save" }
  | { kind: "profile-rename"; id: string }
  | { kind: "profile-delete"; id: string };

export interface ModelSwitcherPanelProps {
  anchor?: RefObject<HTMLElement | null>;
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
  /** Claude names the last switch left in colony and org overrides (issue #1130), with the option to clear them. */
  leftovers?: LeftoverClaude | null;
  onClearLeftovers?: () => void;
  onDismissLeftovers?: () => void;
  /** Plan usage (GET /api/models/plans): null while loading. */
  plans?: PlanUsage[] | null;
  plansError?: string | null;
  /** Saved profiles and starters (GET /api/models/profiles): null while loading. */
  profiles?: ModelProfile[] | null;
  profileNote?: { text: string; tone: "info" | "err" } | null;
  profileBusy?: boolean;
  onUseProfile?: (profile: ModelProfile) => void;
  onSaveProfile?: (name: string) => Promise<boolean>;
  onRenameProfile?: (profile: ModelProfile, name: string) => Promise<boolean>;
  onDeleteProfile?: (profile: ModelProfile) => Promise<boolean>;
  /** The judge row (issue #1201): the autonomy module's model, saved on pick. Absent, no row. */
  judge?: JudgeSectionProps;
  /** For the tests: where the menu starts, and the clock the countdowns read. */
  initialView?: ModelView;
  nowMs?: number;
}

/** The model a role runs on in the draft: its pick, else what it resolves to now. */
function effectiveValue(row: ScopeRow, scope: ModelScope, draft: ModelDraft): string {
  const value = rowSelectValue(row, scope, draft);
  return value === INHERIT ? row.value : value;
}

/** What the draft changes, as a line per role: "Orchestrator model → Opus 5.5". */
export function draftLines(view: { rows: ScopeRow[] }, draft: ModelDraft, models: readonly SwitchableModel[], scope: ModelScope): string[] {
  return view.rows.filter((r) => r.role in draft.roles).map((r) => `${r.title} → ${draft.roles[r.role] === null ? "inherited" : shortModelName(rowSelectValue(r, scope, draft), models)}`);
}

/** A role's pick as the draft the Apply card will send: only that role changes, every other pick stays. */
export function roleDraft(view: { rows: ScopeRow[] }, scope: ModelScope, draft: ModelDraft, role: string, value: string): ModelDraft {
  const row = view.rows.find((r) => r.role === role);
  return row && row.editable ? pickRole(draft, row, scope, value) : draft;
}

/** The roles a quick switch moves: every one this scope can set except the account fallback. */
export function quickDraft(view: { rows: ScopeRow[] }, scope: ModelScope, draft: ModelDraft, model: string): ModelDraft {
  let next = draft;
  for (const row of view.rows) if (row.editable && row.role !== ACCOUNT_FALLBACK_ROLE) next = pickRole(next, row, scope, model);
  return next;
}

/** The first model of a provider that can be picked now. */
export function firstUsable(models: readonly SwitchableModel[], provider: string): SwitchableModel | null {
  return models.find((m) => m.provider === provider && !m.out_of_quota) ?? null;
}

export function ModelSwitcherPanel(p: ModelSwitcherPanelProps): ReactElement {
  const { assignments: a, scope, draft, stage } = p;
  const [view, setView] = useState<ModelView>(p.initialView ?? { kind: "root" });
  const [query, setQuery] = useState("");
  const scoped = scopeView(a, scope, draft.module);
  const groups = useMemo(() => groupModels(a.models), [a.models]);
  const busy = stage.step === "counting" || stage.step === "applying";
  const mainNow = scoped.rows.find((r) => r.role === "model")?.value ?? "";
  const recent = recentChoices(p.recent, mainNow);
  const q = query.trim().toLowerCase();
  const dirty = draftDirty(draft);
  const blocked = a.modules.find((m) => m.id === scoped.module)?.blocked ?? null;
  const installModule = a.modules.find((m) => m.id === a.install.module);
  const nowMs = p.nowMs;

  const go = (next: ModelView, text = "") => {
    setView(next);
    setQuery(text);
  };
  const back = () => go({ kind: "root" });
  const fits = (...parts: (string | undefined)[]) => q === "" || parts.some((x) => x?.toLowerCase().includes(q));

  const modelRow = (m: SwitchableModel, current: boolean, onPick: () => void): PanelRow => ({
    id: `model:${m.id}`,
    title: shortModelName(m.id, a.models),
    subtitle: m.out_of_quota ? quotaUntil(m) : modelOptionLabel(m).replace(`${m.label} — `, "") === m.label ? m.provider_name : `${m.provider_name} · ${modelOptionLabel(m).replace(`${m.label} — `, "")}`,
    leading: <DotTile color={TONE_COLOR[modelHealth(m.id, a.models)]} />,
    checked: current,
    disabled: m.out_of_quota,
    verb: "use this model",
    onPick,
  });

  // --- Lists ------------------------------------------------------------------------------------

  const sections: PanelSection[] = [];
  let placeholder = "Switch model…";
  let title: string | null = null;
  let body: ReactNode | undefined;
  let empty: ReactNode = "Nothing matches.";

  if (view.kind === "root") {
    const roleRows: PanelRow[] = scoped.rows
      .filter((r) => fits(r.title, r.role, shortModelName(effectiveValue(r, scope, draft), a.models)))
      .map((r) => {
        const value = effectiveValue(r, scope, draft);
        const badge = roleQuotaBadge(value, a.models, nowMs);
        const changed = r.role in draft.roles;
        return {
          id: `role:${r.role}`,
          title: r.title,
          subtitle: (
            [
              value ? shortModelName(value, a.models) : r.role === ACCOUNT_FALLBACK_ROLE ? "Off" : "Module default",
              changed ? `was ${r.value ? shortModelName(r.value, a.models) : "module default"}` : !r.editable ? "install-wide only" : value ? sourceLabel(r.source, scope) : null,
            ]
              .filter(Boolean)
              .join(" · ")
          ),
          leading: <DotTile color={TONE_COLOR[modelHealth(value, a.models)]} />,
          trailing: badge ? (
            <span data-quota-badge title={badge.title} className="rounded-full bg-err-soft px-2 py-0.5 text-meta-lg font-semibold text-err">
              {badge.text}
            </span>
          ) : changed ? (
            <span className="rounded-full bg-accent-soft px-2 py-0.5 text-meta-lg font-medium text-accent">changed</span>
          ) : undefined,
          disabled: !r.editable,
          verb: "choose",
          onPick: () => go({ kind: "role", role: r.role }),
        } satisfies PanelRow;
      });
    if (p.judge?.module && fits("judge", currentJudgeModel(p.judge.module))) {
      const tone = judgeTone(p.judge.status);
      const current = currentJudgeModel(p.judge.module);
      roleRows.push({
        id: "role:judge",
        title: "Judge",
        subtitle: `${current ? shortModelName(current, a.models) : "No model set"} · answers colonies' questions for you`,
        leading: <DotTile color={TONE_COLOR[tone]} />,
        trailing: (
          <span data-health={tone} className={cx("text-meta-lg", tone === "err" ? "font-semibold text-err" : tone === "warn" ? "text-warn" : "")}>
            {judgeToneWords(tone)}
          </span>
        ),
        verb: "choose",
        onPick: () => go({ kind: "judge" }),
      });
    }
    sections.push({ id: "roles", title: "Roles", aside: scoped.moduleSource === "org" && scope.kind === "org" ? "this org's own agent" : undefined, rows: roleRows });

    // A model typed in the box: set as the main one.
    if (q) {
      const mainRow = scoped.rows.find((r) => r.role === "model" && r.editable);
      const hits = a.models.filter((m) => fits(m.label, m.id, m.provider_name));
      if (mainRow && hits.length)
        sections.push({
          id: "models",
          title: `Set ${mainRow.title.toLowerCase()}`,
          rows: hits.slice(0, 8).map((m) =>
            modelRow(m, rowSelectValue(mainRow, scope, draft) === m.id, () => {
              p.onDraft(pickRole(draft, mainRow, scope, m.id));
              back();
            }),
          ),
        });
    }

    // Providers: what is left on each plan.
    const providerRows: PanelRow[] = sortPlans(p.plans ?? [])
      .filter((plan) => fits(plan.name, plan.id))
      .map((plan) => {
        const v = planView(plan, nowMs);
        return {
          id: `provider:${plan.id}`,
          title: plan.name,
          subtitle: [v.usedBy, v.details[0]].filter(Boolean).join(" · "),
          leading: <DotTile color={TONE_COLOR[v.tone]} />,
          trailing: (
            <span className={cx("flex items-center gap-2 tabular-nums", PLAN_COLOR[v.tone])}>
              {v.usedPct != null && (
                <span role="meter" aria-label={`${plan.name} plan used`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={v.usedPct} className="h-1.5 w-14 overflow-hidden rounded-full bg-panel-3 max-sm:hidden">
                  <span className="block h-full rounded-full bg-current" style={{ width: `${Math.max(v.usedPct, 3)}%` }} />
                </span>
              )}
              <span data-plan={plan.id} className={cx("text-meta-lg", v.tone === "err" && "font-semibold")}>{v.figure}</span>
            </span>
          ),
          verb: "see its models",
          onPick: () => go({ kind: "provider", provider: plan.id }),
        } satisfies PanelRow;
      });
    if (p.plansError) providerRows.push({ id: "provider:error", title: "Plan usage unavailable", subtitle: p.plansError, leading: <DotTile color="var(--faint)" />, disabled: true, onPick: () => {} });
    else if (p.plans === null || p.plans === undefined) providerRows.push({ id: "provider:loading", title: "Reading plan usage…", leading: <DotTile color="var(--faint)" />, disabled: true, onPick: () => {} });
    sections.push({ id: "providers", title: "Providers", aside: p.plans?.some((x) => x.exhausted) ? `${p.plans.filter((x) => x.exhausted).length} out` : undefined, rows: providerRows });

    // Quick switches.
    const quick: PanelRow[] = [];
    const claude = firstUsable(a.models, "anthropic");
    const out = (p.plans ?? []).filter((x) => x.exhausted && x.id !== "anthropic");
    const quickRow = (id: string, label: string, sub: string, model: SwitchableModel): PanelRow => ({
      id,
      title: label,
      subtitle: sub,
      leading: <DotTile color={TONE_COLOR[modelHealth(model.id, a.models)]} />,
      verb: "load into the draft",
      onPick: () => {
        p.onDraft(quickDraft(scoped, scope, draft, model.id));
        back();
      },
    });
    for (const g of groups.filter((x) => x.provider !== "anthropic").slice(0, 3)) {
      const m = firstUsable(a.models, g.provider);
      if (m) quick.push(quickRow(`quick:${g.provider}`, `Everything to ${g.name}`, `Every role on ${shortModelName(m.id, a.models)}`, m));
    }
    if (claude)
      quick.push(
        quickRow(
          "quick:claude",
          out.length ? `Use Claude until ${out[0].name} resets` : "Everything to Claude",
          out.length ? `${out[0].name} is out · ${shortModelName(claude.id, a.models)} meanwhile` : `Every role on ${shortModelName(claude.id, a.models)}`,
          claude,
        ),
      );
    sections.push({ id: "quick", title: "Quick switches", rows: quick.filter((r) => fits(typeof r.title === "string" ? r.title : "")) });

    // Profiles and recents.
    const profileRows: PanelRow[] = (p.profiles ?? []).filter((x) => fits(x.name)).slice(0, q ? 8 : 3).map((x) => ({
      id: `profile:${x.id}`,
      title: x.name,
      subtitle: profileSummary(x),
      leading: <DotTile color="var(--accent)" />,
      hint: x.builtin ? "starter" : undefined,
      verb: "load into the draft",
      onPick: () => p.onUseProfile?.(x),
    }));
    if (!q) profileRows.push({ id: "profiles:manage", title: "Profiles…", subtitle: "Save this selection, rename or delete a profile", leading: <DotTile color="var(--faint)" />, verb: "open", onPick: () => go({ kind: "profiles" }) });
    sections.push({ id: "profiles", title: "Profiles", rows: profileRows });
    sections.push({
      id: "recent",
      title: "Recent · main model, new colonies",
      rows: recent
        .filter((id) => fits(shortModelName(id, a.models), id))
        .map((id) => {
          const m = a.models.find((x) => x.id === id);
          return {
            id: `recent:${id}`,
            title: shortModelName(id, a.models),
            subtitle: m?.out_of_quota ? quotaUntil(m) : `Switch the main model to ${id}`,
            leading: <DotTile color={TONE_COLOR[modelHealth(id, a.models)]} />,
            disabled: busy || m?.out_of_quota === true,
            verb: "switch the main model",
            onPick: () => p.onRecent(id),
          } satisfies PanelRow;
        }),
    });
    empty = `Nothing matches “${query.trim()}”.`;
  } else if (view.kind === "role") {
    const row = scoped.rows.find((r) => r.role === view.role);
    title = row?.title ?? view.role;
    placeholder = "Search models…";
    if (row) {
      const value = rowSelectValue(row, scope, draft);
      const known = value === INHERIT || a.models.some((m) => m.id === value);
      const inheritLabel = inheritOptionLabel(
        row.role,
        scope.kind === "org" ? `Use install default${row.source !== "org" && row.value ? ` (${shortModelName(row.value, a.models)})` : ""}` : "Module default",
      );
      const choose = (v: string) => {
        p.onDraft(roleDraft(scoped, scope, draft, row.role, v));
        back();
      };
      if (fits(inheritLabel)) sections.push({ id: "inherit", title, rows: [{ id: "inherit", title: inheritLabel, leading: <DotTile color="var(--faint)" />, checked: value === INHERIT, verb: "choose", onPick: () => choose(INHERIT) }] });
      if (!known && fits(value)) sections.push({ id: "unknown", rows: [{ id: `model:${value}`, title: value, leading: <DotTile color="var(--faint)" />, checked: true, verb: "keep", onPick: () => choose(value) }] });
      for (const g of groupsForRole(row.role, groups)) {
        sections.push({ id: `g:${g.provider}`, title: g.name, rows: g.models.filter((m) => fits(m.label, m.id, g.name)).map((m) => modelRow(m, value === m.id, () => choose(m.id))) });
      }
      if (row.role === ACCOUNT_FALLBACK_ROLE) body = undefined;
    }
  } else if (view.kind === "provider") {
    const g = groups.find((x) => x.provider === view.provider);
    title = g?.name ?? view.provider;
    placeholder = `Search ${title} models…`;
    const mainRow = scoped.rows.find((r) => r.role === "model" && r.editable);
    sections.push({
      id: "provider-models",
      title: `Set ${(mainRow?.title ?? "main model").toLowerCase()}`,
      rows: (g?.models ?? []).filter((m) => fits(m.label, m.id)).map((m) =>
        modelRow(m, !!mainRow && rowSelectValue(mainRow, scope, draft) === m.id, () => {
          if (mainRow) p.onDraft(pickRole(draft, mainRow, scope, m.id));
          back();
        }),
      ),
    });
    empty = "This provider offers no model to this agent.";
  } else if (view.kind === "judge" && p.judge?.module) {
    title = "Judge";
    placeholder = "Search judge models…";
    const current = currentJudgeModel(p.judge.module);
    const options = judgeOptions(p.judge.providers);
    const known = !current || options.some((o) => o.id === current);
    if (!known) sections.push({ id: "judge-current", rows: [{ id: `judge:${current}`, title: current, leading: <DotTile color="var(--faint)" />, checked: true, onPick: back }] });
    for (const g of [...new Set(options.map((o) => o.group))]) {
      sections.push({
        id: `jg:${g}`,
        title: g,
        rows: options
          .filter((o) => o.group === g && fits(o.label, o.id))
          .map((o) => ({
            id: `judge:${o.id}`,
            title: o.label,
            leading: <DotTile color={TONE_COLOR[modelHealth(o.id, a.models)]} />,
            checked: o.id === current,
            disabled: p.judge?.saving,
            verb: "save as judge",
            onPick: () => {
              p.judge?.onPick(o.id);
              back();
            },
          })),
      });
    }
    empty = "No provider is configured for the judge yet.";
  } else if (view.kind === "scope") {
    title = "Scope";
    placeholder = "Search scopes…";
    const orgRows: PanelRow[] = [
      { id: "scope:install", title: "All orgs", subtitle: "The install default", leading: <DotTile color="var(--accent)" />, checked: scope.kind === "install", verb: "choose", onPick: () => (p.onScope(INSTALL_SCOPE), back()) },
      ...a.orgs.map((o) => ({
        id: `scope:${o.org}`,
        title: o.org,
        subtitle: o.roles.some((r) => r.source === "org") || o.module_source === "org" ? "has overrides" : "uses the install default",
        leading: <DotTile color="var(--faint)" />,
        checked: scope.kind === "org" && scope.org === o.org,
        verb: "choose",
        onPick: () => (p.onScope({ kind: "org", org: o.org }), back()),
      })),
    ];
    sections.push({ id: "scopes", title: "Where the switch applies", rows: orgRows.filter((r) => fits(typeof r.title === "string" ? r.title : "")) });
  } else if (view.kind === "module") {
    title = "Agent";
    placeholder = "Search agents…";
    const rows: PanelRow[] = [];
    if (scope.kind === "org") rows.push({ id: "module:", title: `Use install default (${installModule?.name ?? a.install.module})`, leading: <DotTile color="var(--faint)" />, checked: scoped.moduleSource === "install", verb: "choose", onPick: () => (p.onDraft(pickModule(draft, a, scope, "")), back()) });
    for (const m of a.modules)
      rows.push({
        id: `module:${m.id}`,
        title: m.name,
        subtitle: m.blocked ?? undefined,
        leading: <DotTile color={m.blocked ? "var(--err)" : "var(--ok)"} />,
        checked: m.id === scoped.module && !(scope.kind === "org" && scoped.moduleSource === "install"),
        disabled: m.blocked !== null && m.id !== scoped.module,
        trailing: m.blocked ? <span className="text-meta-lg text-err">can&apos;t launch here</span> : undefined,
        verb: "choose",
        onPick: () => (p.onDraft(pickModule(draft, a, scope, m.id)), back()),
      });
    sections.push({ id: "modules", title: "Agent module · new colonies only", rows: rows.filter((r) => fits(typeof r.title === "string" ? r.title : "")) });
  } else if (view.kind === "profiles") {
    title = "Profiles";
    placeholder = "Search profiles…";
    const rows: PanelRow[] = (p.profiles ?? [])
      .filter((x) => fits(x.name))
      .map((x) => ({
        id: `profile:${x.id}`,
        title: x.name,
        subtitle: profileSummary(x),
        leading: <DotTile color="var(--accent)" />,
        hint: x.builtin ? "starter" : undefined,
        actions: x.builtin ? undefined : (
          <>
            <button type="button" disabled={p.profileBusy} onClick={() => go({ kind: "profile-rename", id: x.id }, x.name)} className="h-7 cursor-pointer rounded-lg border-0 bg-transparent px-2 text-small text-muted hover:bg-panel-3 hover:text-text">
              Rename
            </button>
            <button type="button" disabled={p.profileBusy} onClick={() => go({ kind: "profile-delete", id: x.id })} className="h-7 cursor-pointer rounded-lg border-0 bg-transparent px-2 text-small text-muted hover:bg-panel-3 hover:text-err">
              Delete
            </button>
          </>
        ),
        verb: "load into the draft",
        onPick: () => (p.onUseProfile?.(x), back()),
      }));
    sections.push({
      id: "profile-save",
      rows: [{ id: "profile:new", title: "Save the current selection…", subtitle: "Every device sees it", leading: <DotTile color="var(--accent)" />, disabled: scoped.rows.length === 0, primary: true, verb: "name it", onPick: () => go({ kind: "profile-save" }) }],
    });
    sections.push({ id: "profile-list", title: "Saved and starters", rows });
    empty = p.profiles === null ? "Loading profiles…" : "No profiles yet.";
  } else if (view.kind === "profile-save" || view.kind === "profile-rename") {
    const target = view.kind === "profile-rename" ? (p.profiles ?? []).find((x) => x.id === view.id) : undefined;
    title = view.kind === "profile-save" ? "Save as profile" : `Rename “${target?.name ?? ""}”`;
    placeholder = view.kind === "profile-save" ? "Name this selection…" : "New name…";
    const name = query.trim();
    sections.push({
      id: "profile-name",
      rows: [
        {
          id: "profile:confirm",
          title: view.kind === "profile-save" ? (name ? `Save as “${name}”` : "Type a name") : name ? `Rename to “${name}”` : "Type a name",
          leading: <DotTile color="var(--accent)" />,
          primary: true,
          disabled: name === "" || p.profileBusy,
          verb: "save",
          onPick: () => {
            const run = view.kind === "profile-save" ? p.onSaveProfile?.(name) : target ? p.onRenameProfile?.(target, name) : undefined;
            void run?.then((ok) => ok && go({ kind: "profiles" }));
          },
        },
      ],
    });
  } else if (view.kind === "profile-delete") {
    const target = (p.profiles ?? []).find((x) => x.id === view.id);
    title = "Delete profile";
    sections.push({
      id: "profile-delete",
      title: target ? `Delete “${target.name}” on every device?` : "Profile not found",
      rows: target
        ? [
            { id: "profile:keep", title: "Keep it", leading: <DotTile color="var(--faint)" />, verb: "keep", onPick: () => go({ kind: "profiles" }) },
            {
              id: "profile:delete",
              title: "Delete",
              leading: <DotTile color="var(--err)" />,
              disabled: p.profileBusy,
              verb: "delete",
              onPick: () => void p.onDeleteProfile?.(target).then((ok) => ok && go({ kind: "profiles" })),
            },
          ]
        : [],
    });
  }

  // --- Chips and the dock -------------------------------------------------------------------------

  const chips =
    view.kind === "root" ? (
      <div className="flex flex-wrap items-center gap-2 px-4 pb-3 sm:px-5">
        <button type="button" className="spot-chip" aria-label={`scope · ${scope.kind === "org" ? scope.org : "All orgs"}`} onClick={() => go({ kind: "scope" })}>
          <span className="text-faint">Scope</span> {scope.kind === "org" ? scope.org : "All orgs"} <span aria-hidden="true">▾</span>
        </button>
        <button type="button" className="spot-chip" aria-label={`agent · ${a.modules.find((m) => m.id === scoped.module)?.name ?? scoped.module}`} onClick={() => go({ kind: "module" })}>
          <span className="text-faint">Agent</span> {a.modules.find((m) => m.id === scoped.module)?.name ?? scoped.module} <span aria-hidden="true">▾</span>
        </button>
        <span className="ml-auto text-meta-lg text-faint max-sm:hidden">{scoped.moduleSource === "org" && scope.kind === "org" ? "this org's own pick" : scope.kind === "org" ? "from the install" : "install default"} · new colonies only</span>
      </div>
    ) : undefined;

  const lines = draftLines(scoped, draft, a.models, scope);
  const judge = p.judge;
  const alternative = judge?.module && judgeFailing(judge.status) ? judgeAlternative(judgeOptions(judge.providers), currentJudgeModel(judge.module), a.models) : null;
  const dock: ReactElement | undefined =
    view.kind !== "root" && view.kind !== "judge" ? undefined : (
      <div>
        {blocked && view.kind === "root" && <p className="m-0 px-5 pb-2 text-meta-lg text-warn">{blocked}</p>}
        {p.profileNote && view.kind === "root" && (
          <p role={p.profileNote.tone === "err" ? "alert" : "status"} className={cx("m-0 px-5 pb-2 text-meta-lg", p.profileNote.tone === "err" ? "text-err" : "text-muted")}>
            {p.profileNote.text}
          </p>
        )}
        {judge && (alternative || judge.error) && (
          <div data-judge className="spot-card">
            {alternative && (
              <p data-judge-suggestion className="m-0 text-small-lg text-warn">
                The judge is failing.{" "}
                <button type="button" disabled={judge.saving} onClick={() => judge.onPick(alternative.id)} className="cursor-pointer border-0 bg-transparent p-0 font-medium text-accent underline-offset-2 hover:underline disabled:opacity-50">
                  Switch to {alternative.id}
                </button>
              </p>
            )}
            {judge.error && (
              <p role="alert" className="m-0 whitespace-pre-line text-small text-err">
                {judge.error}{" "}
                {judge.onSaveAnyway && (
                  <button type="button" onClick={judge.onSaveAnyway} className="cursor-pointer border-0 bg-transparent p-0 font-medium text-accent underline-offset-2 hover:underline">
                    Save anyway
                  </button>
                )}
              </p>
            )}
          </div>
        )}
        {hasLeftovers(p.leftovers) && (
          <div role="status" data-leftovers className="spot-card !border-warn !bg-warn-soft text-small-lg text-warn">
            <p className="m-0">{leftoverLine(p.leftovers)}</p>
            <ul className="m-0 mt-1 list-none space-y-0.5 p-0 text-meta-lg">
              {p.leftovers.colonies.map((c) => (
                <li key={`${c.id}:${c.role}`}>
                  colony {c.id} · {c.role} {c.model}
                </li>
              ))}
              {p.leftovers.orgs.map((o) => (
                <li key={`${o.org}:${o.role}`}>
                  org {o.org} · {o.role} {o.model}
                </li>
              ))}
            </ul>
            <div className="mt-2 flex justify-end gap-2">
              <button type="button" onClick={p.onDismissLeftovers} className="spot-btn spot-btn-quiet">
                Leave them
              </button>
              <button type="button" disabled={busy} onClick={p.onClearLeftovers} className="spot-btn">
                Clear these too
              </button>
            </div>
          </div>
        )}
        {p.error && (
          <p role="alert" className="m-0 whitespace-pre-line px-5 pb-2 text-small text-err">
            {p.error}
          </p>
        )}
        {dirty && view.kind === "root" && (
          <div className="spot-card" data-apply-card>
            {stage.step === "confirm" ? (
              <div role="alertdialog" aria-label="confirm the switch">
                <p className="m-0 text-small-lg text-text">{confirmLine(stage.affected.length)}</p>
                <div className="mt-2.5 flex justify-end gap-2">
                  <button type="button" onClick={p.onCancel} className="spot-btn spot-btn-quiet">
                    Cancel
                  </button>
                  <button type="button" onClick={p.onConfirm} className="spot-btn">
                    {stage.affected.length ? `Switch and restart ${stage.affected.length}` : "Switch"}
                  </button>
                </div>
              </div>
            ) : (
              <>
                <ul className="m-0 mb-2.5 list-none space-y-0.5 p-0 text-small-lg text-text">
                  {draft.module !== undefined && <li>Agent → {a.modules.find((m) => m.id === scoped.module)?.name ?? scoped.module}</li>}
                  {lines.map((l) => (
                    <li key={l}>{l}</li>
                  ))}
                </ul>
                <div className="flex flex-wrap items-center gap-2">
                  <div role="radiogroup" aria-label="apply to" className="flex gap-1.5">
                    {(
                      [
                        ["new", "New colonies only"],
                        ["running", "Also running colonies"],
                      ] as const
                    ).map(([mode, label]) => (
                      <button key={mode} type="button" role="radio" aria-checked={p.apply === mode} data-on={p.apply === mode} disabled={busy} onClick={() => p.onApplyMode(mode)} className="spot-chip">
                        {label}
                      </button>
                    ))}
                  </div>
                  <span className="ml-auto flex items-center gap-2">
                    <button type="button" disabled={busy} onClick={() => p.onDraft({ roles: {} })} className="spot-btn spot-btn-quiet">
                      Discard
                    </button>
                    <button type="button" disabled={busy} onClick={p.onApply} className="spot-btn">
                      {stage.step === "counting" ? "Counting colonies…" : stage.step === "applying" ? "Switching…" : "Apply"}
                      {stage.step === "edit" && <Key className="!bg-white/20 !text-on-accent border-transparent">{MOD}↵</Key>}
                    </button>
                  </span>
                </div>
              </>
            )}
          </div>
        )}
      </div>
    );

  const roleNote = view.kind === "role" && view.role === ACCOUNT_FALLBACK_ROLE ? <p className="m-0 px-5 pb-2 text-meta-lg leading-snug text-faint">{ACCOUNT_FALLBACK_NOTE}</p> : null;
  const profileBack = view.kind.startsWith("profile-") ? { kind: "profiles" as const } : null;

  return (
    <SpotlightPanel
      label="Switch models"
      placement="anchored"
      anchor={p.anchor}
      align="end"
      width={480}
      onClose={p.onClose}
      onEscape={() => {
        if (view.kind === "root") return false;
        go(profileBack ?? { kind: "root" });
        return true;
      }}
      query={query}
      onQuery={setQuery}
      placeholder={placeholder}
      fieldLabel={view.kind === "root" ? "Switch model" : placeholder.replace(/…$/, "")}
      icon={
        view.kind === "root" ? undefined : (
          <button type="button" aria-label="back" onClick={() => go(profileBack ?? { kind: "root" })} className="grid size-6 cursor-pointer place-items-center rounded-md border-0 bg-transparent p-0 text-muted hover:text-text">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        )
      }
      below={
        <>
          {chips}
          {roleNote}
        </>
      }
      sections={sections}
      empty={empty}
      body={body}
      dock={dock}
      resetOn={`${view.kind}:${"role" in view ? view.role : ""}:${query}`}
      onKeyDown={(e) => {
        // ⌘↵ applies the draft from anywhere in the menu.
        if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && dirty && view.kind === "root" && stage.step === "edit") {
          e.preventDefault();
          p.onApply();
          return true;
        }
        if (e.key === "Backspace" && query === "" && view.kind !== "root") {
          e.preventDefault();
          go(profileBack ?? { kind: "root" });
          return true;
        }
        return false;
      }}
      extraHints={dirty && view.kind === "root" ? [{ keys: [MOD, "↵"], label: "apply" }] : []}
    />
  );
}
