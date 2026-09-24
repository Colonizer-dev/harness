import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useApi } from "./context";
import { IconMenu, IconSpark } from "./components/icons";
import { MemoryView } from "./components/MemoryView";
import { OrgPromptCard } from "./components/OrgPromptCard";
import { OrgSettingsDialog } from "./components/OrgSettingsDialog";
import { SessionView, type InterfaceFlags } from "./components/SessionView";
import { SettingsBody, SettingsDialog, type SectionId } from "./components/SettingsDialog";
import { Sidebar, type MainView, type SidebarTab } from "./components/Sidebar";
import { Cockpit } from "./cockpit/Cockpit";
import { Button, cx, isLive, orgOf, sameOrg, store, stored, useMediaQuery } from "./components/ui";
import {
  NOTIFICATIONS_KEY,
  applyFavicon,
  applyTabTitle,
  diffEvents,
  eventText,
  needsYou,
  orgFilterForTarget,
  parseNotificationPrefs,
  playQuestionBlip,
  serializeNotificationPrefs,
  showColonyNotification,
  snapshotOf,
  type NotificationPrefs,
  type SessionSnapshot,
} from "./notifications";
import { floatingColumnClass } from "./floatingColumn";
import {
  LiveStream,
  removeSessionById,
  shouldPollWhileLive,
  upsertSession as upsertSessionList,
  type LiveConnection,
} from "./liveStream";
import { orgEntries, pendingOrgPrompt, reconcileSelectedOrg } from "./orgs";
import { setupView, stackPresetOf, type SetupView } from "./setup";
import { useImagePull } from "./useImagePull";
import { usePollTick } from "./usePollTick";
import type {
  FleetHost,
  HarnessStatus,
  ModuleInfo,
  OrgInfo,
  OrgSettings,
  RedTeamRun,
  Session,
  StartRedTeamRunRequest,
  StorageHealth,
  StorageSummary,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
} from "./types";

