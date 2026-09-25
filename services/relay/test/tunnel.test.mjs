// InstallTunnel driven end to end on the fakes in fakes.mjs: real WebCrypto for the hello, fake WebSocket
// pairs for the tunnel and the browser, a recording state object. Every frame a test asserts on was either
// produced or consumed through the protocol, so nothing reaches into DO internals.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { runtime } from '../src/runtime.js';
import { CHUNK_RAW, MAX_PENDING, MAX_STREAMS, PING_MS, helloMessage, pathTemplate, stripHopByHop } from '../src/protocol.js';
import { b64decode, b64encode } from '../src/crypto.js';
import { FakeMothership, INSTALL, fakePair, makeDo, proxyRequest, within } from './fakes.mjs';

const ENCODER = new TextEncoder();
const bytes = (text) => ENCODER.encode(text);

// Node cannot build a 101 Response or a WebSocketPair, so the seams in runtime.js are replaced wholesale.
// The fake upgrade returns a lookalike: its callers and the tests only read status and webSocket.
const browserEnds = [];
runtime.pair = () => {
  const pair = fakePair();
  browserEnds.push(pair[0]);
  return pair;
};
runtime.upgrade = (client) => ({ status: 101, webSocket: client });

const concat = (list) => {
  const out = new Uint8Array(list.reduce((n, part) => n + part.length, 0));
  let at = 0;
  for (const part of list) {
    out.set(part, at);
    at += part.length;
  }
  return out;
};
const sha = async (value) =>
  Buffer.from(await crypto.subtle.digest('SHA-256', typeof value === 'string' ? bytes(value) : value)).toString('hex');

// Resolves with the close event of a socket that is expected to close from this point on.
const closed = (socket) => new Promise((resolve) => socket.addEventListener('close', resolve));

// One answered round trip: a request through the relay, res and body frames sent back by hand.
async function roundTrip(relay, ms, { headers = {}, ...rest } = {}) {
  const pending = within(relay.fetch(proxyRequest('/quiet/77?token=SEKRET', { headers, ...rest })), 'round trip never finished');
  const req = await within(ms.next('req'), 'no req frame');
  ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'text/plain' } });
  ms.send({ t: 'body', id: req.id, chunk: b64encode(bytes('ok')), end: true });
  const response = await pending;
  return { req, response, text: await response.text() };
}

test('protocol constants hold the sizes the mothership client is built against', () => {
  assert.equal(CHUNK_RAW, 36864);
  assert.equal(b64encode(new Uint8Array(CHUNK_RAW)).length, 49152); // exactly 48 KiB of base64
  assert.equal(MAX_STREAMS, 32);
  assert.equal(PING_MS, 20000);
});

test('hop-by-hop, connection-named, x-relay-* and ws handshake headers are stripped, the rest survive', () => {
  const stripped = stripHopByHop({
    Host: 'my.colonizer.dev',
    Connection: 'keep-alive, X-Drop-Me',
    'X-Drop-Me': 'named in connection',
    TE: 'trailers',
    'Transfer-Encoding': 'chunked',
    Upgrade: 'websocket',
    'Sec-WebSocket-Key': 'K',
    'Sec-WebSocket-Version': '13',
    'X-Relay-Kind': 'proxy',
    'X-Relay-Install-Id': INSTALL,
    'Keep-Alive': 'timeout=5',
    Cookie: 'a=b',
  });
  // The handshake headers survive a plain strip: only a ws_open drops them.
  assert.deepEqual(stripped, { host: 'my.colonizer.dev', cookie: 'a=b', 'sec-websocket-key': 'K', 'sec-websocket-version': '13' });

  const ws = stripHopByHop({ 'sec-websocket-key': 'K', 'sec-websocket-protocol': 'events-v1', host: 'x' }, { ws: true });
  assert.deepEqual(ws, { host: 'x', 'sec-websocket-protocol': 'events-v1' });

  const headers = new Headers({ 'x-relay-kind': 'proxy', connection: 'close', accept: 'text/html' });
  assert.deepEqual(stripHopByHop(headers), { accept: 'text/html' });
});

