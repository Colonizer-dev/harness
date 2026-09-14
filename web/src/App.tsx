import { useCallback, useEffect, useRef, useState } from "react";
import { useApi } from "./context";
import { IconMenu, IconSpark } from "./components/icons";
import { SessionView, type InterfaceFlags } from "./components/SessionView";
import { SettingsDialog } from "./components/SettingsDialog";
import { Sidebar } from "./components/Sidebar";
import { Button, isLive, store, stored, useMediaQuery } from "./components/ui";
import type { HarnessStatus, ModuleInfo, Session } from "./types";

export function App() {
  const api = useApi();
  const narrow = useMediaQuery("(max-width: 899px)");
  const [status, setStatus] = useState<HarnessStatus | null>(null);
  const [statusError, setStatusError] = useState(false);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(() => stored("colonizer.session"));
  const [interfaces, setInterfaces] = useState<InterfaceFlags>({ chat: true, terminal: true });
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
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

  const applyModules = useCallback((modules: ModuleInfo[]) => {
    const module = modules.find((m) => m.kind === "interfaces");
    if (!module || !module.enabled) {
      setInterfaces({ chat: true, terminal: true });
      return;
    }
    const flag = (key: string) => (module.settings?.[key] ?? module.schema?.properties?.[key]?.default) !== false;
    setInterfaces({ chat: flag("chat"), terminal: flag("terminal") });
  }, []);

  useEffect(() => {
    void loadStatus();
    void loadSessions();
    api.modules().then(applyModules).catch(() => {});
    const sessionsTimer = setInterval(loadSessions, 4000);
    const statusTimer = setInterval(loadStatus, 30_000);
    return () => {
      clearInterval(sessionsTimer);
      clearInterval(statusTimer);
    };
  }, [api, loadStatus, loadSessions, applyModules]);

  // Keep a valid selection: fall back to the newest running session.
  useEffect(() => {
    if (!sessionsLoaded) return;
    if (selectedId && sessions.some((s) => s.id === selectedId)) return;
    setSelectedId(sessions.find((s) => isLive(s.status))?.id ?? null);
  }, [sessionsLoaded, sessions, selectedId]);

  useEffect(() => {
    store("colonizer.session", selectedId);
  }, [selectedId]);

  useEffect(() => {
    if (promptedForSettings.current || !status) return;
    promptedForSettings.current = true;
    if (!status.github.connected || !status.claude.configured) setSettingsOpen(true);
  }, [status]);

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
    setSidebarOpen(false);
  };

  const current = sessions.find((s) => s.id === selectedId) ?? null;

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
      }}
      onOpenSettings={() => {
        setSidebarOpen(false);
        setSettingsOpen(true);
      }}
      onClose={narrow ? () => setSidebarOpen(false) : undefined}
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
        {current ? (
          <SessionView
            key={current.id}
            sessionId={current.id}
            fallback={current}
            interfaces={interfaces}
            narrow={narrow}
            onSessionChanged={upsertSession}
            onOpenSidebar={() => setSidebarOpen(true)}
          />
        ) : (
          <EmptyState narrow={narrow} onOpenSidebar={() => setSidebarOpen(true)} />
        )}
      </main>
      <SettingsDialog
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        status={status}
        onStatusChanged={loadStatus}
        onModulesChanged={applyModules}
      />
    </div>
  );
}

function EmptyState({ narrow, onOpenSidebar }: { narrow: boolean; onOpenSidebar: () => void }) {
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
          <h2 className="text-lg font-semibold">Pick an issue to launch a colony</h2>
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