export function App() {
  const api = useApi();
  const narrow = useMediaQuery("(max-width: 899px)");
  const [status, setStatus] = useState<HarnessStatus | null>(null);
  const [statusError, setStatusError] = useState(false);
  // Self plus every peer configured via COLONIZER_FLEET_PEERS (issue #231); older mothership builds
  // have no /api/hosts, so a failed poll just leaves the fleet panel with nothing to show.
  const [fleet, setFleet] = useState<FleetHost[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  const [redRuns, setRedRuns] = useState<RedTeamRun[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(() => stored("colonizer.session"));
  const [interfaces, setInterfaces] = useState<InterfaceFlags>({ chat: true, terminal: true });
  const [autopilotDefault, setAutopilotDefault] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SectionId | undefined>(undefined);
  const [telemetry, setTelemetry] = useState<TelemetryStatus | null>(null);
  const [usage, setUsage] = useState<UsageStatus | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [orgs, setOrgs] = useState<OrgInfo[]>([]);
  // Whether the first /api/orgs answer (or its failure) is in: until then an empty list says nothing.
  const [orgsLoaded, setOrgsLoaded] = useState(false);
  const [selectedOrg, setSelectedOrg] = useState<string | null>(() => stored("colonizer.org") || null);
  const [view, setView] = useState<MainView>(() => (stored("colonizer.view") === "memory" ? "memory" : "colonies"));
  const [pendingMemory, setPendingMemory] = useState(0);
  const [orgSettingsFor, setOrgSettingsFor] = useState<string | null>(null);
  // Orgs answered this session: the PUT has marked them decided server-side, but until the 15 s
  // poll confirms, this is what keeps a just-answered prompt card from flashing back in.
  const [answeredOrgs, setAnsweredOrgs] = useState<ReadonlySet<string>>(() => new Set());
  const [notifyPrefs, setNotifyPrefs] = useState<NotificationPrefs>(() => parseNotificationPrefs(stored(NOTIFICATIONS_KEY)));
  // The mothership's storage alert is sticky, so dismissal is client-side, keyed on the alert's ts
  // (or its message when an older mothership omits the ts): a newer failure shows the card again.
  // Every dismissed key is kept, not just the last: load damage the operator dismissed comes back
  // from the mothership once a later write failure recovers (issue #371), and must stay hidden.
  const [dismissedStorage, setDismissedStorage] = useState<ReadonlySet<string>>(() => new Set());
  const promptedForSettings = useRef(false);
  // Setup's own state lives only in this page load: "Not now" holds the auto-open off, and
  // "has been shown" retires the standalone live-map prompt. Neither is ever persisted.
  const setupDismissed = useRef(false);
  const [setupShown, setSetupShown] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus | null>(null);
  const [launchRequests, setLaunchRequests] = useState(0);
  const [settingsRequests, setSettingsRequests] = useState(0);
  const [memoryRequests, setMemoryRequests] = useState(0);
  // Whether the cockpit is showing its inspector; the fixed card column steps left of it.
  const [inspectorShown, setInspectorShown] = useState(false);
  // The sidebar's tab, lifted so Setup's launch button can open the launcher directly.
  // (colonizer.sidebar-tab stays the sidebar's own memory of itself.)
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>(() => (stored("colonizer.sidebar-tab") === "new" ? "new" : "sessions"));
  const [modules, setModules] = useState<ModuleInfo[]>([]);
  // The realtime feed (issue #446): while its connection is open the sessions/orgs/fleet poll
  // ticks skip their fetch; a pushed storage frame feeds the storage panel through liveStorage.
  const [liveConnection, setLiveConnection] = useState<LiveConnection>("connecting");
  const [liveStorage, setLiveStorage] = useState<StorageSummary | null>(null);
  // One image-pull poller for the whole app; Setup, Settings and the sidebar all read it.
  const pull = useImagePull(true);

  const loadStatus = useCallback(async (fresh?: boolean) => {
    try {
      setStatus(await api.status(fresh));
      setStatusError(false);
    } catch {
      setStatusError(true);
    }
  }, [api]);

  const loadFleet = useCallback(async () => {
    try {
      setFleet((await api.hosts()).hosts);
    } catch {
      /* older mothership: no /api/hosts, and the fleet panel just has nothing to show */
    }
  }, [api]);

  const loadSessions = useCallback(async () => {
    try {
      setSessions(await api.sessions());
      setSessionsLoaded(true);
    } catch {
      /* keep the last list */
    }
  }, [api]);

  const loadRedRuns = useCallback(async () => {
    try {
      setRedRuns(await api.redTeamRuns());
    } catch {
      /* older mothership, or offline: the overview simply shows no raids */
    }
  }, [api]);

  const loadOrgs = useCallback(async () => {
    try {
      setOrgs(await api.orgs());
    } catch {
      /* older mothership, or offline: the switcher falls back to orgs seen in colonies */
    } finally {
      setOrgsLoaded(true);
    }
  }, [api]);

  const loadPendingMemory = useCallback(async () => {
    try {
      setPendingMemory((await api.memoryProposals()).length);
    } catch {
      /* keep the last count */
    }
  }, [api]);

  const applyModules = useCallback((modules: ModuleInfo[]) => {
    setModules(modules);
    const publish = modules.find((m) => m.kind === "publish");
    setAutopilotDefault((publish?.settings?.autopilot ?? publish?.schema?.properties?.autopilot?.default) === true);
    const module = modules.find((m) => m.kind === "interfaces");
    if (!module || !module.enabled) {
      setInterfaces({ chat: true, terminal: true });
      return;
    }
    const flag = (key: string) => (module.settings?.[key] ?? module.schema?.properties?.[key]?.default) !== false;
    setInterfaces({ chat: flag("chat"), terminal: flag("terminal") });
  }, []);

  const loadTelemetry = useCallback(async () => {
    try {
      setTelemetry(await api.telemetry());
    } catch {
      /* older mothership: no live map, and nothing to ask */
    }
  }, [api]);

  const loadUsage = useCallback(async () => {
    try {
      setUsage(await api.usage());
    } catch {
      /* older mothership: no usage endpoint, and nothing to ask */
    }
  }, [api]);

  const loadUpdate = useCallback(async () => {
    try {
      setUpdateStatus(await api.update());
    } catch {
      /* older mothership: no update endpoint, so the cockpit shows no version chip */
    }
  }, [api]);

  useEffect(() => {
    void loadStatus();
    void loadFleet();
    void loadSessions();
    void loadTelemetry();
    void loadUsage();
    void loadOrgs();
    void loadPendingMemory();
    void loadUpdate();
    void loadRedRuns();
    api.modules().then(applyModules).catch(() => {});
  }, [api, loadStatus, loadFleet, loadSessions, loadOrgs, loadPendingMemory, loadTelemetry, loadUsage, loadUpdate, loadRedRuns, applyModules]);

  // The poll schedule lives in a module worker (usePollTick) whose timers keep their cadence
  // while the tab is hidden — Chrome throttles hidden-tab main-thread timers to one wake-up per
  // minute, which used to delay notifications for colonies the open chat is not showing
  // (issue #159). Error semantics are unchanged: every loader swallows its own failure and keeps
  // the last data, exactly as the old setIntervals did.
  // While the realtime stream is open it owns the sessions/orgs/fleet ticks, so those polls skip
  // (shouldPollWhileLive); every other tick keeps running, and a dropped stream falls back to the
  // full schedule until it reconnects. A guarded poll also drops its own result when the stream
  // reopened mid-flight, so a stale fetch can never rewind fresher pushed frames.
  const liveConnectionRef = useRef(liveConnection);
  liveConnectionRef.current = liveConnection;

  const pollSessions = useCallback(async () => {
    if (!shouldPollWhileLive("sessions", liveConnectionRef.current)) return;
    try {
      const list = await api.sessions();
      if (!shouldPollWhileLive("sessions", liveConnectionRef.current)) return;
      setSessions(list);
      setSessionsLoaded(true);
    } catch {
      /* keep the last list */
    }
  }, [api]);

  const pollOrgs = useCallback(async () => {
    if (!shouldPollWhileLive("orgs", liveConnectionRef.current)) return;
    try {
      const list = await api.orgs();
      if (!shouldPollWhileLive("orgs", liveConnectionRef.current)) return;
      setOrgs(list);
      setOrgsLoaded(true);
    } catch {
      /* keep the last list */
    }
  }, [api]);

  const pollFleet = useCallback(async () => {
    if (!shouldPollWhileLive("fleet", liveConnectionRef.current)) return;
    try {
      const fleet = (await api.hosts()).hosts;
      if (!shouldPollWhileLive("fleet", liveConnectionRef.current)) return;
      setFleet(fleet);
    } catch {
      /* keep the last list */
    }
  }, [api]);

  usePollTick({
    sessions: () => void pollSessions(),
    redRuns: loadRedRuns,
    status: loadStatus,
    // Fleet stats change about as slowly as the host's own, so it shares that cadence.
    fleet: () => void pollFleet(),
    orgs: () => void pollOrgs(),
    pendingMemory: loadPendingMemory,
    // A release check is a request to github.com, so it runs far more slowly than the rest.
    update: loadUpdate,
  });

  // The realtime feed: created once per api, closed on unmount. Full-list frames replace the
  // same state the polls feed; single-session frames upsert like GET /api/sessions (newest
  // created first). On a drop the stream asks for one immediate guarded refresh and clears the
  // storage override, so nothing goes stale before the polls catch up.
  useEffect(() => {
    const stream = new LiveStream(() => api.openStream(), {
      onSessions: (list) => {
        setSessions(list);
        setSessionsLoaded(true);
      },
      onSession: (session) => setSessions((list) => upsertSessionList(list, session)),
      onSessionRemoved: (id) => setSessions((list) => removeSessionById(list, id)),
      onOrgs: (list) => {
        setOrgs(list);
        setOrgsLoaded(true);
      },
      onHosts: (hosts) => setFleet(hosts),
      onStorage: (storage) => setLiveStorage(storage),
      onConnection: (connection) => setLiveConnection(connection),
      onDrop: () => {
        setLiveStorage(null);
        void pollSessions();
        void pollOrgs();
        void pollFleet();
      },
    });
    stream.start();
    return () => stream.stop();
  }, [api, pollSessions, pollOrgs, pollFleet]);

  // A returning tab refreshes at once instead of waiting for the next tick: the worker already
  // kept polling while hidden, so this only closes the gap since its last tick.
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === "visible") {
        void loadSessions();
        void loadRedRuns();
      }
    };
    document.addEventListener("visibilitychange", refresh);
    return () => document.removeEventListener("visibilitychange", refresh);
  }, [loadSessions, loadRedRuns]);

  // Keep a valid org filter: the stored one can name an org that has gone or been switched off
  // since, which would filter everything to an empty nest under a rail that highlights nothing.
  // Only once both lists are in, so a slow first poll never clears a good choice.
  useEffect(() => {
    if (!sessionsLoaded || !orgsLoaded || !selectedOrg) return;
    const kept = reconcileSelectedOrg(selectedOrg, orgEntries(orgs, sessions), narrow);
    if (kept === selectedOrg) return;
    setSelectedOrg(kept);
    store("colonizer.org", kept);
  }, [sessionsLoaded, orgsLoaded, orgs, sessions, selectedOrg, narrow]);

  // Keep a valid selection: fall back to the newest running colony in the current workspace.
  useEffect(() => {
    if (!sessionsLoaded) return;
    if (selectedId && sessions.some((s) => s.id === selectedId)) return;
    const pool = selectedOrg ? sessions.filter((s) => sameOrg(orgOf(s), selectedOrg)) : sessions;
    setSelectedId(pool.find((s) => isLive(s.status))?.id ?? null);
  }, [sessionsLoaded, sessions, selectedId, selectedOrg]);

  useEffect(() => {
    store("colonizer.session", selectedId);
  }, [selectedId]);

  useEffect(() => {
    store("colonizer.view", view);
  }, [view]);

  // Notifications preferences are client-side only (no client-settings endpoint), so they persist as one localStorage blob like the other colonizer.* keys.
  useEffect(() => {
    store(NOTIFICATIONS_KEY, serializeNotificationPrefs(notifyPrefs));
  }, [notifyPrefs]);

  // The in-tab layer. With the layer off App passes 0, which is today's look exactly: tabTitle(0) is
  // the static "Colonizer" and faviconHref(false) the href index.html ships with.
  useEffect(() => {
    const count = notifyPrefs.inTab ? sessions.filter(needsYou).length : 0;
    applyTabTitle(count);
    applyFavicon(count > 0);
  }, [sessions, notifyPrefs.inTab]);

  // The Setup view, derived once from the same state the dialog renders. `pull.status` and the
  // module list ride along so the checklist reacts to a finished download or a stack change.
  // `now` is read here rather than inside setup.ts so the derivation stays pure; the 30-second
  // status poll is what refreshes the Claude row's expiry advisory.
  const sandboxModule = modules.find((m) => m.kind === "sandbox") ?? null;
  const setup = useMemo<SetupView | null>(
    () =>
      status
        ? setupView({
            status,
            pull: pull.status,
            telemetry,
            stackPreset: stackPresetOf(sandboxModule?.settings),
            sessionCount: sessions.length,
            now: Date.now(),
          })
        : null,
    [status, pull.status, telemetry, sandboxModule, sessions.length],
  );

  /**
   * Settings has two frames: a dialog on a narrow window, and a cockpit view on a wide one. Both are
   * opened through here so a caller — the rail, the version chip, an inspector row, Setup's auto-open —
   * never has to know which is on screen.
   */
  const openSettings = useCallback(
    (section?: SectionId) => {
      setSettingsSection(section);
      if (narrow) setSettingsOpen(true);
      else setSettingsRequests((n) => n + 1);
    },
    [narrow],
  );

  // Replaces the old `!github.connected || !claude.configured` auto-open: `setup.autoOpen` is
  // "a row that gates launch is unmet", which also catches runtime failures. Fires at most once
  // per page load, and "Not now" (setupDismissed) holds it closed until the next one.
  useEffect(() => {
    if (promptedForSettings.current || !setup) return;
    promptedForSettings.current = true;
    if (setup.autoOpen && !setupDismissed.current) openSettings("setup");
  }, [setup, openSettings]);

  /** "Not now": in-memory only, so the next page load asks again. */
  const dismissSetup = useCallback(() => {
    setupDismissed.current = true;
    setSettingsOpen(false);
  }, []);

  /** Setup's launch row: close Setup, open the existing launcher. */
  const openLauncher = useCallback(() => {
    setSettingsOpen(false);
    setSidebarTab("new");
    setView("colonies");
    setSidebarOpen(true);
    // The cockpit has no sidebar to open. Setup lives in the dialog App owns, so this counter is how
    // its launch row reaches across and asks the cockpit for the launch view.
    setLaunchRequests((n) => n + 1);
  }, []);

  const removeSession = useCallback((id: string) => {
    setSessions((list) => removeSessionById(list, id));
  }, []);

  const upsertSession = useCallback((session: Session) => {
    // Newest-created first like GET /api/sessions, shared with the stream's single-session frames.
    setSessions((list) => upsertSessionList(list, session));
  }, []);

  // Red-team start/stop fold the returned run into the list immediately; the 5 s poll confirms.
  // Start rejects with the server's 409 reason — the card renders that verbatim at the form.
  const startRedRun = useCallback(
    async (body: StartRedTeamRunRequest) => {
      const run = await api.startRedTeamRun(body);
      setRedRuns((list) => [run, ...list.filter((r) => r.id !== run.id)]);
    },
    [api],
  );

  const stopRedRun = useCallback(
    async (id: string) => {
      const run = await api.stopRedTeamRun(id);
      setRedRuns((list) => list.map((r) => (r.id === run.id ? run : r)));
    },
    [api],
  );

  // Stable identity, so the notifier effect can call the latest selection without re-running on every render.
  const select = useCallback((id: string) => {
    setSelectedId(id);
    setView("colonies");
    setSidebarOpen(false);
  }, []);

  // A notification's click is delivered to the onSelect captured when the notification was raised,
  // which can be hours earlier — meanwhile the colony may have moved orgs, the org filter may have
  // been switched, or the colony may be gone. So the resolver below is kept in a ref that always
  // holds the latest one, and this stable wrapper is what the notifier effect passes: it reads
  // nothing but the ref, so a click resolves against the list and filter as they are at click time.
  const openFromNotificationRef = useRef<(id: string) => void>(() => {});
  const openFromNotification = useCallback((id: string) => openFromNotificationRef.current(id), []);

  // The edge-triggered notifier, fed by both paths that update `sessions` (the 4s poll and the
  // per-colony WebSocket frames). The first list only seeds the snapshot, so colonies already
  // waiting on load stay quiet — the sidebar shows them. The snapshot updates even when every
  // channel is off, so switching a channel on later cannot replay a backlog.
  const notifySeen = useRef<Record<string, SessionSnapshot> | null>(null);
  useEffect(() => {
    const snapshot = snapshotOf(sessions);
    const previous = notifySeen.current;
    notifySeen.current = snapshot;
    if (!previous) return;
    for (const event of diffEvents(previous, sessions, notifyPrefs.events)) {
      if (event.kind === "question" && notifyPrefs.sound) playQuestionBlip();
      // Browser notifications are for when this tab is not in front; in front of it, the strip and title are the message.
      if (notifyPrefs.browser && !document.hasFocus()) showColonyNotification(eventText(event), event.id, openFromNotification);
    }
  }, [sessions, notifyPrefs, openFromNotification]);

  const selectOrg = (org: string | null) => {
    setSelectedOrg(org);
    store("colonizer.org", org);
    const current = sessions.find((s) => s.id === selectedId);
    if (org && current && !sameOrg(orgOf(current), org)) {
      const inOrg = sessions.filter((s) => sameOrg(orgOf(s), org));
      setSelectedId(inOrg.find((s) => isLive(s.status))?.id ?? inOrg[0]?.id ?? null);
    }
  };

  // The strip counts every waiting colony whatever the org filter shows, so one of its entries can
  // open a colony the filtered list does not contain. Clearing goes through selectOrg — writing the
  // stored filter here too would let the two drift — and a colony the filter already shows leaves
  // the filter untouched.
  const openColony = (session: Session) => {
    const filter = orgFilterForTarget(selectedOrg, session);
    if (filter !== selectedOrg) selectOrg(filter);
    select(session.id);
  };

  // The notification click takes the same reveal as the strip — this is its only other caller, so
  // the reveal stays defined once. The colony is looked up at click time from the list as it is
  // then, because the event deliberately carries only the address (`id`, `repo`), never the org;
  // and a colony that has vanished by then cannot be hidden by any filter, so it falls through to
  // the plain select and its fallback effect for a missing id, exactly as before.
  useEffect(() => {
    openFromNotificationRef.current = (id: string) => {
      const session = sessions.find((s) => s.id === id);
      if (session) openColony(session);
      else select(id);
    };
  });

  // Like settings, memory has two frames: the narrow layout's main view, and a cockpit view.
  const openMemory = useCallback(() => {
    setSidebarOpen(false);
    if (narrow) setView("memory");
    else setMemoryRequests((n) => n + 1);
  }, [narrow]);

  // The one newly-appeared org to ask about now, if any; several pending are asked one at a time.
  const pendingOrg = useMemo(() => pendingOrgPrompt(orgs, answeredOrgs), [orgs, answeredOrgs]);

  /** The prompt card's answer, folded back so the sidebar moves at once; the 15 s poll confirms it. */
  const answerPendingOrg = useCallback((org: string, settings: OrgSettings) => {
    setAnsweredOrgs((answered) => new Set(answered).add(org));
    setOrgs((list) => list.map((info) => (sameOrg(info.org, org) ? { ...info, awaiting_decision: false, settings } : info)));
  }, []);

  const current = sessions.find((s) => s.id === selectedId) ?? null;

  const storageAlert = visibleStorageAlert(status?.storage, dismissedStorage);
  // The Setup checklist's live-map row replaces this prompt wherever Setup has been shown;
  // a mothership already set up still gets asked, in memory, on its first load of the page.
  const liveMapPrompt = telemetry !== null && telemetry.enabled === null && !telemetry.blocked_by && !settingsOpen && status !== null && !setupShown;

  const sidebar = (
    <Sidebar
      status={status}
      statusError={statusError}
      sessions={sessions}
      sessionsLoaded={sessionsLoaded}
      selectedId={selectedId}
      onSelect={select}
      onOpenColony={openColony}
      onCreated={(session) => {
        upsertSession(session);
        select(session.id);
        void loadOrgs();
      }}
      onOpenSettings={() => {
        setSidebarOpen(false);
        // Reopening mid-setup lands back on the checklist, at the first row needing the user
        // (the pane scrolls itself there). With nothing blocking, the landing is unchanged.
        setSettingsSection(setup?.autoOpen ? "setup" : undefined);
        setSettingsOpen(true);
      }}
      onClose={narrow ? () => setSidebarOpen(false) : undefined}
      orgs={orgs}
      selectedOrg={selectedOrg}
      onSelectOrg={selectOrg}
      onOpenOrgSettings={(org) => {
        setSidebarOpen(false);
        setOrgSettingsFor(org);
      }}
      view={view}
      onOpenMemory={openMemory}
      pendingMemory={pendingMemory}
      autopilotDefault={autopilotDefault}
      attentionStrip={notifyPrefs.inTab}
      tab={sidebarTab}
      onTab={setSidebarTab}
      pull={pull}
    />
  );

  // Both layouts show the same two panes; only the chrome around them differs, so they are built
  // once here and handed to whichever shell is on screen.
  const memoryPane = (
    <MemoryView
      narrow={narrow}
      selectedOrg={selectedOrg}
      orgs={orgs}
      onOpenSidebar={() => setSidebarOpen(true)}
      onChanged={() => {
        void loadPendingMemory();
        void loadOrgs();
      }}
    />
  );

  const colonyPane = current ? (
    <SessionView
      key={current.id}
      sessionId={current.id}
      fallback={current}
      interfaces={interfaces}
      narrow={narrow}
      showOrg={!selectedOrg}
      sessions={sessions}
      onSessionChanged={upsertSession}
      onSessionDeleted={removeSession}
      onSelectSession={select}
      onOpenSidebar={() => setSidebarOpen(true)}
      onOpenMemory={openMemory}
      onMemoryProposed={loadPendingMemory}
    />
  ) : (
    <EmptyState narrow={narrow} org={selectedOrg} onOpenSidebar={() => setSidebarOpen(true)} />
  );

  // Same body the dialog renders, in the cockpit's own column. Keyed on the request count so an
  // external jump ("open providers") re-seeds the section; clicking around inside it does not remount.
  /** A saved workspace, from the dialog or from Settings → Workspaces: patch it in, then refetch. */
  const saveOrgInfo = (saved: OrgInfo) => {
    setOrgs((list) => {
      const index = list.findIndex((o) => sameOrg(o.org, saved.org));
      if (index < 0) return [...list, saved];
      const next = list.slice();
      next[index] = saved;
      return next;
    });
    void loadOrgs();
  };

  const settingsPane = (close: () => void) => (
    <SettingsBody
      key={settingsRequests}
      embedded
      status={status}
      onStatusChanged={loadStatus}
      onModulesChanged={applyModules}
      telemetry={telemetry}
      onTelemetryChanged={setTelemetry}
      usage={usage}
      onUsageChanged={setUsage}
      notifications={notifyPrefs}
      onNotificationsChanged={setNotifyPrefs}
      initialSection={settingsSection}
      setup={setup}
      pull={pull}
      onLaunch={openLauncher}
      onSetupShown={() => setSetupShown(true)}
      onSetupDismissed={() => {
        setupDismissed.current = true;
        close();
      }}
      onClose={close}
      orgs={orgs}
      onOrgSaved={saveOrgInfo}
      sessions={sessions}
    />
  );

  const orgPrompt = pendingOrg && (
    // A standalone card at the top of the pane: seen without hunting for it, but nothing
    // behind it is blocked while the decision waits.
    <div className="shrink-0 border-b border-border px-4 py-3 sm:px-6">
      <div className="mx-auto w-full max-w-xl">
        <OrgPromptCard org={pendingOrg.org} avatarUrl={pendingOrg.avatar_url} onAnswered={answerPendingOrg} />
      </div>
    </div>
  );

  return (
    <div className="flex h-full min-h-0">
      {narrow ? (
        <>
          {sidebarOpen && (
            <div className="fixed inset-0 z-40 flex">
              <div className="absolute inset-0 bg-black/40" onClick={() => setSidebarOpen(false)} aria-hidden="true" />
              <aside className="relative h-full w-[min(340px,88vw)] border-r border-border bg-panel shadow-[var(--shadow)]">
                {sidebar}
              </aside>
            </div>
          )}
          <main className="flex h-full min-w-0 flex-1 flex-col">
            {orgPrompt}
            <div className="min-h-0 flex-1">{view === "memory" ? memoryPane : colonyPane}</div>
          </main>
        </>
      ) : (
        // The cockpit is a desktop shell — a rail, a nest and a 360px inspector need the width —
        // so a narrow window keeps the sidebar layout above rather than folding the nest up.
        <div className="flex h-full min-w-0 flex-1 flex-col">
          {orgPrompt}
          <div className="min-h-0 flex-1">
            <Cockpit
              sessions={sessions}
              orgs={orgs}
              redRuns={redRuns}
              selectedOrg={selectedOrg}
              onSelectOrg={selectOrg}
              selectedId={selectedId}
              onSelectSession={setSelectedId}
              onOpenColony={openColony}
              status={status}
              statusError={statusError}
              fleet={fleet}
              update={updateStatus}
              liveConnection={liveConnection}
              liveStorage={liveStorage}
              autopilotDefault={autopilotDefault}
              launchRequests={launchRequests}
              settingsRequests={settingsRequests}
              memoryRequests={memoryRequests}
              pendingMemory={pendingMemory}
              settings={settingsPane}
              onSessionChanged={upsertSession}
              onRedStart={startRedRun}
              onRedStop={stopRedRun}
              onCreated={(session) => {
                upsertSession(session);
                select(session.id);
                void loadOrgs();
              }}
              onOpenSettings={openSettings}
              onOpenOrgSettings={(org) => openSettings(`org:${org}`)}
              onInspectorShown={setInspectorShown}
              colony={colonyPane}
              memory={memoryPane}
            />
          </div>
        </div>
      )}
      <SettingsDialog
        open={settingsOpen}
        onClose={() => {
          setSettingsOpen(false);
          void loadTelemetry();
          void loadUsage();
        }}
        status={status}
        onStatusChanged={loadStatus}
        onModulesChanged={applyModules}
        telemetry={telemetry}
        onTelemetryChanged={setTelemetry}
        usage={usage}
        onUsageChanged={setUsage}
        notifications={notifyPrefs}
        onNotificationsChanged={setNotifyPrefs}
        initialSection={settingsSection}
        setup={setup}
        pull={pull}
        onLaunch={openLauncher}
        onSetupShown={() => setSetupShown(true)}
        onSetupDismissed={dismissSetup}
      />
      {(storageAlert || liveMapPrompt) && (
        // Both fixed cards live in the same corner; the shared column keeps them stacked and clickable.
        <div className={cx("fixed z-30 flex flex-col gap-3", floatingColumnClass(narrow, inspectorShown))}>
          {storageAlert && (
            <StorageAlert
              storage={storageAlert}
              onDismiss={() => setDismissedStorage((dismissed) => dismissStorageAlert(dismissed, storageAlert))}
            />
          )}
          {liveMapPrompt && (
            <LiveMapPrompt
              onAnswered={setTelemetry}
              onDetails={() => {
                setSettingsSection("live-map");
                setSettingsOpen(true);
              }}
            />
          )}
        </div>
      )}
      <OrgSettingsDialog
        org={orgSettingsFor}
        info={orgs.find((o) => sameOrg(o.org, orgSettingsFor))}
        onClose={() => setOrgSettingsFor(null)}
        onSaved={saveOrgInfo}
      />
    </div>
  );
}

