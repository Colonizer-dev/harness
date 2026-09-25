// The relay tunnel: one InstallTunnel Durable Object per install. The mothership dials in over a WebSocket
// and proves it holds the install's key (challenge/hello); the cockpit's requests arrive on the same object
// as ordinary requests tagged x-relay-kind: proxy. Every request is then replayed to the mothership as JSON
// frames (protocol v1, see the issue) and the answer streamed back. Nothing is persisted: request and
// response bodies live only in memory, never in state.storage.

import { b64decode, b64encode, randomToken, verifyEd25519 } from './crypto.js';
import { CHUNK_DECODED_MAX, CHUNK_RAW, MAX_PENDING, MAX_STREAMS, PING_MS, TS_SKEW, helloMessage, pathTemplate, stripHopByHop } from './protocol.js';
import { offlinePage } from './pages.js';
import { runtime } from './runtime.js';

const DEFAULTS = {
  pingMs: PING_MS,
  helloTimeoutMs: 10000,
  responseTimeoutMs: 60000,
  idleTimeoutMs: 60000,
  streamIdleMs: 300000,
  maxBufferedBytes: 8 * 1024 * 1024,
  rate: { capacity: 120, perSecond: 20 },
};

const ENCODER = new TextEncoder();
const byteLength = (data) => (typeof data === 'string' ? ENCODER.encode(data).length : data.byteLength ?? data.length);

// Largest base64 string that can still decode into the inbound limit; longer chunks are rejected unread.
const CHUNK_B64_MAX = Math.ceil(CHUNK_DECODED_MAX / 3) * 4;

const textResponse = (status, text, headers = {}) =>
  new Response(`${text}\n`, { status, headers: { 'content-type': 'text/plain; charset=utf-8', ...headers } });

export class InstallTunnel {
  // opts shortens the timers in tests only; production uses the DEFAULTS above.
  constructor(state, env, opts = {}) {
    this.state = state; // deliberately never touched: no storage, bodies stay in memory
    this.env = env;
    this.o = { ...DEFAULTS, ...opts };
    const rate = { ...DEFAULTS.rate, ...opts.rate };
    this.rate = { ...rate, tokens: rate.capacity, at: Date.now() };
    this.tunnel = null; // the verified mothership socket: { ws, installId }
    this.pending = new Set(); // the dials still mid-handshake; independent of each other and of the tunnel
    this.streams = new Map(); // id -> HTTP stream (replaced mid-flight, failed, or finishing)
    this.sockets = new Map(); // id -> WS passthrough
    this.nextId = 1;
    this.pingTimer = null;
    this.idleTimer = null;
  }

  async fetch(request) {
    if (request.headers.get('x-relay-kind') === 'tunnel') return this.#dial(request);
    return this.#proxy(request);
  }

  // ---- mothership side ------------------------------------------------------------------

  // A tunnel dial: make a socket pair, keep the server end, challenge, and wait for a signed hello. A bad
  // handshake closes the socket 1008 and leaves any live tunnel alone. Pending handshakes are independent
  // — each has its own nonce and timeout, and a new dial never closes another — but only MAX_PENDING may
  // be open at once, so a re-dial loop cannot pin unbounded memory before proving the key.
  #dial(request) {
    const [client, ws] = runtime.pair();
    if (this.pending.size >= MAX_PENDING) {
      ws.accept(); // workerd wants a socket accepted before it can be closed
      this.#close(ws, 1013, 'try again later');
      return runtime.upgrade(client);
    }
    const hello = {
      ws,
      installId: request.headers.get('x-relay-install-id') ?? '',
      publicKey: request.headers.get('x-relay-public-key') ?? '',
      nonce: randomToken(32),
      settled: false,
    };
    this.pending.add(hello);
    ws.accept();
    ws.addEventListener('message', (ev) => this.#helloFrame(hello, ev));
    ws.addEventListener('close', () => {
      clearTimeout(hello.timer);
      this.pending.delete(hello);
    });
    hello.timer = setTimeout(() => {
      if (!this.pending.delete(hello)) return;
      this.#close(ws, 1008, 'hello timeout');
    }, this.o.helloTimeoutMs);
    this.#sendTo(ws, { t: 'challenge', nonce: hello.nonce });
    return runtime.upgrade(client);
  }

