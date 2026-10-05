import { useCallback, useEffect, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { errorMessage, useApi } from "../context";
import type {
  HarnessStatus,
  ModelOption,
  ModelProvider,
  ModuleInfo,
  OrgInfo,
  RemoteStatus,
  Session,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
} from "../types";
import { type NotificationPrefs } from "../notifications";
import { useModels } from "../useModels";
import { type ImagePull } from "../useImagePull";
import { setupTone, type SetupView } from "../setup";
import { orgEnabled } from "../orgs";
import { Spinner, cx, meshBroken, sameOrg, useMediaQuery, type Tone } from "./ui";
import { IconX } from "./icons";
import { SettingsNavContext } from "./ModelPicker";
import { RemoteAccessPane } from "./RemoteAccessPane";
import { TokensPane } from "./TokensPane";
import { FleetPane } from "./FleetPane";
import { SetupSection } from "./SetupSection";
import { OrgSettingsForm } from "./OrgSettingsDialog";
import { PhonePane } from "./PhonePane";
import { YourCockpitCard } from "./YourCockpitCard";
import { SectionHero, guideFor, isAdvancedField, type FlowChip, type FlowNode, type HeroStat } from "./settingsGuide";
import { ConnectionsPane } from "./settings/ConnectionsPane";
import { RuntimePane } from "./settings/RuntimePane";
import { UpdatesPane } from "./settings/UpdatesPane";
import { LiveMapPane } from "./settings/LiveMapPane";
import { UsagePane } from "./settings/UsagePane";
import { DesktopPane } from "./settings/DesktopPane";
import { NotificationsPane } from "./settings/NotificationsPane";
import { ModulePane } from "./settings/ModulePane";
import { ProvidersPane } from "./settings/ProvidersPane";
import { draftOf, isDirty, kindInfo, type ModuleDraft } from "./settings/moduleFields";
import { HeroContext, Pane, SectionNav, TopNav, type NavGroup, type SectionId } from "./settings/ui";

// ---------------------------------------------------------------------------
// The settings screen's shell and state. Every section pane lives in
// ./settings/*, one file each; the registry below picks the one to render.
// Everything other modules and tests already import from here is re-exported.
// ---------------------------------------------------------------------------

export type { SectionId } from "./settings/ui";
export { Pane, Row } from "./settings/ui";
export { JevCompactionNotice, VoiceKeyRow, VoiceTestRow } from "./settings/moduleFields";
export { HealthStatus } from "./settings/ProvidersPane";
export { AutonomyHealth } from "./settings/AutonomyHealth";
export { ChipsInput, ProviderForm } from "./settings/ProviderForm";
export { duplicateModelMapCanonicals, modelMapCanonicals, providerSaveBody, sameWireFallbacks } from "./settings/providerCatalog";
export type { ModelMapRow, ProviderSaveInput } from "./settings/providerCatalog";

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
  remote,
  onRemoteChanged,
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
  /** The remote-access view App polls (issue #535); null until the first answer. */
  remote: RemoteStatus | null;
  /** The pane's setter, shared with the top bar's badge so a toggle moves both at once. */
  onRemoteChanged: (remote: RemoteStatus) => void;
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
          remote={remote}
          onRemoteChanged={onRemoteChanged}
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
  remote,
  onRemoteChanged,
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
  /** The remote-access view App polls (issue #535); null until the first answer. */
  remote: RemoteStatus | null;
  onRemoteChanged: (remote: RemoteStatus) => void;
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
  // "Set key" in a model picker: Model providers, opened on that provider's editor.
  const [focusProvider, setFocusProvider] = useState<string | undefined>(undefined);
  const openAt = (section: "providers", providerId?: string) => {
    setFocusProvider(providerId);
    select(section);
  };
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
          id: "cockpit",
          label: "Your cockpit",
          hint: "The address to bookmark for this cockpit",
        },
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
          id: "remote",
          label: "Remote access",
          hint: "Open this cockpit from your phone or another computer",
          badge: remote ? (remote.enabled ? "On" : "Off") : undefined,
        },
        {
          id: "phone",
          label: "Add your phone",
          hint: "Pair your phone with a code, and revoke it here",
        },
        {
          id: "tokens",
          label: "API tokens",
          hint: "Scoped keys for CLIs, agents and CI, in place of the owner token",
        },
        {
          id: "fleet",
          label: "Fleet",
          hint: "Let other machines join this one, or join another's fleet, with a pairing code",
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
          hint: "An anonymous batch, shown in full and sent only to an endpoint you name",
          badge: usage ? (usage.enabled ? "On" : "Off") : undefined,
        },
        {
          id: "notifications",
          label: "Notifications",
          hint: "What tells you a colony needs you when the tab is not in front",
          // The badge follows the two opt-in channels; the in-tab layer is on by default and needs no advertising.
          badge: notifications.sound || notifications.browser ? "On" : "Off",
        },
        {
          id: "desktop",
          label: "Desktop",
          hint: "Install the cockpit as an app, start the mothership at login",
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
      case "remote":
        return remote
          ? [
              { label: "Remote access", value: remote.enabled ? "On" : "Off", tone: onOff(remote.enabled) },
              ...(remote.enabled ? [{ label: "Tunnel", value: remote.connected ? "Connected" : "Offline", tone: remote.connected ? ("ok" as const) : ("warn" as const) }] : []),
            ]
          : [];
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

  // The section to render, from the one registry entry that answers to it. The
  // `org:` and `module:` families match by prefix, as the if/else chain did.
  const sectionContext: SectionContext = {
    active: active ?? "connections",
    back,
    select,
    status,
    onStatusChanged,
    telemetry,
    onTelemetryChanged,
    usage,
    onUsageChanged,
    notifications,
    onNotificationsChanged,
    remote,
    onRemoteChanged,
    setup,
    pull,
    onLaunch,
    onSetupShown,
    onSetupDismissed,
    modules,
    drafts,
    setDrafts,
    providers,
    providersError,
    setProviders,
    loadProviders,
    claude: claude ?? null,
    models,
    update,
    setUpdate,
    focusProvider,
    orgs,
    onOrgSaved,
    onModuleSaved,
  };
  const pane: ReactNode = active ? renderSection(active, sectionContext) : null;

  // What the page is for, as a card: Pane shows it at the top of its body; Setup and a workspace
  // draw their own frame, so for them it sits above the pane instead.
  const hero = active ? <SectionHero guide={guideFor(active)} stats={heroStats(active)} flow={active === "module:source" ? sourceFlow(drafts.source?.settings, sessions) : undefined} /> : null;
  const ownFrame = active === "setup" || Boolean(active?.startsWith("org:"));
  const framed = (
    <SettingsNavContext.Provider value={openAt}>
      <HeroContext.Provider value={ownFrame ? null : hero}>
        {ownFrame ? (
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            <div className="page-pad shrink-0 px-5 pt-4">{hero}</div>
            {pane}
          </div>
        ) : (
          pane
        )}
      </HeroContext.Provider>
    </SettingsNavContext.Provider>
  );

  return (
    <div className={cx("flex flex-col", embedded ? "h-full min-h-0" : "h-[min(680px,calc(100dvh-24px))]")}>
      {/* The cockpit has its own header and crumb, so the embedded frame does not repeat them. */}
      {!embedded && (
        <div className="flex shrink-0 items-center gap-3 border-b border-border px-5 py-3">
          <h2 id="settings-title" className="min-w-0 flex-1 text-title-sm font-semibold">
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
// The sections, in the order the nav lists them. One entry per section: the id
// it answers to (or a `match` for the `org:`/`module:` families), and how to
// render it from the state SettingsBody holds. Adding a section is one entry.
// ---------------------------------------------------------------------------

type SectionContext = {
  active: SectionId;
  back?: () => void;
  select: (id: SectionId) => void;
  status: HarnessStatus | null;
  onStatusChanged: (fresh?: boolean) => Promise<void> | void;
  telemetry: TelemetryStatus | null;
  onTelemetryChanged: (telemetry: TelemetryStatus) => void;
  usage: UsageStatus | null;
  onUsageChanged: (usage: UsageStatus) => void;
  notifications: NotificationPrefs;
  onNotificationsChanged: Dispatch<SetStateAction<NotificationPrefs>>;
  remote: RemoteStatus | null;
  onRemoteChanged: (remote: RemoteStatus) => void;
  setup: SetupView | null;
  pull: ImagePull;
  onLaunch: () => void;
  onSetupShown: () => void;
  onSetupDismissed: () => void;
  modules: ModuleInfo[] | null;
  drafts: Record<string, ModuleDraft>;
  setDrafts: Dispatch<SetStateAction<Record<string, ModuleDraft>>>;
  providers: ModelProvider[] | null;
  providersError: string | null;
  setProviders: Dispatch<SetStateAction<ModelProvider[] | null>>;
  loadProviders: () => Promise<void>;
  claude: HarnessStatus["claude"] | null;
  models: ModelOption[];
  update: UpdateStatus | null;
  setUpdate: Dispatch<SetStateAction<UpdateStatus | null>>;
  focusProvider?: string;
  orgs?: OrgInfo[];
  onOrgSaved?: (saved: OrgInfo) => void;
  onModuleSaved: (saved: ModuleInfo) => void;
};

type SectionEntry = {
  /** The exact id this entry renders, when it is not a `match`. */
  id?: SectionId;
  /** For the families (`org:`/`module:`): whether this entry takes the id. Checked in order. */
  match?: (id: SectionId) => boolean;
  render: (c: SectionContext) => ReactNode;
};

const SECTIONS: SectionEntry[] = [
  // Your cockpit first: the address to bookmark, with Copy and a QR code, and the way into pairing.
  {
    id: "cockpit",
    render: (c) => (
      <Pane title="Your cockpit" subtitle="The address to bookmark for this cockpit" back={c.back}>
        <YourCockpitCard remote={c.remote} onOpenPhone={() => c.select("phone")} />
      </Pane>
    ),
  },
  {
    id: "setup",
    render: (c) => (
      <SetupSection
        status={c.status}
        setup={c.setup}
        pull={c.pull}
        telemetry={c.telemetry}
        sandbox={(c.modules ?? []).find((m) => m.kind === "sandbox") ?? null}
        onStatusChanged={c.onStatusChanged}
        onSandboxSaved={c.onModuleSaved}
        onTelemetryChanged={c.onTelemetryChanged}
        onLaunch={c.onLaunch}
        onDismiss={c.onSetupDismissed}
        onShown={c.onSetupShown}
        onOpenLiveMap={() => c.select("live-map")}
        onOpenCockpit={() => c.select("cockpit")}
        back={c.back}
      />
    ),
  },
  { id: "connections", render: (c) => <ConnectionsPane status={c.status} onStatusChanged={c.onStatusChanged} back={c.back} /> },
  { id: "runtime", render: (c) => <RuntimePane status={c.status} back={c.back} /> },
  { id: "live-map", render: (c) => <LiveMapPane telemetry={c.telemetry} onChanged={c.onTelemetryChanged} back={c.back} /> },
  { id: "remote", render: (c) => <RemoteAccessPane remote={c.remote} onChanged={c.onRemoteChanged} back={c.back} /> },
  { id: "phone", render: (c) => <PhonePane back={c.back} /> },
  { id: "tokens", render: (c) => <TokensPane back={c.back} /> },
  { id: "fleet", render: (c) => <FleetPane back={c.back} /> },
  { id: "updates", render: (c) => <UpdatesPane update={c.update} onChanged={c.setUpdate} back={c.back} /> },
  { id: "usage", render: (c) => <UsagePane usage={c.usage} onChanged={c.onUsageChanged} back={c.back} /> },
  { id: "notifications", render: (c) => <NotificationsPane prefs={c.notifications} onChanged={c.onNotificationsChanged} orgs={c.orgs} back={c.back} /> },
  { id: "desktop", render: (c) => <DesktopPane back={c.back} /> },
  {
    id: "providers",
    render: (c) => (
      <ProvidersPane
        providers={c.providers}
        error={c.providersError}
        setProviders={c.setProviders}
        reload={c.loadProviders}
        claude={c.claude}
        models={c.models}
        onOpenConnections={() => c.select("connections")}
        focusId={c.focusProvider}
        back={c.back}
      />
    ),
  },
  {
    match: (id) => id.startsWith("org:"),
    render: (c) => {
      const org = c.active.slice("org:".length);
      return (
        <OrgSettingsForm
          key={org}
          embedded
          org={org}
          info={c.orgs?.find((o) => sameOrg(o.org, org))}
          onClose={() => {}}
          onSaved={(saved) => c.onOrgSaved?.(saved)}
        />
      );
    },
  },
  {
    match: (id) => id.startsWith("module:"),
    render: (c) => {
      const kind = c.active.slice("module:".length);
      const module = c.modules?.find((m) => m.kind === kind);
      const draft = c.drafts[kind];
      return module && draft ? (
        <ModulePane
          key={kind}
          module={module}
          draft={draft}
          models={kind === "agent" ? c.models : undefined}
          pull={c.pull}
          back={c.back}
          onDraft={(patch) => c.setDrafts((d) => ({ ...d, [kind]: { ...d[kind], ...patch } }))}
          onReset={() => c.setDrafts((d) => ({ ...d, [kind]: draftOf(module) }))}
          onSaved={c.onModuleSaved}
        />
      ) : (
        <Pane title={kindInfo(kind).title} back={c.back}>
          <p className="flex items-center gap-2 text-body-sm text-muted">
            <Spinner /> Loading…
          </p>
        </Pane>
      );
    },
  },
];

/** The first entry that answers to the id, rendered; null when nothing matches (as the chain did). */
function renderSection(id: SectionId, c: SectionContext): ReactNode {
  return SECTIONS.find((entry) => (entry.match ? entry.match(id) : entry.id === id))?.render(c) ?? null;
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
