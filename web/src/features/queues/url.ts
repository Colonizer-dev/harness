// The Queues view's filters live in the address (issue #1127), like the route table's `?org=`:
// `?host=&reason=&repo=&agent=&q=&group=`. Pure string work, no router imports, so the round trip
// is pinned by tests without a browser. The grouping is the view's own; the rest are GET
// /api/queues' query parameters.
import type { QueuesFilters, QueuesGroup } from "./types";

/** The query keys this view owns, which its URL write-back replaces wholesale. */
export const QUEUES_FILTER_KEYS: readonly string[] = ["host", "reason", "repo", "agent", "q", "group"];

const GROUPS: readonly QueuesGroup[] = ["none", "host", "reason", "repo"];

/** The filters a query string names: empty values dropped, an unknown or missing `group` read as "none". */
export function queuesFiltersFromSearch(search: string): QueuesFilters {
  const params = new URLSearchParams(search);
  const one = (key: string): string | undefined => params.get(key)?.trim() || undefined;
  const group = params.get("group");
  return {
    host: one("host"),
    reason: one("reason"),
    repo: one("repo"),
    agent: one("agent"),
    q: one("q"),
    group: group && (GROUPS as readonly string[]).includes(group) ? (group as QueuesGroup) : "none",
  };
}

/** The query string (with its `?`) for filters: empty values and the default `group` leave nothing behind. */
export function queuesSearchFromFilters(f: QueuesFilters): string {
  const params = new URLSearchParams();
  for (const key of ["host", "reason", "repo", "agent", "q"] as const) {
    const value = f[key]?.trim();
    if (value) params.set(key, value);
  }
  if (f.group && f.group !== "none") params.set("group", f.group);
  const s = params.toString();
  return s ? `?${s}` : "";
}
