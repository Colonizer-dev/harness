# Remote access: security review

This is the security review [#536](https://github.com/Colonizer-dev/harness/issues/536) asks for
before the remote-access relay is deployed for the first time. It was carried out on 2026-09-26
against three revisions, and every line reference below points at them:

- **The relay**: `services/relay/` at commit `5325415`
  ([#555](https://github.com/Colonizer-dev/harness/issues/555), on `main`). `worker.js`, `tunnel.js`,
  `protocol.js`, `auth.js` and `wrangler.toml` refs are all under `services/relay/`.
- **The tunnel client**: `crates/colonizer/src/remote.rs`, plus its `host_guard` and `/api/remote`
  changes in `crates/colonizer/src/main.rs`, at commit `a518798` — the
  [#533](https://github.com/Colonizer-dev/harness/issues/533) branch, unmerged at review time.
- Other cockpit refs (`auth.rs`, `notify.rs`, `providers.rs`, `gateway.rs`, `mesh.rs`, `util.rs`,
  `activity.rs` under `crates/colonizer/src/`) are at `main`.

Method: code reading split by threat area — the relay edge, the frame protocol and tunnel, and the
cockpit boundary — plus Node proof-of-concept runs against the shipped relay modules, and an
independent re-check of every filed finding by a second reviewer.

Outcome: the cockpit's own fences hold for tunnelled traffic. No tunnelled request skips
`host_guard`, nothing reaches loopback services the cockpit does not itself serve, and the `Origin`
fence from [#375](https://github.com/Colonizer-dev/harness/issues/375) is pinned for the tunnel
host rather than widened. The id, cookie and pairing design at the relay holds too. Five findings
were filed as separate security issues; three of them (R1–R3) should block deployment.

## Threat-model verdicts

| Threat | Verdict | Where it is held, or the finding |
| :--- | :--- | :--- |
| Install-id guessing and enumeration | Holds | Ids are 20 base32 characters — 100 bits from `crypto.getRandomValues` (`worker.js:89-104`) — and register and dial are per-IP rate limited (`wrangler.toml:38-48`; `worker.js:62-64`, `:111-113`). Unknown, registered, bound and online installs do answer differently (404 / 302 to `/_auth` / 403 / 502 / proxied), but that oracle is useless against 100-bit ids. Registration growth is L2 below. |
| A stolen session cookie | Holds, one caveat | `__Host-colonizer_session`: Secure, HttpOnly, SameSite=Lax, no Domain, HMAC-SHA256 sealed, 7 days (`auth.js:11-21`); bound to one install and its current GitHub owner and re-checked against D1 on every request, so unbinding revokes it at once (`auth.js:186-195`; `worker.js:223`); stripped before forwarding to the cockpit (`auth.js:197-209`; `worker.js:224-226`); fails closed without `SESSION_SECRET` (`auth.js:53-61`). Caveat: logout only clears the browser copy (`auth.js:180-182`) — revoking one stolen cookie means unbinding the owner. |
| A relay compromise | Finding R3 (and R2) | The relay sees every request and response in cleartext, including the cockpit's api-token, and can act as the owner. The hello signature — Ed25519 over nonce, install id and timestamp, a fresh 32-byte nonce per dial, ±300 s skew (`tunnel.js:70`, `:106-107`) — stops anyone from impersonating an install to the relay, but nothing authenticates the relay to the cockpit beyond WebPKI TLS. |
| Request smuggling through the frame protocol | Holds | Relay stream ids are a monotone counter, and unknown or late ids are ignored (`tunnel.js:43`, `:154-155`, `:186`, `:246`); the client refuses duplicate ids without touching the live stream (`remote.rs:704-730`). `clean_headers` strips hop-by-hop headers, Host and `sec-websocket-*`, and drops values containing CR, LF or NUL (`remote.rs:1030-1055`); Host is always overwritten with the registered tunnel host (`remote.rs:789`); the body is rebuilt from frames in-process, so Content-Length and Transfer-Encoding are inert; `//evil.com/x` parses as a path with no authority, and absolute-form fails `plausible_path` (`remote.rs:583`). Nothing is ever dialled out — every request is answered by `router.oneshot`. R1 is an interop crash in this layer, not a smuggling vector. |
| WebSocket hijack | Holds | A cross-site handshake does not carry the SameSite=Lax relay session cookie and gets 401 at the relay (`worker.js:222-235`); a tunnelled cookie-authenticated upgrade still needs Origin to equal `https://<tunnel host>` exactly at the cockpit (`main.rs:966-978` at a518798); the relay forwards Origin untouched. Subprotocol negotiation is broken (L4). |
| Origin-fence bypass (#375's Origin-absent class) | Holds | For tunnelled requests Origin must equal `https://<host>` exactly; absent, `null`, `http://`, a sibling install's origin and a port mismatch all fail (`main.rs:967-969`; tests at `remote.rs:1353-1396`). The local fence is not widened: the tunnel host is admitted only with the in-process `Tunnelled` extension and the switch on (`main.rs:939-950`), so a DNS-rebinding page sending `Host: <id>.my.colonizer.dev` to `127.0.0.1:7878` gets 403. With remote access off, tunnelled requests get 503 and the host is refused locally. |
| DoS of one install | Findings R5 and R1's slot leak | Otherwise held at the relay by MAX_STREAMS 32, MAX_PENDING 16, a 120-burst / 20 rps bucket per install, chunk caps, 8 MiB body backpressure and head/idle/hello timers (`tunnel.js:59-86`, `:171-197`, `:239-248`, `:375-385`; `protocol.js:4-15`), and at the client by MAX_STREAMS, a 10 MiB MAX_BODY, a 60 s BODY_WAIT and a bounded 256-frame outbound queue (`remote.rs:59-81`, `:534-536`, `:756-777`). |
| DoS of the relay itself | Holds | Durable Objects are only allocated for known ids, behind the dial limiter and MAX_PENDING with 10 s hello timeouts (`worker.js:110-126`; `tunnel.js:59-86`); apex JSON bodies are capped at 1 KiB by bytes (`worker.js:243-264`); Ed25519 verification runs once per hello. Notes: every request to an unknown subdomain costs one D1 read with no limiter (`worker.js:219`), and registration rows accumulate (L2). |
| Key theft from `<config>/remote/` | Finding R2; L3 | The key file is written 0600 (`util.rs:328-339`) and no endpoint or log returns key material; registration sends only the public key. |
| Logging leaks | Holds | The relay logs one line per stream: method, a templated path (query dropped, id-shaped segments replaced by `:id`), status, bytes, ms (`tunnel.js:368-370`; `protocol.js:73-77`); the worker logs nothing; error pages name missing variables, not values (`auth.js:61`). The client's activity entries record kind, via and target only, never query strings or bodies (`remote.rs:331-340`; `activity.rs:646-661`). Minor: see L5. |
| The pairing-code race | Holds | Codes are 6 uniform digits with a 10-minute expiry (`auth.js:15`, `:89-99`). Confirming requires the mothership's Ed25519 signature, so a browser can neither brute-force nor forge a confirm (`worker.js:54-56`, `:130-142`). The bind is `UPDATE … WHERE owner_github_id IS NULL` inside one D1 batch, so two concurrent confirms bind exactly one owner and the loser gets 409 (`worker.js:176-184`). One dependency on the cockpit side: the confirm screen must show each pending code's `github_login` prominently (`worker.js:155`), because an owner who confirms an attacker's pending pairing binds the attacker. Signed endpoints accept a 300 s replay window without a nonce (`worker.js:28`, `:136`); the mutations they guard are idempotent or first-wins, so the impact is negligible. |

## The two boundary checks #536 asks for

**A tunnelled request cannot skip the cockpit's own auth.** HTTP goes through `serve_req` →
`router.oneshot` on the full router, where `host_guard` is layered after `web_router` is merged
(`main.rs:1634-1637`; `remote.rs:796`). WebSockets go through an in-memory `axum::serve` of the same
router with the `Tunnelled` extension layered outside it (`remote.rs:605-607`). `Tunnelled` is
constructed only in `remote.rs` (`:605`, `:795`) and cannot come from a frame or a header;
`Authenticated` / `Via` are inserted by `host_guard` itself, never read from headers; nothing in the
crate uses `ConnectInfo`, so no handler trusts a peer address. The relay's GitHub sign-in is a
second gate, not a substitute: the browser still needs the cockpit token.

**A tunnelled request cannot reach loopback-only services the cockpit doesn't serve.** The client
opens TCP only to the relay — register and dial (`remote.rs:288-300`, `:414-447`); everything else
is in-process. Two pre-existing settings reachable by any token holder do accept loopback URLs —
the notify webhook (`notify.rs:449-474`) and a provider's `base_url` (`providers.rs:512-527`) — but
the loopback services they could reach authenticate on their own (the gateway wants a per-colony
token, `gateway.rs:626-641`; headscale sits behind a 0700 socket and API keys,
`mesh.rs:245-296`), so a remote token holder gains nothing a local one lacks. Blocking loopback and
link-local targets there is hardening independent of remote access
([sandbox-network.md](sandbox-network.md) covers the colony-side fence).

## Findings filed as separate security issues

The colony could file at most five issues; each finding was confirmed by code reading and
re-checked independently. The titles are exact, so the issues can be found:

| Id | Severity | Issue | Where |
| :--- | :--- | :--- | :--- |
| R1 | High | "Remote access: relay throws on the tunnel client's array-shaped `res` headers, hanging tunnelled responses and leaking stream slots" | `remote.rs:800-807` at a518798; `protocol.js:46-51`; `tunnel.js:158-161` |
| R2 | High | "Remote access: reset_identity leaves the old install live at the relay, so a leaked &lt;config&gt;/remote/key keeps receiving the owner's traffic" | `remote.rs:245-263`; `worker.js:47-59` |
| R3 | High | "Remote access: the cockpit api-token crosses the relay in cleartext, survives every reset, and COLONIZER_REMOTE_URL accepts plaintext ws://" | `worker.js:224-230`; `main.rs:961-966`, `:1001-1014` at a518798; `auth.rs:45-59`; `remote.rs:319-327` |
| R4 | Medium | "Remote access: relay forwards tunnelled Set-Cookie with Domain=, letting one install toss cookies onto sibling installs; cockpit cookie lacks Secure" | `protocol.js:23-66`; `auth.rs:135-137` |
| R5 | Medium | "Remote access: tunnel client queues relay-to-cockpit frames without bound (ws_in, bodies), so a relay or remote browser can exhaust mothership memory" | `remote.rs:484`, `:493`, `:445-451`, `:756-766` |

How each one works:

- **R1.** The client sends response headers as `[name, value]` pairs (`remote.rs:800-807` at
  a518798); the relay's `stripHopByHop` iterates them as `[index, pair]` and throws
  (`protocol.js:46-51`), after `#onRes` has already cleared the head timer (`tunnel.js:158-161`).
  Every real response hangs, and after 32 the install answers 503. The relay's tests only use
  object-shaped headers, which is why the shipped modules pass their own suite.
- **R2.** `reset_identity` registers a new install but never retires the old one
  (`remote.rs:245-263`), and the relay has no endpoint to do so (`worker.js:47-59`). The old
  install stays owner-bound, so the phone's traffic to the old origin goes to whoever dials with
  the leaked key.
- **R3.** `worker.js:224-230` forwards the `colonizer_token` cookie; a bearer skips the Origin
  fence (`main.rs:961-966` at a518798); no code rotates the token (`auth.rs:45-59`); the `?token=`
  sign-in path works through the tunnel (`main.rs:1001-1014`); and `ws://` relays are accepted
  without a loopback restriction (`remote.rs:319-327`).
- **R4.** Installs are sibling subdomains of a domain that is not on the Public Suffix List, the
  relay passes `Set-Cookie … Domain=` through (`protocol.js:23-66`), and the cockpit's
  `colonizer_token` has no `Secure` (`auth.rs:135-137`). Impact is sign-in confusion or DoS on the
  victim's install, not takeover.
- **R5.** Unbounded channels (`remote.rs:484`, `:493`), default 64 MiB tungstenite messages
  (`remote.rs:445-451`), and base64 decoded before the size check (`remote.rs:756-766`): an
  authenticated browser can grow `ws_in` against a slow handler, and a relay can do worse.

## Lower-severity notes, not filed

Each was confirmed by code reading; L1 also by a proof-of-concept run.

- **L1, Low — open redirect after sign-in.** `safeNext` blocks `//` and `/\` but not `/` followed by
  a tab (`auth.js:81-85`). `next=%2F%09%2Fevil.example%2F` becomes `Location: /\t/evil.example/`,
  which browsers resolve to `https://evil.example/` right after a successful GitHub sign-in
  (`auth.js:136`, `:152`, `:161`). Fix: reject control characters, or resolve against the install's
  own origin and require it unchanged.
- **L2, Low — unbounded registrations.** `POST /api/installs` is unauthenticated beyond a per-IP
  limiter the code treats as optional (`if (env.REGISTER_LIMITER)`), and nothing ever deletes
  unowned installs; only pairings are purged (`worker.js:61-87`; `auth.js:211-214`). Fix: make the
  limiter mandatory and collect unowned, never-dialled installs.
- **L3, Low — key directory mode and plaintext key.** `<config>/remote/` is created with a bare
  `create_dir_all` (umask, typically 0755; `remote.rs:169`, `:275`), and the key is plaintext
  PKCS#8 outside the `COLONIZER_MASTER_KEY` envelope (`util.rs:109-137`). The 0700 config directory
  protects it in the default layout, so this is defence in depth.
- **L4, Low — WebSocket subprotocols cannot be negotiated.** The relay keeps
  `sec-websocket-protocol` so the cockpit can negotiate (`protocol.js:36-38`), but the client
  strips every `sec-websocket-*` header (`remote.rs:1051`) and the relay accepts the browser with a
  bare `server.accept()` (`tunnel.js:298`).
- **L5, Low — audit and error hygiene.** `remote.*` activity entries record `via: cockpit|api` but
  not that the caller came through the tunnel, and error bodies returned through the tunnel include
  local filesystem paths (`remote.rs:280-283`).
- Also worth knowing: the short-lived `__Host-colonizer_oauth` cookie is forwarded to the cockpit
  (only the session cookie is stripped). Harmless, but unnecessary.

## Before the relay is deployed

- R1 fixed, with a relay e2e test that uses the Rust client's exact frame shape.
- R2 and R3 fixed, or the relay's trust level — it can read everything and act as the owner —
  accepted in writing.
- Deploy configuration: `SESSION_SECRET` set (the relay fails closed without it), `REGISTER_LIMITER`
  and `DIAL_LIMITER` bindings deployed, the real `GITHUB_CLIENT_ID` and D1 `database_id` in place of
  `wrangler.toml`'s placeholders, and the GitHub OAuth app's wildcard callback matching enabled
  (`services/relay/README.md:98-107`).
- The tunnel-client half of this review re-run once #533 merges, since `a518798` was unmerged at
  review time.
