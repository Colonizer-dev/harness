import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { errorMessage, useApi, useToast } from "../context";
import { HIDE_EMPTY_ORGS_KEY, orgEnabled, parseHideEmptyOrgs, serializeHideEmptyOrgs } from "../orgs";
import type { ModuleInfo, OrgInfo, OrgSettings } from "../types";
import { useModels } from "../useModels";
import { Avatar } from "./Avatar";
import { IconX } from "./icons";
import { pluginCost, pluginNames, usePlugins } from "./Skillsets";
import { ModelPicker } from "./ModelPicker";
import { Button, Spinner, Switch, cx, inputClass, stored, store } from "./ui";

type FieldKey =
  | "agent_module"
  | "model"
  | "subagent_model"
  | "background_model"
  | "max_parallel"
  | "repo_max_parallel"
  | "budget_usd"
  | "host_disk"
  | "stack"
  | "memory_enabled"
  | "watchdog_enabled"
  | "stall_minutes"
  | "max_nudges";

interface FieldSpec {
  key: FieldKey;
  group: string;
  label: string;
  hint: string;
  kind: "model" | "number" | "size" | "boolean" | "choice";
  min?: number;
  max?: number;
  unit?: string;
  /** A number field that takes fractions, like a dollar amount. */
  decimal?: boolean;
}

const FIELDS: FieldSpec[] = [
  { key: "agent_module", group: "Models", label: "Agent module", hint: "Which installed agent runs this org's colonies", kind: "choice" },
  { key: "model", group: "Models", label: "Orchestrator", hint: "The main agent in each colony", kind: "model" },
  { key: "subagent_model", group: "Models", label: "Subagents", hint: "Agents the orchestrator starts for side tasks", kind: "model" },
  { key: "background_model", group: "Models", label: "Background", hint: "Small, fast work like summaries and titles", kind: "model" },
  { key: "stack", group: "Colonies", label: "Stack", hint: "The sandbox stack for this org's colonies; Automatic reads each repository's own", kind: "choice" },
  { key: "max_parallel", group: "Colonies", label: "Parallel colonies", hint: "Live colonies in this org at once", kind: "number", min: 1, max: 64 },
  { key: "repo_max_parallel", group: "Colonies", label: "Per repository", hint: "Live colonies in any one of this org's repositories at once", kind: "number", min: 1, max: 32 },
  { key: "budget_usd", group: "Colonies", label: "Budget per colony", hint: "Dollars one colony may spend on models in total; 0 means unlimited", kind: "number", min: 0, decimal: true, unit: "USD" },
  { key: "host_disk", group: "Colonies", label: "Host disk per colony", hint: "Most disk one colony may leave on the host, like 512M or 16G; 0 means unlimited", kind: "size" },
  { key: "memory_enabled", group: "Memory", label: "Shared memory", hint: "Colonies read global, org and repository notes and propose new ones", kind: "boolean" },
  { key: "watchdog_enabled", group: "Watchdog", label: "Watchdog", hint: "Nudge colonies that stop making progress", kind: "boolean" },
  { key: "stall_minutes", group: "Watchdog", label: "Stalled after", hint: "Minutes without agent activity", kind: "number", min: 1, max: 1440, unit: "min" },
  { key: "max_nudges", group: "Watchdog", label: "Nudges", hint: "Before the colony is flagged as still stalled", kind: "number", min: 0, max: 20 },
];

type Value = string | number | boolean | null | undefined;
type Draft = Record<FieldKey, { override: boolean; value: string | boolean }>;

function readSetting(settings: OrgSettings, key: FieldKey): Value {
  switch (key) {
    case "agent_module":
      return settings.agent?.module;
    case "model":
    case "subagent_model":
    case "background_model":
      return settings.agent?.[key];
    case "max_parallel":
    case "repo_max_parallel":
      return settings[key];
    case "budget_usd":
      return settings.budget_usd;
    case "host_disk":
      // `""` and `"0"` both mean unlimited on the server; show the canonical spelling.
      return settings.host_disk === "" ? "0" : settings.host_disk;
    case "stack":
      return settings.stack;
    case "memory_enabled":
      return settings.memory?.enabled;
    case "watchdog_enabled":
      return settings.watchdog?.enabled;
    case "stall_minutes":
    case "max_nudges":
      return settings.watchdog?.[key];
  }
}

