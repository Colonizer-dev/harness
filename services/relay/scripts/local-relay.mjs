// A local stand-in for the deployed relay, for cross-language end-to-end tests: the Rust tunnel
// client (crates/colonizer/src/remote.rs) spawns this script and points COLONIZER_REMOTE_URL at it.
//
// Contract:
//   - `node scripts/local-relay.mjs` listens on 127.0.0.1 on an ephemeral port and writes exactly
//     one line to stdout, `listening <port>`. Everything else (per-request logs, crashes) goes to
//     stderr. The process exits when stdin closes — spawn it with a piped stdin and close or drop
//     the pipe to take it down. Needs Node >= 22.5 (node:sqlite).
//   - RELAY_DOMAIN (default `my.colonizer.dev`) is the host every request is re-homed onto, so the
//     real worker (src/worker.js) sees the URL shapes it routes on; the client's Host header does
//     not matter. Configure the client with COLONIZER_REMOTE_URL=ws://127.0.0.1:<port>: it derives
//     http://127.0.0.1:<port> for registration (remote.rs http_base) and dials
//     ws://127.0.0.1:<port>/tunnel/<install_id>.
//   - POST /api/installs: the real registration against a fake in-memory D1 (test/d1.mjs), answering
//     {"install_id", "host"} with 201.
//   - GET /tunnel/<install_id> with an upgrade: the real worker dial into a real InstallTunnel DO
//     (a fake namespace, like test/e2e.test.mjs). The DO's side of the 101 is bridged onto the raw
//     socket with a minimal RFC 6455 codec — masked frames in, unmasked out; text, close with
//     code/reason, ping/pong — so a close the DO sends (4000 'replaced') reaches the client as a
//     real close frame.
//   - A request whose Host is `<install_id>.<RELAY_DOMAIN>` keeps that host, so the real owner sign-in
//     (/_auth, /_auth/callback) runs, and with the session cookie it mints, the real worker proxies
//     the request to the install's DO exactly as deployed. A WebSocket upgrade on that host is the
//     browser's passthrough: the DO's browser end is bridged onto the raw socket with the same codec
//     as the tunnel dial, so a browser socket rides the real worker, DO and tunnel end to end. GitHub is stubbed: the callback's `code` names the account,
//     `<github_id>:<login>`, and the token exchange and /user read answer exactly that. The session
//     secret and OAuth app are fixed test values. With this, the pairing flow runs end to end: sign in,
//     read the code off the pairing page, confirm it with the mothership's signed call.
//   - POST /_local/expire-pairings?install=<install_id> (harness only, never the worker) moves every
//     pending pairing of that install into the past, so an expiry can be tested without waiting.
//   - Any other request carrying header `x-local-relay-install: <install_id>` skips GitHub owner
//     sign-in: it is forwarded to that install's DO as a proxy request (x-relay-kind: proxy, like
//     test/fakes.mjs proxyRequest), and the DO's Response is written back faithfully — status,
//     every header (each set-cookie its own header line) and the streamed body, delimited by
//     connection: close. Requests are served one at a time, in arrival order.

import { createHash } from 'node:crypto';
import { STATUS_CODES, createServer } from 'node:http';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { runtime } from '../src/runtime.js';
import worker from '../src/worker.js';
import { fakeD1 } from '../test/d1.mjs';
import { fakePair, makeDo } from '../test/fakes.mjs';

const DOMAIN = (process.env.RELAY_DOMAIN ?? 'my.colonizer.dev').toLowerCase();
const NO_CTX = { waitUntil: () => {} };

// One real InstallTunnel DO per install id (created on demand, like a DO namespace), with generous
// timers so a slow e2e is not cut off; the 5 s ping keeps the DO's idle timer fed.
const dos = new Map();
const env = {
  RELAY_DOMAIN: DOMAIN,
  DB: fakeD1(),
  GITHUB_CLIENT_ID: 'local-relay-client',
  GITHUB_CLIENT_SECRET: 'local-relay-secret',
  SESSION_SECRET: 'local-relay-session-secret',
  TUNNELS: {
    idFromName: (name) => name,
    get(id) {
      if (!dos.has(id)) {
        dos.set(id, makeDo({ env, helloTimeoutMs: 10000, responseTimeoutMs: 60000, idleTimeoutMs: 120000, pingMs: 5000 }));
      }
      const made = dos.get(id);
      return { fetch: (request) => made.relay.fetch(request) };
    },
  },
};

