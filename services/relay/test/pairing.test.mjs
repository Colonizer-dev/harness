// Pairing with the Colonizer pair code alone (#1086), driven through worker.fetch() against the fake D1
// and a fake DO namespace that records what the worker forwards. On an install with require_github off
// the relay forwards exactly three things without a GitHub session — an invite, its claim poll, and a
// link/phone credential — and every one of them is the mothership's to judge; the relay only throttles
// them. These are the negative tests the issue asks for: no credential never reaches the tunnel, a
// rejected credential gets the pair page at once, and the throttle holds per client and per install.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { sealSession } from '../src/auth.js';
import { b64encode } from '../src/crypto.js';
import { LIMITS } from '../src/throttle.js';
import { runtime } from '../src/runtime.js';
import worker from '../src/worker.js';
import { fakeD1 } from './d1.mjs';
import { FakeMothership, fakePair, makeDo, proxyRequest, within } from './fakes.mjs';

const DOMAIN = 'my.colonizer.dev';
const NO_CTX = { waitUntil: () => {} };
const HEX64 = 'a'.repeat(64);
const INVITE = `0123456789abcdef${'f'.repeat(48)}`;
const LINK = `clk_${'1'.repeat(64)}`;
const PHONE = `cph_${'2'.repeat(64)}`;
const PAIR = `ph_0123abcd.${'3'.repeat(64)}`;

/** The worker over a fake D1 and a recording DO namespace. `answer` decides what the "mothership"
 * says to each forwarded request; by default 200 with a body, and the rejection verdict for any
 * credential listed in `rejected`. */
function setup({ rejected = new Set() } = {}) {
  const db = fakeD1();
  const forwarded = [];
  const env = {
    RELAY_DOMAIN: DOMAIN,
    GITHUB_CLIENT_ID: 'Iv1.0fakeclientid0',
    GITHUB_CLIENT_SECRET: 'fake-client-secret-for-tests',
    SESSION_SECRET: 'fake-session-secret-for-tests-only',
    DB: db,
    TUNNELS: {
      idFromName: (name) => ({ name }),
      get: (id) => ({
        fetch: async (request) => {
          forwarded.push({ install: id.name, request });
          const cookie = request.headers.get('cookie') ?? '';
          const bearer = request.headers.get('authorization') ?? '';
          const pair = new URL(request.url).searchParams.get('pair');
          const dead = [...rejected].some((value) => cookie.includes(value) || bearer.includes(value) || pair === value);
          if (dead) {
            return new Response('{"error":"unauthorized"}', {
              status: 401,
              headers: { 'content-type': 'application/json', 'x-colonizer-credential': 'rejected' },
            });
          }
          return new Response('the cockpit', { status: 200, headers: { 'x-from-do': 'yes' } });
        },
      }),
    },
  };
  return { db, env, forwarded };
}

async function register(env, body = {}) {
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, {
      method: 'POST',
      body: JSON.stringify({ public_key: b64encode(crypto.getRandomValues(new Uint8Array(32))), ...body }),
    }),
    env,
    NO_CTX,
  );
  assert.equal(response.status, 201);
  return response.json();
}

async function signedInstall(env, body = {}) {
  const pair = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  const publicKey = b64encode(new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey)));
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, { method: 'POST', body: JSON.stringify({ public_key: publicKey, ...body }) }),
    env,
    NO_CTX,
  );
  const install = await response.json();
  const call = async (method, path, rawBody = '') => {
    const ts = Math.floor(Date.now() / 1000);
    const sig = await crypto.subtle.sign('Ed25519', pair.privateKey, new TextEncoder().encode(`${method}\n${path}\n${ts}\n${rawBody}`));
    const headers = { 'x-colonizer-ts': String(ts), 'x-colonizer-sig': b64encode(new Uint8Array(sig)) };
    return worker.fetch(new Request(`https://${DOMAIN}${path}`, { method, headers, body: rawBody || undefined }), env, NO_CTX);
  };
  return { install, call };
}

const browse = (env, host, path, { method = 'GET', headers = {}, ip = '198.51.100.7', body } = {}) =>
  worker.fetch(new Request(`https://${host}${path}`, { method, headers: { 'cf-connecting-ip': ip, ...headers }, body }), env, NO_CTX);

const setCookies = (response) => response.headers.getSetCookie();

