# Observability

Send the harness's logs and metrics to a backend you run or subscribe to (Grafana Cloud, Datadog,
Honeycomb, a local OpenTelemetry Collector, or any other OTLP receiver), over OpenTelemetry's OTLP/HTTP
protocol. It is **off until you configure it**: with no saved and enabled `observability` module, and
no `COLONIZER_OBSERVABILITY=on`, nothing is written and nothing leaves the machine. The header a
backend wants is kept in a secret, never in a setting or a log line.

This is separate from the [live map](telemetry.md) and [usage data](usage-data.md), which report to
colonizer.dev. Observability goes only to the endpoint you name.

- [Settings and privacy](observability/settings.md): every setting of the module.
- [Export policy and OTLP encoding](observability/export-policy.md): how every exported string is
  allowlisted, gated, redacted again, capped and hashed, and how requests are batched and encoded.
- [The tailer](observability/tailer.md): how the exporter reads the ledgers and survives rotation.
- [Architecture](design/observability.md): the design decisions behind it (issue
  [#839](https://github.com/Colonizer-dev/harness/issues/839)).

## How it runs

The exporter is a separate binary, `colonizer-observability`, so the mothership itself links none of
OpenTelemetry, protobuf or gzip, and a fault in the exporter cannot touch a colony. When export is
on, the mothership writes `<data>/observability/exporter.json` (the effective settings and the
colony list, never a header), starts the add-on, and hands it the headers as one line on its stdin.
The add-on tails the ledgers the mothership already writes, maps them, sends them, and commits its
read position to `<data>/observability/state.json` only after the backend acknowledged the batch.

Nothing in a colony's path waits on it: the ledgers are the spool. If the backend is down, the
exporter retries with backoff (1 s doubling to 60 s, with jitter) and reads further later; it holds
at most one tick's batch in memory. A request the backend refuses outright (400, 413) is split in
half until the bad record is isolated, which is dropped and counted. Lines older than **Max backlog
(days)** (7 by default) are dropped instead of sent late, oldest first, and counted. A restart
resumes from the committed offsets, and every record carries a deterministic `colonizer.record.id`,
so a replay can be deduplicated.

The add-on is found, in order, at `COLONIZER_OBSERVABILITY_BIN`,
`<data>/addons/observability/<version>/colonizer-observability`, or beside the `colonizer` binary.
Its version must match the mothership's. Release tarballs do not ship it yet; build it with:

```sh
cargo build --release -p colonizer-observability
cp target/release/colonizer-observability "$(dirname "$(command -v colonizer)")/"
```

`GET /api/observability/status` says whether export is on, why not, where each setting came from,
the header names (never values) and the exporter's counts. `POST /api/observability/test` sends one
log record and one metric point and reports what the backend answered. Both are in the cockpit at
**Settings → Modules → Observability**, with a headers field and a **Send test** button.

## Configuration

Configure it in the cockpit (the module) or with the standard OpenTelemetry variables. A variable
overrides its one field of the saved module; it never turns export on by itself, so an
`OTEL_EXPORTER_OTLP_ENDPOINT` meant for another program in the same environment sends nothing.

| Variable | Effect |
| :--- | :--- |
| `COLONIZER_OBSERVABILITY=on` | Turns export on without a saved module (every other field at its default). The only switch besides the module. |
| `OTEL_SDK_DISABLED=true` | Turns export off, whatever else says. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The base URL; `/v1/logs` and `/v1/metrics` are appended. |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | A full URL for one signal, used as is. |
| `OTEL_EXPORTER_OTLP_HEADERS` | `k=v,k2=v2`, values percent-encoded. Overrides the **Observability headers** secret. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` (default) or `http/json`. gRPC is not built. |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | `gzip` (default) or `none`. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | Milliseconds. |
| `OTEL_SERVICE_NAME` | `service.name`, default `colonizer`. |
| `OTEL_RESOURCE_ATTRIBUTES` | Extra resource attributes, `k=v,k2=v2`. |

The endpoint rules of a save apply to the effective value too: `http://` only to a loopback or
private address unless **Allow plain http to a public host** is on, and no credentials or query
string in the URL.

The headers are the one secret: save them under **Settings → Secrets → Observability headers** (or
in the observability pane) as a `k=v,k2=v2` list. The value goes to the add-on on its stdin, never
into `exporter.json`, its arguments or its environment, and the add-on cuts it out of any error text
before it reaches `status.json` or a log line.

## What is exported

Every record goes through the [export policy](observability/export-policy.md): only allowlisted
attributes, every string redacted again with the same detectors the harness uses on disk, capped,
and repository names hashed under `repo_names = hashed`. Conversation content (prompts, completions,
tool input and output, error text, file paths, titles) is never exported by this build, whatever the
content switches say.

**Resource:** `service.name`, `service.version` (the mothership's version), `service.instance.id`
(the install's host id), `colonizer.fleet.id`, plus `OTEL_RESOURCE_ATTRIBUTES`. Scope: `colonizer`.

### Logs

Each record has `colonizer.record.id`, `colonizer.source`, `colonizer.stream` (`operational` or
`activity`), and, when it belongs to one, `colonizer.colony.id`, `colonizer.org` and `colonizer.repo`.
The **Operational logs** and **Colony activity** switches gate their streams; a stream that is off is
never read.

| Source | Stream | Event name | Body | Attributes |
| :--- | :--- | :--- | :--- | :--- |
| `sessions/<id>/harness.jsonl` | operational | `colonizer.harness_log` | the log line (redacted) | `origin`, `level`; severity from `level` |
| `sessions/<id>/gateway.jsonl` | operational | `colonizer.gateway_request` | `gateway_request` | `provider`, `wire`, `model`, `wire_model`, `status`, `failure`, `fallback`, `queue_ms`, `duration_ms`, `request_bytes`, `response_bytes`, `input_tokens`, `output_tokens` |
| `sessions/<id>/events.jsonl` | activity | `colonizer.agent_event` | the event type | `type`, `state`, `tool_call_id`, `name` (tool), `is_error`, `denial.class`, `question_id`, `risk`, `kind`, `blocking`, `model`, `cost_usd`, `duration_ms`, `access`, `policy`, `agent_ref.id`, `agent_ref.name`, `model_usage.<model>.<input/output/cache_read/cache_write>_tokens` |
| `activity.jsonl` | activity | `colonizer.activity` | the kind | `kind`, `actor`, `colony`, `repo`, `target` |
| `spend.jsonl` | activity | `colonizer.spend` | `spend` | `kind`, `org`, `session`, `agent`, `model`, token counts, `cost_usd`, `scoring_ms` |

Streaming text deltas are skipped. Findings, decisions, routing and the Jev ledgers are not mapped
yet ([#845](https://github.com/Colonizer-dev/harness/issues/845)).

### Metrics

Cumulative, pushed every 30 seconds while **Metrics** is on. The sums survive a restart: they are
committed with the read offsets that produced them.

| Metric | Type | Unit | Attributes |
| :--- | :--- | :--- | :--- |
| `colonizer.colonies` | gauge | `{colony}` | `colonizer.colony.status` |
| `colonizer.queue.depth` | gauge | `{colony}` | — |
| `colonizer.gateway.requests` | counter | `{request}` | `provider`, `model` |
| `colonizer.gateway.failures` | counter | `{request}` | `provider`, `model`, `failure` |
| `colonizer.gateway.duration` | histogram | `ms` | `provider`, `model` |
| `colonizer.gateway.queue_time` | histogram | `ms` | `provider`, `model` |
| `colonizer.gateway.input_tokens`, `colonizer.gateway.output_tokens` | counter | `{token}` | `provider`, `model` |
| `colonizer.spend.cost` | counter | `USD` | `org`, `model`, `kind` |
| `colonizer.spend.input_tokens`, `colonizer.spend.output_tokens` | counter | `{token}` | `org`, `model`, `kind` |
| `colonizer.observability.exported` | counter | `{record}` | — |
| `colonizer.observability.dropped` | counter | `{record}` | `colonizer.drop.reason`: `backlog`, `refused`, `rejected`, `oversized`, `malformed`, `series_limit`, `state_reset`, `gap_*` |
| `colonizer.observability.export_failures` | counter | `{request}` | — |

### Traces

Not exported yet. One trace per colony (launch to outcome, with child spans per turn, tool call and
gateway request) is designed in [the architecture](design/observability.md#spans-reconstructing-a-trace-from-files)
and tracked in [#846](https://github.com/Colonizer-dev/harness/issues/846) and
[#847](https://github.com/Colonizer-dev/harness/issues/847).

## Examples

These follow each vendor's public OTLP documentation; check it for your region or site.

**A local OpenTelemetry Collector**, which can then fan out to anything it supports:

```sh
COLONIZER_OBSERVABILITY=on
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

**Grafana Cloud.** On your stack's *OpenTelemetry* connection page, copy the OTLP endpoint (of the
form `https://otlp-gateway-<zone>.grafana.net/otlp`), the instance ID and an access token. The header
is HTTP Basic auth of `<instance id>:<token>`, base64-encoded, with the space percent-encoded:

```text
endpoint:  https://otlp-gateway-<zone>.grafana.net/otlp
headers:   Authorization=Basic%20<base64 of instance-id:token>
```

**Honeycomb.** The endpoint is `https://api.honeycomb.io` (US) or `https://api.eu1.honeycomb.io` (EU),
and the header carries an ingest API key. Honeycomb files metrics under a dataset you name:

```text
endpoint:  https://api.honeycomb.io
headers:   x-honeycomb-team=<ingest key>,x-honeycomb-dataset=colonizer-metrics
```

**Datadog.** The simplest path is the Datadog Agent's OTLP receiver, enabled as Datadog's *OTLP
Ingestion by the Datadog Agent* documentation describes, on the same host:

```sh
COLONIZER_OBSERVABILITY=on
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

To send without an Agent, take the per-signal intake URLs Datadog documents for your site, set
`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` to them, and put
`dd-api-key=<API key>` in the headers.

## Turning it off

Switch the module off in the cockpit, or set `OTEL_SDK_DISABLED=true`. The mothership closes the
add-on's stdin; it commits its offsets and exits. Nothing more is sent.
