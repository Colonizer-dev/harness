// The workspace switcher's pure half (issue #1228): which workspaces sit under Pinned, Recent and
// All, and how a search narrows them. Pins and recents live in this browser only.
import { sameOrg } from "../components/ui";
import type { OrgEntry } from "../orgs";

export const PINNED_ORGS_KEY = "colonizer.pinnedOrgs";
export const RECENT_ORGS_KEY = "colonizer.recentOrgs";
/** How many recently chosen workspaces the menu remembers. */
export const RECENT_ORGS_MAX = 3;

export function loadOrgList(raw: string | null): string[] {
  try {
    const parsed: unknown = JSON.parse(raw ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Newest first, no repeats (case-insensitively), a few at most. */
export function pushRecentOrg(list: readonly string[], org: string): string[] {
  return [org, ...list.filter((x) => !sameOrg(x, org))].slice(0, RECENT_ORGS_MAX);
}

export function togglePinned(list: readonly string[], org: string): string[] {
  return list.some((x) => sameOrg(x, org)) ? list.filter((x) => !sameOrg(x, org)) : [...list, org];
}

export interface WorkspaceGroups {
  pinned: OrgEntry[];
  recent: OrgEntry[];
  all: OrgEntry[];
  /** Switched off: not a choice, but the way back to switching one on. */
  off: OrgEntry[];
}

/**
 * The menu's groups. Without a search a workspace appears once: pinned first (in pin order), then
 * the recent ones that are not pinned, then everyone else. With a search there is one flat list of
 * matches, so a result is never hidden behind a heading.
 */
export function workspaceGroups(orgs: readonly OrgEntry[], hidden: readonly OrgEntry[], pinned: readonly string[], recent: readonly string[], query: string): WorkspaceGroups {
  const q = query.trim().toLowerCase();
  const fits = (o: OrgEntry) => o.org.toLowerCase().includes(q);
  if (q) return { pinned: [], recent: [], all: orgs.filter(fits), off: hidden.filter(fits) };
  const pick = (names: readonly string[], taken: ReadonlySet<string>) =>
    names.flatMap((n) => {
      const found = orgs.find((o) => sameOrg(o.org, n));
      return found && !taken.has(found.org) ? [found] : [];
    });
  const taken = new Set<string>();
  const p = pick(pinned, taken);
  p.forEach((o) => taken.add(o.org));
  const r = pick(recent, taken);
  r.forEach((o) => taken.add(o.org));
  return { pinned: p, recent: r, all: orgs.filter((o) => !taken.has(o.org)), off: [...hidden] };
}
