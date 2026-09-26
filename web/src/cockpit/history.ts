// The History timeline, built from the mothership's activity log (GET /api/activity) with the colony
// list filling in what the log is too young to know.
//
// The page used to be a reading of the colony list, one line per colony stamped with its
// `updated_at`. That field moves on every write — a reclaim sweep flipping `cleaned_up`, a restart
// re-marking a colony stopped — so one housekeeping pass re-dated days-old outcomes to "just now",
// and three colonies on the same issue read as the same event three times. The log records an
// outcome once, at the transition, and a person's action once, at the press; the colony list only
// supplies an outcome for a colony whose transition predates the log, marked as approximate.
import { dayLabel } from "./feed";
import { needsYou } from "../notifications";
import type { ActivityEntry, Session } from "../types";

/** How a row is drawn: its icon and colour. */
export type HistoryTone = "pr" | "merged" | "closed" | "failed" | "question" | "stopped" | "no_changes" | "launch" | "action";

/** Who a row says did it: you through the cockpit, you through the API token, or the colony. */
export type HistoryActor = "you" | "api" | "colony";

export interface HistoryItem {
  /** Stable across polls: the log's `seq`, or the colony and outcome for a row read off the colony list. */
  key: string;
  at: string;
  kind: string;
  tone: HistoryTone;
  actor: HistoryActor;
  /** One sentence, subject first, in plain words. */
  text: string;
  /** `chi-web#3473`, or null when the row is not about a repository. */
  subject: string | null;
  repo: string | null;
  org: string | null;
  colonyId: string | null;
  title: string | null;
  prUrl: string | null;
  /** Where the cockpit shows the row's target (a settings section, `secrets`, `loops`, …). */
  section: string | null;
  detail: string | null;
  /** Read off the colony list rather than recorded at the time: the time is the best the list has. */
  approximate: boolean;
}

/** The kind filter's options. */
export type HistoryKindFilter = "all" | "outcomes" | "questions" | "launches" | "yours" | "failures";

export const KIND_FILTERS: { value: HistoryKindFilter; label: string }[] = [
  { value: "all", label: "Everything" },
  { value: "outcomes", label: "Outcomes" },
  { value: "questions", label: "Questions" },
  { value: "launches", label: "Launches" },
  { value: "yours", label: "Your actions" },
  { value: "failures", label: "Failures" },
];

export interface HistoryFilters {
  kind: HistoryKindFilter;
  repo: string;
  actor: string;
}

export const HISTORY_ALL: HistoryFilters = { kind: "all", repo: "all", actor: "all" };

const OUTCOME_TONE: Record<string, HistoryTone> = {
  "outcome.pr_opened": "pr",
  "outcome.merged": "merged",
  "outcome.closed": "closed",
  "outcome.no_changes": "no_changes",
  "outcome.stopped": "stopped",
  "outcome.failed": "failed",
  "outcome.question": "question",
};

const LAUNCH_KINDS = new Set(["colony.launch", "chat.colony", "colonize.colony", "loop.run_now", "redteam.start", "colony.resume"]);

export function toneFor(kind: string): HistoryTone {
  if (OUTCOME_TONE[kind]) return OUTCOME_TONE[kind];
  if (LAUNCH_KINDS.has(kind)) return "launch";
  if (kind === "colony.publish") return "pr";
  if (kind === "colony.stop") return "stopped";
  if (kind === "colony.answer") return "question";
  return "action";
}

/** `chi-web#3473`, `chi-web`, or null. */
export function subjectOf(repo: string | null | undefined, issue: number | null | undefined): string | null {
  if (!repo) return null;
  const short = repo.split("/")[1] ?? repo;
  return issue != null ? `${short}#${issue}` : short;
}

function actorOf(entry: ActivityEntry): HistoryActor {
  if (entry.actor === "colony") return "colony";
  return entry.via === "api" ? "api" : "you";
}