// GitHub, stubbed: the OAuth `code` is `<github_id>:<login>`; the token exchange hands it back as the
// access token and /user reads the account out of it. Nothing else is fetched from here.
const realFetch = globalThis.fetch;
globalThis.fetch = async (input, init = {}) => {
  const url = String(input instanceof Request ? input.url : input);
  if (url === 'https://github.com/login/oauth/access_token') {
    const { code } = JSON.parse(init.body ?? '{}');
    return Response.json({ access_token: code });
  }
  if (url === 'https://api.github.com/user') {
    const token = String(new Headers(init.headers).get('authorization') ?? '').replace(/^Bearer /, '');
    const at = token.indexOf(':');
    const id = Number(token.slice(0, at));
    return at > 0 && Number.isInteger(id) ? Response.json({ id, login: token.slice(at + 1) }) : new Response('bad token', { status: 401 });
  }
  return realFetch(input, init);
};

// The upgrade currently in flight (a mothership dial or a browser websocket): the raw socket, and once
// the DO has made its socket pair, the bridge end its side speaks through. Requests are served one at a
// time, so one global is safe.
let dial = null;

runtime.pair = () => {
  const current = dial;
  if (!current) return fakePair(); // no upgrade in flight to bridge onto
  const [bridge, server] = fakePair();
  bridge.accept();
  // Out: whatever the DO writes on its end leaves as a text frame. In: frames read off the socket
  // are injected on the bridge, so they surface on the DO's end like the platform would deliver.
  bridge.addEventListener('message', (ev) => writeFrame(current.socket, TEXT, Buffer.from(ev.data)));
  bridge.addEventListener('close', ({ code, reason }) => {
    // end() lets the close frame flush before the FIN; destroy() could drop it under backpressure.
    if (writableClose(code)) current.socket.end(closeFrame(code, reason));
    else current.socket.destroy();
  });
  current.bridge = bridge;
  return [bridge, server];
};
runtime.upgrade = (client) => ({ status: 101, webSocket: client });

// ------------------------------------------------------------------------ RFC 6455, just enough

const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';
const TEXT = 0x1;
const CLOSE = 0x8;
const PING = 0x9;
const PONG = 0xa;
const MAX_FRAME = 16 * 1024 * 1024; // far under the client's 64 MiB message limit

const acceptKey = (key) => createHash('sha1').update(key + WS_GUID).digest('base64');

// 1005/1006 never ride a close frame, and an out-of-range code makes strict clients throw.
const writableClose = (code) =>
  (code >= 1000 && code <= 1014 && code !== 1005 && code !== 1006) || (code >= 3000 && code <= 4999);

/** One unmasked frame (servers never mask), with 16- and 64-bit lengths. */
function frame(opcode, payload = Buffer.alloc(0)) {
  const head = [0x80 | opcode];
  const len = payload.length;
  if (len < 126) {
    head.push(len);
  } else if (len < 65536) {
    head.push(126, len >> 8, len & 255);
  } else {
    head.push(127);
    const big = Buffer.alloc(8);
    big.writeBigUInt64BE(BigInt(len));
    head.push(...big);
  }
  return Buffer.concat([Buffer.from(head), payload]);
}

const writeFrame = (socket, opcode, payload) => {
  if (!socket.destroyed) socket.write(frame(opcode, payload));
};

/** A close frame: the code, then up to 123 bytes of reason. */
function closeFrame(code, reason = '') {
  const body = Buffer.from(String(reason).slice(0, 100), 'utf8');
  const out = Buffer.alloc(2 + body.length);
  out.writeUInt16BE(code);
  body.copy(out, 2);
  return frame(CLOSE, out);
}

/** Reads masked client frames — 16- and 64-bit lengths — off the raw stream. tungstenite sends each
 * message as one frame, so fragmentation is not reassembled; the tunnel protocol speaks text only,
 * so every non-control frame is delivered as text. */
class ClientFrames {
  constructor(socket, head, handlers) {
    this.socket = socket;
    this.handlers = handlers;
    this.buf = Buffer.alloc(0);
    socket.on('data', (chunk) => this.push(chunk));
    if (head.length) this.push(head);
  }

