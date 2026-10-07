// The mock's per-call state slice for the fleet feature (issue #827). The one shared state object
// (MockState in src/mockState.ts) carries these fields so a reassignment is seen by every feature.
import type { FleetHistoryEntry, FleetHistoryTotals, FleetInvite, FleetJoining, FleetMember, FleetMembership, FleetPending, FleetRole } from "../../types";
import { ago } from "../../mockShared";
import type { MockState } from "../../mockState";

export type FleetMockState = {
    fleetInvites: FleetInvite[];
    fleetPending: FleetPending[];
    fleetMembers: FleetMember[];
    fleetMembership: FleetMembership | null;
    fleetJoining: FleetJoining | null;
    fleetHistoryRows: FleetHistoryEntry[];
    fleetTotals: (rows: FleetHistoryEntry[]) => FleetHistoryTotals;
    fleetRole: () => FleetRole;
    mockInviteCode: () => string;
};

export function installFleetMockState(ms: MockState): void {
  // Fleet pairing (issue #686, docs/fleet.md): the mock starts as a fleet owner — one member, one
  // machine mid-pairing, one open invite — so the owner side of the Fleet pane has all three lists
  // filled. Removing the last member drops the role to "none", which unlocks the join form; the
  // simulated owner approves a join six seconds in, so "Codes match" can be watched going from
  // pending to joined without a second browser. As anywhere, the invite's code is handed out once.
  ms.fleetInvites = [{ id: "inv_seed1", expires_at: new Date(Date.now() + 9 * 60_000).toISOString() }];
  ms.fleetPending = [
    { id: "pen_seed1", name: "rfc-annex", url: "http://10.0.0.6:7878", confirm_code: "512849", expires_at: new Date(Date.now() + 11 * 60_000).toISOString(), status: "pending" },
  ];
  // Issue #764: the seeded member shows a degraded badge, so the demo has something to point at.
  ms.fleetMembers = [
    {
      id: "mem_seed1",
      name: "studio-2",
      url: "http://10.0.0.5:7878",
      joined_at: ago(3 * 1440),
      health: { state: "degraded", code: "no_heartbeat", reason: "No heartbeat for 12 min", hint: "the machine may be asleep" },
    },
  ];
  ms.fleetMembership = null;
  ms.fleetJoining = null;
  // Issue #762: what the members synced, as the owner's history view reads it. One entry comes
  // from a member that was since removed, so the demo shows the "removed" marker too.
  const fleetHistoryRow = (n: number, member: [string, string, boolean], repo: string, status: "merged" | "pr_opened" | "failed", cost: number | null): FleetHistoryEntry => {
    const host = member[1];
    const id = `${host}:session-${n}`;
    return {
      key: `${member[0]}/${id}`, member_id: member[0], member_name: member[1], member_removed: member[2], id, received_at: ago(n * 700),
      record: {
        id, origin_host: host, original_id: `session-${n}`, repo, org: repo.split("/")[0], issue: 100 + n, issue_title: `Synced colony ${n}`, status,
        branch: `colonizer/issue-${100 + n}`, pr_url: status === "failed" ? null : `https://github.com/${repo}/pull/${n}`,
        merged_at: status === "merged" ? ago(n * 720) : null, summary: `Finished on ${host}.`, error: status === "failed" ? "tests failed" : null,
        cost_usd: cost, agent: "claude", created_at: ago(n * 760), updated_at: ago(n * 720),
      },
      payloads: [{ name: "events.jsonl", sha256: "0".repeat(63) + String(n % 10), bytes: 2048 * n }],
    };
  };
  ms.fleetHistoryRows = [
    fleetHistoryRow(1, ["mem_seed1", "studio-2", false], "acme/web", "merged", 1.25),
    fleetHistoryRow(2, ["mem_seed1", "studio-2", false], "acme/api", "pr_opened", 0.8),
    fleetHistoryRow(3, ["mem_gone", "old-laptop", true], "acme/web", "merged", null),
    fleetHistoryRow(4, ["mem_seed1", "studio-2", false], "acme/web", "failed", 0.3),
  ];
  ms.fleetTotals = (rows: FleetHistoryEntry[]): FleetHistoryTotals => {
    const costs = rows.map((r) => r.record.cost_usd).filter((c): c is number => c != null);
    return { colonies: rows.length, merged: rows.filter((r) => r.record.status === "merged").length, cost_usd: costs.length ? costs.reduce((a, b) => a + b, 0) : null };
  };
  ms.fleetRole = (): FleetRole => (ms.fleetMembership ? "member" : ms.fleetMembers.length > 0 ? "owner" : "none");
  ms.mockInviteCode = () => Array.from({ length: 16 }, () => "abcdefghijklmnopqrstuvwxyz234567"[Math.floor(Math.random() * 32)]).join(""); // 80 bits
}
