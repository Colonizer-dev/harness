# relay

The relay is the Cloudflare Worker behind `my.colonizer.dev` (issues **#532** and **#534**). It gives every
mothership that switches remote access on a private subdomain — `<install_id>.my.colonizer.dev` — and puts a
browser-facing front door on it: a "Pair this device" page that forwards only pairing invites and paired
devices' credentials (#1086), optional owner sign-in through GitHub, a per-install tunnel into the cockpit,
and a small signed API the mothership itself calls.

```
browser ──> my.colonizer.dev            register an install, mothership tunnel dial, signed cockpit API
browser ──> <install>.my.colonizer.dev  invites and credentials (or, with the GitHub gate, the owner's
                                         session) proxied into the tunnel; the pair page for the rest
                     │
                     └── D1 (installs, pairings, throttle) + one InstallTunnel Durable Object per install
```

## Trust boundaries

What the relay stores is the whole of what it knows (see `migrations/`):

- the **Ed25519 public key** an install registered with, its **created-at**, the **owner binding**
  (GitHub id + login) once a pairing was confirmed, and its **`require_github`** switch (#1086);
- short-lived **pairing rows**: install id, 6-digit code, the waiting GitHub id/login, an expiry;
- short-lived **throttle rows** (#1086): a key per install or per client, a count and the end of its
  window. A client key is an HMAC of the connecting IP under `SESSION_SECRET`, truncated — the address
  itself is never stored — and a row is dead, and deleted on the next count, once its window ends.

Nothing else. No request or response bodies, no headers, no IP addresses, no cookies, no GitHub access
tokens ever reach storage — the access token is used once, to read `/user`, and discarded. Logs carry only
method, path template, status, bytes and duration. Storage is not the whole trust story, though: the
relay terminates TLS, so the worker and the tunnel DO see every proxied request and response in
cleartext in memory, including the cockpit's own login cookie
([remote-access-review.md](../../docs/remote-access-review.md), finding R3).

The DO tunnel (connect, framing, offline handling) is described in `src/tunnel.js`; the worker never
forwards a client-supplied `x-relay-*` header — it strips them all and stamps its own verdicts.
Per stream, the response body must keep moving — 5 minutes without an inbound body frame fails the
stream — and a response body the browser has not read yet may queue at most 8 MiB before the stream
fails. A response head must arrive within 60 s (`504` otherwise). Each install gets a 120-request
burst refilled at 20 per second (`429` past it), at most 32 open streams (`503` with
`retry-after: 1`), and an offline tunnel answers a `502` page.

## Protocol encodings pinned here

Where the tunnel spec left the encoding open, these are now fixed (implementations on both sides must
agree; `src/protocol.js` is the reference):

- public keys and Ed25519 signatures are **standard base64** (padding included);
- nonces and tokens are **base64url without padding**;
- timestamps are **unix seconds**;
- the signed hello message is **UTF-8 concatenation** `nonce ‖ install_id ‖ ts` (no separators);
- stream ids are **integers**;
- the relay sends body chunks of at most **36 KiB raw (48 KiB of base64)** and accepts chunks that
  decode to at most **48 KiB**.

The mothership (`crates/colonizer/src/remote.rs`) and this relay do not yet agree with each other or
with the pinned contract in every detail — for one, the relay cannot read the `[name, value]` header
pairs the mothership sends in `res`. [docs/remote-tunnel.md](../../docs/remote-tunnel.md#where-the-code-differs-today)
lists the differences.

## Signed mothership API (apex, `my.colonizer.dev`)

Registration and the tunnel dial are unauthenticated (the dial proves the key in its hello). The
pairing and owner endpoints are signed with the key the install registered: headers `x-colonizer-ts`
(unix seconds, `|now − ts| ≤ 300`) and `x-colonizer-sig` (base64 Ed25519 over the UTF-8 string
`METHOD\npathname\nts\nrawBody`, with `rawBody` empty when there is no body). Bad or missing → 401;
a request body over 1 KiB → 413.

The mothership calls these from `crates/colonizer/src/remote.rs` (#599): the cockpit's Settings →
Remote access shows the pending codes with Confirm and Reject, and the bound owner with Unbind.
Confirming is local-only at the mothership — never through the tunnel.

- `POST /api/installs` `{"public_key": …, "require_github"?: bool}` → `201 {"install_id", "host",
  "require_github"}` — 20 random base32 chars, 100 bits; the key must be 32 bytes. Every call makes a new
  install, even for a key already registered. The GitHub gate is on only for a literal
  `"require_github": true`; otherwise the install pairs with the pair code alone (#1086).
  Rate limited per IP (10 a minute) when the `REGISTER_LIMITER` binding is present.
- `GET /tunnel/<install_id>` — the mothership's WebSocket dial (`426` for anything that is not an
  `Upgrade: websocket`); the worker adds `x-relay-kind: tunnel`, `x-relay-install-id`,
  `x-relay-public-key` and hands the request to the install's DO. An unknown install is `404`. Rate
  limited per IP (30 a minute) when the `DIAL_LIMITER` binding is present. Inside the DO, pending handshakes are independent of each other
  and capped at 16 per install — the 17th dial is closed with `1013` before it can disturb the others.
- `GET /api/installs/<id>/pairing` → `{"owner": {"github_login"} | null, "pending": [{code, github_login,
  expires_at}], "require_github": bool}` — what the local cockpit's Settings → Remote access is meant to show.
- `PUT /api/installs/<id>/settings` `{"require_github": bool}` → `200 {"require_github"}` — the GitHub gate
  on or off (#1086); `400` unless a boolean. The owner binding and pending pairings are untouched, so
  switching it back on restores the gate as it was.
- `POST /api/installs/<id>/pairing/confirm` `{"code": …}` → `200 {"owner": …}` — binds that pairing's
  GitHub account, deletes every pairing of the install (single-use). Unknown/expired/used → 404; an
  owner already bound → 409.
- `POST /api/installs/<id>/pairing/reject` `{"code": …}` → `200 {"github_login"}` — deletes just that
  pairing, so the sign-in behind it never becomes the owner. Malformed → 400; unknown/expired → 404.
- `DELETE /api/installs/<id>/owner` → `204` — unbinds the owner and clears pending pairings. Existing
  sessions die on their next request, because each one re-checks the cookie against the current owner.
- `DELETE /api/installs/<id>` → `204` — the "reset link" (review finding R2): deletes the install, its
  owner, its pending pairings and its own throttle rows in one batch, then tells the install's DO, which closes a live tunnel
  with `4404`. From then on the install is unknown: its key signs nothing (404), a dial is 404, and its
  subdomain shows the unknown-install page to every session.

## Pairing with the pair code (`<install>.my.colonizer.dev`, #1086)

For an install with `require_github` off — every install registered without asking for the gate — a
request without a valid owner session is forwarded only when it is one of these (`src/passthrough.js`):

- an invite: `GET /?pair=<invite>` (hex, at the root, exactly one `pair`);
- its claim poll: `POST /api/phone/claim` with a well-formed `colonizer_pair` cookie;
- a credential: a `clk_…` or `cph_…` value as the `colonizer_token` cookie or an `Authorization: Bearer`.

The relay trusts none of them; the mothership authenticates each. Anything else is the relay's own
**Pair this device** page (`401`, `no-store`; a JSON `401` for `/api/*`, websockets and writes), and nothing
reaches the DO. The throttle (`src/throttle.js`) is checked before forwarding: every invite open counts
(10 per client, 30 per install, per 10 minutes), and so does every forwarded request the mothership
rejected (20 per client, 200 per install, per 10 minutes); past either the relay answers `429` with
`retry-after`. The mothership marks a rejection with the response header `x-colonizer-credential:
rejected`, which the worker counts and strips from every answer, or closes a tunnelled websocket `4401`,
which the DO counts. A page load on a rejected credential gets the pair page at once and the dead
`colonizer_token` cookie cleared; an API call or the claim poll keeps the mothership's own answer. A bound
owner's GitHub session (below) is forwarded in either mode and never throttled.

## Owner sign-in (`<install>.my.colonizer.dev`)

Since #1086 this is optional: it gates the install only while its `require_github` is on (installs
registered before the switch existed keep it on until their mothership turns it off).

- `GET /_auth?next=/path` → 302 to GitHub's authorize endpoint (`allow_signup=false`, no scope — the
  relay needs only the public `{id, login}`), with a 10-minute HMAC-sealed `__Host-colonizer_oauth`
  cookie holding the state and the next path (same-origin relative paths only).
- `GET /_auth/callback` → state checked against the cookie (400 on mismatch), code exchanged,
  `/user` read. Owner bound and equal → 7-day `__Host-colonizer_session` cookie, 302 to next. Owner
  bound and different → 403. Unbound → a 6-digit code (crypto-random, rejection-sampled) stored as a
  10-minute pairing; the page tells the owner to confirm it in the local cockpit under Settings →
  Remote access, with a Continue link back to `/_auth`.
- `POST /_auth/logout` → clears the session cookie.
- Fail closed: on an install's subdomain, a missing `SESSION_SECRET` — or, on `/_auth` and
  `/_auth/callback`, a missing GitHub OAuth client id/secret — is a plain `500`, never a fallback
  that would accept forgeries.
- Anything else on the host: unknown install → 404; valid session (`i` = this install, `g` = current
  owner, unexpired) → proxied to the DO with `x-relay-kind: proxy` and the relay's own session cookie
  stripped (the cockpit's own login cookie still reaches it); otherwise, with `require_github` on, a plain
  GET/HEAD is redirected to `/_auth?next=…` and anything else, WebSocket upgrades included, gets 401 —
  and with it off, the pair-code rules above apply.

Both cookies are `__Host-` prefixed: `Secure; HttpOnly; SameSite=Lax; Path=/`, no `Domain`, sealed with
HMAC-SHA256 under the `SESSION_SECRET` secret and verified in constant time.

Cookies the cockpit sets come back host-only (review finding R4): the DO drops every `Domain` attribute
from a forwarded `set-cookie`, so one install can never set a cookie on `my.colonizer.dev` or a sibling
install, and drops outright any cookie named like one of the two above (`hostOnlyCookie`,
`src/protocol.js`).

## Deploy (done by a human, out of scope for the PR)

From this directory:

1. `npm ci`, then `node --test` — the suite runs on plain Node (24+), no Workers runtime needed.
2. `npx --no-install wrangler d1 create colonizer-relay` and paste the returned database id over the
   placeholder in `wrangler.toml` (marked with a comment).
3. `npx --no-install wrangler d1 migrations apply colonizer-relay --remote`. On an existing deployment this
   is also the upgrade step: apply new migrations (e.g. `0002_pair_code_access.sql`, #1086) **before**
   deploying a worker that reads them.
4. Create a **GitHub OAuth App** (not a GitHub App) with no webhook and callback URL
   `https://my.colonizer.dev/_auth/callback`. GitHub matches a redirect_uri against the registered
   callback URL by **host and port, with the path required to be a subdirectory** — and since August
   2026 subdomain matches are only accepted when **wildcard matching is enabled** for that callback
   URI. Single-callback apps carry wildcard matching by default (it is the pre-2026 behaviour, now a
   visible toggle): check the app's settings and leave it enabled, because the browser is sent back to
   `https://<install>.my.colonizer.dev/_auth/callback`, a subdomain of the registered host. That is
   safe here and only here — GitHub warns wildcard matching is dangerous when subdomains host
   untrusted content, and every `*.my.colonizer.dev` subdomain is this worker, which 404s unknown
   installs and stores no user content.
5. `npx --no-install wrangler secret put GITHUB_CLIENT_SECRET` and
   `npx --no-install wrangler secret put SESSION_SECRET` (any long random string; it never rotates
   per-request). Set the app's client id as the `GITHUB_CLIENT_ID` var in `wrangler.toml`.
6. DNS, on the `colonizer.dev` zone: a proxied record for `my.colonizer.dev` and a proxied wildcard
   `*.my.colonizer.dev`, both to this worker — the `[[routes]]` entries in `wrangler.toml` cover the
   worker side (`my.colonizer.dev/*` and `*.my.colonizer.dev/*`).
7. `npx --no-install wrangler deploy`.

There are no cron triggers in this Cloudflare account (see the note in
`services/telemetry/src/worker.js`), so the relay deliberately has none: expired pairing rows are
deleted opportunistically, on the back of the requests that touch them.
