// The relay worker (issues #532 and #534). Two kinds of host land here, matched case-insensitively:
//
//   my.colonizer.dev            the apex: install registration, the mothership's tunnel dial, and the
//                               signed mothership endpoints (pairing view, confirm and reject, owner
//                               unbind, the install's settings, retiring the install);
//   <install>.my.colonizer.dev  one subdomain per install: owner sign-in under /_auth, everything else
//                               proxied to the install's tunnel DO once a valid session is present —
//                               or, on an install with `require_github` off (#1086), when the request
//                               carries a pairing invite or a link/phone credential for the mothership
//                               to judge (src/passthrough.js), throttled per install and per client
//                               (src/throttle.js). Anything else there gets the "Pair this device" page.
//
// Anything else is 404. Every request handed to a DO first loses all client-supplied x-relay-* headers
// — the worker is the only thing allowed to speak that dialect, so a browser cannot forge a tunnel or
// a proxy identity. Nothing about a request is stored: D1 holds the public key, the created-at, the
// owner binding and the require_github switch (migrations/), plus short-lived throttle counters keyed
// by install and by an HMAC of the client — never an address, a header or a body.

import { b64decode, b64encode, verifyEd25519 } from './crypto.js';
import { finishSignIn, logout, misconfigured, purgeExpiredPairings, readSession, relayConfigured, sessionOwns, startSignIn, stripSessionCookie } from './auth.js';
import { pairDevicePage, tooManyAttemptsPage, unknownInstallPage } from './pages.js';
import { VERDICT_HEADER, clearTokenCookie, passThrough, presentedCredential } from './passthrough.js';
import { LIMITS, clientKey, count, forgetInstall, throttled } from './throttle.js';

export { InstallTunnel } from './tunnel.js';

const DOMAIN_LABEL = /^[a-z2-7]{16,}$/; // 20 random base32 chars in practice; 16+ leaves headroom
const PATH_INSTALL_ID = '([a-z2-7]{16,})';
const TUNNEL_PATH = new RegExp(`^/tunnel/${PATH_INSTALL_ID}$`);
const PAIRING_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/pairing$`);
const CONFIRM_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/pairing/confirm$`);
const REJECT_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/pairing/reject$`);
const OWNER_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/owner$`);
const SETTINGS_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}/settings$`);
const INSTALL_PATH = new RegExp(`^/api/installs/${PATH_INSTALL_ID}$`);

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
  if (method === 'POST' && (id = REJECT_PATH.exec(path))) {
    return signed(request, env, url, id[1], (install, rawBody) => rejectPairing(env, install, rawBody));
  }
  if (method === 'DELETE' && (id = OWNER_PATH.exec(path))) return signed(request, env, url, id[1], (install) => unbind(env, install));
  if (method === 'PUT' && (id = SETTINGS_PATH.exec(path))) {
    return signed(request, env, url, id[1], (install, rawBody) => putSettings(env, install, rawBody));
  }
  if (method === 'DELETE' && (id = INSTALL_PATH.exec(path))) return signed(request, env, url, id[1], (install) => retire(env, install));
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
  // A new install pairs with the Colonizer pair code alone (#1086) unless its mothership asks for the
  // GitHub gate too; only a literal `true` turns it on.
  const requireGithub = body?.require_github === true;

  const installId = randomInstallId();
  await env.DB.prepare('INSERT INTO installs (id, public_key, created_at, require_github) VALUES (?, ?, ?, ?)')
    .bind(installId, b64encode(key), Math.floor(Date.now() / 1000), requireGithub ? 1 : 0)
    .run();
  return json({ install_id: installId, host: `${installId}.${env.RELAY_DOMAIN}`, require_github: requireGithub }, 201);
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
    require_github: requiresGithub(install),
  });
}

/** `PUT …/settings {"require_github": bool}` (#1086): whether this install's subdomain sends every
 * browser through the GitHub owner sign-in first. Signed like every mothership call, so only the
 * install's key holder — the operator, from the local cockpit — can turn the gate off. The owner
 * binding and pending pairings are untouched either way, so switching back on restores the old gate
 * as it was. */
async function putSettings(env, install, rawBody) {
  let body;
  try {
    body = JSON.parse(rawBody);
  } catch {
    return json({ error: 'body must be JSON' }, 400);
  }
  if (typeof body?.require_github !== 'boolean') return json({ error: 'require_github must be a boolean' }, 400);
  await env.DB.prepare('UPDATE installs SET require_github = ? WHERE id = ?')
    .bind(body.require_github ? 1 : 0, install.id)
    .run();
  return json({ require_github: body.require_github });
}

/** An install row's GitHub gate. A missing column (a relay not yet migrated) reads as on: the old
 * behaviour, never the open one. */
const requiresGithub = (install) => install.require_github !== 0;

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

/** Rejecting a pairing deletes just that code, so the sign-in behind it never becomes the owner; the
 * browser that got it has to start over. Unknown and expired codes are the same 404 as a confirm's. */
async function rejectPairing(env, install, rawBody) {
  const code = pairingCode(rawBody);
  if (code === null) return json({ error: 'code must be 6 digits' }, 400);
  const now = Math.floor(Date.now() / 1000);
  const pairing = await env.DB.prepare('SELECT github_login, expires_at FROM pairings WHERE install_id = ? AND code = ?')
    .bind(install.id, code)
    .first();
  if (!pairing || pairing.expires_at <= now) return json({ error: 'no such pairing' }, 404);
  await env.DB.prepare('DELETE FROM pairings WHERE install_id = ? AND code = ?').bind(install.id, code).run();
  return json({ github_login: pairing.github_login });
}

/** The `code` of a signed confirm or reject body: six digits, or null for anything else. */
function pairingCode(rawBody) {
  let body;
  try {
    body = JSON.parse(rawBody);
  } catch {
    return null;
  }
  const code = body?.code;
  return typeof code === 'string' && /^\d{6}$/.test(code) ? code : null;
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

/** Retire an install ("reset link" in the cockpit, review finding R2): its row, owner binding and
 * pending pairings go in one transaction, and a live tunnel on it is closed. From then on the install
 * is unknown everywhere — a dial is 404, the subdomain shows the unknown-install page, every session
 * for it fails — so a copy of the retired key reaches nothing. Signed by that key, like every other
 * mothership call: only the key's holder can retire it. */
async function retire(env, install) {
  await env.DB.batch([
    env.DB.prepare('DELETE FROM pairings WHERE install_id = ?').bind(install.id),
    env.DB.prepare('DELETE FROM installs WHERE id = ?').bind(install.id),
    forgetInstall(env, install.id),
  ]);
  const headers = new Headers({ 'x-relay-kind': 'retire', 'x-relay-install-id': install.id });
  await env.TUNNELS.get(env.TUNNELS.idFromName(install.id)).fetch(new Request(`https://${env.RELAY_DOMAIN}/_retire`, { method: 'POST', headers }));
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

  const now = Math.floor(Date.now() / 1000);
  const session = await readSession(request, env);
  if (sessionOwns(session, install, installId, now)) return withoutVerdict(await forward(request, env, url, installId));

  if (requiresGithub(install)) {
    // The GitHub gate (#534), as before: sign in first, whatever else the request carries.
    if ((method === 'GET' || method === 'HEAD') && request.headers.get('upgrade') === null) {
      return new Response(null, { status: 302, headers: { location: `/_auth?next=${encodeURIComponent(path + url.search)}` } });
    }
    return text('sign in required', 401);
  }

  // The pair code alone (#1086): only an invite, its claim, or a credential goes on, and the mothership
  // judges each one. Everything else is the relay's own "Pair this device" answer.
  const kind = passThrough(request, url);
  if (kind === null) return notPaired(request, path);
  const client = await clientKey(request, env);
  const kinds = kind === 'invite' ? ['invite', 'fail'] : ['fail'];
  if (await throttled(env, kinds, installId, client, now)) return slowDown(request, path);
  if (kind === 'invite') await count(env, 'invite', installId, client, now);

  const response = await forward(request, env, url, installId, client);
  // A websocket's 101 is the relay's own; a rejected one is closed 4401 later, and the DO counts it.
  if (response.status === 101) return response;
  if (response.headers.get(VERDICT_HEADER) === null) return withoutVerdict(response);

  // The mothership turned the invite, pairing secret or credential down: count it, and clear a dead
  // credential cookie so the browser's next request stops at the pair page instead of the tunnel. A
  // page load gets the pair page now; an API call or the claim poll keeps the mothership's own answer,
  // which its caller already understands.
  await count(env, 'fail', installId, client, now);
  const page = (method === 'GET' || method === 'HEAD') && !isApiPath(path);
  const answer = page ? pairDevicePage({ expired: true }) : withoutVerdict(response);
  if (page) await response.body?.cancel();
  if (presentedCredential(request) === 'cookie') answer.headers.append('set-cookie', clearTokenCookie());
  return answer;
}

