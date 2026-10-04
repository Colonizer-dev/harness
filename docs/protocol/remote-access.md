# 6.10 Remote access (tunnel)

Part of the [Colonizer protocol](../protocol.md).

Drive this mothership's cockpit from outside the machine without opening a port: the mothership
keeps one outbound WebSocket to a relay (`wss://my.colonizer.dev`), and the cockpit is served
*through* that connection. Nothing is ever listened on. A tunnelled request lands on the same
router as a localhost request, with the same token check (`host_guard` in `server.rs`, #405); the only things
that differ are who is allowed to name the Host, and what the Origin fence accepts.

The identity is an Ed25519 key pair, generated on first use and kept at `<config>/remote/key`
(PKCS#8, mode 0600, never logged) beside the switch state at `<config>/remote/state.json`:

```json
{"enabled": false, "install_id": null, "host": null}
```

`install_id` and `host` are what the relay answers at registration:
`POST https://my.colonizer.dev/api/installs` with `{"public_key": "<base64 of the raw 32 bytes>"}`
answers `{"install_id": "...", "host": "<install_id>.my.colonizer.dev"}`. The relay binds the
install to that host, so the mothership's tunnel URL is `wss://<relay>/tunnel/<install_id>` and
the Host every tunnelled request carries is `<install_id>.…`. The relay base is
`COLONIZER_REMOTE_URL` (default `wss://my.colonizer.dev`); the registration URL is the same host
over `https://` (`http://` for a `ws://` base) plus `/api/installs`.

## `GET /api/remote`

```json
{"enabled": true, "host": "c1c9215b.my.colonizer.dev", "connected": true, "since": "2026-09-25T00:16:11+00:00", "replaced": false}
```

`connected` is true only while the switch is on *and* the tunnel's handshake has succeeded; a
stale status after a disable never reads as a live link. `since` is when the current tunnel came
up. `replaced` is true only while the switch is on and the relay has parked the tunnel because a
newer one took this install over (a second mothership on the same key); a re-enable, a reset or a
fresh connect clears it.

## `PUT /api/remote {"enabled": bool}`

Enabling mints the key if this install has no key file yet, registers it if the state names no
`install_id` yet, persists and wakes the supervisor. If the relay refuses or misses the
registration the answer is **502** and the switch stays off; if a key file exists but cannot be
read or parsed, the answer is **500** naming `POST /api/remote/reset` as the way out — the key is
never silently replaced, since the registered `install_id` still names the old key. Disabling
persists `enabled: false` and tears the tunnel and every in-flight stream down at once; the key
is kept, so re-enabling comes back under the same host. A PUT that does not change the switch
answers the current view and records nothing.

## `POST /api/remote/reset`

A fresh key, registered at once and persisted in place of the old identity; if the switch was on
the tunnel redials immediately under it. The way to retire a key that leaked. It answers the new
view. Before the old key is replaced, the old install's owner is unbound at the relay (a signed
`DELETE`, best effort), so the new link starts unowned and the old one has no owner left.

The three switches record `remote.enable`, `remote.disable` and `remote.reset` in the activity
log (§6.9), with actor `you`, but only when the state actually changes.

## Pairing: `GET /api/remote/pairing`, `POST /api/remote/pairing/confirm`, `POST /api/remote/pairing/reject`, `DELETE /api/remote/owner`

The relay forwards only the install's bound owner. The first GitHub sign-in on the link gets a
six-digit, single-use code that expires after 10 minutes, and the local cockpit confirms it (#534,
#599). Each route is one signed call to the relay's `…/api/installs/<install_id>/…` endpoint:
`x-colonizer-ts` in Unix seconds and `x-colonizer-sig`, Ed25519 by the install key over
`METHOD\npath\nts\nbody`, standard base64. [remote-tunnel.md](../remote-tunnel.md#pairing-and-the-owner)
has the relay side.

- `GET /api/remote/pairing` answers the relay's view as is:
  `{"owner": {"github_login": "octocat"} | null, "pending": [{"code": "481516", "github_login": "octocat", "expires_at": 1790000600}]}`.
- `POST /api/remote/pairing/confirm {"code": "481516"}` binds that code's GitHub account and answers
  `{"owner": {"github_login": "octocat"}}`. **400** unless six digits, **404** for an unknown,
  expired or already-used code, **409** when an owner is already bound. Records `remote.pair`.
- `POST /api/remote/pairing/reject {"code"}` drops one pending code and answers
  `{"github_login": "…"}`; **400**/**404** as for confirm. Records `remote.pair_reject`.
- `DELETE /api/remote/owner` unbinds the owner and drops pending codes: **204**. The owner's relay
  sessions end on their next request. Records `remote.unpair`.

All four are owner-only (a scoped token gets **403**). Confirm, reject and unbind are
**local-only**: a request carrying the tunnel's `Tunnelled` marker gets **403**, so nothing that
arrives through the link can bind, refuse or drop an owner. **409** means remote access was never
switched on (no install to pair); a relay that cannot be reached, or answers `401`/`404` for the
install itself, is a **502** naming it.

## The tunnel, version 1

Frames are JSON text over the WebSocket. The relay opens with a challenge and the mothership
proves the key:

```json
{"t": "challenge", "nonce": "Nvc3ByZWxheQ"}
{"t": "hello", "sig": "<base64>", "ts": 1789000000, "version": 1}
```

`sig` is Ed25519 over the UTF-8 bytes `nonce + install_id + ts` (`ts` in unix seconds, the same
decimal the JSON carries); the base64 is standard and padded, of the raw 64-byte signature. A
nonce longer than 1 KiB is refused. A relay that cannot verify the hello has no business talking
to this install.

After the hello, frames go both ways:

- `{"t": "req", "id", "method", "path", "headers"}` opens a tunnelled HTTP request. `headers` is
  an array of `[name, value]` pairs (an object of name to value is accepted too). `id` is the
  relay's opaque stream id.
- `{"t": "body", "id", "chunk", "end"}` moves body bytes, base64 of at most 48 KiB raw per frame,
  in either direction. The last frame of a body always carries `"end": true` — a single
  `{"chunk": "", "end": true}` when there is no body. `GET` and `HEAD` requests carry no body
  frames at all; any other method ends its body with the empty frame. A tunnelled request body
  larger than 10 MiB is refused with **413**, and one that never ends within a minute per frame
  is answered **408** — a stream is never left hanging without a status.
- `{"t": "res", "id", "status", "headers"}` answers a request (hop-by-hop headers stripped, the
  same list both sides), followed by the answer's body frames, the last with `end: true`.
- `{"t": "ws_open", "id", "path", "headers"}` opens a tunnelled WebSocket: the mothership dials
  its own router over an in-memory connection, so the upgrade is a real one. `ws_msg`
  `{"t": "ws_msg", "id", "data", "binary"}` carries text as-is and binary as base64; `ws_close`
  `{"t": "ws_close", "id", "code"}` closes either way.
- `{"t": "ping"}` / `{"t": "pong"}`. Both sides ping every 20 s; about 60 s of total silence ends
  the tunnel.

One tunnel carries at most **32** concurrent streams, requests and tunnelled websockets together,
and an `id` names at most one of them: a `req` or `ws_open` repeating a live stream's id is
refused (`503` with an empty body, or `ws_close` `1008`) without touching the open stream. A `req`
beyond the thirty-two is answered `503` with an empty body; a `ws_open` beyond it is answered
`ws_close` `1013`. A tunnelled websocket whose inner upgrade fails is answered `1014`; one whose
path does not start with `/` (or runs past 2048 characters) `1008`; an inner error `1011`; a
clean end `1000`. A tunnelled `req` with a bad method, bad base64 or a bad path is answered
**400**, and an answer whose next body frame does not come within 300 s ends its stream. Answers
are queued to the relay behind a bounded buffer, so a relay that stops
reading slows a streaming answer instead of growing it without limit.

If the relay goes away, the mothership redials after 1 s, doubling to at most 60 s, with a little
jitter so a relay blip does not align every install's retries. The backoff resets to 1 s only
once the relay has shown it accepted the hello — its first frame on the connection; the socket
merely coming up counts for nothing, so a relay that hangs up on a bad signature cannot turn the
redial into a silent one-second loop (the log says so, distinctly). A disable or reset needs no
backoff: the redial happens at once.

One close is never redialed: a close frame saying this tunnel was replaced — the `4000` the relay
sends for it, or the `4409` its contract pins — means another mothership dialed on the same
install key, and dialing back would have the two replace each other forever. The supervisor parks
instead, waiting on the switch/reset signal like a disabled tunnel, and `GET /api/remote` reports
`"replaced": true`; a re-enable or reset dials again and clears the flag.

Tunnelled requests skip the LAN host allowlist — the tunnel host is never a LAN host — but only
for a request the supervisor itself decoded (the `Tunnelled` marker cannot be forged from outside
the process), only while the switch is on, and only when the Host header is exactly the tunnel's
own host; anything else naming a `Tunnelled` extension is answered **503**. Authentication is the
cockpit's own, unchanged; the Origin fence for cookie writes accepts exactly `https://<host>`
through the tunnel, where the LAN fence accepts any scheme.