test('pathTemplate keeps ordinary segments and templates ids, queries and all', () => {
  assert.equal(pathTemplate('/api/repos/42/file?token=SEKRET'), '/api/repos/:id/file');
  assert.equal(pathTemplate('/gists/0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b'), '/gists/:id');
  assert.equal(pathTemplate('/trees/deadbeef01'), '/trees/:id'); // hex of 8+
  assert.equal(pathTemplate('/objects/aBcDeFgH01jKlMnOp5'), '/objects/:id'); // long letter+digit token
  assert.equal(pathTemplate('/user/repos'), '/user/repos');
  assert.equal(pathTemplate('/tags/v1.2.3'), '/tags/v1.2.3');
  assert.equal(pathTemplate('/x/abcdef'), '/x/abcdef'); // hex shorter than 8
  assert.equal(pathTemplate('/plain?secret=yes'), '/plain');
  assert.equal(helloMessage('n', 'install', 123), 'ninstall123');
  assert.equal(helloMessage('n', 'install', 123.5), 'ninstall123.5');
});

test('a proxied request is replayed as protocol frames and the answer comes back', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  const { req, response, text } = await roundTrip(relay, ms, {
    method: 'POST',
    headers: { 'content-type': 'application/json', connection: 'keep-alive', te: 'trailers', 'x-relay-evil': '1', 'x-custom': 'yes' },
    body: '{"hello":"world"}',
  });

  assert.equal(req.method, 'POST');
  assert.equal(req.path, '/quiet/77?token=SEKRET');
  assert.equal(req.headers['content-type'], 'application/json');
  assert.equal(req.headers['x-custom'], 'yes');
  for (const absent of ['connection', 'te', 'x-relay-evil', 'x-relay-kind', 'x-relay-install-id']) {
    assert.equal(req.headers[absent], undefined, absent);
  }
  assert.deepEqual(await ms.body(req.id), [b64encode(bytes('{"hello":"world"}'))]);

  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'text/plain');
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('null-body statuses carry no body, and a status outside 200-599 is a 502', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  for (const [method, status] of [['GET', 204], ['GET', 304], ['HEAD', 200]]) {
    const pending = within(relay.fetch(proxyRequest('/thing', { method })), `${method} ${status} never finished`);
    const req = await within(ms.next('req'), 'no req frame');
    ms.send({ t: 'res', id: req.id, status, headers: { 'content-type': 'text/plain', 'content-length': '99' } });
    const response = await pending;
    assert.equal(response.status, status);
    assert.equal((await response.arrayBuffer()).byteLength, 0, `${method} ${status} must have no body`);
  }

  for (const bad of [101, 199, 600]) {
    const pending = within(relay.fetch(proxyRequest('/thing')), `status ${bad} never finished`);
    const req = await within(ms.next('req'), 'no req frame');
    ms.send({ t: 'res', id: req.id, status: bad, headers: {} });
    ms.send({ t: 'body', id: req.id, chunk: '', end: true });
    assert.equal((await pending).status, 502, `status ${bad} must become 502`);
  }
  ms.socket.close();
});