/** What an inherited field resolves to, from the global modules. */
function globalValue(modules: ModuleInfo[] | null, key: FieldKey): Value {
  if (!modules) return undefined;
  const module = (kind: string) => modules.find((m) => m.kind === kind);
  const setting = (kind: string, name: string): Value => {
    const m = module(kind);
    return (m?.settings?.[name] ?? m?.schema?.properties?.[name]?.default) as Value;
  };
  const toggle = (kind: string): Value => {
    const m = module(kind);
    return m ? m.enabled && setting(kind, "enabled") !== false : undefined;
  };
  switch (key) {
    case "agent_module":
      // The mothership's own agent module: what an org without a pick of its own launches on.
      return module("agent")?.provider;
    case "model":
    case "subagent_model":
    case "background_model":
      return setting("agent", key);
    case "max_parallel":
    case "repo_max_parallel":
      return setting("sandbox", key);
    case "budget_usd":
      return setting("sandbox", "budget_usd");
    case "host_disk":
      return setting("sandbox", "host_disk");
    case "stack":
      return setting("sandbox", "preset");
    case "memory_enabled":
      return toggle("memory");
    case "watchdog_enabled":
      return toggle("watchdog");
    case "stall_minutes":
    case "max_nudges":
      return setting("watchdog", key);
  }
}

function describe(spec: FieldSpec, value: Value, modules: ModuleInfo[] | null = null): string {
  if (value === undefined || value === null) return "global default";
  if (typeof value === "boolean") return value ? "on" : "off";
  // 0 — or nothing set at all, on the server's quota fields — is how unlimited is written.
  if (spec.key === "budget_usd" && value === 0) return "unlimited";
  if (spec.kind === "size" && (value === "" || value === "0")) return "unlimited";
  if (spec.key === "agent_module" && typeof value === "string")
    // The module's display name, not its id.
    return modules?.find((m) => m.kind === "agent")?.providers?.find((p) => p.id === value)?.name ?? value;
  if (spec.kind === "choice" && typeof value === "string")
    // A blank `preset` reads as automatic too, the same as in Setup.
    return value.trim() === "" || value === "auto" ? "Automatic" : value.charAt(0).toUpperCase() + value.slice(1);
  if (value === "") return spec.key === "model" ? "Claude Code default" : "same as orchestrator";
  return spec.unit ? `${value} ${spec.unit}` : String(value);
}

function toDraft(settings: OrgSettings, modules: ModuleInfo[] | null): Draft {
  const draft = {} as Draft;
  for (const spec of FIELDS) {
    const own = readSetting(settings, spec.key);
    const fallback = globalValue(modules, spec.key);
    const override = own !== undefined && own !== null && !(spec.kind === "model" && own === "");
    const base = override ? own : fallback;
    draft[spec.key] = {
      override,
      value: spec.kind === "boolean" ? base !== false : base === undefined || base === null ? "" : String(base),
    };
  }
  return draft;
}

function parseNumber(spec: FieldSpec, raw: string): number | null {
  const pattern = spec.decimal ? /^\d+(\.\d+)?$/ : /^\d+$/;
  if (!pattern.test(raw.trim())) return null;
  const n = Number(raw);
  if ((spec.min !== undefined && n < spec.min) || (spec.max !== undefined && n > spec.max)) return null;
  return n;
}

/** The server's disk-size rule, mirrored for instant feedback: `512M`, `16G`, bare bytes, either case.
 *  Empty is unlimited too; returns the trimmed text to send, or null when it isn't a size. */
function parseSize(raw: string): string | null {
  const text = raw.trim();
  if (text === "") return "";
  return /^\d+[KkMmGgTt]?$/.test(text) ? text : null;
}

