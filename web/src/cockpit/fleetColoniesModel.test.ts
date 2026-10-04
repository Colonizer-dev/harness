// The fleet colony list's pure half (issue #689): the "waiting on" word, projecting a local Session
// and an imported history record onto one row, merging them newest-first, and the per-repo/host/day
// totals — including the member that has pushed nothing, which must still appear with zero colonies
// and an unmeasured (null) cost rather than a made-up $0.00.
import { describe, expect, it } from "vitest";

import type { FleetHistoryEntry, FleetHost, Session } from "../types";
import { dayKeyOf } from "./dash";
import { filterColonies, fromImported, fromSession, hostOptions, mergeFleetColonies, totalsBy, waitingOn } from "./fleetColoniesModel";

const session = (o: Partial<Session> = {}): Session => ({
  id: "s1", repo: "acme/webshop", org: "acme", issue: 42, issue_title: "Checkout fails for guest users",
  status: "running", branch: "b", base: "main", worktree: "/wt", git_admin_dir: null, sandbox: "sb",
  mesh: null, agent: "claude-code", autopilot: false, pr_url: null, error: null, cost_usd: null,
  cleaned_up: false, keep_worktree: false, created_at: "2026-09-18T09:00:00Z", updated_at: "2026-09-18T09:10:00Z",
  ...o,
});

const imported = (o: Partial<FleetHistoryEntry["record"]> = {}, e: Partial<FleetHistoryEntry> = {}): FleetHistoryEntry => {
  const id = o.id ?? "host-1:session-7";
  return {
    key: `mem_1/${id}`, member_id: "mem_1", member_name: "studio-2", member_removed: false, id, received_at: "2026-09-19T00:00:00Z",
    record: {
      id, origin_host: "host-1", original_id: "session-7", repo: "acme/api", org: "acme", issue: 7,
      issue_title: "Rate-limit the search endpoint", status: "merged", branch: "b", pr_url: null, merged_at: "2026-09-19T00:00:00Z",
      summary: null, error: null, cost_usd: null, agent: "claude", created_at: "2026-09-17T09:00:00Z", updated_at: "2026-09-19T00:00:00Z",
      ...o,
    },
    payloads: [], ...e,
  };
};

const host = (id: string, name: string): FleetHost => ({
  id, name, platform: "", os: "", version: null, slots_in_use: 0, slots_ceiling: 3, queue_depth: 0,
  disk_free_bytes: null, last_heartbeat: null, health: "online",
});

describe("waitingOn", () => {
  it("maps each waiting state to its one word", () => {
    expect(waitingOn(session({ status: "waiting_for_answer" }))).toBe("answer");
    expect(waitingOn(session({ status: "queued" }))).toBe("slot");
    expect(waitingOn(session({ status: "running", queued_behind: "s0" }))).toBe("slot");
    expect(waitingOn(session({ status: "running", claim_wait: true }))).toBe("slot");
    expect(waitingOn(session({ status: "parked" }))).toBe("quota");
    expect(waitingOn(session({ status: "running", parked: { at: "x", reason: "hold_timeout", vm_kept: true } }))).toBe("quota");
    expect(waitingOn(session({ status: "pr_opened", ci_state: "pending" }))).toBe("ci");
    expect(waitingOn(session({ status: "pr_opened", ci_state: "failure" }))).toBe("ci");
    expect(waitingOn(session({ status: "pr_opened", ci_state: "success" }))).toBe("review");
    expect(waitingOn(session({ status: "pr_opened" }))).toBe("review");
  });

  it("reads a working or finished colony as waiting on nothing, and an imported pr_opened row as review", () => {
    expect(waitingOn(session({ status: "running" }))).toBeNull();
    expect(waitingOn(session({ status: "merged" }))).toBeNull();
    expect(waitingOn(imported({ status: "pr_opened" }).record)).toBe("review");
    expect(waitingOn(imported({ status: "stopped" }).record)).toBeNull();
  });
});

