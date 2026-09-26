// The relay end to end: the real worker.fetch in front of the real InstallTunnel (through a fake DO
// namespace whose instances come from makeDo), the fake D1, and a FakeMothership — registration, the
// tunnel handshake, owner sign-in with GitHub stubbed, a proxied round trip, the offline page. The
// issue's acceptance test runs last: across the whole flow, not a byte of either body reaches the
// logs, a D1 bind, or DO storage — which records zero calls at all.

import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { b64encode } from '../src/crypto.js';
import { runtime } from '../src/runtime.js';
import worker from '../src/worker.js';
import { fakeD1 } from './d1.mjs';
import { FakeMothership, fakePair, makeDo, within } from './fakes.mjs';

const DOMAIN = 'my.colonizer.dev';
const NO_CTX = { waitUntil: () => {} };
const ENCODER = new TextEncoder();
const MARKER_IN = 'e2e-request-marker-9f2ce1b7a3d5-never-logged';
const MARKER_OUT = 'e2e-response-marker-7c1e4a9f2b6d5-never-logged';

// State one shared flow builds up, in order, across the tests below.
const ctx = { lines: [], chunkSamples: [] };
const realConsole = new Map();

// Node has no WebSocketPair and cannot build a 101, so the runtime seams are replaced for this file;
// console is captured for the no-bodies assertion. Both go back in after().
const realPair = runtime.pair;
const realUpgrade = runtime.upgrade;
runtime.pair = fakePair;
runtime.upgrade = (client) => ({ status: 101, webSocket: client });

before(async () => {
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    realConsole.set(level, console[level]);
    console[level] = (...args) => ctx.lines.push(`${level} ${args.join(' ')}`);
  }
  ctx.ms = await FakeMothership.create();
  ctx.db = fakeD1();
  const cache = new Map();
  ctx.env = {
    RELAY_DOMAIN: DOMAIN,
    GITHUB_CLIENT_ID: 'Iv1.0fakeclientid0',
    GITHUB_CLIENT_SECRET: 'fake-client-secret-for-tests',
    SESSION_SECRET: 'fake-session-secret-for-tests-only',
    DB: ctx.db,
    // A fake namespace over the real class: one makeDo per install id, cached, with the instance's
    // fetch exposed the way the runtime would call it. `do` hands tests the tunnel and its storage.
    TUNNELS: {
      idFromName: (name) => name,
      get(id) {
        if (!cache.has(id)) cache.set(id, makeDo());
        const made = cache.get(id);
        return { fetch: (request) => made.relay.fetch(request), do: made };
      },
    },
  };
  ctx.dos = cache;
});

after(() => {
  for (const [level, real] of realConsole) console[level] = real;
  runtime.pair = realPair;
  runtime.upgrade = realUpgrade;
});

const get = (url, headers = {}) => worker.fetch(new Request(url, { headers }), ctx.env, NO_CTX);

// The signed-endpoint headers the mothership sends: Ed25519 over METHOD\npath\nts\nbody, its key.
async function sigHeaders(method, path, body = '') {
  const ts = Math.floor(Date.now() / 1000);
  const sig = await crypto.subtle.sign('Ed25519', ctx.ms.keys.privateKey, ENCODER.encode(`${method}\n${path}\n${ts}\n${body}`));
  return { 'x-colonizer-ts': String(ts), 'x-colonizer-sig': b64encode(new Uint8Array(sig)) };
}

function stubGitHub(t) {
  return t.mock.method(globalThis, 'fetch', async (url) => {
    const json = (body) => new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
    if (String(url) === 'https://github.com/login/oauth/access_token') return json({ access_token: 'fake-access-token' });
    if (String(url) === 'https://api.github.com/user') return json({ id: 4242, login: 'owner' });
    return new Response('unexpected fetch', { status: 500 });
  });
}

// One browser pass through /_auth on an install host: the 302 to GitHub, then the replayed callback.
async function browserSignIn(host) {
  const start = await get(`https://${host}/_auth`);
  assert.equal(start.status, 302);
  const oauth = start.headers.getSetCookie().find((c) => c.startsWith('__Host-colonizer_oauth=')).split(';')[0];
  const state = encodeURIComponent(new URL(start.headers.get('location')).searchParams.get('state'));
  return get(`https://${host}/_auth/callback?code=fake-code&state=${state}`, { cookie: oauth });
}

// The base64 fragments that encode `marker` at each of the three byte phases of an enclosing stream:
// wherever the marker sits in a body, if that body is ever base64'd, one of these is a substring of
// it. Each window keeps only the characters that encode marker bits alone.
function b64Windows(marker) {
  return [0, 1, 2].map((lead) => {
    const stream = b64encode(ENCODER.encode('¥'.repeat(lead) + marker + '¥¥¥'));
    return stream.slice(lead ? Math.ceil((lead * 8) / 6) : 0, Math.floor((marker.length * 8) / 6));
  });
}

test('an install registers, and the mothership dials /tunnel/<id> through the worker and completes the hello', async () => {
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, { method: 'POST', body: JSON.stringify({ public_key: ctx.ms.publicKeyB64 }) }),
    ctx.env,
    NO_CTX,
  );
  assert.equal(response.status, 201);
  ctx.install = await response.json();
  assert.equal(ctx.install.host, `${ctx.install.install_id}.${DOMAIN}`);

  // The dial runs through the real worker (which tags it from D1); only the 101 hop itself is faked.
  const handle = ctx.env.TUNNELS.get(ctx.install.install_id);
  const shell = {
    fetch: (request) => worker.fetch(new Request(request.url, { headers: { upgrade: 'websocket' } }), ctx.env, NO_CTX),
    get tunnel() {
      return handle.do.relay.tunnel;
    },
  };
  await ctx.ms.connect(shell, ctx.install.install_id);
});