  async #helloFrame(hello, ev) {
    if (!this.pending.has(hello) || hello.settled) return;
    let frame;
    try {
      frame = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (frame.t !== 'hello') {
      hello.settled = true;
      clearTimeout(hello.timer);
      this.pending.delete(hello);
      this.#close(hello.ws, 1008, 'expected hello');
      return;
    }
    hello.settled = true;
    clearTimeout(hello.timer);
    this.pending.delete(hello);
    const stale = typeof frame.ts !== 'number' || Math.abs(Date.now() / 1000 - frame.ts) > TS_SKEW;
    const ok = frame.version === 1 && !stale && (await verifyEd25519(hello.publicKey, helloMessage(hello.nonce, hello.installId, frame.ts), frame.sig));
    if (!ok) return this.#close(hello.ws, 1008, 'bad hello');
    this.#establish(hello.ws, hello.installId);
  }

  // A verified tunnel replaces the old one; the old socket's in-flight streams fail.
  #establish(ws, installId) {
    if (this.tunnel) this.#killTunnel(this.tunnel, 4000, 'replaced');
    this.tunnel = { ws, installId };
    ws.addEventListener('message', (ev) => this.#tunnelFrame(ev));
    ws.addEventListener('close', () => {
      if (this.tunnel?.ws === ws) this.#killTunnel(this.tunnel, 1006, 'closed');
    });
    this.pingTimer = setInterval(() => this.#send({ t: 'ping' }), this.o.pingMs);
    this.#resetIdle();
  }

  // Drop the tunnel and everything riding on it. code 4000 = replaced, 1006 = socket closed, 1000 = idle.
  #killTunnel(tunnel, code, reason) {
    clearInterval(this.pingTimer);
    clearTimeout(this.idleTimer);
    this.pingTimer = this.idleTimer = null;
    if (this.tunnel === tunnel) this.tunnel = null;
    this.#close(tunnel.ws, code, reason);
    for (const s of this.streams.values()) s.responded ? this.#failBody(s) : this.#failPending(s, 502);
    for (const s of this.sockets.values()) {
      this.#close(s.server, 1012, 'tunnel replaced');
      this.#releaseWs(s);
    }
  }