/** Asked once, after setup: the live map stays off until the user says otherwise (docs/telemetry.md). */
function LiveMapPrompt({
  onAnswered,
  onDetails,
}: {
  onAnswered: (telemetry: TelemetryStatus) => void;
  onDetails: () => void;
}) {
  const api = useApi();
  const [busy, setBusy] = useState(false);
  const answer = async (enabled: boolean) => {
    setBusy(true);
    try {
      onAnswered(await api.setTelemetry(enabled));
    } catch {
      setBusy(false);
    }
  };
  return (
    <div role="region" aria-label="Live map" className="rounded-2xl border border-border bg-panel p-4 shadow-[var(--shadow)]">
      <p className="text-[14px] font-semibold">Put this mothership on the live map?</p>
      <p className="mt-1.5 text-[12.5px] text-muted">
        colonizer.dev/live shows where colonies are running, to within about 25 km. If yours is the only mothership in its area,
        that dot is you. It gets a heartbeat every 5 minutes: a random id, the version, the platform and how many colonies run.
        Nothing about your code. Off unless you say yes.
      </p>
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button variant="primary" size="sm" disabled={busy} onClick={() => void answer(true)}>
          Show on the map
        </Button>
        <Button size="sm" disabled={busy} onClick={() => void answer(false)}>
          No thanks
        </Button>
        <button type="button" onClick={onDetails} className="ml-auto cursor-pointer text-[12.5px] text-accent hover:underline">
          What is sent
        </button>
      </div>
    </div>
  );
}