/** The sentence a log line reads as. `s` is the subject: a colony, a loop, a repository. */
export function sentence(entry: Pick<ActivityEntry, "kind" | "actor" | "target" | "repo" | "issue" | "org">, stillWaiting = false): string {
  const s = subjectOf(entry.repo, entry.issue) ?? "a colony";
  const target = entry.target ?? "";
  const loop = target ? `the loop “${target}”` : "a loop";
  const you = entry.actor === "colony" ? null : "You";
  switch (entry.kind) {
    case "outcome.pr_opened":
      return you ? `You opened a pull request for ${s}` : `${s} opened a pull request`;
    case "outcome.merged":
      return `${s} was merged`;
    case "outcome.closed":
      return `${s} had its pull request closed`;
    case "outcome.no_changes":
      return `${s} finished with nothing to change`;
    case "outcome.stopped":
      return you ? `You stopped ${s}` : `${s} stopped`;
    case "outcome.failed":
      return `${s} failed`;
    case "outcome.question":
      return stillWaiting ? `${s} is waiting on your answer` : `${s} asked a question`;
    case "colony.launch":
      return `You launched ${s}`;
    case "chat.colony":
      return `You turned a chat into a colony on ${s}`;
    case "colony.stop":
      return `You stopped ${s}`;
    case "colony.resume":
      return `You resumed ${s}`;
    case "colony.delete":
      return `You deleted ${s}`;
    case "colony.publish":
      return `You pressed Create PR on ${s}`;
    case "colony.catch_up":
      return `You brought ${s} up to date with its base`;
    case "colony.cleanup":
      return `You cleaned up ${s}'s worktree`;
    case "colony.retain":
      return `You changed whether ${s} keeps its worktree`;
    case "colony.answer":
      return `You answered ${s}`;
    case "chat.issue":
      return "You filed an issue from a chat";
    case "colonize.issue":
      return `You created ${subjectOf(entry.repo, entry.issue) ?? "an issue"} from Colonize`;
    case "colonize.colony":
      return `You dispatched a colony on ${s} from Colonize`;
    case "loop.create":
      return `You created ${loop}`;
    case "loop.update":
      return `You edited ${loop}`;
    case "loop.pause":
      return `You paused ${loop}`;
    case "loop.resume":
      return `You resumed ${loop}`;
    case "loop.delete":
      return `You deleted ${loop}`;
    case "loop.run_now":
      return `You ran ${loop} now`;
    case "redteam.start":
      return `You started a red-team run on ${subjectOf(entry.repo, null) ?? "a repository"}`;
    case "redteam.stop":
      return "You stopped a red-team run";
    case "redteam.schedule":
      return "You saved a red-team schedule";
    case "redteam.unschedule":
      return "You removed a red-team schedule";
    case "remote.enable":
      return "You switched remote access on";
    case "remote.disable":
      return "You switched remote access off";
    case "remote.reset":
      return "You reset the remote access link";
    case "workspace.enable":
      return `You switched the ${target || entry.org || ""} workspace on`;
    case "workspace.disable":
      return `You switched the ${target || entry.org || ""} workspace off`;
    case "workspace.settings":
      return `You changed the ${target || entry.org || ""} workspace settings`;
    case "settings.save":
      return `You saved the ${target || "settings"}`;
    case "settings.remove":
      return `You removed the ${target || "setting"}`;
    case "memory.review":
    case "memory.note":
      return target ? `You ${target}` : "You changed shared memory";
    case "burn_down.stop":
      return "You stopped burn-down";
    case "app.update":
      return "You updated Colonizer";
    case "map.create":
      return `You asked for an architecture map of ${subjectOf(entry.repo, null) ?? "a repository"}`;
    default:
      return `${you ?? s}: ${entry.kind}`;
  }
}

function fromEntry(entry: ActivityEntry, waitingIds: ReadonlySet<string>): HistoryItem {
  return {
    key: `seq:${entry.seq}`,
    at: entry.ts,
    kind: entry.kind,
    tone: toneFor(entry.kind),
    actor: actorOf(entry),
    text: sentence(entry, entry.kind === "outcome.question" && entry.colony != null && waitingIds.has(entry.colony)),
    subject: subjectOf(entry.repo, entry.issue),
    repo: entry.repo ?? null,
    org: entry.org ?? entry.repo?.split("/")[0] ?? null,
    colonyId: entry.colony ?? null,
    title: entry.title ?? null,
    prUrl: entry.pr_url ?? null,
    section: entry.section ?? null,
    detail: entry.detail ?? null,
    approximate: false,
  };
}

/** The outcome kind a colony is showing now, or null while it is in between. */
export function currentOutcome(session: Session): string | null {
  if (needsYou(session) && session.status === "waiting_for_answer") return "outcome.question";
  switch (session.status) {
    case "pr_opened":
      return "outcome.pr_opened";
    case "merged":
      return "outcome.merged";
    case "closed":
      return "outcome.closed";
    case "no_changes":
      return "outcome.no_changes";
    case "stopped":
      return "outcome.stopped";
    case "failed":
      return "outcome.failed";
    default:
      return null;
  }
}