  push(chunk) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk;
    for (;;) {
      const next = this.#frame();
      if (!next) return;
      this.#dispatch(next);
    }
  }

  // The next complete frame in the buffer, or null while there is less than one in it.
  #frame() {
    const buf = this.buf;
    if (buf.length < 2) return null;
    const opcode = buf[0] & 0x0f;
    let len = buf[1] & 0x7f;
    let at = 2;
    if (len === 126) {
      if (buf.length < 4) return null;
      len = buf.readUInt16BE(2);
      at = 4;
    } else if (len === 127) {
      if (buf.length < 10) return null;
      len = Number(buf.readBigUInt64BE(2));
      at = 10;
    }
    if (len > MAX_FRAME) {
      this.handlers.close(1009, 'frame too big');
      return null; // the close tears the socket down; nothing more is read
    }
    if ((buf[1] & 0x80) === 0) {
      this.handlers.close(1002, 'client frames must be masked');
      return null;
    }
    at += 4;
    if (buf.length < at + len) return null;
    const mask = buf.subarray(at - 4, at);
    const payload = Buffer.allocUnsafe(len);
    for (let i = 0; i < len; i++) payload[i] = buf[at + i] ^ mask[i & 3];
    this.buf = buf.subarray(at + len);
    return { opcode, payload };
  }

  #dispatch({ opcode, payload }) {
    if (opcode === CLOSE) {
      this.handlers.close(payload.length >= 2 ? payload.readUInt16BE(0) : 1005, payload.subarray(2).toString('utf8'));
      return;
    }
    if (opcode === PING) return writeFrame(this.socket, PONG, payload);
    if (opcode === PONG || opcode === 0x0) return; // nothing is outstanding; no fragmentation expected
    this.handlers.message(payload.toString('utf8'));
  }
}

// ------------------------------------------------------------------------ the local HTTP server

const server = createServer((req, res) => {
  res.on('error', () => {}); // a client hanging up mid-response is not a harness crash
  enqueue(() => serve(req, res));
});

// An Upgrade request is the mothership's tunnel dial on the apex, or a signed-in browser's websocket
// on an install host; the worker tells them apart, and either way the DO's end is bridged onto the
// socket. The sign-in bypass header serves plain HTTP only.
server.on('upgrade', (req, socket, head) => {
  socket.on('error', () => {});
  if (req.headers['x-local-relay-install'] !== undefined) {
    return plain(socket, 501, 'the sign-in bypass header serves plain HTTP only; sign in for a websocket');
  }
  enqueue(() => bridgeUpgrade(req, socket, head));
});

server.on('error', (e) => {
  process.stderr.write(`local-relay: ${e?.stack ?? e}\n`);
  process.exit(1);
});

// One request at a time, in arrival order: the dial needs the socket pair() picks up to be its own.
let tail = Promise.resolve();
function enqueue(job) {
  tail = tail.then(job, job).catch((e) => process.stderr.write(`local-relay: ${e?.stack ?? e}\n`));
}

async function serve(req, res) {
  try {
    const hook = new URL(req.url, 'http://local');
    if (req.method === 'POST' && hook.pathname === '/_local/expire-pairings') {
      await env.DB.prepare('UPDATE pairings SET expires_at = ? WHERE install_id = ?')
        .bind(Math.floor(Date.now() / 1000) - 1, hook.searchParams.get('install') ?? '')
        .run();
      res.writeHead(204, { connection: 'close' });
      return res.end();
    }
    const install = req.headers['x-local-relay-install'];
    const request = buildRequest(req, install);
    const response =
      install === undefined
        ? await worker.fetch(request, env, NO_CTX)
        : await env.TUNNELS.get(install).fetch(request);
    await writeResponse(res, response);
  } catch (e) {
    process.stderr.write(`local-relay: ${req.method} ${req.url}: ${e?.stack ?? e}\n`);
    res.destroy();
  }
}

