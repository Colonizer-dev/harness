// Owner sign-in for an install's subdomain (#534): the /_auth browser flow and the two cookies it
// mints. The oauth cookie carries the browser from /_auth through GitHub back to /_auth/callback; the
// session cookie is what the router re-checks on every proxied request against the owner currently
// bound in D1 — which is why unbinding an install kills its existing sessions without any token store.
// Both cookies are sealed with HMAC-SHA256 (crypto.js): base64url(JSON).tag, the tag verified with
// crypto.subtle.verify so a forged cookie is rejected in constant time.

import { b64decode, b64urlEncode, hmacSign, hmacVerify, randomToken } from './crypto.js';
import { forbiddenPage, pairingPage } from './pages.js';

export const OAUTH_COOKIE = '__Host-colonizer_oauth';
export const SESSION_COOKIE = '__Host-colonizer_session';

/** The sign-in state and a pairing code both live this long. */
export const STATE_SECONDS = 10 * 60;
/** A session lasts a week — but every request still re-checks it against the current owner. */
export const SESSION_SECONDS = 7 * 24 * 60 * 60;

const ENCODER = new TextEncoder();
const DECODER = new TextDecoder();
const COOKIE_ATTRIBUTES = 'Path=/; Secure; HttpOnly; SameSite=Lax';

const nowSeconds = () => Math.floor(Date.now() / 1000);

async function seal(payload, secret) {
  const body = b64urlEncode(ENCODER.encode(JSON.stringify(payload)));
  return `${body}.${await hmacSign(secret, body)}`;
}

/** Reads a sealed cookie from a Cookie header, verifying the tag before touching the payload. */
export async function readCookie(header, name, secret) {
  for (const part of (header ?? '').split(';')) {
    const eq = part.indexOf('=');
    if (eq === -1 || part.slice(0, eq).trim() !== name) continue;
    const value = part.slice(eq + 1).trim();
    const dot = value.lastIndexOf('.');
    const body = value.slice(0, dot);
    if (dot === -1 || !(await hmacVerify(secret, body, value.slice(dot + 1)))) return null;
    try {
      return JSON.parse(DECODER.decode(b64decode(body)));
    } catch {
      return null;
    }
  }
  return null;
}

export const readSession = (request, env) =>
  readCookie(request.headers.get('cookie'), SESSION_COOKIE, env.SESSION_SECRET);

// A sealed cookie is only as good as its key: with a secret missing, TextEncoder would seal — and
// verify — under the string "undefined", which anyone can compute. These checks fail closed instead.
const hasSecret = (value) => typeof value === 'string' && value.length > 0;

/** The relay serves cookies only with its signing key; the OAuth paths also need the GitHub app. */
export function relayConfigured(env, { oauth = false } = {}) {
  return hasSecret(env.SESSION_SECRET) && (!oauth || (hasSecret(env.GITHUB_CLIENT_ID) && hasSecret(env.GITHUB_CLIENT_SECRET)));
}

/** The clean 500 every guarded flow returns instead of ever touching a cookie with a missing key. */
export const misconfigured = () => text('relay misconfigured: SESSION_SECRET or the GitHub OAuth app is not set', 500);

/** Mints a session cookie value; the flow below sets it via Set-Cookie, tests use it directly. */
export const sealSession = (installId, githubId, secret) =>
  seal({ i: installId, g: githubId, e: nowSeconds() + SESSION_SECONDS }, secret);

export function sessionSetCookie(value) {
  return `${SESSION_COOKIE}=${value}; ${COOKIE_ATTRIBUTES}; Max-Age=${SESSION_SECONDS}`;
}

function stateSetCookie(name, value) {
  return `${name}=${value}; ${COOKIE_ATTRIBUTES}; Max-Age=${STATE_SECONDS}`;
}

export function clearCookie(name) {
  return `${name}=; ${COOKIE_ATTRIBUTES}; Max-Age=0`;
}

// Only same-origin relative paths: a single leading '/', never '//' or '/\' — those are treated as
// scheme-relative by browsers and would bounce the owner off the install's subdomain.
export function safeNext(value) {
  return typeof value === 'string' && value.startsWith('/') && !value.startsWith('//') && !value.startsWith('/\\')
    ? value
    : '/';
}

// Six uniform digits from crypto random bytes. Rejection sampling keeps every digit equally likely; a
// bare modulo of a 32-bit word would lean slightly on the low digits.
export function sixDigitCode() {
  const word = new Uint32Array(1);
  let code = '';
  for (let i = 0; i < 6; i++) {
    do {
      crypto.getRandomValues(word);
    } while (word[0] >= 4_294_967_290); // the largest multiple of 10 under 2**32
    code += String(word[0] % 10);
  }
  return code;
}