/** When a colony's current outcome happened, as best the colony list knows, and whether that is exact. */
function outcomeTime(session: Session, kind: string): { at: string; exact: boolean } {
  if (kind === "outcome.merged" && session.merged_at) return { at: session.merged_at, exact: true };
  if (kind === "outcome.pr_opened" && session.pr_opened_at) return { at: session.pr_opened_at, exact: true };
  const since = (session.attention as { since?: string } | null | undefined)?.since;
  if (kind === "outcome.question" && since) return { at: since, exact: true };
  return { at: session.updated_at, exact: false };
}

const ms = (at: string) => {
  const t = Date.parse(at);
  return Number.isNaN(t) ? 0 : t;
};

/**
 * One timeline, newest first: every log line, plus — for a colony whose current outcome the log
 * does not hold — one row read off the colony list. `complete` says the log was read to its start;
 * while older pages are unread, a colony older than the oldest loaded line is left out, because its
 * logged outcome may simply be on a page not loaded yet.
 */
export function buildTimeline(entries: readonly ActivityEntry[], sessions: readonly Session[], complete: boolean): HistoryItem[] {
  const waiting = new Set(sessions.filter((s) => s.status === "waiting_for_answer").map((s) => s.id));
  const seen = new Set<string>();
  const items: HistoryItem[] = [];
  for (const entry of entries) {
    if (seen.has(`seq:${entry.seq}`)) continue;
    seen.add(`seq:${entry.seq}`);
    if (entry.colony && OUTCOME_TONE[entry.kind]) seen.add(`${entry.colony}:${entry.kind}`);
    items.push(fromEntry(entry, waiting));
  }
  const oldest = entries.reduce((min, e) => Math.min(min, ms(e.ts) || Infinity), Infinity);
  for (const session of sessions) {
    const kind = currentOutcome(session);
    if (!kind || seen.has(`${session.id}:${kind}`)) continue;
    const { at, exact } = outcomeTime(session, kind);
    if (!complete && ms(at) < oldest) continue;
    seen.add(`${session.id}:${kind}`);
    items.push({
      key: `colony:${session.id}:${kind}`,
      at,
      kind,
      tone: toneFor(kind),
      actor: "colony",
      text: sentence({ kind, actor: "colony", repo: session.repo, issue: session.issue, target: null, org: session.org }, true),
      subject: subjectOf(session.repo, session.issue),
      repo: session.repo,
      org: session.org || session.repo.split("/")[0],
      colonyId: session.id,
      title: session.issue_title || session.summary || null,
      prUrl: session.pr_url,
      section: null,
      detail: kind === "outcome.failed" || kind === "outcome.stopped" ? session.error : null,
      approximate: !exact,
    });
  }
  return items.sort((a, b) => ms(b.at) - ms(a.at) || (a.key < b.key ? 1 : a.key > b.key ? -1 : 0));
}

/** Whether an item passes the kind filter. */
export function matchesKind(item: HistoryItem, kind: HistoryKindFilter): boolean {
  switch (kind) {
    case "all":
      return true;
    case "outcomes":
      return item.kind.startsWith("outcome.");
    case "questions":
      return item.kind === "outcome.question" || item.kind === "colony.answer";
    case "launches":
      return LAUNCH_KINDS.has(item.kind);
    case "yours":
      return item.actor !== "colony";
    case "failures":
      return item.kind === "outcome.failed";
  }
}

/** The search box, the kind, repository and actor filters, all at once. */
export function matchesHistory(item: HistoryItem, query: string, filters: HistoryFilters): boolean {
  if (!matchesKind(item, filters.kind)) return false;
  if (filters.repo !== "all" && item.repo !== filters.repo) return false;
  if (filters.actor !== "all" && item.actor !== filters.actor) return false;
  if (!query) return true;
  return [item.text, item.repo, item.title, item.detail, item.colonyId, item.subject].some((f) => f != null && f.toLowerCase().includes(query));
}

export const ACTOR_LABEL: Record<HistoryActor, string> = { you: "You", api: "You (API token)", colony: "Colonies" };

// ---------------------------------------------------------------------------
// Collapsing: a run of low-signal rows is one row until opened.
// ---------------------------------------------------------------------------

/** The kinds a run of which says less than one line saying how many. */
const QUIET_KINDS = new Set(["outcome.no_changes", "outcome.stopped", "outcome.closed", "colony.stop", "colony.delete", "colony.cleanup"]);

