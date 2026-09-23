// The dashboard's realtime feed (issue #446): one WebSocket to GET /api/stream pushing the lists
// the poll ticks fetch today. While open, the sessions/orgs/fleet ticks skip; when it drops those
// ticks fetch as before (that IS the fallback). Frames mirror the GET bodies; the client never sends.
import type { SocketLike } from "./api";
import type { PollTickName } from "./pollSchedule";
import type { FleetHost, OrgInfo, Session, StorageSummary } from "./types";

export type LiveConnection = "connecting" | "open" | "reconnecting";

/** Ticks the stream owns while open; status, redRuns, pendingMemory and update always poll. */
const LIVE_TICKS: ReadonlySet<PollTickName> = new Set(["sessions", "orgs", "fleet"]);

/** False while the stream is open and owns this tick — the poll skips its fetch. True otherwise. */
export function shouldPollWhileLive(tick: PollTickName, connection: LiveConnection): boolean {
  return connection !== "open" || !LIVE_TICKS.has(tick);
}

/** Reconnect delay after `retries` drops: 1s, 2s, 4s … capped at 10s. Exported for the tests. */
export function streamBackoffMs(retries: number): number {
  return Math.min(1000 * 2 ** retries, 10_000);
}

/** Matches GET /api/sessions: the server reverses insertion order (newest created first), so a
 *  brand-new session goes to the front and a known one is replaced where it stands. */
export function upsertSession(list: Session[], session: Session): Session[] {
  const at = list.findIndex((s) => s.id === session.id);
  if (at >= 0) {
    const next = list.slice();
    next[at] = session;
    return next;
  }
  return [session, ...list];
}

export function removeSessionById(list: Session[], id: string): Session[] {
  return list.some((s) => s.id === id) ? list.filter((s) => s.id !== id) : list;
}

export type StreamFrame =
  | { type: "sessions"; sessions: Session[] }
  | { type: "session"; session: Session }
  | { type: "session_removed"; id: string }
  | { type: "orgs"; orgs: OrgInfo[] }
  | { type: "hosts"; hosts: FleetHost[] }
  | { type: "storage"; storage: StorageSummary };

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

/** Validates one parsed text frame; null means junk or an unknown type — ignore it. */
export function parseStreamFrame(data: unknown): StreamFrame | null {
  if (!isRecord(data) || typeof data.type !== "string") return null;
  switch (data.type) {
    case "sessions":
      return Array.isArray(data.sessions) ? { type: "sessions", sessions: data.sessions as Session[] } : null;
    case "session":
      return isRecord(data.session) && typeof data.session.id === "string"
        ? { type: "session", session: data.session as unknown as Session }
        : null;
    case "session_removed":
      return typeof data.id === "string" ? { type: "session_removed", id: data.id } : null;
    case "orgs":
      return Array.isArray(data.orgs) ? { type: "orgs", orgs: data.orgs as OrgInfo[] } : null;
    case "hosts":
      return Array.isArray(data.hosts) ? { type: "hosts", hosts: data.hosts as FleetHost[] } : null;
    case "storage":
      return isRecord(data.storage) ? { type: "storage", storage: data.storage as unknown as StorageSummary } : null;
    default:
      return null;
  }
}

export interface LiveHandlers {  onSessions(sessions: Session[]): void;
  onSession(session: Session): void;
  onSessionRemoved(id: string): void;
  onOrgs(orgs: OrgInfo[]): void;
  onHosts(hosts: FleetHost[]): void;
  onStorage(storage: StorageSummary): void;
  onConnection(connection: LiveConnection): void;
  /** Fires once per open → reconnecting transition so App refreshes before the next tick. */
  onDrop(): void;
}

/** One shared /api/stream socket, modelled on SessionStream: injected factory, backoff reconnect. */
export class LiveStream {
  private ws: SocketLike | null = null;
  private retries = 0;
  private stopped = false;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private connection: LiveConnection = "connecting";

  constructor(
    private readonly openSocket: () => SocketLike,
    private readonly handlers: LiveHandlers,
  ) {}

  start(): void {
    this.stopped = false;
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    const ws = this.ws;
    this.ws = null;
    try {
      ws?.close();
    } catch {
      /* a dead stub must not break unmount */
    }
  }

  private setConnection(next: LiveConnection): void {
    const prev = this.connection;
    this.connection = next;
    if (prev !== next) {
      this.handlers.onConnection(next);
      if (prev === "open" && next === "reconnecting") this.handlers.onDrop();
    }
  }

  private connect(): void {
    this.setConnection(this.retries > 0 ? "reconnecting" : "connecting");
    const ws = this.openSocket();
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.retries = 0;
      this.setConnection("open");
    };
    ws.onmessage = (event) => {
      if (this.ws !== ws || typeof event.data !== "string") return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.data);
      } catch {
        return;
      }
      const frame = parseStreamFrame(parsed);
      if (!frame) return;
      const h = this.handlers;
      switch (frame.type) {
        case "sessions":
          h.onSessions(frame.sessions);
          break;
        case "session":
          h.onSession(frame.session);
          break;
        case "session_removed":
          h.onSessionRemoved(frame.id);
          break;
        case "orgs":
          h.onOrgs(frame.orgs);
          break;
        case "hosts":
          h.onHosts(frame.hosts);
          break;
        case "storage":
          h.onStorage(frame.storage);
          break;
      }
    };
    ws.onerror = () => {};
    ws.onclose = () => {
      if (this.ws !== ws || this.stopped) return;
      this.ws = null;
      this.setConnection("reconnecting");
      const delay = streamBackoffMs(this.retries);
      this.retries += 1;
      this.timer = setTimeout(() => {
        if (!this.stopped) this.connect();
      }, delay);
    };
  }
}
