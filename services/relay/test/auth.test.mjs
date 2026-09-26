// The #534 acceptance criteria, driven through worker.fetch() with GitHub stubbed on globalThis.fetch
// (fake client id, fake client secret, fake {id, login} — nothing here talks to the real GitHub), the
// fake D1 of test/d1.mjs, and a fake DO namespace that records what the worker forwards.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { OAUTH_COOKIE, SESSION_COOKIE, SESSION_SECONDS, STATE_SECONDS, readCookie, sealSession } from '../src/auth.js';
import { b64encode } from '../src/crypto.js';
import worker from '../src/worker.js';
import { fakeD1 } from './d1.mjs';

const DOMAIN = 'my.colonizer.dev';
const NO_CTX = { waitUntil: () => {} };
const SESSION_COOKIE_PATTERN = new RegExp(
  `^${SESSION_COOKIE}=[A-Za-z0-9_-]+\\.[A-Za-z0-9_-]+; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=${SESSION_SECONDS}$`,
);

function setup() {
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
          return new Response('tunnel-ok');
        },
      }),
    },
  };
  return { db, env, forwarded };
}

async function register(env, publicKey = b64encode(crypto.getRandomValues(new Uint8Array(32)))) {
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, { method: 'POST', body: JSON.stringify({ public_key: publicKey }) }),
    env,
    NO_CTX,
  );
  assert.equal(response.status, 201);
  return response.json();
}

async function ed25519Key() {
  const pair = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  return { privateKey: pair.privateKey, publicKey: b64encode(new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey))) };
}

async function sigHeaders({ privateKey }, method, path, ts, body = '') {
  const signature = await crypto.subtle.sign('Ed25519', privateKey, new TextEncoder().encode(`${method}\n${path}\n${ts}\n${body}`));
  return { 'x-colonizer-ts': String(ts), 'x-colonizer-sig': b64encode(new Uint8Array(signature)) };
}

const setCookie = (response, name) => response.headers.getSetCookie().find((c) => c.startsWith(`${name}=`)) ?? null;
const cookieValue = (setCookieValue) => setCookieValue.split(';')[0].split('=').slice(1).join('=');

function githubStub(t, user = { id: 4242, login: 'owner' }) {
  return t.mock.method(globalThis, 'fetch', async (url) => {
    const json = (body) => new Response(JSON.stringify(body), { headers: { 'content-type': 'application/json' } });
    if (String(url) === 'https://github.com/login/oauth/access_token') return json({ access_token: 'fake-access-token' });
    if (String(url) === 'https://api.github.com/user') return json(user);
    return new Response('unexpected fetch', { status: 500 });
  });
}

// Drives GET /_auth and returns the response (the GitHub 302) plus a callback runner that replays the
// state cookie against /_auth/callback with the state from the authorize URL.
async function signIn(env, host, next = '/cockpit') {
  const start = await worker.fetch(new Request(`https://${host}/_auth?next=${encodeURIComponent(next)}`), env, NO_CTX);
  assert.equal(start.status, 302);
  const stateCookie = setCookie(start, OAUTH_COOKIE);
  const state = new URL(start.headers.get('location')).searchParams.get('state');
  return {
    start,
    callback: (stateOverride) =>
      worker.fetch(
        new Request(`https://${host}/_auth/callback?code=fake-code&state=${stateOverride ?? state}`, {
          headers: { cookie: `${OAUTH_COOKIE}=${cookieValue(stateCookie)}` },
        }),
        env,
        NO_CTX,
      ),
  };
}

test('an unauthenticated browser is redirected to /_auth; websocket upgrades and non-GET get 401', async () => {
  const { env, forwarded } = setup();
  const install = await register(env);

  const get = await worker.fetch(new Request(`https://${install.host}/cockpit/colonies`), env, NO_CTX);
  assert.equal(get.status, 302);
  assert.equal(get.headers.get('location'), '/_auth?next=%2Fcockpit%2Fcolonies');
  assert.equal(setCookie(get, SESSION_COOKIE), null);

  assert.equal(
    (await worker.fetch(new Request(`https://${install.host}/ws`, { headers: { upgrade: 'websocket' } }), env, NO_CTX)).status,
    401,
  );
  assert.equal(
    (await worker.fetch(new Request(`https://${install.host}/api/tasks`, { method: 'POST', body: 'x' }), env, NO_CTX)).status,
    401,
  );
  assert.equal(forwarded.length, 0); // nothing reached the mothership unauthenticated
});

