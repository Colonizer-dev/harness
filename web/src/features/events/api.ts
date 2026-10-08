// Realtime sockets API — split out of src/api.ts (issue #827).
// The root `Api` interface composes this with the other features.
import { enc, query, request, wsUrl } from "../../http";
import type { SocketLike } from "../../http";
import type { EventsPage } from "./types";

export interface EventsApi {
  /** `limit` asks for the newest page only (issue #1210); the older events come from `eventsPage`. */
  openEvents(sessionId: string, since: number, epoch?: number, limit?: number): SocketLike;
  /** GET /api/sessions/{id}/events: the newest page, or the one before the `before` cursor. */
  eventsPage(sessionId: string, page: { before?: number; epoch?: number; offset?: number; limit?: number }): Promise<EventsPage>;
  /** GET /api/stream: the dashboard's realtime feed (issue #446); same-origin cookie auth, like openEvents. */
  openStream(): SocketLike;
}

export const eventsHttp: EventsApi = {
  openEvents: (id, since, epoch = 0, limit) =>
    new WebSocket(wsUrl(`/api/sessions/${enc(id)}/events?since=${since}&epoch=${epoch}${limit ? `&limit=${limit}` : ""}`)),
  eventsPage: (id, page) =>
    request<EventsPage>(
      `/api/sessions/${enc(id)}/events${query({
        before: page.before?.toString(),
        epoch: page.epoch?.toString(),
        offset: page.offset?.toString(),
        limit: page.limit?.toString(),
      })}`,
    ),
  openStream: () => new WebSocket(wsUrl("/api/stream")),
};
