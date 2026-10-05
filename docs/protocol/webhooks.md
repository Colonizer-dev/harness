# Webhooks

Part of the [Colonizer protocol](../protocol.md).

The notify module (§6.3, [Notify](mothership-api.md)) can POST one JSON note per event to a webhook
URL. This page is what a receiver needs: the request, the payload, how to verify it and how to
dedupe it. Which events exist and when they fire is in the Notify section of the
[mothership API](mothership-api.md).

## The request

```
POST <webhook_url>
content-type: application/json
X-Colonizer-Timestamp: 1789000000
X-Colonizer-Event-Id: evt_3f9c0d1e2a4b5c6d7e8f90a1b2c3d4e5
X-Colonizer-Signature: sha256=<hex>     (only when a signing secret is set)
```

```json
{"version": 1, "id": "evt_3f9c0d1e2a4b5c6d7e8f90a1b2c3d4e5",
 "event": "question", "at": "2026-09-18T00:00:00+00:00",
 "text": "acme/webshop #42 needs an answer",
 "colony": {"id": "…", "repo": "acme/webshop", "org": "acme", "issue": 42, "status": "waiting_for_answer"},
 "pr_url": null,
 "provider": null}
```

Every payload has the same eight keys, whatever the event: a receiver reads one shape. The note
carries no repository content (no issue title, no question text, no branch, no error) and never a
secret.

## Schema and versioning

The payload is described by a JSON Schema,
[webhook-events.schema.json](../webhook-events.schema.json), and a test keeps its `event` list
equal to what the harness can send. `version` is the shape's version, `1` today. It changes only
when a key changes meaning or goes away. A new event name or a new key does not change it, so a
receiver must ignore event names and keys it does not know, as everywhere else in this protocol.

| Key | Meaning |
| --- | --- |
| `version` | The payload shape's version: `1` |
| `id` | The event's stable id (below) |
| `event` | What happened: one of the names in the tables below |
| `at` | When the mothership built this note (RFC 3339) |
| `text` | One short line, at most 200 characters, naming the repository and issue (or the provider) and what happened |
| `colony` | `{id, repo, org, issue, status}` for a colony event, `null` for a host-level one; `status` is the colony's status after the event |
| `pr_url` | The colony's pull request on `pull_request` and `needs_rebase`, else `null` |
| `provider` | The provider behind `provider_degraded`, `judge_degraded` or `provider_quota_exhausted`, else `null` |

## Colony lifecycle events

