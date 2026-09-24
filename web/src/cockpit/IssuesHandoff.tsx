// The top bar's issues button: the GitHub mark and how many open issues the scope has, opening a
// slide-over where issues are picked and handed off to colonies in one go. The list comes from
// GET /api/repos/{o}/{r}/issues, which the mothership already narrows by the Source module's label
// filter; the badge is GitHub's own count until the pane has loaded the filtered one.
import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { createPortal } from "react-dom";

import { ApiError, heldByFor } from "../api";
import { errorMessage, useApi, useToast } from "../context";
import { Spinner, Switch, cx, sameOrg } from "../components/ui";
import type { Issue, Repo, Session } from "../types";
import { relative } from "./InboxView";
import { taskLine } from "../summary";

/** Repositories fetched when the pane shows "all repos in scope": the most recently pushed first. */
export const ALL_REPOS_LIMIT = 30;
/** Issue lists fetched at once, so a wide scope does not burst the mothership's `gh`. */
const FETCH_CONCURRENCY = 4;

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

/** Issues matching the search text (title or #number) and carrying every chosen label. */
export function filterIssues(issues: readonly ScopedIssue[], search: string, labels: ReadonlySet<string>): ScopedIssue[] {
  const q = search.trim().toLowerCase().replace(/^#/, "");
  return issues.filter(
    (i) =>
      (q === "" || i.title.toLowerCase().includes(q) || String(i.number).startsWith(q)) &&
      [...labels].every((l) => i.labels.some((x) => x.name === l)),
  );
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

function GitHubMark({ size = 15 }: { size?: number }): ReactElement {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M9 19c-4 1.3-4-2-5.5-2.5M14.5 21v-3.2c0-1 .1-1.5-.5-2 2.8-.3 5.5-1.4 5.5-6a4.7 4.7 0 0 0-1.3-3.3 4.4 4.4 0 0 0-.1-3.3s-1-.3-3.4 1.3a11.6 11.6 0 0 0-6.2 0C6.1 2.9 5.1 3.2 5.1 3.2A4.4 4.4 0 0 0 5 6.5a4.7 4.7 0 0 0-1.3 3.3c0 4.6 2.7 5.7 5.5 6-.6.5-.6 1.2-.5 2V21" />
    </svg>
  );
}

export function IssuesButton({ variant = "compact", ...props }: IssuesActions & { variant?: "compact" | "header" }): ReactElement {
  const [open, setOpen] = useState(false);
  // The filtered count the pane last loaded for this scope, which beats GitHub's rough one.
  const [loaded, setLoaded] = useState<{ scope: string; count: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const scope = useMemo(() => scopeRepos(props.repos, props.org), [props.repos, props.org]);
  const scopeKey = props.org ?? "*";
  const exact = loaded?.scope === scopeKey ? loaded.count : null;
  const count = exact ?? roughIssueCount(scope);
  const label =
    exact !== null
      ? `${count} open issues to hand off (filtered by the Source labels)`
      : `about ${count} open issues (GitHub's count, which includes pull requests; the pane loads the filtered list)`;

  return (
    <>
      <button
        ref={button}
        type="button"
        title={label}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        disabled={!props.githubConnected}
        onClick={() => setOpen((o) => !o)}
        className={cx(
          // The dashboard's primary action: solid accent, the one orange button on the page.
          variant === "header"
            ? "inline-flex h-9 shrink-0 cursor-pointer items-center gap-2 rounded-lg border-0 bg-accent px-3.5 text-[13px] font-semibold text-on-accent shadow-[0_1px_0_rgb(0_0_0/0.15)] transition-[filter,transform] hover:brightness-110 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-50"
            : "inline-flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-transparent px-2.5 text-[12.5px] font-medium text-muted transition-colors hover:border-border-strong hover:text-text disabled:cursor-not-allowed disabled:opacity-50",
          open && (variant === "header" ? "brightness-110" : "border-border-strong bg-panel-2 text-text"),
        )}
      >
        <GitHubMark size={variant === "header" ? 16 : 14} />
        {variant === "header" && <span>Send colonies</span>}
        <span className={cx("tabular-nums", variant === "header" && "rounded-full bg-black/20 px-2 py-px text-[12px]")}>{count > 999 ? "999+" : count}</span>
      </button>
      {open && (
        <IssuesPane
          {...props}
          scope={scope}
          onLoadedCount={(n) => setLoaded({ scope: scopeKey, count: n })}
          onClose={() => {
            setOpen(false);
            button.current?.focus();
          }}
        />
      )}
    </>
  );
}

function IssuesPane({
  scope,
  sessions,
  autopilotDefault,
  onCreated,
  onOpenColony,
  onLoadedCount,
  onClose,
}: IssuesActions & { scope: Repo[]; onLoadedCount: (count: number) => void; onClose: () => void }): ReactElement {
  const api = useApi();
  const toast = useToast();
  const panel = useRef<HTMLDivElement>(null);
  const [repo, setRepo] = useState<string>("*");
  const [repoQuery, setRepoQuery] = useState("");
  const [lists, setLists] = useState<Record<string, ScopedIssue[] | "loading" | { error: string }>>({});
  const [search, setSearch] = useState("");
  const [labels, setLabels] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [instructions, setInstructions] = useState("");
  const [autopilot, setAutopilot] = useState(autopilotDefault);
  const [results, setResults] = useState<Record<string, HandoffResult>>({});
  const [running, setRunning] = useState(false);

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
  const issues = useMemo(
    () =>
      wanted
        .flatMap((n) => (Array.isArray(lists[n]) ? (lists[n] as ScopedIssue[]) : []))
        .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt)),
    [wanted, lists],
  );

  // Once every repository in "all" has answered, the filtered total replaces the badge's rough count.
  useEffect(() => {
    if (repo === "*" && loadingCount === 0 && errors.length === 0 && wanted.length > 0) onLoadedCount(issues.length);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, loadingCount, errors.length, issues.length]);

  const shown = filterIssues(issues, search, labels);
  const pickable = selectable(shown, sessions);
  const chosen = issues.filter((i) => selected.has(issueKey(i.repo, i.number)) && heldByFor(sessions, i.repo, i.number) === null);
  const allOn = pickable.length > 0 && pickable.every((i) => selected.has(issueKey(i.repo, i.number)));

  // Focus into the pane on open; Escape closes it.
  useEffect(() => {
    panel.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const handOff = async () => {
    if (chosen.length === 0 || running) return;
    setRunning(true);
    const next: Record<string, HandoffResult> = Object.fromEntries(chosen.map((i) => [issueKey(i.repo, i.number), { state: "pending" } as HandoffResult]));
    setResults(next);
    // One at a time: the queue decides what starts, and a burst would only race the 409 guard.
    for (const issue of chosen) {
      const key = issueKey(issue.repo, issue.number);
      let result: HandoffResult;
      try {
        const session = await api.createSession({
          repo: issue.repo,
          issue: issue.number,
          title: issue.title,
          instructions: instructions.trim() || undefined,
          autopilot,
        });
        onCreated(session);
        result = { state: session.status === "queued" ? "queued" : "started", sessionId: session.id };
      } catch (e) {
        result = e instanceof ApiError && e.status === 409 ? { state: "held", message: errorMessage(e) } : { state: "error", message: errorMessage(e) };
      }
      next[key] = result;
      setResults({ ...next });
    }
    setRunning(false);
    setSelected(new Set());
    const line = summarize(next);
    toast(`Handed off: ${line}`, Object.values(next).some((r) => r.state === "error") ? "error" : undefined);
  };

  const repoChoices = repoQuery.trim() ? scope.filter((r) => r.full_name.toLowerCase().includes(repoQuery.trim().toLowerCase())) : scope;

  // Portalled to <body>: rendered in place it sat inside the page's own stacking context, below the
  // top bar, whose avatars and bell then covered the pane's title.
  return createPortal(
    <>
      <div aria-hidden="true" className="fixed inset-0 z-[60] bg-black/30" onClick={onClose} />
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label="hand off issues to colonies"
        tabIndex={-1}
        className="fixed inset-y-0 right-0 z-[61] flex w-full max-w-[480px] animate-[ck-in_160ms_ease-out_both] flex-col border-l border-border-strong bg-panel text-text shadow-[-16px_0_48px_rgb(0_0_0/0.35)] outline-none"
      >
        <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-4 py-3">
          <span className="grid size-8 place-items-center rounded-lg bg-panel-2 text-text">
            <GitHubMark size={17} />
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-[15px] font-semibold">Hand off issues</h2>
            <p className="text-[12px] text-muted">Pick issues and start a colony on each. Labels follow Settings → Source.</p>
          </div>
          <button type="button" onClick={onClose} aria-label="close" className="grid size-8 cursor-pointer place-items-center rounded-lg border-0 bg-transparent text-muted hover:bg-panel-2 hover:text-text">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="shrink-0 space-y-2 border-b border-border px-4 py-3">
          <label className="block text-[11.5px] font-medium uppercase tracking-wide text-faint" htmlFor="handoff-repo">
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
              id="handoff-repo"
              value={repo}
              onChange={(e) => setRepo(e.target.value)}
              className="min-w-0 max-w-[230px] flex-1 rounded-md border border-border bg-panel-2 px-2 py-1.5 text-[13px] outline-none focus:border-border-strong"
            >
              <option value="*">All repos in scope ({Math.min(ALL_REPOS_LIMIT, scope.filter((r) => r.open_issues_count > 0).length)})</option>
              {repoChoices.map((r) => (
                <option key={r.full_name} value={r.full_name}>
                  {r.full_name} · {r.open_issues_count}
                </option>
              ))}
            </select>
          </div>
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search title or #number…"
            aria-label="search issues"
            className="w-full rounded-md border border-border bg-panel-2 px-2.5 py-1.5 text-[13px] outline-none focus:border-border-strong"
          />
          {issues.length > 0 && (
            <div role="group" aria-label="filter by label" className="flex max-h-16 flex-wrap gap-1 overflow-y-auto">
              {labelCounts(issues)
                .slice(0, 24)
                .map(([name, n]) => {
                  const on = labels.has(name);
                  return (
                    <button
                      key={name}
                      type="button"
                      aria-pressed={on}
                      onClick={() =>
                        setLabels((s) => {
                          const next = new Set(s);
                          if (on) next.delete(name);
                          else next.add(name);
                          return next;
                        })
                      }
                      className={cx(
                        "cursor-pointer rounded-full border px-2 py-0.5 text-[11.5px]",
                        on ? "border-accent bg-accent-soft text-accent" : "border-border bg-transparent text-muted hover:text-text",
                      )}
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
            {shown.length} shown · {chosen.length} selected
          </span>
          {loadingCount > 0 && (
            <span className="ml-auto inline-flex items-center gap-1.5">
              <Spinner className="size-3" /> loading {loadingCount}…
            </span>
          )}
        </div>

        <ul aria-label="issues" className="scroll-thin min-h-0 flex-1 overflow-y-auto px-2 pb-2">
          {errors.map((e) => (
            <li key={e} className="px-2 py-1.5 text-[12px] text-err">
              {e}
            </li>
          ))}
          {loadingCount === 0 && shown.length === 0 && <li className="px-2 py-6 text-center text-[13px] text-faint">No open issues match.</li>}
          {shown.map((issue) => {
            const key = issueKey(issue.repo, issue.number);
            const held = heldByFor(sessions, issue.repo, issue.number);
            const result = results[key];
            const id = `handoff-${key.replace(/[^a-z0-9]/gi, "-")}`;
            return (
              <li key={key} className={cx("flex items-start gap-2.5 rounded-lg px-2 py-2 hover:bg-panel-2", held && "opacity-60")}>
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
                    <span>{relative(issue.updatedAt)}</span>
                    {issue.author && <span>@{issue.author.login}</span>}
                    {issue.labels.slice(0, 4).map((l) => (
                      <span key={l.name} className="rounded-full border border-border px-1.5 leading-4 text-muted">
                        {l.name}
                      </span>
                    ))}
                  </span>
                </label>
                <span className="shrink-0 pt-0.5 text-[11.5px]">
                  {held ? (
                    <button
                      type="button"
                      onClick={() => onOpenColony(held.id)}
                      title={`held by a colony: ${taskLine(held, held.id)}`}
                      className="cursor-pointer border-0 bg-transparent p-0 text-accent underline-offset-2 hover:underline"
                    >
                      held · open
                    </button>
                  ) : result ? (
                    <ResultBadge result={result} onOpen={onOpenColony} />
                  ) : null}
                </span>
              </li>
            );
          })}
        </ul>

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
              onClick={() => void handOff()}
              className="ml-auto inline-flex cursor-pointer items-center gap-2 rounded-lg border-0 bg-accent px-3.5 py-2 text-[13px] font-medium text-on-accent disabled:cursor-not-allowed disabled:opacity-50"
            >
              {running && <Spinner className="size-3.5" />}
              Hand off {chosen.length} to {chosen.length === 1 ? "a colony" : "colonies"}
            </button>
          </div>
        </div>
      </div>
    </>,
    document.body,
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
