// The `fleet` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { ago, clone, isLive, now, sleep } from "../../mockShared";
import type { CreatedFleetInvite, FleetHistoryPage, FleetHost, FleetMember, FleetSyncPreview, FleetSyncStatus } from "../../types";
import { ApiError } from "../../http";
import type { MockState } from "../../mockState";
import type { FleetApi } from "./api";

/** GET /api/hosts (issue #231): self, plus a demo peer so the fleet panel has something to show
 * in the mock — one reachable, one not, so the online/unreachable distinction is visible without a
 * real second machine. */
export function mockFleet(live: number): { hosts: FleetHost[] } {
  return {
    hosts: [
      {
        id: "1e6f2a84-c5b3-4f2a-9f1c-8d4e2a1b6c90",
        name: "archlinux",
        platform: "linux-x86_64",
        os: "Arch Linux",
        version: "0.1.5",
        slots_in_use: live,
        slots_ceiling: 3,
        queue_depth: 0,
        disk_free_bytes: 176_093_659_136, // 164G
        last_heartbeat: now(),
        health: "online",
      },
      {
        id: "https://colony-2.example.internal:9443",
        name: "https://colony-2.example.internal:9443",
        platform: "linux-x86_64",
        os: "Debian GNU/Linux",
        version: "0.1.4",
        slots_in_use: 1,
        slots_ceiling: 2,
        queue_depth: 0,
        disk_free_bytes: 12_884_901_888, // 12G
        last_heartbeat: ago(38),
        health: "unreachable",
      },
    ],
  };
}