// The client's request, re-homed on RELAY_DOMAIN so the worker sees the hosts it routes on. Repeated
// headers keep their repeats; content-length rides along and the DO re-chunks the body itself. With
// an install it becomes the proxy request a signed-in owner's browser would have produced: relay
// kind, install tagged, and nothing left of the sign-in bypass.
function buildRequest(req, install) {
  const headers = new Headers();
  for (let at = 0; at < req.rawHeaders.length; at += 2) headers.append(req.rawHeaders[at], req.rawHeaders[at + 1]);
  if (install !== undefined) {
    headers.delete('x-local-relay-install');
    headers.set('x-relay-kind', 'proxy');
    headers.set('x-relay-install-id', install);
  }
  const body = req.method === 'GET' || req.method === 'HEAD' ? null : Readable.toWeb(req);
  // An install's own host is kept, so the worker routes it to the owner sign-in; the scheme is https
  // because that is what the worker builds its callback URL from.
  const asked = String(req.headers.host ?? '').toLowerCase();
  if (install === undefined && asked.endsWith(`.${DOMAIN}`)) {
    return new Request(`https://${asked}${req.url}`, { method: req.method, headers, body, ...(body ? { duplex: 'half' } : {}) });
  }
  return new Request(`http://${DOMAIN}${req.url}`, { method: req.method, headers, body, ...(body ? { duplex: 'half' } : {}) });
}

// Status, every header and the streamed body, closed off with connection: close so a body without a
// length is still delimited. The flat-array writeHead form writes each pair as its own header line,
// and set-cookie comes from getSetCookie() so every cookie keeps its own line.
async function writeResponse(res, response) {
  const head = ['connection', 'close'];
  for (const [name, value] of response.headers.entries()) {
    if (name !== 'set-cookie') head.push(name, value);
  }
  for (const cookie of response.headers.getSetCookie()) head.push('set-cookie', cookie);
  res.writeHead(response.status, head);
  if (response.body) await pipeline(Readable.fromWeb(response.body), res).catch(() => {}); // a stalled body kills the socket
  res.end(() => res.socket?.end());
}

// An upgrade through the real worker. On the apex it is the mothership's dial: the fake D1 holds the
// registered key, so the worker tags the dial with it and the DO challenges over the bridged socket. On
// an install host it is a browser's websocket, which the worker forwards only with the owner's session.
async function bridgeUpgrade(req, socket, head) {
  let bridge = null;
  try {
    dial = { socket, bridge: null };
    const response = await worker.fetch(buildRequest(req), env, NO_CTX);
    bridge = dial.bridge;
    dial = null;
    if (response.status !== 101 || !bridge) return plain(socket, response.status, await response.text());
    socket.write(
      'HTTP/1.1 101 Switching Protocols\r\n' +
        'upgrade: websocket\r\n' +
        'connection: Upgrade\r\n' +
        `sec-websocket-accept: ${acceptKey(req.headers['sec-websocket-key'] ?? '')}\r\n` +
        '\r\n',
    );
    new ClientFrames(socket, head, {
      message: (text) => {
        try {
          bridge.send(text);
        } catch {
          // the DO already dropped this end
        }
      },
      close: (code, reason) => {
        try {
          bridge.close(code, reason);
        } catch {
          // already closed
        }
      },
    });
    socket.on('close', () => bridge.close(1006, 'socket gone')); // no close frame came: abnormal
  } catch (e) {
    dial = null;
    process.stderr.write(`local-relay: upgrade ${req.url}: ${e?.stack ?? e}\n`);
    socket.destroy();
  }
}

const plain = (socket, status, text) => {
  const body = Buffer.from(`${text}\n`);
  socket.end(
    `HTTP/1.1 ${status} ${STATUS_CODES[status] ?? 'OK'}\r\n` +
      'connection: close\r\n' +
      'content-type: text/plain; charset=utf-8\r\n' +
      `content-length: ${body.length}\r\n` +
      '\r\n' +
      `${body}`,
  );
};

// The DO's per-request log goes to stderr: stdout carries exactly the one contract line.
for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
  console[level] = (...args) => process.stderr.write(`${args.map(String).join(' ')}\n`);
}

process.on('unhandledRejection', (e) => {
  process.stderr.write(`local-relay: unhandled rejection: ${e?.stack ?? e}\n`);
  process.exit(1);
});

// Gone when the parent's pipe closes, so a parent that dies never leaves the harness behind.
process.stdin.resume();
process.stdin.on('end', () => process.exit(0));
process.stdin.on('error', () => process.exit(0));

server.listen(0, '127.0.0.1', () => process.stdout.write(`listening ${server.address().port}\n`));
