// Fakes for driving InstallTunnel on plain Node: a linked WebSocket pair, a mothership that speaks protocol
// v1 with a real Ed25519 key from WebCrypto, and small helpers. Nothing here knows Workers beyond the two
// seams in src/runtime.js, which the tests overwrite.

import { InstallTunnel } from '../src/tunnel.js';
import { b64encode } from '../src/crypto.js';

const ENCODER = new TextEncoder();

// One end of a fake pair. send() delivers to the peer as a message event, close() closes both ends, exactly
// like the runtime does; closeEvent remembers the last close for assertions.
export class FakeSocket {
  constructor(peer = null) {
    this.peer = peer;
    this.readyState = 0; // CONNECTING until accept()
    this.handlers = new Map();
    this.sent = [];
    this.closeEvent = null;
  }

  accept() {
    if (this.readyState === 0) this.readyState = 1;
  }

  addEventListener(type, handler) {
    if (!this.handlers.has(type)) this.handlers.set(type, []);
    this.handlers.get(type).push(handler);
  }

  emit(type, event) {
    for (const handler of this.handlers.get(type) ?? []) handler(event);
  }

  send(data) {
    if (this.readyState !== 1) throw new Error(`send on readyState ${this.readyState}`);
    this.sent.push(data);
    // Delivered on the next event-loop turn, like a runtime dispatching to the peer: a sender never sees
    // its own listener run, and listeners attached in the same tick still get the frame.
    setImmediate(() => this.peer?.emit('message', { data }));
  }

  close(code = 1005, reason = '') {
    if (this.readyState >= 2) return;
    this.readyState = 3;
    this.closeEvent = { code, reason };
    this.emit('close', { code, reason });
    if (this.peer) this.peer.close(code, reason);
  }
}

export function fakePair() {
  const a = new FakeSocket();
  const b = new FakeSocket(a);
  a.peer = b;
  return [a, b];
}

/** Fails a promise after 2s, so a protocol bug shows as a test failure instead of a hang. */
export function within(promise, label = 'timed out', ms = 2000) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(label)), ms);
      if (timer.unref) timer.unref();
    }),
  ]).finally(() => clearTimeout(timer));
}

/**
 * A fresh DO with a state object that records every property touch (tests assert storage stays untouched)
 * and test-scale timers. A leftover tunnel dies of idleness after 4s, so node --test always exits.
 */
export function makeDo(opts = {}) {
  const touched = [];
  const state = new Proxy(
    {},
    {
      get(target, key) {
        touched.push(String(key));
        return target[key];
      },
      set(target, key, value) {
        touched.push(String(key));
        target[key] = value;
        return true;
      },
    },
  );
  const relay = new InstallTunnel(
    state,
    {},
    {
      helloTimeoutMs: 2000,
      responseTimeoutMs: 1000,
      idleTimeoutMs: 4000,
      pingMs: 60000,
      ...opts,
      rate: { capacity: 120, perSecond: 20, ...opts.rate },
    },
  );
  return { relay, state, touched };
}

/** The browser request the worker would forward after owner sign-in: proxy kind, install tagged. */
export function proxyRequest(path = '/app?keep=1', { method = 'GET', headers = {}, body, installId = INSTALL } = {}) {
  const request = new Request(`https://my.colonizer.dev${path}`, {
    method,
    headers: { 'x-relay-kind': 'proxy', 'x-relay-install-id': installId, ...headers },
    body,
    ...(body && typeof body !== 'string' && body.getReader ? { duplex: 'half' } : {}),
  });
  return request;
}

export const INSTALL = '11111111-2222-4333-8444-555555555555';

/** The mothership end of the tunnel: dials the DO, answers challenges, and records every frame it receives. */
export class FakeMothership {
  static async create() {
    const keys = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
    const raw = new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey));
    const ms = new FakeMothership();
    ms.keys = keys;
    ms.publicKeyB64 = b64encode(raw);
    return ms;
  }

  constructor() {
    this.socket = null;
    this.frames = [];
    this.claimed = new Set();
    this.waiters = [];
  }

  /** Opens a tunnel socket and waits for the challenge. Returns { socket, nonce }; call hello() to finish.
   * The mothership end is the socket the DO handed back in its 101, as the platform would wire it. */
  async dial(relay, installId = INSTALL, { publicKey = this.publicKeyB64 } = {}) {
    const request = new Request(`https://my.colonizer.dev/tunnel/${installId}`, {
      headers: { 'x-relay-kind': 'tunnel', 'x-relay-install-id': installId, 'x-relay-public-key': publicKey },
    });
    const response = await relay.fetch(request);
    if (response.status !== 101) throw new Error(`tunnel dial answered ${response.status}`);
    const msEnd = response.webSocket;
    msEnd.accept();
    msEnd.addEventListener('message', (ev) => this.#receive(ev.data));
    this.socket = msEnd;
    const challenge = await within(this.next('challenge'), 'no challenge arrived');
    return { socket: msEnd, nonce: challenge.nonce };
  }

  /** Answers a challenge with a signature over nonce ‖ install_id ‖ ts. signTs decouples what is signed from
   * what is sent (tamper tests); the remaining override replaces hello fields wholesale. */
  async hello(installId, nonce, { ts = Math.floor(Date.now() / 1000), signTs = ts, version = 1, ...override } = {}) {
    const message = nonce + installId + String(signTs);
    const sig = await crypto.subtle.sign('Ed25519', this.keys.privateKey, ENCODER.encode(message));
    this.send({ t: 'hello', sig: b64encode(new Uint8Array(sig)), ts, version, ...override });
  }

  /** dial + hello: a verified, live tunnel. Waits until this socket is the DO's live tunnel (past the async
   * signature check and any replacement), so what a test does next never races the handshake. */
  async connect(relay, installId = INSTALL) {
    const { socket, nonce } = await this.dial(relay, installId);
    await this.hello(installId, nonce);
    for (let tries = 500; relay.tunnel?.ws?.peer !== socket && tries > 0; tries--) await new Promise((r) => setImmediate(r));
    if (relay.tunnel?.ws?.peer !== socket) throw new Error('hello was never verified');
    return socket;
  }

  /** The next unclaimed frame matching a type or predicate, as a promise. */
  next(match) {
    const test = typeof match === 'string' ? (f) => f.t === match : match;
    const ready = this.frames.find((f) => !this.claimed.has(f) && test(f));
    if (ready) {
      this.claimed.add(ready);
      return Promise.resolve(ready);
    }
    return new Promise((resolve, reject) => this.waiters.push({ test, resolve, reject }));
  }

  /** Collects the body frames of one stream until end:true, returning their non-empty base64 chunk
   * strings: the relay terminates every body with an empty end:true frame. */
  async body(id) {
    const chunks = [];
    for (;;) {
      const frame = await within(this.next((f) => f.t === 'body' && f.id === id), 'body never ended');
      if (frame.chunk) chunks.push(frame.chunk);
      if (frame.end) return chunks;
    }
  }

  send(frame) {
    this.socket.send(JSON.stringify(frame));
  }

  /** A raw text frame, for tests that feed the DO malformed JSON. */
  raw(text) {
    this.socket.send(text);
  }

  #receive(data) {
    const frame = JSON.parse(data);
    this.frames.push(frame);
    const at = this.waiters.findIndex((w) => w.test(frame));
    if (at !== -1) {
      const [waiter] = this.waiters.splice(at, 1);
      this.claimed.add(frame);
      waiter.resolve(frame);
    }
  }
}
