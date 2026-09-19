import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useApi } from "./context";
import { IconMenu, IconSpark } from "./components/icons";
import { MemoryView } from "./components/MemoryView";
import { OrgPromptCard } from "./components/OrgPromptCard";
import { OrgSettingsDialog } from "./components/OrgSettingsDialog";
import { SessionView, type InterfaceFlags } from "./components/SessionView";
import { SettingsDialog, type SectionId } from "./components/SettingsDialog";
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
import { pendingOrgPrompt } from "./orgs";
import { setupView, stackPresetOf, type SetupView } from "./setup";
import { useImagePull } from "./useImagePull";
import type {
  HarnessStatus,
  ModuleInfo,
  OrgInfo,
  OrgSettings,
  Session,
  StorageHealth,
  TelemetryStatus,
  UpdateStatus,
  UsageStatus,
} from "./types";

export function App() {
  const api = useApi();
  const narrow = useMediaQuery("(max-width: 899px)");
  const [status, setStatus] = useState<HarnessStatus | null>(null);
  const [statusError, setStatusError] = useState(false);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(() => stored("colonizer.session"));
  const [interfaces, setInterfaces] = useState<InterfaceFlags>({ chat: true, terminal: true });
  const [autopilotDefault, setAutopilotDefault] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SectionId | undefined>(undefined);
  const [telemetry, setTelemetry] = useState<TelemetryStatus | null>(null);
  const [usage, setUsage] = useState<UsageStatus | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [orgs, setOrgs] = useState<OrgInfo[]>([]);
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
  const [dismissedStorageTs, setDismissedStorageTs] = useState<string | null>(null);
  const promptedForSettings = useRef(false);
  // Setup's own state lives only in this page load: "Not now" holds the auto-open off, and
  // "has been shown" retires the standalone live-map prompt. Neither is ever persisted.
  const setupDismissed = useRef(false);
  const [setupShown, setSetupShown] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus | null>(null);
  const [launchRequests, setLaunchRequests] = useState(0);
  // The sidebar's tab, lifted so Setup's launch button can open the launcher directly.
  // (colonizer.sidebar-tab stays the sidebar's own memory of itself.)
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>(() => (stored("colonizer.sidebar-tab") === "new" ? "new" : "sessions"));
  const [modules, setModules] = useState<ModuleInfo[]>([]);
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

  const loadSessions = useCallback(async () => {
    try {
      setSessions(await api.sessions());
      setSessionsLoaded(true);
    } catch {
      /* keep the last list */
    }
  }, [api]);

  const loadOrgs = useCallback(async () => {
    try {
      setOrgs(await api.orgs());
    } catch {
      /* older mothership, or offline: the switcher falls back to orgs seen in colonies */
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
    void loadSessions();
    void loadTelemetry();
    void loadUsage();
    void loadOrgs();
    void loadPendingMemory();
    void loadUpdate();
    api.modules().then(applyModules).catch(() => {});
    const timers = [
      setInterval(loadSessions, 4000),
      setInterval(loadStatus, 30_000),
      setInterval(loadOrgs, 15_000),
      setInterval(loadPendingMemory, 10_000),
      // A release check is a request to github.com, so it runs far more slowly than the rest.
      setInterval(loadUpdate, 900_000),
    ];
    return () => timers.forEach(clearInterval);
  }, [api, loadStatus, loadSessions, loadOrgs, loadPendingMemory, loadTelemetry, loadUsage, loadUpdate, applyModules]);

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

  // Replaces the old `!github.connected || !claude.configured` auto-open: `setup.autoOpen` is
  // "a row that gates launch is unmet", which also catches runtime failures. Fires at most once
  // per page load, and "Not now" (setupDismissed) holds it closed until the next one.
  useEffect(() => {
    if (promptedForSettings.current || !setup) return;
    promptedForSettings.current = true;
    if (setup.autoOpen && !setupDismissed.current) {
      setSettingsSection("setup");
      setSettingsOpen(true);
    }
  }, [setup]);

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
    setSessions((list) => list.filter((s) => s.id !== id));
  }, []);

  const upsertSession = useCallback((session: Session) => {
    setSessions((list) => {
      const index = list.findIndex((s) => s.id === session.id);
      if (index < 0) return [session, ...list];
      const next = list.slice();
      next[index] = session;
      return next;
    });
  }, []);

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

  const openMemory = useCallback(() => {
    setView("memory");
    setSidebarOpen(false);
  }, []);

  // The one newly-appeared org to ask about now, if any; several pending are asked one at a time.
  const pendingOrg = useMemo(() => pendingOrgPrompt(orgs, answeredOrgs), [orgs, answeredOrgs]);

  /** The prompt card's answer, folded back so the sidebar moves at once; the 15 s poll confirms it. */
  const answerPendingOrg = useCallback((org: string, settings: OrgSettings) => {
    setAnsweredOrgs((answered) => new Set(answered).add(org));
    setOrgs((list) => list.map((info) => (sameOrg(info.org, org) ? { ...info, awaiting_decision: false, settings } : info)));
  }, []);

  const current = sessions.find((s) => s.id === selectedId) ?? null;

  const storage = status?.storage;
  const storageAlert = storage && storage.ok === false && storageAlertKey(storage) !== dismissedStorageTs ? storage : null;
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
      onSessionChanged={upsertSession}
      onSessionDeleted={removeSession}
      onOpenSidebar={() => setSidebarOpen(true)}
      onOpenMemory={openMemory}
      onMemoryProposed={loadPendingMemory}
    />
  ) : (
    <EmptyState narrow={narrow} org={selectedOrg} onOpenSidebar={() => setSidebarOpen(true)} />
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
              selectedOrg={selectedOrg}
              onSelectOrg={selectOrg}
              selectedId={selectedId}
              onSelectSession={setSelectedId}
              onOpenColony={openColony}
              status={status}
              update={updateStatus}
              autopilotDefault={autopilotDefault}
              launchRequests={launchRequests}
              onSessionChanged={upsertSession}
              onCreated={(session) => {
                upsertSession(session);
                select(session.id);
                void loadOrgs();
              }}
              onOpenSettings={(section) => {
                setSettingsSection(section ?? (setup?.autoOpen ? "setup" : undefined));
                setSettingsOpen(true);
              }}
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
        <div className={cx("fixed z-30 flex flex-col gap-3", narrow ? "inset-x-3 bottom-3" : "bottom-5 right-5 w-[380px]")}>
          {storageAlert && <StorageAlert storage={storageAlert} onDismiss={() => setDismissedStorageTs(storageAlertKey(storageAlert))} />}
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
        onSaved={(saved) => {
          setOrgs((list) => {
            const index = list.findIndex((o) => sameOrg(o.org, saved.org));
            if (index < 0) return [...list, saved];
            const next = list.slice();
            next[index] = saved;
            return next;
          });
          void loadOrgs();
        }}
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

/** Identity of a storage failure for dismissal: its ts, or — for older motherships that omit it — its message, prefixed so neither can be confused with "nothing dismissed yet" (null). */
const storageAlertKey = (storage: StorageHealth) => storage.ts ?? `no-ts:${storage.message ?? "unknown"}`;

/** A write the mothership could not make (issue #87). Sticky server-side; dismissed here per failure, a newer one reopens it. */
function StorageAlert({ storage, onDismiss }: { storage: StorageHealth; onDismiss: () => void }) {
  // harness_log frames render ts with toLocaleTimeString (SessionView's activity strip); an odd or missing ts shows nothing.
  const at = storage.ts ? new Date(storage.ts) : null;
  const when = at && !Number.isNaN(at.getTime()) ? `At ${at.toLocaleTimeString()}` : null;
  const meta = [
    when,
    storage.failures != null ? `${storage.failures} failed ${storage.failures === 1 ? "write" : "writes"}` : null,
  ].filter(Boolean);
  return (
    <div role="alert" className="rounded-2xl border border-err/30 bg-err-soft p-4 shadow-[var(--shadow)]">
      <p className="text-[14px] font-semibold text-err">The mothership could not write to disk</p>
      {storage.message && <p className="mt-1.5 font-mono text-[12px] text-err [overflow-wrap:anywhere]">{storage.message}</p>}
      <p className="mt-1.5 text-[12.5px] text-muted">
        What you see here can drift from what is on disk, and colony event logs may have gaps. The alert clears only when the
        mothership restarts — dismissing just hides this card.
      </p>
      {meta.length > 0 && <p className="mt-1.5 text-[12px] text-err">{meta.join(" · ")}</p>}
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
