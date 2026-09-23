// The realtime dashboard feed (issue #446): frame parsing, session ordering, poll gating,
// backoff, and the LiveStream's open → drop → reconnect cycle against a fake socket.
import { afterEach, describe, expect, it, vi } from "vitest";

import type { SocketLike } from "./api";
import { POLL_TICK_NAMES, type PollTickName } from "./pollSchedule";
import {
  LiveStream,
  parseStreamFrame,
  removeSessionById,
  shouldPollWhileLive,
  streamBackoffMs,
  upsertSession,
  type LiveConnection,
  type LiveHandlers,
} from "./liveStream";
import type { FleetHost, OrgInfo, Session, StorageSummary } from "./types";

function session(overrides: Partial<Session> = {}): Session {
  return {
    id: "s1",
    repo: "acme/webshop",
    org: "acme",
    issue: 42,
    issue_title: "Checkout fails for guest users",
    status: "running",
    branch: "colonizer/issue-42-s1",
    base: "main",
    parent: null,
    worktree: "/wt/s1",
    git_admin_dir: "/git/s1",
    sandbox: "colony-s1",
    mesh: null,
    agent: "claude-code",
    autopilot: false,
    pr_url: null,
    error: null,
    cost_usd: null,
    cleaned_up: false,
    keep_worktree: false,
    created_at: "2026-09-18T09:00:00Z",
    updated_at: "2026-09-18T09:10:00Z",
    attention: null,
    ...overrides,
  };
}

const org = (name: string): OrgInfo => ({
  org: name,
  colonies: { live: 1, total: 2 },
  pending_memory: 0,
  settings: { enabled: true },
});

const host = (id: string): FleetHost => ({
  id,
  name: id,
  platform: "linux-x86_64",
  os: "Arch",
  version: "0.1.5",
  slots_in_use: 1,
  slots_ceiling: 3,
  queue_depth: 0,
  disk_free_bytes: null,
  last_heartbeat: null,
  health: "online",
});

const storage = (): StorageSummary => ({
  enabled: true,
  retention_secs: 43200,
  min_free_bytes: 1,
  warn_free_bytes: 5,
  free_bytes: 100,
  admission_paused: false,
  totals: { worktrees_bytes: 1, repos_bytes: 2, sessions_bytes: 3, microsandbox_bytes: null },
  reclaimable: [],
  unpushed: [],
  orphans: [],
});

describe("parseStreamFrame", () => {
  it("accepts every documented frame shape", () => {
    const s = session();
    expect(parseStreamFrame({ type: "sessions", sessions: [s] })).toEqual({ type: "sessions", sessions: [s] });
    expect(parseStreamFrame({ type: "session", session: s })).toEqual({ type: "session", session: s });
    expect(parseStreamFrame({ type: "session_removed", id: "s1" })).toEqual({ type: "session_removed", id: "s1" });
    expect(parseStreamFrame({ type: "orgs", orgs: [org("acme")] })).toEqual({ type: "orgs", orgs: [org("acme")] });
    expect(parseStreamFrame({ type: "hosts", hosts: [host("h1")] })).toEqual({ type: "hosts", hosts: [host("h1")] });
    expect(parseStreamFrame({ type: "storage", storage: storage() })).toEqual({ type: "storage", storage: storage() });
  });

  it("ignores unknown types and malformed frames", () => {
    for (const junk of [
      null,
      "sessions",
      { type: "status" },
      { type: "sessions" },
      { type: "sessions", sessions: {} },
      { type: "session" },
      { type: "session", session: { repo: "acme/webshop" } },
      { type: "session_removed" },
      { type: "session_removed", id: 7 },
      { type: "orgs", orgs: null },
      { type: "hosts", hosts: "h1" },
      { type: "storage" },
    ]) {
      expect(parseStreamFrame(junk)).toBeNull();
    }
  });
});

describe("session list helpers", () => {
  it("upsert replaces in place and never moves a known colony", () => {
    const list = [session({ id: "a" }), session({ id: "b" })];
    const next = upsertSession(list, session({ id: "b", status: "merged" }));
    expect(next.map((s) => s.id)).toEqual(["a", "b"]);
    expect(next[1].status).toBe("merged");
  });

  it("a brand-new session goes to the front, like a fresh GET /api/sessions", () => {
    const list = [session({ id: "a" }), session({ id: "b" })];
    expect(upsertSession(list, session({ id: "new" })).map((s) => s.id)).toEqual(["new", "a", "b"]);
    expect(upsertSession([], session({ id: "only" })).map((s) => s.id)).toEqual(["only"]);
  });

  it("remove drops by id and keeps the list when the id is unknown", () => {
    const list = [session({ id: "a" }), session({ id: "b" })];
    expect(removeSessionById(list, "a").map((s) => s.id)).toEqual(["b"]);
    expect(removeSessionById(list, "zzz")).toBe(list);
  });
});