With the notify module's `on_lifecycle` setting on (off by default; issue
[#897](https://github.com/Colonizer-dev/harness/issues/897)), the webhook receives every colony
lifecycle transition: exactly one event per status change, plus `cleaned`. These go to the webhook
only, never to the desktop or a phone, and they skip the anti-spam rate limiter, so the stream is an
exact record a receiver can rebuild a colony's history from.

| `event` | The colony's status became | Notes |
| --- | --- | --- |
| `queued` | `queued` | Waiting for a free slot |
| `started` | `starting` | Booting its microVM |
| `running` | `running` | Includes a new turn after `idle` |
| `idle` | `idle` | Its turn ended; it waits for a message |
| `question` | `waiting_for_answer` | Also the event a person is told about |
| `answered` | `running` or `idle`, from `waiting_for_answer` | Its question was answered |
| `publishing` | `publishing` | Pushing and opening its pull request |
| `pull_request` | `pr_opened` | Also the event a person is told about; carries `pr_url` |
| `merged` | `merged` | Its pull request was merged |
| `closed` | `closed` | Its pull request was closed without merging |
| `no_changes` | `no_changes` | It finished with nothing to push |
| `parked` | `parked` | Set aside (out of quota, or a hold timed out), worktree kept |
| `resumed` | `queued`, `starting` or `running`, from `parked`, `stopped` or `failed` | Coming back |
| `stopped` | `stopped` | |
| `failed` | `failed` | Also the event a person is told about |
| `cleaned` | (any) | Its worktree was reclaimed; not a status change |

`question`, `pull_request` and `failed` are both lifecycle events and events a person is told
about. With `on_lifecycle` on, the webhook gets each of them once, from the lifecycle stream; the
desktop and phones still get them through the rate limiter. Either way the id is the same. With
`on_lifecycle` off, the webhook gets only the events a person is told about, as before. A colony
seen for the first time (after a restart, say) is recorded without an event, so a restart never
replays a backlog. A transition the 30-second poll did not see — a colony that went from `running`
through `publishing` to `pr_opened` between two polls — is reported as the one change it saw.

## Other events

| `event` | When | Switch |
| --- | --- | --- |
| `attention` | The watchdog flagged a colony (`stalled`, `nudges_exhausted`, a risky hold, a control defeat) | `on_attention` |
| `needs_rebase` | A pull request fell behind its base and no colony is left to rebase it | `on_attention` |
| `provider_degraded` | A model provider's failure rate crossed 10% | `on_provider` |
| `judge_degraded` | The autonomy judge could not reach its model three times running | always |
| `provider_quota_exhausted` | A provider ran out of quota with colonies waiting on it | `on_quota` |
| `account_needs_sign_in`, `account_resolved` | A Claude account's sign-in expired, or works again | always |
| `ci_unavailable` | The merge-train loop found a repository's CI unable to run | always |
| `digest` | The hourly line summing what the rate limiter held | always |

## Event ids

Every payload carries a stable `id`, and the same value travels in the `X-Colonizer-Event-Id`
header so a receiver can dedupe before it parses the body (issue
[#896](https://github.com/Colonizer-dev/harness/issues/896)). An id is `evt_` followed by 32
lowercase hex digits: the first half of a SHA-256 over what the event is about, what happened, and
where in that history it happened.

- A colony event hashes the colony id, the event (with the attention reason, so a stall and an
  out-of-nudges are different events), and the colony's `updated_at` at the moment the edge was
  seen. Rebuilding the payload for the same edge later gives the same id, even though `at` moves.
- A host-level event (a provider, the judge, a Claude account, an out-of-quota card, a loop's line,
  the digest) hashes its topic, the event name and the moment it was detected.

The same event delivered twice carries the same id both times. Two different events never share
one. A receiver that keeps the ids it has processed for a day can drop any repeat safely. The id is
a hash, so it carries nothing of the colony or the repository.

## Verifying the signature

When a signing secret is set (`config/notify-secret`, mode 0600, or `COLONIZER_NOTIFY_SECRET`;
`PUT /api/notify/secret` saves one), the request carries `X-Colonizer-Signature: sha256=<hex>`:
HMAC-SHA256 with the secret over the exact bytes `"{timestamp}.{body}"`, where `timestamp` is the
`X-Colonizer-Timestamp` header. Recompute it from the raw body before parsing, compare in constant
time, and refuse a timestamp more than a few minutes old. The body includes `id`, so the signature
covers it. Without a secret the request is sent unsigned.

## Delivery, retries and the dead letter

A 2xx answer is a delivery. A non-2xx answer or a transport error (including a 15-second timeout)
is logged into the colony's log, or onto stderr for a host-level event, and the delivery goes into
the outbox to be retried (issue [#898](https://github.com/Colonizer-dev/harness/issues/898)):

- **Backoff.** The first retry waits 30 seconds, and each later one twice as long as the one
  before, capped at an hour. Each wait is moved up to 20% either way at random (jitter), so a burst
  of failures does not retry in lockstep against a receiver that is just coming back.
- **Bounded.** A delivery gets at most 6 attempts, the first included, so the last retry comes
  roughly fifteen minutes after the first failure. One that still fails moves to the dead letter.
- **The same event.** Every attempt sends the same body, so the same `id`, re-signed with the
  current secret and a fresh `X-Colonizer-Timestamp`. A receiver that dedupes on the id takes the
  event once however many attempts it took. A 500 followed by a 200 is one delivery.
- **Persistent.** The waiting deliveries and the dead letter live in `data/notify-webhook-outbox.json`
  (mode 0600), so a restart neither forgets a retry nor loses a dead letter. It keeps the payload
  and the address it was sent to, never the signing secret. Retries run whether or not the notify
  module is still on: what waits was announced while it was. At most 1,000 deliveries wait and 500
  dead letters are kept; past either, the oldest goes to the dead letter, or out of it.
- **Replay.** A dead letter stays until the owner replays or discards it, from Settings > Notify
  or the API below. A replay is one attempt now. If it fails again, the letter stays in the dead
  letter with that attempt counted, rather than starting a fresh round of retries.

The anti-spam ledger counts an announcement whose webhook failed its first attempt as undelivered
(when no other channel took it), even if a retry delivers it later.

| Method & path | Purpose |
| --- | --- |
| `GET /api/notify/deliveries` | `{pending, dead_letters, last_success_at, max_attempts}`. Each delivery is `{key, event_id, event, target, url, colony, attempts, first_at, last_at, next_at, last_error}`: never the body, and `url` without its query string, where a webhook address sometimes keeps a token. `next_at` is `null` in the dead letter, which lists the newest first |
| `POST /api/notify/dead-letters/{key}/replay` | One attempt now: `{delivered, error}`, **200** either way. **404** for an unknown key |
| `POST /api/notify/dead-letters/replay` | Replays every dead letter, oldest first, one attempt each: `{delivered, failed}` |
| `DELETE /api/notify/dead-letters/{key}` | Discards one dead letter. **404** for an unknown key |

All four are owner-only: a scoped API token gets a 403.
