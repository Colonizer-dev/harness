# relay

The relay is the Cloudflare Worker behind `my.colonizer.dev` (issues **#532** and **#534**). It gives every
mothership that switches remote access on a private subdomain — `<install_id>.my.colonizer.dev` — and puts a
browser-facing front door on it: owner sign-in through GitHub, a per-install tunnel into the cockpit, and a
small signed API the mothership itself calls.

```
browser ──> my.colonizer.dev            register an install, mothership tunnel dial, signed cockpit API
browser ──> <install>.my.colonizer.dev  owner sign-in (/_auth) and everything proxied into the tunnel
                     │
                     └── D1 (installs, pairings) + one InstallTunnel Durable Object per install
```

## Trust boundaries

What the relay stores is the whole of what it knows (see `migrations/0001_installs.sql`):

- the **Ed25519 public key** an install registered with, its **created-at**, and the **owner binding**
  (GitHub id + login) once a pairing was confirmed;
- short-lived **pairing rows**: install id, 6-digit code, the waiting GitHub id/login, an expiry.

Nothing else. No request or response bodies, no headers, no IP addresses, no cookies, no GitHub access
tokens ever reach storage — the access token is used once, to read `/user`, and discarded. Logs carry only
method, path template, status, bytes and duration. The mothership's WebSocket and all proxied traffic are
end-to-end with the tunnel DO; the relay routes bytes, it does not read them.

The DO tunnel (connect, framing, offline handling) is described in `src/tunnel.js`; the worker never
forwards a client-supplied `x-relay-*` header — it strips them all and stamps its own verdicts.
Per stream, the response body must keep moving — 5 minutes without an inbound body frame fails the
stream — and a buffered (non-streaming) response body is capped at 8 MiB.

## Protocol encodings pinned here

Where the tunnel spec left the encoding open, these are now fixed (implementations on both sides must
agree; `src/protocol.js` is the reference):

- public keys and Ed25519 signatures are **standard base64** (padding included);
- nonces and tokens are **base64url without padding**;
- timestamps are **unix seconds**;
- the signed hello message is **UTF-8 concatenation** `nonce ‖ install_id ‖ ts` (no separators);
- stream ids are **integers**;
- one frame carries at most **36 KiB raw → 48 KiB base64**.

## Signed mothership API (apex, `my.colonizer.dev`)

Every request the mothership makes is authenticated with the key it registered: headers `x-colonizer-ts`
(unix seconds, `|now − ts| ≤ 300`) and `x-colonizer-sig` (base64 Ed25519 over the UTF-8 string
`METHOD\npathname\nts\nrawBody`, with `rawBody` empty when there is no body). Bad or missing → 401;
an oversized request body → 413.

- `POST /api/installs` `{"public_key": …}` → `201 {"install_id", "host"}` — 20 random base32 chars, 100
  bits; the key must be 32 bytes. Rate limited per IP when the `REGISTER_LIMITER` binding is present.
- `GET /tunnel/<install_id>` — the mothership's WebSocket dial (`426` for anything that is not an
  `Upgrade: websocket`); the worker adds `x-relay-kind: tunnel`, `x-relay-install-id`,
  `x-relay-public-key` and hands the request to the install's DO. Rate limited per IP when the
  `DIAL_LIMITER` binding is present. Inside the DO, pending handshakes are independent of each other
  and capped at 16 per install — the 17th dial is closed with `1013` before it can disturb the others.
- `GET /api/installs/<id>/pairing` → `{"owner": {"github_login"} | null, "pending": [{code, github_login,
  expires_at}]}` — what the local cockpit's Settings → Remote access shows.
- `POST /api/installs/<id>/pairing/confirm` `{"code": …}` → `200 {"owner": …}` — binds that pairing's
  GitHub account, deletes every pairing of the install (single-use). Unknown/expired/used → 404; an
  owner already bound → 409.
- `DELETE /api/installs/<id>/owner` → `204` — the "reset link": unbinds the owner and clears pending
  pairings. Existing sessions die on their next request, because each one re-checks the cookie against
  the current owner.

## Owner sign-in (`<install>.my.colonizer.dev`)

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
  stripped (the cockpit's own login cookie still reaches it); otherwise a plain GET/HEAD is redirected
  to `/_auth?next=…` and anything else, WebSocket upgrades included, gets 401.

Both cookies are `__Host-` prefixed: `Secure; HttpOnly; SameSite=Lax; Path=/`, no `Domain`, sealed with
HMAC-SHA256 under the `SESSION_SECRET` secret and verified in constant time.

## Deploy (done by a human, out of scope for the PR)

From this directory:

1. `npm ci`, then `node --test` — the suite runs on plain Node (24+), no Workers runtime needed.
2. `npx --no-install wrangler d1 create colonizer-relay` and paste the returned database id over the
   placeholder in `wrangler.toml` (marked with a comment).
3. `npx --no-install wrangler d1 migrations apply colonizer-relay --remote`.
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
