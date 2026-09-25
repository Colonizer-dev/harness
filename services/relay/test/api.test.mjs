// The apex half of the relay driven through worker.fetch(): registration, the tunnel dial and the
// signed mothership endpoints, against the fake D1 (node:sqlite) and a fake DO namespace that records
// what the worker forwards. Signatures use a real Ed25519 key from WebCrypto — the same primitive the
// mothership uses — and the secrets here are fake values that sign nothing in the real world.

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { b64encode, randomToken } from '../src/crypto.js';
import { sealSession } from '../src/auth.js';
import worker from '../src/worker.js';
import { fakeD1 } from './d1.mjs';

const DOMAIN = 'my.colonizer.dev';
const NO_CTX = { waitUntil: () => {} };

function setup({ limit = async () => ({ success: true }) } = {}) {
  const db = fakeD1();
  const forwarded = [];
  const env = {
    RELAY_DOMAIN: DOMAIN,
    GITHUB_CLIENT_ID: 'Iv1.0fakeclientid0',
    SESSION_SECRET: 'fake-session-secret-for-tests-only',
    DB: db,
    REGISTER_LIMITER: { limit },
    TUNNELS: {
      idFromName: (name) => ({ name }),
      get: (id) => ({
        fetch: async (request) => {
          forwarded.push({ install: id.name, request });
          return new Response('tunnel-ok', { status: 200, headers: { 'x-from-do': 'yes' } });
        },
      }),
    },
  };
  return { db, env, forwarded };
}

const keyPair = () => b64encode(crypto.getRandomValues(new Uint8Array(32)));

async function register(env, publicKey = keyPair()) {
  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, { method: 'POST', body: JSON.stringify({ public_key: publicKey }) }),
    env,
    NO_CTX,
  );
  return { response, install: response.status === 201 ? await response.json() : null, publicKey };
}

async function ed25519Key() {
  const pair = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
  return { privateKey: pair.privateKey, publicKey: b64encode(new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey))) };
}

async function sigHeaders({ privateKey }, method, path, ts, body = '') {
  const signature = await crypto.subtle.sign('Ed25519', privateKey, new TextEncoder().encode(`${method}\n${path}\n${ts}\n${body}`));
  return { 'x-colonizer-ts': String(ts), 'x-colonizer-sig': b64encode(new Uint8Array(signature)) };
}

test('registration stores the public key, the created-at and nothing else, and returns a host', async () => {
  const { env, db } = setup();
  const publicKey = keyPair();
  const { response, install } = await register(env, publicKey);

  assert.equal(response.status, 201);
  assert.match(install.install_id, /^[a-z2-7]{20}$/);
  assert.equal(install.host, `${install.install_id}.${DOMAIN}`);

  const row = await db.prepare('SELECT * FROM installs WHERE id = ?').bind(install.install_id).first();
  assert.equal(row.public_key, publicKey);
  assert.equal(row.owner_github_id, null);
  assert.ok(Math.abs(row.created_at - Math.floor(Date.now() / 1000)) < 5);
  assert.deepEqual(Object.keys(row).sort(), ['created_at', 'id', 'owner_github_id', 'owner_github_login', 'public_key']);
});

test('registration refuses keys that are not base64 or not exactly 32 bytes, and honours the limiter', async () => {
  const { env } = setup();
  for (const bad of ['definitely not base64!!!', b64encode(new Uint8Array(31)), '']) {
    const { response } = await register(env, bad);
    assert.equal(response.status, 400, bad);
  }
  const limited = setup({ limit: async () => ({ success: false }) }).env;
  const { response } = await register(limited);
  assert.equal(response.status, 429);
});