test('bodies past 48 KiB travel in bounded chunks both ways and reassemble exactly', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  const payload = new Uint8Array(200 * 1024);
  for (let at = 0; at < payload.length; at += 65536) crypto.getRandomValues(payload.subarray(at, at + 65536));
  const pending = within(relay.fetch(proxyRequest('/upload', { method: 'POST', body: payload })), 'upload never finished');
  const req = await within(ms.next('req'), 'no req frame');

  const outbound = await ms.body(req.id);
  assert.ok(outbound.length >= 6, `expected several chunks, got ${outbound.length}`);
  for (const chunk of outbound) assert.ok(chunk.length <= 49152, `chunk of ${chunk.length} chars`);
  assert.equal(await sha(concat(outbound.map(b64decode))), await sha(payload));

  ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'application/octet-stream' } });
  for (let at = 0; at < payload.length; at += CHUNK_RAW) {
    const piece = payload.subarray(at, Math.min(payload.length, at + CHUNK_RAW));
    ms.send({ t: 'body', id: req.id, chunk: b64encode(piece), end: at + CHUNK_RAW >= payload.length });
  }
  const response = await pending;
  assert.equal(await sha(new Uint8Array(await response.arrayBuffer())), await sha(payload));
  ms.socket.close();
});

test('websocket passthrough relays text and binary both ways and propagates closes', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  const open1 = within(relay.fetch(proxyRequest('/events', { headers: { upgrade: 'websocket', 'sec-websocket-key': 'K', 'sec-websocket-protocol': 'events-v1' } })));
  const wsOpen = await within(ms.next('ws_open'), 'no ws_open');
  const res1 = await open1;
  assert.equal(res1.status, 101);
  assert.equal(wsOpen.path, '/events');
  assert.equal(wsOpen.headers['sec-websocket-key'], undefined);
  assert.equal(wsOpen.headers['x-relay-install-id'], undefined);
  assert.equal(wsOpen.headers['sec-websocket-protocol'], 'events-v1');

  const browser = browserEnds.at(-1);
  browser.accept();
  const pushed = [];
  browser.addEventListener('message', (ev) => pushed.push(ev.data));

  browser.send('hello from browser');
  const msg1 = await within(ms.next('ws_msg'), 'no ws_msg for text');
  assert.deepEqual({ data: msg1.data, binary: msg1.binary }, { data: 'hello from browser', binary: false });

  browser.send(new Uint8Array([0, 1, 254, 255]));
  const msg2 = await within(ms.next((f) => f.t === 'ws_msg' && f.binary), 'no ws_msg for binary');
  assert.deepEqual([...b64decode(msg2.data)], [0, 1, 254, 255]);

  ms.send({ t: 'ws_msg', id: wsOpen.id, data: 'pushed text', binary: false });
  ms.send({ t: 'ws_msg', id: wsOpen.id, data: b64encode(new Uint8Array([9, 8, 7])), binary: true });
  await within((async () => {
    while (pushed.length < 2) await new Promise((r) => setImmediate(r));
  })(), 'browser never received pushed frames');
  assert.equal(pushed[0], 'pushed text');
  assert.deepEqual([...pushed[1]], [9, 8, 7]);

  // Closing from the browser tells the mothership; closing from the mothership closes the browser.
  const gone = closed(browser);
  browser.close(1000, 'bye');
  const closeFrame = await within(ms.next('ws_close'), 'no ws_close for browser close');
  assert.equal(closeFrame.code, 1000);
  assert.equal((await gone).code, 1000);

  const open2 = within(relay.fetch(proxyRequest('/ws', { headers: { upgrade: 'websocket' } })));
  const wsOpen2 = await within(ms.next('ws_open'), 'no second ws_open');
  assert.equal((await open2).status, 101);
  const gone2 = closed(browserEnds.at(-1));
  ms.send({ t: 'ws_close', id: wsOpen2.id, code: 4321 });
  assert.equal((await gone2).code, 4321);
  ms.socket.close();
});

