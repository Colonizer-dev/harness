// The friendlier issues list (issue #1218), as pure functions: where each issue stands (new,
// colonizing, PR open, blocked, done) read off the colonies, a plain one-line summary of what it
// asks, its priority from its labels, the filters, the grouping by repository, and the hidden set.
// The Colonize pane draws them (IssueList.tsx); Spotlight's Issues section ranks the same rows.
import { holdsIssue } from "../features/repos/claims";
import type { Issue, Session } from "../types";

export type IssueStatus = "new" | "colonizing" | "pr_open" | "blocked" | "done";
export type IssuePriority = "high" | "normal" | "low";

export const STATUS_ORDER: readonly IssueStatus[] = ["new", "colonizing", "pr_open", "blocked", "done"];

export const STATUS_LABEL: Record<IssueStatus, string> = {
  new: "New",
  colonizing: "Colonizing",
  pr_open: "PR open",
  blocked: "Needs you",
  done: "Done",
};

/** An issue with the repository it belongs to, as the list keeps it. */
export interface RepoIssue extends Issue {
  repo: string;
}

export interface IssueState {
  status: IssueStatus;
  /** The colony that holds (or last worked) the issue. */
  colony: Session | null;
  /** The colony is running, not waiting: the pill gets its live dot. */
  live: boolean;
  /** Where its pull request is, once there is one. */
  prUrl: string | null;
  /** A queued colony can be moved to the front. */
  queued: boolean;
}

const BLOCKED = new Set(["waiting_for_answer", "failed", "blocked", "parked"]);
const LIVE = new Set(["starting", "running", "idle", "publishing"]);

/** The newest colony that ever worked `(repo, number)`, whatever its status. */
function latestFor(sessions: readonly Session[], repo: string, number: number): Session | null {
  let best: Session | null = null;
  for (const s of sessions) {
    if (s.repo !== repo || s.issue !== number) continue;
    if (!best || Date.parse(s.updated_at ?? s.created_at) > Date.parse(best.updated_at ?? best.created_at)) best = s;
  }
  return best;
}

/** Where one issue stands, read off the colonies. Pure. */
export function issueState(sessions: readonly Session[], repo: string, number: number): IssueState {
  const holder = sessions.find((s) => s.repo === repo && s.issue === number && holdsIssue(s.status) && !s.claim_wait) ?? null;
  const colony = holder ?? latestFor(sessions, repo, number);
  if (!colony) return { status: "new", colony: null, live: false, prUrl: null, queued: false };
  const prUrl = colony.pr_url ?? null;
  const base = { colony, prUrl, live: LIVE.has(colony.status), queued: colony.status === "queued" };
  if (colony.status === "merged" || colony.status === "closed") return { ...base, status: "done" };
  if (BLOCKED.has(colony.status) && holder) return { ...base, status: "blocked" };
  if (colony.status === "pr_opened") return { ...base, status: "pr_open" };
  if (holder) return { ...base, status: "colonizing" };
  // A finished colony that left no PR (stopped, failed, no changes) leaves the issue free again.
  return { ...base, status: "new", live: false, queued: false };
}

