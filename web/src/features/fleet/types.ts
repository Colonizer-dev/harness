import type { SessionStatus } from "../sessions/types";

/** Whether a fleet host answered the mothership's live poll (issue #231). */
export type FleetHostHealth = "online" | "unreachable";

/**
 * GET /api/hosts (issue #231): every host the mothership knows about — itself, always first and
 * always `online`, plus each peer configured via `COLONIZER_FLEET_PEERS`, polled live on every
 * request. An unreachable peer never disappears from the list: it keeps its last-known cached
 * stats (with `health: "unreachable"`) once it has answered before, or comes back as a bare
 * placeholder — `id`/`name` are its configured URL, `platform`/`os` are `""`, everything else is
 * `null`/`0` — if it has never been reached at all.
 */
export interface FleetHost {
  id: string;
  name: string;
  platform: string;
  os: string;
  /** Absent on an older peer build, or a peer never reached. */
  version: string | null;
  slots_in_use: number;
  slots_ceiling: number;
  queue_depth: number;
  /** Absent when the peer has never reported it. */
  disk_free_bytes: number | null;
  /** The disk's size (issue #764); absent when unknown. */
  disk_total_bytes?: number;
  /** RFC3339; null when the peer has never answered. */
  last_heartbeat: string | null;
  health: FleetHostHealth;
}

// ---------------------------------------------------------------------------
// Fleet pairing (issue #686): GET/POST /api/fleet… (docs/fleet.md). A mothership joins another's
// fleet like phone pairing: the owner mints a single-use invite, the joiner redeems it, both
// screens show the same six-digit confirmation code, and the owner approves what they see.
// ---------------------------------------------------------------------------

/** GET /api/fleet `role`: where this mothership stands — a fleet owner with members, a member of someone else's fleet, or in neither. */
export type FleetRole = "owner" | "member" | "none";

/** One open invite. The code itself is shown once at creation (POST /api/fleet/invites) and stored only as a hash. */
export interface FleetInvite {
  id: string;
  /** RFC3339-ish; an invite lives 15 minutes. */
  expires_at: string;
}

/** A machine that redeemed an invite and now waits for the owner's decision. */
export interface FleetPending {
  id: string;
  /** The name the joiner gave itself. */
  name: string;
  /** The joiner's own URL, when it gave one; null when it did not. */
  url: string | null;
  /** The six digits both screens must show, "123 456". */
  confirm_code: string;
  expires_at: string;
  status: "pending" | "approved" | "rejected";
}

/** GET /api/fleet member `health.state` (issue #764): the worst of what the owner observed; `unknown` until a poll has checked the member. */
export type FleetMemberHealthState = "ok" | "unknown" | "degraded" | "stopped";

/**
 * GET /api/fleet member `health` (issue #764): one state, and when it is not `ok`, why and what to do.
 * `code` is a stable key (`token_revoked`, `no_heartbeat`, `unreachable`, `disk_full`, `unwatched`, `not_checked`, …);
 * `reason` and `hint` are for showing verbatim, e.g. "No heartbeat for 12 min" / "the machine may be asleep".
 */
export interface FleetMemberHealth {
  state: FleetMemberHealthState;
  code: string | null;
  reason: string | null;
  hint: string | null;
  /** Something worth knowing that is not a fault, whatever the state: "History sync off". Absent from an older owner. */
  note?: string | null;
}

/** A mothership that joined this one's fleet; it hosts colonies and sees the fleet view. */
export interface FleetMember {
  id: string;
  name: string;
  url: string | null;
  joined_at: string;
  /** Absent from owners built before issue #764. */
  health?: FleetMemberHealth;
}

/** GET /api/fleet `membership`: this mothership's place in the fleet it joined. */
export interface FleetMembership {
  owner_url: string;
  member_id: string;
  joined_at: string;
  /** Whether this machine's operator consented to pushing its history to the owner (issue #762). Off at every join. */
  history_sync: boolean;
}

/** GET /api/fleet/sync/preview (issue #762): what turning the history push on would send — read from the same collection the push sends. */
export interface FleetSyncPreview {
  owner_url: string;
  /** Finished colonies, one row each. */
  colonies: number;
  /** Their log files, and those files' bytes. */
  payloads: number;
  payload_bytes: number;
  /** Logs over the size limit: named on their row, never sent. */
  omitted_payloads: number;
  row_bytes: number;
  total_bytes: number;
  /** What the owner has not acknowledged yet. */
  pending_colonies: number;
  pending_bytes: number;
  includes: string;
  excludes: string;
}

