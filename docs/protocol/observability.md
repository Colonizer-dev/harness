# Observability: `GET /metrics`

The Prometheus surface of the [`observability`](../observability.md) module kind. The catalogue, the
scrape configuration and the privacy rules are in [Metrics (`GET /metrics`)"](../observability/metrics.md);
this page is the wire contract.

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