test('the 33rd concurrent stream gets 503, and a freed slot serves again (ws counts too)', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  const ws = within(relay.fetch(proxyRequest('/events', { headers: { upgrade: 'websocket' } })));
  await within(ms.next('ws_open'), 'no ws_open');
  assert.equal((await ws).status, 101);

  const pending = [];
  for (let i = 1; i < MAX_STREAMS; i++) pending.push(within(relay.fetch(proxyRequest(`/stream/${i}`)), 'stream never finished'));
  const opened = [];
  for (let i = 1; i < MAX_STREAMS; i++) opened.push(await within(ms.next('req'), 'missing req frame'));

  const full = await relay.fetch(proxyRequest('/one-too-many'));
  assert.equal(full.status, 503);
  assert.equal(full.headers.get('retry-after'), '1');

  // Finishing the first HTTP stream frees its slot (the ws_open stream still holds one).
  const answer = (id) => {
    ms.send({ t: 'res', id, status: 200, headers: {} });
    ms.send({ t: 'body', id, chunk: '', end: true });
  };
  answer(opened[0].id);
  assert.equal((await pending[0]).status, 200);
  // The slot frees when the body frame lands, an event-loop turn after the head this await saw.
  for (let i = 0; relay.streams.size + relay.sockets.size >= MAX_STREAMS && i < 100; i++) await new Promise((r) => setImmediate(r));

  const again = within(relay.fetch(proxyRequest('/again')), 'retry never finished');
  const retry = await within(ms.next('req'), 'missing retry req');
  answer(retry.id);
  assert.equal((await again).status, 200);

  // Drain the rest so every stream finishes cleanly instead of timing out.
  for (const open of opened.slice(1)) answer(open.id);
  await Promise.all(pending.slice(1));
  ms.socket.close();
});

test('a verified replacement kills the old tunnel mid-flight: 4000, 502, errored body, 1012', async () => {
  const msA = await FakeMothership.create();
  const { relay } = makeDo();
  const sockA = await msA.connect(relay);

  const slow = within(relay.fetch(proxyRequest('/slow')), 'slow never finished');
  const slowReq = await within(msA.next('req'), 'no req frame');

  const stream = within(relay.fetch(proxyRequest('/stream')), 'stream never finished');
  const streamReq = await within(msA.next('req'), 'no second req frame');
  msA.send({ t: 'res', id: streamReq.id, status: 200, headers: { 'content-type': 'text/plain' } });
  const streaming = await stream;
  // Handler attached now, so the coming rejection is never an unhandled one.
  const bodyErrored = assert.rejects(streaming.text(), 'streaming body must error when the tunnel is replaced');

  await relay.fetch(proxyRequest('/events', { headers: { upgrade: 'websocket' } }));
  await within(msA.next('ws_open'), 'no ws_open');
  const browser = browserEnds.at(-1);
  browser.accept();

  const msB = await FakeMothership.create();
  await msB.connect(relay);
  assert.deepEqual(sockA.closeEvent, { code: 4000, reason: 'replaced' });

  assert.equal((await slow).status, 502);
  await within(bodyErrored, 'streaming body never errored');
  assert.equal(browser.closeEvent.code, 1012);

  const after = within(relay.fetch(proxyRequest('/after')), 'new tunnel never served');
  const afterReq = await within(msB.next('req'), 'new tunnel got no req frame');
  msB.send({ t: 'res', id: afterReq.id, status: 200, headers: {} });
  msB.send({ t: 'body', id: afterReq.id, chunk: '', end: true });
  assert.equal((await after).status, 200);
  msB.socket.close();
});

