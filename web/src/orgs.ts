// The org workspace list's pure half (issue #176): which orgs are workspaces, which are switched
// off, and which newly-appeared one the UI should be asking about. The components render; this
// module decides what is in the list and in what order. Nothing here touches the browser.
import { orgOf, occupiesSlot } from "./components/ui";
import type { OrgInfo, OrgSettings, OrgSpend, Session } from "./types";

/** Absent, null and true all mean on — nothing changes for an existing install; only an explicit false is off. */
export function orgEnabled(settings: OrgSettings | undefined): boolean {
  return settings?.enabled !== false;
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
