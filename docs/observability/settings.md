# Observability: settings and privacy

The `observability` module sends the harness's logs, traces and metrics to a backend you choose,
over OpenTelemetry's OTLP protocol, or writes them to capped files under the data directory. It is
**off until you configure it**. The standard `OTEL_*` variables override its fields, but never turn
it on by themselves; see [Observability](../observability.md#configuration).

## Turning it on

1. In **Settings → Modules → Observability**, pick a provider:
   - **OTLP endpoint** sends to Grafana Cloud, a local collector, Datadog, Honeycomb, Elastic,
     SigNoz, New Relic or any OpenTelemetry backend.
   - **Local file** writes the same events under the data directory and sends nothing anywhere.
2. Fill in the endpoint (or the directory) and the shared settings below.
3. If the backend wants a header, save it under **Settings → Secrets** as **Observability headers**,
   a standard `k=v,k2=v2` list — the same form `OTEL_EXPORTER_OTLP_HEADERS` takes. The value lives
   only in that secret; it is never written into the module settings, a log line or the URL.

The module appears in `GET /api/modules` only after the first save, and a save is `PUT
/api/modules/observability`.

## Settings

Both providers share the second table.

| OTLP endpoint | Default | What it is |
| :--- | :--- | :--- |
| Endpoint URL | *(empty)* | `http://` or `https://`. Required while the module is on (an enabled OTLP module with no endpoint is refused; leave it empty only while the module is off). It must carry no credentials (`user:pass@`) and no query string — those go in the **Observability headers** secret. |
| Protocol | `http/protobuf` | `http/protobuf`, `http/json`, or `grpc`. `grpc` is refused in this build; a later issue adds it. |
| Backend | `custom` | Names the header the backend expects: Honeycomb `x-honeycomb-team`, Datadog `dd-api-key`, New Relic `api-key`, Grafana Cloud `Authorization` (Basic), Elastic `Authorization` (ApiKey); SigNoz and a local collector usually want none. The value is in the secret. |
| Compression | `gzip` | `gzip` or `none`. |
| Request timeout (seconds) | `10` | 1 to 120. |
| Allow plain http to a public host | off | Off, a plain `http://` endpoint is accepted only for loopback and private addresses. On, it is sent anyway, unencrypted. |

| Local file | Default | What it is |
| :--- | :--- | :--- |
| Directory | *(empty)* | Relative to the data directory; empty means `<data>/observability/otlp`. |
| Total size cap (MB) | `512` | Across all the files; the oldest is trimmed first. |

| Both providers | Default | What it is |
| :--- | :--- | :--- |
| Operational logs | on | Boots, parks, quota pauses, watchdog nudges. |
| Colony activity | on | Stages, tool use and status changes. |
| Traces | on | One trace per colony: its turns, tool calls and subagents as spans ([Traces](traces.md)). |
| Metrics | on | Counters and durations. |
| Conversation content | off | The words you and the agent exchanged. Turning it on needs the same save to carry `"confirm_content": true`. |
| Agent thinking | off | The agent's reasoning. Turning it on needs the same save to carry `"confirm_content": true`. |
| Trace sample ratio | `1` | 0 to 1; the share of colonies traced, chosen per colony so a trace is always whole. Logs and metrics are never sampled. |
| Max attribute size (bytes) | `1024` | 128 to 8192. |
| Max content size (bytes) | `32768` | 1024 to 196608. |
| Max trace size (bytes) | `4194304` | One colony trace's budget, under Tempo's 5 MB per trace: past 90 % of it, detail spans are counted instead of sent ([Traces](traces.md#long-colonies-and-tempo-limits)). |
| Repository names | `plain` | `plain` sends `owner/repo`; `hashed` sends a salted hash instead. |
| Expose Prometheus metrics | off | Serve the metrics for scraping on the harness's own endpoint, too. |
| Max backlog (days) | `7` | Older events are dropped rather than exported late; `0` keeps everything. |
| Max read rate (MiB/s) | `8` | A ceiling so the exporter never crowds a running colony. |
| Start from | `now` | Where a new endpoint starts: `now` sends only what is written from then on; `backlog` also sends what the ledgers hold, back to Max backlog. Settled once per endpoint. |

## What is sent, and what is never sent

- Sent: the harness's own events, and the colony activity, traces and metrics the switches above
  turn on. `repo_names = hashed` replaces `owner/repo` with a salted hash.
- Conversation content and agent thinking are **off by default**, and turning either on requires an
  explicit `"confirm_content": true` in the same save — a deliberate second word, not a stray click.
  The confirmation is a request field and is never stored.
- Never sent: credentials of any kind. A backend's header value lives in the `observability-headers`
  secret and is read only by the exporter; it never enters a colony, a setting, a log line or the
  URL. The endpoint refuses userinfo and query strings at save time for the same reason.
- The **Local file** provider sends nothing at all: everything stays in files under the data
  directory.

## Turning it off

Switch the module off in **Settings → Modules → Observability** (or `PUT /api/modules/observability`
with `"enabled": false`). Nothing is exported while it is off. `OTEL_SDK_DISABLED=true` turns it off too, whatever the module
says. A full reset of the exporter's state is added by a later issue.