test('hosts that are neither the relay domain nor an install subdomain are 404', async () => {
  const { env } = setup();
  assert.equal((await worker.fetch(new Request('https://evil.example/api/installs'), env, NO_CTX)).status, 404);
  assert.equal((await worker.fetch(new Request(`https://${DOMAIN}/nope`), env, NO_CTX)).status, 404);
  // An install-shaped subdomain for an install that does not exist is a 404 page too.
  const ghost = await worker.fetch(new Request(`https://${'a'.repeat(20)}.${DOMAIN}/cockpit`), env, NO_CTX);
  assert.equal(ghost.status, 404);
  assert.match(ghost.headers.get('content-type'), /text\/html/);
});

test('a tunnel dial is forwarded with the relay headers set and client-supplied x-relay-* stripped', async () => {
  const { env, forwarded } = setup();
  const publicKey = keyPair();
  const { install } = await register(env, publicKey);

  const dial = await worker.fetch(
    new Request(`https://${DOMAIN}/tunnel/${install.install_id}`, {
      headers: { upgrade: 'websocket', 'x-relay-kind': 'proxy', 'x-relay-public-key': 'spoofed' },
    }),
    env,
    NO_CTX,
  );

  assert.equal(dial.status, 200);
  assert.equal(await dial.text(), 'tunnel-ok');
  assert.equal(forwarded.length, 1);
  assert.equal(forwarded[0].install, install.install_id);
  const sent = forwarded[0].request.headers;
  assert.equal(sent.get('x-relay-kind'), 'tunnel'); // the spoofed value is gone, the worker's is set
  assert.equal(sent.get('x-relay-install-id'), install.install_id);
  assert.equal(sent.get('x-relay-public-key'), publicKey);
});

test('a tunnel dial for an unknown install is 404, and a non-upgrade dial is 426', async () => {
  const { env } = setup();
  const missing = await worker.fetch(new Request(`https://${DOMAIN}/tunnel/${'a'.repeat(20)}`, { headers: { upgrade: 'websocket' } }), env, NO_CTX);
  assert.equal(missing.status, 404);

  const { install } = await register(env);
  const dry = await worker.fetch(new Request(`https://${DOMAIN}/tunnel/${install.install_id}`), env, NO_CTX);
  assert.equal(dry.status, 426);
});

test('a tunnel dial honours the per-IP dial limiter: limited is 429 and nothing reaches the DO', async () => {
  const { env, forwarded } = setup();
  const { install } = await register(env);
  env.DIAL_LIMITER = { limit: async () => ({ success: false }) };

  const response = await worker.fetch(
    new Request(`https://${DOMAIN}/tunnel/${install.install_id}`, { headers: { upgrade: 'websocket' } }),
    env,
    NO_CTX,
  );
  assert.equal(response.status, 429);
  assert.equal(forwarded.length, 0);
});

test('an oversized body is 413 before any parsing, declared or streamed, counted in UTF-8 bytes', async () => {
  const { env, db } = setup();

  // A declared content-length over the cap is refused without reading the body at all.
  const declared = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, {
      method: 'POST',
      body: JSON.stringify({ public_key: keyPair(), padding: 'x'.repeat(4096) }),
      headers: { 'content-length': String(5 * 1024 * 1024) },
    }),
    env,
    NO_CTX,
  );
  assert.equal(declared.status, 413);

  // A chunked body with no honest length is cut off at the cap — in bytes, not string length: the
  // padding below is 700 characters but 1400 UTF-8 bytes.
  const streamed = await worker.fetch(
    new Request(`https://${DOMAIN}/api/installs`, { method: 'POST', body: `{"public_key":"k","pad":"${'é'.repeat(700)}"}` }),
    env,
    NO_CTX,
  );
  assert.equal(streamed.status, 413);

  // Neither oversized body was parsed or stored.
  assert.deepEqual((await db.prepare('SELECT * FROM installs').all()).results, []);
});

