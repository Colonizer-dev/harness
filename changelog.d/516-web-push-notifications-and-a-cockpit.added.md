- **Web Push notifications, and a cockpit that works on a phone.** When a colony needs an answer,
  stalls, fails or opens a pull request, the mothership can now wake the phone on the operator's
  nightstand: the browser subscribes from Settings → Notifications and the harness delivers an
  RFC 8291-encrypted Web Push message, authorized by a VAPID key generated on first use and kept
  beside the other secrets. Subscribing is the opt-in — no new module setting to forget — each
  device carries a name of the operator's choosing and can be revoked alone, and a push service
  reporting an endpoint gone (404/410) drops the subscription by itself. A push carries only what
  the desktop popup already carries — a short title, the colony's one line, and a link back into
  the cockpit for the colony it names — never question text, agent output, or repository content,
  and it passes the same anti-spam ledger as every other channel. The cockpit grows a
  narrow-screen bottom tab bar, so the same pages work on the phone the pushes arrive on.
  ([#516])

[#516]: https://github.com/Colonizer-dev/harness/issues/516