test('a new install pairs with the code alone; with no invite and no credential nothing reaches the tunnel', async () => {
  const { env, forwarded } = setup();
  const install = await register(env);
  assert.equal(install.require_github, false);

  // A page load: the relay's own "Pair this device" page, 401, never a GitHub redirect.
  const page = await browse(env, install.host, '/cockpit/colonies');
  assert.equal(page.status, 401);
  assert.equal(page.headers.get('location'), null);
  assert.equal(page.headers.get('cache-control'), 'no-store');
  const html = await page.text();
  assert.match(html, /Pair this device/);
  assert.match(html, /Settings → Remote access →\s+Sign in on another device/);
  assert.doesNotMatch(html, /github/i);

  // Not-quite pass-throughs: every shape the relay does not recognise stops here too.
  const refused = [
    ['/api/remote', {}],
    ['/ws', { headers: { upgrade: 'websocket' } }],
    ['/api/tasks', { method: 'POST', body: 'x' }],
    ['/?pair=not-an-invite', {}],
    [`/?pair=${INVITE}&pair=${INVITE}`, {}], // two invites: ambiguous, not forwarded
    [`/cockpit?pair=${INVITE}`, {}], // an invite opens only at the root
    [`/?pair=${INVITE}`, { method: 'HEAD' }], // a HEAD would not spend it, and is not forwarded
    [`/?pair=${INVITE}`, { method: 'POST', body: 'x' }],
    ['/', { headers: { cookie: 'colonizer_token=clk_not-hex' } }],
    ['/', { headers: { cookie: `colonizer_token=${HEX64}` } }], // an install token never crosses (R3)
    ['/', { headers: { cookie: `colonizer_token=col_${HEX64}` } }], // a scoped API token is no credential here
    ['/', { headers: { authorization: `Bearer ${HEX64}` } }],
    ['/', { headers: { cookie: `other=${LINK}` } }], // a credential under any other cookie name
    ['/api/phone/claim', { method: 'POST' }], // a claim with no pairing cookie
    ['/api/phone/claim', { method: 'POST', headers: { cookie: 'colonizer_pair=forged' } }],
    ['/api/phone/claim', { headers: { cookie: `colonizer_pair=${PAIR}` } }], // the claim is a POST
    ['/', { headers: { 'x-relay-kind': 'proxy', 'x-relay-install-id': install.install_id } }],
  ];
  for (const [path, init] of refused) {
    const response = await browse(env, install.host, path, init);
    assert.equal(response.status, 401, `${init.method ?? 'GET'} ${path} ${JSON.stringify(init.headers ?? {})}`);
  }
  assert.equal(forwarded.length, 0, 'nothing reached the mothership without an invite or a credential');
});

test('an invite, its claim and a link or phone credential are forwarded, untrusted and stripped of relay state', async () => {
  const { env, forwarded } = setup();
  const install = await register(env);
  const asProxied = () => forwarded.at(-1).request;

  const invite = await browse(env, install.host, `/?pair=${INVITE}`, {
    headers: { 'x-relay-kind': 'tunnel', 'x-relay-public-key': 'forged', cookie: '__Host-colonizer_session=forged.tag; keep=1' },
  });
  assert.equal(invite.status, 200);
  assert.equal(await invite.text(), 'the cockpit');
  assert.equal(asProxied().headers.get('x-relay-kind'), 'proxy');
  assert.equal(asProxied().headers.get('x-relay-install-id'), install.install_id);
  assert.equal(asProxied().headers.get('x-relay-public-key'), null);
  assert.equal(asProxied().headers.get('cookie'), 'keep=1');
  assert.match(asProxied().headers.get('x-relay-client'), /^[A-Za-z0-9_-]{22}$/);
  assert.equal(new URL(asProxied().url).search, `?pair=${INVITE}`);

  const claim = await browse(env, install.host, '/api/phone/claim', { method: 'POST', headers: { cookie: `colonizer_pair=${PAIR}` } });
  assert.equal(claim.status, 200);
  assert.equal(asProxied().method, 'POST');

  for (const headers of [
    { cookie: `colonizer_token=${LINK}` },
    { cookie: `a=b; colonizer_token=${PHONE}` },
    { authorization: `Bearer ${LINK}` },
  ]) {
    const before = forwarded.length;
    const response = await browse(env, install.host, '/api/remote', { headers });
    assert.equal(response.status, 200, JSON.stringify(headers));
    assert.equal(forwarded.length, before + 1);
  }
  // A websocket with a credential goes through too (the fake answers it with a plain 200 here).
  const before = forwarded.length;
  await browse(env, install.host, '/api/stream', { headers: { upgrade: 'websocket', cookie: `colonizer_token=${LINK}` } });
  assert.equal(forwarded.length, before + 1);
  // The client key is an HMAC, never the address.
  assert.ok(!forwarded.some(({ request }) => (request.headers.get('x-relay-client') ?? '').includes('198.51.100.7')));
});

