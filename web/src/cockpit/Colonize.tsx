// Colonize: the one way to send colonies out. The sidebar's orange button, the dashboard's, and ⌘K
// all open the same slide-over, which does two things:
//
// - It lists the scope's open issues (GET /api/repos/{o}/{r}/issues, already narrowed by the Source
//   module's label filter), searched, label-filtered and paged like every other list (paging.ts), to
//   pick and dispatch: one colony per issue, one launch at a time.
// - Above the list, a free-form box, typed or spoken. The text is drafted into one issue, or a few when
//   it clearly holds independent tasks (POST /api/colonize/draft: the cheap summary model that titles
//   a chat), shown for a confirm or an edit, filed on GitHub (POST /api/repos/{o}/{r}/issues, the same
//   `gh issue create` a chat's "file an issue" runs), dropped into the list pre-selected, and, unless
//   "dispatch right after creating" is off, dispatched at once. `/loop 1h <task>` makes a loop instead,
//   and "Launch without an issue" starts an open colony on the text, both as the composer does.
//
// The badge on the buttons is GitHub's own open-issue count until the pane has loaded the filtered one.
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";

import { ApiError, heldByFor, type Api } from "../api";
import { errorMessage, useApi, useToast } from "../context";
import { Spinner, Switch, cx, sameOrg } from "../components/ui";
import type { CreatedIssue, Issue, IssueDraft, Repo, Session } from "../types";
import { AntGlyph } from "./chat/PersonaAnt";
import { MicButton, appendHeard, useVoiceInput } from "./Composer";
import { relative } from "./InboxView";
import { Pagination, SearchBox } from "./ListControls";
import { describeLoopCadence, nameFromPrompt, parseLoopCommand } from "./loops";
import { usePagedFilter } from "./paging";
import { taskLine } from "../summary";

/** Repositories fetched when the pane shows "all repos in scope": the most recently pushed first. */
export const ALL_REPOS_LIMIT = 30;
/** Issue lists fetched at once, so a wide scope does not burst the mothership's `gh`. */
const FETCH_CONCURRENCY = 4;
/** The `origin` a hand-off from this pane carries, which the activity log records as `colonize.colony`. */
export const COLONIZE_ORIGIN = "colonize";

export interface IssuesActions {
  repos: Repo[];
  /** The chosen workspace, or null for every workspace. */
  org: string | null;
  sessions: Session[];
  githubConnected: boolean;
  autopilotDefault: boolean;
  /** A colony the hand-off created, so the list shows it at once. */
  onCreated: (session: Session) => void;
  onOpenColony: (id: string) => void;
}

/** One issue in the pane, keyed across repositories. */
export interface ScopedIssue extends Issue {
  repo: string;
}

export type HandoffResult =
  | { state: "pending" }
  | { state: "started" | "queued"; sessionId: string }
  | { state: "held"; message: string }
  | { state: "error"; message: string };

export const issueKey = (repo: string, number: number): string => `${repo}#${number}`;

/** The repositories the button counts and the pane offers: the scope's, with issues, newest push first. */
export function scopeRepos(repos: readonly Repo[], org: string | null): Repo[] {
  return repos
    .filter((r) => !r.archived && r.has_issues !== false && (org === null || sameOrg(r.full_name.split("/")[0], org)))
    .sort((a, b) => (b.pushed_at ?? "").localeCompare(a.pushed_at ?? "") || a.full_name.localeCompare(b.full_name));
}

/** GitHub's open-issue count for the scope; it counts open pull requests too, so it is an upper bound. */
export function roughIssueCount(repos: readonly Repo[]): number {
  return repos.reduce((total, r) => total + r.open_issues_count, 0);
}

