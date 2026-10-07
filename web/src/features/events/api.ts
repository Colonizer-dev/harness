// Realtime sockets API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { enc, wsUrl } from "../../http";
import type { SocketLike } from "../../http";

export interface EventsApi {
  openEvents(sessionId: string, since: number, epoch?: number): SocketLike;
  /** GET /api/stream: the dashboard's realtime feed (issue #446); same-origin cookie auth, like openEvents. */
  openStream(): SocketLike;
}

export const eventsHttp: EventsApi = {
  openEvents: (id, since, epoch = 0) => new WebSocket(wsUrl(`/api/sessions/${enc(id)}/events?since=${since}&epoch=${epoch}`)),
  openStream: () => new WebSocket(wsUrl("/api/stream")),
};