test('a credential the mothership rejects gets the pair page at once, its cookie cleared, and the verdict never leaks', async () => {
  const { env, db, forwarded } = setup({ rejected: new Set([LINK, INVITE]) });
  const install = await register(env);

  // A revoked clk_ on a page load: the pair page within that one request, and the dead cookie cleared.
  const page = await browse(env, install.host, '/', { headers: { cookie: `colonizer_token=${LINK}` } });
  assert.equal(page.status, 401);
  assert.match(await page.text(), /Pair this device/);
  assert.equal(page.headers.get('x-colonizer-credential'), null);
  assert.deepEqual(setCookies(page), ['colonizer_token=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0']);
  assert.equal(forwarded.length, 1);
  // With the cookie gone, the browser's next request stops at the relay.
  assert.equal((await browse(env, install.host, '/')).status, 401);
  assert.equal(forwarded.length, 1);

  // On an API call the mothership's own answer stands (the cockpit knows what a 401 means), minus the
  // verdict, and the cookie is cleared all the same.
  const api = await browse(env, install.host, '/api/remote', { headers: { cookie: `colonizer_token=${LINK}` } });
  assert.equal(api.status, 401);
  assert.equal(await api.text(), '{"error":"unauthorized"}');
  assert.equal(api.headers.get('x-colonizer-credential'), null);
  assert.equal(setCookies(api).length, 1);

  // A bearer is not a cookie: nothing to clear.
  const bearer = await browse(env, install.host, '/api/remote', { headers: { authorization: `Bearer ${LINK}` } });
  assert.equal(bearer.status, 401);
  assert.deepEqual(setCookies(bearer), []);

  // A spent or wrong invite: the pair page saying so.
  const spent = await browse(env, install.host, `/?pair=${INVITE}`);
  assert.equal(spent.status, 401);
  assert.match(await spent.text(), /already used/);

  // Every rejection counted, against this install and this client, and the rows hold no address.
  const rows = await db.prepare("SELECT key, count FROM throttle WHERE key LIKE 'fail:%' ORDER BY key").all();
  assert.deepEqual(
    rows.results.map((row) => row.count),
    [4, 4],
  );
  assert.ok(rows.results.some((row) => row.key === `fail:install:${install.install_id}`));
  assert.ok(!db.bound.some(({ values }) => values.some((value) => String(value).includes('198.51.100.7'))), 'no IP is ever stored');

  // An accepted answer passes the verdict-free response through untouched, and a stray verdict header
  // on a session-forwarded answer is stripped too.
  const fine = await browse(env, install.host, '/api/remote', { headers: { cookie: `colonizer_token=${PHONE}` } });
  assert.equal(fine.status, 200);
  assert.equal(fine.headers.get('x-from-do'), 'yes');
});

test('invite opens are throttled per client and per install, and nothing is forwarded past the limit', async () => {
  const { env, db, forwarded } = setup();
  const install = await register(env);
  const open = (ip) => browse(env, install.host, `/?pair=${INVITE}`, { ip });

  const perClient = LIMITS.invite.client.limit;
  for (let i = 0; i < perClient; i++) assert.equal((await open('203.0.113.1')).status, 200);
  const limited = await open('203.0.113.1');
  assert.equal(limited.status, 429);
  assert.match(await limited.text(), /Too many attempts/);
  assert.equal(limited.headers.get('retry-after'), String(LIMITS.invite.client.seconds));
  assert.equal(forwarded.length, perClient, 'the throttled open never reached the mothership');

  // Another client is not held back by the first one's count...
  assert.equal((await open('203.0.113.2')).status, 200);
  // ...until the install's own ceiling is reached, from however many clients.
  let ip = 3;
  while (forwarded.length < LIMITS.invite.install.limit) {
    assert.equal((await open(`203.0.113.${ip++}`)).status, 200);
  }
  assert.equal((await open(`203.0.113.${ip++}`)).status, 429);
  assert.equal(forwarded.length, LIMITS.invite.install.limit);

  // Another install is not held by this one's ceiling; a client's own count spans every install.
  const other = await register(env);
  assert.equal((await browse(env, other.host, `/?pair=${INVITE}`, { ip: '203.0.113.250' })).status, 200);
  assert.equal((await browse(env, other.host, `/?pair=${INVITE}`, { ip: '203.0.113.1' })).status, 429);

  // Once the window ends, opens are allowed again.
  await db.prepare('UPDATE throttle SET window_ends = ?').bind(Math.floor(Date.now() / 1000) - 1).run();
  assert.equal((await open('203.0.113.1')).status, 200);
});

