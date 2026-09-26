# Remote access: the tunnel contract

Any install can opt in to a stable link, `https://<install_id>.my.colonizer.dev`, that reaches its
own cockpit from anywhere, with no port forwarding and no tailnet. It is off by default. The
mothership dials out to a relay on colonizer.dev over a WebSocket; the relay forwards requests down
that connection, and turning the feature off closes the link immediately. Each install gets its own
subdomain rather than a path under one shared domain: with paths, every user's cockpit would share
one browser origin, and cookies and storage could leak between users. The link is an address, not a
key — the relay demands the owner's sign-in before forwarding anything, and the cockpit's own auth
still applies behind it.

**Status.** This is the pinned v1 wire contract for issue #531; nothing is implemented yet. #532
builds the relay (`services/relay`), #533 the mothership tunnel client and `/api/remote`, #534 the
owner sign-in at the relay, #535 the cockpit settings toggle and badge, #536 the security review.
Where this document pins something the issue text did not have — the `ready` frame, the `cancel`
frame, the close codes, the header pair format, the 101 accept — it says so.

## Install identity

On first enable the mothership generates an Ed25519 key pair and stores it under
`<config_dir>/remote/`: the directory 0700, the key file 0600, written the way `auth.rs` writes the
api-token (through `util::write_private`, `crates/colonizer/src/util.rs:328`). The exact file names
are #533's choice. The seed is never logged and never leaves the mothership.

The mothership registers the public key:

```
POST https://my.colonizer.dev/api/installs
{"public_key": "<base64>"}
```

`public_key` is the 32 raw Ed25519 public key bytes, base64-encoded. Responses:

| Status | Meaning |
| :--- | :--- |
| `201` | Registered. Body: `{"install_id": "…", "host": "…"}`. |
| `200` | This exact key is already registered; body is the same pair. Registration is idempotent, so a mothership that lost its stored pair recovers it by posting the key again. |
| `400` | Malformed key. |
| `429` | Rate limited. |

`install_id` is at least 16 random characters from the lowercase RFC 4648 base32 alphabet `a-z2-7`,
unpadded (DNS labels are case-insensitive). `host` is `<install_id>.my.colonizer.dev`. The
mothership stores the seed and the returned `install_id`/`host` under `<config_dir>/remote/`.

