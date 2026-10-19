# Conformance

The add-on's own tests assert against fakes (an axum server that speaks enough OTLP to answer) and
committed goldens. The conformance run asserts the same things against a **real OpenTelemetry
Collector** ([issue #864](https://github.com/Colonizer-dev/harness/issues/864)): it proves the wire
contract, not our model of it. It runs in CI on every change to the add-on, the mothership's
observability module, the examples, or the collector config, and it is ignored by `cargo test`
because it needs that collector.

## What the job runs

`.github/workflows/observability-conformance.yml` builds `.github/ci/otel-collector/Dockerfile` —
an `otel/opentelemetry-collector-contrib` image pinned by digest (dependabot bumps it weekly) — and
starts it with the config beside it. The collector's OTLP receiver listens on 4317 (gRPC) and 4318
(HTTP), and two `file` exporters store what it parsed, as one pdata-JSON line per received request,
under `/tmp/otel-conformance`:

- `collector-output.json` — what the live receiver made of our requests,
- `file-receiver-output.json` — what the collector's own `otlp_json_file` receivers made of our
  committed OTLP/JSON goldens (epic [#850](https://github.com/Colonizer-dev/harness/issues/850)'s
  file provider is not built yet; the goldens are byte-identical to what it would write).

Then one command runs the whole thing:

```sh
cargo test -p colonizer-observability --locked observability_conformance --features otlp-grpc -- --ignored --nocapture
```

## Running it locally

With Docker, exactly as CI does:

```sh
mkdir -p /tmp/otel-conformance
docker run --rm -p 127.0.0.1:4317:4317 -p 127.0.0.1:4318:4318 -p 127.0.0.1:13133:13133 \
  -v /tmp/otel-conformance:/tmp/otel-conformance $(docker build -q .github/ci/otel-collector)
```

or with a plain `otelcol-contrib` binary of the pinned version (the tag is in the Dockerfile's
comments) and the same config:

```sh
mkdir -p /tmp/otel-conformance
otelcol-contrib --config .github/ci/otel-collector/conformance.yaml
```

then the `cargo test` command above in another shell. Without `--features otlp-grpc` the run skips
only the gRPC leg.

Everything lives under `/tmp/otel-conformance`; the endpoints, both output files and the golden
dir can be moved with `OTEL_CONFORMANCE_ENDPOINT`, `OTEL_CONFORMANCE_GRPC_ENDPOINT`,
`OTEL_CONFORMANCE_OUTPUT`, `OTEL_CONFORMANCE_FILE_OUTPUT` and `OTEL_CONFORMANCE_GOLDEN_DIR`.

## What is asserted

The run is one sequential test (in `crates/colonizer-observability/src/observability_conformance.rs`)
that seeds a data dir the way the exporter tests do — the trace fixtures, the outcomes that close
them, every secret canary in the harness log, a content canary in an activity `detail` — and then:

1. **Three transport legs** — `http/protobuf`, `http/json` and (behind `otlp-grpc`) `grpc` — each
   through the real `Exporter` and `Transport` in a fresh data dir. For each: the collector stored
   exactly as many log, trace and metric records as the exporter reports sent; the stored spans
   are the fixtures' spans with their trace and span ids in lowercase hex and their parent links
   intact; the resource carries `service.name`, `service.version`, `service.instance.id` and
   `colonizer.fleet.id`; and the metric families are there (`colonizer.gateway.requests`,
   `colonizer.observability.bytes`).
2. **The secret sweep** over everything the collector stored, per parsed request, with the same
   canary kit the wire tests use: every secret in every form a leak could take (raw,
   percent-encoded, base64 at every alignment), and the content canary nowhere — the closed
   content gate must drop content-tier text.
3. **The gate-open leg** (test-only, the one place content ever leaves the crate): with the content
   gate open, the content canary arrives and every secret arrives only as its `[REDACTED:<kind>]`
   mark.
4. **The golden leg**: the committed OTLP/JSON goldens, appended into the file receivers' inputs,
   reappear in their output — a real collector parsed our exact encoding bytes.

## The demo toggles

`OTEL_CONFORMANCE_DEMO=break-ids` and `OTEL_CONFORMANCE_DEMO=leak-canary` each run the normal
phases and then one extra phase that **deliberately fails**, to demonstrate the run catches the
regression it exists for:

- `break-ids` — a `#[cfg(test)]` toggle in `encode.rs` writes trace and span ids in decimal
  instead of the spec's lowercase hex; the collector refuses the request, and the run fails with a
  `DEMO:` panic explaining what it caught.
- `leak-canary` — a `#[cfg(test)]` toggle in `policy.rs` skips the export-time redaction pass, so
  a seeded canary secret reaches the collector unredacted; the sweep catches it and the run fails
  the same way.

Neither toggle exists outside a test build. Both are meant for a red run issue #864's acceptance
criteria ask to see; unset the variable for a normal run.
