// The `events` feature's mock methods and fixtures, split out of src/mock.ts (issue #827).
// Shared state lives in src/mockState.ts; shared helpers in src/mockShared.ts.
import { MockSocket } from "../../mockShared";
import type { SocketLike } from "../../http";
import type { MockState } from "../../mockState";
import type { EventsApi } from "./api";

export function eventsMock(ms: MockState): EventsApi {
  return {
    openEvents: (id, since) => {
      const session = ms.sessions.get(id);
      const socket = new MockSocket({
    open: (s) => {
      if (!session) {
        s.close();
        return;
      }
      session.attach(s, since);
    },
    message: (_s, data) => session?.command(data),
    close: (s) => session?.detach(s),
      });
      return socket as unknown as SocketLike;
    },
    openStream: () =>
      ({
    binaryType: "blob",
    readyState: 0,
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
    send: () => {},
    close: () => {},
      }) as unknown as SocketLike
  };
}
