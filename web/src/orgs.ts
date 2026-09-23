// The org workspace list's pure half (issue #176): which orgs are workspaces, which are switched
// off, and which newly-appeared one the UI should be asking about. The components render; this
// module decides what is in the list and in what order. Nothing here touches the browser.
import type { CockpitView } from "./cockpit/Header";
import { orgOf, occupiesSlot, sameOrg } from "./components/ui";
import type { OrgInfo, OrgSettings, OrgSpend, Session } from "./types";

/** Absent, null and true all mean on — nothing changes for an existing install; only an explicit false is off. */
export function orgEnabled(settings: OrgSettings | undefined): boolean {
  return settings?.enabled !== false;
}

/**
 * The design's "Hide orgs with no colonies" toggle, persisted client-side under the house
 * `colonizer.*` naming. On matches the design's default; only an explicit "0" is off, so a
 * stored blob from a newer build never hides orgs for an existing install by accident.
 */
export const HIDE_EMPTY_ORGS_KEY = "colonizer.hideEmptyOrgs";

export function parseHideEmptyOrgs(raw: string | null): boolean {
  return raw !== "0";
}

export function serializeHideEmptyOrgs(hide: boolean): string {
  return hide ? "1" : "0";
}

/**
 * The toggle applied to the workspace list: with it on, entries with no colonies in the live
 * list are hidden from the overview, the rail and the totals. The design hides on "no colony
 * launched in the last 30 days", but the mothership serves no launch history — only the live
 * list — so empty means zero sessions there. Takes either the visible or the hidden split.
 */
export function hideEmptyOrgEntries(entries: OrgEntry[], hideEmpty: boolean): OrgEntry[] {
  return hideEmpty ? entries.filter((e) => e.total > 0) : entries;
}

/** One row of the sidebar's org switcher, merged from GET /api/orgs and the colony list. */
export interface OrgEntry {
  org: string;
  live: number;
  queued: number;
  total: number;
  pending: number;
  /** The org's avatar, when /api/orgs knows one; null for an org that only appears in the colony list. */
  avatar: string | null;
  /** The org's spend from /api/orgs; absent on a mothership that does not measure it. */
  spend?: OrgSpend;
}

/**
 * Orgs from GET /api/orgs plus any org that only appears in the colony list; counts come from the
 * live list. An org switched off (`enabled: false`) is not a workspace choice, but it stays in
 * `hidden` with its counts so the switcher's disclosure can still reach its settings; one still
 * awaiting a decision is in neither — it is not a workspace until the operator says so, and the
 * prompt card is where that happens.
 */
export function orgEntries(orgs: OrgInfo[], sessions: Session[]): { visible: OrgEntry[]; hidden: OrgEntry[] } {
  const byKey = new Map<string, OrgEntry>();
  const off = new Set<string>();
  const entry = (org: string, avatar: string | null) => {
    const key = org.toLowerCase();
    let found = byKey.get(key);
    if (!found) byKey.set(key, (found = { org, live: 0, queued: 0, total: 0, pending: 0, avatar }));
    else if (avatar && !found.avatar) found.avatar = avatar;
    return found;
  };
  for (const info of orgs) {
    if (info.awaiting_decision === true) continue;
    const e = entry(info.org, info.avatar_url ?? null);
    e.pending = info.pending_memory ?? 0;
    if (info.spend !== undefined) e.spend = info.spend;
    if (!orgEnabled(info.settings)) off.add(e.org.toLowerCase());
  }
  for (const session of sessions) {
    const org = orgOf(session);
    if (!org) continue;
    const e = entry(org, null);
    e.total += 1;
    if (session.status === "queued") e.queued += 1;
    else if (occupiesSlot(session.status)) e.live += 1;
  }
  const visible: OrgEntry[] = [];
  const hidden: OrgEntry[] = [];
  for (const e of [...byKey.values()].sort((a, b) => a.org.localeCompare(b.org))) {
    (off.has(e.org.toLowerCase()) ? hidden : visible).push(e);
  }
  return { visible, hidden };
}

/**
 * The one pending org to ask about now, or null. An org already answered this session is skipped —
 * the PUT that answered it has marked it decided server-side, but the 15 s poll may not have
 * confirmed that yet, and its card must not flash back in between. The API carries no timestamp
 * for a pending org, so the rest are asked in name order: stable, and never a wall of cards.
 */
export function pendingOrgPrompt(orgs: OrgInfo[], answered: ReadonlySet<string>): OrgInfo | null {
  const done = new Set([...answered].map((org) => org.toLowerCase()));
  return (
    orgs
      .filter((info) => info.awaiting_decision === true && !done.has(info.org.toLowerCase()))
      .sort((a, b) => a.org.localeCompare(b.org))[0] ?? null
  );
}

// The cockpit's org selection (the rail and the header switcher). The components hold no rules of
// their own about it; what a click means, what survives a reload and which view survives a switch
// are decided here.

/** Clicking the org already selected clears the filter back to every workspace; any other click selects it. */
export function toggledOrg(selected: string | null, clicked: string | null): string | null {
  return clicked === null || sameOrg(selected, clicked) ? null : clicked;
}

/**
 * The selection to keep once the org list is known. The stored "colonizer.org" can name an org
 * that has since gone (its colonies forgotten) or been switched off; left alone it filters the nest
 * to nothing while the rail highlights nothing and the header reads "colonizer". A selection that
 * is not a workspace is cleared. `keepHidden` is for the narrow sidebar, whose switcher shows a
 * switched-off selection as the current org and reaches its settings from there; the cockpit has
 * no such row, so it passes false. The entry's own spelling is returned, so later strict
 * comparisons against the list cannot miss on case.
 */
export function reconcileSelectedOrg(
  selected: string | null,
  entries: { visible: OrgEntry[]; hidden: OrgEntry[] },
  keepHidden: boolean,
): string | null {
  if (!selected) return null;
  const pool = keepHidden ? [...entries.visible, ...entries.hidden] : entries.visible;
  return pool.find((e) => sameOrg(e.org, selected))?.org ?? null;
}

/**
 * The memory item's badge: proposals waiting for review. With an org chosen it is that org's
 * count from /api/orgs; across every workspace it is the global count, which also carries the
 * proposals that belong to no org.
 */
export function memoryBadge(selected: string | null, workspaces: OrgEntry[], total: number): number {
  if (!selected) return total;
  return workspaces.find((e) => sameOrg(e.org, selected))?.pending ?? 0;
}

/**
 * The cockpit view to land on after switching org. The views that read the chosen org — the nest,
 * history, launch and memory — stay put, so switching org from history shows the new org's
 * history; the others are not about any one org (the overview, the cross-workspace inbox,
 * settings) or not about this one (an open colony), so they fall back to the nest.
 */
export function viewAfterOrgSwitch(view: CockpitView): CockpitView {
  return view === "home" || view === "history" || view === "launch" || view === "memory" ? view : "home";
}
