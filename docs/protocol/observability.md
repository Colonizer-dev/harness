# Observability: `GET /metrics`

The Prometheus surface of the [`observability`](../observability.md) module kind. The catalogue, the
scrape configuration and the privacy rules are in [Metrics (`GET /metrics`)"](../observability/metrics.md);
this page is the wire contract — the scrape route below, the module's `/api` surface (the backfill
routes, the status route and the reset route) at the end.

## The route

```
GET /metrics
```

Responds `200` with `Content-Type: text/plain; version=0.0.4; charset=utf-8` — the Prometheus text
exposition format 0.0.4, hand-rendered. There is no query parameter, no body and no negotiation:
a scrape either gets the whole catalogue or an error.

The route is not under `/api`, and it is not part of the UHP surface. It is guarded by the same
`host_guard` every other route is (see [Accounts, tokens and install settings](auth.md)), plus the
two rules below.

## When it exists

| State | Answer |
| --- | --- |
| No `observability` module configured | `404` |
| The module is present but disabled | `404` |
| The module is enabled and `prometheus` is off (the default) | `404` |
| The module is enabled and `prometheus` is on | `200` |

An endpoint nobody configured does not exist to be found. The 404 body is the API's usual error
object (`client_error`), so a client that parses JSON gets a message rather than the cockpit's
sign-in page.

## Who may read it

The same contract as the module's own rule, in one table:

| Caller | Answer | Why |
| --- | --- | --- |
| Owner (install API token, or the cockpit cookie) | `200` | It is the install's own state. |
| `read`, `operate` or `launch` token, no `orgs` and no `repos` | `200` | Unbounded by construction, so nothing is filtered. |
| Any token with a non-empty `orgs` or `repos` | `403` | The catalogue is install-wide; a filtered view would hide exactly the series that are out of reach. |
| `fleet` token | `403` | It watches the host list across the fleet, not the owner's colonies or spend. |
| Paired phone | `403` | It authenticates as the owner for the cockpit, but a device is not the install's operator. |
| No token, or one that does not authenticate | `401` | Plain text, not the sign-in page: the caller is a scraper. |
| `repo_names = hashed`, and no hash key is readable | `503` | Reporting every org as `unknown` would be a number that says nothing. The key is retried per scrape. |

The credential is a scoped read token ([`POST /api/tokens`](auth.md)) or the install's own API
token. It is never the `observability-headers` secret — an OTLP collector and a Prometheus scraper
hold different credentials on purpose. The `503` is plain text too, like the `401`.

## The body

Every line is one of the three legal forms of the format:

```
# HELP colonizer_questions_open Colonies waiting for an answer.
# TYPE colonizer_questions_open gauge
colonizer_questions_open 1
```

Metric names match `[a-zA-Z_:][a-zA-Z0-9_:]*`, label names match the same, and every value is
printed with `\\`, `"` and newline escaped. `# HELP` and `# TYPE` precede every sample of the
metric they declare, and appear on every scrape even for a family with no series yet.

There is an `# EOF` convention for streaming exposition; this endpoint is not streamed and does not
emit it — a fixed-length body is complete when the connection closes.

A scrape of the same unchanged state is byte-identical to the last one: series are emitted in
first-seen order, not in hash-map order, so a diff of two scrapes shows what changed in the install
rather than what an iteration decided.

## Stability

The metric names, label names and label values in the catalogue are the public contract and follow
the repository's compatibility rules. Bucket bounds, the `other` fold and the set of counted
outcomes are the current implementation; a change to any of them is a changelog entry. See
[Metrics (`GET /metrics`)"](../observability/metrics.md) for the full table and the reasoning.

## The backfill and export routes

The module's `/api` surface, beside the scrape route above: the history backfill (issue #858) and
the offline export (issue #859), served by the mothership and done by the add-on it supervises
([Backfill](../observability/backfill.md),
[Archived colonies and offline export](../observability/archive-export.md)). All three are
owner-only — the install's own API token.

| Route | Does |
| :--- | :--- |
| `POST /api/observability/backfill` | `{since?, until?, repo?, rate?, dry_run?, include_archive?}` → with `dry_run`, the counts summary (`{per_signal, per_org, records_sent, bytes_sent, …}`); otherwise `{started: true, progress: "GET /api/observability/status"}` and the replay runs beside the exporter. 409 when export is off or a backfill is already running; 503 when the add-on is missing or its version does not match. |
| `DELETE /api/observability/backfill` | Cancels the running backfill: the child finishes its tick, writes `state: "cancelled"` and exits; → `{cancelled: true}`. 409 when none is running. |
| `POST /api/observability/export` | `{out, since?, until?, signals?, include_archive?, content?}` — an offline export, run to completion inside the request: → the add-on's summary `{files, requests, logs, spans, colonies, archived}`. `out` must be an absolute path (the add-on writes beside the mothership) and `signals` only `logs` and `traces`, comma-separated; anything else is a 400 before the add-on is sought. 409 when export is off; 503 when the add-on is missing or mismatched; 502 with the add-on's message when the export fails. |

`GET /api/observability/status` carries the backfill's progress as `backfill` while one runs:
`{state: "running"|"complete"|"cancelled"|"failed", window, records_sent, bytes_sent, cursors,
started_unix, updated_unix, …}`, read from the add-on's
`<data>/observability/backfill.json` (`error` beside a `failed` state).

## `GET /api/observability/status`

The module's own page: whether export is on, why not, where each setting came from, the header
*names* (never values), the add-on's own health and any backfill's progress. It answers
**who may read it exactly as the scrape route does** — the owner and an unbounded `read`-or-better
token (`200`); any token with a non-empty `orgs` or `repos`, a `fleet` token and a paired phone
(`403`, the same words the table above gives for `/metrics`): the page is as install-wide as the
catalogue is. Unlike the scrape route it is always `200` when the module is off — the page that
says why nothing runs is the one an operator configuring it needs.

`GET /api/status`, the cockpit's poll, carries the one word `observability`: `off`, `ok` or
`degraded` ([the harness API](harness-api.md)).

## `POST /api/observability/reset`

Forgets where the export got to, so it starts over (issue #850; [Resetting](../observability/reset.md)).
**Owner-only** — it is nowhere in a token's scope, unlike the status route above.

`{dry_run?, hash_key?}` → with `dry_run`, `{dry_run: true, delete: [{path, bytes}, …]}` and nothing
deleted; otherwise `{reset: true, deleted: [{path, bytes}, …]}`. It deletes
`<data>/observability/state.json` — and with `hash_key` also `<data>/observability/hash.key` —
nothing else: not the ledgers, not the file sink's output, not `status.json`. `409`, deleting
nothing, in three cases: a backfill is registered, because a backfill reads `state.json` to know
where history ends; another reset already holds this data directory's pending entry; or the
exporter is still stopping — the child had not stopped within the settle window, and a live child
would write its cursors back over the deletion.