describe("fromSession and fromImported", () => {
  it("projects a session with a local deep link, its org fallback and its summed cost", () => {
    const row = fromSession(session({ id: "s9", org: undefined, cost_usd: 1.5, routed_cost_usd: 0.25 }), "archlinux");
    expect(row).toMatchObject({ key: "s9", host: "archlinux", url: "?colony=s9", org: "acme", imported: false, costUsd: 1.75, day: dayKeyOf("2026-09-18T09:00:00Z") });
  });

  it("projects an entry with the member's name as host and a link to the member's cockpit", () => {
    const row = fromImported(imported({ cost_usd: 1, routed_cost_usd: 0.5 }), new Map([["mem_1", "https://studio.example:7878/"]]));
    expect(row).toMatchObject({ key: "mem_1/host-1:session-7", host: "studio-2", url: "https://studio.example:7878/?colony=session-7", imported: true, costUsd: 1.5 });
  });

  it("leaves a removed member unlinked, and falls back to origin_host without a member name", () => {
    const removed = fromImported(imported({}, { member_removed: true }), new Map([["mem_1", "https://studio.example"]]));
    expect(removed.url).toBeNull();
    expect(removed.removed).toBe(true);
    const noName = fromImported(imported({}, { member_name: "" }), new Map());
    expect(noName.host).toBe("host-1");
    expect(noName.url).toBeNull();
    expect(noName.costUsd).toBeNull();
  });
});

describe("mergeFleetColonies", () => {
  it("keeps local and imported rows, newest first, and dedupes by key", () => {
    const local = fromSession(session({ id: "s1", updated_at: "2026-09-18T10:00:00Z" }), "archlinux");
    const old = fromImported(imported({ id: "host-1:old", original_id: "old", updated_at: "2026-09-10T00:00:00Z" }), new Map());
    const newer = fromImported(imported({ id: "host-1:new", original_id: "new", updated_at: "2026-09-20T00:00:00Z" }), new Map());
    const merged = mergeFleetColonies([local], [old, newer, { ...local, host: "dupe" }]);
    expect(merged.map((c) => c.key)).toEqual(["mem_1/host-1:new", "s1", "mem_1/host-1:old"]);
    expect(merged.find((c) => c.key === "s1")!.host).toBe("archlinux");
  });
});

describe("totalsBy", () => {
  const a = fromSession(session({ id: "a", repo: "acme/web", cost_usd: 1 }), "archlinux");
  const b = fromSession(session({ id: "b", repo: "acme/web", cost_usd: null, routed_cost_usd: null, created_at: "2026-09-19T09:00:00Z" }), "archlinux");
  const c = fromImported(imported({ cost_usd: 2.5 }), new Map());

  it("adds colonies and cost per repo, keeping a fully unmeasured bucket null", () => {
    expect(totalsBy([a, b], "repo")).toEqual([{ key: "acme/web", colonies: 2, costUsd: 1 }]);
    expect(totalsBy([c], "repo")).toEqual([{ key: "acme/api", colonies: 1, costUsd: 2.5 }]);
  });

  it("seeds per-host totals so a member with no data shows 0 colonies and a null cost", () => {
    const hosts = totalsBy([a, b], "host", ["archlinux", "studio-2"]);
    expect(hosts.find((t) => t.key === "archlinux")).toEqual({ key: "archlinux", colonies: 2, costUsd: 1 });
    expect(hosts.find((t) => t.key === "studio-2")).toEqual({ key: "studio-2", colonies: 0, costUsd: null });
  });

  it("buckets per day, newest first", () => {
    // `b` is exactly a day newer than `a`, so their local-calendar days differ by one in any zone.
    expect(totalsBy([a, b], "day").map((t) => t.key)).toEqual([dayKeyOf("2026-09-19T09:00:00Z"), dayKeyOf("2026-09-18T09:00:00Z")]);
  });
});

describe("filterColonies and hostOptions", () => {
  const a = fromSession(session({ id: "a", repo: "acme/web", org: "acme" }), "archlinux");
  const b = fromImported(imported({ repo: "globex/api", org: "globex" }), new Map());

  it("filters by host, org and repo; empty means all", () => {
    expect(filterColonies([a, b], {})).toHaveLength(2);
    expect(filterColonies([a, b], { org: "globex" })).toEqual([b]);
    expect(filterColonies([a, b], { host: "archlinux", repo: "acme/web" })).toEqual([a]);
    expect(filterColonies([a, b], { host: "nobody" })).toEqual([]);
  });

  it("offers every known host plus any an imported row names", () => {
    expect(hostOptions([a], [host("self", "archlinux"), host("peer", "studio-2")])).toEqual(["archlinux", "studio-2"]);
  });
});
