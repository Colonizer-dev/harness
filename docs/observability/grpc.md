# The OTLP/gRPC transport

Some backends expose OTLP only over gRPC on 4317: a self-hosted OpenTelemetry Collector, Grafana
Alloy, SigNoz. For them the observability add-on carries a second transport, OTLP over gRPC on
tonic — behind its `otlp-grpc` cargo feature, **off by default**. A build without the feature,
which is every release binary, links no gRPC stack at all, and CI keeps both truths checked: the
add-on without the feature links no tonic, and the feature-on build still compiles, lints and
passes its tests.

Build it with — this pulls in tonic and the hyper channel stack beneath it, which is exactly why
it is a feature and not a default:

```sh
cargo build --release -p colonizer-observability --features otlp-grpc
```

Everything around the transport is unchanged: the same records, the same [export
policy](export-policy.md), the same batching and committed offsets as
[OTLP/HTTP](../observability.md).

## Where gRPC applies

Every backend preset — a local collector, Datadog, Honeycomb, Elastic, SigNoz, New Relic — sends
`http/protobuf`; Grafana Cloud is HTTP-only, its gateway takes no gRPC. `grpc` is for a custom or
self-hosted endpoint you run: the collector's 4317, Alloy, SigNoz on your own host.

## Configuration

`protocol: "grpc"` in the exporter settings. The endpoint's scheme decides the channel:

| Scheme | Channel |
| :--- | :--- |
| `https://` | TLS via rustls (the ring provider) with the webpki root store. |
| `http://` | Plaintext h2c. |

Per-signal endpoints (`logs_endpoint`, `traces_endpoint`, `metrics_endpoint`) are honored like
HTTP. The **Observability headers** become gRPC metadata — `authorization` works as usual — while
`content-type`, `content-length`, `content-encoding`, `host` and `te` are dropped: they belong to
the channel, not the operator. gzip request compression follows the `compression` setting; `none`
sends uncompressed.

## When the backend says no

| Status | What happens |
| :--- | :--- |
| `OK` | Committed, like a 2xx over HTTP. |
| `UNAVAILABLE`, `DEADLINE_EXCEEDED`, `ABORTED`, `CANCELLED`, `OUT_OF_RANGE`, `DATA_LOSS` | Retried with backoff: 1 s doubling to 60 s, with jitter. |
| `RESOURCE_EXHAUSTED` carrying `RetryInfo` | Retried after the delay the `RetryInfo` carries, at most 5 minutes — the same cap as the HTTP `Retry-After` handling. |
| `INVALID_ARGUMENT`, `RESOURCE_EXHAUSTED` without `RetryInfo`, `UNKNOWN`, `NOT_FOUND`, `UNIMPLEMENTED`, `ALREADY_EXISTS`, `FAILED_PRECONDITION`, `INTERNAL` | The request is at fault: it is split in half and each half retried, down to the single bad record, which is dropped and counted as `refused`. |
| `UNAUTHENTICATED`, `PERMISSION_DENIED` | **The credential is wrong or expired.** The batch is held, nothing dropped, state `auth_failed`, retried with backoff. |

## What still refuses it

The mothership's settings validation still refuses `protocol: grpc` when saving observability
settings — "this build has no gRPC" — and that is by design, not an oversight: the mothership
never links the gRPC stack, so a saved `grpc` setting has nothing it could reach. Wiring the
settings and the cockpit to a gRPC-capable add-on is future work.

Until then a gRPC build is exercised by hand: write an exporter contract
(`<data>/observability/exporter.json`, contract version 1, settings with `"protocol": "grpc"`)
and point the add-on at it:

```sh
echo '{"headers": "k=v"}' | colonizer-observability send-test-event --contract exporter.json
echo '{"headers": "k=v"}' | colonizer-observability run --contract exporter.json
```

Both read their headers as one JSON line on stdin first, and never print a header value.
`send-test-event` sends one test log record and one metric point and prints one JSON line
(`{"ok": …}`, with each signal's answer); `run` exports until its stdin closes, as the mothership
spawns it. The settings themselves are in [settings and privacy](settings.md).
