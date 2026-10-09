// The merge-train loop (issue #754): the pure half of the Loops page's built-in "Merge train"
// card — the defaults, the per-repository opt-in, and how a run's report reads.
import type { MergeLoopAction, MergeLoopReport, MergeLoopSettings } from "../types";

/** The server's defaults: off, hourly, nothing opted in, no self-heal, no revert, no redo colonies. */
export function defaultMergeLoopSettings(): MergeLoopSettings {
  return {
    enabled: false,
    cadence: { every: "interval", minutes: 60 },
    allow: [],
    never: [],
    max_merges: 4,
    repo_max_merges: {},
    cooldown_secs: 120,
    ci_wait_minutes: 20,
    ci_poll_secs: 120,
    flaky_checks: [],
    self_heal: false,
    revert_on_red: false,
    redo_on_conflict: false,
    max_api_calls: 400,
    min_call_gap_ms: 1000,
    held: [],
    local_checks: [],
    resolve_conflicts: false,
    resolve_attempts: 3,
  };
}

/** Where a repository stands: opted in (itself or through its org), never, or not opted in. */
export type RepoOptIn = "on" | "org" | "never" | "off";

export function repoOptIn(settings: MergeLoopSettings, repo: string): RepoOptIn {
  const r = repo.toLowerCase();
  const owner = r.split("/")[0];
  const has = (list: string[], key: string) => list.some((t) => t.toLowerCase() === key);
  if (has(settings.never, r) || has(settings.never, owner)) return "never";
  if (has(settings.allow, r)) return "on";
  if (has(settings.allow, owner) || has(settings.allow, "*")) return "org";
  return "off";
}

/** The settings with one repository switched in or out; switching it in takes it off the never list. */
export function setRepoOptIn(settings: MergeLoopSettings, repo: string, on: boolean): MergeLoopSettings {
  const r = repo.toLowerCase();
  const allow = settings.allow.filter((t) => t.toLowerCase() !== r);
  const never = settings.never.filter((t) => t.toLowerCase() !== r);
  return { ...settings, allow: on ? [...allow, r] : allow, never };
}

/** The settings with one repository on the never list (and off the allowlist). */
export function setRepoNever(settings: MergeLoopSettings, repo: string, never: boolean): MergeLoopSettings {
  const r = repo.toLowerCase();
  const rest = settings.never.filter((t) => t.toLowerCase() !== r);
  return { ...settings, allow: never ? settings.allow.filter((t) => t.toLowerCase() !== r) : settings.allow, never: never ? [...rest, r] : rest };
}

/** The settings with a repository's own merge cap set, or cleared (null) back to the global cap. */
export function setRepoCap(settings: MergeLoopSettings, repo: string, cap: number | null): MergeLoopSettings {
  const caps = { ...settings.repo_max_merges };
  delete caps[repo.toLowerCase()];
  if (cap !== null && Number.isFinite(cap) && cap > 0) caps[repo.toLowerCase()] = Math.round(cap);
  return { ...settings, repo_max_merges: caps };
}

/** The settings with one repository switched in or out of the local-checks list, once (issue #969). */
export function setRepoLocalChecks(settings: MergeLoopSettings, repo: string, on: boolean): MergeLoopSettings {
  const r = repo.toLowerCase();
  const rest = settings.local_checks.filter((t) => t.toLowerCase() !== r);
  return { ...settings, local_checks: on ? [...rest, r] : rest };
}

/** Whether the local-checks list covers a repository: its own entry, or its owner's. */
export function repoWantsLocalChecks(settings: MergeLoopSettings, repo: string): boolean {
  const r = repo.toLowerCase();
  return settings.local_checks.some((t) => t.toLowerCase() === r || t.toLowerCase() === r.split("/")[0]);
}

/** The comma-separated field the form edits, read back as a list. */
export function parseNames(text: string): string[] {
  return [...new Set(text.split(",").map((t) => t.trim()).filter(Boolean))];
}

/** The order the report lists pull requests in: what moved first, then what needs a person, then the rest. */
const ORDER: MergeLoopAction[] = ["merged", "updated", "rebased", "resolving", "rerun", "redo_dispatched", "red", "needs_redo", "waiting", "skipped"];

/** The words an action reads as, for a real run or a dry one. */
export function actionLabel(action: MergeLoopAction, dry: boolean): string {
  const real: Record<MergeLoopAction, string> = {
    merged: "merged",
    updated: "updated (CI running)",
    rebased: "rebased (CI running)",
    red: "red",
    rerun: "re-ran flaky checks",
    needs_redo: "needs redo",
    redo_dispatched: "redo dispatched",
    resolving: "resolving conflicts",
    waiting: "waiting",
    skipped: "skipped",
  };
  const would: Partial<Record<MergeLoopAction, string>> = {
    merged: "would merge",
    updated: "would update",
    rebased: "would rebase",
    rerun: "would re-run",
    redo_dispatched: "would dispatch a redo",
    resolving: "would resolve conflicts",
  };
  return dry ? (would[action] ?? real[action]) : real[action];
}

export interface ReportRow {
  repo: string;
  pr: string;
  pr_url: string;
  title: string;
  action: MergeLoopAction;
  label: string;
  reason: string;
  /** The colony behind this pull request, for an "Open colony" link. */
  session: string;
  /** Which pass of the resolve loop this is, pulled out of the reason (issue #968). */
  attempt?: { n: number; m: number };
}

/** Whether a row is a plain "not opted in" skip: the repository is simply not in the train, so the
 * report hides the rows behind one quiet line instead of listing every pull request (issue #1224). */
export function isNotOptedIn(row: ReportRow): boolean {
  return row.action === "skipped" && /not opted in/i.test(row.reason);
}

/** Lifts "attempt 1/3" out of a resolving reason so the line can show it as a pill, not prose. */
function attemptOf(reason: string): { reason: string; attempt?: { n: number; m: number } } {
  const m = reason.match(/\battempt (\d+)\/(\d+)\b/);
  if (!m) return { reason };
  const text = reason
    .replace(/\s*\(?attempt \d+\/\d+\)?/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/[.,;:]$/, "");
  return { reason: text, attempt: { n: Number(m[1]), m: Number(m[2]) } };
}

/** Every pull request of a report as one sorted row, `#N` for its number. */
export function reportRows(report: MergeLoopReport): ReportRow[] {
  const rows = report.repos.flatMap((r) =>
    r.items.map((i) => ({
      repo: r.repo,
      pr: `#${i.pr_url.split("/").pop() ?? "?"}`,
      pr_url: i.pr_url,
      title: i.title,
      action: i.action,
      label: actionLabel(i.action, report.dry_run),
      ...attemptOf(i.reason),
      session: i.session,
    })),
  );
  return rows.sort((a, b) => ORDER.indexOf(a.action) - ORDER.indexOf(b.action) || a.repo.localeCompare(b.repo) || a.pr.localeCompare(b.pr));
}

/** The repositories worth a toggle: every opted-in or never entry, plus the ones the cockpit knows. */
export function toggleRepos(settings: MergeLoopSettings, known: readonly string[]): string[] {
  const all = new Set<string>();
  for (const r of known) all.add(r.toLowerCase());
  for (const t of [...settings.allow, ...settings.never]) if (t.includes("/")) all.add(t.toLowerCase());
  return [...all].sort();
}