/** Inherited fields are sent as null; an empty model or size override also means inherit. */
function fromDraft(draft: Draft): { settings: OrgSettings; error: string | null } {
  const pick = (key: FieldKey): string | number | boolean | null => {
    const field = draft[key];
    const spec = FIELDS.find((f) => f.key === key)!;
    if (!field.override) return null;
    if (spec.kind === "boolean") return Boolean(field.value);
    if (spec.kind === "choice") return String(field.value).trim() || null;
    if (spec.kind === "model") return String(field.value).trim() || null;
    if (spec.kind === "size") {
      const text = parseSize(String(field.value));
      return text === "" ? null : text; // an override left empty is no override at all
    }
    return parseNumber(spec, String(field.value));
  };
  const invalid = FIELDS.find(
    (spec) =>
      draft[spec.key].override &&
      ((spec.kind === "number" && parseNumber(spec, String(draft[spec.key].value)) === null) ||
        (spec.kind === "size" && parseSize(String(draft[spec.key].value)) === null)),
  );
  const settings: OrgSettings = {
    agent: {
      module: pick("agent_module") as string | null,
      model: pick("model") as string | null,
      subagent_model: pick("subagent_model") as string | null,
      background_model: pick("background_model") as string | null,
    },
    max_parallel: pick("max_parallel") as number | null,
    repo_max_parallel: pick("repo_max_parallel") as number | null,
    budget_usd: pick("budget_usd") as number | null,
    host_disk: pick("host_disk") as string | null,
    stack: pick("stack") as string | null,
    memory: { enabled: pick("memory_enabled") as boolean | null },
    watchdog: {
      enabled: pick("watchdog_enabled") as boolean | null,
      stall_minutes: pick("stall_minutes") as number | null,
      max_nudges: pick("max_nudges") as number | null,
    },
  };
  return {
    settings,
    error: invalid
      ? invalid.kind === "size"
        ? `${invalid.label} must be a size like 512M or 16G, or 0 for unlimited`
        : numberError(invalid)
      : null,
  };
}

/** What a rejected number must look like, for the message under the form. */
function numberError(spec: FieldSpec): string {
  const shape = spec.decimal ? "a dollar amount" : "a whole number";
  if (spec.min !== undefined && spec.max !== undefined) return `${spec.label} must be ${shape} from ${spec.min} to ${spec.max}`;
  if (spec.min !== undefined) return `${spec.label} must be ${shape} of ${spec.min} or more`;
  return `${spec.label} must be ${shape}`;
}

/** Skillset overrides with sorted keys, so switching one back and forth doesn't read as a change. */
function sortedSkillsets(map: Record<string, boolean> | null | undefined): Record<string, boolean> {
  return Object.fromEntries(Object.entries(map ?? {}).sort(([a], [b]) => a.localeCompare(b)));
}

/** No skillset overrides is sent as null: the same as inheriting every one of them. */
function withSkillsets(settings: OrgSettings, skillsets: Record<string, boolean>): OrgSettings {
  const sorted = sortedSkillsets(skillsets);
  return { ...settings, agent: { ...settings.agent, skillsets: Object.keys(sorted).length ? sorted : null } };
}

/**
 * The workspace switch is not an inherit/override field, but it still rides in the same settings
 * payload: off is an explicit `false`; on is `null` — inherit, which resolves to on — so an org
 * nobody has touched carries no opinion of its own. Only this switch writes `enabled`, so
 * "Inherit all" (the module-setting overrides) never re-enables a switched-off org.
 */
function withEnabled(settings: OrgSettings, enabled: boolean): OrgSettings {
  return { ...settings, enabled: enabled ? null : false };
}

/** The skillsets switched on in Settings → Modules, which every org inherits. */
function globalSkillsets(modules: ModuleInfo[] | null): string[] | null {
  const agent = modules?.find((m) => m.kind === "agent");
  if (!agent) return null;
  return pluginNames(agent.settings?.plugins ?? agent.schema?.properties?.plugins?.default);
}

export function OrgSettingsDialog({
  org,
  info,
  onClose,
  onSaved,
}: {
  org: string | null;
  info: OrgInfo | undefined;
  onClose: () => void;
  onSaved: (saved: OrgInfo) => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const [lastOrg, setLastOrg] = useState(org);
  if (org && org !== lastOrg) setLastOrg(org);

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (org && !dialog.open) dialog.showModal();
    else if (!org && dialog.open) dialog.close();
  }, [org]);

  const shown = org ?? lastOrg;

  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="org-settings-title"
      className="m-auto w-[min(640px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      {org && shown && <OrgSettingsForm key={shown} org={shown} info={info} onClose={onClose} onSaved={onSaved} />}
    </dialog>
  );
}

/**
 * One org's workspace settings. The dialog above is one frame for it; the cockpit's Settings →
 * Workspaces section is the other (`embedded`: no close button, no Cancel, it fills its column).
 */