/** How many alike rows in a row make a group. */
export const COLLAPSE_AT = 3;

export type HistoryRow =
  | { type: "item"; key: string; item: HistoryItem }
  | { type: "group"; key: string; kind: string; actor: HistoryActor; tone: HistoryTone; items: HistoryItem[] };

function collapseKey(item: HistoryItem, now: Date): string | null {
  if (!QUIET_KINDS.has(item.kind)) return null;
  return `${dayLabel(item.at, now)}|${item.actor}|${item.kind === "colony.stop" ? "outcome.stopped" : item.kind}`;
}

/**
 * Folds every run of at least COLLAPSE_AT alike quiet rows — same kind, same actor, same day, next
 * to each other in time — into one group row. Everything else stays a row of its own.
 */
export function collapse(items: readonly HistoryItem[], now: Date): HistoryRow[] {
  const rows: HistoryRow[] = [];
  let i = 0;
  while (i < items.length) {
    const key = collapseKey(items[i], now);
    let j = i + 1;
    while (key && j < items.length && collapseKey(items[j], now) === key) j++;
    if (key && j - i >= COLLAPSE_AT) {
      const run = items.slice(i, j);
      rows.push({ type: "group", key: `group:${run[0].key}`, kind: run[0].kind, actor: run[0].actor, tone: run[0].tone, items: run });
    } else {
      for (const item of items.slice(i, key ? j : i + 1)) rows.push({ type: "item", key: item.key, item });
    }
    i = key ? j : i + 1;
  }
  return rows;
}

/** A group's one line: "12 colonies finished with nothing to change". */
export function groupText(row: Extract<HistoryRow, { type: "group" }>): string {
  const n = row.items.length;
  const you = row.actor !== "colony";
  switch (row.kind) {
    case "outcome.no_changes":
      return `${n} colonies finished with nothing to change`;
    case "outcome.stopped":
    case "colony.stop":
      return you ? `You stopped ${n} colonies` : `${n} colonies stopped`;
    case "outcome.closed":
      return `${n} pull requests were closed`;
    case "colony.delete":
      return `You deleted ${n} colonies`;
    case "colony.cleanup":
      return `You cleaned up ${n} worktrees`;
    default:
      return `${n} events`;
  }
}

/** The repositories a group touches, most frequent first, as `chi-web, chi-app +3`. */
export function groupRepos(items: readonly HistoryItem[], shown = 3): string {
  const counts = new Map<string, number>();
  for (const item of items) {
    const name = item.repo?.split("/")[1] ?? item.repo;
    if (name) counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  const names = [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([name, count]) => (count > 1 ? `${name} ×${count}` : name));
  return names.length > shown ? `${names.slice(0, shown).join(", ")} +${names.length - shown}` : names.join(", ");
}

/** The wall-clock time, 24h (a locale adding AM/PM would wrap the column). */
export function clock(at: string): string {
  const when = new Date(at);
  if (Number.isNaN(when.getTime())) return "";
  return when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}

/** A group's span, oldest to newest: "00:15–00:16", or one time when they share a minute. */
export function groupSpan(items: readonly HistoryItem[]): string {
  const newest = clock(items[0].at);
  const oldest = clock(items[items.length - 1].at);
  return newest === oldest ? newest : `${oldest}–${newest}`;
}

/** The time a row sorts and files under. */
export function rowAt(row: HistoryRow): string {
  return row.type === "item" ? row.item.at : row.items[0].at;
}

/** A page's rows under their day headings. */
export function byDay(rows: readonly HistoryRow[], now: Date): { day: string; rows: HistoryRow[] }[] {
  const days: { day: string; rows: HistoryRow[] }[] = [];
  for (const row of rows) {
    const day = dayLabel(rowAt(row), now);
    const last = days[days.length - 1];
    if (last && last.day === day) last.rows.push(row);
    else days.push({ day, rows: [row] });
  }
  return days;
}

/** The summary above the timeline, over what the filters let through. */
export function summarize(items: readonly HistoryItem[]): { prs: number; merged: number; failed: number; questions: number; yours: number } {
  const out = { prs: 0, merged: 0, failed: 0, questions: 0, yours: 0 };
  for (const item of items) {
    if (item.kind === "outcome.pr_opened") out.prs++;
    else if (item.kind === "outcome.merged") out.merged++;
    else if (item.kind === "outcome.failed") out.failed++;
    else if (item.kind === "outcome.question") out.questions++;
    if (item.actor !== "colony") out.yours++;
  }
  return out;
}
