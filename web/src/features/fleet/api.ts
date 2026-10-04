// Fleet API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { del, enc, post, query, request, ApiError } from "../../http";
import type { CreatedFleetInvite, FleetHistoryDetail, FleetHistoryPage, FleetHistoryQuery, FleetHost, FleetJoinRequest, FleetJoinStatus, FleetMember, FleetState, FleetSyncPreview, FleetSyncStatus } from "./types";

export interface FleetApi {
  /** GET /api/hosts (issue #231): self plus every peer configured via COLONIZER_FLEET_PEERS, polled live on each call. */
  hosts(): Promise<{ hosts: FleetHost[] }>;
  /** GET /api/fleet (issue #686, docs/fleet.md): this mothership's role and everything the Fleet pane renders, in one view. */
  fleet(): Promise<FleetState>;
  /** POST /api/fleet/invites: mints a single-use invite; its code is shown once. 409 while this mothership is itself in a fleet. */
  createFleetInvite(): Promise<CreatedFleetInvite>;
  /** DELETE /api/fleet/invites/{id}: revokes an open invite before it is redeemed. */
  deleteFleetInvite(id: string): Promise<void>;
  /** POST /api/fleet/pending/{id}/approve: admits the joining machine; the answer's member carries a fleet-scoped token on the joining side. */
  approveFleetPending(id: string): Promise<{ member: FleetMember }>;
  /** POST /api/fleet/pending/{id}/reject: turns the request down; the joiner's next confirm reads `rejected`. */
  rejectFleetPending(id: string): Promise<void>;
  /** DELETE /api/fleet/members/{id}: ends one membership — the member's fleet token is revoked, its local data stays. */
  removeFleetMember(id: string): Promise<void>;
  /** POST /api/fleet/join: redeems the owner's invite; both screens then show the answer's `confirm_code`. 409 while already a member, or an owner with members. */
  joinFleet(body: FleetJoinRequest): Promise<{ confirm_code: string; status: "pending" }>;
  /** POST /api/fleet/join/confirm: asks whether the owner has decided; `pending` means wait and try again. */
  confirmFleetJoin(): Promise<{ status: FleetJoinStatus }>;
  /** DELETE /api/fleet/join: cancels an in-progress join. */
  cancelFleetJoin(): Promise<void>;
  /** POST /api/fleet/leave: ends this mothership's own membership; every local colony and setting stays. */
  leaveFleet(): Promise<void>;
  /** GET /api/fleet/sync/preview (issue #762): what the history push would send; sends nothing. 409 when not a member. */
  fleetSyncPreview(): Promise<FleetSyncPreview>;
  /** POST /api/fleet/sync/consent: turns the history push on or off for this membership. 409 when not a member. */
  setFleetHistorySync(enabled: boolean): Promise<FleetSyncStatus>;
  /** GET /api/fleet/history (issue #762, owner-only): the members' synced colonies, filtered and paged, with totals. */
  fleetHistory(q?: FleetHistoryQuery): Promise<FleetHistoryPage>;
  /** GET /api/fleet/history/{member}/{row_id}: one synced colony's record and its logs. */
  fleetHistoryEntry(member: string, rowId: string): Promise<FleetHistoryDetail>;
  /** GET /api/fleet/history/{member}/{row_id}/logs/{name}: one stored log, as text. */
  fleetHistoryLog(member: string, rowId: string, name: string): Promise<string>;
}

export const fleetHttp: FleetApi = {
  hosts: () => request("/api/hosts"),
  fleet: () => request("/api/fleet"),
  createFleetInvite: () => post("/api/fleet/invites"),
  deleteFleetInvite: (id) => del(`/api/fleet/invites/${enc(id)}`),
  approveFleetPending: (id) => post(`/api/fleet/pending/${enc(id)}/approve`),
  rejectFleetPending: (id) => post(`/api/fleet/pending/${enc(id)}/reject`),
  removeFleetMember: (id) => del(`/api/fleet/members/${enc(id)}`),
  joinFleet: (body) => post("/api/fleet/join", body),
  confirmFleetJoin: () => post("/api/fleet/join/confirm"),
  cancelFleetJoin: () => del("/api/fleet/join"),
  leaveFleet: () => post("/api/fleet/leave"),
  fleetSyncPreview: () => request("/api/fleet/sync/preview"),
  setFleetHistorySync: (enabled) => post("/api/fleet/sync/consent", { enabled }),
  fleetHistory: (q = {}) =>
    request(`/api/fleet/history${query({ member: q.member, repo: q.repo, status: q.status, since: q.since, until: q.until, limit: q.limit?.toString(), cursor: q.cursor })}`),
  fleetHistoryEntry: (member, rowId) => request(`/api/fleet/history/${enc(member)}/${enc(rowId)}`),
  fleetHistoryLog: async (member, rowId, name) => {
    // A log is text: never JSON-parsed, even when it is a single JSON line.
    const res = await fetch(`/api/fleet/history/${enc(member)}/${enc(rowId)}/logs/${enc(name)}`);
    const text = await res.text();
    if (!res.ok) throw new ApiError(text || res.statusText, res.status);
    return text;
  },
};
