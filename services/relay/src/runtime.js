// The two Workers-only spots in the tunnel, behind a seam the tests overwrite (Node has no WebSocketPair and
// cannot build a 101 Response). See test/fakes.mjs for the stand-ins.

export const runtime = {
  // A fresh pair for a proxied browser socket, [client, server] like WebSocketPair itself: we keep and
  // accept the server side, and hand the client side to upgrade() for the 101.
  pair() {
    const p = new WebSocketPair();
    return [p[0], p[1]];
  },
  // Accepts the server side, handing the socket to the browser in the 101.
  upgrade(client) {
    return new Response(null, { status: 101, webSocket: client });
  },
};
