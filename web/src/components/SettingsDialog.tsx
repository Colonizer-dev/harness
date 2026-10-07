import { useCallback, useEffect, useMemo, useRef, useState, type Dispatch, type ReactNode, type SetStateAction } from "react";
import { errorMessage, useApi } from "../context";
import type {
  AutonomyStatus,
  BuiltWith,
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
import { modelRoute } from "../modelRoute";
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
import { BuiltWithPane } from "./settings/BuiltWithPane";
import { NotificationsPane } from "./settings/NotificationsPane";
import { ModulePane } from "./settings/ModulePane";
import { ProvidersPane } from "./settings/ProvidersPane";
import { draftOf, isDirty, kindInfo, type ModuleDraft } from "./settings/moduleFields";
import { CrumbContext, HeroContext, IntroLine, Pane, type SectionId } from "./settings/ui";
import {
  NeedsYou,
  PageChips,
  SettingsList,
  SettingsRail,
  SettingsSearchBox,
  SettingsSearchResults,
  SettingsSidebar,
  type AttentionItem,
  type NavGroupModel,
  type NavPage,
} from "./settings/SettingsNav";
import {
  FIXED_PAGES,
  GROUPS,
  buildSearchIndex,
  groupInfo,
  groupOf,
  searchSettings,
  sectionFromSettingsPath,
  settingsPath,
  worstAttention,
  type Attention,
  type SearchHit,
} from "./settings/nav";
import { providerMissingKey } from "./settings/providerCatalog";
import { flashField } from "./settings/flashField";
import "../settingsFlash.css";
import { JUDGE_ALERT_AFTER, judgeAlertTitle } from "../cockpit/Header";
import { go, usePath } from "../router";
import { useOpenSecrets, providerSecretId } from "../secretsNav";

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
  onDismissedChanged,
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
  onDismissedChanged: (ids: string[]) => void;
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
          onDismissedChanged={onDismissedChanged}
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
  onDismissedChanged,
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
  onDismissedChanged: (ids: string[]) => void;
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
  const wide = useMediaQuery("(min-width: 1024px)");
  const openSecrets = useOpenSecrets();
  // In the cockpit the address is the state: `/settings/<group>/<page>` names the page, so the page
  // can be bookmarked and the back button walks the pages. The dialog keeps its own.
  const path = usePath();
  const [localSection, setLocalSection] = useState<SectionId | null>(() => (embedded ? null : (initialSection ?? null)));
  const urlSection = embedded ? sectionFromSettingsPath(path) : null;
  const section: SectionId | null = embedded ? urlSection : localSection;
  // Where focus returns when a narrow window goes back to the section list.
  const lastSection = useRef<SectionId | undefined>(undefined);
  // "Set key" in a model picker: Model providers, opened on that provider's editor.
  const [focusProvider, setFocusProvider] = useState<string | undefined>(undefined);
  const openAt = (section: "providers", providerId?: string) => {
    setFocusProvider(providerId);
    select(section);
  };
  const select = (id: SectionId) => {
    // Secrets is a page of its own, not a pane: it lives in the cockpit's Secrets view.
    if (id === "secrets") {
      openSecrets?.();
      return;
    }
    lastSection.current = id;
    if (embedded) go(settingsPath(id));
    else setLocalSection(id);
  };
  // An external jump ("open providers") lands here once, unless the address already names a page.
  useEffect(() => {
    if (embedded && initialSection && initialSection !== "secrets" && sectionFromSettingsPath(window.location.pathname) === null) {
      go(settingsPath(initialSection));
    }
    // Once, on mount: the body is keyed on the request that opened it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  // The search box: what is typed, and the field a result asked to land on.
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const [landing, setLanding] = useState<{ section: SectionId; label: string; n: number } | null>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [modules, setModules] = useState<ModuleInfo[] | null>(null);
  const [modulesError, setModulesError] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<Record<string, ModuleDraft>>({});
  const [providers, setProviders] = useState<ModelProvider[] | null>(null);
  const [providersError, setProvidersError] = useState<string | null>(null);
  // Fetched here rather than threaded through App: nothing outside Settings needs it.
  const [update, setUpdate] = useState<UpdateStatus | null>(null);
  // The venture's stack, loaded here for the same reason as `update`: nothing outside Settings reads
  // it, and the hero's live/planned counts need it on this screen (issue #944).
  const [builtWith, setBuiltWith] = useState<BuiltWith | null>(null);
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
    // A mothership from before issue #944 has no such route; the pane falls back to its empty state.
    api
      .builtWith()
      .then((b) => !cancelled && setBuiltWith(b))
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

  const defaultSection: SectionId = setup?.autoOpen ? "setup" : "cockpit";
  const active: SectionId | null = narrow ? section : (section ?? defaultSection);

  const github = status?.github;
  const claude = status?.claude;
  // The Setup summary tone shares one derivation with the pane itself.
  const setupToneValue: Tone | null = setup ? setupTone(setup) : null;

  /** One save handler for module panes and Setup's stack pick alike: keeps the drafts and the app's modules in step. */
  const onModuleSaved = (saved: ModuleInfo) => {
    const next = (modules ?? []).map((m) => (m.kind === saved.kind ? saved : m));
    setModules(next);
    setDrafts((d) => ({ ...d, [saved.kind]: draftOf(saved) }));
    onModulesChanged(next);
  };

  // What the autonomy judge last said, for the red dot on Models → Autonomy. Older mothership builds
  // have no such route; the dot simply never appears.
  const [judge, setJudge] = useState<AutonomyStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    api
      .autonomyStatus()
      .then((j) => !cancelled && setJudge(j))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [api]);

  // Everything that needs the person, page by page: amber needs you, red is broken. The same list
  // draws the dots in the menu and the callout at the top of the page, so they cannot disagree.
  const attention: Partial<Record<SectionId, AttentionItem[]>> = {};
  const flag = (id: SectionId, item: AttentionItem) => (attention[id] = [...(attention[id] ?? []), item]);
  if (status) {
    if (!github?.connected) flag("connections", { tone: "warn", text: "GitHub is not connected, so colonies cannot read or push code. Sign in below." });
    const route = modelRoute(status);
    if (!route.ok) flag("providers", { tone: "warn", text: `${route.reason ?? "No route to a model."} Sign in under Subscriptions or add a provider key.` });
    if (!status.sandbox.msb_version) flag("runtime", { tone: "err", text: "microsandbox is missing, so colonies cannot start. The page lists how to install it." });
    if (status.sandbox.claude_bin_error) flag("runtime", { tone: "err", text: `The Claude binary has a problem: ${status.sandbox.claude_bin_error}` });
    if (meshBroken(status.mesh)) flag("runtime", { tone: "err", text: "The private network between the mothership and colonies is not healthy. See the Mesh module." });
  }
  if (setup && setupToneValue && setupToneValue !== "ok") flag("setup", { tone: "warn", text: setup.firstActionable ? `Next step: ${setup.firstActionable.title}.` : "Your setup checklist is not finished yet." });
  const needKey = (providers ?? []).filter(providerMissingKey);
  if (needKey.length > 0) {
    flag("providers", {
      tone: "warn",
      text: `${needKey.length === 1 ? "One provider needs" : `${needKey.length} providers need`} a key: ${needKey.map((p) => p.name).join(", ")}.`,
      action: openSecrets ? { label: "Set key", run: () => openSecrets(providerSecretId(needKey[0].id)) } : undefined,
    });
  }
  if (judge && judge.consecutive_failures >= JUDGE_ALERT_AFTER) {
    flag("module:autonomy", { tone: "err", text: judgeAlertTitle(judge), action: { label: "Open Model providers", run: () => select("providers") } });
  } else if (judge?.enabled && judge.problem) {
    flag("module:autonomy", { tone: "warn", text: judge.problem });
  }
  if (remote?.enabled && !remote.connected) flag("remote", { tone: "warn", text: "Remote access is on, but the tunnel is offline right now." });
  const attentionOf = (id: SectionId): Attention | null => worstAttention((attention[id] ?? []).map((i) => i.tone));

  // The pages, in the order each group lists them: the fixed ones, then modules by kind, then workspaces.
  const pageBase = (id: SectionId): NavPage => {
    if (id.startsWith("module:")) {
      const info = kindInfo(id.slice("module:".length));
      return { id, label: info.title, hint: info.description };
    }
    if (id.startsWith("org:")) {
      const org = orgs?.find((o) => sameOrg(o.org, id.slice("org:".length)));
      return { id, label: id.slice("org:".length), hint: org ? `${org.colonies.live} live · ${org.colonies.total} ${org.colonies.total === 1 ? "colony" : "colonies"}` : "Settings for this workspace" };
    }
    const page = FIXED_PAGES.find((p) => p.id === id);
    return { id, label: page?.label ?? id, hint: page?.hint ?? "" };
  };
  const badges: Partial<Record<SectionId, string | undefined>> = {
    providers: providers ? String(providers.length + 1) : undefined,
    "live-map": telemetry ? (telemetry.enabled ? "On" : "Off") : undefined,
    remote: remote ? (remote.enabled ? "On" : "Off") : undefined,
    updates: update?.available ? `${update.latest?.version} available` : update ? update.installed.version : undefined,
    usage: usage ? (usage.enabled ? "On" : "Off") : undefined,
    // The badge follows the two opt-in channels; the in-tab layer is on by default and needs no advertising.
    notifications: notifications.sound || notifications.browser ? "On" : "Off",
  };
  const sectionIds: SectionId[] = [
    ...FIXED_PAGES.map((p) => p.id),
    ...(modules ?? []).map((m) => `module:${m.kind}` as SectionId),
    ...(orgs ?? []).filter((o) => !o.awaiting_decision).map((o) => `org:${o.org}` as SectionId),
  ];
  const groups: NavGroupModel[] = GROUPS.map((info) => {
    const pages: NavPage[] = sectionIds
      .filter((id) => groupOf(id) === info.id)
      .map((id) => {
        const base = pageBase(id);
        const module = id.startsWith("module:") ? modules?.find((m) => `module:${m.kind}` === id) : undefined;
        const org = id.startsWith("org:") ? orgs?.find((o) => sameOrg(o.org, id.slice(4))) : undefined;
        return {
          ...base,
          attention: attentionOf(id),
          attentionText: attention[id]?.[0]?.tone === "err" ? "broken" : "needs you",
          badge: module ? (module.enabled ? undefined : "Off") : org ? (orgEnabled(org.settings) ? undefined : "Off") : badges[id],
          dirty: module ? isDirty(module, drafts[module.kind]) : undefined,
        };
      });
    return {
      info,
      pages,
      attention: worstAttention(pages.map((p) => p.attention)),
      loading: info.id === "runtime" && !modules && !modulesError,
      error: info.id === "runtime" ? modulesError : null,
    };
  }).filter((g) => g.pages.length > 0 || g.info.id === "runtime");

  // Search: pages, their fields, every module setting and every provider, found by label, help and synonyms.
  const index = useMemo(
    () =>
      buildSearchIndex({
        pages: groups.flatMap((g) => g.pages),
        modules: (modules ?? []).map((m) => ({ kind: m.kind, title: kindInfo(m.kind).title, schema: m.schema })),
        providers: (providers ?? []).map((p) => ({ name: p.name, models: p.models, missingKey: providerMissingKey(p) })),
        orgs: (orgs ?? []).filter((o) => !o.awaiting_decision).map((o) => o.org),
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [modules, providers, orgs, status, setup, update, remote],
  );
  const crumbsOf = (entry: { section: SectionId; label: string; field: boolean }): string[] => {
    const page = groups.flatMap((g) => g.pages).find((p) => p.id === entry.section);
    const crumbs = [groupInfo(groupOf(entry.section)).label, page?.label ?? entry.label];
    return entry.field && page?.label !== entry.label ? [...crumbs, entry.label] : crumbs;
  };
  const hits = searchSettings(index, crumbsOf, query);
  const pick = (hit: SearchHit) => {
    setQuery("");
    setLanding({ section: hit.entry.section, label: hit.entry.field ? hit.entry.label : "", n: Date.now() });
    select(hit.entry.section);
  };
  const searching = query.trim().length > 0;
  const search = {
    box: <SettingsSearchBox value={query} onChange={setQuery} onSubmit={() => hits[0] && pick(hits[0])} inputRef={searchRef} />,
    results: searching ? <SettingsSearchResults query={query} hits={hits} onPick={pick} /> : null,
  };

  // "/" jumps to the search box from anywhere on the page that is not already a text field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const target = e.target as HTMLElement | null;
      if (target?.closest("input, textarea, select, [contenteditable='true'], dialog:not([open])")) return;
      if (!searchRef.current) return;
      e.preventDefault();
      searchRef.current.focus();
      searchRef.current.select();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // A result for a field lands on its page, then finds the field's label there and flashes it. The
  // page may still be loading its data, so the look is repeated for a moment.
  useEffect(() => {
    if (!landing || active !== landing.section) return;
    if (!landing.label) {
      contentRef.current?.scrollTo?.({ top: 0 });
      return;
    }
    let tries = 0;
    const look = window.setInterval(() => {
      if ((contentRef.current && flashField(contentRef.current, landing.label)) || ++tries > 12) window.clearInterval(look);
    }, 150);
    return () => window.clearInterval(look);
  }, [landing, active]);

  const back = narrow ? () => (embedded ? go("/settings", { replace: true }) : setLocalSection(null)) : undefined;

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
            ]
          : [];
      case "providers":
        return status
          ? [
              { label: "Model", value: modelRoute(status).ok ? "Connected" : "Not connected", tone: modelRoute(status).ok ? "ok" : "err" },
              ...(providers ? [{ label: "API providers", value: String(providers.length) }] : []),
            ]
          : [];
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
      case "built-with":
        return builtWith
          ? [
              { label: "Live", value: String(builtWith.uses.filter((u) => u.status === "live").length), tone: "ok" as const },
              { label: "Planned", value: String(builtWith.uses.filter((u) => u.status === "planned").length) },
            ]
          : [];
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
    onDismissedChanged,
    usage,
    onUsageChanged,
    builtWith,
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
  // draw their own frame, so for them it sits above the pane instead. What needs the person comes first.
  const hero = active ? (
    <>
      <NeedsYou items={attention[active] ?? []} />
      {active === "providers" ? (
        // The list is the point of this page: its help is one line, and the diagram waits behind the "i".
        <IntroLine text="What is left in each plan and when it resets. Open a provider for its details." label="How requests reach a model">
          <SectionHero guide={guideFor(active)} stats={heroStats(active)} />
        </IntroLine>
      ) : (
        <SectionHero guide={guideFor(active)} stats={heroStats(active)} flow={active === "module:source" ? sourceFlow(drafts.source?.settings, sessions) : undefined} />
      )}
    </>
  ) : null;
  const ownFrame = active === "setup" || Boolean(active?.startsWith("org:"));
  const framed = (
    <SettingsNavContext.Provider value={openAt}>
      <CrumbContext.Provider value={active ? groupInfo(groupOf(active)).label : null}>
        <HeroContext.Provider value={ownFrame ? null : hero}>
          {/* Keyed on the page, so moving between pages starts the new one at its top. */}
          <div key={active ?? "list"} ref={contentRef} className="flex min-h-0 min-w-0 flex-1 flex-col [--page-max:760px]">
            {ownFrame ? (
              <div className="flex min-h-0 min-w-0 flex-1 flex-col">
                <div className="page-pad shrink-0 px-5 pt-4">{hero}</div>
                {pane}
              </div>
            ) : (
              pane
            )}
          </div>
        </HeroContext.Provider>
      </CrumbContext.Provider>
    </SettingsNavContext.Provider>
  );
  const activeGroup = active ? groups.find((g) => g.pages.some((p) => p.id === active)) : undefined;

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
          <SettingsList groups={groups} onSelect={select} initialFocus={lastSection.current} search={search} />
        ) : (
          framed
        )
      ) : (
        <div className="flex min-h-0 flex-1">
          {wide ? (
            <SettingsSidebar groups={groups} active={active} onSelect={select} search={search} />
          ) : (
            <SettingsRail groups={groups} active={active} onSelect={select} />
          )}
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            {!wide && (
              <div className="shrink-0 border-b border-border" data-search-scope>
                <div className="mx-auto w-full max-w-[760px] px-5 py-2.5">{search.box}</div>
              </div>
            )}
            {!wide && search.results ? (
              <div data-search-scope className="scroll-thin min-h-0 flex-1 overflow-y-auto">
                <div className="mx-auto w-full max-w-[760px] px-3 py-3">{search.results}</div>
              </div>
            ) : (
              <>
                {!wide && <PageChips group={activeGroup} active={active} onSelect={select} />}
                {framed}
              </>
            )}
          </div>
        </div>
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
  onDismissedChanged: (ids: string[]) => void;
  usage: UsageStatus | null;
  onUsageChanged: (usage: UsageStatus) => void;
  /** The venture's stack, for the Built with pane and its hero counts (issue #944). */
  builtWith: BuiltWith | null;
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
        onDismissedChanged={c.onDismissedChanged}
        onLaunch={c.onLaunch}
        onDismiss={c.onSetupDismissed}
        onShown={c.onSetupShown}
        onOpenLiveMap={() => c.select("live-map")}
        onOpenCockpit={() => c.select("cockpit")}
        onOpenModels={() => c.select("providers")}
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
  { id: "built-with", render: (c) => <BuiltWithPane builtWith={c.builtWith} back={c.back} /> },
  {
    id: "providers",
    render: (c) => (
      <ProvidersPane
        providers={c.providers}
        error={c.providersError}
        setProviders={c.setProviders}
        reload={c.loadProviders}
        status={c.status}
        models={c.models}
        onStatusChanged={c.onStatusChanged}
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
