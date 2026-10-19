# Backend recipes

Where the [overview](../observability.md)'s Examples section gives the short form, this page is the
fuller per-backend reference: the endpoint shape, the header, the Colonizer `backend` value, and the
ingest limits worth knowing. Everything is a placeholder — no real token or key appears here, and
none belongs in a setting: a header's value lives in the **Observability headers** secret (or
`OTEL_EXPORTER_OTLP_HEADERS`), never in the endpoint URL. Values in the `k=v,…` list are
percent-encoded (`Basic%20…` for the space in a Basic or ApiKey scheme), and the **Backend** setting
(`custom` by default) only names which header the cockpit offers — it never stores the value.

**None of the vendor recipes below has been verified against the live services.** Endpoints, headers
and limits follow each vendor's public documentation at the time of writing — check it for your
region or site before relying on a number. The exceptions are the last two recipes, which rest on
machinery the repository runs itself: the [conformance run](conformance.md) in CI drives the
exporter through a real `otelcol-contrib` receiver, and it runs on every change under
`examples/observability/` as well.

The rules every recipe shares:

- **Protocol** `http/protobuf` (default) or `http/json`. `grpc` is refused at save unless the add-on
  was built with the `otlp-grpc` feature ([gRPC](grpc.md)).
- The endpoint is a **base URL**: the exporter appends `/v1/logs`, `/v1/traces` and `/v1/metrics`.
  A per-signal `OTEL_EXPORTER_OTLP_{LOGS,TRACES,METRICS}_ENDPOINT` is used verbatim, which is how a
  backend with per-signal hosts is reached.
- The URL carries no credentials (`user:pass@`) and no query string, and plain `http://` goes only
  to a loopback or private host unless **Allow plain http to a public host** is on.

## Grafana Cloud

```text
endpoint:  https://otlp-gateway-<zone>.grafana.net/otlp
headers:   Authorization=Basic%20<base64 of instance-id:token>
backend:   grafana_cloud
```

The zone (`prod-us-east-0`, …), the instance id and the access token come from the stack's
*OpenTelemetry* connection page. The base64 is of the string `<instance id>:<token>`. HTTP only — a
`grafana_cloud` endpoint with protocol `grpc` is refused at save. One endpoint serves all three
signals; the gateway fans out to Loki, Tempo and Mimir, whose limits are on
[the limits page](limits-and-cost.md).

Not verified against the live service.

## Datadog

The simplest path is the Datadog Agent's OTLP receiver on the same host, enabled as Datadog's *OTLP
Ingestion by the Datadog Agent* documentation describes — the Agent holds the API key, so no header
is needed:

```sh
COLONIZER_OBSERVABILITY=on
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

Without an Agent, send straight to Datadog's intake with the per-signal endpoints verbatim and the
API key in the headers (US hosts shown; Datadog's page gives each site's — `datadoghq.eu`, `us3`,
`us5`, … — and a 403 usually means the endpoint does not match the org's site):

```sh
OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=https://http-intake.logs.datadoghq.com/api/v2/otlp/v1/logs
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://<traces intake host>/api/v2/otlp/v1/traces
OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=https://<metrics intake host>/api/v2/otlp/v1/metrics
OTEL_EXPORTER_OTLP_HEADERS=dd-api-key=<API key>        # backend: datadog
```

The path is always `/api/v2/otlp/v1/<signal>`. Payloads are capped per intake (≈ 15 MiB for traces
and ≈ 5 MiB for logs uncompressed, less for metrics); the logs intake wants `http/protobuf`.

Not verified against the live service.

## Honeycomb

```text
endpoint:  https://api.honeycomb.io            (EU: https://api.eu1.honeycomb.io)
headers:   x-honeycomb-team=<ingest key>,x-honeycomb-dataset=colonizer-metrics
backend:   honeycomb
```

`x-honeycomb-dataset` files metrics under a dataset you name; with an environment-scoped key the
dataset follows `service.name` instead and the header is ignored.

Not verified against the live service.

## Elastic

```text
endpoint:  https://<apm-server-host>:8200
headers:   Authorization=ApiKey%20<base64 API key>     (or Bearer <secret token>)
backend:   elastic
```

APM Server takes OTLP/HTTP on the same port as its own intake (8200), at `/v1/<signal>`; the base64
is the API key exactly as Elastic prints it. On Elastic Cloud (serverless), use the managed OTLP
endpoint the integration page gives instead. A self-managed APM Server without TLS is a plain
`http://` endpoint, so it must be loopback or private unless the insecure switch is on.

