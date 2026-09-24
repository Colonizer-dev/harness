// The cockpit: the rail, the header, and whichever view the rail is pointing at.
//
// It owns only what is its own — which view is showing, what the inspector is looking at, and the
// theme override. The colony and memory panes are passed in as slots so App keeps its existing
// wiring for them, and settings stays the dialog App already owns rather than a second copy.
import { CodeView } from "./CodeView";
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../context";
import type { QuestionActions } from "../components/AskUserCard";
import type { SectionId } from "../components/SettingsDialog";
import { isLive, orgOf, sameOrg, store, stored } from "../components/ui";
import { needsYou } from "../notifications";
import { memoryBadge, orgEntries, viewAfterOrgSwitch } from "../orgs";
import { sortSessions } from "../sessionOrder";
import { sessionCost, sumCosts } from "../spend";
import { buildThread, useSessionStream } from "../sessionStream";
import type { FleetHost, HarnessStatus, OrgInfo, RedTeamRun, Repo, Session, StartRedTeamRunRequest, StorageSummary, UpdateStatus } from "../types";
import type { LiveConnection } from "../liveStream";
import { Composer } from "./Composer";
import { Header } from "./Header";
import { HostView } from "./HostView";
import { recordHost } from "./hostHistory";
import { NavRail, type CockpitView } from "./NavRail";
import { HistoryView } from "./HistoryView";
import { LoopsView } from "./LoopsView";
import { SecretsView } from "./SecretsView";
import { InboxView } from "./InboxView";
import { Inspector, pendingQuestionsOf, type InspectorTarget } from "./Inspector";
import { LaunchView } from "./LaunchView";
import { NestView } from "./NestView";
import { OverviewView } from "./OverviewView";
import { QuotaBanner, dismissQuotaBanner, resumeQuotaParkedSessions, visibleQuotaBanner } from "./QuotaBanner";
import { needCountByOrg } from "./feed";
import { providerSnapshots } from "./dash";

const VIEW_KEY = "colonizer.cockpitView";

const THEME_KEY = "colonizer.theme";

const VIEWS: readonly CockpitView[] = ["overview", "home", "colony", "launch", "inbox", "history", "loops", "settings", "memory", "host", "secrets", "code"];

function storedView(): CockpitView {
  const saved = stored(VIEW_KEY);
  return VIEWS.includes(saved as CockpitView) ? (saved as CockpitView) : "home";
}

