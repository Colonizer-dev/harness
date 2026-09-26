- **Remote access: the relay.** A Cloudflare Worker at `my.colonizer.dev` puts every install one
  subdomain away: the mothership registers an install — an Ed25519 public key in D1, nothing else —
  and dials `/tunnel/<id>`, where a Durable Object per install holds its tunnel: an Ed25519
  challenge/hello handshake proves the dialer holds the key, one live tunnel per install (a new dial
  replaces the old and fails what was riding on it), request, response and body frames for HTTP plus
  `ws_*` passthrough frames for WebSockets, 32 streams at a time, a per-install token-bucket rate
  limit, and a 502 offline page when no tunnel is connected. The owner signs in through GitHub; on
  an install with no owner the account is parked behind a 6-digit pairing code that the local
  cockpit confirms with a signed request, so the machine gets the final say on who owns it. No
  request or response body is ever stored or logged: D1 keeps the public key, the created-at and
  the owner binding, and the access log is method, path template, status, bytes and milliseconds.
  Deployment is manual (services/relay/README.md). ([#532], [#534])

[#532]: https://github.com/Colonizer-dev/harness/issues/532
[#534]: https://github.com/Colonizer-dev/harness/issues/534