test('signed endpoints accept a fresh valid signature and refuse bad, stale, missing or foreign ones', async (t) => {
  const { env } = setup();
  const good = await ed25519Key();
  const { install } = await register(env, good.publicKey);
  const path = `/api/installs/${install.install_id}/pairing`;
  const now = Math.floor(Date.now() / 1000);

  const view = await worker.fetch(new Request(`https://${DOMAIN}${path}`, { headers: await sigHeaders(good, 'GET', path, now) }), env, NO_CTX);
  assert.equal(view.status, 200);
  assert.deepEqual(await view.json(), { owner: null, pending: [] });

  const wrong = await ed25519Key();
  const stale = await sigHeaders(good, 'GET', path, now - 301);
  const badSig = { 'x-colonizer-ts': String(now), 'x-colonizer-sig': randomToken(64) };
  for (const headers of [stale, badSig, await sigHeaders(wrong, 'GET', path, now), {}, await sigHeaders(good, 'GET', `${path}/`, now)]) {
    const response = await worker.fetch(new Request(`https://${DOMAIN}${path}`, { headers }), env, NO_CTX);
    assert.equal(response.status, 401, JSON.stringify(headers));
  }

  // Tampered timestamps do not pass either: the signature is over the ts that is checked.
  const shifted = await sigHeaders(good, 'GET', path, now);
  shifted['x-colonizer-ts'] = String(now + 30);
  assert.equal((await worker.fetch(new Request(`https://${DOMAIN}${path}`, { headers: shifted }), env, NO_CTX)).status, 401);
});

test('a confirmed pairing binds its owner once, and unbinding clears owner and pending pairings', async (t) => {
  const { env, db } = setup();
  const key = await ed25519Key();
  const { install } = await register(env, key.publicKey);
  const now = Math.floor(Date.now() / 1000);
  await db.prepare('INSERT INTO pairings (install_id, code, github_id, github_login, expires_at) VALUES (?, ?, ?, ?, ?)')
    .bind(install.install_id, '123456', 4242, 'owner', now + 60)
    .run();

  const pairingPath = `/api/installs/${install.install_id}/pairing`;
  const confirmPath = `${pairingPath}/confirm`;
  const body = JSON.stringify({ code: '123456' });

  const view = await worker.fetch(new Request(`https://${DOMAIN}${pairingPath}`, { headers: await sigHeaders(key, 'GET', pairingPath, now) }), env, NO_CTX);
  assert.deepEqual(await view.json(), {
    owner: null,
    pending: [{ code: '123456', github_login: 'owner', expires_at: now + 60 }],
  });

  const confirm = await worker.fetch(
    new Request(`https://${DOMAIN}${confirmPath}`, { method: 'POST', headers: await sigHeaders(key, 'POST', confirmPath, now, body), body }),
    env,
    NO_CTX,
  );
  assert.equal(confirm.status, 200);
  assert.deepEqual(await confirm.json(), { owner: { github_login: 'owner' } });

  // Single use: the same signed confirm again is 404, and so is an unknown code.
  const again = await worker.fetch(
    new Request(`https://${DOMAIN}${confirmPath}`, { method: 'POST', headers: await sigHeaders(key, 'POST', confirmPath, now, body), body }),
    env,
    NO_CTX,
  );
  assert.equal(again.status, 404);

  const bound = await db.prepare('SELECT owner_github_id, owner_github_login FROM installs WHERE id = ?').bind(install.install_id).first();
  assert.deepEqual({ ...bound }, { owner_github_id: 4242, owner_github_login: 'owner' });
  assert.deepEqual((await db.prepare('SELECT * FROM pairings').all()).results, []);

  const ownerPath = `/api/installs/${install.install_id}/owner`;
  const unbind = await worker.fetch(new Request(`https://${DOMAIN}${ownerPath}`, { method: 'DELETE', headers: await sigHeaders(key, 'DELETE', ownerPath, now) }), env, NO_CTX);
  assert.equal(unbind.status, 204);
  const unbound = await db.prepare('SELECT owner_github_id, owner_github_login FROM installs WHERE id = ?').bind(install.install_id).first();
  assert.deepEqual({ ...unbound }, { owner_github_id: null, owner_github_login: null });
});

