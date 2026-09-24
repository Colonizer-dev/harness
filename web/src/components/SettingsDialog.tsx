import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type ComponentType,
  type Dispatch,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type SetStateAction,
} from "react";
import { errorMessage, useApi, useToast } from "../context";
import { notificationSupport, requestNotificationPermission, type NotificationPermissionState, type NotificationPrefs } from "../notifications";
import { Avatar } from "./Avatar";
import type {
  HarnessStatus,
  HeadroomStatus,
  Mem0Check,
  Mem0Status,
  ModelOption,
  ModelProvider,
  ModelSetting,
  ModuleInfo,
  OrgInfo,
  Session,
  ProviderAuth,
  ProviderHealth,
  ProviderLimits,
  ProviderPreset,
  ProviderPricing,
  ProviderWire,
  SchemaField,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
  VoiceStatus,
} from "../types";
import { PROVIDER_CATALOG, fillTemplate, type CatalogEntry } from "../providerCatalog";
import { avgLatencyText, failureRateText, formatAvgLatency, formatFailureRate, formatSince, quotaExhaustedText, quotaTone, usageHealthTone } from "../providerHealth";
import { useModels } from "../useModels";
import { type ImagePull } from "../useImagePull";
import { setupTone, type SetupView } from "../setup";
import { canRecord, keySourceLabel, startRecording } from "../voiceRecorder";
import {
  BrandAlibabaCloud,
  BrandClaude,
  BrandDeepSeek,
  BrandGitHubCopilot,
  BrandKimi,
  BrandMiniMax,
  BrandModelScope,
  BrandNvidia,
  BrandOpenRouter,
  BrandXai,
  BrandXiaomi,
  IconCheck,
  IconChevron,
  IconExternal,
  IconNetwork,
  IconPencil,
  IconPlug,
  IconPlus,
  IconServer,
  IconX,
  type IconProps,
} from "./icons";
import { SkillsetField } from "./Skillsets";
import { ClaudeLoginSection, GithubTokenForm } from "./Connections";
import { SetupSection } from "./SetupSection";
import { OrgSettingsForm } from "./OrgSettingsDialog";
import { GuideIcon, ModuleProviderMark, SectionHero, guideFor, isAdvancedField, type FlowChip, type FlowNode, type HeroStat } from "./settingsGuide";
import { orgEnabled } from "../orgs";
import { Badge, Button, InfoButton, ModelInput, Spinner, Switch, cx, formatDuration, inputClass, meshBroken, sameOrg, seconds, timeAgo, useMediaQuery, type Tone } from "./ui";

// ---------------------------------------------------------------------------
// Shell: the sections across the top (groups as tabs, sections as chips), the page beneath.
// Below 700px the list is the first screen and each section is a back-navigable page.
// ---------------------------------------------------------------------------

export type SectionId = "setup" | "connections" | "providers" | "runtime" | "live-map" | "updates" | "usage" | "notifications" | `module:${string}` | `org:${string}`;

const PANE_TITLE_ID = "settings-pane-title";

/** The page's hero card (settingsGuide.tsx), which every Pane shows at the top of its body. */
const HeroContext = createContext<ReactNode>(null);

const KIND_INFO: Record<string, { title: string; description: string }> = {
  source: { title: "Source", description: "Where tasks come from" },
  sandbox: { title: "Sandbox", description: "Where agents run" },
  mesh: { title: "Mesh", description: "Private network between the Mothership and colonies" },
  agent: { title: "Agent", description: "The coding agent inside each microVM" },
  interfaces: { title: "Interfaces", description: "Panels in the colony view" },
  publish: { title: "Publish", description: "Where finished work goes" },
  memory: { title: "Memory", description: "Shared notes colonies can search and propose" },
  watchdog: { title: "Watchdog", description: "Notices stalled colonies and nudges them" },
  autonomy: { title: "Autonomy", description: "Who answers a colony's questions when you are not there" },
  burn_down: { title: "Burn-down", description: "Spend the weekly token plan down to a reserve before it resets" },
  voice: { title: "Voice", description: "Speech-to-text for the composer's microphone" },
};

const kindInfo = (kind: string) => KIND_INFO[kind] ?? { title: kind, description: "" };

type ModuleDraft = { provider: string; enabled: boolean; settings: Record<string, unknown> };

const draftOf = (m: ModuleInfo): ModuleDraft => ({ provider: m.provider, enabled: m.enabled, settings: m.settings ?? {} });

const isDirty = (m: ModuleInfo, d: ModuleDraft | undefined) =>
  d !== undefined && (d.provider !== m.provider || d.enabled !== m.enabled || JSON.stringify(d.settings) !== JSON.stringify(m.settings ?? {}));

export function SettingsDialog({
  open,
  onClose,
  status,
  onStatusChanged,
  onModulesChanged,
  telemetry,
  onTelemetryChanged,
  usage,
  onUsageChanged,
  notifications,
  onNotificationsChanged,
  initialSection,
  setup,
  pull,
  onLaunch,
  onSetupShown,
  onSetupDismissed,
}: {
  open: boolean;
  onClose: () => void;
  status: HarnessStatus | null;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
  onModulesChanged: (modules: ModuleInfo[]) => void;
  telemetry: TelemetryStatus | null;
  onTelemetryChanged: (telemetry: TelemetryStatus) => void;
  usage: UsageStatus | null;
  onUsageChanged: (usage: UsageStatus) => void;
  notifications: NotificationPrefs;
  /** A React-style setter, so the pane can compose over the latest prefs: the browser's permission answer lands after a delay, and a value-based write from the opening render would revert anything toggled meanwhile. */
  onNotificationsChanged: Dispatch<SetStateAction<NotificationPrefs>>;
  /** The section to open on, instead of the first. */
  initialSection?: SectionId;
  /** The Setup checklist's derivation, computed in App from the same state the app polls. */
  setup: SetupView | null;
  /** The app-wide image-pull poller, shared with the sidebar and Setup. */
  pull: ImagePull;
  /** Setup's Launch row: closes Settings and opens the sidebar's launcher. */
  onLaunch: () => void;
  /** Fired once the Setup pane has been on screen, so the standalone live-map prompt stands down. */
  onSetupShown: () => void;
  /** "Not now": held in memory, so the auto-open may fire again on the next page load only. */
  onSetupDismissed: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  }, [open]);

  return (
    <dialog
      ref={ref}
      onClose={onClose}
      aria-labelledby="settings-title"
      className="m-auto w-[min(900px,calc(100vw-24px))] max-w-none overflow-hidden rounded-2xl border border-border bg-panel p-0 text-text shadow-[var(--shadow)] backdrop:bg-black/50"
    >
      {open && (
        <SettingsBody
          status={status}
          onStatusChanged={onStatusChanged}
          onModulesChanged={onModulesChanged}
          telemetry={telemetry}
          onTelemetryChanged={onTelemetryChanged}
          usage={usage}
          onUsageChanged={onUsageChanged}
          notifications={notifications}
          onNotificationsChanged={onNotificationsChanged}
          initialSection={initialSection}
          setup={setup}
          pull={pull}
          onLaunch={onLaunch}
          onSetupShown={onSetupShown}
          onSetupDismissed={onSetupDismissed}
          onClose={onClose}
        />
      )}
    </dialog>
  );
}

/**
 * The settings screen itself: the grouped section nav and whichever pane it points at. The dialog
 * above is one frame for it; the cockpit's settings view is the other, which is what `embedded` picks.
 */