// GitHub calls go through globalThis.fetch so tests can stub them. The access token is used once, to
// read /user, and discarded: only {id, login} survives, into a pairing row or the owner binding.
export async function githubUser(env, code, redirectUri) {
  const token = await fetch('https://github.com/login/oauth/access_token', {
    method: 'POST',
    headers: { accept: 'application/json', 'content-type': 'application/json' },
    body: JSON.stringify({
      client_id: env.GITHUB_CLIENT_ID,
      client_secret: env.GITHUB_CLIENT_SECRET,
      code,
      redirect_uri: redirectUri,
    }),
  })
    .then((r) => r.json().catch(() => ({})))
    .catch(() => ({}));
  if (typeof token?.access_token !== 'string') return null;
  const user = await fetch('https://api.github.com/user', {
    headers: { accept: 'application/json', authorization: `Bearer ${token.access_token}`, 'user-agent': 'colonizer-relay' },
  })
    .then((r) => r.json().catch(() => null))
    .catch(() => null);
  return user && Number.isInteger(user.id) && typeof user.login === 'string' ? { id: user.id, login: user.login } : null;
}

/** 302 to GitHub's authorize endpoint. No scope: the relay needs only the public {id, login}. */
export async function startSignIn(request, env, url) {
  const state = randomToken(16);
  const authorize = new URL('https://github.com/login/oauth/authorize');
  authorize.searchParams.set('client_id', env.GITHUB_CLIENT_ID);
  authorize.searchParams.set('redirect_uri', `https://${url.hostname}/_auth/callback`);
  authorize.searchParams.set('state', state);
  authorize.searchParams.set('allow_signup', 'false');
  const headers = new Headers({ location: authorize });
  headers.append(
    'set-cookie',
    stateSetCookie(OAUTH_COOKIE, await seal({ s: state, n: safeNext(url.searchParams.get('next')), e: nowSeconds() + STATE_SECONDS }, env.SESSION_SECRET)),
  );
  return new Response(null, { status: 302, headers });
}

/** The GitHub callback: check state, read {id, login}, then bind, 403, or park a pairing. */
export async function finishSignIn(request, env, install, url) {
  const oauth = await readCookie(request.headers.get('cookie'), OAUTH_COOKIE, env.SESSION_SECRET);
  const state = url.searchParams.get('state');
  const code = url.searchParams.get('code');
  const forget = clearCookie(OAUTH_COOKIE);
  if (!oauth || oauth.e <= nowSeconds() || state === null || state !== oauth.s) {
    return text('sign-in state mismatch or expired; start again at /_auth', 400, forget);
  }
  const user = code === null ? null : await githubUser(env, code, `https://${url.hostname}/_auth/callback`);
  if (!user) return text('GitHub sign-in failed; start again at /_auth', 502, forget);
  const next = safeNext(oauth.n);

  if (install.owner_github_id !== null) {
    if (user.id !== install.owner_github_id) {
      const refused = forbiddenPage();
      refused.headers.append('set-cookie', forget);
      return refused;
    }
    const session = await seal({ i: install.id, g: user.id, e: nowSeconds() + SESSION_SECONDS }, env.SESSION_SECRET);
    return redirect(next, [sessionSetCookie(session), forget]);
  }

  // Nobody owns this install yet: park the GitHub account behind a 6-digit code. The browser shows the
  // code, the local cockpit confirms it with a signed request (worker.js) — the cockpit gets the final
  // say on who owns the install, not whoever happened to sign in first.
  const now = nowSeconds();
  await purgeExpiredPairings(env, now);
  const pairingCode = sixDigitCode();
  await env.DB.prepare(
    'INSERT OR REPLACE INTO pairings (install_id, code, github_id, github_login, expires_at) VALUES (?, ?, ?, ?, ?)',
  )
    .bind(install.id, pairingCode, user.id, user.login, now + STATE_SECONDS)
    .run();
  const pairing = pairingPage(pairingCode, next);
  pairing.headers.append('set-cookie', forget);
  return pairing;
}

export function logout() {
  return redirect('/', [clearCookie(SESSION_COOKIE)]);
}

/** A session is valid for an install only while it names that install, is unexpired, and its GitHub id
 * is still the one bound as owner — the check that makes unbinding instant. */
export function sessionOwns(session, install, installId, now) {
  return (
    session !== null &&
    session.i === installId &&
    session.g === install.owner_github_id &&
    install.owner_github_id !== null &&
    Number.isInteger(session.e) &&
    session.e > now
  );
}

/** The relay's own session cookie never reaches the mothership; its own login cookie must, because the
 * cockpit's auth still applies behind the relay. */
export function stripSessionCookie(headers) {
  const header = headers.get('cookie');
  if (header === null) return headers;
  const kept = header
    .split(';')
    .map((c) => c.trim())
    .filter((c) => c !== '' && !c.startsWith(`${SESSION_COOKIE}=`));
  if (kept.length === 0) headers.delete('cookie');
  else headers.set('cookie', kept.join('; '));
  return headers;
}

/** Expired pairings are deleted on the back of whatever touched them, never read. */
export async function purgeExpiredPairings(env, now) {
  await env.DB.prepare('DELETE FROM pairings WHERE expires_at <= ?').bind(now).run();
}

function redirect(location, cookies = []) {
  const headers = new Headers({ location });
  for (const cookie of cookies) headers.append('set-cookie', cookie);
  return new Response(null, { status: 302, headers });
}

function text(body, status, cookie) {
  const headers = new Headers({ 'content-type': 'text/plain; charset=utf-8' });
  if (cookie) headers.append('set-cookie', cookie);
  return new Response(body, { status, headers });
}