Disabling remote access keeps the key, so re-enabling gives the same link. A separate "forget"
action (#535, implemented in #533) deletes it.

**Encodings (pinned).** Every binary value on the wire — `public_key`, `nonce`, `sig`, body
chunks, binary `ws_msg` data — is standard base64 with padding (RFC 4648 §4).

## Tunnel handshake

The mothership dials `wss://my.colonizer.dev/tunnel/<install_id>`. The relay sends a challenge
immediately on upgrade:

```json
{"t": "challenge", "nonce": "<base64>"}
```

`nonce` is 32 random bytes. The mothership answers within 10 seconds:

```json
{"t": "hello", "sig": "<base64>", "ts": 1735689600, "version": 1}
```

- `ts` is a JSON integer: Unix seconds.
- The signed message is `nonce ‖ install_id ‖ ts`: the 32 decoded nonce bytes, then the
  `install_id` as ASCII bytes, then `ts` as ASCII decimal Unix seconds with no sign and no leading
  zeros. No separators between the three parts.

The relay verifies against the key registered for the `install_id` in the URL. It rebuilds the
message from its own values — it never parses a message back out of a frame — and only against the
nonce it sent on that same socket: a hello captured from one connection cannot be replayed on a
new one, which gets a fresh nonce. The `ts` window just bounds clock skew. It rejects when
|now − ts| > 60 s, and uses each nonce exactly once. A bad signature, a stale `ts` or a missing
`hello` closes the socket. Any other key closes the socket. An unknown `version` closes it too.

*Note for the #536 security review:* the signed message carries no domain-separation tag. That is
safe here because this key signs nothing but this handshake and the nonce is fresh per socket, so
no other signed artifact can be turned into a hello. A later version that changes the signed bytes
must add such a tag.

After a valid hello the relay sends `{"t": "ready", "host": "<install_id>.my.colonizer.dev"}`
(added to the issue text), so the mothership knows when it is live and can show "connected".

There is one live tunnel per install; a new tunnel replaces the old one.

**Close codes** (added to the issue text), in the WebSocket application range:

| Code | Meaning |
| :--- | :--- |
| `4400` | Malformed frame or protocol error. |
| `4401` | Bad signature, stale `ts`, or hello timeout. |
| `4404` | Unknown `install_id`. |
| `4409` | Replaced by a newer tunnel. |
| `4426` | Unsupported `version`. |

## Frames

Every frame is a JSON text frame. HTTP bodies travel as base64 chunks of at most 48 KiB (49152
decoded bytes) per frame.

| Frame | Direction | Fields |
| :--- | :--- | :--- |
| `challenge` | relay → mothership | `nonce` |
| `hello` | mothership → relay | `sig`, `ts`, `version` |
| `ready` | relay → mothership | `host` |
| `req` | relay → mothership | `id`, `method`, `path`, `headers` |
| `res` | mothership → relay | `id`, `status`, `headers` |
| `body` | both | `id`, `chunk`, `end` |
| `ws_open` | relay → mothership | `id`, `path`, `headers` |
| `ws_msg` | both | `id`, `data`, `binary` |
| `ws_close` | both | `id`, `code`, optional `reason` |
| `cancel` | both | `id` |
| `ping` / `pong` | both | — |

- `headers` is an array of `[name, value]` pairs with lowercase names (added to the issue text), so
  repeated headers such as `set-cookie` survive.
- `method` is uppercase. `path` is origin-form: it starts with `/` and includes the query string.
- `id` and `status` are JSON integers.

Every `req`, and every `res` that is not a `101`, is followed by one or more `body` frames; the
last carries `"end": true`. A message with no body sends exactly one
`{"t":"body","id":…,"chunk":"","end":true}`. A receiver MUST close the tunnel with 4400 on a body
chunk above 49152 decoded bytes.

`cancel` (added to the issue text): `{"t":"cancel","id":…}` in either direction abandons a stream —
the browser went away, or the mothership aborted. The receiver drops the stream and frees its slot.
There is no reply.

Unknown `t` values and unknown fields inside a known frame are ignored (forward compatibility) —
except in what the relay receives from the mothership before a valid hello, where anything but
`hello` closes the tunnel with 4400.

## Streams and limits

- `id` is a positive integer allocated by the relay, unique among the tunnel's open streams.
- A stream holds one slot from its `req` or `ws_open` until both directions have ended: a final
  `body` frame both ways, or `ws_close`, or `cancel`. A single `ws_close` from either side ends the
  stream and frees the slot on both sides; no reply is expected.
- At most 32 streams are open per tunnel.
- The relay answers a 33rd concurrent browser request itself, with `503` and `retry-after: 1`,
  without touching the tunnel. Should the mothership still receive an over-limit `req`, it answers
  with `res` status `503`.
- A receiver may close the tunnel with 4400 on any text frame above 128 KiB.
- When a tunnel closes or is replaced, the relay fails its in-flight HTTP streams with `502` — or
  drops the browser connection if the response had already started — and closes its browser
  WebSockets with code 1001. The mothership drops all streams of the old tunnel.

## WebSocket passthrough

The relay treats a browser request as a WebSocket when its `upgrade` header contains the token
`websocket`, case-insensitive, and turns it into a `ws_open`; it does not hardcode paths. Any other
`upgrade` value is stripped as hop-by-hop and the request travels as a plain `req`. (The issue text
named `/events` and `/ws`, but the cockpit's actual WebSocket routes are `/api/stream`,
`/api/sessions/{id}/events` and `/api/sessions/{id}/terminal` —
`stream::routes` and `sessions::routes` in `crates/colonizer/src`. Path-agnostic forwarding covers whichever exist.)

`ws_open` only flows relay → mothership. The mothership accepts by answering with a `res` of status
`101` (added to the issue text), which carries no `body` frames — `ws_msg` and `ws_close` follow
directly — and whose headers include `sec-websocket-protocol` if the mothership picked one. It
refuses with a `res` of any other status plus its `body` frames.

`ws_msg.data` is UTF-8 text when `binary` is `false`, and base64 when `binary` is `true`. A
`ws_msg` payload fits in one frame: a sender whose application message exceeds 96 KiB decoded
closes the stream with `ws_close` code 1009 instead of sending it. `ws_close` carries a `code`, a
JSON integer 1000–4999, and an optional string `reason`. The relay terminates the browser's own
socket, so it strips the `sec-websocket-*` headers on `ws_open` — except `sec-websocket-protocol`,
which it forwards.

## Keepalive and reconnect

Either side sends `{"t":"ping"}` every 20 s; the other answers `{"t":"pong"}`. If no frame of any
kind arrives for 60 s, close the tunnel and — on the mothership — reconnect.

Reconnect rules:

| Close | Mothership behaviour |
| :--- | :--- |
| `4401`, `4404`, `4426` | Do not retry fast. Surface the error in `GET /api/remote` instead. |
| `4409` | Do not reconnect: another process holds the tunnel. |
| Anything else, or network failure | Reconnect with exponential backoff from 1 s to 60 s, with jitter. |

## Headers

Hop-by-hop headers are stripped on both sides: `connection`, `keep-alive`, `proxy-authenticate`,
`proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`, and any header listed in
`connection`. `content-length` may be sent, but receivers recompute it from the body frames.

The relay MUST drop every client-supplied `host` header and send exactly one
`host: <install_id>.my.colonizer.dev`; the browser's `origin` passes through unchanged. The
mothership answers `res` status `400` to a `req` or `ws_open` carrying zero or more than one
`host`, or any host other than its own tunnel host.

The relay MUST strip any `Domain` attribute from forwarded `set-cookie` headers, so cockpit
cookies stay host-only: one install can never toss cookies onto `my.colonizer.dev` or onto another
install's host. It removes its own `__Host-` session cookie from `cookie` before forwarding, so
the mothership never sees relay credentials. It adds no client-identifying headers — no
`x-forwarded-for`.

## Trust boundaries

### Relay

- The relay terminates TLS, so cockpit cookies, tokens and bodies pass through it in transit; the
  guarantee is that it stores and logs none of them.
- Its per-request log holds: the method, a path template (query string dropped, id-like path
  segments replaced by `:id`), the status, byte counts in and out, and the duration.
- A tunnel that is offline gets a `502` from the relay with a short "this cockpit is offline" page.
- A visitor who has not signed in gets the relay's owner sign-in (#534) and is never forwarded to.
  The relay serves `<install_id>.my.colonizer.dev` only after that sign-in; its session cookie is
  `__Host-`-prefixed (the exact name is #534's choice) with `Secure; HttpOnly; SameSite=Lax;
  Path=/`, which scopes it to that one host.

### Mothership

Today the cockpit answers to loopback only. `host_guard` (`crates/colonizer/src/server.rs`)
rejects any `Host` that is not `localhost`, `127.0.0.1`, `[::1]`, the bind host or in
`COLONIZER_ALLOWED_HOSTS`, then requires the per-install API token (`crates/colonizer/src/auth.rs`)
as `Authorization: Bearer` or the `colonizer_token` cookie; cookie-authenticated writes and
WebSocket upgrades must also carry an `Origin` that matches the `Host` header (#375,
`server.rs`). The cookie is `HttpOnly; SameSite=Strict; Path=/`, host-only, and carries no
`Secure` today (`auth.rs:131–133`).

Tunnelled requests are dispatched in-process into the same axum router, so `host_guard` and the
cockpit auth run unchanged; the relay does not bypass either. The tunnel client marks each request
with a request extension that only it can set, and the fence exemption keys off that marker, never
off the `Host` header alone — a local process can send any `Host` header to `127.0.0.1`. With the
marker, and only while remote access is on (checked per request, not at connect time),
`host_guard` accepts `Host` `<install_id>.my.colonizer.dev` and `Origin` exactly
`https://<install_id>.my.colonizer.dev`.

Turning remote access off closes the tunnel and fails any in-flight tunnelled streams. The
`colonizer_token` cookie set through the tunnel lands on the tunnel host only (a host-only cookie),
separate from the localhost one; over the tunnel it MUST also carry `Secure`.

## Cockpit API

For #533 to implement and #535 to consume, following the existing toggle pattern
(`GET`/`PUT /api/telemetry`, `crates/colonizer/src/telemetry.rs:299–319`). Both routes sit behind
`host_guard` like every `/api/*` route:

```
GET /api/remote
PUT /api/remote   {"enabled": true|false}
```

Both answer with the same body:

```json
{
  "enabled": false,
  "state": "off",
  "url": null,
  "error": null
}
```

| Field | What it is |
| :--- | :--- |
| `enabled` | Whether remote access is switched on. |
| `state` | `"off"`, `"registering"`, `"connecting"`, `"connected"` or `"error"`. |
| `url` | `"https://<host>"` once registered, otherwise `null`. |
| `error` | A human-readable string while `state` is `"error"`, otherwise `null`. |

The relay's pairing flow and the cockpit badge belong to #534/#535 and are not defined here.

## Test vector

Both implementations must agree on these exact bytes. The key is RFC 8032 §7.1 TEST 1; Ed25519 is
deterministic, so every correct signer produces exactly this signature.

| Value | |
| :--- | :--- |
| seed (hex) | `9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60` |
| `public_key` (base64) | `11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=` |
| nonce: bytes `0x00..0x1f` (base64) | `AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=` |
| `install_id` | `abcdefghijklmnop` |
| `ts` | `1790000000` |

The signed message is the 32 nonce bytes, `abcdefghijklmnop` and `1790000000` concatenated — 58
bytes, in hex:

```
000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
6162636465666768696a6b6c6d6e6f70
31373930303030303030
```

The signature, base64:

```
PlEkUx+L9jCzIUWPc9EOslGjU8z2Cz1CmO6mJ4iIK5zNTQp+4gxGR3TQupbPSU0cgaMLfcNbUUlN8VxmKLJVCg==
```

And the resulting hello frame:

```json
{"t": "hello", "sig": "PlEkUx+L9jCzIUWPc9EOslGjU8z2Cz1CmO6mJ4iIK5zNTQp+4gxGR3TQupbPSU0cgaMLfcNbUUlN8VxmKLJVCg==", "ts": 1790000000, "version": 1}
```

Computed with Node's `crypto.sign`; verified independently with
`openssl pkeyutl -verify -rawin` (OpenSSL 3.0.20) and the RFC 8032 reference algorithm.

## What is not colony work

The human steps, before any of this can run: wildcard DNS for `*.my.colonizer.dev`, the GitHub
OAuth app behind the owner sign-in (#534), deploying `services/relay`, and submitting
`my.colonizer.dev` to the Public Suffix List (private section), so browsers treat every install
host as its own site.
