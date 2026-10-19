# Local Grafana observability stack

The whole local stack behind the [Colonizer dashboards](../README.md): one `docker compose up -d`
runs Grafana Alloy (OTLP in on 4317/4318), Loki, Tempo and Prometheus, plus a Grafana that loads
the provisioned datasources and dashboards. Every published port is bound to `127.0.0.1`, so
nothing here is reachable from another machine.

- `alloy/config.alloy` — OTLP receiver; fans logs out to Loki, traces to Tempo, metrics to
  Prometheus. The Loki label rule (ADR: only `service.name`, `colonizer.stream` and the severity
  become labels, everything else structured metadata) lives in `loki/loki.yaml`.
- `alloy/file.alloy` — the not-yet-built `file` provider variant ([below](#the-file-provider)).
- `docker-compose.yml`, `loki/`, `tempo/`, `prometheus/` — the services, digest-pinned.

## 1. Create the scrape token

Prometheus scrapes the mothership's `GET /metrics` with a scoped API token: `read` scope, **no**
org or repo limits ([the metrics docs](../../../docs/observability/metrics.md) explain why that
exact shape). Mint one with the install's own token:

```sh
curl -H "Authorization: Bearer $OWNER_TOKEN" -H 'content-type: application/json' \
  -d '{"name":"prometheus-local","scope":"read"}' \
  http://127.0.0.1:7878/api/tokens
```

The `col_…` plaintext comes back exactly once; save it (the directory is gitignored):

```sh
mkdir -p secrets && printf %s 'col_PASTE_HERE' > secrets/prometheus.token
```

## 2. Start the stack

```sh
docker compose up -d
```

Prometheus refuses to start without the token file, and its scrape of the mothership only works
when the mothership is bound where the docker bridge can reach it (`host.docker.internal`): the
default `127.0.0.1:7878` bind is host-only, so either rebind the mothership or leave the target
down — every dashboard family also arrives via the OTLP push, so the scrape is redundancy.

## 3. Point the mothership at it

In the cockpit, give the observability module the OTLP endpoint `http://localhost:4318` — plain
HTTP is allowed to loopback — with protocol `http/protobuf` (the default). Then send a test
event from the cockpit's observability page, or with an operate-or-owner token:

```sh
curl -X POST -H "Authorization: Bearer $OWNER_TOKEN" http://127.0.0.1:7878/api/observability/test
```

## 4. Look

Grafana is at <http://localhost:3000> — anonymous Admin, no password, safe because the port is
localhost-only. In Explore, pick Colonizer Loki and find a colony's records:

```logql
{service_name="colonizer-mothership", colonizer_stream="activity"} | colonizer_colony_id="c0ffee12"
```

`colonizer_colony_id` is structured metadata (the label rule), so it filters after the pipe. Each
log row shows a `trace_id` field that jumps into the trace in Tempo; from a span, the view-logs
link jumps back via the matching `trace_id`. The dashboards in the Colonizer folder chart the
same `colonizer_*` metrics the exporter pushes, and Tempo's span-metrics series are in
Prometheus for ad-hoc queries.

## The file provider

The mothership's `file` provider — writing OTLP/JSON to disk instead of pushing — is not built
yet ([#850](https://github.com/Colonizer-dev/harness/issues/850)); `alloy/file.alloy` is the
receiver ready for it, with its limits written at the top of the file. To try it once the
provider exists, mount the provider's directory and run that config instead:

```sh
docker compose run --rm \
  -v "$PWD/alloy/file.alloy:/etc/alloy/config.alloy:ro" \
  -v "$COLONIZER_DATA/observability/otlp:/var/lib/colonizer/observability/otlp:ro" \
  alloy run --stability.level=public-preview --storage.path=/var/lib/alloy/data /etc/alloy/config.alloy
```

## Teardown

```sh
docker compose down        # keep the data volumes
docker compose down -v     # start over from nothing
```