/** Whether one issue matches the search text (title or #number, already trimmed and lower-cased) and carries every chosen label. */
export function issueMatches(issue: ScopedIssue, needle: string, labels: Iterable<string>): boolean {
  const q = needle.replace(/^#/, "");
  return (q === "" || issue.title.toLowerCase().includes(q) || String(issue.number).startsWith(q)) && [...labels].every((l) => issue.labels.some((x) => x.name === l));
}

/** Issues matching the search text (title or #number) and carrying every chosen label. */
export function filterIssues(issues: readonly ScopedIssue[], search: string, labels: ReadonlySet<string>): ScopedIssue[] {
  const q = search.trim().toLowerCase();
  return issues.filter((i) => issueMatches(i, q, labels));
}

/** Every label on the listed issues with how many carry it, most used first. */
export function labelCounts(issues: readonly ScopedIssue[]): [string, number][] {
  const counts = new Map<string, number>();
  for (const i of issues) for (const l of i.labels) counts.set(l.name, (counts.get(l.name) ?? 0) + 1);
  return [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

/** The shown issues that can still be handed off: not already held by a live colony. */
export function selectable(issues: readonly ScopedIssue[], sessions: Session[]): ScopedIssue[] {
  return issues.filter((i) => heldByFor(sessions, i.repo, i.number) === null);
}

/** Select all toggles between every selectable shown issue and none of them, keeping hidden picks. */
export function toggleAll(selected: ReadonlySet<string>, shown: readonly ScopedIssue[]): Set<string> {
  const keys = shown.map((i) => issueKey(i.repo, i.number));
  const next = new Set(selected);
  const all = keys.length > 0 && keys.every((k) => next.has(k));
  for (const k of keys) {
    if (all) next.delete(k);
    else next.add(k);
  }
  return next;
}

/** One line per outcome kind, for the toast after a hand-off. */
export function summarize(results: Record<string, HandoffResult>): string {
  const n = { started: 0, queued: 0, held: 0, error: 0 };
  for (const r of Object.values(results)) if (r.state !== "pending") n[r.state] += 1;
  return [
    n.started > 0 && `${n.started} started`,
    n.queued > 0 && `${n.queued} queued`,
    n.held > 0 && `${n.held} already held`,
    n.error > 0 && `${n.error} failed`,
  ]
    .filter(Boolean)
    .join(" · ");
}

// --- From text to issues ------------------------------------------------------------------------

/** The repository new issues go to: the pane's chosen one, or the scope's only one. Null means ask. */
export function draftRepo(scope: readonly Repo[], chosen: string): string | null {
  if (chosen !== "*") return chosen;
  return scope.length === 1 ? scope[0].full_name : null;
}

/** A filed issue as a row of the list, so it shows (and can be dispatched) before GitHub's list catches up. */
export function asScopedIssue(made: CreatedIssue, body: string, now: Date = new Date()): ScopedIssue | null {
  if (made.number == null) return null;
  return { repo: made.repo, number: made.number, title: made.title, body, labels: [], author: null, updatedAt: now.toISOString(), url: made.url };
}

/** The fetched issues with the ones this pane just filed, newest first and never twice. */
export function mergeIssues(fetched: readonly ScopedIssue[], made: readonly ScopedIssue[]): ScopedIssue[] {
  const seen = new Set(fetched.map((i) => issueKey(i.repo, i.number)));
  return [...made.filter((i) => !seen.has(issueKey(i.repo, i.number))), ...fetched].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}

/** The selection after a batch is filed: exactly the new issues, so Dispatch sends those and nothing picked before. */
export function preselect(created: readonly ScopedIssue[]): Set<string> {
  return new Set(created.map((i) => issueKey(i.repo, i.number)));
}

export interface FiledDrafts {
  created: ScopedIssue[];
  /** Filed, but GitHub's answer named no number, so it cannot be dispatched from here. */
  unnumbered: CreatedIssue[];
  failed: { title: string; message: string }[];
}

/** Files each draft on `repo`, one at a time, and says what was made and what failed. */
export async function fileDrafts(api: Pick<Api, "createIssue">, repo: string, drafts: readonly IssueDraft[], now: Date = new Date()): Promise<FiledDrafts> {
  const out: FiledDrafts = { created: [], unnumbered: [], failed: [] };
  for (const draft of drafts) {
    try {
      const made = await api.createIssue(repo, { title: draft.title.trim(), body: draft.body.trim() });
      const row = asScopedIssue(made, draft.body.trim(), now);
      if (row) out.created.push(row);
      else out.unnumbered.push(made);
    } catch (e) {
      out.failed.push({ title: draft.title, message: errorMessage(e) });
    }
  }
  return out;
}

/**
 * Starts one colony per issue, one at a time — the queue decides what starts, and a burst would only
 * race the 409 guard. `onResult` hears each outcome as it lands; the whole record is answered too.
 */
export async function dispatchIssues(
  api: Pick<Api, "createSession">,
  issues: readonly ScopedIssue[],
  opts: { instructions?: string; autopilot: boolean },
  onResult: (key: string, result: HandoffResult, session: Session | null) => void = () => {},
): Promise<Record<string, HandoffResult>> {
  const out: Record<string, HandoffResult> = {};
  for (const issue of issues) {
    const key = issueKey(issue.repo, issue.number);
    let result: HandoffResult;
    let session: Session | null = null;
    try {
      session = await api.createSession({
        repo: issue.repo,
        issue: issue.number,
        title: issue.title,
        instructions: opts.instructions?.trim() || undefined,
        autopilot: opts.autopilot,
        origin: COLONIZE_ORIGIN,
      });
      result = { state: session.status === "queued" ? "queued" : "started", sessionId: session.id };
    } catch (e) {
      result = e instanceof ApiError && e.status === 409 ? { state: "held", message: errorMessage(e) } : { state: "error", message: errorMessage(e) };
    }
    out[key] = result;
    onResult(key, result, session);
  }
  return out;
}

/** One draft on the confirm step: editable, and dropped from the batch when unticked. */
export interface DraftRow extends IssueDraft {
  id: number;
  keep: boolean;
}

export type DraftStep =
  | { step: "write" }
  | { step: "drafting" }
  | { step: "confirm"; drafts: DraftRow[]; repo: string | null; note: string | null }
  | { step: "creating"; drafts: DraftRow[]; repo: string; note: string | null };

export interface DraftState {
  text: string;
  stage: DraftStep;
  error: string | null;
}

export type DraftAction =
  | { type: "text"; text: string }
  | { type: "drafting" }
  | { type: "drafted"; drafts: IssueDraft[]; repo: string | null; note: string | null }
  | { type: "failed"; message: string }
  | { type: "edit"; id: number; patch: Partial<Omit<DraftRow, "id">> }
  | { type: "repo"; repo: string | null }
  | { type: "back" }
  | { type: "creating" }
  | { type: "created"; failed: { title: string; message: string }[] }
  | { type: "sent" };

export const DRAFT_START: DraftState = { text: "", stage: { step: "write" }, error: null };

/** The kept drafts with a title: what "Create" files. */
export function keptDrafts(drafts: readonly DraftRow[]): DraftRow[] {
  return drafts.filter((d) => d.keep && d.title.trim() !== "");
}

/**
 * The box's steps: write → drafting → confirm (edit, untick, pick the repository) → creating → back to
 * write with the text cleared. A failure keeps the text (and, when filing, the drafts) and says why.
 * Pure, for the tests.
 */
export function draftReducer(state: DraftState, action: DraftAction): DraftState {
  const { stage } = state;
  switch (action.type) {
    case "text":
      return { ...state, text: action.text, error: null };
    case "drafting":
      return state.text.trim() ? { ...state, stage: { step: "drafting" }, error: null } : state;
    case "drafted":
      if (stage.step !== "drafting") return state;
      return {
        ...state,
        stage: { step: "confirm", drafts: action.drafts.map((d, id) => ({ ...d, id, keep: true })), repo: action.repo, note: action.note },
      };
    case "failed":
      return { ...state, stage: stage.step === "creating" ? { ...stage, step: "confirm" } : { step: "write" }, error: action.message };
    case "edit":
      if (stage.step !== "confirm") return state;
      return { ...state, stage: { ...stage, drafts: stage.drafts.map((d) => (d.id === action.id ? { ...d, ...action.patch } : d)) } };
    case "repo":
      return stage.step === "confirm" ? { ...state, stage: { ...stage, repo: action.repo }, error: null } : state;
    case "back":
      return stage.step === "confirm" ? { ...state, stage: { step: "write" }, error: null } : state;
    case "creating":
      if (stage.step !== "confirm" || stage.repo === null || keptDrafts(stage.drafts).length === 0) return state;
      return { ...state, stage: { ...stage, step: "creating", repo: stage.repo }, error: null };
    case "created": {
      if (stage.step !== "creating") return state;
      if (action.failed.length === 0) return DRAFT_START;
      // Keep only what failed on the confirm step, so a retry does not file the others twice.
      const failed = new Set(action.failed.map((f) => f.title));
      return {
        ...state,
        stage: { ...stage, step: "confirm", drafts: stage.drafts.map((d) => ({ ...d, keep: d.keep && failed.has(d.title) })) },
        error: action.failed.map((f) => `${f.title}: ${f.message}`).join("\n"),
      };
    }
    case "sent":
      return DRAFT_START;
  }
}

// --- The buttons and the pane's owner -----------------------------------------------------------

interface ColonizeHandle {
  open: () => void;
  isOpen: boolean;
  count: number;
  exact: boolean;
  disabled: boolean;
}

const ColonizeContext = createContext<ColonizeHandle | null>(null);

/** The Colonize pane's handle, or null outside a ColonizeProvider. */
export function useColonize(): ColonizeHandle | null {
  return useContext(ColonizeContext);
}

/**
 * Owns the pane: whether it is open, the filtered count its last load found, and ⌘K / Ctrl+K, which
 * opens it from anywhere in the cockpit. Every Colonize button reads it through `useColonize`.
 */
export function ColonizeProvider({ children, onOpenLaunch, ...actions }: IssuesActions & { children: ReactNode; onOpenLaunch?: () => void }): ReactElement {
  const [open, setOpen] = useState(false);
  // The filtered count the pane last loaded for this scope, which beats GitHub's rough one.
  const [loaded, setLoaded] = useState<{ scope: string; count: number } | null>(null);
  const opener = useRef<Element | null>(null);
  const scope = useMemo(() => scopeRepos(actions.repos, actions.org), [actions.repos, actions.org]);
  const scopeKey = actions.org ?? "*";
  const exact = loaded?.scope === scopeKey;
  const count = exact ? loaded.count : roughIssueCount(scope);

  const show = useCallback(() => {
    opener.current = typeof document === "undefined" ? null : document.activeElement;
    setOpen(true);
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!isColonizeShortcut(event)) return;
      event.preventDefault();
      show();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [show]);

  const handle = useMemo<ColonizeHandle>(() => ({ open: show, isOpen: open, count, exact, disabled: !actions.githubConnected }), [show, open, count, exact, actions.githubConnected]);

  return (
    <ColonizeContext.Provider value={handle}>
      {children}
      {open && (
        <ColonizePane
          {...actions}
          scope={scope}
          onOpenLaunch={
            onOpenLaunch &&
            (() => {
              setOpen(false);
              onOpenLaunch();
            })
          }
          onLoadedCount={(n) => setLoaded({ scope: scopeKey, count: n })}
          onClose={() => {
            setOpen(false);
            if (opener.current instanceof HTMLElement) opener.current.focus();
          }}
        />
      )}
    </ColonizeContext.Provider>
  );
}

/**
 * Whether a key press is ⌘K / Ctrl+K meant for Colonize: not one the code editor or a terminal owns
 * (Monaco's ⌘K chords, a shell's Ctrl+K), nor one something else already handled.
 */
export function isColonizeShortcut(event: Pick<KeyboardEvent, "metaKey" | "ctrlKey" | "altKey" | "key" | "defaultPrevented" | "target">): boolean {
  if (!(event.metaKey || event.ctrlKey) || event.altKey || event.key.toLowerCase() !== "k" || event.defaultPrevented) return false;
  const target = event.target as { closest?: (selector: string) => unknown } | null;
  return !target?.closest?.(".monaco-editor, .xterm");
}

/** What the badge says, for the button's label and tooltip. */
export function countLabel(count: number, exact: boolean): string {
  return exact
    ? `${count} open issues to dispatch (filtered by the Source labels)`
    : `about ${count} open issues (GitHub's count, which includes pull requests; the pane loads the filtered list)`;
}

/**
 * The dashboard's Colonize button: the ant, the word, and how many open issues the scope has. Renders
 * nothing outside a ColonizeProvider.
 */
export function ColonizeButton(): ReactElement | null {
  const colonize = useColonize();
  if (!colonize) return null;
  const { count, exact, isOpen, disabled } = colonize;
  const label = `Colonize · ${countLabel(count, exact)}`;
  return (
    <button
      type="button"
      title={`${label} · ⌘K`}
      aria-label={label}
      aria-haspopup="dialog"
      aria-expanded={isOpen}
      aria-keyshortcuts="Meta+K Control+K"
      disabled={disabled}
      onClick={colonize.open}
      className={cx(
        // The dashboard's primary action: solid accent, the one orange button on the page.
        "ant-glyph-host inline-flex h-9 shrink-0 cursor-pointer items-center gap-2 rounded-lg border-0 bg-accent px-3.5 text-[13px] font-semibold text-on-accent shadow-[0_1px_0_rgb(0_0_0/0.15)] transition-[filter,transform] hover:brightness-110 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50",
        isOpen && "brightness-110",
      )}
    >
      <AntGlyph size={22} className="-mx-1" />
      <span>Colonize</span>
      <span className="rounded-full bg-black/20 px-2 py-px text-[12px] tabular-nums">{count > 999 ? "999+" : count}</span>
    </button>
  );
}

// --- The pane ------------------------------------------------------------------------------------

type ListState = ScopedIssue[] | "loading" | { error: string };

export function ColonizePane({
  scope,
  sessions,
  githubConnected,
  autopilotDefault,
  onCreated,
  onOpenColony,
  onOpenLaunch,
  onLoadedCount,
  onClose,
  preloaded,
  initialDraft,
}: IssuesActions & {
  scope: Repo[];
  onOpenLaunch?: () => void;
  onLoadedCount: (count: number) => void;
  onClose: () => void;
  /** Issue lists already in hand, by repository; for the tests, which render without effects. */
  preloaded?: Record<string, ScopedIssue[]>;
  /** Where the text box starts; for the tests. */
  initialDraft?: DraftState;
}): ReactElement {
  const api = useApi();
  const toast = useToast();
  const panel = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const [repo, setRepo] = useState<string>("*");
  const [repoQuery, setRepoQuery] = useState("");
  const [lists, setLists] = useState<Record<string, ListState>>(preloaded ?? {});
  // Issues this pane filed, shown at once whatever GitHub's list (or the Source label filter) says.
  const [made, setMade] = useState<ScopedIssue[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [instructions, setInstructions] = useState("");
  const [autopilot, setAutopilot] = useState(autopilotDefault);
  const [results, setResults] = useState<Record<string, HandoffResult>>({});
  const [running, setRunning] = useState(false);
  const [draft, send] = useReducer(draftReducer, initialDraft ?? DRAFT_START);
  const [dispatchAfter, setDispatchAfter] = useState(true);
  const [busy, setBusy] = useState(false);

  const wanted = useMemo(
    () => (repo === "*" ? scope.filter((r) => r.open_issues_count > 0).slice(0, ALL_REPOS_LIMIT) : scope.filter((r) => r.full_name === repo)).map((r) => r.full_name),
    [repo, scope],
  );

  // Fetch each wanted repository's filtered issues once, a few at a time.
  useEffect(() => {
    const missing = wanted.filter((name) => lists[name] === undefined);
    if (missing.length === 0) return;
    let cancelled = false;
    setLists((l) => ({ ...l, ...Object.fromEntries(missing.map((name) => [name, "loading" as const])) }));
    const queue = [...missing];
    const worker = async () => {
      for (let name = queue.shift(); name !== undefined; name = queue.shift()) {
        const repoName = name;
        try {
          const issues = await api.issues(repoName);
          if (!cancelled) setLists((l) => ({ ...l, [repoName]: issues.map((i) => ({ ...i, repo: repoName })) }));
        } catch (e) {
          if (!cancelled) setLists((l) => ({ ...l, [repoName]: { error: errorMessage(e) } }));
        }
      }
    };
    void Promise.all(Array.from({ length: Math.min(FETCH_CONCURRENCY, missing.length) }, worker));
    return () => {
      cancelled = true;
    };
    // `lists` is read for what is missing, not reacted to.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, wanted]);

  const loadingCount = wanted.filter((n) => lists[n] === "loading" || lists[n] === undefined).length;
  const errors = wanted.flatMap((n) => {
    const l = lists[n];
    return l && typeof l === "object" && "error" in l ? [`${n}: ${l.error}`] : [];
  });
  const fetched = useMemo(() => wanted.flatMap((n) => (Array.isArray(lists[n]) ? (lists[n] as ScopedIssue[]) : [])), [wanted, lists]);
  const issues = useMemo(() => mergeIssues(fetched, repo === "*" ? made : made.filter((i) => i.repo === repo)), [fetched, made, repo]);

  // Once every repository in "all" has answered, the filtered total replaces the badge's rough count.
  useEffect(() => {
    if (repo === "*" && loadingCount === 0 && errors.length === 0 && wanted.length > 0) onLoadedCount(fetched.length);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, loadingCount, errors.length, fetched.length]);

  const list = usePagedFilter(issues, { filters: { labels: [] as string[] }, match: matchIssue });
  const labelSet = new Set(list.filters.labels);
  const pickable = selectable(list.matched, sessions);
  const chosen = issues.filter((i) => selected.has(issueKey(i.repo, i.number)) && heldByFor(sessions, i.repo, i.number) === null);
  const allOn = pickable.length > 0 && pickable.every((i) => selected.has(issueKey(i.repo, i.number)));
  const target = draftRepo(scope, repo);

  // Focus the text box on open; Escape closes the pane (or leaves the confirm step first).
  useEffect(() => {
    (field.current ?? panel.current)?.focus();
  }, []);
  const stepRef = useRef(draft.stage.step);
  stepRef.current = draft.stage.step;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      if (stepRef.current === "confirm") send({ type: "back" });
      else onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // What was heard lands after what is typed; the ref keeps a late transcript from using stale text.
  const draftTextRef = useRef(draft.text);
  draftTextRef.current = draft.text;
  const voice = useVoiceInput({ active: true, onHeard: (phrase) => send({ type: "text", text: appendHeard(draftTextRef.current, phrase) }) });
  const shown = voice.interim ? appendHeard(draft.text, voice.interim) : draft.text;

  // The text box grows with what is in it, up to a point.
  useEffect(() => {
    const el = field.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 180)}px`;
  }, [shown]);

  const dispatch = async (batch: readonly ScopedIssue[]) => {
    if (batch.length === 0 || running) return;
    setRunning(true);
    const next: Record<string, HandoffResult> = Object.fromEntries(batch.map((i) => [issueKey(i.repo, i.number), { state: "pending" } as HandoffResult]));
    setResults((r) => ({ ...r, ...next }));
    const done = await dispatchIssues(api, batch, { instructions, autopilot }, (key, result, session) => {
      if (session) onCreated(session);
      setResults((r) => ({ ...r, [key]: result }));
    });
    setRunning(false);
    setSelected((s) => {
      const left = new Set(s);
      for (const key of Object.keys(done)) left.delete(key);
      return left;
    });
    const failed = Object.values(done).some((r) => r.state === "error");
    toast({ title: `Dispatched: ${summarize(done)}`, kind: failed ? "error" : "success" });
  };

  /** Enter in the box: a loop for `/loop …`, otherwise drafts for the confirm step. */
  const submit = async () => {
    const text = shown.trim();
    if (!text || busy || draft.stage.step !== "write") return;
    if (voice.listening) voice.stop();
    const loop = parseLoopCommand(text);
    if (loop) {
      if (loop.error) return send({ type: "failed", message: loop.error });
      if (!target) return send({ type: "failed", message: "Pick a repository above for the loop." });
      setBusy(true);
      try {
        const created = await api.createLoop({
          name: nameFromPrompt(loop.prompt),
          repo: target,
          prompt: loop.prompt,
          cadence: loop.cadence,
          tz_offset_minutes: -new Date().getTimezoneOffset(),
          autopilot,
        });
        toast({ title: `Loop on ${target}`, body: `"${created.name}" runs ${describeLoopCadence(created.cadence)}. Manage it under Loops.`, kind: "success" });
        send({ type: "sent" });
      } catch (e) {
        send({ type: "failed", message: errorMessage(e) });
      } finally {
        setBusy(false);
      }
      return;
    }
    send({ type: "text", text });
    send({ type: "drafting" });
    try {
      const answer = await api.draftIssues(target ? { text, repo: target } : { text });
      send({ type: "drafted", drafts: answer.issues, repo: target, note: answer.model ? `Drafted by ${answer.model}` : (answer.note ?? null) });
    } catch (e) {
      send({ type: "failed", message: errorMessage(e) });
    }
  };

  /** "Launch without an issue": the text as an open colony's instructions, as the composer launches. */
  const launchOpen = async () => {
    const text = shown.trim();
    if (!text || busy) return;
    if (!target) return send({ type: "failed", message: "Pick a repository above to launch on." });
    if (voice.listening) voice.stop();
    setBusy(true);
    try {
      const session = await api.createSession({ repo: target, instructions: text, autopilot, origin: COLONIZE_ORIGIN });
      toast(session.status === "queued" ? `Queued on ${target} — it starts when a colony finishes` : `Colony launched on ${target}`);
      onCreated(session);
      send({ type: "sent" });
    } catch (e) {
      send({ type: "failed", message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  /** Files the kept drafts, puts them in the list pre-selected, and dispatches them unless told not to. */
  const create = async () => {
    const stage = draft.stage;
    if (stage.step !== "confirm" || stage.repo === null) return;
    const kept = keptDrafts(stage.drafts);
    if (kept.length === 0) return;
    const on = stage.repo;
    send({ type: "creating" });
    const filed = await fileDrafts(api, on, kept);
    send({ type: "created", failed: filed.failed });
    if (filed.created.length > 0) {
      setMade((m) => [...filed.created, ...m]);
      setSelected(preselect(filed.created));
      list.reset();
      const numbers = filed.created.map((i) => `#${i.number}`).join(", ");
      toast({ title: `Created ${numbers} on ${on}`, body: dispatchAfter ? "Dispatching colonies now." : "Selected below: press Dispatch when ready.", kind: "success" });
      if (dispatchAfter) await dispatch(filed.created);
    }
    for (const u of filed.unnumbered) toast({ title: `Created ${u.title}`, body: u.url, kind: "success" });
  };

  const repoChoices = repoQuery.trim() ? scope.filter((r) => r.full_name.toLowerCase().includes(repoQuery.trim().toLowerCase())) : scope;
  const stage = draft.stage;

  const body = (
    <>
      <div aria-hidden="true" className="fixed inset-0 z-[60] bg-black/30" onClick={onClose} />
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label="colonize"
        tabIndex={-1}
        className="fixed inset-y-0 right-0 z-[61] flex w-full max-w-[520px] animate-[ck-in_160ms_ease-out_both] flex-col border-l border-border-strong bg-panel text-text shadow-[-16px_0_48px_rgb(0_0_0/0.35)] outline-none"
      >
        <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-3">
          <span className="grid size-8 place-items-center rounded-lg bg-accent text-on-accent">
            <AntGlyph size={24} />
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-[15px] font-semibold">Colonize</h2>
            <p className="text-[12px] text-muted">Describe new work, or pick open issues. One colony per issue.</p>
          </div>
          <button type="button" onClick={onClose} aria-label="close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="scroll-thin flex min-h-0 flex-1 flex-col overflow-y-auto">
          {!githubConnected && <p className="m-0 border-b border-border px-4 py-3 text-[13px] text-warn">Connect GitHub in Settings to list, create and dispatch issues.</p>}

          <div className="shrink-0 space-y-2 border-b border-border px-4 py-3">
            <label className="block text-[11.5px] font-medium uppercase tracking-wide text-faint" htmlFor="colonize-repo">
              Repository
            </label>
            <div className="flex gap-2">
              <input
                value={repoQuery}
                onChange={(e) => setRepoQuery(e.target.value)}
                placeholder="Find a repository…"
                aria-label="find a repository"
                className="min-w-0 flex-1 rounded-md border border-border bg-panel-2 px-2.5 py-1.5 text-[13px] outline-none focus:border-border-strong"
              />
              <select
                id="colonize-repo"
                value={repo}
                onChange={(e) => setRepo(e.target.value)}
                className="min-w-0 max-w-[240px] flex-1 rounded-md border border-border bg-panel-2 px-2 py-1.5 text-[13px] outline-none focus:border-border-strong"
              >
                <option value="*">All repos in scope ({Math.min(ALL_REPOS_LIMIT, scope.filter((r) => r.open_issues_count > 0).length)})</option>
                {repoChoices.map((r) => (
                  <option key={r.full_name} value={r.full_name}>
                    {r.full_name} · {r.open_issues_count}
                  </option>
                ))}
              </select>
            </div>
          </div>

          <section aria-label="describe new work" className="shrink-0 border-b border-border px-4 py-3">
            {stage.step === "write" || stage.step === "drafting" ? (
              <div className="rounded-xl border border-border-strong bg-panel-2 p-2 focus-within:border-accent">
                <textarea
                  ref={field}
                  rows={2}
                  value={shown}
                  disabled={!githubConnected || stage.step === "drafting"}
                  onChange={(e) => send({ type: "text", text: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                      e.preventDefault();
                      void submit();
                    }
                  }}
                  placeholder={voice.listening ? (voice.recording ? "Recording — press the mic again to transcribe" : "Listening…") : "Describe what you want done. It becomes issues, then colonies…"}
                  aria-label="describe new work"
                  className="bare-field block min-h-[44px] w-full resize-none border-0 bg-transparent px-1.5 py-1 text-[14px] leading-[1.55] text-text outline-none placeholder:text-faint focus-visible:outline-none"
                />
                {voice.transcribing && (
                  <p role="status" className="m-0 flex items-center gap-2 px-1.5 text-[12px] text-muted">
                    <Spinner className="size-3" /> Transcribing with {voice.label}…
                  </p>
                )}
                {voice.left && <p className="m-0 px-1.5 text-[12px] text-warn">{voice.left} left</p>}
                {voice.error && <p className="m-0 px-1.5 text-[12px] text-warn">{voice.error}</p>}
                <div className="mt-1 flex flex-wrap items-center justify-end gap-x-2 gap-y-1.5">
                  {voice.supported && <MicButton listening={voice.listening} busy={voice.transcribing} label={voice.label} onClick={() => (voice.listening ? voice.stop() : voice.start())} />}
                  <button
                    type="button"
                    disabled={!githubConnected || busy || stage.step === "drafting" || !shown.trim()}
                    onClick={() => void launchOpen()}
                    title={target ? `Start an open colony on ${target} with this text as its instructions` : "Pick a repository above first"}
                    className="cursor-pointer rounded-lg border border-border bg-transparent px-2.5 py-1.5 text-[12.5px] text-muted hover:border-border-strong hover:text-text disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    Launch without an issue
                  </button>
                  <button
                    type="button"
                    disabled={!githubConnected || busy || stage.step === "drafting" || !shown.trim()}
                    onClick={() => void submit()}
                    className="inline-flex cursor-pointer items-center gap-1.5 rounded-lg border-0 bg-accent px-3 py-1.5 text-[12.5px] font-semibold text-on-accent hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    {(stage.step === "drafting" || busy) && <Spinner className="size-3" />}
                    {stage.step === "drafting" ? "Drafting…" : parseLoopCommand(shown) ? "Create loop" : "Draft issues"}
                  </button>
                </div>
                <div className="mt-1.5 flex flex-wrap items-center justify-between gap-x-3 gap-y-1 px-1">
                  <DispatchAfter checked={dispatchAfter} onChange={setDispatchAfter} />
                  <p className="m-0 text-[11.5px] text-faint">
                    <kbd className="font-sans">↵</kbd> drafts · <kbd className="font-sans">⇧↵</kbd> new line · <span className="font-mono">/loop 1h &lt;task&gt;</span> repeats it
                    {onOpenLaunch && (
                      <>
                        {" · "}
                        <button type="button" onClick={onOpenLaunch} className="cursor-pointer border-0 bg-transparent p-0 text-[11.5px] text-muted underline underline-offset-2 hover:text-text">
                          launch form
                        </button>
                      </>
                    )}
                  </p>
                </div>
              </div>
            ) : (
              <ConfirmDrafts
                stage={stage}
                scope={scope}
                dispatchAfter={dispatchAfter}
                onDispatchAfter={setDispatchAfter}
                onEdit={(id, patch) => send({ type: "edit", id, patch })}
                onRepo={(r) => send({ type: "repo", repo: r })}
                onBack={() => send({ type: "back" })}
                onCreate={() => void create()}
              />
            )}
            {draft.error && (
              <p role="alert" className="m-0 mt-2 whitespace-pre-line text-[12.5px] text-err">
                {draft.error}
              </p>
            )}
          </section>

          <div className="shrink-0 space-y-2 px-4 pt-3">
            <SearchBox value={list.query} onChange={list.setQuery} placeholder="Search title or #number…" label="search issues" className="w-full" />
            {issues.length > 0 && (
              <div role="group" aria-label="filter by label" className="flex max-h-16 flex-wrap gap-1 overflow-y-auto">
                {labelCounts(issues)
                  .slice(0, 24)
                  .map(([name, n]) => {
                    const on = labelSet.has(name);
                    return (
                      <button
                        key={name}
                        type="button"
                        aria-pressed={on}
                        onClick={() => list.setFilters({ labels: on ? list.filters.labels.filter((l) => l !== name) : [...list.filters.labels, name] })}
                        className={cx("cursor-pointer rounded-full border px-2 py-0.5 text-[11.5px]", on ? "border-accent bg-accent-soft text-accent" : "border-border bg-transparent text-muted hover:text-text")}
                      >
                        {name} <span className="tabular-nums text-faint">{n}</span>
                      </button>
                    );
                  })}
              </div>
            )}
          </div>

          <div className="flex shrink-0 items-center gap-2 px-4 py-2 text-[12px] text-muted">
            <button
              type="button"
              disabled={pickable.length === 0}
              onClick={() => setSelected((s) => toggleAll(s, pickable))}
              className="cursor-pointer rounded-md border border-border bg-transparent px-2 py-1 text-[12px] text-text hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {allOn ? "Select none" : `Select all ${pickable.length}`}
            </button>
            <span className="tabular-nums">
              {list.total} shown · {chosen.length} selected
            </span>
            {loadingCount > 0 && (
              <span className="ml-auto inline-flex items-center gap-1.5">
                <Spinner className="size-3" /> loading {loadingCount}…
              </span>
            )}
          </div>

          <ul aria-label="issues" className="m-0 list-none px-2 pb-1">
            {errors.map((e) => (
              <li key={e} className="px-2 py-1.5 text-[12px] text-err">
                {e}
              </li>
            ))}
            {loadingCount === 0 && list.total === 0 && <li className="px-2 py-6 text-center text-[13px] text-faint">No open issues match.</li>}
            {list.rows.map((issue) => {
              const key = issueKey(issue.repo, issue.number);
              const held = heldByFor(sessions, issue.repo, issue.number);
              const result = results[key];
              const fresh = made.some((m) => issueKey(m.repo, m.number) === key);
              const id = `colonize-${key.replace(/[^a-z0-9]/gi, "-")}`;
              return (
                <li key={key} data-new={fresh || undefined} className={cx("flex items-start gap-2.5 rounded-lg px-2 py-2 hover:bg-panel-2", held && "opacity-60", fresh && "bg-accent-soft/60")}>
                  <input
                    id={id}
                    type="checkbox"
                    disabled={held !== null || running}
                    checked={held === null && selected.has(key)}
                    onChange={() =>
                      setSelected((s) => {
                        const next = new Set(s);
                        if (next.has(key)) next.delete(key);
                        else next.add(key);
                        return next;
                      })
                    }
                    className="mt-0.5 size-4 shrink-0 cursor-pointer accent-[var(--accent)] disabled:cursor-not-allowed"
                  />
                  <label htmlFor={id} className="min-w-0 flex-1 cursor-pointer">
                    <span className="block text-[13px] leading-snug text-text">{issue.title}</span>
                    <span className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11.5px] text-faint">
                      <span className="font-mono">
                        {issue.repo.split("/")[1]}#{issue.number}
                      </span>
                      {fresh ? <span className="rounded-full bg-accent px-1.5 leading-4 text-on-accent">new</span> : <span>{relative(issue.updatedAt)}</span>}
                      {issue.author && <span>@{issue.author.login}</span>}
                      {issue.labels.slice(0, 4).map((l) => (
                        <span key={l.name} className="rounded-full border border-border px-1.5 leading-4 text-muted">
                          {l.name}
                        </span>
                      ))}
                    </span>
                  </label>
                  <span className="shrink-0 pt-0.5 text-[11.5px]">
                    {/* What this pane just did wins over "held": the colony holding it is the one it started. */}
                    {result ? (
                      <ResultBadge result={result} onOpen={onOpenColony} />
                    ) : held ? (
                      <button
                        type="button"
                        onClick={() => onOpenColony(held.id)}
                        title={`held by a colony: ${taskLine(held, held.id)}`}
                        className="cursor-pointer border-0 bg-transparent p-0 text-accent underline-offset-2 hover:underline"
                      >
                        held · open
                      </button>
                    ) : null}
                  </span>
                </li>
              );
            })}
          </ul>
          <Pagination view={list} onPage={list.setPage} noun="issues" className="mb-2 px-4" />
        </div>

        <div className="shrink-0 space-y-2.5 border-t border-border px-4 py-3">
          <textarea
            value={instructions}
            onChange={(e) => setInstructions(e.target.value)}
            rows={2}
            placeholder="Optional instructions for every colony…"
            aria-label="shared instructions"
            className="w-full resize-none rounded-md border border-border bg-panel-2 px-2.5 py-1.5 text-[13px] outline-none focus:border-border-strong"
          />
          <div className="flex items-center gap-3">
            <span className="flex items-center gap-2 text-[12.5px] text-muted">
              <Switch checked={autopilot} onChange={setAutopilot} label="autopilot" />
              Autopilot
            </span>
            <button
              type="button"
              disabled={chosen.length === 0 || running}
              onClick={() => void dispatch(chosen)}
              className="ant-glyph-host ml-auto inline-flex cursor-pointer items-center gap-2 rounded-lg border-0 bg-accent px-3.5 py-2 text-[13px] font-semibold text-on-accent hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {running ? <Spinner className="size-3.5" /> : <AntGlyph size={20} className="-mx-0.5" />}
              Dispatch {chosen.length} {chosen.length === 1 ? "colony" : "colonies"}
            </button>
          </div>
        </div>
      </div>
    </>
  );
  // Portalled to <body>: rendered in place it sat inside the page's own stacking context, below the
  // top bar, whose avatars and bell then covered the pane's title. (The static-markup tests have no document.)
  return typeof document === "undefined" ? body : createPortal(body, document.body);
}

/** The list's search and label match, for usePagedFilter. */
function matchIssue(issue: ScopedIssue, needle: string, filters: { labels: string[] }): boolean {
  return issueMatches(issue, needle, filters.labels);
}

function DispatchAfter({ checked, onChange }: { checked: boolean; onChange: (on: boolean) => void }): ReactElement {
  return (
    <label className="inline-flex cursor-pointer items-center gap-1.5 text-[12px] text-muted">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} className="size-3.5 cursor-pointer accent-[var(--accent)]" />
      Dispatch right after creating
    </label>
  );
}

function ConfirmDrafts({
  stage,
  scope,
  dispatchAfter,
  onDispatchAfter,
  onEdit,
  onRepo,
  onBack,
  onCreate,
}: {
  stage: Extract<DraftStep, { step: "confirm" | "creating" }>;
  scope: readonly Repo[];
  dispatchAfter: boolean;
  onDispatchAfter: (on: boolean) => void;
  onEdit: (id: number, patch: Partial<Omit<DraftRow, "id">>) => void;
  onRepo: (repo: string | null) => void;
  onBack: () => void;
  onCreate: () => void;
}): ReactElement {
  const creating = stage.step === "creating";
  const kept = keptDrafts(stage.drafts).length;
  const noun = kept === 1 ? "issue" : "issues";
  return (
    <div role="group" aria-label="confirm drafted issues" className="space-y-2.5">
      <div className="flex items-center gap-2">
        <h3 className="m-0 flex-1 text-[13px] font-semibold">
          {stage.drafts.length === 1 ? "One issue drafted" : `${stage.drafts.length} issues drafted`}
          {stage.note && <span className="ml-2 font-normal text-[11.5px] text-faint">{stage.note}</span>}
        </h3>
        <button type="button" disabled={creating} onClick={onBack} className="cursor-pointer border-0 bg-transparent p-0 text-[12px] text-muted underline underline-offset-2 hover:text-text disabled:opacity-50">
          Edit the text
        </button>
      </div>
      <label className="flex items-center gap-2 text-[12.5px] text-muted">
        <span className="shrink-0">Create on</span>
        <select
          aria-label="repository for the new issues"
          value={stage.repo ?? ""}
          disabled={creating}
          onChange={(e) => onRepo(e.target.value || null)}
          className={cx("min-w-0 flex-1 rounded-md border bg-panel-2 px-2 py-1 font-mono text-[12.5px] outline-none", stage.repo ? "border-border" : "border-accent")}
        >
          <option value="">Which repository?</option>
          {scope.map((r) => (
            <option key={r.full_name} value={r.full_name}>
              {r.full_name}
            </option>
          ))}
        </select>
      </label>
      <ol className="m-0 list-none space-y-2 p-0">
        {stage.drafts.map((d, i) => (
          <li key={d.id} className={cx("rounded-xl border bg-panel-2 p-2.5", d.keep ? "border-border-strong" : "border-border opacity-55")}>
            <div className="flex items-center gap-2">
              {stage.drafts.length > 1 && (
                <input
                  type="checkbox"
                  aria-label={`create draft ${i + 1}`}
                  checked={d.keep}
                  disabled={creating}
                  onChange={(e) => onEdit(d.id, { keep: e.target.checked })}
                  className="size-4 shrink-0 cursor-pointer accent-[var(--accent)]"
                />
              )}
              <input
                value={d.title}
                disabled={creating || !d.keep}
                onChange={(e) => onEdit(d.id, { title: e.target.value.replace(/\n/g, " ") })}
                aria-label={`title of draft ${i + 1}`}
                className="min-w-0 flex-1 rounded-md border border-transparent bg-transparent px-1.5 py-1 text-[13.5px] font-medium text-text outline-none hover:border-border focus:border-border-strong"
              />
            </div>
            <textarea
              value={d.body}
              rows={3}
              disabled={creating || !d.keep}
              onChange={(e) => onEdit(d.id, { body: e.target.value })}
              aria-label={`body of draft ${i + 1}`}
              className="mt-1 w-full resize-y rounded-md border border-transparent bg-transparent px-1.5 py-1 font-mono text-[12px] leading-[1.5] text-muted outline-none hover:border-border focus:border-border-strong"
            />
          </li>
        ))}
      </ol>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <DispatchAfter checked={dispatchAfter} onChange={onDispatchAfter} />
        <button
          type="button"
          disabled={creating || kept === 0 || !stage.repo}
          onClick={onCreate}
          className="ant-glyph-host ml-auto inline-flex cursor-pointer items-center gap-2 rounded-lg border-0 bg-accent px-3.5 py-2 text-[13px] font-semibold text-on-accent hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {creating ? <Spinner className="size-3.5" /> : <AntGlyph size={20} className="-mx-0.5" />}
          {creating ? "Creating…" : dispatchAfter ? `Create ${kept} ${noun} and dispatch` : `Create ${kept} ${noun}`}
        </button>
      </div>
    </div>
  );
}

function ResultBadge({ result, onOpen }: { result: HandoffResult; onOpen: (id: string) => void }): ReactElement {
  if (result.state === "pending") return <Spinner className="size-3.5" />;
  if (result.state === "held" || result.state === "error")
    return (
      <span title={result.message} className={result.state === "held" ? "text-warn" : "text-err"}>
        {result.state === "held" ? "already held" : "failed"}
      </span>
    );
  return (
    <button type="button" onClick={() => onOpen(result.sessionId)} className={cx("cursor-pointer border-0 bg-transparent p-0 underline-offset-2 hover:underline", result.state === "started" ? "text-ok" : "text-muted")}>
      {result.state}
    </button>
  );
}