test('once an owner is bound, a different GitHub account gets a 403 page and no pairing is created', async (t) => {
  const { env, db } = setup();
  const install = await register(env);
  await db.prepare('UPDATE installs SET owner_github_id = 1111, owner_github_login = ? WHERE id = ?')
    .bind('alice', install.install_id)
    .run();
  githubStub(t, { id: 2222, login: 'mallory' });

  const { callback } = await signIn(env, install.host);
  const refused = await callback();
  assert.equal(refused.status, 403);
  assert.match(await refused.text(), /another GitHub account/);
  assert.equal(setCookie(refused, SESSION_COOKIE), null);
  assert.deepEqual((await db.prepare('SELECT * FROM pairings').all()).results, []);
});

test('the full flow: sign-in, code, signed confirm, sign-in again, session, proxied through to the DO', async (t) => {
  const { env, db, forwarded } = setup();
  const key = await ed25519Key();
  const install = await register(env, key.publicKey);
  const github = githubStub(t);

  // Sign in on the unbound install: the browser gets a pairing code to carry to the cockpit.
  const first = await signIn(env, install.host, '/cockpit');
  const authorize = new URL(first.start.headers.get('location'));
  assert.equal(authorize.origin, 'https://github.com');
  assert.equal(authorize.searchParams.get('client_id'), env.GITHUB_CLIENT_ID);
  assert.equal(authorize.searchParams.get('redirect_uri'), `https://${install.host}/_auth/callback`);
  assert.equal(authorize.searchParams.get('allow_signup'), 'false');
  assert.ok(/^[A-Za-z0-9_-]{20,}$/.test(authorize.searchParams.get('state')));

  const page = await first.callback();
  assert.equal(page.status, 200);
  const html = await page.text();
  assert.match(html, /Confirm this code in your local cockpit/);
  assert.match(html, new RegExp(`href="/_auth\\?next=%2Fcockpit"`));
  const code = html.match(/class="pairing-code"[^>]*>(\d{6})</)?.[1];
  assert.match(code, /^\d{6}$/);

  // The exchange used the fake client id/secret, and the token was used once, for /user, with a UA.
  const [tokenCall, userCall] = github.mock.calls.map((call) => call.arguments[1]);
  assert.deepEqual(JSON.parse(tokenCall.body), {
    client_id: env.GITHUB_CLIENT_ID,
    client_secret: env.GITHUB_CLIENT_SECRET,
    code: 'fake-code',
    redirect_uri: `https://${install.host}/_auth/callback`,
  });
  assert.equal(userCall.headers.authorization, 'Bearer fake-access-token');
  assert.equal(userCall.headers['user-agent'], 'colonizer-relay');

  // The local cockpit confirms the code with a signed request; the browser's code is single-use.
  const confirmPath = `/api/installs/${install.install_id}/pairing/confirm`;
  const confirm = async () => {
    const body = JSON.stringify({ code });
    return worker.fetch(
      new Request(`https://${DOMAIN}${confirmPath}`, {
        method: 'POST',
        headers: await sigHeaders(key, 'POST', confirmPath, Math.floor(Date.now() / 1000), body),
        body,
      }),
      env,
      NO_CTX,
    );
  };
  const bound = await confirm();
  assert.equal(bound.status, 200);
  assert.deepEqual(await bound.json(), { owner: { github_login: 'owner' } });
  assert.equal((await confirm()).status, 404); // already used

  // Sign in again: the owner is recognised, gets a session cookie, and lands on next.
  const second = await signIn(env, install.host, '/cockpit');
  const done = await second.callback();
  assert.equal(done.status, 302);
  assert.equal(done.headers.get('location'), '/cockpit');
  const session = setCookie(done, SESSION_COOKIE);
  assert.match(session, SESSION_COOKIE_PATTERN);
  assert.ok(setCookie(done, OAUTH_COOKIE)?.includes('Max-Age=0')); // the oauth cookie is cleared

  // A proxied request with the session reaches the fake DO: relay headers set, the relay's own cookie
  // stripped, the cockpit's own login cookie kept (its auth still applies behind the relay).
  const proxied = await worker.fetch(
    new Request(`https://${install.host}/api/colonies`, { headers: { cookie: `${SESSION_COOKIE}=${cookieValue(session)}; cockpit_login=pilot` } }),
    env,
    NO_CTX,
  );
  assert.equal(proxied.status, 200);
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0].request.headers.get('x-relay-kind'), 'proxy');
  assert.equal(forwarded[0].request.headers.get('x-relay-install-id'), install.install_id);
  assert.equal(forwarded[0].request.headers.get('cookie'), 'cockpit_login=pilot');
});