test('rejected credentials are throttled per client and per install; the GitHub owner session is not', async () => {
  const { env, db, forwarded } = setup({ rejected: new Set([LINK]) });
  const install = await register(env);
  const tryLink = (ip, path = '/api/remote') => browse(env, install.host, path, { ip, headers: { cookie: `colonizer_token=${LINK}` } });

  for (let i = 0; i < LIMITS.fail.client.limit; i++) assert.equal((await tryLink('192.0.2.1')).status, 401);
  const before = forwarded.length;
  const limited = await tryLink('192.0.2.1');
  assert.equal(limited.status, 429);
  assert.match(await limited.text(), /too many/);
  // A failing client is held back from opening invites too, and from presenting a good credential.
  assert.equal((await browse(env, install.host, `/?pair=${INVITE}`, { ip: '192.0.2.1' })).status, 429);
  assert.equal((await browse(env, install.host, '/', { ip: '192.0.2.1', headers: { cookie: `colonizer_token=${PHONE}` } })).status, 429);
  assert.equal(forwarded.length, before, 'nothing forwarded while throttled');
  // Another client still gets through.
  assert.equal((await tryLink('192.0.2.2')).status, 401);
  assert.equal(forwarded.length, before + 1);

  // The install's ceiling, reached from many clients, holds the pass-through for everyone...
  await db.prepare('UPDATE throttle SET count = ? WHERE key = ?').bind(LIMITS.fail.install.limit, `fail:install:${install.install_id}`).run();
  assert.equal((await tryLink('192.0.2.99', '/')).status, 429);
  assert.equal(forwarded.length, before + 1);
  // ...but not the bound owner's GitHub session, which is the recovery path and never touches it.
  await db.prepare('UPDATE installs SET owner_github_id = 4242, owner_github_login = ? WHERE id = ?').bind('owner', install.install_id).run();
  const session = await sealSession(install.install_id, 4242, env.SESSION_SECRET);
  const owner = await browse(env, install.host, '/', { ip: '192.0.2.1', headers: { cookie: `__Host-colonizer_session=${session}` } });
  assert.equal(owner.status, 200);
  assert.equal(forwarded.length, before + 2);
});

test('the mothership switches the GitHub gate with a signed PUT …/settings, and the old gate comes back as it was', async () => {
  const { env, forwarded } = setup();
  const { install, call } = await signedInstall(env);
  const path = `/api/installs/${install.install_id}/settings`;

  assert.equal((await call('GET', `/api/installs/${install.install_id}/pairing`).then((r) => r.json())).require_github, false);

  // Unsigned, badly signed and malformed are refused, and change nothing.
  const unsigned = await worker.fetch(new Request(`https://${DOMAIN}${path}`, { method: 'PUT', body: '{"require_github":true}' }), env, NO_CTX);
  assert.equal(unsigned.status, 401);
  for (const body of ['{"require_github":"yes"}', '{}', 'nope']) assert.equal((await call('PUT', path, body)).status, 400, body);
  assert.equal((await browse(env, install.host, '/')).status, 401, 'still pair-code');

  const on = await call('PUT', path, '{"require_github":true}');
  assert.equal(on.status, 200);
  assert.deepEqual(await on.json(), { require_github: true });
  assert.equal((await call('GET', `/api/installs/${install.install_id}/pairing`).then((r) => r.json())).require_github, true);
  // With the gate on, even an invite or a credential goes to GitHub sign-in first.
  const invite = await browse(env, install.host, `/?pair=${INVITE}`);
  assert.equal(invite.status, 302);
  assert.equal(invite.headers.get('location'), `/_auth?next=${encodeURIComponent(`/?pair=${INVITE}`)}`);
  assert.equal((await browse(env, install.host, '/api/remote', { headers: { cookie: `colonizer_token=${LINK}` } })).status, 302);
  assert.equal((await browse(env, install.host, '/ws', { headers: { upgrade: 'websocket', cookie: `colonizer_token=${LINK}` } })).status, 401);
  assert.equal(forwarded.length, 0);

  const off = await call('PUT', path, '{"require_github":false}');
  assert.deepEqual(await off.json(), { require_github: false });
  assert.equal((await browse(env, install.host, '/api/remote', { headers: { cookie: `colonizer_token=${LINK}` } })).status, 200);
});