test('a bad hello never establishes or displaces a tunnel', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ helloTimeoutMs: 40 });
  const live = await ms.connect(relay);
  const now = Math.floor(Date.now() / 1000);

  // Dials a fresh socket, answers however the case wants, and expects the 1008 close.
  const rejected = async (dial = {}, hello = {}, sender = ms) => {
    const attempt = await ms.dial(relay, INSTALL, dial);
    const gone = closed(attempt.socket);
    sender.socket = attempt.socket; // an attacker replies on the socket it dialed from
    if (hello.frame) ms.send(hello.frame);
    else if (!hello.silent) await sender.hello(INSTALL, attempt.nonce, hello);
    return (await within(gone, 'the bad hello socket did not close')).code;
  };

  // The worker attaches the install's registered key; a stranger signs with its own.
  const stranger = await FakeMothership.create();
  assert.equal(await rejected({}, {}, stranger), 1008);
  assert.equal(await rejected({}, { ts: now, signTs: now + 1 }), 1008); // ts tampered after signing
  assert.equal(await rejected({}, { ts: now - 400 }), 1008); // stale, though correctly signed
  assert.equal(await rejected({}, { ts: now + 400 }), 1008); // from the future
  assert.equal(await rejected({}, { frame: { t: 'req', id: 1, method: 'GET', path: '/', headers: {} } }), 1008);
  assert.equal(await rejected({}, { silent: true }), 1008); // no hello at all within helloTimeoutMs

  // The live tunnel survived all of that and still serves. The bad dials left ms.socket pointing at a
  // closed socket, so aim the frame pipe back at the live one.
  assert.equal(live.closeEvent, null);
  ms.socket = live;
  const { text } = await roundTrip(relay, ms);
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('pending handshakes are independent: attacker dials never stop the real hello, and past MAX_PENDING dials are refused', async () => {
  const ms = await FakeMothership.create();
  const attacker = await FakeMothership.create();
  const { relay } = makeDo();
  const real = await ms.dial(relay); // the real mothership's handshake is now pending

  // An attacker who knows the public install id re-dials in a loop. No dial may close another pending
  // one, or the real mothership could never complete its hello.
  const spares = [];
  while (spares.length < MAX_PENDING - 1) {
    const attempt = await attacker.dial(relay);
    for (const socket of [real.socket, ...spares]) assert.equal(socket.closeEvent, null, 'a dial displaced a pending handshake');
    spares.push(attempt.socket);
  }

  // One past the cap, the dial is closed 1013 right after the upgrade, before any challenge.
  const refused = await relay.fetch(
    new Request(`https://my.colonizer.dev/tunnel/${INSTALL}`, {
      headers: { 'x-relay-kind': 'tunnel', 'x-relay-install-id': INSTALL, 'x-relay-public-key': attacker.publicKeyB64 },
    }),
  );
  assert.equal(refused.status, 101);
  assert.deepEqual(refused.webSocket.closeEvent, { code: 1013, reason: 'try again later' });

  // The real handshake still completes and takes the tunnel.
  await ms.hello(INSTALL, real.nonce);
  for (let tries = 500; relay.tunnel?.ws?.peer !== real.socket && tries > 0; tries--) await new Promise((r) => setImmediate(r));
  assert.equal(relay.tunnel?.ws?.peer, real.socket, 'the real hello never completed');
  assert.equal(real.socket.closeEvent, null);

  for (const socket of spares) socket.close(); // drain the attacker's pending dials and their timers
  ms.socket.close();
});

