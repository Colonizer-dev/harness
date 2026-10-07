// The cockpit: the rail, the header, and whichever view the rail is pointing at.
//
// It owns only what is its own — which view is showing, what the inspector is looking at, and the
// theme override. The colony and memory panes are passed in as slots so App keeps its existing
// wiring for them, and settings stays the dialog App already owns rather than a second copy.
import { isActive } from "../redTeam";
import { CodeView } from "./CodeView";
import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { errorMessage, useApi, useToast } from "../context";
import type { QuestionActions } from "../components/AskUserCard";
import type { SectionId } from "../components/SettingsDialog";
import { isLive, orgOf, sameOrg, store, stored, useMediaQuery } from "../components/ui";
import { COCKPIT_VIEWS, LAUNCH_PARAMS, holdingSession, sharedIssueFromUrl, viewFromUrl, welcomeFromUrl, type SharedIssue } from "../launchUrl";
import { canQueue, droppedText, sendOrQueue, useOutbox } from "../outbox";
import { needsYou } from "../notifications";
import { memoryBadge, orgEntries, viewAfterOrgSwitch } from "../orgs";
import { colonyFromUrl, pushTarget } from "../push";
import { formatRoute, isRootPath, parseRoute, type Route } from "../routes";
import { currentLocation, navigate as pushUrl, routerBase, subscribe } from "../router";
import { sortSessions } from "../sessionOrder";
import { sessionCost, sumCosts } from "../spend";
import { settlersOf, useSessionStream } from "../sessionStream";
import type { AutonomyStatus, FleetHost, HarnessStatus, OrgInfo, RedTeamRun, Repo, Session, StartRedTeamRunRequest, StorageSummary, UpdateStatus } from "../types";
import type { LiveConnection } from "../liveStream";
import { ColonizeProvider } from "./Colonize";
import { Composer } from "./Composer";
import { Header } from "./Header";
import { ModelSwitcher } from "./ModelSwitcher";
import { HostView } from "./HostView";
import { autoCeilingLabel } from "./host";
import { recordHost } from "./hostHistory";
import { NavRail, type CockpitView } from "./NavRail";
import { MobileTabBar } from "./MobileTabBar";
import { HistoryView } from "./HistoryView";
import { LoopsView } from "./LoopsView";
import { focusTurn } from "./turnFocus";
import { UpdateBanner, restartOnNewVersion } from "./UpdateBanner";
import { SecretsView } from "./SecretsView";
import { InboxView } from "./InboxView";
import { runQuotaAction } from "./ProviderQuotaCard";
import { runDecisionAnswer, runPrAction } from "./DecisionCards";
import { decisionsCount, useDecisions } from "./decisions";
import { Inspector, pendingQuestionsOf, type InspectorTarget } from "./Inspector";
import { LaunchView } from "./LaunchView";
import { NestView } from "./NestView";
import { NestDashboard } from "./NestDashboard";
import { OverviewView } from "./OverviewView";
import { Page } from "./Page";
import { PhoneWelcomeSheet } from "../components/PhoneWelcomeSheet";
import { BookmarkPrompt } from "../components/BookmarkPrompt";
import { DEMO } from "../demo";
import { QuotaBanner, dismissQuotaBanner, resumeQuotaParkedSessions, visibleQuotaBanner } from "./QuotaBanner";
import { AccountBanner } from "./AccountBanner";
import { GitHubBanner } from "./GitHubBanner";
import { StewardBanner } from "./StewardBanner";
import { needCountByOrg } from "./feed";
import { backlogBadge } from "./backlog";
import { providerSnapshots } from "./dash";
import { GATEWAY_RETRY_MESSAGE } from "./questions";

// The Chat view (and its highlighter, which it loads later still) stays out of the main bundle.
const ChatView = lazy(() => import("./ChatView").then((m) => ({ default: m.ChatView })));

const VIEW_KEY = "colonizer.cockpitView";

const DASH_KEY = "colonizer.nestDashboard";

/** The dashboard choice: true/false once the visitor picked, null to follow the window width. */
function storedDashOpen(): boolean | null {
  const saved = stored(DASH_KEY);
  return saved === "open" ? true : saved === "closed" ? false : null;
}

const THEME_KEY = "colonizer.theme";

/** Every view the rail can route to; "home" is the Nest. The list itself lives in launchUrl.ts,
 *  next to the `?view=` parsing that must agree with it. */