test('a pairing code stops working ten minutes after it was minted', async (t) => {
  const { env } = setup();
  const key = await ed25519Key();
  const install = await register(env, key.publicKey);
  githubStub(t);

  const { callback } = await signIn(env, install.host);
  const html = await (await callback()).text();
  const code = html.match(/class="pairing-code"[^>]*>(\d{6})</)[1];

  t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
  t.mock.timers.tick((STATE_SECONDS + 1) * 1000);

  const confirmPath = `/api/installs/${install.install_id}/pairing/confirm`;
  const body = JSON.stringify({ code });
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}${confirmPath}`, {
      method: 'POST',
      headers: await sigHeaders(key, 'POST', confirmPath, Math.floor(Date.now() / 1000), body),
      body,
    }),
    env,
    NO_CTX,
  );
  assert.equal(response.status, 404);
});

test('a relay without its secrets fails closed: a cookie sealed under "undefined" is never accepted', async (t) => {
  const { env, forwarded } = setup();
  delete env.SESSION_SECRET;
  const install = await register(env);

  // What TextEncoder used to make of an undefined key: a session anyone could forge for themselves.
  const forged = await sealSession(install.install_id, 4242, 'undefined');
  const get = await worker.fetch(
    new Request(`https://${install.host}/cockpit`, { headers: { cookie: `${SESSION_COOKIE}=${forged}` } }),
    env,
    NO_CTX,
  );
  assert.equal(get.status, 500);
  assert.match(await get.text(), /relay misconfigured/);
  assert.equal(forwarded.length, 0);

  // The OAuth paths fail closed too, before sealing a state cookie or talking to GitHub.
  assert.equal((await worker.fetch(new Request(`https://${install.host}/_auth`), env, NO_CTX)).status, 500);

  // And under the hood: no cookie verifies against a missing key at all.
  assert.equal(await readCookie(`${SESSION_COOKIE}=${forged}`, SESSION_COOKIE, undefined), null);
});

test('a session is bound to one install and one owner: other hosts, tampered cookies and unbinding all fail', async (t) => {
  const { env, db, forwarded } = setup();
  const key = await ed25519Key();
  const a = await register(env, key.publicKey);
  const b = await register(env);
  await db.prepare('UPDATE installs SET owner_github_id = 4242, owner_github_login = ? WHERE id = ?')
    .bind('owner', a.install_id)
    .run();
  const session = cookieValue(await sealSession(a.install_id, 4242, env.SESSION_SECRET));
  const tampered = session.replace(/.$/, (c) => (c === 'A' ? 'B' : 'A'));

  const get = (host, value) =>
    worker.fetch(new Request(`https://${host}/cockpit`, { headers: { cookie: `${SESSION_COOKIE}=${value}` } }), env, NO_CTX);

  // Minted for a's host: refused on b's host (redirected to sign-in, never forwarded)...
  assert.equal((await get(b.host, session)).status, 302);
  // ...and a tampered tag is refused on the right host too.
  assert.equal((await get(a.host, tampered)).status, 302);
  assert.equal(forwarded.length, 0);

  // Unbinding ("reset link" in the cockpit) kills the session on its next request.
  const ownerPath = `/api/installs/${a.install_id}/owner`;
  const unbind = await worker.fetch(
    new Request(`https://${DOMAIN}${ownerPath}`, { method: 'DELETE', headers: await sigHeaders(key, 'DELETE', ownerPath, Math.floor(Date.now() / 1000)) }),
    env,
    NO_CTX,
  );
  assert.equal(unbind.status, 204);
  assert.equal((await get(a.host, session)).status, 302);
  assert.equal(forwarded.length, 0);
});

test('cookies carry the __Host- shape exactly, and a wrong or expired oauth state is a 400', async (t) => {
  const { env } = setup();
  const install = await register(env);
  githubStub(t);

  const start = await worker.fetch(new Request(`https://${install.host}/_auth?next=/cockpit`), env, NO_CTX);
  const oauth = setCookie(start, OAUTH_COOKIE);
  assert.match(oauth, new RegExp(`^${OAUTH_COOKIE}=[^;]+; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=${STATE_SECONDS}$`));

  const withCookie = { headers: { cookie: `${OAUTH_COOKIE}=${cookieValue(oauth)}` } };
  assert.equal(
    (await worker.fetch(new Request(`https://${install.host}/_auth/callback?code=x&state=not-the-state`, withCookie), env, NO_CTX)).status,
    400,
  );
  assert.equal((await worker.fetch(new Request(`https://${install.host}/_auth/callback?code=x&state=anything`), env, NO_CTX)).status, 400);

  t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
  t.mock.timers.tick((STATE_SECONDS + 1) * 1000);
  assert.equal(
    (await worker.fetch(new Request(`https://${install.host}/_auth/callback?code=x&state=anything`, withCookie), env, NO_CTX)).status,
    400,
  );

  const out = await worker.fetch(
    new Request(`https://${install.host}/_auth/logout`, {
      method: 'POST',
      headers: { cookie: `${SESSION_COOKIE}=${await sealSession(install.install_id, 1, env.SESSION_SECRET)}` },
    }),
    env,
    NO_CTX,
  );
  assert.equal(out.status, 302);
  assert.equal(out.headers.get('location'), '/');
  assert.match(setCookie(out, SESSION_COOKIE), /Max-Age=0/);
});