describe("shouldPollWhileLive", () => {
  const connections: LiveConnection[] = ["connecting", "open", "reconnecting"];
  it("gates exactly the stream-fed ticks while open", () => {
    for (const tick of ["sessions", "orgs", "fleet"] as PollTickName[]) {
      expect(shouldPollWhileLive(tick, "open")).toBe(false);
      for (const connection of ["connecting", "reconnecting"] as LiveConnection[]) {
        expect(shouldPollWhileLive(tick, connection)).toBe(true);
      }
    }
  });

  it("never gates status, redRuns, pendingMemory or update", () => {
    for (const tick of POLL_TICK_NAMES) {
      if (["sessions", "orgs", "fleet"].includes(tick)) continue;
      for (const connection of connections) expect(shouldPollWhileLive(tick, connection)).toBe(true);
    }
  });

  it("backs off 1s, 2s, 4s … capped at 10s", () => {
    expect([0, 1, 2, 3, 4, 10].map(streamBackoffMs)).toEqual([1000, 2000, 4000, 8000, 10000, 10000]);
  });

  it("a poll resolving after the stream reopened is stale and must be dropped", () => {
    // The gated loaders re-check this at resolution time: open means the stream owns the tick.
    for (const tick of ["sessions", "orgs", "fleet"] as PollTickName[]) {
      expect(shouldPollWhileLive(tick, "open")).toBe(false);
    }
  });
});

// ---------------------------------------------------------------------------
// LiveStream against a fake socket
// ---------------------------------------------------------------------------

class FakeSocket {
  onopen: ((event: Event) => unknown) | null = null;
  onmessage: ((event: MessageEvent) => unknown) | null = null;
  onclose: ((event: CloseEvent) => unknown) | null = null;
  onerror: ((event: Event) => unknown) | null = null;
  closed = false;

  open(): void {
    this.onopen?.({} as Event);
  }
  deliver(data: string): void {
    this.onmessage?.({ data } as MessageEvent);
  }
  drop(): void {
    this.onclose?.({} as CloseEvent);
  }
}

function harness() {
  const sockets: FakeSocket[] = [];
  const seen = { connection: [] as LiveConnection[], drops: 0, sessions: [] as Session[][], removed: [] as string[], storage: 0, orgs: 0, hosts: 0 };
  const handlers: LiveHandlers = {
    onSessions: (sessions) => seen.sessions.push(sessions),
    onSession: (s) => seen.sessions.push([s]),
    onSessionRemoved: (id) => seen.removed.push(id),
    onOrgs: () => (seen.orgs += 1),
    onHosts: () => (seen.hosts += 1),
    onStorage: () => (seen.storage += 1),
    onConnection: (connection) => seen.connection.push(connection),
    onDrop: () => (seen.drops += 1),
  };
  const stream = new LiveStream(
    () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket as unknown as SocketLike;
    },
    handlers,
  );
  return { stream, sockets, seen };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("LiveStream", () => {
  it("opens, dispatches frames, and ignores junk without sending anything", () => {
    vi.useFakeTimers();
    const { stream, sockets, seen } = harness();
    stream.start();
    expect(sockets).toHaveLength(1);
    sockets[0].open();
    expect(seen.connection).toEqual(["open"]);

    const s = session();
    sockets[0].deliver(JSON.stringify({ type: "sessions", sessions: [s] }));
    sockets[0].deliver(JSON.stringify({ type: "session_removed", id: "s1" }));
    sockets[0].deliver(JSON.stringify({ type: "storage", storage: storage() }));
    sockets[0].deliver("not json");
    sockets[0].deliver(JSON.stringify({ type: "status", ok: true }));
    sockets[0].deliver(JSON.stringify({ type: "session" }));
    expect(seen.sessions).toEqual([[s]]);
    expect(seen.removed).toEqual(["s1"]);
    expect(seen.storage).toBe(1);
    stream.stop();
  });

  it("a drop reconnects with backoff, refreshes once, and re-gates on reopen", () => {
    vi.useFakeTimers();
    const { stream, sockets, seen } = harness();
    stream.start();
    sockets[0].open();
    expect(shouldPollWhileLive("sessions", "open")).toBe(false);

    sockets[0].drop();
    expect(seen.connection).toEqual(["open", "reconnecting"]);
    expect(seen.drops).toBe(1);
    // Polls resume while the stream is down.
    expect(shouldPollWhileLive("sessions", "reconnecting")).toBe(true);

    // First reconnect waits 1s.
    vi.advanceTimersByTime(999);
    expect(sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(2);
    // The second socket drops before it opens: no new open → reconnecting transition, so no
    // second refresh — but retries only reset on open, so the reconnect waits 2s.
    sockets[1].drop();
    expect(seen.drops).toBe(1);
    expect(seen.connection).toEqual(["open", "reconnecting"]);
    vi.advanceTimersByTime(1999);
    expect(sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(3);
    sockets[2].open();
    expect(seen.connection).toEqual(["open", "reconnecting", "open"]);
    expect(shouldPollWhileLive("fleet", "open")).toBe(false);
    stream.stop();
  });

  it("stale sockets and stop() schedule nothing", () => {
    vi.useFakeTimers();
    const { stream, sockets, seen } = harness();
    stream.start();
    sockets[0].open();
    sockets[0].drop();
    vi.advanceTimersByTime(1000);
    expect(sockets).toHaveLength(2);
    // The dead socket's late open is ignored.
    sockets[0].open();
    expect(seen.connection).toEqual(["open", "reconnecting"]);
    stream.stop();
    vi.advanceTimersByTime(30_000);
    expect(sockets).toHaveLength(2);
  });
});
