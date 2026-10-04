// The fleet's colonies in one list (issue #689): this host's own colonies (live from the session
// stream) beside the finished colonies members pushed to the owner (issue #762). A local `Session`
// and an imported `FleetHistoryRecord` are both projected onto one `FleetColony` row here, along with
// the "waiting on" word, the per-repo/host/day totals and the filters — all pure, so the component
// stays markup and the tests stay pure. In-flight colonies on another member are not available
// anywhere: a member pushes only finished colonies, so an imported row is always done.
import { orgOf } from "../components/ui";
import { sessionCost, sumCosts } from "../spend";
import { dayKeyOf } from "./dash";
import type { CiState, FleetHistoryEntry, FleetHistoryRecord, FleetHost, Session, SessionStatus } from "../types";

/** What a colony is waiting on, in one word. */
export type WaitingOn = "answer" | "slot" | "quota" | "ci" | "review";

/** The fields `waitingOn` reads. A `Session` supplies them all; an imported record omits
 *  `queued_behind`/`claim_wait`/`parked`/`ci_state`, so its `pr_opened` rows read `review`. */
export interface WaitingInput {
  status: SessionStatus;
  queued_behind?: string | null;
  claim_wait?: boolean;
  parked?: unknown | null;
  ci_state?: CiState | null;
}

/** Why a colony is not progressing, or null when it is working or done. `parked` is tokens whatever
 *  the reason: a provider's plan is out, or the autopilot hold timed out. */
export function waitingOn(s: WaitingInput): WaitingOn | null {
  if (s.status === "waiting_for_answer") return "answer";
  if (s.status === "queued" || s.queued_behind || s.claim_wait) return "slot";
  if (s.status === "parked" || s.parked) return "quota";
  if (s.status === "pr_opened") return s.ci_state === "pending" || s.ci_state === "failure" ? "ci" : "review";
  return null;
}

/** One row of the fleet colony list, whichever host it came from. */
export interface FleetColony {
  /** Local: the session id. Imported: the history entry's key (`<member_id>/<row id>`). */
  key: string;
  host: string;
  /** Deep link to open the colony, or null when there is none (a removed member, or one with no URL). */
  url: string | null;
  repo: string;
  org: string;
  issue: number | null;
  title: string;
  status: SessionStatus;
  waitingOn: WaitingOn | null;
  /** Measured spend, null when nothing was ever measured (rendered "—", never "$0.00"). */
  costUsd: number | null;
  /** Local-calendar "YYYY-MM-DD" of `created_at`. */
  day: string;
  updatedAt: string;
  imported: boolean;
  /** An imported row whose member has since been removed. */
  removed: boolean;
}

/** This host's own colony; the link is the `?colony=` deep link the cockpit reads on load. */
export function fromSession(s: Session, hostName: string): FleetColony {
  return {
    key: s.id,
    host: hostName,
    url: `?colony=${encodeURIComponent(s.id)}`,
    repo: s.repo,
    org: orgOf(s),
    issue: s.issue,
    title: s.issue_title,
    status: s.status,
    waitingOn: waitingOn(s),
    costUsd: sessionCost(s),
    day: dayKeyOf(s.created_at),
    updatedAt: s.updated_at,
    imported: false,
    removed: false,
  };
}

/** A colony a member finished and pushed. `memberUrls` maps a member id to its cockpit URL; a
 *  removed member, or one that gave no URL, leaves the row unlinked. */
export function fromImported(entry: FleetHistoryEntry, memberUrls: Map<string, string | null>): FleetColony {
  const r: FleetHistoryRecord = entry.record;
  const base = entry.member_removed ? null : memberUrls.get(entry.member_id) ?? null;
  return {
    key: entry.key,
    host: entry.member_name || r.origin_host,
    url: base ? `${base.replace(/\/+$/, "")}/?colony=${encodeURIComponent(r.original_id)}` : null,
    repo: r.repo,
    org: r.org,
    issue: r.issue,
    title: r.issue_title,
    status: r.status,
    waitingOn: waitingOn(r),
    costUsd: sumCosts([r.cost_usd ?? null, r.routed_cost_usd ?? null]),
    day: dayKeyOf(r.created_at),
    updatedAt: r.updated_at,
    imported: true,
    removed: entry.member_removed,
  };
}

/** Local and imported rows in one list, deduped by key (a local row wins a clash) and newest first. */
export function mergeFleetColonies(local: FleetColony[], imported: FleetColony[]): FleetColony[] {
  const byKey = new Map<string, FleetColony>();
  for (const c of local) byKey.set(c.key, c);
  for (const c of imported) if (!byKey.has(c.key)) byKey.set(c.key, c);
  return [...byKey.values()].sort((a, b) => (Date.parse(b.updatedAt) || 0) - (Date.parse(a.updatedAt) || 0) || a.key.localeCompare(b.key));
}

export type TotalKind = "repo" | "host" | "day";

/** One bucket of a total: how many colonies and their summed cost (null when none was measured). */
export interface FleetTotal {
  key: string;
  colonies: number;
  costUsd: number | null;
}

/** Totals per repo, host or day. A host bucket is seeded from `seed` (the known hosts) so a member
 *  that pushed nothing still appears with zero colonies and a null cost. Sorted alphabetically,
 *  except days, newest first. */
export function totalsBy(colonies: FleetColony[], kind: TotalKind, seed: string[] = []): FleetTotal[] {
  const groups = new Map<string, FleetColony[]>();
  if (kind === "host") for (const key of seed) if (!groups.has(key)) groups.set(key, []);
  for (const c of colonies) {
    const key = kind === "repo" ? c.repo : kind === "host" ? c.host : c.day;
    const list = groups.get(key);
    if (list) list.push(c);
    else groups.set(key, [c]);
  }
  return [...groups.entries()]
    .map(([key, list]) => ({ key, colonies: list.length, costUsd: sumCosts(list.map((c) => c.costUsd)) }))
    .sort((a, b) => (kind === "day" ? b.key.localeCompare(a.key) : a.key.localeCompare(b.key)));
}

/** One filter choice per field; empty or null means "all". */
export interface FleetFilter {
  host?: string | null;
  org?: string | null;
  repo?: string | null;
}

/** Narrow the list to the chosen host, org and repo. */
export function filterColonies(colonies: FleetColony[], f: FleetFilter): FleetColony[] {
  return colonies.filter((c) => (!f.host || c.host === f.host) && (!f.org || c.org === f.org) && (!f.repo || c.repo === f.repo));
}

/** Every known host, plus any an imported row names. */
export function hostOptions(colonies: FleetColony[], hosts: FleetHost[]): string[] {
  return [...new Set([...hosts.map((h) => h.name), ...colonies.map((c) => c.host)])].sort((a, b) => a.localeCompare(b));
}
