# Dashboards

Four Grafana dashboards ship under [`examples/observability`](../../examples/observability/README.md)
and load through provisioning into a "Colonizer" folder (issue #863). Each is one question an
operator asks — what are my motherships doing, what did this colony do, what does the gateway cost,
how do agents behave — and every panel on it is an answer cut from that question. The datasource
uids are the contract: `colonizer-prom`, `colonizer-loki` and `colonizer-tempo`, the names the
[datasource provisioning](../../examples/observability/README.md) pins, so a dashboard works
unchanged against the local stack or a remote one. The stack that feeds them — the compose file,
the OpenTelemetry Collector, the fixture replay — is a separate piece (issue #862); until it lands,
you point your own Grafana at the uids.

## What is provisioned

| Dashboard | uid | Variables | Answers |
| --- | --- | --- | --- |
| `motherships-fleet.json` | `colonizer-fleet` | — | what are my motherships doing |
| `colony.json` | `colonizer-colony` | `$colony`, `$org` | what did this colony do |
| `gateway-spend.json` | `colonizer-gateway` | — | what does the gateway cost |
| `agent-behaviour.json` | `colonizer-agents` | — | how do agents behave |

The files sit in `grafana/dashboards/`; `grafana/provisioning/datasources/datasources.yml` declares
the three datasources behind the uids, and `grafana/provisioning/dashboards/dashboards.yml` watches
the directory. Mount `provisioning/` at `/etc/grafana/provisioning` and `dashboards/` at
`/var/lib/grafana/dashboards` — the [README](../../examples/observability/README.md) has the volume
block.

### Motherships & fleet

Colonies right now by status, org and agent module, starts and finishes by outcome, queue depth,
open questions, and attention by reason: the [catalogue](metrics.md)'s gauges as one page. Storage
gets its own panels — `colonizer_storage_alert` and `colonizer_disk_free_bytes`, which is absent
until first measured rather than zero. The bottom row watches the exporter itself: `exported` and
`dropped` records by `colonizer_drop_reason`, and `export_failures`, each per
`service_instance_id`, so one install falling behind is visible from the same page. With repo-name
hashing on, the `org` label is HMAC-hashed like every other surface
([what a series may name](metrics.md#what-a-series-may-name)).

### Colony

`$colony` is a plain textbox, because a colony id is never a label or a series
([what a series may name](metrics.md#what-a-series-may-name)). Every span in a colony's trace
carries `span.colonizer.colony.id`, so the per-colony span panels filter directly on it: the
trace search is `{span.colonizer.colony.id="$colony"}` (which finds the trace via any span),
and turn durations at p50/p95, the table of tool calls, tool errors by tool, and questions with
`colonizer.answered_by` (a person or the autonomy judge) each query by attribute alone,
e.g. `{span.colonizer.colony.id="$colony" && name=~"execute_tool .*"}`, with no structural
operator needed.
The harness log is Loki filtered on the same id as an attribute,
`| colonizer_colony_id="$colony"`. Tokens and spend are scoped to the colony's org through an
`$org` variable over `label_values(colonizer_colonies, org)`, because colony-level spend is never
exported at all. The content panel counts content log records by `trace_id` and shows "Content
export is off for this org" while it is empty: content log records do not exist in this build yet
(issue #848) — the panel lights up when they do.

### Gateway & spend

Requests, failures and fallbacks per provider, the failure share over 15 minutes, in-flight and
queued requests, and latency quantiles from the `colonizer_gateway_request_duration_seconds`
histogram. Tokens come in the two shapes of `colonizer_tokens_total` — `model="all"` split by type,
and one row per named model — and [the two shapes must never be
summed](metrics.md#the-two-token-shapes): the `all` rows already contain every model. Spend is
charted per hour by org and by model, with the last 24 hours as a single stat.

### Agent behaviour

Every panel is a Tempo TraceQL metrics query over the exported spans, whose names and attributes
are [the traces page's](traces.md): `execute_tool <tool>`, `subagent <type>`, `question`,
`host_step <step>`, `turn <n>`. Tool calls and errors by tool, denials by
`colonizer.denial.class`, verification verdicts by `colonizer.verdict`, subagent fan-out by
`gen_ai.agent.name`, and answers split person against autonomy-judge — deliberately with no
per-colony dimension. This is the one dashboard with a backend requirement: it needs a Tempo with
trace metrics (local blocks) enabled.

One assumption runs through these queries: the resource's `service.name` is the default
`colonizer-mothership`. Every agent-behaviour panel selects
`resource.service.name="colonizer-mothership"` in TraceQL, and the colony dashboard's harness-log
and content panels select `service_name="colonizer-mothership"` in LogQL — an install that
overrides it with `OTEL_SERVICE_NAME` must update those selectors to match.

## Loki names, on the wire

Loki sanitises label names on ingest — dots become underscores — so LogQL selectors write
`service_name` and `colonizer_stream`. `colonizer.colony.id` stays a log attribute and filters as
`| colonizer_colony_id="…"`; per the [design
doc](../design/observability.md#cardinality-and-loki-labels), only `service.name`,
`colonizer.stream` and `severity` (plus an `org` an operator opts into) ever become labels.

## The lint

[`scripts/check-dashboards.mjs`](../../scripts/check-dashboards.mjs) runs in CI's `scripts` job;
[`scripts/test/check-dashboards.test.mjs`](../../scripts/test/check-dashboards.test.mjs) tests the
checker. It rejects a dashboard that does not parse, colliding panel ids (or a dashboard uid two
files claim), a datasource uid nothing provisions, a PromQL `colonizer_*` metric outside the
catalogues — [the pull surface](metrics.md) plus the OTLP names of [the
export](../observability.md) translated to their Prometheus wire names — and any 8-hex-digit run
inside a string value, the shape of a colony id, anywhere in the file: object keys and numbers are
not string values and are never flagged. A renamed metric or a pasted colony id
should fail the pull request, not a reader's panel.