Not verified against the live service.

## SigNoz

```text
self-hosted:  endpoint: http://<signoz-host>:4318    headers: (none)
cloud:        endpoint: https://ingest.<region>.signoz.cloud:443
              headers:  signoz-ingestion-key=<ingestion key>
backend:   signoz
```

SigNoz Cloud serves both OTLP/HTTP and gRPC on port 443, takes gzip, and caps a payload at 16 MB;
the older `ingest.signoz.io` hosts are deprecated. Self-hosted SigNoz speaks the standard 4317/4318
ports and usually wants no header.

Not verified against the live service.

## New Relic

```text
endpoint:  https://otlp.nr-data.net            (EU: https://otlp.eu01.nr-data.net)
headers:   api-key=<license key>
backend:   new_relic
```

The `api-key` value is the account's license key (an **Ingest-License** type key); TLS 1.2 or newer.
Two limits bite: string attribute values are truncated at 4095 characters (the exporter's
`max_attribute_bytes` default of 1 KiB is well under), and a request over **1 MB (10⁶ bytes)** is
rejected. The exporter splits requests below 1 MiB — about 5 % above 10⁶ — so a maximal request can
draw a 413, which it then bisects and retries with nothing dropped; gzip, on by default, keeps most
requests far under.

Not verified against the live service.

## The local example stack

A runnable Grafana stack — Alloy as the collector, Loki, Tempo, Prometheus and Grafana with
provisioned dashboards — ships in the repository:
[examples/observability/grafana](../../examples/observability/grafana/README.md). Point the exporter
at the Alloy receiver and read the dashboards:

```sh
COLONIZER_OBSERVABILITY=on
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
```

The stack's Tempo sets `max_bytes_per_trace` beside the exporter's `max_trace_bytes`
([limits](limits-and-cost.md)), and Loki's line and old-sample limits are the defaults the exporter's
caps assume. The OTLP wire contract this recipe relies on is what the repository's
[conformance run](conformance.md) asserts against a real `otelcol-contrib` in CI.

## A plain `otelcol-contrib` collector

Any collector the OpenTelemetry project ships works; this config receives OTLP/HTTP on 4318, batches,
and fans the signals out to two destinations (the same receiver shape the
[conformance run](conformance.md) drives):

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 127.0.0.1:4318
processors:
  batch: {}
exporters:
  otlphttp/archive:
    endpoint: https://otlp-gateway-prod-eu-west-0.grafana.net/otlp
    headers:
      authorization: Basic <base64 of instance-id:token>
  otlphttp/second:
    endpoint: https://ingest.eu.signoz.cloud:443
    headers:
      signoz-ingestion-key: <ingestion key>
service:
  pipelines:
    logs:    { receivers: [otlp], processors: [batch], exporters: [otlphttp/archive, otlphttp/second] }
    traces:  { receivers: [otlp], processors: [batch], exporters: [otlphttp/archive] }
    metrics: { receivers: [otlp], processors: [batch], exporters: [otlphttp/second] }
```

A collector's gRPC receiver on 4317 needs the add-on's `otlp-grpc` feature ([gRPC](grpc.md)); the
HTTP receiver on 4318 needs nothing beyond the base endpoint. The exporter's requests stay below the
common 4 MiB receiver cap ([limits](limits-and-cost.md)).