/** GET /api/fleet/sync's `status`: where the history push stands. */
export type FleetSyncState = "idle" | "synced" | "backoff" | "unauthorized" | "removed" | "error" | "consent_required";

/** GET /api/fleet/sync, and POST /api/fleet/sync/consent's answer. */
export interface FleetSyncStatus {
  member: boolean;
  consent: boolean;
  enabled: boolean;
  status: FleetSyncState;
  detail: string | null;
  acknowledged: number;
  retired: { id: string; error: string; attempts: number; at: string }[];
  last_drain_at: string | null;
  last_synced_at: string | null;
  next_attempt_at: string | null;
}

// Fleet history (issue #762, the owner's view): what members pushed with history sync on, read
// back from <data_dir>/fleet-ingest/ by GET /api/fleet/history… (docs/fleet.md). Owner-only.

/** A synced colony's record: the allowlist projection the member sent (`ImportedSession`). */
export interface FleetHistoryRecord {
  /** `<origin_host>:<original_id>`. */
  id: string;
  origin_host: string;
  original_id: string;
  repo: string;
  org: string;
  issue: number | null;
  issue_title: string;
  status: SessionStatus;
  branch: string;
  base?: string | null;
  pr_url: string | null;
  pr_opened_at?: string | null;
  merged_at: string | null;
  summary: string | null;
  error: string | null;
  cost_usd: number | null;
  model_tier?: string | null;
  agent: string;
  created_at: string;
  updated_at: string;
}

/** One log a synced colony carries; `omitted` was too large to travel. */
export interface FleetHistoryPayload {
  name: string;
  sha256: string;
  bytes: number;
  omitted?: boolean;
}

/** One synced colony in GET /api/fleet/history. `key` (`<member_id>/<row id>`) is its cursor. */
export interface FleetHistoryEntry {
  key: string;
  member_id: string;
  member_name: string;
  /** The member was removed from the fleet; its history stays. */
  member_removed: boolean;
  id: string;
  received_at: string;
  record: FleetHistoryRecord;
  payloads: FleetHistoryPayload[];
}

/** Totals over the filtered history; `cost_usd` is null when no row carried a cost. */
export interface FleetHistoryTotals {
  colonies: number;
  merged: number;
  cost_usd: number | null;
}

/** GET /api/fleet/history's filters and page; every field optional. Dates are YYYY-MM-DD or RFC 3339, on the colony's finish. */
export interface FleetHistoryQuery {
  member?: string;
  repo?: string;
  status?: string;
  since?: string;
  until?: string;
  limit?: number;
  cursor?: string;
}

/** GET /api/fleet/history: one page, newest finish first, the totals over every filtered row, and the filter options. */
export interface FleetHistoryPage {
  colonies: FleetHistoryEntry[];
  next_cursor: string | null;
  stats: {
    total: FleetHistoryTotals;
    members: (FleetHistoryTotals & { member_id: string; name: string; removed: boolean })[];
    repos: (FleetHistoryTotals & { repo: string })[];
  };
  members: { id: string; name: string; removed: boolean }[];
  repos: string[];
  retention_days: number;
}

/** GET /api/fleet/history/{member}/{row_id}: the entry, and each log with whether the owner holds it. */
export interface FleetHistoryDetail extends FleetHistoryEntry {
  logs: (FleetHistoryPayload & { omitted: boolean; stored: boolean })[];
}

/** GET /api/fleet `joining`: a join this mothership started and has not finished; both screens show `confirm_code` until the owner decides. */
export interface FleetJoining {
  owner_url: string;
  confirm_code: string;
  started_at: string;
}

/** GET /api/fleet: everything the Fleet settings pane renders, in one view. */
export interface FleetState {
  role: FleetRole;
  invites: FleetInvite[];
  pending: FleetPending[];
  members: FleetMember[];
  membership: FleetMembership | null;
  joining: FleetJoining | null;
}

/** POST /api/fleet/invites' answer: the invite code, shown exactly once — the registry keeps only its hash. */
export interface CreatedFleetInvite {
  id: string;
  code: string;
  expires_at: string;
}

/** POST /api/fleet/join: what the join form collects. `name` and `url` are optional labels for the owner's member list. */
export interface FleetJoinRequest {
  owner_url: string;
  code: string;
  name?: string;
  url?: string;
}

/** POST /api/fleet/join/confirm's answer: `pending` until the owner decides, then `joined` — or the decision, bad news both other ways. */
export type FleetJoinStatus = "joined" | "pending" | "rejected" | "expired";