/** Identity of a storage alert for dismissal: its ts, or — for older motherships that omit it — its message, prefixed so it cannot pass for a ts. */
const storageAlertKey = (storage: StorageHealth) => storage.ts ?? `no-ts:${storage.message ?? "unknown"}`;

/** Whether `storage` carries an alert: a failing write, a recovered one (`ok` again, but the gap it reports still happened), or load damage, which is `ok` because writes work yet never recovers (issue #371). */
export const storageAlertShown = (storage: StorageHealth) => storage.kind === "load_damage" || storage.ok === false || !!storage.recovered_at;

/** The storage alert to show, or null when there is none or the operator dismissed this one. */
export const visibleStorageAlert = (storage: StorageHealth | undefined, dismissed: ReadonlySet<string>) =>
  storage && storageAlertShown(storage) && !dismissed.has(storageAlertKey(storage)) ? storage : null;

/** `dismissed` plus `storage`'s key; earlier keys stay, so a dismissed alert the mothership shows again stays hidden. */
export const dismissStorageAlert = (dismissed: ReadonlySet<string>, storage: StorageHealth): ReadonlySet<string> =>
  new Set(dismissed).add(storageAlertKey(storage));

/** harness_log frames render ts with toLocaleTimeString (SessionView's activity strip); an odd or missing ts shows nothing. */
const localTime = (ts: string | null | undefined) => {
  const at = ts ? new Date(ts) : null;
  return at && !Number.isNaN(at.getTime()) ? at.toLocaleTimeString() : null;
};