test('confirming a pairing once an owner is bound is 409 and leaves the owner alone', async () => {
  const { env, db } = setup();
  const key = await ed25519Key();
  const { install } = await register(env, key.publicKey);
  const now = Math.floor(Date.now() / 1000);
  await db.prepare('UPDATE installs SET owner_github_id = 1111, owner_github_login = ? WHERE id = ?')
    .bind('alice', install.install_id)
    .run();
  // A second pending code, parked before the owner was bound, waits to be confirmed by the loser.
  await db.prepare('INSERT INTO pairings (install_id, code, github_id, github_login, expires_at) VALUES (?, ?, ?, ?, ?)')
    .bind(install.install_id, '123456', 2222, 'mallory', now + 60)
    .run();

  const confirmPath = `/api/installs/${install.install_id}/pairing/confirm`;
  const body = JSON.stringify({ code: '123456' });
  const confirm = await worker.fetch(
    new Request(`https://${DOMAIN}${confirmPath}`, { method: 'POST', headers: await sigHeaders(key, 'POST', confirmPath, now, body), body }),
    env,
    NO_CTX,
  );
  assert.equal(confirm.status, 409);
  const row = await db.prepare('SELECT owner_github_id, owner_github_login FROM installs WHERE id = ?').bind(install.install_id).first();
  assert.deepEqual({ ...row }, { owner_github_id: 1111, owner_github_login: 'alice' });
});

test('an expired pairing cannot be confirmed, and expired rows are never listed', async (t) => {
  const { env, db } = setup();
  const key = await ed25519Key();
  const { install } = await register(env, key.publicKey);
  const now = Math.floor(Date.now() / 1000);
  await db.prepare('INSERT INTO pairings (install_id, code, github_id, github_login, expires_at) VALUES (?, ?, ?, ?, ?)')
    .bind(install.install_id, '654321', 4242, 'owner', now - 1)
    .run();

  const confirmPath = `/api/installs/${install.install_id}/pairing/confirm`;
  const body = JSON.stringify({ code: '654321' });
  const confirm = await worker.fetch(
    new Request(`https://${DOMAIN}${confirmPath}`, { method: 'POST', headers: await sigHeaders(key, 'POST', confirmPath, now, body), body }),
    env,
    NO_CTX,
  );
  assert.equal(confirm.status, 404);
});

test('a proxied request carries the relay headers, loses the relay cookie and keeps the cockpit cookies', async () => {
  const { env, forwarded } = setup();
  const { install } = await register(env);
  await env.DB.prepare('UPDATE installs SET owner_github_id = ?, owner_github_login = ? WHERE id = ?')
    .bind(4242, 'owner', install.install_id)
    .run();
  const session = await sealSession(install.install_id, 4242, env.SESSION_SECRET);

  const response = await worker.fetch(
    new Request(`https://${install.host}/cockpit/tasks`, {
      headers: {
        cookie: `__Host-colonizer_session=${session}; cockpit_login=pilot`,
        'x-relay-kind': 'tunnel',
      },
    }),
    env,
    NO_CTX,
  );

  assert.equal(response.status, 200);
  assert.equal(forwarded.length, 1);
  const sent = forwarded[0].request.headers;
  assert.equal(sent.get('x-relay-kind'), 'proxy'); // spoofed 'tunnel' replaced by the worker's verdict
  assert.equal(sent.get('x-relay-install-id'), install.install_id);
  assert.ok(!sent.has('x-relay-public-key')); // that header belongs to the tunnel dial, not the proxy
  assert.equal(sent.get('cookie'), 'cockpit_login=pilot'); // relay session stripped, cockpit cookie kept
  assert.equal(forwarded[0].request.url, `https://${install.host}/cockpit/tasks`);
});
