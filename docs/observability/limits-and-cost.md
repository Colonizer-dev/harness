# Limits and cost

Two questions this page answers: which backend limits the exporter's caps keep you under, and what a
colony-hour of export volume costs. The limits are facts about defaults (the exporter's verified in
code, the vendors' from their public documentation); the volumes are **estimates, measured on the
repository's fixtures and rounded** — run the dry run below for your own numbers.

## The caps the exporter stays under

| Backend limit | Default | Exporter setting | Default (range) |
| :--- | :--- | :--- | :--- |
| Tempo `max_bytes_per_trace` | 5 MB per trace | **Max trace size** (`max_trace_bytes`) | 4 MiB (min 1 KiB) |
| Tempo `max_attribute_value_size` | 2 KiB, truncated at ingest | **Max attribute size** (`max_attribute_bytes`) | 1 KiB (128–8192) |
| Loki `max_line_size` | 256 KB per line | **Max content size** (`max_content_bytes`) | 32 KiB (1 KiB–192 KiB) |
| Loki structured metadata | ~64 KB and 128 entries per line | attributes as metadata stay far below both | — |
| Loki `reject_old_samples_max_age` | 168 h (1 week) | **Max backlog** (`max_backlog_days`) | 7 days |
| Typical OTLP receiver request cap | 4 MiB | request split (not a setting) | below 1 MiB, 4 MiB hard max |
| New Relic OTLP payload | 1 MB (10⁶ bytes) | — see the note below | — |

How the trace budget works: every span's encoded size is counted per colony. Past **90 %** of
`max_trace_bytes` the detail spans (`execute_tool`, `subagent`, `question`, `chat`, `host_step`) are
suppressed instead of sent — counted on their turn as `colonizer.spans_suppressed.<kind>` and on the
root — so the last 10 % always holds the turns and the root, which are never dropped. A content
string always travels as a log record's body, never as a span attribute, which is why content can
never blow the trace budget. The example stack's Tempo pairs its `max_bytes_per_trace` with
`max_trace_bytes` on purpose ([backends](backends.md#the-local-example-stack)).

Three edges worth knowing:

- **Old samples.** A [backfill](backfill.md) older than the destination's window is rejected by Loki
  (Tempo keys spans by id, not age); Grafana Cloud applies a similar window to Loki and Mimir. The
  live exporter never sends anything older than **Max backlog (days)**.
- **New Relic's 1 MB** sits just under the exporter's 1 MiB split ceiling (≈ 5 % above 10⁶), so a
  maximal request can be refused with 413 — the exporter bisects it and retries, and nothing is
  dropped. gzip, on by default, keeps most requests far under.
- **Oversized ledger lines.** A line over 4 MiB is never sent: the tailer skips it whole and marks
  the hole with an `oversized_line` export gap. Attributes above 8192 bytes would exceed Tempo's
  truncation default even at the setting's maximum.

## What export volume costs, measured

Method, so the numbers can be reproduced or dismissed: the add-on was built
(`cargo build --locked -p colonizer-observability`) and its `backfill --dry-run` — the same summary
`colonizer observability backfill --dry-run` prints through the mothership — was run over two /tmp
data dirs: a copy of `crates/colonizer/tests/fixtures/data-v0.1.9` plus the install-wide ledgers the
backfill tests seed, and one busy synthetic colony built from the trace fixtures
(`claude-code-events`, its gateway lines, an outcome). The gzipped column is the real wire bytes of
the same run sent into a local HTTP sink. The fixture colonies run 11 and 53 minutes (from their
`created_at` to `updated_at`), together ≈ 1.07 colony-hours.

| What | Protobuf (uncompressed) | On the wire (gzip) |
| :--- | :--- | :--- |
| one structure-tier log record | ≈ 0.3–0.4 KB | ≈ 0.1 KB |
| one trace span | ≈ 0.33 KB | — |
| a small colony's whole trace (11 spans) | ≈ 3.6 KB | ≈ 1.3 KB |
| one metrics push, one-colony install | ≈ 0.8 KB | ≈ 0.4 KB |
| gzip share of a batch | — | logs ≈ 23 %, traces ≈ 37 %, metrics ≈ 46 % |

Per colony-hour, then — arithmetic on the measured per-record sizes, not a promise:

| Shape | Logs | Traces |
| :--- | :--- | :--- |
| the fixture's quiet colonies (≈ 11 records/colony-hour) | ≈ 3 KB/colony-hour | one ≈ 3.6 KB trace per colony, not per hour |
| the busy synthetic colony (a two-minute run of turns, tool calls and gateway requests) | ≈ 315 KB/colony-hour | ≈ 3.6 KB per colony |
| metrics, any install, at the 30 s default | ≈ 2.3 MB/day | — |

A real colony sits between the two log rows: multiply its events per hour by ≈ 0.36 KB (protobuf) or
≈ 0.08 KB (gzipped). The metrics row is the fixed floor — the catalogue is pushed whatever the
colonies are doing, and it grows only with the number of distinct models, providers and orgs, each of
which adds series. At a 300 s interval the floor drops to a tenth.

**Content tier: zero in this build.** The content gate is closed regardless of the
**Conversation content** and **Agent thinking** switches — nothing they would send leaves the
machine. When it opens, a content-bearing record is bounded by **Max content size** (32 KiB default),
about a hundred times a structure record, which is the lever to keep hold of.

## The levers

| Lever | Effect |
| :--- | :--- |
| **Trace sample ratio** (`trace_sample_ratio`) | the share of colonies whose trace is sent, chosen per colony so a trace is always whole. Logs and metrics are never sampled. |
| **Conversation content** / **Agent thinking** | keep off; zero today, the loudest stream once the gate opens. |
| Stream switches (`stream_operational`, `stream_activity`, `stream_traces`, `stream_metrics`) | a stream that is off is never read — the cheapest way to drop a whole signal. |
| **Metrics push interval** (`metrics_interval_secs`) | the metrics floor scales with 1/interval (5 s minimum, 86400 s maximum). |
| **Compression** (`compression`) | gzip (default) puts roughly a quarter of the bytes on the wire; `none` is about four times as much. |
| **Max attribute / content / trace size** | caps that bound the worst case per record and per trace — privacy rules as much as cost. |
| **Max backlog** / **Start from** | how much history a new destination takes, and how much survives an outage. |
| [Backfill](backfill.md) `--dry-run` | counts a window before sending it. |

## Measuring your own install

The dry run is the estimator — the cockpit has no preview; its **Send test** sends one log record and
one metric point, not an estimate. Against the running mothership:

```sh
colonizer observability backfill --since 2026-09-01 --dry-run
```

It prints, per signal and per org, the records, the ledger bytes they come from and the estimated
encoded volume on the wire (OTLP/protobuf, before compression), sends nothing and writes nothing, so
a window can be narrowed as often as needed ([backfill](backfill.md#the-dry-run)). Multiply the
gzip share above into it for a wire estimate, and by the window's hours for a rate.