/**
 * A write the mothership could not make (issue #87). Sticky server-side; dismissed here per failure, a newer one reopens it. Once a later write goes through (issue #220) the card stays, in amber, saying so: the gap it reports still happened.
 * Colony records lost at startup (issue #371) never recover, whatever `ok` says: that card stays red, pointing at the saved copy, until dismissed.
 */
export function StorageAlert({ storage, onDismiss }: { storage: StorageHealth; onDismiss: () => void }) {
  const raisedAt = localTime(storage.ts);
  const recoveredAt = localTime(storage.recovered_at);
  const loadDamage = storage.kind === "load_damage";
  const recovered = storage.ok && !loadDamage;
  const meta = [
    raisedAt ? (loadDamage ? `Found at startup, ${raisedAt}` : `Failed at ${raisedAt}`) : null,
    !loadDamage && storage.failures != null ? `${storage.failures} failed ${storage.failures === 1 ? "write" : "writes"}` : null,
    recovered && recoveredAt ? `Writing again since ${recoveredAt}` : null,
  ].filter(Boolean);
  const tone = recovered ? "text-warn" : "text-err";
  const heading = loadDamage
    ? "The mothership could not load all its colony records"
    : recovered
      ? "The mothership is writing to disk again"
      : "The mothership could not write to disk";
  const note = loadDamage
    ? "The copy named above keeps the original file as it was. Later writes do not bring the missing colonies back, so this card stays until you dismiss it."
    : recovered
      ? "Writes are going through again, but colony event logs may still have gaps from while they failed. Dismissing hides this card."
      : "What you see here can drift from what is on disk, and colony event logs may have gaps. The card turns amber once a write goes through again — dismissing just hides it.";
  return (
    <div
      role={recovered ? "status" : "alert"}
      className={cx(
        "rounded-2xl border p-4 shadow-[var(--shadow)]",
        recovered ? "border-warn/30 bg-warn-soft" : "border-err/30 bg-err-soft",
      )}
    >
      <p className={cx("text-[14px] font-semibold", tone)}>{heading}</p>
      {storage.message && <p className={cx("mt-1.5 font-mono text-[12px] [overflow-wrap:anywhere]", tone)}>{storage.message}</p>}
      <p className="mt-1.5 text-[12.5px] text-muted">{note}</p>
      {meta.length > 0 && <p className={cx("mt-1.5 text-[12px]", tone)}>{meta.join(" · ")}</p>}
      <div className="mt-3 flex justify-end">
        <Button size="sm" onClick={onDismiss}>
          Dismiss
        </Button>
      </div>
    </div>
  );
}

