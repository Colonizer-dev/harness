- **Remote access: drive the cockpit through a relay.** An opt-in switch (`PUT /api/remote`) that
  keeps one outbound WebSocket to `my.colonizer.dev` — nothing is ever listened on — and serves the
  cockpit's own API and websockets through it, so the mothership is reachable from outside the
  machine without opening a port or joining the mesh. The install proves itself with an Ed25519 key
  (generated on first use, kept 0600 under the config dir, never logged) that the relay binds to a
  per-install host at registration; every tunnelled request then lands on the same router as
  localhost, under the same API token, admitted only while the switch is on, only for the tunnel's
  own Host, with the Origin fence pinned to exactly `https://<host>` — the tunnel host is never a
  LAN host. Disabling closes the tunnel and every in-flight stream at once but keeps the key;
  `POST /api/remote/reset` retires a leaked identity with a fresh one, reconnecting immediately.
  `GET /api/remote` shows the switch, the host and whether the link is live. Switch changes are
  recorded in the activity log. See [docs/protocol.md](docs/protocol.md) §6.10. ([#533])

[#533]: https://github.com/Colonizer-dev/harness/issues/533
