// What the relay forwards without a GitHub session (#1086), for an install whose `require_github` is
// off. Exactly three shapes, and the relay trusts none of them — it only recognises them; the
// mothership authenticates every one (crates/colonizer/src/server.rs `host_guard`, phone.rs):
//
//   invite      GET /?pair=<invite>             the single-use, five-minute invite the mothership minted
//                                               (Settings → Remote access → Sign in on another device,
//                                               or Add your phone); opening it shows six digits that are
//                                               confirmed on the mothership's own screen;
//   claim       POST /api/phone/claim           that page's poll, carrying the device-secret cookie the
//               + colonizer_pair cookie         invite set, which turns into a credential once confirmed;
//   credential  colonizer_token cookie or       a link credential (clk_…) or a paired phone's (cph_…),
//               Authorization: Bearer           stored hashed on the mothership, revocable one by one,
//                                               and all rotated by Reset link.
//
// Everything else — no invite, no credential — is answered by the relay itself with the "Pair this
// device" page, and never reaches the tunnel. The shapes are checked tightly (hex of the lengths the
// mothership mints), so junk is not forwarded either; a well-shaped value is still only a claim.

/** The cockpit's own cookie (crates/colonizer/src/auth.rs COOKIE_NAME). */
export const TOKEN_COOKIE = 'colonizer_token';
/** The pairing page's device-secret cookie (crates/colonizer/src/phone.rs PAIR_COOKIE). */
export const PAIR_COOKIE = 'colonizer_pair';
/** The one header the mothership answers a tunnelled request with when the invite, pairing secret or
 * credential that request carried did not authenticate. Its only value is `rejected`. The relay counts
 * it toward the throttle and strips it from every answer, so a browser never sees it. */
export const VERDICT_HEADER = 'x-colonizer-credential';
/** The close code a tunnelled websocket ends with when its credential was rejected (remote.rs). */
export const REJECTED_CLOSE = 4401;

const INVITE = /^[0-9a-f]{32,128}$/;
const CREDENTIAL = /^(?:clk|cph)_[0-9a-f]{32,128}$/;
const PAIR_SECRET = /^ph_[0-9a-f]{8,32}\.[0-9a-f]{32,128}$/;

/** Every value of cookie `name` in a Cookie header. */
export function cookieValues(header, name) {
  const out = [];
  for (const part of (header ?? '').split(';')) {
    const eq = part.indexOf('=');
    if (eq !== -1 && part.slice(0, eq).trim() === name) out.push(part.slice(eq + 1).trim());
  }
  return out;
}

/** How a request presents a link or phone credential: 'bearer', 'cookie', or null for neither. */
export function presentedCredential(request) {
  const auth = /^Bearer\s+(\S+)$/i.exec(request.headers.get('authorization') ?? '');
  if (auth && CREDENTIAL.test(auth[1])) return 'bearer';
  return cookieValues(request.headers.get('cookie'), TOKEN_COOKIE).some((v) => CREDENTIAL.test(v)) ? 'cookie' : null;
}

/** Which pass-through this request is — 'invite', 'claim' or 'credential' — or null for none. */
export function passThrough(request, url) {
  const method = request.method;
  const upgrade = request.headers.get('upgrade') !== null;
  if (method === 'GET' && !upgrade && url.pathname === '/') {
    const pair = url.searchParams.getAll('pair');
    if (pair.length === 1 && INVITE.test(pair[0])) return 'invite';
  }
  if (method === 'POST' && url.pathname === '/api/phone/claim') {
    if (cookieValues(request.headers.get('cookie'), PAIR_COOKIE).some((v) => PAIR_SECRET.test(v))) return 'claim';
  }
  return presentedCredential(request) === null ? null : 'credential';
}

/** Clears a dead credential cookie on the link's origin, with the attributes the mothership set it with
 * (auth.rs `set_cookie_header`, plus the `Secure` the tunnel client adds), so the browser's next request
 * lands straight on the pair page instead of reaching the mothership again. */
export const clearTokenCookie = () => `${TOKEN_COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0`;