export { COCKPIT_VIEWS };

function storedView(): CockpitView {
  const saved = stored(VIEW_KEY);
  return COCKPIT_VIEWS.includes(saved as CockpitView) ? (saved as CockpitView) : "home";
}

function storedTheme(): "light" | "dark" | null {
  const saved = stored(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : null;
}

/** The toast for a failed inspector action: what failed, on which colony, and why. */
export function actionError(action: "stop" | "resume" | "retry", colony: string, error: unknown): string {
  return `Couldn't ${action} ${colony}: ${errorMessage(error)}`;
}

export function Cockpit({
  sessions,
  sessionsLoaded,
  orgs,
  redRuns = [],
  selectedOrg,
  onSelectOrg,
  selectedId,
  onSelectSession,
  onOpenColony,
  status,
  statusError = false,
  remoteOn = false,
  fleet,
  liveConnection,
  liveStorage = null,
  update,
  onUpdateChanged,
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
  onRedCancel,
  onRedSynthesize,
  onCreated,
  onOpenSettings,
  onOpenOrgSettings,
  onInspectorShown,
  colony,
  memory,
  notice = null,
}: {
  /** Every colony the mothership knows; the cockpit filters to the chosen workspace itself. */
  sessions: Session[];
  /** Whether the first session poll has landed; launch urls wait for it before acting. */
  sessionsLoaded: boolean;
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
  /** The remote-access switch (issue #535); while on, the header keeps a small badge. */
  remoteOn?: boolean;
  /** Self plus every configured peer (issue #231); older mothership builds send an empty list. */
  fleet?: FleetHost[];
  /** The realtime feed's connection (issue #446); absent renders the indicator as reconnecting. */
  liveConnection?: LiveConnection;
  /** A storage frame the stream pushed; the overview's storage panel shows it (issue #446). */
  liveStorage?: StorageSummary | null;
  update: UpdateStatus | null;
  /** Takes a fresh update status after the banner restarted colonies on the new version (issue #1097). */
  onUpdateChanged?: (update: UpdateStatus) => void;
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
  /** Cancels a red-team run (#1145): every hunter stopped, findings kept. */
  onRedCancel?: (id: string) => Promise<void>;
  /** (Re)launches a done run's synthesis colony (issue #309). */
  onRedSynthesize?: (id: string) => Promise<void>;
  onCreated: (session: Session) => void;
  onOpenSettings: (section?: SectionId) => void;
  /** One org's settings dialog, which App owns; how a switched-off org gets switched back on. */
  onOpenOrgSettings?: (org: string) => void;
  /** Told whether the inspector — or the dashboard that shares its slot — is on screen, so App can
   *  keep its fixed cards clear of the right-hand aside. */
  onInspectorShown?: (shown: boolean) => void;
  /** The open colony's own pane, wired by App (chat, terminal, publish). */
  colony: ReactNode;
  memory: ReactNode;
  /** A phone-only card in the page flow (the live-map prompt): at the top of the Nest and of the
   *  Inbox list, where it pushes the content down instead of covering it. App passes it below `sm` only. */
  notice?: ReactNode;
}) {
  const api = useApi();
  const toast = useToast();
  // The autonomy judge's health (issue #875): a slow poll feeding the header's warning chip.
  // Errors are ignored — an older mothership has no /api/autonomy/status, and the chip stays hidden.
  const [judge, setJudge] = useState<AutonomyStatus | null>(null);
  useEffect(() => {
    let live = true;
    const load = () => void api.autonomyStatus().then((s) => live && setJudge(s)).catch(() => {});
    load();
    const id = window.setInterval(load, 60_000);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [api]);
  // A launch url (`?view=`, issue #745) overrides the persisted view once, at boot.
  // The address names the view (issue #1180): `/nest`, `/settings/models/providers`, `/colonies/<id>`.
  // The bare root is the Overview, but a returning visitor's remembered view still wins there, so a
  // bookmark of `/` keeps opening where it always did.
  const [bootRoute] = useState<Route | null>(() => {
    if (DEMO) return null;
    const { pathname, search } = currentLocation();
    return isRootPath(pathname, routerBase()) ? null : parseRoute(pathname, search, routerBase());
  });
  const [view, setView] = useState<CockpitView>(() => viewFromUrl(window.location.href) ?? (bootRoute?.view === "colony" ? "home" : bootRoute?.view) ?? storedView());
  // A question from the composer's Ask mode, handed to Chat once (a fresh `n` each time).
  const [askPrompt, setAskPrompt] = useState<{ text: string; n: number } | null>(null);
  // A file the Chat view asked the Code page to open.
  const [codeRequest, setCodeRequest] = useState<{ repo: string; path: string; n: number } | null>(null);
  // An issue a share-target launch handed over (`?share_url=…`, issue #745), kept until the colony
  // list can say where it belongs; and the prefill that decision hands to the launch form.
  const [shared, setShared] = useState<SharedIssue | null>(() => sharedIssueFromUrl(window.location.href));
  const [launchPrefill, setLaunchPrefill] = useState<SharedIssue | null>(null);
  // A colony a push deep link asked for (issue #516): `?colony=<id>` from boot, or a
  // `colonizer:open` message from the service worker, opened once the session list has it.
  const [deeplink, setDeeplink] = useState<string | null>(() => colonyFromUrl(window.location.href) ?? bootRoute?.colony ?? null);
  // A phone that just signed in through a scanned code lands on `?welcome=phone` (issue #746),
  // which offers the install-and-notify sheet once.
  const [welcome, setWelcome] = useState<"phone" | null>(() => welcomeFromUrl(window.location.href));
  const [theme, setTheme] = useState<"light" | "dark" | null>(storedTheme);
  const [inspector, setInspector] = useState<InspectorTarget | null>(null);
  const [repos, setRepos] = useState<Repo[]>([]);
  // The nest dashboard: null follows the width (open on wide screens, closed on narrow), an
  // explicit choice from a hide/show click sticks until the next one. It owns the nest's right-hand
  // slot only while nothing is picked and the nest is the view; a colony selection swaps it for the
  // inspector, and closing the inspector brings it back.
  const roomy = useMediaQuery("(min-width: 1280px)");
  const [dashOpen, setDashOpen] = useState<boolean | null>(storedDashOpen);
  const dashShown = view === "home" && inspector === null && (dashOpen ?? roomy);

  useEffect(() => {
    store(VIEW_KEY, view);
  }, [view]);

  useEffect(() => {
    store(DASH_KEY, dashOpen === null ? null : dashOpen ? "open" : "closed");
  }, [dashOpen]);

  // The inspector renders only on the home view, and only while something is picked; the dashboard
  // takes that same slot, and the fixed cards App owns keep clear of whichever aside is showing.
  useEffect(() => {
    onInspectorShown?.((view === "home" && inspector !== null) || dashShown);
  }, [view, inspector, dashShown, onInspectorShown]);

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
  // The decisions inbox (issue #1036) joins the same count: one number for everything that needs you.
  const decisions = useDecisions(api);
  const decisionCount = useMemo(() => decisionsCount(decisions.view, sessions), [decisions.view, sessions]);
  const needAnywhere = useMemo(() => Object.values(needByOrg).reduce((a, b) => a + b, 0) + decisionCount, [needByOrg, decisionCount]);
  const needHere = useMemo(() => inOrg.filter(needsYou).length, [inOrg]);
  const liveCount = inOrg.filter((s) => isLive(s.status)).length;
  const queuedCount = inOrg.filter((s) => s.status === "queued").length;
  const spend = sumCosts(inOrg.map(sessionCost));
  // Counted by the mothership (issues only, Colonizer's own orgs) and refreshed with the status poll.
  const backlog = useMemo(() => backlogBadge(status?.backlog, selectedOrg), [status?.backlog, selectedOrg]);

  const selectColony = useCallback(
    (id: string) => {
      const session = sessions.find((s) => s.id === id);
      if (session) {
        setInspector({ kind: "colony", session });
        onSelectSession(id);
      }
    },
    [sessions, onSelectSession],
  );

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
  // The outbox's view of what the worker is holding, so an answer queued while the socket was
  // down can be reported when the mothership finally refuses it.
  const outbox = useOutbox();
  const queuedAnswers = useRef(new Set<string>());
  const settlers = useMemo(() => settlersOf(state), [state]);
  // The inspector answers the colony's question from this same stream, so the pane clears itself
  // the moment `question_answered` arrives — nothing here is cached from render to render.
  const pendingQuestions = useMemo(() => pendingQuestionsOf(state), [state]);
  const questionActions = useMemo<QuestionActions>(() => {
    const live = inspector?.kind === "colony" ? isLive(inspector.session.status) : false;
    const connected = state.connection === "open";
    // Offline is no longer a wall (issue #746): with the worker's outbox behind us the answer can
    // queue and send on reconnect, so the card stays open — and owns up to the queueing.
    const queueing = !connected && live && canQueue();
    return {
      answer: (questionId, answers, response, questions) => {
        try {
          const outcome = sendOrQueue(stream, { type: "answer", question_id: questionId, answers, response }, questions);
          if (outcome.status === "failed") toast("Not connected to the colony — try again in a moment.", "error");
          if (outcome.status === "queued") {
            queuedAnswers.current.add(outcome.id);
            toast("Answer queued — it sends when you're back online.");
          }
          return outcome.status !== "failed";
        } catch (error) {
          toast(errorMessage(error), "error");
          return false;
        }
      },
      submitting: state.submitting,
      canAnswer: (connected || queueing) && live,
      willQueue: queueing,
      blockedBy: !live ? "ended" : !connected && !queueing ? "disconnected" : null,
    };
  }, [stream, inspector, state.submitting, state.connection, toast]);

  // A queued answer the mothership refused — a fresher answer won (409), or the colony is gone —
  // must not vanish without a word.
  useEffect(() => {
    for (const drop of outbox.dropped) {
      if (queuedAnswers.current.has(drop.id)) {
        queuedAnswers.current.delete(drop.id);
        toast(droppedText("answer", drop.status), "error");
      }
    }
  }, [outbox.dropped, toast]);

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

  // Deep links (issue #516): the service worker tells a running cockpit which colony a tapped
  // notification named. The message is the worker's alone to send, so the origin check is on the
  // event and the shape of the payload, and an url without a colony id is simply ignored.
  useEffect(() => {
    if (!("serviceWorker" in navigator)) return;
    const onMessage = (event: MessageEvent) => {
      if (event.origin !== window.location.origin) return;
      const data = event.data as { type?: string; url?: unknown } | null;
      if (data?.type !== "colonizer:open" || typeof data.url !== "string") return;
      // A colony push opens the colony; the out-of-quota push (issue #767) opens the Inbox card.
      const target = pushTarget(data.url);
      if (target && "colony" in target) setDeeplink(target.colony);
      else if (target) setView(target.view);
    };
    navigator.serviceWorker.addEventListener("message", onMessage);
    return () => navigator.serviceWorker.removeEventListener("message", onMessage);
  }, []);

  // Landing with `?colony=<id>` (or after the message above): wait for the poll that carries the
  // colony, open it through the ordinary path — org filter and all — then strip the param so a
  // reload or a shared url does not pin the cockpit to that colony forever.
  useEffect(() => {
    if (!deeplink) return;
    if (!sessions.some((s) => s.id === deeplink)) return;
    openColonyById(deeplink);
    setPendingOpen(deeplink);
    setDeeplink(null);
    const url = new URL(window.location.href);
    if (url.searchParams.has("colony")) {
      url.searchParams.delete("colony");
      window.history.replaceState(null, "", url);
    }
  }, [deeplink, sessions, openColonyById]);

  // The launch params are read into state at boot, so the address bar can lose them at once — a
  // reload, a re-share or a copied url starts from the persisted view like any other day.
  useEffect(() => {
    const url = new URL(window.location.href);
    const before = url.search;
    for (const name of LAUNCH_PARAMS) url.searchParams.delete(name);
    if (url.search !== before) window.history.replaceState(null, "", url.pathname + url.search + url.hash);
  }, []);

  // A shared issue (issue #745) waits for the first session poll, then goes where it belongs: the
  // colony holding it opens, and otherwise Launch takes it prefilled. A pull request only ever
  // follows a session that recorded its PR url, never an unrelated colony that happens to hold the
  // same number.
  useEffect(() => {
    if (!shared || !sessionsLoaded) return;
    const holder = holdingSession(sessions, shared);
    if (holder) {
      openColonyById(holder.id);
      setPendingOpen(holder.id);
    } else {
      const owner = shared.repo.split("/")[0];
      if (selectedOrg && !sameOrg(owner, selectedOrg)) onSelectOrg(owner);
      setLaunchPrefill(shared);
      setView("launch");
    }
    setShared(null);
  }, [shared, sessionsLoaded, sessions, openColonyById, selectedOrg, onSelectOrg]);

  // The prefill is one-shot: the launch form reads it only when it mounts, so leaving Launch drops
  // it — coming back starts from whatever the visitor last picked, not the shared issue again.
  useEffect(() => {
    if (view !== "launch") setLaunchPrefill(null);
  }, [view]);

  // Opening a colony from a deep link or a share competes with App's keep-a-valid-selection
  // effect, which can run in the same commit still seeing the selection as empty and pick the
  // newest live colony over the one just asked for. So the asked-for id is remembered, and once
  // the selection has settled, re-asserted if it lost — a no-op whenever it stuck.
  const [pendingOpen, setPendingOpen] = useState<string | null>(null);
  useEffect(() => {
    if (!pendingOpen) return;
    if (selectedId !== pendingOpen && sessions.some((s) => s.id === pendingOpen)) onSelectSession(pendingOpen);
    setPendingOpen(null);
  }, [pendingOpen, selectedId, sessions, onSelectSession]);

  // ---- The address bar (issue #1180) ---------------------------------------------------------
  //
  // State to address: whenever the view, the open colony or the workspace changes, the address
  // follows with pushState (replaceState when only the query moved), so back and forward walk the
  // views. The settings page and the colony's tab live in the address itself, written by the panes,
  // so they are read back from it here rather than overwritten.
  useEffect(() => {
    if (DEMO) return;
    // A colony or a shared issue is still being looked up: the address names it, so leave it be.
    if (deeplink || shared) return;
    const here = currentLocation();
    const base = routerBase();
    const current = parseRoute(here.pathname, here.search, base);
    const route: Route = { view, org: selectedOrg ?? undefined };
    if (view === "colony") {
      route.colony = selectedId ?? undefined;
      if (current?.view === "colony" && current.colony === selectedId) route.colonyTab = current.colonyTab;
    }
    if (view === "settings") route.section = current?.view === "settings" ? current.section : null;
    const next = formatRoute(route, here.search, base);
    const now = here.pathname + here.search;
    if (next === now) return;
    // Same path, different query (the workspace chip, a stripped launch param): not a new page.
    pushUrl(next + here.hash, { replace: next.split("?")[0] === now.split("?")[0], silent: true });
  }, [view, selectedId, selectedOrg, deeplink, shared]);

  // Address to state: back, forward, and links the app makes itself (a settings page that opens
  // Secrets) arrive as navigations, and the cockpit follows them. A colony the list does not have
  // yet waits as a deep link; a path the cockpit has no view at is ignored.
  const followAddress = useCallback(() => {
    if (DEMO) return;
    const here = currentLocation();
    const route = parseRoute(here.pathname, here.search, routerBase());
    if (!route) return;
    if (route.view === "colony" && route.colony) setDeeplink(route.colony);
    else setView(route.view);
    if (route.org !== (selectedOrgRef.current ?? undefined)) onSelectOrgRef.current(route.org ?? null);
  }, []);
  const selectedOrgRef = useRef(selectedOrg);
  selectedOrgRef.current = selectedOrg;
  const onSelectOrgRef = useRef(onSelectOrg);
  onSelectOrgRef.current = onSelectOrg;
  useEffect(() => subscribe(followAddress), [followAddress]);

  // The workspace a deep link names, applied once at boot (App reconciles it if it does not exist).
  useEffect(() => {
    if (bootRoute?.org) onSelectOrg(bootRoute.org);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A colony the address names that the mothership does not have (deleted, or another machine's):
  // once the list is in, stop waiting so the address settles on the Nest.
  useEffect(() => {
    if (deeplink && sessionsLoaded && !sessions.some((s) => s.id === deeplink)) {
      setDeeplink(null);
      if (view === "colony") setView("home");
    }
  }, [deeplink, sessionsLoaded, sessions, view]);

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

  // Move to front / back on a queued colony's row (issue #1156): the 4s poll shows the new order.
  const moveQueued = useCallback(
    async (id: string, to: "front" | "back") => {
      try {
        onSessionChanged(await api.moveSession(id, to));
      } catch (error) {
        toast(errorMessage(error), "error");
      }
    },
    [api, onSessionChanged, toast],
  );

  // Retry on a colony stopped on a model gateway error (issue #1093): one backing off an automatic
  // retry is parked, so Retry now resumes it; one held after the retries ran out is still live, so
  // Retry sends its agent on again.
  const retry = useCallback(
    async (id: string) => {
      const target = sessions.find((s) => s.id === id);
      if (target?.status === "parked") return act(id, "resume", (x) => api.resumeSession(x));
      try {
        await api.messageSession(id, GATEWAY_RETRY_MESSAGE);
      } catch (error) {
        toast(actionError("retry", target ? `${target.repo}#${target.issue}` : id, error), "error");
      }
    },
    [act, api, sessions, toast],
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
      // Tools that own their panes edge to edge sit in the full-bleed page; Settings caps its
      // columns at the readable width inside it.
      case "colony":
        return <Page width="full">{colony}</Page>;
      case "memory":
        return <Page width="full">{memory}</Page>;
      case "settings":
        return (
          <Page width="full">
            {settings(() => setView("home"))}
          </Page>
        );
      case "overview":
        return (
          <OverviewView
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
            onCancel={onRedCancel}
            onSynthesize={onRedSynthesize}
            onOpenColony={openColonyById}
            onResume={(id) => act(id, "resume", (x) => api.resumeSession(x))}
            onMove={moveQueued}
            onOpenSettings={(section) => onOpenSettings(section)}
          />
        );
      case "launch":
        return (
          <LaunchView
            // A prefill that lands while Launch is already showing must remount the form, which
            // reads the prefill only on mount.
            key={launchPrefill ? `${launchPrefill.repo}#${launchPrefill.number}` : "blank"}
            org={selectedOrg}
            githubConnected={status?.github.connected ?? false}
            statusKnown={status !== null}
            autopilotDefault={autopilotDefault}
            maxParallel={status?.sandbox.max_parallel ?? null}
            capacityNote={autoCeilingLabel(status?.sandbox)}
            sessions={sessions}
            prefill={launchPrefill}
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
            openRequest={codeRequest}
          />
        );
      case "loops":
        return <LoopsView org={selectedOrg} orgs={orgs} repos={repos} sessions={sessions} avatarFor={avatarFor} onOpenColony={openColonyById} />;
      case "secrets":
        return <SecretsView focusId={secretsRequest?.id} focusRequest={secretsRequest?.n} />;
      case "chat":
        return (
          <Page width="full">
            <Suspense fallback={<div className="flex flex-1 items-center justify-center text-body-sm text-muted">Loading chat…</div>}>
              <ChatView
                org={selectedOrg}
                repos={repos}
                sessions={sessions}
                autopilotDefault={autopilotDefault}
                initialPrompt={askPrompt}
                onPromptTaken={() => setAskPrompt(null)}
                onCreated={(session) => {
                  onCreated(session);
                  setView("home");
                }}
                workspaces={workspaces}
                onOpenFile={(repo, path) => {
                  const owner = repo.split("/")[0];
                  if (!sameOrg(owner, selectedOrg)) onSelectOrg(owner);
                  setCodeRequest((r) => ({ repo, path, n: (r?.n ?? 0) + 1 }));
                  setView("code");
                }}
              />
            </Suspense>
          </Page>
        );
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
            notice={notice}
            sessions={sessions}
            onOpenColony={openColonyById}
            onOpenNotificationSettings={() => onOpenSettings("notifications")}
            quotaCards={status?.quota_cards ?? []}
            onQuotaAction={(provider, body) =>
              runQuotaAction(api.quotaAction, (message, tone) => toast(message, tone), provider, body)
            }
            decisions={decisions.view}
            onAnswerDecision={(body) =>
              runDecisionAnswer(api.answerDecision, (message, tone) => toast(message, tone), body).finally(() => void decisions.refresh())
            }
            onDecisionPrAction={(card, action) =>
              runPrAction(api.decisionPrAction, (message, tone) => toast(message, tone), card, action, (id) => api.publishSession(id)).finally(() => void decisions.refresh())
            }
          />
        );
      case "history":
        return (
          <HistoryView
            sessions={inOrg}
            org={selectedOrg}
            onOpenColony={openColonyById}
            onOpenTurn={(colony, turn) => {
              // A transcript hit (issue #739): open its colony in-app, then ask the chat panel to
              // land on the turn — a no-op when the hit named none.
              openColonyById(colony);
              focusTurn(turn);
            }}
            onOpenSection={(section) => {
              // A row names where its target lives: a cockpit view, or a settings section.
              if (section === "secrets" || section === "loops" || section === "memory") setView(section);
              else if (section === "redteam") setView("overview");
              else onOpenSettings(section as SectionId);
            }}
          />
        );
      default:
        return (
          <NestView
            sessions={inOrg}
            capacity={status?.sandbox.max_parallel ?? null}
            capacityNote={autoCeilingLabel(status?.sandbox)}
            // The inspector wins while it is open; otherwise the chamber for the colony App has
            // selected stays lit, so coming back from the colony view lands somewhere familiar.
            selectedId={inspector?.kind === "colony" ? inspector.session.id : selectedId}
            mothershipSelected={inspector?.kind === "mothership"}
            redRuns={redRuns}
            settlers={settlers}
            // The single open stream's live detail: the selected chamber's balloon escalates to
            // it while non-empty, every other chamber reading its colony-level feed line.
            liveDetail={state.agentDetail}
            backlogCount={backlog.count}
            backlogTitle={backlog.title}
            avatarFor={avatarFor}
            onSelect={selectColony}
            onOpen={openColonyById}
            onSelectMothership={() => setInspector({ kind: "mothership" })}
            onLaunch={() => setView("launch")}
            onOpenLoops={() => setView("loops")}
          />
        );
    }
  };

  return (
    // Colonize — the rail's button, the dashboard's, ⌘K — is one pane, owned here for every view.
    <ColonizeProvider
      repos={repos}
      org={selectedOrg}
      sessions={sessions}
      githubConnected={status?.github.connected ?? false}
      autopilotDefault={autopilotDefault}
      onCreated={onCreated}
      onOpenColony={openColonyById}
      onOpenLaunch={() => setView("launch")}
    >
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
      {/* Pinned to the second track: below `sm` the rail is display:none, and an auto-placed column
          would then size to its content — which a page frame (a size container) does not report. */}
      <div className="relative isolate col-start-2 grid min-h-0 min-w-0 grid-rows-[auto_minmax(0,1fr)]">
      {/* The v3 halo: a faint radial glow behind the top of the page, under the glass bar. */}
      <div aria-hidden="true" className="v3-halo" />
      <Header
        orgs={workspaces}
        selectedOrg={selectedOrg}
        onSelectOrg={switchOrg}
        needByOrg={needByOrg}
        statusError={statusError}
        connection={liveConnection}
        remoteOn={remoteOn}
        judge={judge}
        onOpenRemote={() => onOpenSettings("remote")}
        onOpenCockpit={() => onOpenSettings("cockpit")}
        models={<ModelSwitcher selectedOrg={selectedOrg} judge={judge} />}
        user={{
          login: status?.github.connected ? (status.github.login ?? null) : null,
          name: status?.github.name ?? null,
          avatarUrl: status?.github.avatar_url ?? null,
          onOpenSettings: () => onOpenSettings(),
          onOpenSecrets: () => navigate("secrets"),
        }}
        inbox={{
          sessions,
          onOpenColony: openColonyById,
          onOpenInbox: () => navigate("inbox"),
          onOpenNotificationSettings: () => onOpenSettings("notifications"),
          decisionCount,
        }}
      />
      {/* The mobile tab bar (below `sm`) covers the foot of the screen, so the content it overlays
          is shortened by the same height plus the device's safe-area inset. */}
      <div className="relative z-[1] flex min-h-0 min-w-0 max-sm:pb-[calc(3.75rem+env(safe-area-inset-bottom))]">
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
          {/* Issue #984: while a Claude account needs the owner (a rejected sign-in or an exhausted
              plan), the cockpit banners it above every view, like the quota banner. `Sign in` opens
              the Accounts page — the Connections settings section. Absent on an older mothership. */}
          <AccountBanner alerts={status?.account_alerts} onSignIn={() => onOpenSettings("connections")} />
          {/* Issue #1074: while GitHub refuses the account (suspended, a revoked token, repeated
              secondary limits), one banner above every view names the cause and the next step. */}
          <GitHubBanner pause={status?.github_pause} onReconnect={() => onOpenSettings("connections")} />
          {/* Issue #1172: an org whose GitHub Actions is blocked (billing or a spending limit) is one
              banner, whatever number of its pull requests that fails. */}
          <StewardBanner steward={status?.merge_steward} />
          {/* Issue #1097: a release whose notes flag a critical or fixes-running fix is a banner
              above every view, with how many colonies its probe found affected here; after the
              update, the affected colonies still on the previous version are offered a restart. */}
          <UpdateBanner
            update={update}
            onOpenUpdates={() => onOpenSettings("updates")}
            onRestart={(ids) =>
              void restartOnNewVersion(api, { ids }, (message, tone) => toast(message, tone ?? "info"), onUpdateChanged)
            }
          />
          {/* Issue #880: while a drain holds the queue for an update or a restart, the cockpit says
              so above every view, like the quota banner. It clears itself when the drain finishes,
              so there is nothing to dismiss. Absent on a mothership from before the drain. */}
          {status?.draining ? (
            <div className="px-6 pt-4">
              <div
                role="status"
                className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-warn bg-warn-soft px-3 py-2 text-small-lg text-warn"
              >
                Draining for an update or restart: new colonies stay queued until it finishes.
              </div>
            </div>
          ) : null}
          {/* The Nest is a full-bleed canvas with no scroll root, so its phone notice sits above it
              in the flow, shrinking the canvas rather than covering it. */}
          {notice && view === "home" ? <div className="shrink-0 px-3 pt-3 sm:hidden">{notice}</div> : null}
          <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
            {body()}
            {/* The way back into the dashboard once it has been hidden: a small pill parked above
                the nest's own Nest/Map toggle, in the header's empty top-right corner, so it
                collides with neither that toggle nor the strip below it. */}
            {view === "home" && inspector === null && !dashShown && (
              <button
                type="button"
                onClick={() => setDashOpen(true)}
                className="absolute right-6 top-5 z-[6] cursor-pointer rounded-lg border border-border px-2.5 py-1 text-small-lg text-muted transition-colors hover:text-text"
              >
                Dashboard
              </button>
            )}
          </div>
          {/* The composer floats over every overview-style view; the launch form, an open colony and
              settings have their own inputs. */}
          {(view === "overview" || view === "home" || view === "inbox" || view === "history" || view === "memory" || view === "host") && (
            <Composer
              org={selectedOrg}
              repos={repos}
              githubConnected={status?.github.connected ?? false}
              autopilotDefault={autopilotDefault}
              sessions={sessions}
              shortcutTaken
              onCreated={(session) => {
                onCreated(session);
                setView("home");
              }}
              onAsk={(text) => {
                setAskPrompt({ text, n: Date.now() });
                setView("chat");
              }}
            />
          )}
        </div>
        {/* Only while something is picked: closing it (×) gives the nest the full width back, and
            clicking a chamber or the mothership opens it again. With nothing picked, the workspace
            dashboard takes the same slot — it reads the same scoped list the nest draws. */}
        {view === "home" && inspector !== null ? (
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
            hunterRun={inspector?.kind === "colony" ? (redRuns.find((r) => isActive(r) && r.hunters.some((h) => h.session_id === inspector.session.id)) ?? null) : null}
            onCancelRun={onRedCancel}
            onResume={(id) => void act(id, "resume", (x) => api.resumeSession(x))}
            onRetry={(id) => void retry(id)}
            onLaunch={() => setView("launch")}
            onOpenSettings={(section) => onOpenSettings(section)}
          />
        ) : (
          dashShown && (
            <NestDashboard
              org={selectedOrg}
              avatar={selectedOrg ? avatarFor(selectedOrg) : null}
              sessions={inOrg}
              maxParallel={status?.sandbox.max_parallel ?? null}
              onSelect={selectColony}
              onHide={() => setDashOpen(false)}
            />
          )
        )}
      </div>
      </div>
      <MobileTabBar view={view} onNavigate={navigate} inboxCount={needAnywhere} />
      {/* The phone sign-in welcome (issue #746): offered once, gone on dismiss or on reload —
          the ?welcome= that opened it is stripped at boot. */}
      {welcome && <PhoneWelcomeSheet onClose={() => setWelcome(null)} />}
      {/* The bookmark prompt (issue #867): once per device in the signed-in cockpit, and never
          again once dismissed or installed. Held back while the phone welcome sheet is up so the two
          do not stack, and left out of the hosted demo, which has no real cockpit to bookmark.
          Self-gating, so it renders null when it has nothing to say. */}
      {!welcome && !DEMO && <BookmarkPrompt />}
    </div>
    </ColonizeProvider>
  );
}