/** Hands a browser request to the install's tunnel DO as a proxy request: client x-relay-* headers
 * and the relay's own session cookie stripped, the worker's own x-relay-* headers stamped. `client`
 * (the throttle key, never an address) lets the DO count a websocket the mothership closes as
 * rejected; stripHopByHop drops it, like every x-relay-* header, before the mothership. */
async function forward(request, env, url, installId, client = null) {
  const method = request.method;
  const headers = new Headers(request.headers);
  stripRelayHeaders(headers);
  stripSessionCookie(headers);
  headers.set('x-relay-kind', 'proxy');
  headers.set('x-relay-install-id', installId);
  if (client !== null) headers.set('x-relay-client', client);
  const body = method === 'GET' || method === 'HEAD' ? null : request.body;
  return env.TUNNELS.get(env.TUNNELS.idFromName(installId)).fetch(new Request(request.url, { method, headers, body, duplex: 'half' }));
}

/** The answer without the mothership's verdict header, which is for the relay alone. A Response from
 * a DO stub can have immutable headers, so it is copied rather than edited. */
function withoutVerdict(response) {
  if (response.status === 101) return response;
  const copy = new Response(response.body, response);
  copy.headers.delete(VERDICT_HEADER);
  return copy;
}

const isApiPath = (path) => path === '/api' || path.startsWith('/api/');

/** No invite and no credential on a pair-code install: the pair page for a page load, a short 401 for
 * an API call, a websocket or a write. Nothing reaches the tunnel either way. */
function notPaired(request, path) {
  if ((request.method === 'GET' || request.method === 'HEAD') && request.headers.get('upgrade') === null && !isApiPath(path)) {
    return pairDevicePage();
  }
  return json({ error: 'this device is not paired; pair it from Settings → Remote access on the computer running Colonizer' }, 401);
}

/** The throttle is full: nothing forwarded, a page or a JSON 429 with the longest window as retry-after. */
function slowDown(request, path) {
  const retry = Math.max(LIMITS.invite.client.seconds, LIMITS.fail.client.seconds);
  if ((request.method === 'GET' || request.method === 'HEAD') && request.headers.get('upgrade') === null && !isApiPath(path)) {
    return tooManyAttemptsPage(retry);
  }
  const res = json({ error: 'too many pairing or sign-in attempts; wait a few minutes' }, 429);
  res.headers.set('retry-after', String(retry));
  return res;
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