test('an install registered before the switch existed keeps the GitHub gate', async () => {
  const { env, db } = setup();
  // What the 0002 migration leaves an existing row with: the column's default.
  await db.prepare('INSERT INTO installs (id, public_key, created_at) VALUES (?, ?, ?)').bind('aaaaaaaaaaaaaaaaaaaa', 'k', 1).run();
  const row = await db.prepare('SELECT require_github FROM installs WHERE id = ?').bind('aaaaaaaaaaaaaaaaaaaa').first();
  assert.equal(row.require_github, 1);
  const response = await browse(env, `aaaaaaaaaaaaaaaaaaaa.${DOMAIN}`, `/?pair=${INVITE}`);
  assert.equal(response.status, 302);
});

test('retiring an install drops its throttle rows with it', async () => {
  const { env, db } = setup({ rejected: new Set([LINK]) });
  const { install, call } = await signedInstall(env);
  await browse(env, install.host, `/?pair=${INVITE}`);
  await browse(env, install.host, '/', { headers: { cookie: `colonizer_token=${LINK}` } });
  const ofInstall = () => db.prepare('SELECT key FROM throttle WHERE key LIKE ?').bind(`%:install:${install.install_id}`).all();
  assert.equal((await ofInstall()).results.length, 2);
  assert.equal((await call('DELETE', `/api/installs/${install.install_id}`)).status, 204);
  assert.equal((await ofInstall()).results.length, 0);
});

test('a websocket the mothership closes as rejected (4401) is counted by the DO, and the close reaches the browser', async () => {
  const browserEnds = [];
  runtime.pair = () => {
    const pair = fakePair();
    browserEnds.push(pair[0]);
    return pair;
  };
  runtime.upgrade = (client) => ({ status: 101, webSocket: client });
  const db = fakeD1();
  const ms = await FakeMothership.create();
  const { relay } = makeDo({ env: { DB: db } });
  await ms.connect(relay);

  const open = relay.fetch(proxyRequest('/api/stream', { headers: { upgrade: 'websocket', 'x-relay-client': 'clientkey0123456789abc' } }));
  const wsOpen = await within(ms.next('ws_open'), 'no ws_open');
  assert.equal((await open).status, 101);
  assert.equal(Object.fromEntries(wsOpen.headers)['x-relay-client'], undefined, 'the client key never reaches the mothership');
  const browser = browserEnds.at(-1);
  const closed = new Promise((resolve) => browser.addEventListener('close', resolve));
  ms.send({ t: 'ws_close', id: wsOpen.id, code: 4401 });
  assert.equal((await within(closed, 'the browser socket never closed')).code, 4401);
  await within(
    (async () => {
      while (!(await db.prepare("SELECT 1 FROM throttle WHERE key = 'fail:client:clientkey0123456789abc'").first())) {
        await new Promise((r) => setImmediate(r));
      }
    })(),
    'the rejection was never counted',
  );

  // A socket the worker did not tag (a GitHub-session one) is not counted, and an ordinary close is not either.
  const plain = relay.fetch(proxyRequest('/api/stream', { headers: { upgrade: 'websocket' } }));
  const wsPlain = await within(ms.next('ws_open'), 'no second ws_open');
  await plain;
  ms.send({ t: 'ws_close', id: wsPlain.id, code: 4401 });
  const tagged = relay.fetch(proxyRequest('/api/stream', { headers: { upgrade: 'websocket', 'x-relay-client': 'otherclient0123456789a' } }));
  const wsTagged = await within(ms.next('ws_open'), 'no third ws_open');
  await tagged;
  ms.send({ t: 'ws_close', id: wsTagged.id, code: 1000 });
  await new Promise((r) => setTimeout(r, 50));
  const rows = await db.prepare('SELECT key, count FROM throttle').all();
  assert.deepEqual(
    rows.results.map((row) => [row.key, row.count]).sort(),
    [
      ['fail:client:clientkey0123456789abc', 1],
      ['fail:install:11111111-2222-4333-8444-555555555555', 1],
    ],
  );
  ms.socket.close();
});