  #tunnelFrame(ev) {
    this.#resetIdle();
    let frame;
    try {
      frame = JSON.parse(ev.data);
    } catch {
      return; // malformed frames are ignored, not fatal
    }
    if (frame.t === 'ping') return this.#send({ t: 'pong' });
    if (frame.t === 'res') return this.#onRes(frame);
    if (frame.t === 'body') return this.#onBody(frame);
    if (frame.t === 'ws_msg' || frame.t === 'ws_close') return this.#onWsFrame(frame);
    // pong and anything unknown: ignored
  }

  #onRes(frame) {
    const s = this.streams.get(frame.id);
    if (!s || s.responded) return; // unknown or late id
    const status = frame.status;
    if (!Number.isInteger(status) || status < 200 || status > 599) return this.#failPending(s, 502);
    clearTimeout(s.timer);
    s.responded = true;
    s.status = status;
    const headers = stripHopByHop(frame.headers);
    if (s.method === 'HEAD' || status === 204 || status === 304) {
      this.#release(s);
      return s.resolve(new Response(null, { status, headers }));
    }
    const body = new ReadableStream(
      {
        start: (c) => {
          s.push = (bytes) => {
            c.enqueue(bytes);
            return c.desiredSize >= 0; // false once the unread queue has grown past maxBufferedBytes
          };
          s.endBody = () => c.close();
          s.errorBody = () => c.error(new Error('tunnel closed'));
        },
        cancel: () => this.#release(s),
      },
      new ByteLengthQueuingStrategy({ highWaterMark: this.o.maxBufferedBytes }),
    );
    this.#armBodyIdle(s);
    s.resolve(new Response(body, { status, headers }));
  }

  #onBody(frame) {
    const s = this.streams.get(frame.id);
    if (!s || !s.responded || s.done) return; // unknown, late, or a headless-body frame
    if (typeof frame.chunk !== 'string' || frame.chunk.length > CHUNK_B64_MAX) return this.#failBody(s);
    let bytes;
    try {
      bytes = b64decode(frame.chunk);
    } catch {
      return this.#failBody(s);
    }
    if (bytes.length > CHUNK_DECODED_MAX) return this.#failBody(s);
    s.bytesOut += bytes.length;
    if (bytes.length && !s.push(bytes)) return this.#failBody(s); // the reader never drained it
    this.#armBodyIdle(s); // every inbound frame buys the stream another streamIdleMs
    if (frame.end) {
      s.endBody();
      this.#release(s);
    }
  }

  #onWsFrame(frame) {
    const s = this.sockets.get(frame.id);
    if (!s) return;
    if (frame.t === 'ws_close') {
      // workerd throws on 1005, 1006 and out-of-range codes, which would leave the browser socket open.
      const code = frame.code;
      this.#close(s.server, code === 1000 || (code >= 3000 && code <= 4999) ? code : 1000, '');
      return this.#releaseWs(s);
    }
    // A frame we cannot relay (garbage or over the size bound) must not vanish silently: close the
    // passthrough 1009 so both ends see an explicit end instead of a missing message.
    if (frame.binary) {
      if (typeof frame.data !== 'string' || frame.data.length > CHUNK_B64_MAX) return this.#closeWsPassthrough(s);
      let bytes;
      try {
        bytes = b64decode(frame.data);
      } catch {
        return this.#closeWsPassthrough(s);
      }
      if (bytes.length > CHUNK_DECODED_MAX) return this.#closeWsPassthrough(s);
      s.bytesOut += bytes.length;
      return this.#trySendRaw(s.server, bytes);
    }
    if (typeof frame.data !== 'string') return this.#closeWsPassthrough(s);
    s.bytesOut += byteLength(frame.data);
    this.#trySendRaw(s.server, frame.data);
  }

  #closeWsPassthrough(s) {
    this.#close(s.server, 1009, 'ws frame too big');
    this.#releaseWs(s);
  }

  // ---- cockpit side ---------------------------------------------------------------------

  async #proxy(request) {
    const allow = this.#takeToken();
    if (!allow.ok) return textResponse(429, 'slow down', { 'retry-after': String(allow.retryAfter) });
    if (!this.tunnel) return offlinePage();
    if (this.streams.size + this.sockets.size >= MAX_STREAMS) return textResponse(503, 'too many streams', { 'retry-after': '1' });
    const url = new URL(request.url);
    const path = url.pathname + url.search;
    const id = this.nextId++;
    if ((request.headers.get('upgrade') ?? '').toLowerCase() === 'websocket') return this.#wsProxy(request, id, path);
    return this.#httpProxy(request, id, path);
  }

  async #httpProxy(request, id, path) {
    const s = {
      id,
      method: request.method,
      template: pathTemplate(path),
      bytesIn: 0,
      bytesOut: 0,
      start: Date.now(),
      responded: false,
      done: false,
      status: 0,
    };
    this.streams.set(id, s);
    this.#send({ t: 'req', id, method: request.method, path, headers: stripHopByHop(request.headers) });
    // The timeout guards the head only; once res arrives the body's own idle timer governs it instead.
    s.timer = setTimeout(() => this.#failPending(s, 504), this.o.responseTimeoutMs);
    const head = new Promise((resolve) => {
      s.resolve = resolve;
    });
    this.#pumpRequest(request, s);
    return head;
  }

  // Streams the request body to the mothership in ≤ CHUNK_RAW raw chunks, always ending with end:true.
  async #pumpRequest(request, s) {
    const reader = request.body?.getReader();
    if (reader) {
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          for (let at = 0; at < value.length; at += CHUNK_RAW) {
            const piece = value.subarray(at, Math.min(value.length, at + CHUNK_RAW));
            s.bytesIn += piece.length;
            this.#send({ t: 'body', id: s.id, chunk: b64encode(piece), end: false });
          }
        }
      } catch {
        // Browser went away mid-body: end the frame train anyway so the mothership is not left hanging.
      }
    }
    this.#send({ t: 'body', id: s.id, chunk: '', end: true });
  }

  // WebSocket passthrough: accept the browser socket, then relay frames both ways.
  #wsProxy(request, id, path) {
    const [client, server] = runtime.pair();
    server.accept();
    const s = {
      id,
      method: request.method,
      template: pathTemplate(path),
      bytesIn: 0,
      bytesOut: 0,
      start: Date.now(),
      done: false,
      server,
    };
    this.sockets.set(id, s);
    this.#send({ t: 'ws_open', id, path, headers: stripHopByHop(request.headers, { ws: true }) });
    server.addEventListener('message', (ev) => {
      const binary = typeof ev.data !== 'string';
      s.bytesIn += byteLength(ev.data);
      this.#send({ t: 'ws_msg', id, data: binary ? b64encode(new Uint8Array(ev.data)) : ev.data, binary });
    });
    server.addEventListener('close', (ev) => {
      this.#send({ t: 'ws_close', id, code: typeof ev.code === 'number' ? ev.code : 1005 });
      this.#releaseWs(s);
    });
    return runtime.upgrade(client);
  }

  // ---- streams bookkeeping --------------------------------------------------------------

  #failPending(s, status) {
    if (!s.responded) {
      s.responded = true;
      clearTimeout(s.timer);
      s.resolve(textResponse(status, status === 504 ? 'gateway timeout' : 'bad gateway'));
    } else {
      return this.#failBody(s);
    }
    this.#release(s, status);
  }

  #failBody(s) {
    try {
      s.errorBody();
    } catch {
      // already closed
    }
    this.#release(s);
  }

  // The response body may take as long as it takes, but only while it is moving: streamIdleMs without
  // an inbound body frame errors it and frees the slot, so a stalled mothership cannot pin streams.
  #armBodyIdle(s) {
    clearTimeout(s.bodyTimer);
    s.bodyTimer = setTimeout(() => this.#failBody(s), this.o.streamIdleMs);
  }

  #release(s, status) {
    if (s.done) return;
    s.done = true;
    this.streams.delete(s.id);
    clearTimeout(s.timer);
    clearTimeout(s.bodyTimer);
    this.#log(s.method, s.template, status ?? s.status, s.bytesIn, s.bytesOut, s.start);
  }

  #releaseWs(s) {
    if (s.done) return;
    s.done = true;
    this.sockets.delete(s.id);
    this.#log(s.method, s.template, 101, s.bytesIn, s.bytesOut, s.start);
  }

  #log(method, path, status, bytesIn, bytesOut, start) {
    console.log(JSON.stringify({ method, path, status, bytes_in: bytesIn, bytes_out: bytesOut, ms: Date.now() - start }));
  }

  // ---- helpers --------------------------------------------------------------------------

  // Token bucket, refilled continuously. Returns whether one request may proceed, and how long to wait.
  #takeToken() {
    const r = this.rate;
    const now = Date.now();
    r.tokens = Math.min(r.capacity, r.tokens + ((now - r.at) / 1000) * r.perSecond);
    r.at = now;
    if (r.tokens >= 1) {
      r.tokens -= 1;
      return { ok: true };
    }
    return { ok: false, retryAfter: Math.max(1, Math.ceil((1 - r.tokens) / r.perSecond)) };
  }

  #resetIdle() {
    clearTimeout(this.idleTimer);
    this.idleTimer = setTimeout(() => this.tunnel && this.#killTunnel(this.tunnel, 1000, 'idle'), this.o.idleTimeoutMs);
  }

  #send(frame) {
    return this.tunnel ? this.#trySend(this.tunnel.ws, frame) : false;
  }

  #trySend(ws, frame) {
    try {
      ws.send(JSON.stringify(frame));
      return true;
    } catch {
      return false;
    }
  }

  // A browser socket takes raw data, not a frame: text through as text, binary through as bytes.
  #trySendRaw(ws, data) {
    try {
      ws.send(data);
      return true;
    } catch {
      return false;
    }
  }

  #sendTo(ws, frame) {
    try {
      ws.send(JSON.stringify(frame));
    } catch {
      // socket already gone
    }
  }

  #close(ws, code, reason) {
    try {
      ws.close(code, reason);
    } catch {
      // socket already closed
    }
  }
}