export function OrgSettingsForm({
  org,
  info,
  onClose,
  onSaved,
  embedded = false,
}: {
  org: string;
  info: OrgInfo | undefined;
  onClose: () => void;
  onSaved: (saved: OrgInfo) => void;
  embedded?: boolean;
}) {
  const api = useApi();
  const toast = useToast();
  const models = useModels();
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const { listing } = usePlugins();
  const [draft, setDraft] = useState<Draft>(() => toDraft(info?.settings ?? {}, null));
  const [skillsets, setSkillsets] = useState(() => sortedSkillsets(info?.settings?.agent?.skillsets));
  // Absent and null mean on; only an explicit false opens with the switch off.
  const [enabled, setEnabled] = useState(() => orgEnabled(info?.settings));
  // The workspace-wide "hide orgs with no colonies" toggle also lives here, persisted
  // client-side; the Overview reads it via parseHideEmptyOrgs + hideEmptyOrgEntries (orgs.ts).
  const [hideEmpty, setHideEmpty] = useState(() => parseHideEmptyOrgs(stored(HIDE_EMPTY_ORGS_KEY)));
  const setHideEmptyOrgs = (hide: boolean) => {
    setHideEmpty(hide);
    store(HIDE_EMPTY_ORGS_KEY, serializeHideEmptyOrgs(hide));
  };
  const [initial, setInitial] = useState(() =>
    JSON.stringify(
      withEnabled(
        withSkillsets(fromDraft(toDraft(info?.settings ?? {}, null)).settings, info?.settings?.agent?.skillsets ?? {}),
        orgEnabled(info?.settings),
      ),
    ),
  );
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .modules()
      .then((list) => {
        if (cancelled) return;
        setModules(list);
        // Re-seed inherited values (what an override starts from) now that the global settings are known.
        setDraft((current) => {
          const seeded = toDraft(info?.settings ?? {}, list);
          for (const spec of FIELDS) if (current[spec.key].override) seeded[spec.key] = current[spec.key];
          return seeded;
        });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // Only on open; later polls of the org list must not reset the form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  const { settings: fields, error } = fromDraft(draft);
  const settings = withEnabled(withSkillsets(fields, skillsets), enabled);
  const overrides = FIELDS.filter((spec) => draft[spec.key].override).length + Object.keys(skillsets).length;
  const inheritedSkillsets = globalSkillsets(modules);
  // Every installed skillset, plus any this org still names that is no longer installed.
  const skillsetRows = listing
    ? [
        ...listing.plugins.map((plugin) => ({ name: plugin.name, hint: pluginCost(plugin) })),
        ...Object.keys(skillsets)
          .filter((name) => !listing.plugins.some((p) => p.name === name))
          .map((name) => ({ name, hint: "Not installed: inherit to remove it" })),
      ]
    : [];
  const overrideSkillset = (name: string, override: boolean) =>
    setSkillsets((current) => {
      const next = { ...current };
      // An override starts from what the org inherits, like every other field here.
      if (override) next[name] = inheritedSkillsets?.includes(name) ?? false;
      else delete next[name];
      return sortedSkillsets(next);
    });
  const dirty = JSON.stringify(settings) !== initial;

  const set = (key: FieldKey, patch: Partial<Draft[FieldKey]>) => setDraft((d) => ({ ...d, [key]: { ...d[key], ...patch } }));

  const save = async () => {
    if (error) return;
    setSaving(true);
    try {
      const saved = await api.saveOrg(org, settings);
      setInitial(JSON.stringify(settings));
      onSaved({ colonies: { live: 0, total: 0 }, pending_memory: 0, ...info, ...saved });
      toast(`Saved ${org} workspace settings`);
      onClose();
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const groups = [...new Set(FIELDS.map((f) => f.group))];

  return (
    <div className={cx("flex flex-col", embedded ? "h-full min-h-0 min-w-0 flex-1" : "max-h-[calc(100dvh-24px)]")}>
      <div className="flex shrink-0 items-start gap-3 border-b border-border px-5 py-4">
        <Avatar name={org} src={info?.avatar_url} size={36} rounded="xl" />
        <div className="min-w-0 flex-1">
          <h2 id="org-settings-title" className="text-[16px] font-semibold [overflow-wrap:anywhere]">
            {org} workspace
          </h2>
          <p className="mt-0.5 text-[12.5px] text-muted">
            Colonies on {org} repositories use these settings. Inherit follows Settings → Modules.
          </p>
        </div>
        {!embedded && (
        <button
          type="button"
          onClick={onClose}
          aria-label="Close workspace settings"
          className="grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
        >
          <IconX size={17} />
        </button>
        )}
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-2">
        <section className="border-b border-border py-3">
          <h3 className="text-[11.5px] font-semibold uppercase tracking-wide text-faint">Workspace</h3>
          <div className="divide-y divide-border">
            <div className="flex flex-wrap items-start gap-x-4 gap-y-2 py-3">
              <div className="min-w-0 flex-1 basis-44">
                <div className="text-[13.5px] font-medium">Hide orgs with no colonies</div>
                <div className="text-[12px] text-muted">
                  Orgs with nothing in the colony list stay out of the overview, the rail and the totals.
                </div>
              </div>
              <div className="flex w-full min-w-0 items-center gap-2.5 sm:w-[270px]">
                <Switch checked={hideEmpty} onChange={setHideEmptyOrgs} label="Hide orgs with no colonies" />
                <span className="text-[13px]">{hideEmpty ? "On" : "Off"}</span>
              </div>
            </div>
            <div className="flex flex-wrap items-start gap-x-4 gap-y-2 py-3">
              <div className="min-w-0 flex-1 basis-44">
                <div className="text-[13.5px] font-medium">Include in the workspace list</div>
                <div className="text-[12px] text-muted">
                  Off hides {org} from the workspace list and stops new colonies starting there. Its existing colonies stay listed
                  and resumable, and you can switch it back on here at any time.
                </div>
              </div>
              <div className="flex w-full min-w-0 items-center gap-2.5 sm:w-[270px]">
                <Switch checked={enabled} onChange={setEnabled} label={`Include ${org} as a workspace`} />
                <span className="text-[13px]">{enabled ? "On" : "Off"}</span>
              </div>
            </div>
          </div>
        </section>
        {groups.map((group) => (
          <Fragment key={group}>
            <section className="border-b border-border py-3 last:border-b-0">
              <h3 className="text-[11.5px] font-semibold uppercase tracking-wide text-faint">{group}</h3>
              <div className="divide-y divide-border">
                {FIELDS.filter((f) => f.group === group).map((spec) => (
                  <OverrideRow
                    key={spec.key}
                    label={spec.label}
                    hint={spec.hint}
                    override={draft[spec.key].override}
                    inherited={describe(spec, globalValue(modules, spec.key), modules)}
                    onOverride={(override) => set(spec.key, { override })}
                  >
                    {spec.kind === "model" && (
                      <ModelPicker
                        value={String(draft[spec.key].value)}
                        onChange={(value) => set(spec.key, { value })}
                        models={models}
                        ariaLabel={`${spec.label} model for ${org}`}
                        emptyLabel="Default"
                      />
                    )}
                    {spec.kind === "number" && (
                      <div className="flex items-center gap-2">
                        <input
                          type="number"
                          inputMode={spec.decimal ? "decimal" : "numeric"}
                          min={spec.min}
                          max={spec.max}
                          step={spec.decimal ? "any" : 1}
                          value={String(draft[spec.key].value)}
                          onChange={(e) => set(spec.key, { value: e.target.value })}
                          aria-label={`${spec.label} for ${org}`}
                          aria-invalid={parseNumber(spec, String(draft[spec.key].value)) === null}
                          className={cx(
                            inputClass,
                            "w-28",
                            parseNumber(spec, String(draft[spec.key].value)) === null && "border-err focus:border-err",
                          )}
                        />
                        {spec.unit && <span className="text-[13px] text-muted">{spec.unit}</span>}
                      </div>
                    )}
                    {spec.kind === "size" && (
                      <input
                        type="text"
                        value={String(draft[spec.key].value)}
                        onChange={(e) => set(spec.key, { value: e.target.value })}
                        placeholder="16G"
                        aria-label={`${spec.label} for ${org}`}
                        aria-invalid={parseSize(String(draft[spec.key].value)) === null}
                        className={cx(
                          inputClass,
                          "w-28",
                          parseSize(String(draft[spec.key].value)) === null && "border-err focus:border-err",
                        )}
                      />
                    )}
                    {spec.kind === "choice" && (
                      <select
                        value={String(draft[spec.key].value)}
                        onChange={(e) => set(spec.key, { value: e.target.value })}
                        aria-label={`${spec.label} for ${org}`}
                        className={cx(inputClass, "w-40")}
                      >
                        {(spec.key === "agent_module"
                          ? (modules?.find((m) => m.kind === "agent")?.providers ?? []).map((p) => [p.id, p.name] as const)
                          : (
                              modules?.find((m) => m.kind === "sandbox")?.schema?.properties?.preset?.enum?.map(String) ?? []
                            ).map((id) => [id, id === "auto" ? "Automatic" : id.charAt(0).toUpperCase() + id.slice(1)] as const)
                        ).map(([id, label]) => (
                          <option key={id} value={id}>
                            {label}
                          </option>
                        ))}
                      </select>
                    )}
                    {spec.kind === "boolean" && (
                      <label className="inline-flex h-9 items-center gap-2.5 text-[13px]">
                        <Switch
                          checked={Boolean(draft[spec.key].value)}
                          onChange={(value) => set(spec.key, { value })}
                          label={`${spec.label} for ${org}`}
                        />
                        {draft[spec.key].value ? "On" : "Off"}
                      </label>
                    )}
                  </OverrideRow>
                ))}
              </div>
            </section>
            {group === "Models" && skillsetRows.length > 0 && (
              <section className="border-b border-border py-3 last:border-b-0">
                <h3 className="text-[11.5px] font-semibold uppercase tracking-wide text-faint">Skillsets</h3>
                <div className="divide-y divide-border">
                  {skillsetRows.map(({ name, hint }) => (
                    <OverrideRow
                      key={name}
                      label={name}
                      hint={hint}
                      override={name in skillsets}
                      inherited={inheritedSkillsets ? (inheritedSkillsets.includes(name) ? "on" : "off") : "global default"}
                      onOverride={(override) => overrideSkillset(name, override)}
                    >
                      <label className="inline-flex h-9 items-center gap-2.5 text-[13px]">
                        <Switch
                          checked={skillsets[name] ?? false}
                          onChange={(on) => setSkillsets((current) => sortedSkillsets({ ...current, [name]: on }))}
                          label={`${name} skillset for ${org}`}
                        />
                        {skillsets[name] ? "On" : "Off"}
                      </label>
                    </OverrideRow>
                  ))}
                </div>
              </section>
            )}
          </Fragment>
        ))}
      </div>

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-5 py-3">
        <span className={cx("mr-auto text-[12.5px]", error ? "text-err" : "text-muted")}>
          {error ?? (overrides === 0 ? "Everything inherits the global settings" : `${overrides} override${overrides === 1 ? "" : "s"}`)}
        </span>
        {overrides > 0 && (
          <Button
            variant="ghost"
            onClick={() => {
              setDraft((d) => Object.fromEntries(FIELDS.map((f) => [f.key, { ...d[f.key], override: false }])) as Draft);
              setSkillsets({});
            }}
          >
            Inherit all
          </Button>
        )}
        {!embedded && <Button onClick={onClose}>Cancel</Button>}
        <Button variant="primary" disabled={!dirty || saving || error !== null} onClick={save}>
          {saving && <Spinner />} Save
        </Button>
      </div>
    </div>
  );
}

function OverrideRow({
  label,
  hint,
  override,
  inherited,
  onOverride,
  children,
}: {
  label: string;
  hint: string;
  override: boolean;
  inherited: string;
  onOverride: (override: boolean) => void;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-start gap-x-4 gap-y-2 py-3">
      <div className="min-w-0 flex-1 basis-44">
        <div className="text-[13.5px] font-medium [overflow-wrap:anywhere]">{label}</div>
        <div className="text-[12px] text-muted">{hint}</div>
      </div>
      <div className="flex w-full min-w-0 flex-col gap-2 sm:w-[270px]">
        <div role="radiogroup" aria-label={`${label}: inherit or override`} className="inline-flex self-start rounded-lg bg-panel-2 p-0.5">
          {[false, true].map((value) => (
            <button
              key={String(value)}
              type="button"
              role="radio"
              aria-checked={override === value}
              onClick={() => onOverride(value)}
              className={cx(
                "cursor-pointer rounded-md px-2.5 py-1 text-[12.5px] font-medium transition-colors",
                override === value
                  ? value
                    ? "bg-panel text-accent shadow-sm"
                    : "bg-panel text-text shadow-sm"
                  : "text-muted hover:text-text",
              )}
            >
              {value ? "Override" : "Inherit"}
            </button>
          ))}
        </div>
        {override ? (
          children
        ) : (
          <div className="flex h-9 items-center text-[12.5px] text-faint">
            Global setting: <span className="ml-1 font-medium text-muted [overflow-wrap:anywhere]">{inherited}</span>
          </div>
        )}
      </div>
    </div>
  );
}
