# Metrics push (OTLP)

The exporter does not only send logs and traces: while **Metrics** is on, it also pushes a metric
catalogue to the same endpoint, over the same OTLP/HTTP requests and the same header secret. This
page is that catalogue — what is in it, how often it leaves, what a failed push costs, and how each
of its names reads on the [pull endpoint](metrics.md) (`GET /metrics`) when both are on.

It is one half of the metrics story and never the whole of it: the pull endpoint serves the
mothership's own live state with no exporter involved, and the two catalogues overlap only where the
table below says they do.

## What is pushed

Cumulative series, folded from the ledgers the exporter already tails, plus gauges read at push
time. The catalogue is the one table on the [observability page](../observability.md#metrics): the
gateway and spend series, the colony gauges from the contract, and the exporter's own counters.

| Metric | Type | Unit | Attributes |
| :--- | :--- | :--- | :--- |
| `colonizer.observability.bytes` | counter | `{By}` | `colonizer.signal`: `logs`, `traces`, `metrics` |
| `colonizer.observability.last_success` | gauge | `s` | `colonizer.signal` |
| `colonizer.observability.backlog` | gauge | `{By}` | `colonizer.signal` |

The three families above are the push's own health, one series per signal:

- **`bytes`** counts the wire bytes (after gzip) a backend has acknowledged, per signal, since the
  sums started.
- **`last_success`** is when each signal last had a request acknowledged, unix seconds — a gauge, so
  it moves with time rather than up.
- **`backlog`** is the ledger bytes behind the committed cursors, the same number the status page
  reports per signal. A live reading taken at each push, not a cumulative sum, and the one number
  here that can go down because work was done rather than a state reset.

## How often

**Metrics push interval (seconds)** (see [Settings and privacy](settings.md)), `30` by default, 5 to
86400. It is a setting of the module, shared by both providers, and no `OTEL_*` variable sets it:
the OpenTelemetry environment configures the endpoint, never the cadence.

The interval is a floor, not a deadline: the exporter pushes on its own tick, after the tick's logs
and traces have been flushed, so a busy ledger delays a push by as long as the flush took and no
longer.

## When a push fails

A push is cumulative, so a failure is cheap by construction:

- **Nothing is buffered and nothing is retried.** The next interval's push carries everything the
  failed one would have, because the sums only ever grow. The failure is counted once, in
  `colonizer.observability.export_failures`, and superseded.
- **Logs and traces never wait on it.** The push runs after the tick's flush, on its own timer; a
  metrics endpoint that answers `503` holds up exactly nothing.
- **The sums survive a restart.** They are committed in the same atomic write as the read offsets
  that produced them, so a restart neither loses a line nor counts one twice.

Every series of one push starts at the same instant, and the next push starts where the first did:
the stream is cumulative with a stable start time, so a backend never sees a reset unless there was
one. A genuine reset — a state file that had to be rebuilt — is counted as `state_reset` in
`colonizer.observability.dropped`, and the series start over from there.

## The same quantity, two names

The exporter pushes OTLP metric names (dots, units in braces); the pull endpoint serves Prometheus
names (underscores, `_total` suffixes). Where both catalogue the same quantity, this is the pairing:

| OTLP push name | Prometheus pull name | Notes |
| :--- | :--- | :--- |
| `colonizer.colonies` | `colonizer_colonies` | The push splits by `colonizer.colony.status`; the pull adds `org` and `agent`. |
| `colonizer.queue.depth` | `colonizer_queue_depth` | |
| `colonizer.gateway.requests` | `colonizer_gateway_requests_total` | The push splits by `provider`, `model`; the pull by `provider`, `outcome` (`ok`/`error`). |
| `colonizer.gateway.duration` | `colonizer_gateway_request_duration_seconds` | Both histogram latency; the push buckets in milliseconds, the pull in seconds. |
| `colonizer.spend.cost` | `colonizer_cost_usd_total` | The push adds a `kind` attribute. |
| `colonizer.observability.exported` | `colonizer_export_records_total` | The pull side is the `result="sent"` series, split by `signal`. |
| `colonizer.observability.dropped` | `colonizer_export_records_total` | The pull side is one `signal="all", result="dropped"` series, the non-`gap_*` reasons summed. |
| `colonizer.observability.dropped` (`gap_*`) | `colonizer_export_gaps_total` | Each `gap_*` reason keeps its own series, labelled by the full reason. |
| `colonizer.observability.bytes` | `colonizer_export_bytes_total` | |
| `colonizer.observability.last_success` | `colonizer_export_last_success_timestamp_seconds` | |
| `colonizer.observability.backlog` | `colonizer_export_backlog_bytes` | |

Tokens are the one deliberate mismatch: the push counts them per `provider`, `model`
(`colonizer.gateway.input_tokens`, `.output_tokens`) and per `org`, `model`, `kind`
(`colonizer.spend.input_tokens`, `.output_tokens`) from the ledgers, while the pull serves
`colonizer_tokens_total` from the live colonies' spend. Two views of the same spend, never to be
summed across — see [the pull page's note on the two token shapes](metrics.md#the-two-token-shapes).

Only pushed: `colonizer.gateway.failures`, `colonizer.gateway.queue_time` and
`colonizer.observability.export_failures` have no pull counterpart — a push failure says nothing
about the mothership, which is the machine doing the pulling. Only pulled: the mothership's own
state (`colonizer_colonies_started_total`, `colonizer_colonies_finished_total`,
`colonizer_gateway_fallbacks_total`, `colonizer_gateway_in_flight`, `colonizer_gateway_queued`,
`colonizer_questions_open`, `colonizer_attention`, `colonizer_storage_alert`,
`colonizer_disk_free_bytes`), which the exporter cannot see.

The pull side reads the exporter's `<data>/observability/status.json` — the add-on's own health
file — so the five `colonizer_export_*` families need no push to be on: they appear on a scrape as
long as an exporter is running, and are absent, without breaking anything, when none is. That file
is also the one lifetime difference in the per-signal `bytes`, `records` and `last_success`
pairings above: the pull series restart with the exporter process, while the push side's sums
persist in `state.json` until a state reset.