function storedTheme(): "light" | "dark" | null {
  const saved = stored(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : null;
}

/** The toast for a failed inspector action: what failed, on which colony, and why. */
export function actionError(action: "stop" | "resume", colony: string, error: unknown): string {
  return `Couldn't ${action} ${colony}: ${errorMessage(error)}`;
}

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
  statusError = false,
  fleet,
  liveConnection,
  liveStorage = null,
  update,
  autopilotDefault,
  launchRequests,
  settingsRequests,
  memoryRequests = 0,
  secretsRequest,
  pendingMemory = 0,
  settings,
  onSessionChanged,
  onRedStart,
  onRedStop,
  onCreated,
  onOpenSettings,
  onOpenOrgSettings,
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
  /** The status poll is failing; the header says so, the counts beside it are stale. */
  statusError?: boolean;
  /** Self plus every configured peer (issue #231); older mothership builds send an empty list. */
  fleet?: FleetHost[];
  /** The realtime feed's connection (issue #446); absent renders the indicator as reconnecting. */
  liveConnection?: LiveConnection;
  /** A storage frame the stream pushed; the overview's storage panel shows it (issue #446). */
  liveStorage?: StorageSummary | null;
  update: UpdateStatus | null;
  autopilotDefault: boolean;
  /** Bumped by Setup's launch row, which lives in the settings body App owns. */
  launchRequests: number;
  /** Bumped whenever something outside the cockpit asks for settings, with the section already set. */
  settingsRequests: number;
  /** Bumped whenever something outside the cockpit asks for memory (a colony's "memory" link). */
  memoryRequests?: number;
  /** Bumped by `openSecrets(id)`: open the Secrets view on that row. */
  secretsRequest?: { n: number; id?: string };
  /** Memory proposals waiting for review across every org; the rail's badge narrows it to the chosen org. */
  pendingMemory?: number;
  /** The settings body, given the way back out — the cockpit owns the view, so it owns the exit. */
  settings: (close: () => void) => ReactNode;
  onSessionChanged: (session: Session) => void;
  onRedStart?: (body: StartRedTeamRunRequest) => Promise<void>;
  onRedStop?: (id: string) => Promise<void>;
  onCreated: (session: Session) => void;
  onOpenSettings: (section?: SectionId) => void;
  /** One org's settings dialog, which App owns; how a switched-off org gets switched back on. */
  onOpenOrgSettings?: (org: string) => void;
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

  // The inspector renders only on the home view, and only while something is picked.
  useEffect(() => {
    onInspectorShown?.(view === "home" && inspector !== null);
  }, [view, inspector, onInspectorShown]);

  // An explicit choice is written on the root, where index.css's :root[data-theme] blocks pick it
  // up; clearing it hands the page back to prefers-color-scheme.
  useEffect(() => {
    const root = document.documentElement;
    if (theme) root.setAttribute("data-theme", theme);
    else root.removeAttribute("data-theme");
    store(THEME_KEY, theme);
  }, [theme]);

  // Every host probe becomes a sample for the Host view's trend lines, whichever view is showing.
  useEffect(() => recordHost(status?.host), [status?.host]);

  useEffect(() => {
    if (launchRequests > 0) setView("launch");
  }, [launchRequests]);

  useEffect(() => {
    if (settingsRequests > 0) setView("settings");
  }, [settingsRequests]);

  useEffect(() => {
    if (memoryRequests > 0) setView("memory");
  }, [memoryRequests]);

  useEffect(() => {
    if (secretsRequest && secretsRequest.n > 0) setView("secrets");
  }, [secretsRequest]);

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
  const entries = useMemo(() => orgEntries(orgs, sessions), [orgs, sessions]);
  const workspaces = entries.visible;

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

  // The rail and the header switch org the same way. The view survives when it reads the chosen
  // org (viewAfterOrgSwitch), and so does the inspector unless it is on a colony the new filter hides.
  const switchOrg = useCallback(
    (org: string | null) => {
      onSelectOrg(org);
      setView(viewAfterOrgSwitch);
      setInspector((current) =>
        current?.kind === "colony" && org !== null && !sameOrg(orgOf(current.session), org) ? null : current,
      );
    },
    [onSelectOrg],
  );

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
    async (id: string, action: "stop" | "resume", run: (id: string) => Promise<Session>) => {
      try {
        onSessionChanged(await run(id));
      } catch (error) {
        // The 4s poll confirms a success; a failure needs saying aloud, or the button looks dead.
        const target = sessions.find((s) => s.id === id);
        toast(actionError(action, target ? `${target.repo}#${target.issue}` : id, error), "error");
      }
    },
    [onSessionChanged, sessions, toast],
  );

  // The global quota banner's keyed dismissal: dismissing hides this pause, and a new reset (or a
  // new pause scope) re-shows it — the same pattern as the storage alert App owns.
  const [dismissedQuota, setDismissedQuota] = useState<ReadonlySet<string>>(() => new Set());
  const quotaBanner = visibleQuotaBanner(status?.quota ?? null, dismissedQuota);
  // Resume-all has no bulk endpoint: one resume per parked colony through the existing act path,
  // settled per colony so a single 409 cannot block the rest.
  const resumeAllQuotaParked = useCallback(
    () => resumeQuotaParkedSessions(sessions, (id) => act(id, "resume", (x) => api.resumeSession(x))),
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
            issues={{
              repos,
              org: selectedOrg,
              sessions,
              githubConnected: status?.github.connected ?? false,
              autopilotDefault,
              onCreated,
              onOpenColony: openColonyById,
            }}
            // One control: the sidebar's workspace switcher is the overview's scope, and the page's
            // own "open workspace" / "← All workspaces" move that same scope.
            scopeOrg={selectedOrg}
            onScopeOrg={switchOrg}
            sessions={sessions}
            orgs={workspaces}
            cost={sumCosts(sessions.map(sessionCost))}
            host={status?.host ?? null}
            fleet={fleet}
            runs={redRuns}
            connection={liveConnection}
            liveStorage={liveStorage}
            quota={status?.quota ?? null}
            quotaBannerVisible={quotaBanner !== null}
            // The org dashboard's API-error tile reads the status poll's cumulative provider
            // tallies (mapped in dash.ts); nothing here fetches.
            providers={providerSnapshots(status?.model_providers)}
            onStart={onRedStart}
            onStop={onRedStop}
            onOpenColony={openColonyById}
            onOpenSettings={(section) => onOpenSettings(section)}
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
      case "code":
        return (
          <CodeView
            orgs={orgs}
            repos={repos}
            sessions={sessions}
            selectedOrg={selectedOrg}
            onSelectOrg={onSelectOrg}
            onCreated={onCreated}
            onOpenColony={openColonyById}
          />
        );
      case "loops":
        return <LoopsView org={selectedOrg} repos={repos} sessions={sessions} avatarFor={avatarFor} onOpenColony={openColonyById} />;
      case "secrets":
        return <SecretsView focusId={secretsRequest?.id} focusRequest={secretsRequest?.n} />;
      case "host":
        return (
          <HostView
            status={status}
            fleet={fleet}
            sessions={sessions}
            liveStorage={liveStorage}
            onOpenColony={openColonyById}
            onOpenSettings={(section) => onOpenSettings(section)}
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
            capacity={status?.sandbox.max_parallel ?? null}
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
    <div className="cockpit relative isolate grid h-full min-h-0 grid-cols-[auto_minmax(0,1fr)] bg-bg text-text">
      <NavRail
        orgs={workspaces}
        hiddenOrgs={entries.hidden}
        selectedOrg={selectedOrg}
        onSelectOrg={switchOrg}
        onOpenOrgSettings={(org) => onOpenOrgSettings?.(org)}
        needByOrg={needByOrg}
        view={view}
        onNavigate={navigate}
        inboxCount={needAnywhere}
        liveCount={liveCount}
        pendingMemory={memoryBadge(selectedOrg, workspaces, pendingMemory)}
        theme={theme}
        onToggleTheme={toggleTheme}
        update={update}
        onOpenUpdates={() => onOpenSettings("updates")}
      />
      <div className="relative isolate grid min-h-0 min-w-0 grid-rows-[auto_minmax(0,1fr)]">
      {/* The v3 halo: a faint radial glow behind the top of the page, under the glass bar. */}
      <div aria-hidden="true" className="v3-halo" />
      <Header
        orgs={workspaces}
        selectedOrg={selectedOrg}
        onSelectOrg={switchOrg}
        needByOrg={needByOrg}
        statusError={statusError}
        connection={liveConnection}
        inbox={{
          sessions,
          onOpenColony: openColonyById,
          onOpenInbox: () => navigate("inbox"),
          onOpenNotificationSettings: () => onOpenSettings("notifications"),
        }}
      />
      <div className="relative z-[1] flex min-h-0 min-w-0">
        <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
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
          {/* The composer floats over every overview-style view; the launch form, an open colony and
              settings have their own inputs. */}
          {(view === "overview" || view === "home" || view === "inbox" || view === "history" || view === "memory" || view === "host") && (
            <Composer
              org={selectedOrg}
              repos={repos}
              githubConnected={status?.github.connected ?? false}
              autopilotDefault={autopilotDefault}
              sessions={sessions}
              onCreated={(session) => {
                onCreated(session);
                setView("home");
              }}
            />
          )}
        </div>
        {/* Only while something is picked: closing it (×) gives the nest the full width back, and
            clicking a chamber or the mothership opens it again. */}
        {view === "home" && inspector !== null && (
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
            onStop={(id) => void act(id, "stop", (x) => api.stopSession(x))}
            onResume={(id) => void act(id, "resume", (x) => api.resumeSession(x))}
            onLaunch={() => setView("launch")}
            onOpenSettings={(section) => onOpenSettings(section)}
          />
        )}
      </div>
      </div>
    </div>
  );
}