test('a stalled response body is errored and its slot freed after streamIdleMs of silence', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ streamIdleMs: 100 });
  await ms.connect(relay);

  const pending = within(relay.fetch(proxyRequest('/stall')), 'stalled stream never answered');
  const req = await within(ms.next('req'), 'no req frame');
  ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'text/plain' } });
  const response = await pending;
  const errored = assert.rejects(response.text(), 'a stalled body must error');

  // Fresh frames keep it alive: a chunk halfway through the window resets the timer.
  ms.send({ t: 'body', id: req.id, chunk: b64encode(bytes('one')), end: false });
  await new Promise((r) => setTimeout(r, 50));
  ms.send({ t: 'body', id: req.id, chunk: b64encode(bytes('two')), end: false });
  let dead = false;
  errored.then(() => {
    dead = true;
  }, () => {});
  await new Promise((r) => setTimeout(r, 50)); // 100ms after the first chunk: one full window
  assert.equal(dead, false, 'the body was killed despite fresh frames');

  // Then silence for a full window errors it and frees the slot.
  await within(errored, 'the stalled body never errored');
  assert.equal(relay.streams.size, 0, 'the stalled stream kept its slot');

  const { text } = await roundTrip(relay, ms);
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('a response buffered past maxBufferedBytes with no reader is errored and its slot freed', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ maxBufferedBytes: 1024 });
  await ms.connect(relay);

  const pending = within(relay.fetch(proxyRequest('/firehose')), 'firehose never answered');
  const req = await within(ms.next('req'), 'no req frame');
  ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'application/octet-stream' } });
  const response = await pending; // deliberately never read: every inbound chunk just queues

  // 512-byte chunks against a 1024-byte cap: the third chunk pushes the unread queue past it.
  for (let i = 0; i < 6; i++) ms.send({ t: 'body', id: req.id, chunk: b64encode(bytes('x'.repeat(512))), end: false });
  for (let i = 0; relay.streams.size > 0 && i < 100; i++) await new Promise((r) => setImmediate(r));
  assert.equal(relay.streams.size, 0, 'the unread firehose kept its slot');
  await assert.rejects(response.body.getReader().read(), 'the over-cap body must be errored');

  const { text } = await roundTrip(relay, ms);
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('a mothership ws_close whose code workerd would throw on is normalized to 1000', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  for (const [code, expected] of [[1000, 1000], [1005, 1000], [1006, 1000], [2999, 1000], [5000, 1000], [3000, 3000], [4999, 4999]]) {
    const open = within(relay.fetch(proxyRequest('/ws', { headers: { upgrade: 'websocket' } })), 'upgrade never finished');
    const wsOpen = await within(ms.next('ws_open'), 'no ws_open');
    assert.equal((await open).status, 101);
    const gone = closed(browserEnds.at(-1));
    ms.send({ t: 'ws_close', id: wsOpen.id, code });
    assert.equal((await gone).code, expected, `ws_close ${code} must close the browser ${expected}`);
  }
  ms.socket.close();
});

test('with no live tunnel the browser gets the offline page', async () => {
  const { relay } = makeDo();
  const response = await relay.fetch(proxyRequest('/anything?at=all'));
  assert.equal(response.status, 502);
  assert.equal(response.headers.get('cache-control'), 'no-store');
  const html = await response.text();
  assert.match(html, /This cockpit is not connected right now/);
  assert.match(html, /Start Colonizer on the machine with remote access turned on, then reload\./);
});

test('pings keep the tunnel alive and inbound pings are answered', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ pingMs: 20 });
  await ms.connect(relay);

  await within(ms.next('ping'), 'no periodic ping');
  ms.send({ t: 'ping' });
  await within(ms.next('pong'), 'inbound ping got no pong');

  await within(ms.next('ping'), 'no second periodic ping');
  ms.socket.close();
});

test('the per-install token bucket hands out 429 with a Retry-After when drained', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ rate: { capacity: 2, perSecond: 1 } });
  await ms.connect(relay);

  const pending = [
    within(relay.fetch(proxyRequest('/one')), 'one never finished'),
    within(relay.fetch(proxyRequest('/two')), 'two never finished'),
    within(relay.fetch(proxyRequest('/three')), 'three never finished'),
  ];
  for (const path of ['/one', '/two']) {
    const req = await within(ms.next('req'), `no req frame for ${path}`);
    ms.send({ t: 'res', id: req.id, status: 200, headers: {} });
    ms.send({ t: 'body', id: req.id, chunk: '', end: true });
  }
  assert.equal((await pending[0]).status, 200);
  assert.equal((await pending[1]).status, 200);

  const limited = await pending[2];
  assert.equal(limited.status, 429);
  assert.match(limited.headers.get('retry-after') ?? '', /^[1-9]\d*$/);
  assert.equal((await limited.text()).length < 200, true, 'the 429 body should be small');

  // The first two requests never got body frames here for the third request: none was created.
  ms.socket.close();
});