export function fleetMock(ms: MockState): FleetApi {
  return {
    hosts: () => ms.later(() => mockFleet([...ms.sessions.values()].filter((s) => isLive(s.session.status)).length)),
    fleet: () =>
      ms.later(() => ({
        role: ms.fleetRole(),
        invites: ms.fleetInvites.map(clone),
        pending: ms.fleetPending.map(clone),
        members: ms.fleetMembers.map(clone),
        membership: ms.fleetMembership ? clone(ms.fleetMembership) : null,
        joining: ms.fleetJoining ? clone(ms.fleetJoining) : null,
      })),
    createFleetInvite: async () => {
      await sleep(250);
      if (ms.fleetRole() !== "owner") throw new ApiError("only a fleet owner creates invites", 409);
      const invite: CreatedFleetInvite = { id: `inv_${ms.mockId()}`, code: ms.mockInviteCode(), expires_at: new Date(Date.now() + 15 * 60_000).toISOString() };
      ms.fleetInvites.push({ id: invite.id, expires_at: invite.expires_at });
      return invite;
    },
    deleteFleetInvite: async (id) => {
      await sleep(200);
      const at = ms.fleetInvites.findIndex((row) => row.id === id);
      if (at < 0) throw new ApiError("no such invite", 404);
      ms.fleetInvites.splice(at, 1);
    },
    approveFleetPending: async (id) => {
      await sleep(250);
      const at = ms.fleetPending.findIndex((row) => row.id === id);
      if (at < 0) throw new ApiError("no such pending request", 404);
      const [row] = ms.fleetPending.splice(at, 1);
      const member: FleetMember = {
        id: row.id,
        name: row.name,
        url: row.url,
        joined_at: now(),
        health: { state: "unknown", code: "not_checked", reason: "Not checked yet", hint: "open the cockpit or wait for the next poll" },
      };
      ms.fleetMembers.push(member);
      return { member: clone(member) };
    },
    rejectFleetPending: async (id) => {
      await sleep(200);
      const at = ms.fleetPending.findIndex((row) => row.id === id);
      if (at < 0) throw new ApiError("no such pending request", 404);
      ms.fleetPending.splice(at, 1);
    },
    removeFleetMember: async (id) => {
      await sleep(250);
      const at = ms.fleetMembers.findIndex((row) => row.id === id);
      if (at < 0) throw new ApiError("no such member", 404);
      ms.fleetMembers.splice(at, 1);
    },
    joinFleet: async (body) => {
      await sleep(300);
      // The server's rules in its order: already in a fleet is the 409, a bad/used/expired code is
      // one 404 that names nothing (docs/fleet.md).
      if (ms.fleetRole() !== "none") throw new ApiError("this mothership is already in a fleet", 409);
      if (!body?.owner_url?.trim() || !body.code?.trim()) throw new ApiError("owner_url and code are required", 400);
      if (body.code.trim().length < 16) throw new ApiError("no such invite", 404);
      ms.fleetJoining = { owner_url: body.owner_url.trim(), confirm_code: String(Math.floor(100_000 + Math.random() * 900_000)), started_at: now() };
      return { confirm_code: ms.fleetJoining.confirm_code, status: "pending" as const };
    },
    confirmFleetJoin: async () => {
      await sleep(250);
      if (!ms.fleetJoining) throw new ApiError("not joining any fleet", 404);
      if (Date.now() - Date.parse(ms.fleetJoining.started_at) > 6000) {
        // The simulated owner has approved: the pairing completes and the fleet token arrives on
        // the joining side, where nothing here reads it.
        ms.fleetMembership = { owner_url: ms.fleetJoining.owner_url, member_id: `mem_${ms.mockId()}`, joined_at: now(), history_sync: false };
        ms.fleetJoining = null;
        return { status: "joined" as const };
      }
      return { status: "pending" as const };
    },
    cancelFleetJoin: async () => {
      await sleep(150);
      ms.fleetJoining = null;
    },
    leaveFleet: async () => {
      await sleep(250);
      if (!ms.fleetMembership) throw new ApiError("not a member of any fleet", 409);
      ms.fleetMembership = null;
    },
    fleetSyncPreview: async (): Promise<FleetSyncPreview> => {
      await sleep(200);
      if (!ms.fleetMembership) throw new ApiError("this mothership has not joined a fleet", 409);
      return {
        owner_url: ms.fleetMembership.owner_url,
        colonies: 42,
        payloads: 118,
        payload_bytes: 37 * 1024 ** 2,
        omitted_payloads: 0,
        row_bytes: 96 * 1024,
        total_bytes: 37 * 1024 ** 2 + 96 * 1024,
        pending_colonies: ms.fleetMembership.history_sync ? 0 : 42,
        pending_bytes: ms.fleetMembership.history_sync ? 0 : 37 * 1024 ** 2 + 96 * 1024,
        includes: "each finished colony's record and its event, harness and gateway logs",
        excludes: "running colonies, transcripts, stats, settings, secrets and tokens",
      };
    },
    setFleetHistorySync: async (enabled): Promise<FleetSyncStatus> => {
      await sleep(200);
      if (!ms.fleetMembership) throw new ApiError("this mothership has not joined a fleet", 409);
      ms.fleetMembership = { ...ms.fleetMembership, history_sync: enabled };
      return {
        member: true,
        consent: enabled,
        enabled,
        status: enabled ? "synced" : "consent_required",
        detail: null,
        acknowledged: enabled ? 42 : 0,
        retired: [],
        last_drain_at: enabled ? now() : null,
        last_synced_at: enabled ? now() : null,
        next_attempt_at: null,
      };
    },
    fleetHistory: async (q = {}): Promise<FleetHistoryPage> => {
      await sleep(200);
      const day = (v: string | undefined, end: boolean) => (v ? Date.parse(v.length === 10 ? `${v}T${end ? "23:59:59.999" : "00:00:00"}Z` : v) : null);
      const since = day(q.since, false);
      const until = day(q.until, true);
      const hits = ms.fleetHistoryRows.filter((r) => {
        const at = Date.parse(r.record.updated_at);
        return (!q.member || r.member_id === q.member) && (!q.repo || r.record.repo === q.repo) && (!q.status || r.record.status === q.status) &&
          (since == null || at >= since) && (until == null || at <= until);
      });
      const start = q.cursor ? hits.findIndex((r) => r.key === q.cursor) + 1 : 0;
      if (q.cursor && start === 0) throw new ApiError("`cursor` names no entry in this list", 400);
      const end = Math.min(start + (q.limit ?? 20), hits.length);
      const members = [...new Map(ms.fleetHistoryRows.map((r) => [r.member_id, { id: r.member_id, name: r.member_name, removed: r.member_removed }])).values()];
      return clone({
        colonies: hits.slice(start, end),
        next_cursor: end < hits.length ? hits[end - 1].key : null,
        stats: {
          total: ms.fleetTotals(hits),
          members: members.map((m) => ({ member_id: m.id, name: m.name, removed: m.removed, ...ms.fleetTotals(hits.filter((r) => r.member_id === m.id)) })).filter((m) => m.colonies > 0),
          repos: [...new Set(hits.map((r) => r.record.repo))].sort().map((repo) => ({ repo, ...ms.fleetTotals(hits.filter((r) => r.record.repo === repo)) })),
        },
        members,
        repos: [...new Set(ms.fleetHistoryRows.map((r) => r.record.repo))].sort(),
        retention_days: 90,
      });
    },
    fleetHistoryEntry: async (member, rowId) => {
      await sleep(150);
      const row = ms.fleetHistoryRows.find((r) => r.member_id === member && r.id === rowId);
      if (!row) throw new ApiError("no such fleet history entry", 404);
      return clone({ ...row, logs: row.payloads.map((p) => ({ ...p, omitted: false, stored: true })) });
    },
    fleetHistoryLog: async (member, rowId, name) => {
      await sleep(150);
      const row = ms.fleetHistoryRows.find((r) => r.member_id === member && r.id === rowId);
      if (!row || !row.payloads.some((p) => p.name === name)) throw new ApiError("this colony has no such stored log", 404);
      return `{"type":"status","status":"running"}\n{"type":"status","status":"${row.record.status}"}\n`;
    }
  };
}