test('the owner signs in first: GitHub is stubbed, the unowned install parks the account behind a code', async (t) => {
  stubGitHub(t);
  const callback = await browserSignIn(ctx.install.host);
  assert.equal(callback.status, 200);
  ctx.pairingCode = /pairing-code[^>]*>(\d{6})</.exec(await callback.text())?.[1];
  assert.match(ctx.pairingCode, /^\d{6}$/);
});

test('the cockpit confirms the code via the signed endpoint, and a second sign-in mints the session cookie', async (t) => {
  stubGitHub(t);
  const confirmPath = `/api/installs/${ctx.install.install_id}/pairing/confirm`;
  const body = JSON.stringify({ code: ctx.pairingCode });
  const request = new Request(`https://${DOMAIN}${confirmPath}`, { method: 'POST', headers: await sigHeaders('POST', confirmPath, body), body });
  const confirm = await worker.fetch(request, ctx.env, NO_CTX);
  assert.equal(confirm.status, 200);
  assert.deepEqual(await confirm.json(), { owner: { github_login: 'owner' } });
  const bound = await ctx.db.prepare('SELECT owner_github_id FROM installs WHERE id = ?').bind(ctx.install.install_id).first();
  assert.equal(bound.owner_github_id, 4242);

  const done = await browserSignIn(ctx.install.host); // the owner again: this time a session, not a code
  assert.equal(done.status, 302);
  ctx.sessionCookie = done.headers.getSetCookie().find((c) => c.startsWith('__Host-colonizer_session=')).split(';')[0];
  assert.match(ctx.sessionCookie, /^__Host-colonizer_session=[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/);
});

test('a signed-in POST round-trips through the DO to the mothership and back, both bodies over 48 KiB', async () => {
  const requestBody = `${'q'.repeat(51)}${MARKER_IN}${'q'.repeat(60000)}`; // marker inside the first chunk
  const request = new Request(`https://${ctx.install.host}/cockpit/api/tasks`, {
    method: 'POST',
    headers: { cookie: ctx.sessionCookie },
    body: requestBody,
    duplex: 'half',
  });
  const pending = worker.fetch(request, ctx.env, NO_CTX);

  const req = await within(ctx.ms.next('req'), 'the req frame never reached the mothership');
  assert.equal(req.method, 'POST');
  assert.equal(req.path, '/cockpit/api/tasks');
  const chunks = await ctx.ms.body(req.id);
  assert.equal(Buffer.concat(chunks.map((c) => Buffer.from(c, 'base64'))).toString(), requestBody);
  assert.ok(chunks[0].includes(b64Windows(MARKER_IN)[0])); // the marker really is in the base64
  ctx.chunkSamples.push(chunks[0].slice(1000, 1200));

  const head = new Uint8Array(36864).fill(0x72); // one full max-size chunk
  const tail = ENCODER.encode(`${'r'.repeat(99)}${MARKER_OUT}${'r'.repeat(19000)}`);
  const tailB64 = b64encode(tail);
  assert.ok(tailB64.includes(b64Windows(MARKER_OUT)[0]));
  ctx.chunkSamples.push(tailB64.slice(1000, 1200));
  ctx.ms.send({ t: 'res', id: req.id, status: 200, headers: { 'content-type': 'application/json' } });
  ctx.ms.send({ t: 'body', id: req.id, chunk: b64encode(head), end: false });
  ctx.ms.send({ t: 'body', id: req.id, chunk: tailB64, end: true });

  const response = await pending;
  assert.equal(response.status, 200);
  const text = await response.text();
  assert.equal(text.length, head.length + tail.length);
  assert.ok(text.includes(MARKER_OUT));
});

test('with the tunnel gone, a signed-in request gets the 502 offline page, not an error', async () => {
  ctx.ms.socket.close(1000, 'bye');
  const gone = await get(`https://${ctx.install.host}/cockpit/tasks`, { cookie: ctx.sessionCookie });
  assert.equal(gone.status, 502);
  assert.match(await gone.text(), /This cockpit is not connected right now/);
});

test('acceptance: no body bytes reached the logs, D1 or DO storage — which saw zero calls at all', () => {
  assert.ok(ctx.lines.length > 0); // the flow was watched
  assert.ok(ctx.db.bound.length > 0); // and D1's recorder ran
  const seen = [ctx.lines.join('\n'), JSON.stringify(ctx.db.bound), ...[...ctx.dos.values()].map((m) => JSON.stringify(m.touched))].join('\n');
  const needles = [MARKER_IN, MARKER_OUT, ...b64Windows(MARKER_IN), ...b64Windows(MARKER_OUT), ...ctx.chunkSamples];
  for (const needle of needles) {
    assert.ok(!seen.includes(needle), `body bytes leaked: ${JSON.stringify(needle.slice(0, 48))}`);
  }
  // The one thing the tunnel logs is the fixed metadata struct — method, path template, status, bytes.
  for (const line of ctx.lines.filter((l) => l.startsWith('log '))) {
    assert.deepEqual(Object.keys(JSON.parse(line.slice(4))).sort(), ['bytes_in', 'bytes_out', 'method', 'ms', 'path', 'status']);
  }
  // Storage was never so much as touched: zero state calls on every tunnel DO.
  for (const made of ctx.dos.values()) assert.deepEqual(made.touched, []);
});
