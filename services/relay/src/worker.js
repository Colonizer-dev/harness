// The relay worker (issues #532 and #534). Two kinds of host land here, matched case-insensitively:
//
//   my.colonizer.dev            the apex: install registration, the mothership's tunnel dial, and the
//                               signed mothership endpoints (pairing view + confirm, owner unbind);
//   <install>.my.colonizer.dev  one subdomain per install: owner sign-in under /_auth, everything else
//                               proxied to the install's tunnel DO once a valid session is present.
//
// Anything else is 404. Every request handed to a DO first loses all client-supplied x-relay-* headers
// — the worker is the only thing allowed to speak that dialect, so a browser cannot forge a tunnel or
// a proxy identity. Nothing about a request is stored: D1 holds the public key, the created-at and the
// owner binding (migrations/0001_installs.sql), and nothing else.

import { b64decode, b64encode, verifyEd25519 } from './crypto.js';
import { finishSignIn, logout, misconfigured, purgeExpiredPairings, readSession, relayConfigured, sessionOwns, startSignIn, stripSessionCookie } from './auth.js';
import { unknownInstallPage } from './pages.js';

export { InstallTunnel } from './tunnel.js';

const DOMAIN_LABEL = /^[a-z2-7]{16,}$/; // 20 random base32 chars in practice; 16+ leaves headroom
const PATH_INSTALL_ID = '([a-z2-7]{16,})';
const TUNNEL_PATH = new RegExp(`^/tunnel/${PATH_INSTALL_ID}$`);
const PAIRING_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/pairing$`);
const CONFIRM_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/pairing/confirm$`);
const OWNER_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/owner$`);

const BASE32 = 'abcdefghijklmnopqrstuvwxyz234567'; // 20 chars = 100 bits of install id
const PUBLIC_KEY_BYTES = 32;
const SIGNATURE_WINDOW_SECONDS = 300;
const MAX_JSON_BYTES = 1024;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const host = url.hostname.toLowerCase();
    const domain = (env.RELAY_DOMAIN ?? '').toLowerCase();

    if (host === domain) return apex(request, env, url);
    if (host.endsWith(`.${domain}`) && DOMAIN_LABEL.test(host.slice(0, -1 - domain.length))) {
      return installHost(request, env, url, host.slice(0, -1 - domain.length));
    }
    return json({ error: 'not found' }, 404);
  },
};

// ---------------------------------------------------------------- the apex: mothership-facing

async function apex(request, env, url) {
  const path = url.pathname;
  const method = request.method;
  let id;
  if (method === 'POST' && path === '/api/installs') return register(request, env);
  if (method === 'GET' && (id = TUNNEL_PATH.exec(path))) return dial(request, env, id[1]);
  if (method === 'GET' && (id = PAIRING_PATH.exec(path))) return signed(request, env, url, id[1], (install) => pairingView(env, install));
  if (method === 'POST' && (id = CONFIRM_PATH.exec(path))) {
    return signed(request, env, url, id[1], (install, rawBody) => confirmPairing(env, install, rawBody));
  }
  if (method === 'DELETE' && (id = OWNER_PATH.exec(path))) return signed(request, env, url, id[1], (install) => unbind(env, install));
  return json({ error: 'not found' }, 404);
}

async function register(request, env) {
  if (env.REGISTER_LIMITER) {
    const { success } = await env.REGISTER_LIMITER.limit({ key: request.headers.get('cf-connecting-ip') ?? 'unknown' });
    if (!success) return json({ error: 'slow down' }, 429);
  }
  const raw = await smallBody(request);
  if (raw === null) return json({ error: 'body too large' }, 413);
  let body;
  try {
    body = JSON.parse(raw);
  } catch {
    return json({ error: 'body must be JSON' }, 400);
  }
  let key;
  try {
    key = b64decode(typeof body?.public_key === 'string' ? body.public_key : '');
  } catch {
    return json({ error: 'public_key must be base64' }, 400);
  }
  if (key.length !== PUBLIC_KEY_BYTES) return json({ error: 'public_key must be exactly 32 bytes' }, 400);

  const installId = randomInstallId();
  await env.DB.prepare('INSERT INTO installs (id, public_key, created_at) VALUES (?, ?, ?)')
    .bind(installId, b64encode(key), Math.floor(Date.now() / 1000))
    .run();
  return json({ install_id: installId, host: `${installId}.${env.RELAY_DOMAIN}` }, 201);
}

// 20 base32 chars from 13 random bytes: 5 bits per char is exact, so no rejection sampling is needed.
function randomInstallId() {
  const raw = crypto.getRandomValues(new Uint8Array(13));
  let acc = 0;
  let bits = 0;
  let id = '';
  for (const byte of raw) {
    acc = (acc << 8) | byte;
    bits += 8;
    while (bits >= 5 && id.length < 20) {
      id += BASE32[(acc >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  return id;
}

/** The mothership's websocket dial: unknown install 404, non-upgrade 426, else forward with the relay's
 * tunnel headers — kind, install id, and the stored public key the DO needs to authenticate it. The dial
 * is unauthenticated until the hello, so like registration it is rate-limited per connecting IP (in
 * memory, never stored); the DO separately caps concurrent pending handshakes. */
async function dial(request, env, installId) {
  if (env.DIAL_LIMITER) {
    const { success } = await env.DIAL_LIMITER.limit({ key: request.headers.get('cf-connecting-ip') ?? 'unknown' });
    if (!success) return json({ error: 'slow down' }, 429);
  }
  const install = await getInstall(env, installId);
  if (!install) return json({ error: 'unknown install' }, 404);
  if ((request.headers.get('upgrade') ?? '').toLowerCase() !== 'websocket') {
    return text('expected a websocket upgrade', 426);
  }
  const headers = new Headers(request.headers);
  stripRelayHeaders(headers);
  headers.set('x-relay-kind', 'tunnel');
  headers.set('x-relay-install-id', install.id);
  headers.set('x-relay-public-key', install.public_key);
  return env.TUNNELS.get(env.TUNNELS.idFromName(install.id)).fetch(new Request(request.url, { headers }));
}

/** Every signed endpoint: resolve the install, then check `x-colonizer-ts` + `x-colonizer-sig` — an
 * Ed25519 signature by the install's key over METHOD\npath\nts\nbody (no body means empty). */
async function signed(request, env, url, installId, handler) {
  const install = await getInstall(env, installId);
  if (!install) return json({ error: 'unknown install' }, 404);
  const ts = Number(request.headers.get('x-colonizer-ts'));
  const rawBody = request.method === 'GET' || request.method === 'HEAD' ? '' : await smallBody(request);
  if (rawBody === null) return json({ error: 'body too large' }, 413);
  const fresh = Number.isInteger(ts) && Math.abs(Math.floor(Date.now() / 1000) - ts) <= SIGNATURE_WINDOW_SECONDS;
  const ok =
    fresh &&
    (await verifyEd25519(install.public_key, `${request.method}\n${url.pathname}\n${ts}\n${rawBody}`, request.headers.get('x-colonizer-sig') ?? ''));
  if (!ok) return json({ error: 'bad signature' }, 401);
  return handler(install, rawBody);
}

/** What the local cockpit's Settings → Remote access shows: the owner, and pending pairing codes. */
async function pairingView(env, install) {
  const now = Math.floor(Date.now() / 1000);
  await purgeExpiredPairings(env, now);
  const { results } = await env.DB.prepare(
    'SELECT code, github_login, expires_at FROM pairings WHERE install_id = ? AND expires_at > ? ORDER BY code',
  )
    .bind(install.id, now)
    .all();
  return json({
    owner: install.owner_github_id === null ? null : { github_login: install.owner_github_login },
    pending: (results ?? []).map((row) => ({ code: row.code, github_login: row.github_login, expires_at: row.expires_at })),
  });
}

/** Confirming a pairing binds its GitHub account as owner and deletes every pairing of the install —
 * one code, one use. Unknown, expired and already-used codes are the same 404. The UPDATE only lands
 * while the install is unowned: two confirms racing bind the first and the loser gets a 409. */
async function confirmPairing(env, install, rawBody) {
  let body;
  try {
    body = JSON.parse(rawBody);
  } catch {
    return json({ error: 'body must be JSON' }, 400);
  }
  const code = body?.code;
  if (typeof code !== 'string' || !/^\d{6}$/.test(code)) return json({ error: 'code must be 6 digits' }, 400);
  const now = Math.floor(Date.now() / 1000);
  const pairing = await env.DB.prepare('SELECT github_id, github_login, expires_at FROM pairings WHERE install_id = ? AND code = ?')
    .bind(install.id, code)
    .first();
  if (!pairing || pairing.expires_at <= now) return json({ error: 'no such pairing' }, 404);
  const results = await env.DB.batch([
    env.DB.prepare('UPDATE installs SET owner_github_id = ?, owner_github_login = ? WHERE id = ? AND owner_github_id IS NULL').bind(
      pairing.github_id,
      pairing.github_login,
      install.id,
    ),
    env.DB.prepare('DELETE FROM pairings WHERE install_id = ?').bind(install.id),
  ]);
  if (!results[0].meta.changes) return json({ error: 'owner already bound' }, 409);
  return json({ owner: { github_login: pairing.github_login } });
}

/** Unbind ("reset link" in the cockpit): drop the owner and the pending pairings in one transaction.
 * Existing sessions stop working on their next request, because each one re-checks the owner. */
async function unbind(env, install) {
  await env.DB.batch([
    env.DB.prepare('UPDATE installs SET owner_github_id = NULL, owner_github_login = NULL WHERE id = ?').bind(install.id),
    env.DB.prepare('DELETE FROM pairings WHERE install_id = ?').bind(install.id),
  ]);
  return new Response(null, { status: 204 });
}

// ---------------------------------------------------------- an install's subdomain: browser-facing

async function installHost(request, env, url, installId) {
  const path = url.pathname;
  const method = request.method;

  // Fail closed when the secrets are absent: sealing or verifying cookies under an undefined key would
  // accept forgeries, because TextEncoder turns undefined into the string "undefined".
  if (!relayConfigured(env)) return misconfigured();
  if ((path === '/_auth' || path === '/_auth/callback') && !relayConfigured(env, { oauth: true })) return misconfigured();

  // The sign-in paths never forward, not even with a valid session.
  if (path === '/_auth' || path === '/_auth/callback' || path === '/_auth/logout') {
    const install = await getInstall(env, installId);
    if (!install) return unknownInstallPage();
    if (path === '/_auth' && method === 'GET') return startSignIn(request, env, url);
    if (path === '/_auth/callback' && method === 'GET') return finishSignIn(request, env, install, url);
    if (path === '/_auth/logout' && method === 'POST') return logout();
    return text('method not allowed', 405, null, { allow: 'GET, POST' });
  }

  const install = await getInstall(env, installId);
  if (!install) return unknownInstallPage();

  const session = await readSession(request, env);
  if (sessionOwns(session, install, installId, Math.floor(Date.now() / 1000))) {
    const headers = new Headers(request.headers);
    stripRelayHeaders(headers);
    stripSessionCookie(headers);
    headers.set('x-relay-kind', 'proxy');
    headers.set('x-relay-install-id', installId);
    const body = method === 'GET' || method === 'HEAD' ? null : request.body;
    return env.TUNNELS.get(env.TUNNELS.idFromName(installId)).fetch(new Request(request.url, { method, headers, body, duplex: 'half' }));
  }
  if ((method === 'GET' || method === 'HEAD') && request.headers.get('upgrade') === null) {
    return new Response(null, { status: 302, headers: { location: `/_auth?next=${encodeURIComponent(path + url.search)}` } });
  }
  return text('sign in required', 401);
}

// ------------------------------------------------------------------------ shared small pieces

/** The request body as text, but never more than MAX_JSON_BYTES UTF-8 bytes of it: a declared
 * content-length over the cap is refused unread, and a chunked (or lying) body is refused the moment it
 * passes the cap — bytes, not string length, so a multi-byte body cannot slip under the limit. */
async function smallBody(request) {
  const declared = Number(request.headers.get('content-length'));
  if (Number.isInteger(declared) && declared > MAX_JSON_BYTES) return null;
  if (request.body === null) return '';
  const parts = [];
  let total = 0;
  const reader = request.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > MAX_JSON_BYTES) return null; // the rest is never read
    parts.push(value);
  }
  const all = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    all.set(part, at);
    at += part.length;
  }
  return new TextDecoder().decode(all);
}

async function getInstall(env, installId) {
  return env.DB.prepare('SELECT * FROM installs WHERE id = ?').bind(installId).first();
}

/** Deletes every client-supplied x-relay-* header before the worker adds its own trusted ones. */
function stripRelayHeaders(headers) {
  for (const name of [...headers.keys()]) {
    if (name.toLowerCase().startsWith('x-relay-')) headers.delete(name);
  }
}

function json(value, status = 200) {
  return new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } });
}

function text(body, status, cookie, extra = {}) {
  const headers = new Headers({ 'content-type': 'text/plain; charset=utf-8', ...extra });
  if (cookie) headers.append('set-cookie', cookie);
  return new Response(body, { status, headers });
}