const MARKDOWN = /(```[\s\S]*?```|!\[[^\]]*\]\([^)]*\)|\[([^\]]*)\]\([^)]*\)|[*_~>#]+|<[^>]+>)/g;

/** What an issue asks, in one plain line: its body's first sentence without Markdown, else a note that it has none. */
export function plainSummary(issue: Pick<Issue, "body" | "title">, max = 120): string {
  const body = (issue.body ?? "").replace(/`([^`]*)`/g, "$1").replace(MARKDOWN, (_m, _all, link: string | undefined) => link ?? "").replace(/\s+/g, " ").trim();
  if (!body) return "No description yet";
  const first = body.match(/^(.+?[.!?])(\s|$)/)?.[1] ?? body;
  return first.length > max ? `${first.slice(0, max - 1).trimEnd()}…` : first;
}

const HIGH = /^(p0|p1|priority[\s:/-]*(high|critical|urgent|p0|p1)|high[\s-]*priority|critical|urgent|blocker)$/i;
const LOW = /^(p3|p4|priority[\s:/-]*(low|p3|p4)|low[\s-]*priority|nice[\s-]*to[\s-]*have)$/i;

/** Priority from the labels people already use; normal when none says otherwise. */
export function issuePriority(issue: Pick<Issue, "labels">): IssuePriority {
  if (issue.labels.some((l) => HIGH.test(l.name.trim()))) return "high";
  if (issue.labels.some((l) => LOW.test(l.name.trim()))) return "low";
  return "normal";
}

export interface IssueFilters {
  labels: string[];
  /** Empty means every status. */
  statuses: IssueStatus[];
  priority: IssuePriority | "any";
}

export const NO_FILTERS: IssueFilters = { labels: [], statuses: [], priority: "any" };

/** Whether the filters narrow anything. */
export function filtering(f: IssueFilters, query: string): boolean {
  return f.labels.length > 0 || f.statuses.length > 0 || f.priority !== "any" || query.trim() !== "";
}

/** One issue against the search text (title, #number, repository, summary), the labels and the status and priority filters. */
export function issueFits(issue: RepoIssue, state: IssueStatus, needle: string, f: IssueFilters): boolean {
  const q = needle.replace(/^#/, "");
  const text = q === "" || issue.title.toLowerCase().includes(q) || String(issue.number).startsWith(q) || issue.repo.toLowerCase().includes(q) || plainSummary(issue).toLowerCase().includes(q);
  return (
    text &&
    f.labels.every((l) => issue.labels.some((x) => x.name === l)) &&
    (f.statuses.length === 0 || f.statuses.includes(state)) &&
    (f.priority === "any" || issuePriority(issue) === f.priority)
  );
}

/** How many issues sit in each status, for the chips. */
export function statusCounts(states: Iterable<IssueStatus>): Record<IssueStatus, number> {
  const counts: Record<IssueStatus, number> = { new: 0, colonizing: 0, pr_open: 0, blocked: 0, done: 0 };
  for (const s of states) counts[s] += 1;
  return counts;
}

export interface RepoGroup<T extends { repo: string }> {
  repo: string;
  rows: T[];
}

/** Rows grouped by repository, in the order each repository first appears. */
export function groupByRepo<T extends { repo: string }>(rows: readonly T[]): RepoGroup<T>[] {
  const groups = new Map<string, T[]>();
  for (const r of rows) groups.set(r.repo, [...(groups.get(r.repo) ?? []), r]);
  return [...groups].map(([repo, list]) => ({ repo, rows: list }));
}

/** Hide first the ones that need you, then what is new, then the rest, newest first inside each. */
export function listOrder(a: { state: IssueStatus; updatedAt: string }, b: { state: IssueStatus; updatedAt: string }): number {
  const rank = (s: IssueStatus) => ({ blocked: 0, new: 1, colonizing: 2, pr_open: 3, done: 4 })[s];
  return rank(a.state) - rank(b.state) || Date.parse(b.updatedAt) - Date.parse(a.updatedAt);
}

// --- Hidden issues ("Skip / Hide"), kept in this browser ------------------------------------------------

export const HIDDEN_KEY = "colonizer.issues.hidden";

export function loadHidden(raw: string | null): Set<string> {
  try {
    const list: unknown = raw ? JSON.parse(raw) : [];
    return new Set(Array.isArray(list) ? list.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

export function saveHidden(hidden: ReadonlySet<string>): string {
  return JSON.stringify([...hidden].slice(-500));
}

/** What to say when there is nothing to show, and what to do about it. */
export function emptyHint(input: { githubConnected: boolean; total: number; hidden: number; narrowed: boolean }): { title: string; body: string } {
  if (!input.githubConnected) return { title: "GitHub is not connected", body: "Connect it in Settings to list, create and colonize issues." };
  if (input.narrowed && input.total > 0) return { title: "Nothing matches these filters", body: "Clear a filter or the search to see the rest." };
  if (input.hidden > 0) return { title: "Everything left is hidden", body: `${input.hidden} hidden ${input.hidden === 1 ? "issue" : "issues"}. Show them to bring one back.` };
  return { title: "No open issues here", body: "Describe new work above and Colonizer files it, or pick another repository." };
}