test('no response head within the timeout is a 504, and junk frames are ignored', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ responseTimeoutMs: 50 });
  await ms.connect(relay);

  const pending = within(relay.fetch(proxyRequest('/slow-head')), '504 never arrived');
  await within(ms.next('req'), 'no req frame');
  assert.equal((await pending).status, 504);

  // Unknown ids, malformed JSON, and junk types are all swallowed.
  ms.send({ t: 'res', id: 999, status: 200, headers: {} });
  ms.send({ t: 'body', id: 999, chunk: '', end: true });
  ms.raw('this is not json');
  ms.send({ t: 'mystery' });
  const { text } = await roundTrip(relay, ms);
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('an inbound body chunk that is too big or not base64 fails that stream only', async () => {
  const ms = await FakeMothership.create();
  const { relay } = makeDo();
  await ms.connect(relay);

  for (const [label, chunk] of [['oversized', 'x'.repeat(70000)], ['garbage', 'not base64!!']]) {
    const pending = within(relay.fetch(proxyRequest('/body')), 'stream never finished');
    const req = await within(ms.next('req'), 'no req frame');
    ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'text/plain' } });
    const response = await pending;
    const errored = assert.rejects(response.text(), `${label} chunk must error the body`);
    ms.send({ t: 'body', id: req.id, chunk, end: false });
    await within(errored, 'body did not error');
  }

  // The tunnel itself is none the worse for it.
  const { text } = await roundTrip(relay, ms);
  assert.equal(text, 'ok');
  ms.socket.close();
});

test('a finished stream logs one templated line, and bodies and storage never leak', async () => {
  const marker = 'STEALTH-marker-0xFF00AA';
  const markerB64 = b64encode(bytes(marker));
  const lines = [];
  const originals = {};
  for (const kind of ['log', 'info', 'warn', 'error']) {
    originals[kind] = console[kind];
    console[kind] = (...args) => lines.push(`${kind}: ${args.map(String).join(' ')}`);
  }

  let touched = [];
  try {
    const ms = await FakeMothership.create();
    const do_ = makeDo();
    touched = do_.touched;
    await ms.connect(do_.relay);

    const pending = within(
      do_.relay.fetch(proxyRequest('/quiet/77?token=SEKRET', { method: 'POST', body: `body with ${marker} inside` })),
      'round trip never finished',
    );
    const req = await within(ms.next('req'), 'no req frame');
    assert.deepEqual(await ms.body(req.id), [b64encode(bytes(`body with ${marker} inside`))]);
    ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'text/plain' } });
    ms.send({ t: 'body', id: req.id, chunk: b64encode(bytes(`response with ${marker} inside`)), end: true });
    const response = await pending;
    assert.equal(await response.text(), `response with ${marker} inside`);
    ms.socket.close();
  } finally {
    for (const [kind, original] of Object.entries(originals)) console[kind] = original;
  }

  // Exactly one line, in the documented shape, with the path templated and nothing else about the request.
  assert.equal(lines.length, 1);
  const entry = JSON.parse(lines[0].replace(/^\w+: /, ''));
  assert.deepEqual(Object.keys(entry), ['method', 'path', 'status', 'bytes_in', 'bytes_out', 'ms']);
  assert.deepEqual({ method: entry.method, path: entry.path, status: entry.status }, { method: 'POST', path: '/quiet/:id', status: 200 });
  assert.equal(entry.bytes_in, bytes(`body with ${marker} inside`).length);
  assert.equal(entry.bytes_out, bytes(`response with ${marker} inside`).length);
  assert.equal(typeof entry.ms, 'number');

  for (const line of lines) {
    assert.ok(!line.includes(marker), `log leaked the body marker: ${line}`);
    assert.ok(!line.includes(markerB64), 'log leaked the base64 of the body');
    assert.ok(!line.includes('SEKRET'), 'log leaked the query string');
  }
  assert.deepEqual(touched, []);
});

