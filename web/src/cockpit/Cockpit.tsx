// The cockpit: the rail, the header, and whichever view the rail is pointing at.
//
// It owns only what is its own — which view is showing, what the inspector is looking at, and the
// theme override. The colony and memory panes are passed in as slots so App keeps its existing
// wiring for them, and settings stays the dialog App already owns rather than a second copy.
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../context";
import type { QuestionActions } from "../components/AskUserCard";
import type { SectionId } from "../components/SettingsDialog";
import { isLive, orgOf, sameOrg, store, stored } from "../components/ui";
import { needsYou } from "../notifications";
import { orgEntries } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { sessionCost, sumCosts } from "../spend";
import { buildThread, useSessionStream } from "../sessionStream";
import type { FleetHost, HarnessStatus, OrgInfo, RedTeamRun, Repo, Session, StartRedTeamRunRequest, UpdateStatus } from "../types";
import { Header } from "./Header";
import { HistoryView } from "./HistoryView";
import { InboxView } from "./InboxView";
import { Inspector, pendingQuestionsOf, type InspectorTarget } from "./Inspector";
import { LaunchView } from "./LaunchView";
import { NestView } from "./NestView";
import { OverviewView } from "./OverviewView";
import { QuotaBanner, dismissQuotaBanner, resumeQuotaParkedSessions, visibleQuotaBanner } from "./QuotaBanner";
import { Rail, type CockpitView } from "./Rail";
import { needCountByOrg } from "./feed";

const VIEW_KEY = "colonizer.cockpitView";
const THEME_KEY = "colonizer.theme";

const VIEWS: readonly CockpitView[] = ["overview", "home", "colony", "launch", "inbox", "history", "settings", "memory"];

function storedView(): CockpitView {
  const saved = stored(VIEW_KEY);
  return VIEWS.includes(saved as CockpitView) ? (saved as CockpitView) : "home";
}

