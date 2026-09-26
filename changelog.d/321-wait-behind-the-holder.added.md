- **Wait behind the holder.** A launch on an issue another colony already holds can now queue for
  the issue instead of being refused or duplicating it: `queue_behind_holder` on `POST /api/sessions`
  admits the colony as a `claim_wait` successor (`queued_behind` naming the holder), the oldest
  waiter takes over when the holder releases — carrying the GitHub `colonizer:claimed` mark with
  it — and the cockpit's launch form offers the choice beside Allow duplicate, with each waiter's
  place in line on its Inspector card. ([#321])

[#321]: https://github.com/Colonizer-dev/harness/issues/321