function EmptyState({ narrow, org, onOpenSidebar }: { narrow: boolean; org: string | null; onOpenSidebar: () => void }) {
  return (
    <div className="flex h-full flex-col">
      {narrow && (
        <div className="flex items-center gap-2 border-b border-border bg-panel px-3 py-2">
          <button
            type="button"
            onClick={onOpenSidebar}
            aria-label="Open sidebar"
            className="grid size-9 cursor-pointer place-items-center rounded-lg text-muted hover:bg-panel-2 hover:text-text"
          >
            <IconMenu size={18} />
          </button>
          <span className="font-semibold">Colonizer</span>
        </div>
      )}
      <div className="grid flex-1 place-items-center p-6">
        <div className="max-w-sm text-center">
          <div className="mx-auto mb-4 grid size-12 place-items-center rounded-2xl bg-accent-soft text-accent">
            <IconSpark size={22} />
          </div>
          <h2 className="text-lg font-semibold">{org ? `Pick an issue in ${org} to launch a colony` : "Pick an issue to launch a colony"}</h2>
          <p className="mt-2 text-[13.5px] text-muted">
            Each colony is a private microVM with a fresh git worktree and a coding agent. Watch it work, answer its questions,
            open a terminal, and create the pull request when you're happy. The Mothership keeps them all in sight.
          </p>
          {narrow && (
            <Button className="mt-4" variant="primary" onClick={onOpenSidebar}>
              Browse issues
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
