# Observability

Send the harness's logs, traces and metrics to a backend you run or subscribe to (Grafana Cloud, Datadog,
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
- [Traces](observability/traces.md): one trace per colony, its span names, attributes and ids.
- [Architecture](design/observability.md): the design decisions behind it (issue
  [#839](https://github.com/Colonizer-dev/harness/issues/839)).

## How it runs

The exporter is a separate binary, `colonizer-observability`, so the mothership itself links none of
OpenTelemetry, protobuf or gzip, and a fault in the exporter cannot touch a colony. When export is
on, the mothership writes `<data>/observability/exporter.json` (the effective settings and the
colony list, never a header), starts the add-on, and hands it the headers as one line on its stdin.
The add-on tails the ledgers the mothership already writes, maps them, sends them, and commits its
read position to `<data>/observability/state.json` only after the backend acknowledged the batch.

Nothing in a colony's path waits on it: the ledgers are the spool. Lines older than **Max backlog
(days)** (7 by default) are dropped instead of sent late, oldest first, and counted. A restart
resumes from the committed offsets, and every record carries a deterministic `colonizer.record.id`,
so a replay can be deduplicated.

**Start from** settles where a new endpoint starts: `now` (the default) sends only what is written
from the moment it is configured; `backlog` also sends what the ledgers already hold, back to the
backlog limit. It is settled once per endpoint, so a restart never skips anything, and a colony
that appears later is always read from its first line. Each tick shares the read budget (**Max read
rate**) equally among the files with something new, so one busy colony cannot hold back the rest.
A colony that is finished (merged, no changes, stopped or failed) and fully read is not looked at
again until its status or its directory changes. [The tailer](observability/tailer.md) has the
details.

### When the backend says no

The exporter holds at most one tick's batch in memory and reads nothing new while that batch waits.
Its read offsets are committed, in one atomic write with the metric series, only after the backend
acknowledged the whole batch; until then a restart or crash replays it from the ledgers.

| Answer | What happens | Records dropped |
| :--- | :--- | :--- |
| 2xx | Committed. A `partial_success` with rejected records counts them as `rejected` and keeps the backend's message in `last_partial_success`; they are not retried (an ack is an ack). | Only those the backend says it rejected |
| 401, 403, 407 | **The credential is wrong or expired.** The batch and its offsets are kept, exporting pauses, the state is `auth_failed` with the endpoint and the error (never the header value), and the same batch is retried with backoff. Fix the **Observability headers** secret: the add-on restarts and delivers everything from where it stopped. | None |
| 429, 503 | Retried after the server's `Retry-After` (seconds or an HTTP-date, at most 5 minutes), or the backoff when there is none. | None |
| Network error, timeout, 408, 502, 504, any other 5xx or 4xx (404, 405, 415, …) | Retried with backoff: 1 s doubling to 60 s, with jitter. A status that says nothing about the records never drops them. | None |
| 400, 413, 422 | The request's records are at fault: it is split in half and each half retried, down to the single bad record, which is dropped and counted as `refused`. The rest are delivered. | The bad record only |

### Health

The add-on writes `<data>/observability/status.json` at most once a second, and
`GET /api/observability/status` returns it as `exporter`. `state` is one of:

| State | Meaning |
| :--- | :--- |
| `starting` | No request answered yet. |
| `running` | The last batch was acknowledged. |
| `retrying` | The backend is unreachable or busy; the batch is held until `next_retry_unix`. |
| `auth_failed` | The backend refused the credential; nothing is dropped, the batch is held and retried. |
| `stopped` | A clean shutdown. |

It also carries `endpoint`, `last_error` (redacted, header values cut out), `last_success_unix`,
`consecutive_failures`, `exported`, `bytes_sent` (since the add-on started), `backlog_bytes` (ledger
bytes behind the committed offsets), `dropped` by reason, `export_failures`,
`last_partial_success`, and `signals`: per signal (`logs`, `traces`, `metrics`) its `state` (`ok`,
`backing_off`, `auth_failed`, `off`), `endpoint`, `consecutive_failures`, `last_error`,
`last_success_unix`, `records_sent` and `bytes_sent`.

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
| `OTEL_EXPORTER_OTLP_ENDPOINT` | The base URL; `/v1/logs`, `/v1/traces` and `/v1/metrics` are appended. |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | A full URL for one signal, used as is. |
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

Each record has `colonizer.record.id`, `colonizer.source`, `colonizer.stream` (`operational`,
`activity` or `meta`), and, when it belongs to one, `colonizer.colony.id`, `colonizer.org` and `colonizer.repo`.
The **Operational logs** and **Colony activity** switches gate their streams; a stream that is off is
never read.

| Source | Stream | Event name | Body | Attributes |
| :--- | :--- | :--- | :--- | :--- |
| `sessions/<id>/harness.jsonl` | operational | `colonizer.harness_log` | the log line (redacted) | `origin`, `level`; severity from `level` |
| `sessions/<id>/gateway.jsonl` | operational | `colonizer.gateway_request` | `gateway_request` | `provider`, `wire`, `model`, `wire_model`, `status`, `failure`, `fallback`, `queue_ms`, `duration_ms`, `request_bytes`, `response_bytes`, `input_tokens`, `output_tokens` |
| `sessions/<id>/events.jsonl` | activity | `colonizer.agent_event` | the event type | `type`, `state`, `tool_call_id`, `name` (tool), `is_error`, `denial.class`, `question_id`, `risk`, `kind`, `blocking`, `model`, `cost_usd`, `duration_ms`, `access`, `policy`, `agent_ref.id`, `agent_ref.name`, `model_usage.<model>.<input/output/cache_read/cache_write>_tokens` |
| `activity.jsonl` | activity | `colonizer.activity` | the kind | `kind`, `actor`, `via`, `colony`, `repo`, `target`, `summary` (reviewed kinds only, below) |
| `spend.jsonl` | activity | `colonizer.spend` | `spend` | `kind`, `org`, `session`, `agent`, `model`, token counts, `cost_usd`, `scoring_ms` |
| `sessions/<id>/findings.jsonl` | activity | `colonizer.finding` | the finding's `state` | `state`, `issue`, `duplicate_of`, `severity`, `verdict`, `fix_session`, `review_session`, `pr` (the title and reason are content) |
| `decisions.jsonl` | activity | `colonizer.decision` | the point | `kind`, `point`, `mode`, `options` (a count), `pick`, `confidence`, `latency_ms`, `miss`, `did`, `outcome.progressed`, `outcome.window_min` |
| `routing.jsonl` | activity | `colonizer.routing` | `decision` or `actual` | `kind`, `actual_cost_usd`, `decision.point`, `decision.jev_mode`, `decision.jev_agrees`, `decision.floor`, `decision.tier`, `decision.rule`, `decision.source`, `decision.score`, `decision.model`, `decision.agent`, `decision.misroute`, `decision.sensitivity` |
| `jev_ladder.jsonl` | activity | `colonizer.jev_ladder` | `decision` or `reread` | `kind`, `tool`, `tool_call_id`, `action`, `keep_call`, `keep_result`, `matched_tool_call_id` |
| `jev_focus.jsonl` | activity | `colonizer.jev_focus` | `focus` | `kind`, `session`, `mode`, `candidates` (a count), `chosen`, `would_catch`, `verdict`, `actual_first_failure_ms`, `focused_first_failure_ms`, `total_ms`, `checks_run` |
| `logs/mothership.jsonl` (+ `.1`) | operational | `colonizer.mothership_log` | the log line (redacted) | `level`, `target`; severity from `level` |
| `export_gap` (the exporter's own) | meta | `colonizer.export_gap` | the reason | `reason`, `file`, `bytes`, `lines`, `archived`; severity `WARN` |

Streaming text deltas are skipped. The decision and Jev ledgers name their colony in the row's
`session`, which becomes `colonizer.colony.id` (with its org and repo). An activity line's `detail`
is content for every kind but four whose text the harness writes itself: `decision.shadow`,
`decision.act` and `decision.fallback` (the point and its pick) and `outcome.suspended`, which send
it as `summary`. The `meta` stream is on whenever either log stream is.

An `export_gap` record marks a hole in what the exporter could read: `reason` (`backlog`,
`rotated_past`, `truncated`, `deleted`, `oversized_line`), `file`, `colonizer.colony.id`, and, where
known, `bytes` and `lines` skipped, and for a deleted colony whether `archived` holds a bundle of
it.

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
| `colonizer.observability.dropped` | counter | `{record}` | `colonizer.drop.reason`: `backlog`, `refused`, `rejected`, `oversized`, `malformed`, `series_limit`, `state_reset`, `gap_*` (`gap_deleted` once per deleted colony or file) |
| `colonizer.observability.export_failures` | counter | `{request}` | — |

### Traces

One trace per colony while **Traces** is on: the root `invoke_agent <repo>` span from launch to
outcome, a `turn <n>` span per turn, an `execute_tool <tool>` span per tool call, and a
`subagent <type>` span under the Task call that started it, a `question` span from a question
to its answer, a `chat <model>` span per gateway request and a `host_step <step>` span per
host-chain verdict. **Max trace size** keeps each trace under Tempo's limit. Ids are derived from the install's
host id, the colony id and the turn number or tool call id, so a replay sends the same spans. A
span goes out when it ends; the root goes out once, at the colony's outcome. Structure only, never
content. [Traces](observability/traces.md) has the span names, attributes, ids and sampling.

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
`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` to them, and put
`dd-api-key=<API key>` in the headers.

## Turning it off

Switch the module off in the cockpit, or set `OTEL_SDK_DISABLED=true`. The mothership closes the
add-on's stdin; it commits its offsets and exits. Nothing more is sent.