export function SettingsBody({
  embedded = false,
  status,
  onStatusChanged,
  onModulesChanged,
  telemetry,
  onTelemetryChanged,
  usage,
  onUsageChanged,
  notifications,
  onNotificationsChanged,
  initialSection,
  setup,
  pull,
  onLaunch,
  onSetupShown,
  onSetupDismissed,
  onClose,
  orgs,
  onOrgSaved,
  sessions,
}: {
  /** Rendered inside the cockpit rather than a dialog: no title bar of its own, and it fills its column. */
  embedded?: boolean;
  status: HarnessStatus | null;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
  onModulesChanged: (modules: ModuleInfo[]) => void;
  telemetry: TelemetryStatus | null;
  onTelemetryChanged: (telemetry: TelemetryStatus) => void;
  usage: UsageStatus | null;
  onUsageChanged: (usage: UsageStatus) => void;
  notifications: NotificationPrefs;
  /** A React-style setter, so the pane can compose over the latest prefs (see SettingsDialog). */
  onNotificationsChanged: Dispatch<SetStateAction<NotificationPrefs>>;
  initialSection?: SectionId;
  setup: SetupView | null;
  pull: ImagePull;
  onLaunch: () => void;
  onSetupShown: () => void;
  onSetupDismissed: () => void;
  onClose: () => void;
  /** The workspaces, for a Workspaces group of per-org settings; the cockpit passes them, the old dialog does not. */
  orgs?: OrgInfo[];
  onOrgSaved?: (saved: OrgInfo) => void;
  /** The colony list, for the live counts in the Source page's picture. */
  sessions?: Session[];
}) {
  const api = useApi();
  const narrow = useMediaQuery("(max-width: 699px)");
  // A narrow window opens on the section list; a wide one on the first section.
  const [section, setSection] = useState<SectionId | null>(() => initialSection ?? (narrow ? null : "connections"));
  // Where focus returns when a narrow window goes back to the section list.
  const lastSection = useRef<SectionId>(initialSection ?? "connections");
  const select = (id: SectionId) => {
    lastSection.current = id;
    setSection(id);
  };
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const [modulesError, setModulesError] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, ModuleDraft>>({});
  const [providers, setProviders] = useState<ModelProvider[] | null>(null);
  const [providersError, setProvidersError] = useState<string | null>(null);
  // Fetched here rather than threaded through App: nothing outside Settings needs it.
  const [update, setUpdate] = useState<UpdateStatus | null>(null);
  const models = useModels();

  useEffect(() => {
    let cancelled = false;
    api
      .update()
      .then((u) => !cancelled && setUpdate(u))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api]);

  useEffect(() => {
    let cancelled = false;
    api
      .modules()
      .then((list) => {
        if (cancelled) return;
        setModules(list);
        setDrafts(Object.fromEntries(list.map((m) => [m.kind, draftOf(m)])));
      })
      .catch((e) => !cancelled && setModulesError(errorMessage(e)));
    return () => {
      cancelled = true;
    };
  }, [api]);

  const loadProviders = useCallback(async () => {
    try {
      setProviders(await api.providers());
      setProvidersError(null);
    } catch (e) {
      setProvidersError(errorMessage(e));
    }
  }, [api]);

  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  const active: SectionId | null = narrow ? section : (section ?? "connections");

  const github = status?.github;
  const claude = status?.claude;
  const connectionsTone: Tone | null = !status ? null : github?.connected && claude?.configured ? "ok" : "err";
  const runtimeBroken = Boolean(status && (!status.sandbox.msb_version || status.sandbox.claude_bin_error || meshBroken(status.mesh)));
  // The Setup summary tone shares one derivation with the pane itself.
  const setupToneValue: Tone | null = setup ? setupTone(setup) : null;

  /** One save handler for module panes and Setup's stack pick alike: keeps the drafts and the app's modules in step. */
  const onModuleSaved = (saved: ModuleInfo) => {
    const next = (modules ?? []).map((m) => (m.kind === saved.kind ? saved : m));
    setModules(next);
    setDrafts((d) => ({ ...d, [saved.kind]: draftOf(saved) }));
    onModulesChanged(next);
  };

  const groups: NavGroup[] = [
    {
      label: "General",
      items: [
        {
          id: "setup",
          label: "Setup",
          hint: "The checklist for the first colony",
          tone: setupToneValue,
          toneText:
            setupToneValue === "err" ? "Needs setup" : setupToneValue === "ok" ? "All set" : setupToneValue === "warn" ? "Not finished yet" : undefined,
        },
        {
          id: "connections",
          label: "Connections",
          hint: "GitHub and Claude",
          tone: connectionsTone,
          toneText: connectionsTone === "ok" ? "All connected" : connectionsTone === "err" ? "Needs setup" : undefined,
        },
        {
          id: "providers",
          label: "Model providers",
          hint: "Claude, and other Anthropic-compatible endpoints",
          // Anthropic is always in the list as a built-in row, so the count follows what is on screen.
          badge: providers ? String(providers.length + 1) : undefined,
        },
        { id: "runtime", label: "Runtime", hint: "Detected on this machine", tone: runtimeBroken ? "err" : null, toneText: runtimeBroken ? "Something is missing" : undefined },
        {
          id: "live-map",
          label: "Live map",
          hint: "This mothership as a dot on colonizer.dev",
          badge: telemetry ? (telemetry.enabled ? "On" : "Off") : undefined,
        },
        {
          id: "updates",
          label: "Updates",
          hint: "Which Colonizer this is, and whether a newer one is out",
          tone: update?.available ? "ok" : null,
          toneText: update?.available ? `${update.latest?.version} available` : undefined,
          badge: update && !update.available ? update.installed.version : undefined,
        },
        {
          id: "usage",
          label: "Usage data",
          hint: "An anonymous batch, built here and shown — nothing is sent yet",
          badge: usage ? (usage.enabled ? "On" : "Off") : undefined,
        },
        {
          id: "notifications",
          label: "Notifications",
          hint: "What tells you a colony needs you when the tab is not in front",
          // The badge follows the two opt-in channels; the in-tab layer is on by default and needs no advertising.
          badge: notifications.sound || notifications.browser ? "On" : "Off",
        },
      ],
    },
    {
      label: "Modules",
      loading: !modules && !modulesError,
      error: modulesError,
      items: (modules ?? []).map((m) => ({
        id: `module:${m.kind}` as const,
        label: kindInfo(m.kind).title,
        hint: kindInfo(m.kind).description,
        badge: m.enabled ? undefined : "Off",
        dirty: isDirty(m, drafts[m.kind]),
      })),
    },
    ...(orgs && orgs.length > 0
      ? [
          {
            label: "Workspaces",
            items: orgs
              .filter((o) => !o.awaiting_decision)
              .map((o) => ({
                id: `org:${o.org}` as const,
                label: o.org,
                hint: `${o.colonies.live} live · ${o.colonies.total} ${o.colonies.total === 1 ? "colony" : "colonies"}`,
                badge: orgEnabled(o.settings) ? undefined : "Off",
              })),
          },
        ]
      : []),
  ];

  const back = narrow ? () => setSection(null) : undefined;

  /** Two or three facts for the hero card, from what this screen already has loaded. */
  const heroStats = (id: SectionId): HeroStat[] => {
    const onOff = (on: boolean | null | undefined): HeroStat["tone"] => (on ? "ok" : undefined);
    switch (id) {
      case "setup":
        return setup && setupToneValue ? [{ label: "Status", value: setupToneValue === "ok" ? "All set" : "Not finished", tone: setupToneValue }] : [];
      case "connections":
        return status
          ? [
              { label: "GitHub", value: github?.connected ? (github.login ?? "Connected") : "Not connected", tone: github?.connected ? "ok" : "err" },
              { label: "Claude", value: claude?.configured ? "Connected" : "Not connected", tone: claude?.configured ? "ok" : "err" },
            ]
          : [];
      case "providers":
        return providers ? [{ label: "Providers", value: String(providers.length + 1) }] : [];
      case "runtime":
        return status
          ? [
              { label: "microsandbox", value: status.sandbox.msb_version ?? "missing", tone: status.sandbox.msb_version ? "ok" : "err" },
              { label: "Mesh", value: meshBroken(status.mesh) ? "Needs attention" : "Healthy", tone: meshBroken(status.mesh) ? "err" : "ok" },
            ]
          : [];
      case "live-map":
        return telemetry ? [{ label: "Live map", value: telemetry.enabled ? "On" : "Off", tone: onOff(telemetry.enabled) }] : [];
      case "updates":
        return update
          ? [
              { label: "Installed", value: update.installed.version },
              ...(update.available && update.latest ? [{ label: "Available", value: update.latest.version, tone: "ok" as const }] : []),
            ]
          : [];
      case "usage":
        return usage ? [{ label: "Usage data", value: usage.enabled ? "On" : "Off", tone: onOff(usage.enabled) }] : [];
      case "notifications":
        return [
          { label: "In tab", value: notifications.inTab ? "On" : "Off", tone: onOff(notifications.inTab) },
          { label: "Sound", value: notifications.sound ? "On" : "Off", tone: onOff(notifications.sound) },
          { label: "Browser", value: notifications.browser ? "On" : "Off", tone: onOff(notifications.browser) },
        ];
    }
    if (id.startsWith("org:")) {
      const o = orgs?.find((x) => sameOrg(x.org, id.slice("org:".length)));
      return o
        ? [
            { label: "Live", value: String(o.colonies.live), tone: o.colonies.live > 0 ? "ok" : undefined },
            { label: "Colonies", value: String(o.colonies.total) },
            { label: "Workspace", value: orgEnabled(o.settings) ? "On" : "Off", tone: onOff(orgEnabled(o.settings)) },
          ]
        : [];
    }
    if (id.startsWith("module:")) {
      const kind = id.slice("module:".length);
      const m = modules?.find((x) => x.kind === kind);
      const d = drafts[kind];
      if (!m || !d) return [];
      const stats: HeroStat[] = [{ label: "Module", value: d.enabled ? "On" : "Off", tone: onOff(d.enabled) }];
      const provider = m.providers.find((x) => x.id === d.provider);
      if (m.providers.length > 1 && provider) stats.push({ label: "Provider", value: provider.name });
      // One or two short, essential values: a model, a count, a mode.
      for (const [key, field] of Object.entries(m.schema?.properties ?? {})) {
        if (stats.length >= 3) break;
        if (isAdvancedField(key, field) || field.type === "boolean" || field.format) continue;
        const v = d.settings[key];
        if (v === undefined || v === null || v === "" || String(v).length > 22) continue;
        stats.push({ label: field.title ?? key, value: String(v) });
      }
      return stats;
    }
    return [];
  };

  let pane: ReactNode = null;
  if (active === "setup") {
    pane = (
      <SetupSection
        status={status}
        setup={setup}
        pull={pull}
        telemetry={telemetry}
        sandbox={(modules ?? []).find((m) => m.kind === "sandbox") ?? null}
        onStatusChanged={onStatusChanged}
        onSandboxSaved={onModuleSaved}
        onTelemetryChanged={onTelemetryChanged}
        onLaunch={onLaunch}
        onDismiss={onSetupDismissed}
        onShown={onSetupShown}
        onOpenLiveMap={() => select("live-map")}
        back={back}
      />
    );
  } else if (active === "connections") pane = <ConnectionsPane status={status} onStatusChanged={onStatusChanged} back={back} />;
  else if (active === "runtime") pane = <RuntimePane status={status} back={back} />;
  else if (active === "live-map") pane = <LiveMapPane telemetry={telemetry} onChanged={onTelemetryChanged} back={back} />;
  else if (active === "updates") pane = <UpdatesPane update={update} onChanged={setUpdate} back={back} />;
  else if (active === "usage") pane = <UsagePane usage={usage} onChanged={onUsageChanged} back={back} />;
  else if (active === "notifications") pane = <NotificationsPane prefs={notifications} onChanged={onNotificationsChanged} back={back} />;
  else if (active === "providers") {
    pane = (
      <ProvidersPane
        providers={providers}
        error={providersError}
        setProviders={setProviders}
        reload={loadProviders}
        claude={claude ?? null}
        models={models}
        onOpenConnections={() => select("connections")}
        back={back}
      />
    );
  } else if (active?.startsWith("org:")) {
    const org = active.slice("org:".length);
    pane = (
      <OrgSettingsForm
        key={org}
        embedded
        org={org}
        info={orgs?.find((o) => sameOrg(o.org, org))}
        onClose={() => {}}
        onSaved={(saved) => onOrgSaved?.(saved)}
      />
    );
  } else if (active?.startsWith("module:")) {
    const kind = active.slice("module:".length);
    const module = modules?.find((m) => m.kind === kind);
    const draft = drafts[kind];
    pane =
      module && draft ? (
        <ModulePane
          key={kind}
          module={module}
          draft={draft}
          models={kind === "agent" ? models : undefined}
          pull={pull}
          back={back}
          onDraft={(patch) => setDrafts((d) => ({ ...d, [kind]: { ...d[kind], ...patch } }))}
          onReset={() => setDrafts((d) => ({ ...d, [kind]: draftOf(module) }))}
          onSaved={onModuleSaved}
        />
      ) : (
        <Pane title={kindInfo(kind).title} back={back}>
          <p className="flex items-center gap-2 text-[13px] text-muted">
            <Spinner /> Loading…
          </p>
        </Pane>
      );
  }

  // What the page is for, as a card: Pane shows it at the top of its body; Setup and a workspace
  // draw their own frame, so for them it sits above the pane instead.
  const hero = active ? <SectionHero guide={guideFor(active)} stats={heroStats(active)} flow={active === "module:source" ? sourceFlow(drafts.source?.settings, sessions) : undefined} /> : null;
  const ownFrame = active === "setup" || Boolean(active?.startsWith("org:"));
  const framed = (
    <HeroContext.Provider value={ownFrame ? null : hero}>
      {ownFrame ? (
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="shrink-0 px-5 pt-4">{hero}</div>
          {pane}
        </div>
      ) : (
        pane
      )}
    </HeroContext.Provider>
  );

  return (
    <div className={cx("flex flex-col", embedded ? "h-full min-h-0" : "h-[min(680px,calc(100dvh-24px))]")}>
      {/* The cockpit has its own header and crumb, so the embedded frame does not repeat them. */}
      {!embedded && (
        <div className="flex shrink-0 items-center gap-3 border-b border-border px-5 py-3">
          <h2 id="settings-title" className="min-w-0 flex-1 text-[16px] font-semibold">
            Settings
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close settings"
            className="grid size-8 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconX size={17} />
          </button>
        </div>
      )}
      {narrow ? (
        active === null ? (
          <SectionNav layout="list" groups={groups} active={null} onSelect={select} initialFocus={lastSection.current} />
        ) : (
          framed
        )
      ) : (
        <>
          <TopNav groups={groups} active={active} onSelect={select} />
          <div className="flex min-h-0 flex-1">{framed}</div>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Section list
// ---------------------------------------------------------------------------

type NavItem = {
  id: SectionId;
  label: string;
  hint?: string;
  /** A coloured dot; `toneText` is what a screen reader hears for it. */
  tone?: Tone | null;
  toneText?: string;
  badge?: string;
  dirty?: boolean;
};

type NavGroup = { label: string; items: NavItem[]; loading?: boolean; error?: string | null };

function SectionNav({
  layout,
  groups,
  active,
  onSelect,
  initialFocus,
}: {
  layout: "side" | "list";
  groups: NavGroup[];
  active: SectionId | null;
  onSelect: (id: SectionId) => void;
  /** Focus this item when the list appears (the section a narrow window just left). */
  initialFocus?: SectionId;
}) {
  const refs = useRef<Partial<Record<SectionId, HTMLButtonElement | null>>>({});
  const flat = groups.flatMap((g) => g.items);
  const side = layout === "side";

  useEffect(() => {
    if (initialFocus) refs.current[initialFocus]?.focus();
  }, [initialFocus]);

  // In the side layout the list is one Tab stop: arrows move between sections.
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!side || !["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const index = flat.findIndex((item) => item.id === active);
    let next = index;
    if (e.key === "ArrowDown") next = (index + 1) % flat.length;
    else if (e.key === "ArrowUp") next = (index - 1 + flat.length) % flat.length;
    else if (e.key === "Home") next = 0;
    else next = flat.length - 1;
    const target = flat[next];
    if (!target) return;
    e.preventDefault();
    onSelect(target.id);
    refs.current[target.id]?.focus();
  };

  return (
    <nav
      aria-label="Settings sections"
      onKeyDown={onKeyDown}
      className={cx(
        "scroll-thin min-h-0 overflow-y-auto",
        side ? "w-[200px] shrink-0 space-y-4 border-r border-border px-2.5 py-3" : "flex-1 space-y-4 px-5 py-3",
      )}
    >
      {groups.map((group) => (
        <div key={group.label}>
          <h3 className={cx("text-[11.5px] font-semibold uppercase tracking-wide text-faint", side ? "mb-1 px-2.5" : "mb-1")}>{group.label}</h3>
          {group.loading && (
            <p className={cx("flex items-center gap-2 py-1.5 text-[12.5px] text-muted", side && "px-2.5")}>
              <Spinner className="size-3" /> Loading…
            </p>
          )}
          {group.error && <p className={cx("py-1.5 text-[12.5px] text-err", side && "px-2.5")}>{group.error}</p>}
          <ul className={side ? "space-y-0.5" : "divide-y divide-border overflow-hidden rounded-xl border border-border"}>
            {group.items.map((item) => {
              const current = item.id === active;
              return (
                <li key={item.id}>
                  <button
                    type="button"
                    ref={(el) => {
                      refs.current[item.id] = el;
                    }}
                    aria-current={current ? "true" : undefined}
                    tabIndex={side ? (current ? 0 : -1) : 0}
                    onClick={() => onSelect(item.id)}
                    className={cx(
                      "flex w-full cursor-pointer items-center gap-2 text-left",
                      side
                        ? cx("rounded-lg px-2.5 py-1.5 text-[13px]", current ? "bg-panel-2 font-medium text-text" : "text-muted hover:bg-panel-2 hover:text-text")
                        : "px-3.5 py-3 text-[13.5px] hover:bg-panel-2",
                    )}
                  >
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{item.label}</span>
                      {!side && item.hint && <span className="block truncate text-[12px] text-muted">{item.hint}</span>}
                    </span>
                    {item.dirty && (
                      <>
                        <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                        <span className="sr-only">unsaved changes</span>
                      </>
                    )}
                    {item.badge && <span className="shrink-0 text-[11.5px] text-faint">{item.badge}</span>}
                    {item.tone && (
                      <>
                        <span
                          aria-hidden="true"
                          className={cx("size-2 shrink-0 rounded-full", item.tone === "ok" ? "bg-ok" : item.tone === "err" ? "bg-err" : "bg-warn")}
                        />
                        {item.toneText && <span className="sr-only">{item.toneText}</span>}
                      </>
                    )}
                    {!side && <IconChevron size={14} className="shrink-0 text-faint" />}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

/**
 * The wide layout's nav, across the top: the groups as tabs, and the chosen group's sections as
 * chips beneath, each with its icon. The chip row is one Tab stop; the arrows move along it.
 */
function TopNav({ groups, active, onSelect }: { groups: NavGroup[]; active: SectionId | null; onSelect: (id: SectionId) => void }) {
  const owner = groups.find((g) => g.items.some((item) => item.id === active)) ?? groups[0];
  const [groupLabel, setGroupLabel] = useState(owner?.label);
  // Following the page: a section opened from elsewhere (a link, a deep link) brings its group along.
  const [lastActive, setLastActive] = useState(active);
  if (active !== lastActive) {
    setLastActive(active);
    if (owner) setGroupLabel(owner.label);
  }
  const group = groups.find((g) => g.label === groupLabel) ?? owner;
  const refs = useRef<Partial<Record<SectionId, HTMLButtonElement | null>>>({});
  const items = group?.items ?? [];

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (!["ArrowRight", "ArrowLeft", "Home", "End"].includes(e.key) || items.length === 0) return;
    const index = Math.max(0, items.findIndex((item) => item.id === active));
    const next =
      e.key === "ArrowRight" ? (index + 1) % items.length
      : e.key === "ArrowLeft" ? (index - 1 + items.length) % items.length
      : e.key === "Home" ? 0
      : items.length - 1;
    e.preventDefault();
    onSelect(items[next].id);
    refs.current[items[next].id]?.focus();
  };

  return (
    <nav aria-label="Settings sections" className="shrink-0 border-b border-border">
      <div role="tablist" aria-label="Settings groups" className="flex gap-1 px-4 pt-2.5">
        {groups.map((g) => {
          const current = g.label === group?.label;
          return (
            <button
              key={g.label}
              type="button"
              role="tab"
              aria-selected={current}
              onClick={() => {
                setGroupLabel(g.label);
                if (g.items[0] && !g.items.some((item) => item.id === active)) onSelect(g.items[0].id);
              }}
              className={cx(
                "relative cursor-pointer rounded-md px-3 py-1.5 text-[13px] font-medium transition-colors",
                current ? "text-text" : "text-muted hover:bg-panel-2 hover:text-text",
              )}
            >
              {g.label}
              <span className="ml-1.5 text-[11.5px] font-normal tabular-nums text-faint">{g.items.length || ""}</span>
              {current && <span aria-hidden="true" className="absolute inset-x-2 -bottom-[1px] h-0.5 rounded-full bg-accent" />}
            </button>
          );
        })}
      </div>
      <div className="border-t border-border">
        {group?.loading && (
          <p className="flex items-center gap-2 px-5 py-2.5 text-[12.5px] text-muted">
            <Spinner className="size-3" /> Loading…
          </p>
        )}
        {group?.error && <p className="px-5 py-2.5 text-[12.5px] text-err">{group.error}</p>}
        <ul onKeyDown={onKeyDown} className="scroll-thin flex gap-1.5 overflow-x-auto px-4 py-2.5">
          {items.map((item) => {
            const current = item.id === active;
            return (
              <li key={item.id} className="shrink-0">
                <button
                  type="button"
                  ref={(el) => {
                    refs.current[item.id] = el;
                  }}
                  aria-current={current ? "true" : undefined}
                  tabIndex={current || (!items.some((i) => i.id === active) && item === items[0]) ? 0 : -1}
                  title={item.hint}
                  onClick={() => onSelect(item.id)}
                  className={cx(
                    "flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-1.5 text-[13px] transition-colors",
                    current ? "border-border-strong bg-panel-2 font-medium text-text" : "border-transparent text-muted hover:bg-panel-2 hover:text-text",
                  )}
                >
                  <GuideIcon name={guideFor(item.id).icon} size={15} className={current ? "text-accent" : undefined} />
                  <span className="whitespace-nowrap">{item.label}</span>
                  {item.dirty && (
                    <>
                      <span aria-hidden="true" className="size-1.5 shrink-0 rounded-full bg-accent" />
                      <span className="sr-only">unsaved changes</span>
                    </>
                  )}
                  {item.badge && <span className="text-[11.5px] tabular-nums text-faint">{item.badge}</span>}
                  {item.tone && (
                    <>
                      <span
                        aria-hidden="true"
                        className={cx("size-2 shrink-0 rounded-full", item.tone === "ok" ? "bg-ok" : item.tone === "err" ? "bg-err" : "bg-warn")}
                      />
                      {item.toneText && <span className="sr-only">{item.toneText}</span>}
                    </>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </div>
    </nav>
  );
}

// ---------------------------------------------------------------------------
// Pane and row layout shared by every section
// ---------------------------------------------------------------------------

function Pane({
  title,
  subtitle,
  info,
  aside,
  back,
  footer,
  children,
}: {
  title: string;
  subtitle?: string;
  /** Longer explanation, behind the "i" next to the title. */
  info?: ReactNode;
  aside?: ReactNode;
  back?: () => void;
  footer?: ReactNode;
  children: ReactNode;
}) {
  const titleRef = useRef<HTMLHeadingElement>(null);
  const hero = useContext(HeroContext);
  const stacked = Boolean(back);
  // In the narrow, back-navigable layout the pane replaces the list, so focus must follow.
  useEffect(() => {
    if (stacked) titleRef.current?.focus();
  }, [stacked]);

  return (
    <section aria-labelledby={PANE_TITLE_ID} className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-start gap-2 border-b border-border px-5 py-3.5">
        {back && (
          <button
            type="button"
            onClick={back}
            aria-label="Back to all settings"
            className="-ml-1.5 grid size-8 shrink-0 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconChevron size={16} className="rotate-180" />
          </button>
        )}
        <div className="min-w-0 flex-1">
          <h3
            id={PANE_TITLE_ID}
            ref={titleRef}
            tabIndex={stacked ? -1 : undefined}
            className="flex items-center gap-1 rounded text-[15px] font-semibold leading-8"
          >
            {title}
            {info && <InfoButton label={title}>{info}</InfoButton>}
          </h3>
          {subtitle && <p className="-mt-1 text-[12.5px] text-muted">{subtitle}</p>}
        </div>
        {aside && <div className="flex shrink-0 items-center leading-8">{aside}</div>}
      </div>
      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {hero}
        {children}
      </div>
      {footer && <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-5 py-3">{footer}</div>}
    </section>
  );
}

/** One setting: label on the left, control on the right, explanation behind the "i". The label's id is `${id}-label`. */
function Row({
  id,
  label,
  info,
  inline,
  children,
}: {
  id?: string;
  label: string;
  info?: ReactNode;
  /** For switches: keeps the control on the label's line at every width. */
  inline?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
      <div className="min-w-0 flex-1 basis-40">
        <div className="flex items-center gap-1">
          <label id={id ? `${id}-label` : undefined} htmlFor={id} className="text-[13.5px] font-medium">
            {label}
          </label>
          {info && <InfoButton label={label}>{info}</InfoButton>}
        </div>
      </div>
      <div className={cx("min-w-0", inline ? "flex shrink-0 justify-end sm:w-[260px]" : "w-full sm:w-[260px]")}>{children}</div>
    </div>
  );
}

function Code({ children }: { children: ReactNode }) {
  return <code className="rounded bg-panel-3 px-1 font-mono text-[11.5px]">{children}</code>;
}

// ---------------------------------------------------------------------------
// Connections: GitHub and Claude
// ---------------------------------------------------------------------------

// The account avatar beside the GitHub login row is the shared Avatar (`alt=""` — the login is
// already shown as text), on the same quiet tile as a provider mark.
function ConnectionCard({
  name,
  mark,
  connected,
  detail,
  detailTone,
  info,
  children,
}: {
  name: string;
  /** Optional tile at the front of the header row, e.g. an account avatar. */
  mark?: ReactNode;
  connected: boolean | null;
  detail?: string;
  detailTone?: "err";
  info: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-border">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 px-4 py-3">
        {mark}
        <span className="flex items-center gap-1 text-[14px] font-semibold">
          {name}
          <InfoButton label={name}>{info}</InfoButton>
        </span>
        {connected != null && <Badge tone={connected ? "ok" : "err"}>{connected ? "Connected" : "Not connected"}</Badge>}
        {detail && <span className={cx("text-[12.5px] [overflow-wrap:anywhere]", detailTone === "err" ? "text-err" : "text-muted")}>{detail}</span>}
      </div>
      <div className="space-y-3 border-t border-border px-4 py-3">{children}</div>
    </div>
  );
}

function ConnectionsPane({ status, onStatusChanged, back }: { status: HarnessStatus | null; onStatusChanged: (fresh?: boolean) => Promise<void> | void; back?: () => void }) {
  const github = status?.github ?? null;
  const claude = status?.claude ?? null;
  const githubViaToken = github?.source === "saved token";

  return (
    <Pane title="Connections" subtitle="Both are needed before the first colony" back={back}>
      <div className="space-y-4">
        <ConnectionCard
          name="GitHub"
          mark={github?.connected && github.avatar_url ? <Avatar name={github.login || "GitHub"} src={github.avatar_url} /> : undefined}
          connected={github ? github.connected : null}
          detail={github?.connected ? `@${github.login} · ${github.source}` : github?.error?.split("\n")[0]}
          detailTone={github && !github.connected ? "err" : undefined}
          info={
            <>
              <p>
                Uses your <Code>gh auth login</Code> session when there is one, or a token you save here.
              </p>
              <p className="text-muted">A fine-grained token needs Contents, Issues and Pull requests, read and write.</p>
            </>
          }
        >
          {github?.connected ? (
            <details className="text-[13px]">
              <summary className="cursor-pointer text-muted hover:text-text">{githubViaToken ? "Replace or remove the token" : "Use a token instead"}</summary>
              <div className="mt-2">
                <GithubTokenForm onStatusChanged={onStatusChanged} />
              </div>
            </details>
          ) : (
            <GithubTokenForm onStatusChanged={onStatusChanged} />
          )}
        </ConnectionCard>

        <ConnectionCard
          name="Claude"
          connected={claude ? claude.configured : null}
          detail={claude?.configured ? [claude.account ?? "account not identified", claude.source].filter(Boolean).join(" · ") : undefined}
          info={
            <>
              <p>
                Log in runs <Code>claude setup-token</Code> on the Mothership (this machine) and saves a 1-year token here.
              </p>
              <p className="text-muted">microVMs only ever see a placeholder; the real token is swapped in for requests to api.anthropic.com.</p>
              <p className="text-muted">
                Anthropic does not report an expiry, so the harness shows the documented 1-year lifetime as an estimate.
              </p>
            </>
          }
        >
          <ClaudeLoginSection claude={claude} onStatusChanged={onStatusChanged} />
        </ConnectionCard>
      </div>
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Runtime: what the Mothership found on this machine
// ---------------------------------------------------------------------------

function RuntimePane({ status, back }: { status: HarnessStatus | null; back?: () => void }) {
  const rows: { label: string; value: ReactNode; bad?: boolean; mono?: boolean }[] = status
    ? [
        { label: "microsandbox", value: status.sandbox.msb_version ?? "not found", bad: !status.sandbox.msb_version },
        { label: "Image", value: status.sandbox.image, mono: true },
        {
          label: "Agent binary",
          value: status.sandbox.claude_bin ?? status.sandbox.claude_bin_error ?? "—",
          bad: Boolean(status.sandbox.claude_bin_error),
          mono: true,
        },
        {
          label: "Mesh",
          value: status.mesh
            ? status.mesh.enabled
              ? [status.mesh.provider, status.mesh.state, status.mesh.harness_ip, status.mesh.detail, status.mesh.error]
                  .filter(Boolean)
                  .join(" · ")
              : "disabled"
            : "—",
          bad: meshBroken(status.mesh),
        },
      ]
    : [];

  return (
    <Pane title="Runtime" subtitle="Detected on this machine" back={back}>
      {status ? (
        <dl className="divide-y divide-border">
          {rows.map((row) => (
            <div key={row.label} className="flex flex-wrap items-baseline gap-x-4 gap-y-1 py-2.5">
              <dt className="flex w-32 shrink-0 items-center gap-1.5 text-[13px] text-muted">
                {row.bad && <span aria-hidden="true" className="size-2 rounded-full bg-err" />}
                {row.label}
              </dt>
              <dd className={cx("min-w-0 flex-1 text-[13px] [overflow-wrap:anywhere]", row.mono && "font-mono text-[12.5px]", row.bad && "text-err")}>
                {row.value}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <p className="text-[13px] text-muted">Mothership status unavailable.</p>
      )}
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Updates: which Colonizer this is, and whether a newer release is out (#45)
// ---------------------------------------------------------------------------

/// How far behind the running build is: the gap between when it was built and
/// when the newer release came out. Null when the release carries no date.
function daysBehind(builtAt: string, publishedAt: string | null): number | null {
  if (!publishedAt) return null;
  const gap = Date.parse(publishedAt) - Date.parse(builtAt);
  if (!Number.isFinite(gap) || gap <= 0) return null;
  return Math.floor(gap / 86_400_000);
}

/// One sentence about the gap, with the release date in it once.
function behindLabel(builtAt: string, publishedAt: string | null): string | null {
  const days = daysBehind(builtAt, publishedAt);
  if (days === null || !publishedAt) return null;
  const on = new Date(publishedAt).toLocaleDateString();
  if (days < 1) return `Released ${on}, the same day as the build you are running.`;
  return `Released ${on}, ${days} day${days === 1 ? "" : "s"} after the build you are running.`;
}

function UpdatesPane({
  update,
  onChanged,
  back,
}: {
  update: UpdateStatus | null;
  onChanged: (update: UpdateStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [applying, setApplying] = useState(false);

  // While an update is being applied the process is about to be replaced, so the
  // pane follows it until the answer stops coming.
  useEffect(() => {
    const phase = update?.apply.phase;
    if (phase !== "installing" && phase !== "restarting") return;
    const timer = setInterval(() => {
      api
        .update()
        .then(onChanged)
        .catch(() => {});
    }, 1500);
    return () => clearInterval(timer);
  }, [api, onChanged, update?.apply.phase]);

  const install = async () => {
    setApplying(true);
    try {
      await api.applyUpdate();
      onChanged(await api.update());
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setApplying(false);
    }
  };

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setUpdateCheck(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <p>
      While it is on, the Mothership asks GitHub every few hours whether a newer release of
      Colonizer-dev/harness is out. The request says nothing about this install; the live map is separate
      and off until you switch it on.
    </p>
  );

  return (
    <Pane title="Updates" subtitle="Which Colonizer this is, and whether a newer one is out" info={info} back={back}>
      {!update ? (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <div className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px]">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
              <span className="font-semibold text-[13px]">{update.installed.version}</span>
              {update.installed.dirty && <Badge tone="warn">built from a modified tree</Badge>}
            </div>
            <p className="mt-1 text-muted">
              {update.installed.commit ? (
                <>
                  commit <Code>{update.installed.commit.slice(0, 7)}</Code>,{" "}
                </>
              ) : null}
              built {new Date(update.installed.built_at).toLocaleString()}
              {update.installed.release && update.installed.release !== update.installed.version
                ? ` (after ${update.installed.release})`
                : ""}
            </p>
          </div>

          {update.available && update.latest && (
            <div className="rounded-xl border border-ok/40 bg-ok-soft px-3.5 py-2.5 text-[12.5px]">
              <p className="font-semibold text-[13px]">Colonizer {update.latest.version} is available</p>
              {behindLabel(update.installed.built_at, update.latest.published_at) && (
                <p className="text-muted">{behindLabel(update.installed.built_at, update.latest.published_at)}</p>
              )}
              {update.latest.notes && (
                <pre className="scroll-thin mt-1.5 max-h-48 overflow-auto whitespace-pre-wrap font-sans text-[12.5px] text-muted">
                  {update.latest.notes}
                </pre>
              )}
              <a className="mt-1.5 inline-flex items-center gap-1 text-accent hover:underline" href={update.latest.url} target="_blank" rel="noreferrer">
                Release notes <IconExternal size={12} />
              </a>
              <div className="mt-2.5 flex flex-wrap items-center gap-2">
                <Button
                  variant="primary"
                  disabled={applying || !update.can_apply.ok || update.apply.phase === "installing" || update.apply.phase === "restarting"}
                  onClick={() => void install()}
                >
                  {update.apply.phase === "installing" || update.apply.phase === "restarting" ? <Spinner /> : null}
                  {update.apply.phase === "installing"
                    ? "Installing…"
                    : update.apply.phase === "restarting"
                      ? "Restarting…"
                      : `Update to ${update.latest.version}`}
                </Button>
                {!update.can_apply.ok && <span className="text-muted">{update.can_apply.reason}</span>}
              </div>
              {update.apply.phase === "restarting" && (
                <p className="mt-1.5 text-muted">
                  Installed. The Mothership is restarting into it; colonies keep their microVMs and reconnect.
                </p>
              )}
              {update.apply.colonies.length > 0 && update.apply.phase !== "idle" && (
                <ul className="mt-1.5 space-y-0.5 text-muted">
                  {update.apply.colonies.map((c) => (
                    <li key={c.id}>
                      {c.repo} — {c.outcome}
                    </li>
                  ))}
                </ul>
              )}
              {update.apply.phase === "failed" && update.apply.error && (
                <p className="mt-1.5 text-err">
                  Update failed, and the running version is untouched: {update.apply.error}
                </p>
              )}
            </div>
          )}

          <Row id="update-check-switch" label="Check for new releases" inline>
            <Switch
              id="update-check-switch"
              labelledBy="update-check-switch-label"
              label="Check for new releases"
              checked={update.enabled}
              disabled={saving || update.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>

          {update.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
              Kept off by <Code>{update.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          {!update.enabled && !update.blocked_by && (
            <p className="text-[12.5px] text-muted">Off: the Mothership makes no request to GitHub about releases.</p>
          )}
          {update.error && <p className="text-[12.5px] text-err">Last check failed: {update.error}</p>}
          {update.enabled && update.last_checked && !update.error && (
            <p className="text-[12.5px] text-faint">Last checked {new Date(update.last_checked).toLocaleString()}.</p>
          )}
        </div>
      )}
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Live map: a heartbeat to colonizer.dev, off until switched on (docs/telemetry.md)
// ---------------------------------------------------------------------------

const TELEMETRY_DOCS = "https://colonizer.dev/docs/telemetry";

function LiveMapPane({
  telemetry,
  onChanged,
  back,
}: {
  telemetry: TelemetryStatus | null;
  onChanged: (telemetry: TelemetryStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setTelemetry(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <>
      <p>
        While it is on, the Mothership sends a heartbeat every 5 minutes, and within a minute when the number of running colonies changes.
        colonizer.dev/live shows a dot for its area, about 25 km across, lit while colonies run.
      </p>
      <p>
        Switching it off takes the dot away at once and forgets the random id, so a later period on the map can’t be tied to this
        one. The id is forgotten even if the service can’t be reached; then the dot goes out within 12 minutes instead of at once.
      </p>
    </>
  );

  return (
    <Pane title="Live map" subtitle="This mothership as a dot on colonizer.dev/live" info={info} back={back}>
      {!telemetry ? (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="live-map-switch" label="Show this mothership on the live map" inline>
            <Switch
              id="live-map-switch"
              labelledBy="live-map-switch-label"
              label="Show this mothership on the live map"
              checked={telemetry.enabled === true}
              disabled={saving || telemetry.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>
          {telemetry.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
              Kept off by <Code>{telemetry.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          <div>
            <h4 className="mb-1.5 text-[12.5px] font-semibold">What is sent</h4>
            <pre className="scroll-thin overflow-x-auto rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 font-mono text-[12px] leading-5">
              {JSON.stringify(
                { ...telemetry.heartbeat, install_id: telemetry.heartbeat.install_id ?? "(random, created when you switch it on)" },
                null,
                2,
              )}
            </pre>
            <p className="mt-2 text-[12.5px] text-muted">
              Nothing else: no repositories, issues, code, names or paths. The service sees this machine’s IP address, as any website
              would, turns it into a 25 km area and doesn’t store it. A heartbeat stops counting 12 minutes after it arrives, and
              its row is deleted about an hour after arrival, once anything else reaches the service. If this is the only mothership in its area, that dot is this one.
            </p>
          </div>
          {telemetry.enabled && (telemetry.last_sent_at || telemetry.last_error) && (
            <p className={cx("text-[12.5px] [overflow-wrap:anywhere]", telemetry.last_error ? "text-err" : "text-muted")}>
              {telemetry.last_error
                ? `Last heartbeat failed: ${telemetry.last_error}`
                : `Last heartbeat ${new Date(telemetry.last_sent_at!).toLocaleTimeString()}`}
            </p>
          )}
          <div className="flex flex-wrap gap-x-4 gap-y-1 text-[12.5px]">
            <a className="inline-flex items-center gap-1 text-accent hover:underline" href={telemetry.map_url} target="_blank" rel="noreferrer">
              Open the live map <IconExternal size={12} />
            </a>
            <a className="inline-flex items-center gap-1 text-accent hover:underline" href={TELEMETRY_DOCS} target="_blank" rel="noreferrer">
              How it works <IconExternal size={12} />
            </a>
          </div>
        </div>
      )}
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Usage data: the anonymous batch a sender would one day transmit — on by default, shown in full
// ---------------------------------------------------------------------------

function UsagePane({
  usage,
  onChanged,
  back,
}: {
  usage: UsageStatus | null;
  onChanged: (usage: UsageStatus) => void;
  back?: () => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);

  const set = async (enabled: boolean) => {
    setSaving(true);
    try {
      onChanged(await api.setUsage(enabled));
    } catch (e) {
      toast(errorMessage(e), "error");
    } finally {
      setSaving(false);
    }
  };

  const info = (
    <>
      <p>
        The batch is built on this machine and shown here, and that is all this release does: it has no sender — no endpoint, no
        background loop, no network call. A later one may carry the batch, composed from Cratefield’s module-telemetry.
      </p>
      <p>The batch carries a fresh random id while it is on — which is the default — and switching it off forgets that id, so a later period could never be tied to this one.</p>
    </>
  );

  return (
    <Pane title="Usage data" subtitle="An anonymous batch, collected and shown here only — nothing is sent" info={info} back={back}>
      {!usage ? (
        <p className="flex items-center gap-2 text-[13px] text-muted">
          <Spinner /> Loading…
        </p>
      ) : (
        <div className="space-y-4">
          <Row id="usage-switch" label="Allow an anonymous usage batch" inline>
            <Switch
              id="usage-switch"
              labelledBy="usage-switch-label"
              label="Allow an anonymous usage batch"
              checked={usage.enabled}
              disabled={saving || usage.blocked_by !== null}
              onChange={(checked) => void set(checked)}
            />
          </Row>
          {usage.blocked_by && (
            <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
              Kept off by <Code>{usage.blocked_by}</Code> in the Mothership’s environment.
            </p>
          )}
          <p className="text-[12.5px] text-muted">
            On by default, and nothing is sent yet: this build only collects the batch and shows it here. Switch it off here or with{" "}
            <Code>colonizer telemetry off</Code>; the Mothership’s environment can also hold it off whatever this switch says —{" "}
            <Code>COLONIZER_TELEMETRY=0</Code>, <Code>DO_NOT_TRACK=1</Code> or <Code>CI=true</Code>.
          </p>
          <div>
            <h4 className="mb-1.5 text-[12.5px] font-semibold">The whole batch</h4>
            <pre className="scroll-thin overflow-x-auto rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 font-mono text-[12px] leading-5">
              {JSON.stringify(usage.batch, null, 2)}
            </pre>
            <p className="mt-2 text-[12.5px] text-muted">
              Every byte a sender would transmit, verbatim — <Code>usage_id</Code> is <Code>null</Code> while the switch is off. Nothing
              else is in it: no repository, branch or issue names, no paths, no prompts or agent output, no tokens or URLs, and no setting
              values — setting names only.
            </p>
          </div>
        </div>
      )}
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Notifications: what tells a person a colony needs them when the tab is not
// in front. Client-side only — the prefs persist in localStorage (see
// notifications.ts), not through the Api, so there is nothing here to save.
// ---------------------------------------------------------------------------

function NotificationsPane({
  prefs,
  onChanged,
  back,
}: {
  prefs: NotificationPrefs;
  onChanged: Dispatch<SetStateAction<NotificationPrefs>>;
  back?: () => void;
}) {
  // The browser's answer as of the pane opening, or as of the last ask from the switch below.
  const [permission, setPermission] = useState<NotificationPermissionState>(() => notificationSupport());
  const [asked, setAsked] = useState(false);

  // Functional update: the permission answer below arrives after the dialog has kept taking
  // toggles, and a write from this render's `prefs` would silently revert them.
  const patch = (partial: Partial<NotificationPrefs>) => onChanged((previous) => ({ ...previous, ...partial }));

  // Turning browser notifications on asks the browser right here, inside the click: the prompt is
  // only shown within a user gesture, which is why this setting lives behind a button and can never
  // fire on load. The switch only comes on for "granted" — a denied or dismissed prompt is
  // explained below rather than silently swallowed.
  const setBrowser = (wanted: boolean) => {
    if (!wanted) {
      patch({ browser: false });
      return;
    }
    setAsked(true);
    void requestNotificationPermission().then((outcome) => {
      setPermission(outcome);
      patch({ browser: outcome === "granted" });
    });
  };

  const info = (
    <>
      <p>
        A colony that asks a question and then waits is otherwise quiet: a status pill in a sidebar that may be behind another window or
        another desk. The tab always shows what needs you, and that layer is never the only record — the sidebar list is — so nothing can
        be missed permanently.
      </p>
      <p>
        Notifications stay short and dull on purpose, because they land on screens other people can see: the repository and issue number
        only, never the issue title, the question a colony asked, or an error.
      </p>
    </>
  );

  // A permission the user revoked in the browser keeps the stored switch honest: it cannot count as on.
  const browserOn = prefs.browser && permission === "granted";

  return (
    <Pane title="Notifications" subtitle="How a colony that needs you gets your attention" info={info} back={back}>
      <div className="space-y-4">
        <Row
          id="notifications-in-tab"
          label="In this tab"
          info={<p>The count of colonies that need you in the tab title, a dot on the favicon, and a strip above the colony list.</p>}
          inline
        >
          <Switch
            id="notifications-in-tab"
            labelledBy="notifications-in-tab-label"
            label="In this tab"
            checked={prefs.inTab}
            onChange={(checked) => patch({ inTab: checked })}
          />
        </Row>
        <Row id="notifications-sound" label="Play a sound when a colony asks a question" inline>
          <Switch
            id="notifications-sound"
            labelledBy="notifications-sound-label"
            label="Play a sound when a colony asks a question"
            checked={prefs.sound}
            onChange={(checked) => patch({ sound: checked })}
          />
        </Row>
        <Row id="notifications-browser" label="Browser notifications while the tab is not in front" inline>
          <Switch
            id="notifications-browser"
            labelledBy="notifications-browser-label"
            label="Browser notifications while the tab is not in front"
            checked={browserOn}
            disabled={permission === "unsupported"}
            onChange={setBrowser}
          />
        </Row>
        {permission === "unsupported" && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
            This browser does not offer notifications — the API is missing, or the page is not on a secure origin.
          </p>
        )}
        {permission === "denied" && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
            The browser is blocking notifications for this site. Allow Colonizer in the browser’s own site settings, then switch this on
            here — this switch cannot lift a block the browser set.
          </p>
        )}
        {asked && permission === "default" && !prefs.browser && (
          <p className="rounded-xl border border-border bg-panel-2 px-3.5 py-2.5 text-[12.5px] text-muted">
            The permission prompt was dismissed without an answer. Switch it on again to ask once more.
          </p>
        )}

        <div>
          <h4 className="mb-1 text-[12.5px] font-semibold">Which events interrupt</h4>
          <p className="mb-1 text-[12.5px] text-muted">
            These gate the sound and the browser notifications. The tab title, the favicon and the strip always show every colony that needs
            you, whatever these say.
          </p>
          <Row id="notifications-event-question" label="A colony asks a question" inline>
            <Switch
              id="notifications-event-question"
              labelledBy="notifications-event-question-label"
              label="A colony asks a question"
              checked={prefs.events.question}
              onChange={(checked) => patch({ events: { ...prefs.events, question: checked } })}
            />
          </Row>
          <Row id="notifications-event-attention" label="A colony has stalled, or is out of nudges" inline>
            <Switch
              id="notifications-event-attention"
              labelledBy="notifications-event-attention-label"
              label="A colony has stalled, or is out of nudges"
              checked={prefs.events.attention}
              onChange={(checked) => patch({ events: { ...prefs.events, attention: checked } })}
            />
          </Row>
          <Row id="notifications-event-failed" label="A colony fails" inline>
            <Switch
              id="notifications-event-failed"
              labelledBy="notifications-event-failed-label"
              label="A colony fails"
              checked={prefs.events.failed}
              onChange={(checked) => patch({ events: { ...prefs.events, failed: checked } })}
            />
          </Row>
          <Row id="notifications-event-pull-request" label="A colony opens a pull request" inline>
            <Switch
              id="notifications-event-pull-request"
              labelledBy="notifications-event-pull-request-label"
              label="A colony opens a pull request"
              checked={prefs.events.pull_request}
              onChange={(checked) => patch({ events: { ...prefs.events, pull_request: checked } })}
            />
          </Row>
        </div>
      </div>
    </Pane>
  );
}

// ---------------------------------------------------------------------------
// Modules: one pane per kind, form generated from the provider's JSON Schema
// ---------------------------------------------------------------------------

const MODEL_KEYS = new Set(["model", "subagent_model", "background_model"]);

function valueOf(settings: Record<string, unknown>, key: string, field: SchemaField): unknown {
  if (settings[key] !== undefined) return settings[key];
  if (field.default !== undefined) return field.default;
  return field.type === "boolean" ? false : "";
}

/** What goes behind a field's "i": the schema description plus its default and range. */
function fieldInfo(field: SchemaField): ReactNode | null {
  const facts: string[] = [];
  if (field.default !== undefined && field.default !== null && field.default !== "") {
    facts.push(`Default ${typeof field.default === "boolean" ? (field.default ? "on" : "off") : String(field.default)}`);
  }
  if (field.minimum != null || field.maximum != null) facts.push(`Range ${field.minimum ?? "…"}–${field.maximum ?? "…"}`);
  if (!field.description && facts.length === 0) return null;
  return (
    <>
      {field.description && <p>{field.description}</p>}
      {facts.length > 0 && <p className="text-muted">{facts.join(" · ")}</p>}
    </>
  );
}

/** The Headroom bundle download (GET/POST /api/headroom), polled while it runs. */
function useHeadroom(active: boolean) {
  const api = useApi();
  const [status, setStatus] = useState<HeadroomStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!active) return;
    let stop = false;
    api
      .headroom()
      .then((s) => !stop && setStatus(s))
      .catch(() => {});
    return () => {
      stop = true;
    };
  }, [active, api]);

  const running = status?.state === "downloading" || status?.state === "unpacking";
  useEffect(() => {
    if (!active || !running) return;
    const timer = setInterval(() => {
      api
        .headroom()
        .then(setStatus)
        .catch(() => {});
    }, 1000);
    return () => clearInterval(timer);
  }, [active, api, running]);

  const start = useCallback(async () => {
    setError(null);
    try {
      setStatus(await api.headroomDownload());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [api]);

  return { status, error, start };
}

const megabytes = (bytes: number) => `${Math.round(bytes / 1048576)} MB`;

/** Shown in the agent pane while Headroom is switched on, or while its download runs or has failed. */
function HeadroomRow({ headroom }: { headroom: ReturnType<typeof useHeadroom> }) {
  const { status, error, start } = headroom;
  const box = "flex flex-wrap items-center gap-2 rounded-xl border border-border px-3.5 py-2.5 text-[12.5px]";

  if (error) {
    return (
      <div className={cx(box, "text-err")}>
        <div className="min-w-0 flex-1">Could not start the Headroom download: {error}</div>
        <Button size="sm" onClick={() => void start()}>
          Retry
        </Button>
      </div>
    );
  }
  if (!status) return null;
  switch (status.state) {
    case "unavailable":
      return <div className={cx(box, "text-muted")}>No Headroom bundle is published for this machine's architecture, so colonies run without it.</div>;
    case "installed":
      return (
        <div className={cx(box, "text-muted")}>
          <span className="text-ok">Headroom {status.release} is downloaded.</span> Colonies that start with it switched on use it.
        </div>
      );
    case "downloading":
    case "unpacking": {
      const pct = status.total ? Math.min(100, Math.round((status.bytes * 100) / status.total)) : null;
      return (
        <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-muted">
          <div className="mb-1.5 flex flex-wrap items-center gap-2">
            <Spinner />
            <span>
              {status.state === "unpacking" ? "Unpacking" : "Downloading"} Headroom {status.release}
              {status.state === "downloading" && status.total ? ` · ${megabytes(status.bytes)} of ${megabytes(status.total)}` : ""}
            </span>
            <span className="text-faint">happens once per release</span>
          </div>
          <div
            className="h-1 overflow-hidden rounded-full bg-border"
            role="progressbar"
            aria-label="Downloading Headroom"
            aria-valuenow={pct ?? undefined}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            {pct === null || status.state === "unpacking" ? (
              <div className="pull-slide h-full w-1/3 rounded-full bg-accent" />
            ) : (
              <div className="h-full rounded-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
            )}
          </div>
        </div>
      );
    }
    case "failed":
      return (
        <div className={cx(box, "text-err")}>
          <div className="min-w-0 flex-1">The Headroom download failed: {status.error}</div>
          <Button size="sm" onClick={() => void start()}>
            Retry
          </Button>
        </div>
      );
    default:
      return (
        <div className={cx(box, "text-muted")}>
          <div className="min-w-0 flex-1">Headroom downloads when you save with it switched on (220–245 MB, once). Colonies run without it until then. While it runs, it takes 300–370 MB of each colony’s memory.</div>
          <Button size="sm" onClick={() => void start()}>
            Download now
          </Button>
        </div>
      );
  }
}

/** Shown in the agent pane while Jev compaction is switched on: the data-egress and cost warning. */
export function JevCompactionNotice() {
  return (
    <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-warn">
      Sends this colony&apos;s conversation and tool-call history — file paths, command output — to TypeSafe
      (api.typesafe.ai) at each compaction. TypeSafe bills it directly: the cost isn&apos;t tracked by the
      Colonizer gateway or shown in colony cost. Read TypeSafe&apos;s data terms before using it on private repos.
    </div>
  );
}

function ImagePullRow({ pull }: { pull: ImagePull }) {
  const { status, error, start } = pull;
  // Re-render once a second while pulling so the elapsed time moves.
  const [, tick] = useState(0);
  useEffect(() => {
    if (status?.state !== "pulling") return;
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, [status?.state]);

  const line = (tone: string, body: ReactNode, action?: ReactNode) => (
    <div className={cx("flex flex-wrap items-center gap-2 rounded-xl border border-border px-3.5 py-2.5 text-[12.5px]", tone)}>
      <div className="min-w-0 flex-1">{body}</div>
      {action}
    </div>
  );

  if (error) {
    return line("text-err", <>Could not start the download: {error}</>, <Button size="sm" onClick={() => void start()}>Retry</Button>);
  }
  if (!status || status.state === "idle") {
    return line(
      "text-muted",
      <>The colony image downloads on first use. Get it now so the first colony boots straight away.</>,
      <Button size="sm" onClick={() => void start()}>
        Download image
      </Button>,
    );
  }
  if (status.state === "pulling") {
    return (
      <div className="rounded-xl border border-border px-3.5 py-2.5 text-[12.5px] text-muted">
        <div className="mb-1.5 flex flex-wrap items-center gap-2">
          <Spinner />
          <span>
            Downloading <code className="font-mono text-text">{status.image}</code> · {seconds(status.started_at)}s
          </span>
          <span className="text-faint">happens once per image</span>
        </div>
        {/* Indeterminate on purpose: msb reports no progress when it is not on a terminal. */}
        <div className="h-1 overflow-hidden rounded-full bg-border" role="progressbar" aria-label={`Downloading ${status.image}`}>
          <div className="pull-slide h-full w-1/3 rounded-full bg-accent" />
        </div>
      </div>
    );
  }
  if (status.state === "failed") {
    return line(
      "text-err",
      <>
        Download of <code className="font-mono">{status.image}</code> failed{status.error ? `: ${status.error}` : ""}. A colony will
        try again when it boots.
      </>,
      <Button size="sm" onClick={() => void start()}>
        Retry
      </Button>,
    );
  }
  const ready =
    status.state === "cached" ? (
      <>
        <code className="font-mono text-text">{status.image}</code> is already on this machine.
      </>
    ) : (
      <>
        <code className="font-mono text-text">{status.image}</code> is ready · downloaded in {seconds(status.started_at, status.finished_at)}s.
      </>
    );
  return line(
    "text-muted",
    <span className="inline-flex items-center gap-1.5">
      <IconCheck size={13} className="text-ok" />
      {ready}
    </span>,
  );
}

function ModulePane({
  module,
  draft,
  models,
  pull,
  back,
  onDraft,
  onReset,
  onSaved,
}: {
  module: ModuleInfo;
  draft: ModuleDraft;
  models?: ModelOption[];
  /** App owns the one image-pull poller; the sandbox pane and Setup read the same state. */
  pull: ImagePull;
  back?: () => void;
  onDraft: (patch: Partial<ModuleDraft>) => void;
  onReset: () => void;
  onSaved: (module: ModuleInfo) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const providerId = useId();
  const headroom = useHeadroom(module.kind === "agent");
  const info = kindInfo(module.kind);
  const fields = Object.entries(module.schema?.properties ?? {});
  const dirty = isDirty(module, draft);
  const providerInfo = module.providers.find((p) => p.id === draft.provider);

  const save = async () => {
    setSaving(true);
    try {
      const saved = await api.saveModule(module.kind, { provider: draft.provider, enabled: draft.enabled, settings: draft.settings });
      onSaved(saved);
      toast(`${info.title} module saved`);
      // Choosing a stack is the moment to download it, not the first launch.
      if (module.kind === "sandbox") void pull.start();
      // Switching Headroom on is the moment to download its bundle, too.
      if (module.kind === "agent" && saved.settings?.headroom === true) void headroom.start();
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setSaving(false);
    }
  };

  const setField = (key: string, value: unknown) => onDraft({ settings: { ...draft.settings, [key]: value } });

  // Ports, timers and paths fold under Advanced; what most people change stays on the page.
  const essentials = fields.filter(([key, field]) => !isAdvancedField(key, field));
  const advanced = fields.filter(([key, field]) => isAdvancedField(key, field));
  const renderField = ([key, field]: [string, SchemaField]) => {
    // Jev's tunables only mean anything with the switch on.
    if (
      module.kind === "agent" &&
      (key === "jev_keep_threshold" || key === "jev_preserve_recent") &&
      draft.settings.jev_compaction !== true
    )
      return null;
    const setting = (
      <SettingField
        key={key}
        name={key}
        field={field}
        value={valueOf(draft.settings, key, field)}
        onChange={(v) => setField(key, v)}
        models={models && MODEL_KEYS.has(key) ? models : undefined}
      />
    );
    // The download sits under the switch that asks for it, one divider group with it.
    const showHeadroom =
      module.kind === "agent" &&
      key === "headroom" &&
      (draft.settings.headroom === true || ["downloading", "unpacking", "failed"].includes(headroom.status?.state ?? ""));
    // The data-egress warning sits under the switch that asks for it, one divider group with it.
    const showJevWarning = module.kind === "agent" && key === "jev_compaction" && draft.settings.jev_compaction === true;
    return showHeadroom || showJevWarning ? (
      <div key={key} className="pb-2.5">
        {setting}
        {showHeadroom && <HeadroomRow headroom={headroom} />}
        {showJevWarning && <JevCompactionNotice />}
      </div>
    ) : (
      setting
    );
  };

  return (
    <Pane
      title={info.title}
      subtitle={info.description}
      back={back}
      aside={
        <span className="flex items-center gap-2 text-[12.5px] text-muted">
          <span aria-hidden="true">{draft.enabled ? "On" : "Off"}</span>
          <Switch checked={draft.enabled} onChange={(enabled) => onDraft({ enabled })} label={`${info.title} module enabled`} />
        </span>
      }
      footer={
        <>
          <span className="mr-auto text-[12.5px] text-muted">{dirty ? "Unsaved changes" : "Changes apply to new colonies"}</span>
          {dirty && (
            <Button variant="ghost" onClick={onReset}>
              Reset
            </Button>
          )}
          <Button variant="primary" disabled={!dirty || saving} onClick={save}>
            {saving && <Spinner />} Save
          </Button>
        </>
      }
    >
      {module.kind === "sandbox" && (
        <div className="mb-1">
          <ImagePullRow pull={pull} />
        </div>
      )}
      <div className={cx(!draft.enabled && "opacity-60")}>
      <div className="divide-y divide-border rounded-xl border border-border bg-panel px-4">
        <Row id={providerId} label="Provider" info={providerInfo?.description ? <p>{providerInfo.description}</p> : undefined}>
          {module.providers.length > 1 ? (
            <select id={providerId} value={draft.provider} onChange={(e) => onDraft({ provider: e.target.value })} className={inputClass}>
              {module.providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          ) : (
            <span id={providerId} className="block">
              <ModuleProviderMark id={draft.provider} name={providerInfo?.name ?? draft.provider} />
            </span>
          )}
        </Row>

        {draft.provider !== module.provider && fields.length > 0 && (
          <p className="py-2.5 text-[12.5px] text-warn">These fields belong to the current provider. Save to switch.</p>
        )}

        {essentials.map(renderField)}

        {module.kind === "memory" && draft.provider === "mem0" && <Mem0KeyRow />}

        {module.kind === "voice" && draft.provider !== "browser" && <VoiceKeyRow provider={draft.provider} name={providerInfo?.name ?? draft.provider} />}
        {module.kind === "voice" && <VoiceTestRow unsaved={dirty} />}

        {fields.length === 0 && module.providers.length <= 1 && <p className="py-3 text-[13px] text-faint">Nothing to configure.</p>}
      </div>
      {advanced.length > 0 && (
        <details className="group mt-3 rounded-xl border border-border bg-panel-2/40 [&_summary::-webkit-details-marker]:hidden">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-xl px-4 py-2.5 text-[13px] font-medium text-muted hover:text-text">
            <IconChevron size={14} className="transition-transform group-open:rotate-90" />
            Advanced
            <span className="font-normal text-faint">
              · {advanced.length} {advanced.length === 1 ? "setting" : "settings"}
            </span>
            <span className="ml-auto text-[12px] font-normal text-faint">Ports, timers and paths — the defaults suit most setups</span>
          </summary>
          <div className="divide-y divide-border border-t border-border px-4">{advanced.map(renderField)}</div>
        </details>
      )}
      </div>
    </Pane>
  );
}

/**
 * The mem0 key has its own row and its own save because it is not a module setting: settings go
 * to modules.json and come back from the API, and a key must do neither. Shown as soon as mem0 is
 * picked, so the key can be in place before the switch is saved.
 */
function Mem0KeyRow() {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<Mem0Status | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "remove" | "check" | null>(null);
  const [check, setCheck] = useState<Mem0Check | null>(null);

  useEffect(() => {
    api.mem0Status().then(setStatus, () => setStatus(null));
  }, [api]);

  const saveKey = async (value: string, kind: "save" | "remove") => {
    setBusy(kind);
    setCheck(null);
    try {
      setStatus(await api.saveMem0Key(value));
      setKey("");
      toast(kind === "save" ? "mem0 key saved" : "mem0 key removed");
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  const runCheck = async () => {
    setBusy("check");
    try {
      setCheck(await api.checkMem0());
    } catch (error) {
      setCheck({ ok: false, error: errorMessage(error) });
    } finally {
      setBusy(null);
    }
  };

  const state = !status
    ? "Checking…"
    : !status.has_key
      ? "Not set. Until it is, colonies start without shared memory."
      : status.source === "MEM0_API_KEY"
        ? "Read from MEM0_API_KEY."
        : "Saved on this machine.";

  return (
    <div className="space-y-2 py-2.5">
      <label htmlFor={id} className="block text-[13px] font-medium">
        mem0 API key
      </label>
      <p className="text-[12.5px] text-muted">{state} It stays on the Mothership: colonies never see it.</p>
      <form
        className="flex flex-wrap gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) void saveKey(key.trim(), "save");
        }}
      >
        <input
          id={id}
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder={status?.has_key ? "Replace the key" : "m0-…"}
          className={cx(inputClass, "min-w-48 flex-1")}
        />
        <Button type="submit" variant="primary" disabled={!key.trim() || busy !== null}>
          {busy === "save" && <Spinner />} Save
        </Button>
        {status?.source === "saved" && (
          <Button disabled={busy !== null} onClick={() => void saveKey("", "remove")}>
            {busy === "remove" && <Spinner />} Remove
          </Button>
        )}
        <Button disabled={!status?.has_key || busy !== null} onClick={() => void runCheck()}>
          {busy === "check" && <Spinner />} Check
        </Button>
      </form>
      {check && (
        <p role="status" className={cx("text-[12.5px]", check.ok ? "text-ok" : "text-err")}>
          {check.ok ? "mem0 accepted the key." : check.error}
        </p>
      )}
    </div>
  );
}

/**
 * A voice service's key, beside the module like mem0's: write-only, saved on the Mothership, never
 * in modules.json and never shown again. The status line says where the active key comes from — a
 * key already set on a matching model provider (OpenAI, Groq) is reused, so there may be nothing to add.
 */
export function VoiceKeyRow({ provider, name }: { provider: string; name: string }) {
  const api = useApi();
  const toast = useToast();
  const id = useId();
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "remove" | null>(null);

  useEffect(() => {
    api.voice().then(setStatus, () => setStatus(null));
  }, [api, provider]);

  const saveKey = async (value: string, kind: "save" | "remove") => {
    setBusy(kind);
    try {
      setStatus(await api.saveVoiceKey(provider, value));
      setKey("");
      toast(kind === "save" ? `${name} key saved` : `${name} key removed`);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setBusy(null);
    }
  };

  // The status describes the saved (active) provider; while another one is picked but unsaved,
  // its key state is unknown until the module is saved.
  const same = status?.provider === provider;
  const state = !status ? "Checking…" : !same ? "Save the module to see this service's key." : keySourceLabel(status.source, status.key_optional);

  return (
    <div className="space-y-2 py-2.5">
      <label htmlFor={id} className="block text-[13px] font-medium">
        {name} API key
      </label>
      <p className="text-[12.5px] text-muted">{state} It stays on the Mothership: the browser sends audio there, never the key.</p>
      <form
        className="flex flex-wrap gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) void saveKey(key.trim(), "save");
        }}
      >
        <input
          id={id}
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder={same && status?.has_key ? "Replace the key" : "Paste the key"}
          className={cx(inputClass, "min-w-48 flex-1")}
        />
        <Button type="submit" variant="primary" disabled={!key.trim() || busy !== null}>
          {busy === "save" && <Spinner />} Save
        </Button>
        {same && status?.source === "saved" && (
          <Button disabled={busy !== null} onClick={() => void saveKey("", "remove")}>
            {busy === "remove" && <Spinner />} Remove
          </Button>
        )}
      </form>
    </div>
  );
}

/** Records three seconds and runs them through the saved voice service, so a key and a microphone
 *  are proven together before the composer relies on them. */
export function VoiceTestRow({ unsaved }: { unsaved: boolean }) {
  const api = useApi();
  const [state, setState] = useState<{ phase: "idle" | "recording" | "transcribing" } | { phase: "done"; text: string } | { phase: "failed"; error: string }>({
    phase: "idle",
  });

  const run = async () => {
    try {
      const status = await api.voice();
      if (status.provider === "browser") {
        setState({ phase: "failed", error: "The browser recognises speech itself; there is no service to test. Pick one and save first." });
        return;
      }
      if (!status.configured) {
        setState({ phase: "failed", error: `${status.name} is not ready: add its key${status.key_optional ? " or base URL" : ""} first.` });
        return;
      }
      setState({ phase: "recording" });
      const recording = await startRecording();
      await new Promise((resolve) => setTimeout(resolve, 3000));
      const clip = await recording.stop();
      setState({ phase: "transcribing" });
      const { text } = await api.transcribe(clip);
      setState({ phase: "done", text });
    } catch (error) {
      setState({ phase: "failed", error: error instanceof DOMException && error.name === "NotAllowedError" ? "Microphone access was blocked" : errorMessage(error) });
    }
  };

  const busy = state.phase === "recording" || state.phase === "transcribing";
  return (
    <div className="space-y-2 py-2.5">
      <div className="flex flex-wrap items-center gap-3">
        <Button disabled={busy || unsaved || !canRecord()} onClick={() => void run()}>
          {busy && <Spinner />} Test microphone
        </Button>
        <span className="text-[12.5px] text-muted">
          {unsaved
            ? "Save first: the test uses the saved service."
            : state.phase === "recording"
              ? "Recording 3 seconds — say something…"
              : state.phase === "transcribing"
                ? "Transcribing…"
                : !canRecord()
                  ? "This browser can't record audio."
                  : "Records 3 seconds and transcribes them with the saved service."}
        </span>
      </div>
      {state.phase === "done" && (
        <p role="status" className="text-[13px] text-ok">
          {state.text ? `Heard: “${state.text}”` : "The service answered, but heard nothing."}
        </p>
      )}
      {state.phase === "failed" && (
        <p role="status" className="text-[12.5px] text-err">
          {state.error}
        </p>
      )}
    </div>
  );
}

function SettingField({
  name,
  field,
  value,
  onChange,
  models,
}: {
  name: string;
  field: SchemaField;
  value: unknown;
  onChange: (value: unknown) => void;
  /** Suggestions for free-text model fields. */
  models?: ModelOption[];
}) {
  const id = useId();
  const label = field.title ?? name;
  if (field.format === "plugin-dirs") {
    return <SkillsetField label={label} description={field.description} value={value} onChange={onChange} />;
  }
  const info = fieldInfo(field);
  const text = value === undefined || value === null ? "" : String(value);

  if (field.type === "boolean") {
    return (
      <Row id={id} label={label} info={info} inline>
        <Switch id={id} checked={Boolean(value)} onChange={onChange} label={label} labelledBy={`${id}-label`} />
      </Row>
    );
  }

  if (models && !field.enum) {
    return (
      <Row id={id} label={label} info={info}>
        <ModelInput
          id={id}
          value={text}
          onChange={onChange}
          models={models}
          placeholder={name === "subagent_model" ? "Same as orchestrator" : "Claude Code default"}
        />
      </Row>
    );
  }

  const numeric = field.type === "integer" || field.type === "number";
  return (
    <Row id={id} label={label} info={info}>
      {field.enum ? (
        <select
          id={id}
          value={text}
          onChange={(e) => {
            const raw = e.target.value;
            onChange(numeric ? Number(raw) : raw);
          }}
          className={inputClass}
        >
          {field.enum.map((option) => (
            <option key={String(option)} value={String(option)}>
              {option === "" ? "Default" : String(option)}
            </option>
          ))}
        </select>
      ) : (
        <input
          id={id}
          type={numeric ? "number" : "text"}
          inputMode={numeric ? "numeric" : undefined}
          value={text}
          min={field.minimum}
          max={field.maximum}
          step={field.type === "integer" ? 1 : undefined}
          onChange={(e) => {
            const raw = e.target.value;
            if (!numeric) onChange(raw);
            else onChange(raw === "" ? undefined : field.type === "integer" ? Math.trunc(Number(raw)) : Number(raw));
          }}
          className={cx(inputClass, numeric ? "w-32" : "font-mono text-[13px]")}
        />
      )}
    </Row>
  );
}

// ---------------------------------------------------------------------------
// Model providers (§6.3)
// ---------------------------------------------------------------------------

type ProviderDraft = ProviderLimits & {
  id: string;
  name: string;
  base_url: string;
  auth: ProviderAuth;
  wire: ProviderWire;
  models: string[];
  /** Prefilled from a catalogue entry's verified rates, if it has any; a built-in preset never sets this. */
  pricing?: ProviderPricing;
};

const DEFAULT_TIMEOUT = 600;
const DEFAULT_LIMITS: ProviderLimits = { timeout_secs: DEFAULT_TIMEOUT, max_concurrent: null, queue_timeout_secs: null, context_tokens: null, fallback_model: null };

const PRESETS: Record<ProviderPreset, ProviderDraft> = {
  deepseek: {
    id: "deepseek",
    name: "DeepSeek",
    base_url: "https://api.deepseek.com/anthropic",
    auth: "x-api-key",
    wire: "anthropic",
    models: ["deepseek-flash", "deepseek-v4-pro"],
    ...DEFAULT_LIMITS,
  },
  // OpenAI is not Anthropic-compatible: the gateway translates this one in both directions.
  openai: {
    id: "openai",
    name: "OpenAI",
    base_url: "https://api.openai.com",
    auth: "bearer",
    wire: "openai",
    models: ["gpt-5.6", "gpt-5.5"],
    ...DEFAULT_LIMITS,
    // Conservative: Claude Code compacts against this, and overshooting the real limit costs a failed turn.
    context_tokens: 272_000,
  },
  // Z.AI's coding plan speaks the Anthropic protocol; its key goes in an Authorization header.
  zai: {
    id: "zai",
    name: "Z.AI",
    base_url: "https://api.z.ai/api/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["glm-5.3", "glm-5.3-flash"],
    ...DEFAULT_LIMITS,
  },
  // Alibaba bills coding plans and token plans through different hosts; this is the token plan's.
  alibaba: {
    id: "alibaba",
    name: "Alibaba (Qwen)",
    base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
    auth: "bearer",
    wire: "anthropic",
    models: ["qwen3.7-plus", "qwen3.8-flash"],
    ...DEFAULT_LIMITS,
  },
  // Local servers are slow and usually serve one or two requests at a time.
  local: { id: "local", name: "Local", base_url: "http://127.0.0.1:8080", auth: "none", wire: "anthropic", models: [], ...DEFAULT_LIMITS, timeout_secs: 900, max_concurrent: 1 },
  custom: { id: "", name: "", base_url: "", auth: "x-api-key", wire: "anthropic", models: [], ...DEFAULT_LIMITS },
};

/** Integer fields of the Advanced group, with the ranges the Mothership accepts. */
const LIMIT_RANGE = {
  timeout_secs: [30, 3600],
  max_concurrent: [1, 64],
  queue_timeout_secs: [1, 3600],
  context_tokens: [1024, 2_000_000],
} as const;

type LimitKey = keyof typeof LIMIT_RANGE;

function parseLimit(key: LimitKey, raw: string): { value: number | null; error: string | null } {
  const text = raw.trim().replace(/[_,\s]/g, "");
  if (!text) return { value: null, error: null };
  const [min, max] = LIMIT_RANGE[key];
  const value = Number(text);
  if (!Number.isInteger(value) || value < min || value > max) {
    return { value: null, error: `A whole number from ${min.toLocaleString()} to ${max.toLocaleString()}` };
  }
  return { value, error: null };
}

const limitText = (value: number | null | undefined) => (value == null ? "" : String(value));

/** The four pricing rates, as they sit in the form's text fields. */
type PricingDraft = Record<keyof ProviderPricing, string>;

const PRICING_KEYS = ["input_per_mtok", "output_per_mtok", "cache_read_per_mtok", "cache_write_per_mtok", "thinking_per_mtok"] as const;

const emptyPricingDraft = (): PricingDraft => ({
  input_per_mtok: "",
  output_per_mtok: "",
  cache_read_per_mtok: "",
  cache_write_per_mtok: "",
  thinking_per_mtok: "",
});

const pricingDraftOf = (pricing: ProviderPricing | null | undefined): PricingDraft =>
  pricing ? Object.fromEntries(PRICING_KEYS.map((key) => [key, String(pricing[key] ?? "")])) as PricingDraft : emptyPricingDraft();

/** A rate is a dollar amount per million tokens: finite, never negative. Blank leaves the rate unset. */
function parsePrice(raw: string): { value: number | null; error: string | null } {
  const text = raw.trim().replace(/[_,\s]/g, "");
  if (!text) return { value: null, error: null };
  const value = Number(text);
  if (!Number.isFinite(value) || value < 0) return { value: null, error: "A dollar amount, 0 or more" };
  return { value, error: null };
}

/** The collapsed Pricing summary: the rates currently in the fields, at 0 decimals or as given. */
function pricingSummaryOf(pricing: Record<keyof ProviderPricing, { value: number | null }>): string[] {
  return PRICING_KEYS.map((key) => [key.replace(/_per_mtok$/, "").replace("_", " "), pricing[key].value] as const)
    .filter((entry): entry is [string, number] => entry[1] != null)
    .map(([label, value]) => `${label} $${value}`);
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

/** Short labels for non-default gateway settings, for rows and the collapsed Advanced summary. */
function limitLabels(limits: Partial<ProviderLimits>): string[] {
  const labels: string[] = [];
  if (limits.max_concurrent != null) labels.push(`max ${limits.max_concurrent}`);
  if (limits.timeout_secs != null && limits.timeout_secs !== DEFAULT_TIMEOUT) labels.push(`timeout ${limits.timeout_secs} s`);
  if (limits.queue_timeout_secs != null) labels.push(`queue ${limits.queue_timeout_secs} s`);
  if (limits.context_tokens != null) labels.push(`context ${formatTokens(limits.context_tokens)}`);
  if (limits.fallback_model) labels.push(`fallback ${limits.fallback_model}`);
  return labels;
}

const PRESET_LABEL: Record<ProviderPreset, string> = {
  deepseek: "DeepSeek",
  openai: "OpenAI",
  zai: "Z.AI",
  alibaba: "Alibaba",
  local: "Local",
  custom: "Custom",
};

/**
 * What sits in a provider's tile. Vendors with a CC0 mark in the icon set get it;
 * `local` and `custom` are not brands and keep a plain glyph. Anything else,
 * including OpenAI and Z.AI (no CC0 artwork exists for them) and every preset
 * added later, falls through to a lettermark built from the provider's name.
 */
const PRESET_MARK: Partial<Record<ProviderPreset | "anthropic", ComponentType<IconProps>>> = {
  anthropic: BrandClaude,
  deepseek: BrandDeepSeek,
  alibaba: BrandAlibabaCloud,
  local: IconServer,
  custom: IconPlug,
  // Catalogue vendors whose mark exists under CC0; the rest fall back to initials.
  kimi: BrandKimi,
  "kimi-for-coding": BrandKimi,
  minimax: BrandMiniMax,
  modelscope: BrandModelScope,
  openrouter: BrandOpenRouter,
  "github-copilot": BrandGitHubCopilot,
  "xai-grok": BrandXai,
  nvidia: BrandNvidia,
  xiaomi: BrandXiaomi,
};

/** One or two initials: the capitals of the name ("OpenAI" gives OA, "Z.AI" gives ZA), else the first letters of its words. */
function initialsOf(name: string): string {
  const capitals = name.replace(/[^A-Z]/g, "");
  if (capitals.length >= 2) return capitals.slice(0, 2);
  const words = name.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  const letters = words.map((w) => w[0]).join("").slice(0, 2).toUpperCase();
  return letters || capitals || "?";
}

/**
 * The tile at the start of a provider row or add button. Decorative: the vendor's
 * name is always beside it as text, so the tile is hidden from assistive tech.
 */
function ProviderMark({ preset, name, size = "row" }: { preset?: ProviderPreset | "anthropic"; name: string; size?: "row" | "button" | "tile" }) {
  const Mark = preset ? PRESET_MARK[preset] : undefined;
  const box = { tile: "size-11 rounded-xl", row: "size-8 rounded-lg", button: "size-[18px] rounded-[5px]" }[size];
  const glyph = { tile: 24, row: 18, button: 12 }[size];
  // Initials carry the whole tile when a vendor has no mark, so they scale with it.
  const initials = { tile: "text-[15px] font-semibold tracking-tight", row: "text-[11.5px] font-semibold tracking-tight", button: "text-[8.5px] font-bold" }[size];
  return (
    <span aria-hidden="true" className={cx("grid shrink-0 select-none place-items-center bg-panel-2 text-text", box, !Mark && initials)}>
      {Mark ? <Mark size={glyph} strokeWidth={size === "button" ? 2 : 1.75} /> : initialsOf(name)}
    </span>
  );
}

/** Shown while adding a provider, where the base URL is the thing people get wrong. */
const PRESET_HINT: Partial<Record<ProviderPreset, string>> = {
  zai: "Uses your Z.AI coding plan key as a bearer token.",
  alibaba: "This is the token plan's host. A coding plan key needs coding-intl.dashscope.aliyuncs.com instead — the two are not interchangeable.",
};

const WIRE_LABEL: Record<ProviderWire, string> = { anthropic: "Anthropic API", openai: "OpenAI API" };

const AUTH_LABEL: Record<ProviderAuth, string> = {
  "x-api-key": "API key (x-api-key header)",
  bearer: "Bearer token (Authorization header)",
  none: "No authentication",
};

type Editing = { mode: "new"; preset: ProviderPreset } | { mode: "edit"; id: string } | null;

/** Add buttons, in the order the presets are listed. */
const CATALOG_BY_ID = new Map(PROVIDER_CATALOG.map((entry) => [entry.id, entry]));

/** The starting values for a new provider: a built-in preset, a catalogue entry, or bare Custom. */
function presetDraft(preset: ProviderPreset): ProviderDraft {
  const built = PRESETS[preset];
  if (built) return built;
  const entry = CATALOG_BY_ID.get(preset);
  if (!entry) return PRESETS.custom;
  return {
    ...PRESETS.custom,
    id: entry.id,
    name: entry.name,
    base_url: entry.base_url,
    auth: entry.auth,
    wire: entry.wire,
    context_tokens: entry.context_tokens ?? PRESETS.custom.context_tokens,
    models: entry.models ?? PRESETS.custom.models,
    max_concurrent: entry.max_concurrent ?? PRESETS.custom.max_concurrent,
    pricing: entry.pricing ?? PRESETS.custom.pricing,
  };
}

/** A second (third, …) instance of a preset gets a free id and a suffixed name. */
function uniqueDraft(preset: ProviderPreset, takenIds: string[]): ProviderDraft {
  const base = presetDraft(preset);
  if (!takenIds.includes(base.id)) return base;
  let n = 2;
  while (takenIds.includes(`${base.id}-${n}`)) n++;
  return { ...base, id: `${base.id}-${n}`, name: `${base.name} (${n})` };
}

/** A catalogue entry's label, for the form header and the mark's fallback initials. */
function presetLabel(preset: ProviderPreset): string {
  return PRESET_LABEL[preset] ?? CATALOG_BY_ID.get(preset)?.name ?? "Provider";
}

const ADD_PRESETS: { preset: ProviderPreset; label: string }[] = [
  { preset: "deepseek", label: "DeepSeek" },
  { preset: "openai", label: "OpenAI" },
  { preset: "zai", label: "Z.AI" },
  { preset: "alibaba", label: "Alibaba" },
  { preset: "local", label: "Local server" },
  { preset: "custom", label: "Custom" },
];

function ProvidersPane({
  providers,
  error,
  setProviders,
  reload,
  claude,
  models,
  onOpenConnections,
  back,
}: {
  providers: ModelProvider[] | null;
  error: string | null;
  setProviders: (update: (list: ModelProvider[] | null) => ModelProvider[] | null) => void;
  reload: () => Promise<void>;
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  onOpenConnections: () => void;
  back?: () => void;
}) {
  const api = useApi();
  const addLabelId = useId();
  const [editing, setEditing] = useState<Editing>(null);
  const [browsing, setBrowsing] = useState(false);
  const [catalogQuery, setCatalogQuery] = useState("");
  const [health, setHealth] = useState<Record<string, HealthView>>({});

  // In-flight and queued counts change as colonies work; refresh them quietly.
  useEffect(() => {
    const timer = setInterval(() => {
      if (document.hidden) return;
      void reload();
    }, 5000);
    return () => clearInterval(timer);
  }, [reload]);

  const check = async (id: string) => {
    setHealth((h) => ({ ...h, [id]: { state: "checking" } }));
    try {
      const result = await api.providerHealth(id);
      setHealth((h) => ({ ...h, [id]: { state: "done", result } }));
    } catch (e) {
      setHealth((h) => ({ ...h, [id]: { state: "failed", message: errorMessage(e) } }));
    }
  };

  const upsert = (saved: ModelProvider) =>
    setProviders((list) => {
      const rest = (list ?? []).filter((p) => p.id !== saved.id);
      const index = (list ?? []).findIndex((p) => p.id === saved.id);
      if (index < 0) return [...rest, saved];
      rest.splice(index, 0, saved);
      return rest;
    });

  const addDisabled = !providers || editing !== null;

  return (
    <Pane
      title="Model providers"
      subtitle="Claude, and other Anthropic-compatible endpoints"
      back={back}
      info={
        <>
          <p>
            Claude is the default: a model picked by its plain id, such as <Code>sonnet</Code>, goes to Anthropic. Pick another provider's
            model as <Code>provider/model</Code>, for example <Code>deepseek/deepseek-flash</Code>, wherever you choose an orchestrator,
            subagent or background model.
          </p>
          <p className="text-muted">
            Requests go through the Mothership gateway: colonies never see provider keys, and servers on a private network (LAN, tailnet)
            work without extra setup.
          </p>
        </>
      }
    >
      <div className="space-y-3">
        <div className="space-y-2">
          <ClaudeRow claude={claude} models={models} onOpenConnections={onOpenConnections} />
          {error && <p className="text-[13px] text-err">{error}</p>}
          {!providers && !error && (
            <p className="flex items-center gap-2 py-1 text-[13px] text-muted">
              <Spinner /> Loading providers…
            </p>
          )}
        </div>
        {providers && (
          <div className="space-y-2">
            {providers.length === 0 && editing?.mode !== "new" && (
              <p className="rounded-xl border border-dashed border-border-strong px-3.5 py-4 text-center text-[13px] text-muted">
                No extra providers yet. Claude works without one.
              </p>
            )}
            {providers.map((provider) =>
              editing?.mode === "edit" && editing.id === provider.id ? (
                <ProviderForm
                  key={provider.id}
                  initial={provider}
                  preset={provider.preset}
                  takenIds={[]}
                  onCancel={() => setEditing(null)}
                  onSaved={(saved) => {
                    upsert(saved);
                    setHealth((h) => {
                      const next = { ...h };
                      delete next[saved.id];
                      return next;
                    });
                    setEditing(null);
                  }}
                  onDeleted={(id) => {
                    setProviders((list) => list?.filter((p) => p.id !== id) ?? null);
                    setEditing(null);
                  }}
                />
              ) : (
                <ProviderRow
                  key={provider.id}
                  provider={provider}
                  health={health[provider.id]}
                  disabled={editing !== null}
                  onCheck={() => void check(provider.id)}
                  onEdit={() => setEditing({ mode: "edit", id: provider.id })}
                />
              ),
            )}
            {editing?.mode === "new" && (
              <ProviderForm
                key={`new-${editing.preset}`}
                preset={editing.preset}
                takenIds={providers.map((p) => p.id)}
                onCancel={() => setEditing(null)}
                onSaved={(saved) => {
                  upsert(saved);
                  setEditing(null);
                }}
              />
            )}
          </div>
        )}
        <div role="group" aria-labelledby={addLabelId} className="space-y-2">
          <span id={addLabelId} className="block text-[12.5px] text-muted">
            Add a provider
          </span>
          <div className="grid grid-cols-3 gap-2 sm:grid-cols-6">
            {ADD_PRESETS.map(({ preset, label }) => {
              return (
                <button
                  key={preset}
                  type="button"
                  disabled={addDisabled}
                  onClick={() => setEditing({ mode: "new", preset })}
                  className={cx(
                    "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-border bg-panel px-1.5 py-3",
                    "text-[12px] font-medium text-text transition-colors hover:bg-panel-2",
                    "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
                  )}
                >
                  <ProviderMark preset={preset} name={presetLabel(preset)} size="tile" />
                  <span className="w-full truncate text-center">{label}</span>
                </button>
              );
            })}
            <button
              type="button"
              disabled={addDisabled}
              aria-expanded={browsing}
              onClick={() => setBrowsing((open) => !open)}
              className={cx(
                "flex cursor-pointer select-none flex-col items-center gap-2 rounded-xl border border-dashed border-border-strong bg-panel px-1.5 py-3",
                "text-[12px] font-medium text-muted transition-colors hover:bg-panel-2 hover:text-text",
                "disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:bg-panel",
              )}
            >
              <span aria-hidden="true" className="grid size-11 shrink-0 place-items-center rounded-xl bg-panel-2">
                <IconPlus size={22} />
              </span>
              <span className="w-full truncate text-center">{browsing ? "Close" : "More"}</span>
            </button>
          </div>
          {browsing && <CatalogBrowser query={catalogQuery} onQuery={setCatalogQuery} disabled={addDisabled} onPick={(id) => {
            setBrowsing(false);
            setCatalogQuery("");
            setEditing({ mode: "new", preset: id });
          }} />}
        </div>
        <p className="text-[11.5px] leading-snug text-faint">
          Logos and names are the property of their owners. Colonizer is not affiliated with, endorsed by or connected to any of them.
        </p>
      </div>
    </Pane>
  );
}

/**
 * Claude, at the top of the list. Not a provider anyone configured here: it is the
 * default the harness falls back to, so it is read-only, has no key field and no
 * endpoint to probe. Its state is the Connections card's, its models the ones
 * `/api/models` lists under `anthropic`.
 */
function ClaudeRow({ claude, models, onOpenConnections }: { claude: HarnessStatus["claude"] | null; models: ModelOption[]; onOpenConnections: () => void }) {
  const own = models.filter((m) => m.provider === "anthropic");
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <ProviderMark preset="anthropic" name="Anthropic" />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[14px] font-semibold">Anthropic</span>
          <Badge>Built-in</Badge>
          {claude && <Badge tone={claude.configured ? "ok" : "err"}>{claude.configured ? "Connected" : "Not connected"}</Badge>}
        </div>
        <div className="mt-0.5 text-[12px] text-muted">
          {claude?.configured ? [claude.account, claude.source ?? "Connected", "managed in Connections"].filter(Boolean).join(" · ") : "Managed in Connections"}
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {own.length === 0 ? (
            <span className="text-[12px] text-faint">Model list not loaded.</span>
          ) : (
            own.map((model) => (
              <span key={model.id} title={model.label} className="rounded bg-panel-2 px-1.5 py-px font-mono text-[11.5px] text-muted">
                {model.id}
              </span>
            ))
          )}
        </div>
        <p className="mt-1.5 text-[12px] text-faint">The default. A model id with no provider prefix, and a provider's fallback, go here.</p>
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" onClick={onOpenConnections}>
          Connections <IconChevron size={13} />
        </Button>
      </div>
    </div>
  );
}

function KeyBadge({ provider }: { provider: ModelProvider }) {
  if (provider.auth === "none") return <Badge>No key needed</Badge>;
  return provider.has_key ? <Badge tone="ok">Key saved</Badge> : <Badge tone="warn">No key</Badge>;
}

type HealthView = { state: "checking" } | { state: "done"; result: ProviderHealth } | { state: "failed"; message: string };

export function HealthStatus({ health, degraded }: { health: HealthView; degraded?: boolean }) {
  if (health.state === "checking") {
    return (
      <span role="status" className="flex items-center gap-1.5 text-[12px] text-muted">
        <Spinner className="size-3" /> Checking from the Mothership…
      </span>
    );
  }
  let tone: "ok" | "warn" | "err";
  let text: string;
  let title: string | undefined;
  if (health.state === "failed") {
    tone = "err";
    text = `Check failed: ${health.message}`;
  } else {
    const r = health.result;
    const latency = r.latency_ms != null ? `${Math.round(r.latency_ms)} ms` : null;
    const models = r.models.length ? `${r.models.length} model${r.models.length === 1 ? "" : "s"}` : null;
    // A note marks a non-2xx the Mothership judged healthy (an anthropic-wire endpoint with no
    // /v1/models), so it skips the HTTP warning.
    if (!r.reachable) {
      tone = "err";
      text = `Unreachable${r.error ? `: ${r.error}` : ""}`;
    } else if (!r.note && r.status != null && (r.status < 200 || r.status > 299)) {
      tone = "warn";
      text = [`HTTP ${r.status}`, latency, r.error].filter(Boolean).join(" · ");
    } else {
      tone = "ok";
      // A passing probe is one request; say so next to a provider failing a share of its real traffic.
      text = ["Reachable", latency, models ?? r.note, degraded ? "but failing real traffic" : null].filter(Boolean).join(" · ");
    }
    title = [
      r.models.length ? `Models: ${r.models.join(", ")}` : r.note ? "The endpoint does not list its models; requests route normally." : null,
      r.checked_at ? `Checked ${new Date(r.checked_at).toLocaleTimeString()}` : null,
    ]
      .filter(Boolean)
      .join("\n");
  }
  return (
    <span
      role="status"
      title={title || undefined}
      className={cx("flex items-start gap-1.5 text-[12px] font-medium", tone === "ok" ? "text-ok" : tone === "warn" ? "text-warn" : "text-err")}
    >
      <span className="mt-[5px] size-1.5 shrink-0 rounded-full bg-current" />
      <span className="min-w-0 [overflow-wrap:anywhere]">{text}</span>
    </span>
  );
}

/** Model settings by the names the model form labels them, for the usage line. */
const MODEL_SETTING_LABEL: Record<ModelSetting, string> = {
  model: "Orchestrator model",
  subagent_model: "Subagent model",
  background_model: "Background model",
  model_low: "Model for small tasks",
  model_high: "Model for large tasks",
};

/**
 * Why a provider wired only to subagent or background work can look idle, as one clause: the
 * orchestrator does nearly all of a colony's work, subagent traffic only appears when a colony
 * delegates (rare) and background calls are small auxiliary jobs. Null when the provider is on
 * the main path, or when no setting points at it — that case gets its own, sharper wording.
 */
function idleWiringNote(usedBy: ModelSetting[]): string | null {
  if (usedBy.length === 0 || usedBy.includes("model")) return null;
  const names = usedBy.map((setting) => MODEL_SETTING_LABEL[setting]);
  const settings =
    names.length === 1
      ? `the ${names[0]} setting`
      : `the ${names.slice(0, -1).join(", ")} and ${names[names.length - 1]} settings`;
  return `only wired to ${settings} — the Orchestrator model does nearly all of a colony's work, so it can look idle`;
}

/** The failure-rate segment's tooltip: the Mothership owns the rule, the line only reports it. */
const FAILURE_RATE_TITLE =
  "Failures as a share of the requests counted at the gateway. The Mothership calls a provider degraded at 10% or more of at least 50 requests; below that the rate is shown without a verdict.";
const AVG_LATENCY_TITLE = "Mean time of the requests dispatched to this provider, time spent queued excluded.";

/**
 * The cumulative usage line on a provider card. Its most important job is telling "never used"
 * from "used and working" at a glance: `Reachable` alone once read as "in use" when it only meant
 * the endpoint answered. An absent `usage` means the Mothership didn't report one — not the same
 * thing as never used. Wiring notes explain, they don't advise — an idle provider is not broken.
 */
function UsageLine({ provider }: { provider: ModelProvider }) {
  const usage = provider.usage;
  const requests = usage?.requests ?? 0;
  const failures = usage?.failures ?? 0;
  const fallbacks = usage?.fallbacks ?? 0;
  const lastUsed = timeAgo(usage?.last_request_at);
  const total = formatDuration(usage?.duration_ms ?? 0);
  const usedBy = provider.used_by;
  const wiring = idleWiringNote(usedBy ?? []);
  const health = provider.health;
  const tone = usageHealthTone(health);
  const rate = failureRateText(health, requests);
  const avg = avgLatencyText(health, requests);
  const since = formatSince(usage?.since);

  const segments: { text: string; title?: string; tone?: "warn" | "err" | "lift" }[] = [];
  if (!usage) {
    segments.push({ text: "No usage recorded — this Mothership doesn't report usage", tone: "lift" });
  } else if (requests === 0) {
    segments.push({ text: "Never used", tone: "lift" });
  } else {
    if (rate)
      segments.push({
        text: rate,
        title: FAILURE_RATE_TITLE,
        tone: tone === "err" ? "err" : undefined,
      });
    if (avg) segments.push({ text: avg, title: AVG_LATENCY_TITLE });
    segments.push({ text: `${requests.toLocaleString()} ${requests === 1 ? "request" : "requests"}${since ? ` since ${since}` : ""}` });
    if (lastUsed) segments.push({ text: `last used ${lastUsed}` });
    if (failures > 0)
      segments.push({
        text: `${failures.toLocaleString()} failed${fallbacks > 0 ? `, ${fallbacks.toLocaleString()} of them got a Claude fallback` : ""}`,
        title:
          "Failed means no usable response came back — a queue timeout, an unreachable provider, a request timeout, an upstream status of 400 or more, or an OpenAI-wire body that failed or never finished. A failure part-way through a streamed body is not counted. The fallbacks are a prediction, not an observation: the Mothership answered those with a fallback response, which the colony's router retries on Claude.",
        tone: "warn",
      });
    if (total !== "0s") segments.push({ text: `${total} total` });
  }
  if (usedBy && usedBy.length === 0) {
    segments.push({ text: "no model setting points at it as configured", tone: requests === 0 ? "lift" : undefined });
  } else if (wiring) {
    segments.push({ text: wiring });
  }
  if (provider.quota_exhausted) {
    segments.push({ text: quotaExhaustedText(provider.quota_exhausted) ?? "quota exhausted", tone: quotaTone(provider.quota_exhausted) ?? undefined });
  }

  return (
    <div className="mt-1 text-[12px] text-faint" title="Counted at the Mothership gateway since it first kept tally; the counts survive a restart.">
      {segments.map((segment, index) => (
        <span key={index}>
          {index > 0 && " · "}
          <span
            title={segment.title}
            className={cx(
              segment.tone === "warn" && "text-warn",
              segment.tone === "err" && "font-medium text-err",
              segment.tone === "lift" && "font-medium text-muted",
            )}
          >
            {segment.text}
          </span>
        </span>
      ))}
    </div>
  );
}

function ProviderRow({
  provider,
  health,
  disabled,
  onCheck,
  onEdit,
}: {
  provider: ModelProvider;
  health?: HealthView;
  disabled: boolean;
  onCheck: () => void;
  onEdit: () => void;
}) {
  const running = provider.in_flight ?? 0;
  const queued = provider.queued ?? 0;
  const limits = limitLabels(provider);
  return (
    <div className="flex flex-wrap items-start gap-x-3 gap-y-2 rounded-xl border border-border px-3.5 py-3">
      <ProviderMark preset={provider.preset} name={provider.name} />
      <div className="min-w-0 flex-1 basis-48">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[14px] font-semibold">{provider.name}</span>
          <span className="font-mono text-[12px] text-faint">{provider.id}</span>
          <KeyBadge provider={provider} />
          {provider.wire === "openai" && (
            <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
              {WIRE_LABEL.openai}
            </Badge>
          )}
          {(running > 0 || queued > 0) && (
            <Badge tone={queued > 0 ? "warn" : "info"} pulse={running > 0}>
              {[running > 0 && `${running} running`, queued > 0 && `${queued} queued`].filter(Boolean).join(" · ")}
            </Badge>
          )}
          {provider.health?.degraded && (
            <Badge
              tone="err"
              title={`${formatFailureRate(provider.health.failure_pct)} of requests failed, ${formatAvgLatency(provider.health.avg_latency_ms)} on average. The Mothership rates a provider degraded past 10% failed.`}
            >
              Degraded
            </Badge>
          )}
        </div>
        <div className="mt-0.5 font-mono text-[12px] text-muted [overflow-wrap:anywhere]">{provider.base_url}</div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          {provider.models.length === 0 ? (
            <span className="text-[12px] text-faint">No models listed; type model IDs where you pick a model.</span>
          ) : (
            provider.models.map((model) => (
              <span key={model} className="rounded bg-panel-2 px-1.5 py-px font-mono text-[11.5px] text-muted">
                {model}
              </span>
            ))
          )}
        </div>
        {limits.length > 0 && <div className="mt-1 text-[12px] text-faint">{limits.join(" · ")}</div>}
        <UsageLine provider={provider} />
        {health && (
          <div className="mt-2">
            <HealthStatus health={health} degraded={provider.health?.degraded} />
          </div>
        )}
      </div>
      <div className="flex shrink-0 gap-1.5">
        <Button size="sm" disabled={health?.state === "checking"} onClick={onCheck} title="Check that the Mothership can reach this provider">
          {health?.state === "checking" ? <Spinner className="size-3" /> : <IconNetwork size={13} />} Check
        </Button>
        <Button size="sm" disabled={disabled} onClick={onEdit}>
          <IconPencil size={13} /> Edit
        </Button>
      </div>
    </div>
  );
}

const PROVIDER_ID = /^[a-z0-9][a-z0-9-]{0,31}$/;

/** A labelled field in the provider form: label and "i" above, control below, error under. */
function FormField({
  id,
  label,
  info,
  error,
  hint,
  className,
  children,
}: {
  id: string;
  label: string;
  info?: ReactNode;
  error?: string | null;
  hint?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("min-w-0 space-y-1", className)}>
      <div className="flex items-center gap-1">
        <label htmlFor={id} className="text-[12.5px] font-medium text-muted">
          {label}
        </label>
        {info && <InfoButton label={label}>{info}</InfoButton>}
      </div>
      {children}
      {error ? <span className="block text-[12px] text-err">{error}</span> : hint ? <span className="block text-[12px] text-faint">{hint}</span> : null}
    </div>
  );
}


/** The host part of a base URL, which is what tells two endpoints apart in a list. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * The long tail of Anthropic-compatible endpoints, searchable. Kept behind "More" because six
 * vendors cover almost everyone and seventy would bury them.
 */
function CatalogBrowser({
  query,
  onQuery,
  disabled,
  onPick,
}: {
  query: string;
  onQuery: (value: string) => void;
  disabled: boolean;
  onPick: (id: string) => void;
}) {
  const needle = query.trim().toLowerCase();
  const matches = PROVIDER_CATALOG.filter(
    (entry: CatalogEntry) =>
      !needle || entry.name.toLowerCase().includes(needle) || entry.base_url.toLowerCase().includes(needle),
  );
  return (
    <div className="space-y-2 rounded-xl border border-border bg-panel p-2.5">
      <input
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        placeholder="Search providers"
        aria-label="Search providers"
        autoFocus
        className={inputClass}
      />
      <ul className="scroll-thin max-h-64 space-y-0.5 overflow-y-auto">
        {matches.map((entry) => (
          <li key={entry.id}>
            <button
              type="button"
              disabled={disabled}
              onClick={() => onPick(entry.id)}
              className="flex w-full cursor-pointer items-center gap-2.5 rounded-lg px-2 py-1.5 text-left hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-45"
            >
              <ProviderMark preset={entry.id} name={entry.name} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium">{entry.name}</span>
                <span className="block truncate font-mono text-[11.5px] text-faint">{hostOf(entry.base_url)}</span>
              </span>
              {entry.wire === "openai" && <Badge tone="info">{WIRE_LABEL.openai}</Badge>}
            </button>
          </li>
        ))}
        {matches.length === 0 && <li className="px-2 py-3 text-[13px] text-faint">Nothing matches that.</li>}
      </ul>
      <p className="px-1 text-[11.5px] leading-snug text-faint">
        {PROVIDER_CATALOG.length} endpoints, from the cc-switch catalogue. Colonizer neither vets nor endorses them, and many resell
        access rather than run the model themselves.
      </p>
    </div>
  );
}

function ProviderForm({
  initial,
  preset,
  takenIds,
  onCancel,
  onSaved,
  onDeleted,
}: {
  initial?: ModelProvider;
  preset: ProviderPreset;
  takenIds: string[];
  onCancel: () => void;
  onSaved: (provider: ModelProvider) => void;
  onDeleted?: (id: string) => void;
}) {
  const api = useApi();
  const toast = useToast();
  const start = initial ?? uniqueDraft(preset, takenIds);
  const wire = initial?.wire ?? presetDraft(preset).wire;
  const [id, setId] = useState(start.id);
  const [name, setName] = useState(start.name);
  const [baseUrl, setBaseUrl] = useState(start.base_url);
  const [auth, setAuth] = useState<ProviderAuth>(start.auth);
  const [models, setModels] = useState<string[]>(start.models);
  const [keyMode, setKeyMode] = useState<"keep" | "replace" | "remove">(initial?.has_key ? "keep" : "replace");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<"save" | "delete" | null>(null);
  const [limitDraft, setLimitDraft] = useState<Record<LimitKey, string>>({
    timeout_secs: limitText(start.timeout_secs),
    max_concurrent: limitText(start.max_concurrent),
    queue_timeout_secs: limitText(start.queue_timeout_secs),
    context_tokens: limitText(start.context_tokens),
  });
  const [fallback, setFallback] = useState(start.fallback_model ?? "");
  // The saved rates sit in the fields; `pricing` only goes on the save once they differ from them, the
  // same convention as the key: omitted keeps what is saved, so a save never silently rezeros a rate.
  const [pricingDraft, setPricingDraft] = useState<PricingDraft>(() => pricingDraftOf(start.pricing));
  // A catalogue entry whose base URL has ${…} holes: ask for them, and the URL follows.
  const template = initial ? [] : (CATALOG_BY_ID.get(preset)?.variables ?? []);
  const [vars, setVars] = useState<Record<string, string>>(() =>
    Object.fromEntries(template.map((v) => [v.name, v.default ?? ""])),
  );
  // A new Local provider opens Advanced so the prefilled limits are visible.
  const [advancedOpen, setAdvancedOpen] = useState(!initial && preset === "local");
  const anthropicModels = useModels().filter((m) => m.provider === "anthropic");
  const ids = {
    id: useId(),
    name: useId(),
    url: useId(),
    auth: useId(),
    key: useId(),
    models: useId(),
    fallback: useId(),
  };
  const limits = {
    timeout_secs: parseLimit("timeout_secs", limitDraft.timeout_secs),
    max_concurrent: parseLimit("max_concurrent", limitDraft.max_concurrent),
    queue_timeout_secs: parseLimit("queue_timeout_secs", limitDraft.queue_timeout_secs),
    context_tokens: parseLimit("context_tokens", limitDraft.context_tokens),
  };
  const limitsInvalid = Object.values(limits).some((l) => l.error);
  const setLimit = (k: LimitKey, value: string) => setLimitDraft((d) => ({ ...d, [k]: value }));
  const pricing = {
    input_per_mtok: parsePrice(pricingDraft.input_per_mtok),
    output_per_mtok: parsePrice(pricingDraft.output_per_mtok),
    cache_read_per_mtok: parsePrice(pricingDraft.cache_read_per_mtok),
    cache_write_per_mtok: parsePrice(pricingDraft.cache_write_per_mtok),
    thinking_per_mtok: parsePrice(pricingDraft.thinking_per_mtok),
  };
  const pricingInvalid = Object.values(pricing).some((rate) => rate.error);
  const setPricing = (k: keyof ProviderPricing, value: string) => setPricingDraft((d) => ({ ...d, [k]: value }));
  const savedPricing = initial?.pricing;
  const pricingChanged =
    (pricing.input_per_mtok.value ?? 0) !== (savedPricing?.input_per_mtok ?? 0) ||
    (pricing.output_per_mtok.value ?? 0) !== (savedPricing?.output_per_mtok ?? 0) ||
    (pricing.cache_read_per_mtok.value ?? 0) !== (savedPricing?.cache_read_per_mtok ?? 0) ||
    (pricing.cache_write_per_mtok.value ?? 0) !== (savedPricing?.cache_write_per_mtok ?? 0);
  const advancedSummary = limitLabels({
    timeout_secs: limits.timeout_secs.value ?? undefined,
    max_concurrent: limits.max_concurrent.value,
    queue_timeout_secs: limits.queue_timeout_secs.value,
    context_tokens: limits.context_tokens.value,
    fallback_model: fallback || null,
  });
  const pricingSummary = pricingSummaryOf(pricing);

  const isNew = !initial;
  const idError = !isNew
    ? null
    : !PROVIDER_ID.test(id)
      ? "Lowercase letters, digits and dashes"
      : id === "anthropic"
        ? "“anthropic” is reserved"
        : takenIds.includes(id)
          ? "Already in use"
          : null;
  // What actually gets saved and validated: the template with its holes filled.
  const url = template.length ? fillTemplate(baseUrl, vars) : baseUrl;
  const unfilled = template.filter((v) => !vars[v.name]?.trim());
  const urlError = unfilled.length
    ? `Fill in ${unfilled.map((v) => v.label).join(" and ")}`
    : /^https?:\/\/[^\s/]+(\/\S*)?$/.test(url.trim())
      ? null
      : "An http(s) URL";
  const keyError = auth !== "none" && keyMode === "replace" && initial?.has_key && !key.trim() ? "Paste the new key" : null;
  const invalid = Boolean(idError || urlError || keyError || limitsInvalid || pricingInvalid || !name.trim());
  const loopback = /^https?:\/\/(127\.|localhost|\[::1\])/.test(url.trim());

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (invalid) return;
    setBusy("save");
    let api_key: string | undefined;
    if (auth !== "none") {
      if (keyMode === "remove") api_key = "";
      else if (keyMode === "replace" && key.trim()) api_key = key.trim();
    }
    try {
      const saved = await api.saveProvider(id, {
        name: name.trim(),
        base_url: url.trim(),
        auth,
        wire,
        models,
        preset: initial?.preset ?? preset,
        api_key,
        pricing: pricingChanged
          ? {
              input_per_mtok: pricing.input_per_mtok.value ?? 0,
              output_per_mtok: pricing.output_per_mtok.value ?? 0,
              cache_read_per_mtok: pricing.cache_read_per_mtok.value ?? 0,
              cache_write_per_mtok: pricing.cache_write_per_mtok.value ?? 0,
            }
          : undefined,
        timeout_secs: limits.timeout_secs.value,
        max_concurrent: limits.max_concurrent.value,
        queue_timeout_secs: limits.queue_timeout_secs.value,
        context_tokens: limits.context_tokens.value,
        fallback_model: fallback || null,
      });
      toast(`${saved.name} saved`);
      onSaved(saved);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  const remove = async () => {
    if (!initial || !window.confirm(`Remove ${initial.name}? Colonies using ${initial.id}/… models will fall back to the default model.`)) return;
    setBusy("delete");
    try {
      await api.deleteProvider(initial.id);
      toast(`${initial.name} removed`);
      onDeleted?.(initial.id);
    } catch (e) {
      toast(errorMessage(e), "error");
      setBusy(null);
    }
  };

  return (
    <form onSubmit={save} className="space-y-3 rounded-xl border border-accent/40 bg-panel p-3.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[13.5px] font-semibold">{isNew ? `New ${presetLabel(preset)} provider` : `Edit ${initial.name}`}</span>
        {!isNew && <KeyBadge provider={initial} />}
        {wire === "openai" && (
          <Badge tone="info" title="Speaks the OpenAI protocol; the Mothership gateway translates">
            {WIRE_LABEL.openai}
          </Badge>
        )}
      </div>
      {isNew && PRESET_HINT[preset] && <p className="text-[12.5px] text-muted">{PRESET_HINT[preset]}</p>}
      <div className="grid gap-3 sm:grid-cols-2">
        <FormField
          id={ids.id}
          label="ID"
          error={idError && id ? idError : null}
          hint={isNew ? `Models are picked as ${id || "id"}/model` : "Can't be changed"}
          info={isNew ? <p>Lowercase letters, digits and dashes. Fixed once saved.</p> : undefined}
        >
          <input
            id={ids.id}
            value={id}
            onChange={(e) => setId(e.target.value.toLowerCase())}
            disabled={!isNew}
            placeholder="my-provider"
            spellCheck={false}
            aria-invalid={Boolean(idError && id)}
            className={cx(inputClass, "font-mono text-[13px] disabled:opacity-60")}
          />
        </FormField>
        <FormField id={ids.name} label="Name">
          <input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} placeholder="My provider" className={inputClass} />
        </FormField>
        {template.map((variable) => (
          <FormField
            key={variable.name}
            id={`${ids.url}-${variable.name}`}
            label={variable.label}
            info={<p>Part of this provider's address, so the URL below is only complete once it is filled in.</p>}
          >
            <input
              id={`${ids.url}-${variable.name}`}
              value={vars[variable.name] ?? ""}
              onChange={(e) => setVars((v) => ({ ...v, [variable.name]: e.target.value }))}
              placeholder={variable.placeholder}
              spellCheck={false}
              className={cx(inputClass, "font-mono text-[13px]")}
            />
          </FormField>
        ))}
        <FormField
          id={ids.url}
          label="Base URL"
          className="sm:col-span-2"
          error={urlError && baseUrl && !unfilled.length ? urlError : null}
          hint={
            unfilled.length
              ? urlError ?? undefined
              : loopback
                ? "The Mothership connects to this address, so localhost is the Mothership itself."
                : undefined
          }
        >
          <input
            id={ids.url}
            value={template.length ? url : baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            readOnly={template.length > 0}
            placeholder={wire === "openai" ? "https://api.openai.com" : "https://api.example.com/anthropic"}
            spellCheck={false}
            aria-invalid={Boolean(urlError && baseUrl && !unfilled.length)}
            className={cx(inputClass, "font-mono text-[13px]", template.length > 0 && "text-muted")}
          />
        </FormField>
        <FormField id={ids.auth} label="Authentication">
          <select id={ids.auth} value={auth} onChange={(e) => setAuth(e.target.value as ProviderAuth)} className={inputClass}>
            {(Object.keys(AUTH_LABEL) as ProviderAuth[]).map((mode) => (
              <option key={mode} value={mode}>
                {AUTH_LABEL[mode]}
              </option>
            ))}
          </select>
        </FormField>
        <FormField id={ids.key} label={auth === "bearer" ? "Token" : "API key"} error={keyError}>
          {auth === "none" ? (
            <p id={ids.key} className="flex h-9 items-center text-[13px] text-faint">
              Not needed
            </p>
          ) : keyMode === "keep" ? (
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center gap-1.5 text-[13px] text-ok">
                <IconCheck size={14} /> Saved
              </span>
              <Button size="sm" onClick={() => setKeyMode("replace")}>
                Replace
              </Button>
              <Button size="sm" variant="danger" onClick={() => setKeyMode("remove")}>
                Remove
              </Button>
            </div>
          ) : keyMode === "remove" ? (
            <div id={ids.key} className="flex flex-wrap items-center gap-2">
              <span className="inline-flex h-9 items-center text-[13px] text-warn">Removed on save</span>
              <Button size="sm" variant="ghost" onClick={() => setKeyMode("keep")}>
                Undo
              </Button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <input
                id={ids.key}
                type="password"
                autoComplete="off"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={initial?.has_key ? "New key" : "sk-…"}
                aria-invalid={Boolean(keyError)}
                className={cx(inputClass, "font-mono text-[13px]")}
              />
              {initial?.has_key && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setKey("");
                    setKeyMode("keep");
                  }}
                >
                  Keep saved
                </Button>
              )}
            </div>
          )}
        </FormField>
        <FormField
          id={ids.models}
          label="Models"
          className="sm:col-span-2"
          info={<p>Model IDs as the endpoint expects them. Enter or a comma adds one; leave empty to type IDs where you pick a model.</p>}
        >
          <ChipsInput id={ids.models} values={models} onChange={setModels} placeholder={models.length ? "Add another" : "deepseek-flash, qwen3-coder, …"} />
        </FormField>
        <details
          open={advancedOpen}
          onToggle={(e) => setAdvancedOpen(e.currentTarget.open)}
          className="group min-w-0 rounded-lg border border-border sm:col-span-2"
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-[13px] hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Advanced</span>
            <span className={cx("min-w-0 flex-1 truncate text-[12px]", limitsInvalid ? "text-err" : "text-faint")}>
              {limitsInvalid
                ? "Some values are out of range"
                : advancedSummary.length
                  ? advancedSummary.join(" · ")
                  : "Timeouts, concurrency, context window, fallback"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <LimitField
              label="Request timeout (s)"
              value={limitDraft.timeout_secs}
              onChange={(v) => setLimit("timeout_secs", v)}
              placeholder={String(DEFAULT_TIMEOUT)}
              error={limits.timeout_secs.error}
              help={`How long the Mothership waits for a response. Blank uses ${DEFAULT_TIMEOUT}.`}
            />
            <LimitField
              label="Queue timeout (s)"
              value={limitDraft.queue_timeout_secs}
              onChange={(v) => setLimit("queue_timeout_secs", v)}
              placeholder="Same as request timeout"
              error={limits.queue_timeout_secs.error}
              help="How long a request may wait for a free slot."
            />
            <LimitField
              label="Max concurrent requests"
              value={limitDraft.max_concurrent}
              onChange={(v) => setLimit("max_concurrent", v)}
              placeholder="Unlimited"
              error={limits.max_concurrent.error}
              help="Requests beyond this wait in a queue on the Mothership; a local server usually handles 1-2. Left empty it is unlimited: the request rate is every running colony times its subagents."
            />
            <LimitField
              label="Context window (tokens)"
              value={limitDraft.context_tokens}
              onChange={(v) => setLimit("context_tokens", v)}
              placeholder="Claude default"
              error={limits.context_tokens.error}
              help="The model's context size, so agents compact before they hit it."
            />
            <FormField id={ids.fallback} label="Fallback model" info={<p>Used when the provider is unreachable, times out or the queue is full.</p>}>
              <select id={ids.fallback} value={fallback} onChange={(e) => setFallback(e.target.value)} className={inputClass}>
                <option value="">None</option>
                {fallback && !anthropicModels.some((m) => m.id === fallback) && <option value={fallback}>{fallback}</option>}
                {anthropicModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.label === model.id ? model.id : `${model.id} · ${model.label}`}
                  </option>
                ))}
              </select>
            </FormField>
          </div>
        </details>
        <details className="group min-w-0 rounded-lg border border-border sm:col-span-2">
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2 text-[13px] hover:bg-panel-2 [&::-webkit-details-marker]:hidden">
            <IconChevron size={14} className="shrink-0 text-muted transition-transform group-open:rotate-90" />
            <span className="font-medium">Pricing</span>
            <span className={cx("min-w-0 flex-1 truncate text-[12px]", pricingInvalid ? "text-err" : "text-faint")}>
              {pricingInvalid
                ? "Some values are out of range"
                : pricingSummary.length
                  ? `${pricingSummary.join(" · ")} per million tokens`
                  : "Optional — unset counts as $0 spent"}
            </span>
          </summary>
          <div className="grid gap-3 border-t border-border px-3 pb-3 pt-3 sm:grid-cols-2">
            <PriceField
              label="Input ($ per million tokens)"
              value={pricingDraft.input_per_mtok}
              onChange={(v) => setPricing("input_per_mtok", v)}
              error={pricing.input_per_mtok.error}
              help="What a million fresh input tokens cost."
            />
            <PriceField
              label="Output ($ per million tokens)"
              value={pricingDraft.output_per_mtok}
              onChange={(v) => setPricing("output_per_mtok", v)}
              error={pricing.output_per_mtok.error}
              help="What a million output tokens cost."
            />
            <PriceField
              label="Cache read ($ per million tokens)"
              value={pricingDraft.cache_read_per_mtok}
              onChange={(v) => setPricing("cache_read_per_mtok", v)}
              error={pricing.cache_read_per_mtok.error}
              help="What a million tokens read back from the provider's prompt cache cost."
            />
            <PriceField
              label="Cache write ($ per million tokens)"
              value={pricingDraft.cache_write_per_mtok}
              onChange={(v) => setPricing("cache_write_per_mtok", v)}
              error={pricing.cache_write_per_mtok.error}
              help="What a million tokens written to the provider's prompt cache cost."
            />
            <p className="text-[12px] leading-snug text-faint sm:col-span-2">
              Rates are dollars per million tokens, as the provider bills them, so a colony's spend budget sees this
              provider's traffic. A provider with no rates set still counts its routed tokens but adds $0 to the
              spend — the budget then only sees Claude's cost.
            </p>
          </div>
        </details>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
        {!isNew && (
          <Button size="sm" variant="danger" className="mr-auto" disabled={busy !== null} onClick={remove}>
            {busy === "delete" && <Spinner />} Remove provider
          </Button>
        )}
        <Button size="sm" variant="ghost" className={isNew ? "ml-auto" : ""} onClick={onCancel} disabled={busy !== null}>
          Cancel
        </Button>
        <Button size="sm" type="submit" variant="primary" disabled={invalid || busy !== null}>
          {busy === "save" && <Spinner />} {isNew ? "Add provider" : "Save"}
        </Button>
      </div>
    </form>
  );
}

function LimitField({
  label,
  value,
  onChange,
  placeholder,
  error,
  help,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
  error: string | null;
  help: string;
}) {
  const id = useId();
  return (
    <FormField id={id} label={label} info={<p>{help}</p>} error={error}>
      <input
        id={id}
        inputMode="numeric"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-[13px]", error && "border-err")}
      />
    </FormField>
  );
}

/** One pricing rate, in dollars per million tokens. Blank is allowed and prices that token kind at $0. */
function PriceField({
  label,
  value,
  onChange,
  error,
  help,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  error: string | null;
  help: string;
}) {
  const id = useId();
  return (
    <FormField id={id} label={label} info={<p>{help}</p>} error={error}>
      <input
        id={id}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Unset"
        spellCheck={false}
        autoComplete="off"
        aria-invalid={Boolean(error)}
        className={cx(inputClass, "font-mono text-[13px]", error && "border-err")}
      />
    </FormField>
  );
}

function ChipsInput({
  id,
  values,
  onChange,
  placeholder,
}: {
  id: string;
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState("");
  const commit = (raw: string) => {
    const added = raw
      .split(/[,\s]+/)
      .map((v) => v.trim())
      .filter((v) => v && !values.includes(v));
    if (added.length) onChange([...values, ...new Set(added)]);
    setDraft("");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      commit(draft);
    } else if (e.key === "Backspace" && !draft && values.length) {
      onChange(values.slice(0, -1));
    }
  };
  return (
    <div className="flex min-h-9 w-full min-w-0 flex-wrap items-center gap-1.5 rounded-lg border border-border bg-panel px-2 py-1.5 focus-within:border-accent focus-within:ring-2 focus-within:ring-[var(--accent-ring)]">
      {values.map((value) => (
        <span key={value} className="inline-flex max-w-full items-center gap-1 rounded-md bg-panel-2 py-0.5 pl-2 pr-1 font-mono text-[12px]">
          <span className="truncate">{value}</span>
          <button
            type="button"
            onClick={() => onChange(values.filter((v) => v !== value))}
            aria-label={`Remove ${value}`}
            className="grid size-4 cursor-pointer place-items-center rounded text-faint hover:bg-panel-3 hover:text-text"
          >
            <IconX size={11} />
          </button>
        </span>
      ))}
      <input
        id={id}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onBlur={() => draft.trim() && commit(draft)}
        placeholder={placeholder}
        spellCheck={false}
        className="min-w-24 flex-1 bg-transparent px-1 font-mono text-[13px] text-text outline-none placeholder:text-faint"
      />
    </div>
  );
}

/** Splits a comma-separated label setting into its labels. */
function labelList(value: unknown): string[] {
  return typeof value === "string" ? value.split(",").map((l) => l.trim()).filter(Boolean) : [];
}

/**
 * The Source page's picture, live: issues pass the label filter (as typed, before saving), wait in
 * the queue, run as colonies and come out as pull requests, each step with its count right now.
 */
function sourceFlow(settings: Record<string, unknown> | undefined, sessions: Session[] = []): FlowNode[] {
  const include = labelList(settings?.include_labels);
  const exclude = labelList(settings?.exclude_labels);
  const count = (...statuses: string[]) => sessions.filter((s) => statuses.includes(s.status)).length;
  const queued = count("queued");
  const live = count("starting", "running", "waiting_for_answer", "idle", "publishing");
  const prs = count("pr_opened");
  const chips: FlowChip[] =
    include.length + exclude.length === 0
      ? [{ text: "every open issue", kind: "note" }]
      : [...include.map((text) => ({ text, kind: "in" as const })), ...exclude.map((text) => ({ text, kind: "out" as const }))];
  return [
    { icon: "github", label: "Open issues", metric: "GitHub" },
    { icon: "filter", label: "Label filter", chips, active: include.length + exclude.length > 0 },
    { icon: "queue", label: "Queue", metric: `${queued} waiting`, active: queued > 0 },
    { icon: "ant", label: "Colonies", metric: `${live} running`, active: live > 0 },
    { icon: "pr", label: "Pull requests", metric: `${prs} open` },
  ];
}
