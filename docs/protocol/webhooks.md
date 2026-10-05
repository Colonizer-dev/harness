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
{"id": "evt_3f9c0d1e2a4b5c6d7e8f90a1b2c3d4e5",
 "event": "question", "at": "2026-09-18T00:00:00+00:00",
 "text": "acme/webshop #42 needs an answer",
 "colony": {"id": "…", "repo": "acme/webshop", "org": "acme", "issue": 42, "status": "waiting_for_answer"},
 "pr_url": null,
 "provider": null}
```

Every payload has the same seven keys, whatever the event: a receiver reads one shape. The note
carries no repository content (no issue title, no question text, no branch, no error) and never a
secret.

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

## Delivery

A non-2xx answer or a transport error is logged into the colony's log (or onto stderr for a
host-level event). It is not retried.