function storedTheme(): "light" | "dark" | null {
  const saved = stored(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : null;
}

const CRUMB: Record<CockpitView, string> = {
  overview: "overview",
  home: "nest",
  colony: "colony",
  launch: "launch",
  inbox: "inbox",
  history: "history",
  settings: "settings",
  memory: "memory",
};

export function Cockpit({
  sessions,
  orgs,
  redRuns = [],
  selectedOrg,
  onSelectOrg,
  selectedId,
  onSelectSession,
  onOpenColony,
  status,
  fleet,
  update,
  autopilotDefault,
  launchRequests,
  settingsRequests,
  settings,
  onSessionChanged,
  onRedStart,
  onRedStop,
  onCreated,
  onOpenSettings,
  onInspectorShown,
  colony,
  memory,
}: {
  /** Every colony the mothership knows; the cockpit filters to the chosen workspace itself. */
  sessions: Session[];
  orgs: OrgInfo[];
  /** Red-team runs; the overview card and the nest's raid overlay read them. */
  redRuns?: RedTeamRun[];
  selectedOrg: string | null;
  onSelectOrg: (org: string | null) => void;
  selectedId: string | null;
  onSelectSession: (id: string) => void;
  /** Selecting a colony that the org filter would hide, which App resolves before selecting. */
  onOpenColony: (session: Session) => void;
  status: HarnessStatus | null;
  /** Self plus every configured peer (issue #231); older mothership builds send an empty list. */
  fleet?: FleetHost[];
  update: UpdateStatus | null;
  autopilotDefault: boolean;
  /** Bumped by Setup's launch row, which lives in the settings body App owns. */
  launchRequests: number;
  /** Bumped whenever something outside the cockpit asks for settings, with the section already set. */
  settingsRequests: number;
  /** The settings body, given the way back out — the cockpit owns the view, so it owns the exit. */
  settings: (close: () => void) => ReactNode;
  onSessionChanged: (session: Session) => void;
  onRedStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onRedStop?: (id: string) => Promise<void>;
  onCreated: (session: Session) => void;
  onOpenSettings: (section?: SectionId) => void;
  /** Told whether the inspector is on screen, so App can keep its fixed cards clear of it. */
  onInspectorShown?: (shown: boolean) => void;
  /** The open colony's own pane, wired by App (chat, terminal, publish). */
  colony: ReactNode;
  memory: ReactNode;
}) {
  const api = useApi();
  const toast = useToast();
  const [view, setView] = useState<CockpitView>(storedView);
  const [theme, setTheme] = useState<"light" | "dark" | null>(storedTheme);
  const [inspector, setInspector] = useState<InspectorTarget | null>(null);
  const [repos, setRepos] = useState<Repo[]>([]);

  useEffect(() => {
    store(VIEW_KEY, view);
  }, [view]);

  // The inspector renders only on the home view, whatever it is looking at.
  useEffect(() => {
    onInspectorShown?.(view === "home");
  }, [view, onInspectorShown]);

  // An explicit choice is written on the root, where index.css's :root[data-theme] blocks pick it
  // up; clearing it hands the page back to prefers-color-scheme.
  useEffect(() => {
    const root = document.documentElement;
    if (theme) root.setAttribute("data-theme", theme);
    else root.removeAttribute("data-theme");
    store(THEME_KEY, theme);
  }, [theme]);

  useEffect(() => {
    if (launchRequests > 0) setView("launch");
  }, [launchRequests]);

  useEffect(() => {
    if (settingsRequests > 0) setView("settings");
  }, [settingsRequests]);

  const toggleTheme = useCallback(() => {
    setTheme((current) => {
      // The first press flips whatever the OS is showing, so the button never looks like a no-op.
      const showing = current ?? (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
      return showing === "dark" ? "light" : "dark";
    });
  }, []);

  // The frontier's badge. One fetch: the count moves slowly and a poll would cost a GitHub call a minute.
  useEffect(() => {
    if (!status?.github.connected) return;
    let cancelled = false;
    api
      .repos()
      .then((list) => !cancelled && setRepos(list))
      .catch(() => {
        /* the badge simply reads 0; nothing here is worth an error card */
      });
    return () => {
      cancelled = true;
    };
  }, [api, status?.github.connected]);

  // orgEntries decides what counts as a workspace: an org still awaiting a decision is not one yet
  // (the prompt card is where that is answered), and a switched-off one is hidden. The rail and the
  // switcher must agree with the sidebar about that, so they read the same function.
  const workspaces = useMemo(() => orgEntries(orgs, sessions).visible, [orgs, sessions]);

  const inOrg = useMemo(
    () => sortSessions(selectedOrg ? sessions.filter((s) => sameOrg(orgOf(s), selectedOrg)) : sessions),
    [sessions, selectedOrg],
  );

  const needByOrg = useMemo(() => needCountByOrg(sessions), [sessions]);
  // Two different counts, and mixing them up is what makes a header say "1 need you" over a
  // workspace where nothing does. `needAnywhere` belongs to the rail's inbox badge and the inbox
  // itself, which are deliberately cross-workspace; `needHere` sits beside the live count and the
  // spend, which are this workspace's.
  const needAnywhere = useMemo(() => Object.values(needByOrg).reduce((a, b) => a + b, 0), [needByOrg]);
  const needHere = useMemo(() => inOrg.filter(needsYou).length, [inOrg]);
  const liveCount = inOrg.filter((s) => isLive(s.status)).length;
  const queuedCount = inOrg.filter((s) => s.status === "queued").length;
  const spend = sumCosts(inOrg.map(sessionCost));
  const backlogCount = repos
    .filter((r) => !selectedOrg || sameOrg(r.full_name.split("/")[0], selectedOrg))
    .reduce((total, r) => total + r.open_issues_count, 0);

  // Avatars come from /api/orgs, which keys them by org; a colony whose owner is not a workspace
  // (or an older mothership that sends none) falls back to the initial the Avatar draws.
  const avatarFor = useCallback(
    (org: string) => workspaces.find((w) => sameOrg(w.org, org))?.avatar ?? null,
    [workspaces],
  );

  // Only one stream at a time: the colony view opens its own, so the nest only listens while it is
  // the view on screen. Without this the open colony would carry two sockets.
  const streamFor = view === "home" && inspector?.kind === "colony" ? inspector.session.id : null;
  const { stream, state } = useSessionStream(api, streamFor);
  const settlers = useMemo(() => Object.values(buildThread(state).subagents), [state]);
  // The inspector answers the colony's question from this same stream, so the pane clears itself
  // the moment `question_answered` arrives — nothing here is cached from render to render.
  const pendingQuestions = useMemo(() => pendingQuestionsOf(state), [state]);
  const questionActions = useMemo<QuestionActions>(() => {
    const live = inspector?.kind === "colony" ? isLive(inspector.session.status) : false;
    const connected = state.connection === "open";
    return {
      answer: (questionId, answers, response) => {
        try {
          const sent = stream?.send({ type: "answer", question_id: questionId, answers, response }) ?? false;
          if (!sent) toast("Not connected to the colony — try again in a moment.", "error");
          return sent;
        } catch (error) {
          toast(errorMessage(error), "error");
          return false;
        }
      },
      submitting: state.submitting,
      canAnswer: connected && live,
      blockedBy: !live ? "ended" : !connected ? "disconnected" : null,
    };
  }, [stream, inspector, state.submitting, state.connection, toast]);

  // The inspector points at a colony by identity, so a poll that replaces the list must not leave it
  // holding a stale copy — or pointing at a colony that has since been forgotten.
  useEffect(() => {
    setInspector((current) => {
      if (current?.kind !== "colony") return current;
      const fresh = sessions.find((s) => s.id === current.session.id);
      return fresh ? { kind: "colony", session: fresh } : null;
    });
  }, [sessions]);

  const navigate = useCallback((next: CockpitView) => setView(next), []);

  const openColonyById = useCallback(
    (id: string) => {
      const session = sessions.find((s) => s.id === id);
      if (session) onOpenColony(session);
      else onSelectSession(id);
      setView("colony");
    },
    [sessions, onOpenColony, onSelectSession],
  );

  const act = useCallback(
    async (id: string, run: (id: string) => Promise<Session>) => {
      try {
        onSessionChanged(await run(id));
      } catch {
        /* the 4s poll is the backstop; a failed stop or resume shows up there */
      }
    },
    [onSessionChanged],
  );

  // The global quota banner's keyed dismissal: dismissing hides this pause, and a new reset (or a
  // new reason) re-shows it — the same pattern as the storage alert App owns.
  const [dismissedQuota, setDismissedQuota] = useState<ReadonlySet<string>>(() => new Set());
  const quotaBanner = visibleQuotaBanner(status?.quota ?? null, dismissedQuota);
  // Resume-all has no bulk endpoint: one resume per parked colony through the existing act path,
  // settled per colony so a single 409 cannot block the rest.
  const resumeAllQuotaParked = useCallback(
    () => resumeQuotaParkedSessions(sessions, (id) => act(id, (x) => api.resumeSession(x))),
    [sessions, act, api],
  );

  const body = () => {
    switch (view) {
      case "colony":
        return colony;
      case "memory":
        return memory;
      case "settings":
        return settings(() => setView("home"));
      case "overview":
        return (
          <OverviewView
            sessions={sessions}
            orgs={workspaces}
            cost={sumCosts(sessions.map(sessionCost))}
            host={status?.host ?? null}
            fleet={fleet}
            runs={redRuns}
            quota={status?.quota ?? null}
            onStart={onRedStart}
            onStop={onRedStop}
            onOpenOrg={(org) => {
              onSelectOrg(org);
              setView("home");
              setInspector(null);
            }}
            onOpenColony={openColonyById}
            onSelect={(session) => {
              // "answer in the pane": land on the nest with the colony's question in the inspector.
              setInspector({ kind: "colony", session });
              setView("home");
            }}
          />
        );
      case "launch":
        return (
          <LaunchView
            org={selectedOrg}
            githubConnected={status?.github.connected ?? false}
            statusKnown={status !== null}
            autopilotDefault={autopilotDefault}
            maxParallel={status?.sandbox.max_parallel ?? null}
            sessions={sessions}
            onOpenColony={(session) => openColonyById(session.id)}
            onCreated={(session) => {
              onCreated(session);
              setView("home");
            }}
            onOpenSettings={() => onOpenSettings("setup")}
          />
        );
      case "inbox":
        return (
          <InboxView
            sessions={sessions}
            onOpenColony={openColonyById}
            onOpenNotificationSettings={() => onOpenSettings("notifications")}
          />
        );
      case "history":
        return <HistoryView sessions={inOrg} org={selectedOrg} onOpenColony={openColonyById} />;
      default:
        return (
          <NestView
            sessions={inOrg}
            // The inspector wins while it is open; otherwise the chamber for the colony App has
            // selected stays lit, so coming back from the colony view lands somewhere familiar.
            selectedId={inspector?.kind === "colony" ? inspector.session.id : selectedId}
            mothershipSelected={inspector?.kind === "mothership"}
            redRuns={redRuns}
            settlers={settlers}
            // The single open stream's live detail: the selected chamber's balloon escalates to
            // it while non-empty, every other chamber reading its colony-level feed line.
            liveDetail={state.agentDetail}
            backlogCount={backlogCount}
            avatarFor={avatarFor}
            onSelect={(id) => {
              const session = sessions.find((s) => s.id === id);
              if (session) {
                setInspector({ kind: "colony", session });
                onSelectSession(id);
              }
            }}
            onOpen={openColonyById}
            onSelectMothership={() => setInspector({ kind: "mothership" })}
            onLaunch={() => setView("launch")}
          />
        );
    }
  };

  return (
    <div className="cockpit grid h-full min-h-0 grid-cols-[56px_minmax(0,1fr)] bg-bg text-text">
      <Rail
        orgs={workspaces}
        selectedOrg={selectedOrg}
        onSelectOrg={(org) => {
          onSelectOrg(org);
          setView("home");
          setInspector(null);
        }}
        view={view}
        onNavigate={navigate}
        needCount={needAnywhere}
        needByOrg={needByOrg}
        theme={theme}
        onToggleTheme={toggleTheme}
      />
      <div className="grid min-h-0 min-w-0 grid-rows-[48px_minmax(0,1fr)]">
        <Header
          orgs={workspaces}
          selectedOrg={selectedOrg}
          onSelectOrg={(org) => {
            onSelectOrg(org);
            setView("home");
            setInspector(null);
          }}
          needByOrg={needByOrg}
          crumb={CRUMB[view]}
          liveCount={liveCount}
          needCount={needHere}
          cost={spend != null && spend > 0 ? spend : null}
          update={update}
          onOpenUpdates={() => onOpenSettings("updates")}
        />
        <div className="flex min-h-0 min-w-0">
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            {/* The session-limit banner sits above every view, outside each view's own scroll. */}
            {quotaBanner ? (
              <QuotaBanner
                quota={quotaBanner}
                sessions={sessions}
                onResumeAll={() => void resumeAllQuotaParked()}
                onDismiss={() => setDismissedQuota((dismissed) => dismissQuotaBanner(dismissed, quotaBanner))}
              />
            ) : null}
            {body()}
          </div>
          {view === "home" && (
            <Inspector
              target={inspector}
              avatarUrl={inspector?.kind === "colony" ? avatarFor(orgOf(inspector.session)) : null}
              settlers={inspector?.kind === "colony" ? settlers : []}
              pendingQuestions={pendingQuestions}
              questionActions={questionActions}
              sessions={sessions}
              status={status}
              liveCount={liveCount}
              queuedCount={queuedCount}
              needCount={needHere}
              spend={spend != null && spend > 0 ? spend : null}
              maxParallel={status?.sandbox.max_parallel ?? null}
              update={update}
              onClose={() => setInspector(null)}
              onOpenColony={openColonyById}
              onStop={(id) => void act(id, (x) => api.stopSession(x))}
              onResume={(id) => void act(id, (x) => api.resumeSession(x))}
              onLaunch={() => setView("launch")}
              onOpenSettings={(section) => onOpenSettings(section)}
            />
          )}
        </div>
      </div>
    </div>
  );
}
