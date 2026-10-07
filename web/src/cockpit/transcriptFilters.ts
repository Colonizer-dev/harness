// Transcript search filters (issue #1124): the pure state behind the History page's filter chips.
// The workspace (org) is not a filter here — it comes from the workspace switcher and is added to
// the query by the caller. Everything below is free of React and of `window` so it can be tested
// and round-tripped through the address bar.
import type { HistorySearchQuery } from "../types";

/** What the chips edit; the search text `q` lives beside them in the same URL. */
export interface TranscriptFilters {
  q: string;
  repo: string;
  agent: string;
  status: string;
  since: string;
  until: string;
}

export type FilterKey = Exclude<keyof TranscriptFilters, "q">;

export const EMPTY_TRANSCRIPT_FILTERS: TranscriptFilters = { q: "", repo: "", agent: "", status: "", since: "", until: "" };

/** Address-bar parameter for each field; prefixed so they never collide with the cockpit's own. */
const PARAMS: Record<keyof TranscriptFilters, string> = { q: "tq", repo: "trepo", agent: "tagent", status: "tstatus", since: "tsince", until: "tuntil" };

const DATE = /^\d{4}-\d{2}-\d{2}$/;

/** Parse the filters out of a location search string; unknown statuses and malformed dates are dropped. */
export function parseTranscriptFilters(search: string, statuses: readonly string[]): TranscriptFilters {
  const p = new URLSearchParams(search);
  const get = (k: keyof TranscriptFilters) => p.get(PARAMS[k]) ?? "";
  const date = (k: "since" | "until") => (DATE.test(get(k)) ? get(k) : "");
  const status = get("status");
  return { q: get("q"), repo: get("repo"), agent: get("agent"), status: statuses.includes(status) ? status : "", since: date("since"), until: date("until") };
}

/**
 * Write the filters into a search string, leaving every parameter that is not ours untouched.
 * Returns the new search string (with its leading `?`, or "" when empty).
 */
export function serializeTranscriptFilters(filters: TranscriptFilters, base = ""): string {
  const p = new URLSearchParams(base);
  for (const k of Object.keys(PARAMS) as (keyof TranscriptFilters)[]) {
    const v = filters[k];
    if (v) p.set(PARAMS[k], v);
    else p.delete(PARAMS[k]);
  }
  const s = p.toString();
  return s ? `?${s}` : "";
}

/** The query the mothership takes: `q` trimmed, blanks dropped, the workspace added. Limit stays the mothership's. */
export function searchQuery(filters: TranscriptFilters, org?: string | null): HistorySearchQuery {
  return {
    q: filters.q.trim(),
    repo: filters.repo.trim() || undefined,
    org: org?.trim() || undefined,
    agent: filters.agent.trim() || undefined,
    status: filters.status || undefined,
    since: filters.since || undefined,
    until: filters.until || undefined,
  };
}

/** The distinct, sorted, non-empty values of a field as they occur in `rows`. */
export function distinctValues<T>(rows: readonly T[], pick: (row: T) => string | null | undefined): string[] {
  return [...new Set(rows.map(pick).filter((v): v is string => !!v))].sort((a, b) => a.localeCompare(b));
}

/** Case-insensitive substring filter for a searchable value list. */
export function matchValues(values: readonly string[], needle: string): string[] {
  const n = needle.trim().toLowerCase();
  return n ? values.filter((v) => v.toLowerCase().includes(n)) : [...values];
}

export type DatePreset = "today" | "7d" | "30d";

const ymd = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;

/** A preset's `since`/`until` in the viewer's local calendar; `until` is today for all of them. */
export function datePreset(preset: DatePreset, now: Date = new Date()): Pick<TranscriptFilters, "since" | "until"> {
  const from = new Date(now.getFullYear(), now.getMonth(), now.getDate() - (preset === "today" ? 0 : preset === "7d" ? 6 : 29));
  return { since: ymd(from), until: ymd(now) };
}

/** An active filter, shown as a removable chip; `clear` is the patch that removes it. */
export interface ActiveFilter {
  id: string;
  label: string;
  clear: Partial<TranscriptFilters>;
}

export function activeFilters(filters: TranscriptFilters, statusLabel: (s: string) => string = (s) => s): ActiveFilter[] {
  const out: ActiveFilter[] = [];
  if (filters.repo) out.push({ id: "repo", label: `Repository: ${filters.repo}`, clear: { repo: "" } });
  if (filters.agent) out.push({ id: "agent", label: `Agent: ${filters.agent}`, clear: { agent: "" } });
  if (filters.status) out.push({ id: "status", label: `Status: ${statusLabel(filters.status)}`, clear: { status: "" } });
  if (filters.since || filters.until) {
    const range = filters.since && filters.until ? `${filters.since} – ${filters.until}` : filters.since ? `from ${filters.since}` : `to ${filters.until}`;
    out.push({ id: "date", label: `Date: ${range}`, clear: { since: "", until: "" } });
  }
  return out;
}

/** Every filter blank, the search text kept — what "Clear all" does. */
export function clearFilters(filters: TranscriptFilters): TranscriptFilters {
  return { ...EMPTY_TRANSCRIPT_FILTERS, q: filters.q };
}
