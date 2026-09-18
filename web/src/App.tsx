import { useCallback, useEffect, useRef, useState } from "react";
import { useApi } from "./context";
import { IconMenu, IconSpark } from "./components/icons";
import { MemoryView } from "./components/MemoryView";
import { OrgSettingsDialog } from "./components/OrgSettingsDialog";
import { SessionView, type InterfaceFlags } from "./components/SessionView";
import { SettingsDialog, type SectionId } from "./components/SettingsDialog";
import { Sidebar, type MainView } from "./components/Sidebar";
import { Button, cx, isLive, orgOf, sameOrg, store, stored, useMediaQuery } from "./components/ui";
import type { HarnessStatus, ModuleInfo, OrgInfo, Session, StorageHealth, TelemetryStatus, UsageStatus } from "./types";

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
  // The mothership's storage alert is sticky, so dismissal is client-side, keyed on the alert's ts
  // (or its message when an older mothership omits the ts): a newer failure shows the card again.
  const [dismissedStorageTs, setDismissedStorageTs] = useState<string | null>(null);
  const promptedForSettings = useRef(false);

  const loadStatus = useCallback(async () => {
    try {
      setStatus(await api.status());
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

  useEffect(() => {
    void loadStatus();
    void loadSessions();
    void loadTelemetry();
    void loadUsage();
    void loadOrgs();
    void loadPendingMemory();
    api.modules().then(applyModules).catch(() => {});
    const timers = [
      setInterval(loadSessions, 4000),
      setInterval(loadStatus, 30_000),
      setInterval(loadOrgs, 15_000),
      setInterval(loadPendingMemory, 10_000),
    ];
    return () => timers.forEach(clearInterval);
  }, [api, loadStatus, loadSessions, loadOrgs, loadPendingMemory, loadTelemetry, loadUsage, applyModules]);

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

  useEffect(() => {
    if (promptedForSettings.current || !status) return;
    promptedForSettings.current = true;
    if (!status.github.connected || !status.claude.configured) setSettingsOpen(true);
  }, [status]);

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

  const select = (id: string) => {
    setSelectedId(id);
    setView("colonies");
    setSidebarOpen(false);
  };

  const selectOrg = (org: string | null) => {
    setSelectedOrg(org);
    store("colonizer.org", org);
    const current = sessions.find((s) => s.id === selectedId);
    if (org && current && !sameOrg(orgOf(current), org)) {
      const inOrg = sessions.filter((s) => sameOrg(orgOf(s), org));
      setSelectedId(inOrg.find((s) => isLive(s.status))?.id ?? inOrg[0]?.id ?? null);
    }
  };

  const openMemory = useCallback(() => {
    setView("memory");
    setSidebarOpen(false);
  }, []);

  const current = sessions.find((s) => s.id === selectedId) ?? null;

  const storage = status?.storage;
  const storageAlert = storage && storage.ok === false && storageAlertKey(storage) !== dismissedStorageTs ? storage : null;
  const liveMapPrompt =
    telemetry !== null && telemetry.enabled === null && !telemetry.blocked_by && !settingsOpen && status !== null && status.github.connected && status.claude.configured;

  const sidebar = (
    <Sidebar
      status={status}
      statusError={statusError}
      sessions={sessions}
      sessionsLoaded={sessionsLoaded}
      selectedId={selectedId}
      onSelect={select}
      onCreated={(session) => {
        upsertSession(session);
        select(session.id);
        void loadOrgs();
      }}
      onOpenSettings={() => {
        setSidebarOpen(false);
        setSettingsSection(undefined);
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
    />
  );

  return (
    <div className="flex h-full min-h-0">
      {!narrow && <aside className="h-full w-[320px] shrink-0 border-r border-border bg-panel">{sidebar}</aside>}
      {narrow && sidebarOpen && (
        <div className="fixed inset-0 z-40 flex">
          <div className="absolute inset-0 bg-black/40" onClick={() => setSidebarOpen(false)} aria-hidden="true" />
          <aside className="relative h-full w-[min(340px,88vw)] border-r border-border bg-panel shadow-[var(--shadow)]">{sidebar}</aside>
        </div>
      )}
      <main className="h-full min-w-0 flex-1">
        {view === "memory" ? (
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
        ) : current ? (
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
        )}
      </main>
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
        initialSection={settingsSection}
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
