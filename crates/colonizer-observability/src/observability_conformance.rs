//! The conformance run against a REAL OpenTelemetry Collector (#864): everything the crate's own
//! tests assert against fakes and goldens, asserted instead against what a real collector parsed
//! out of our bytes and stored. It proves the wire contract, not our model of it — ids arrive as
//! the lowercase hex the OTLP/JSON mapping demands, parent links survive, the resource attributes
//! are there, record counts match what the exporter reports sent, the secret canaries never reach
//! the stored output in any form, and the content gate drops content-tier text a closed gate must.
//!
//! CI runs it in `.github/workflows/observability-conformance.yml`, which builds
//! `.github/ci/otel-collector/Dockerfile` (an `otel/opentelemetry-collector-contrib` image pinned
//! by digest) and points the test at it. Locally:
//!
//! ```sh
//! mkdir -p /tmp/otel-conformance
//! docker run --rm -p 127.0.0.1:4317:4317 -p 127.0.0.1:4318:4318 -p 127.0.0.1:13133:13133 \
//!   -v /tmp/otel-conformance:/tmp/otel-conformance $(docker build -q .github/ci/otel-collector)
//! cargo test -p colonizer-observability --locked observability_conformance --features otlp-grpc -- --ignored --nocapture
//! ```
//!
//! or with a plain `otelcol-contrib` binary of the pinned version and the same config:
//!
//! ```sh
//! mkdir -p /tmp/otel-conformance
//! otelcol-contrib --config .github/ci/otel-collector/conformance.yaml
//! ```
//!
//! Everything lives under `/tmp/otel-conformance` (override with `OTEL_CONFORMANCE_ENDPOINT`,
//! `OTEL_CONFORMANCE_GRPC_ENDPOINT`, `OTEL_CONFORMANCE_OUTPUT`, `OTEL_CONFORMANCE_FILE_OUTPUT`
//! and `OTEL_CONFORMANCE_GOLDEN_DIR`). Setting `OTEL_CONFORMANCE_DEMO=break-ids` or
//! `leak-canary` runs one extra phase after the normal ones that deliberately fails, demonstrating
//! the run catches a spec-violating id encoding and an unredacted secret; see
//! docs/observability/conformance.md.

use crate::batch::{BatchConfig, Batcher};
use crate::contract::{self, ColonyPolicy, Contract, Settings};
use crate::exporter::Exporter;
use crate::map::Mapper;
use crate::policy::{ContentGate, Policy, PolicyConfig, Source};
use crate::testkit::Canaries;
use crate::transport::{Outcome, Transport};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long a phase waits for the file exporter's background flush to reach the output file.
const DEADLINE: Duration = Duration::from_secs(15);
/// The shorter wait the demo phases use: they expect the collector to refuse or leak, not to work.
const DEMO_DEADLINE: Duration = Duration::from_secs(5);

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The collector's stored output: the whole file's bytes, and every NDJSON line parsed. The file
/// only grows while the collector runs, so a phase's own records are the lines past the baseline
/// snapshot it started from.
struct Stored {
    bytes: Vec<u8>,
    lines: Vec<Value>,
}

/// One signal's three keys in pdata JSON: the resource list, the scope list, the record list.
fn signal_keys(signal: &str) -> (&'static str, &'static str, &'static str) {
    match signal {
        "logs" => ("resourceLogs", "scopeLogs", "logRecords"),
        "traces" => ("resourceSpans", "scopeSpans", "spans"),
        "metrics" => ("resourceMetrics", "scopeMetrics", "metrics"),
        other => panic!("unknown signal {other}"),
    }
}

impl Stored {
    fn read(path: &Path) -> Stored {
        let bytes = std::fs::read(path).unwrap_or_default();
        let lines = bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_slice(l).ok())
            .collect();
        Stored { bytes, lines }
    }

    /// Every `(resource, record)` pair of one signal, across every stored line.
    fn records<'a>(&'a self, signal: &str) -> Vec<(&'a Value, &'a Value)> {
        let (resource_key, scope_key, record_key) = signal_keys(signal);
        let mut out = Vec::new();
        for line in &self.lines {
            for resource in line.get(resource_key).and_then(Value::as_array).into_iter().flatten() {
                for scope in resource.get(scope_key).and_then(Value::as_array).into_iter().flatten() {
                    for record in scope.get(record_key).and_then(Value::as_array).into_iter().flatten() {
                        out.push((resource, record));
                    }
                }
            }
        }
        out
    }

    /// How many records of one signal the collector stored.
    fn count(&self, signal: &str) -> usize {
        self.records(signal).len()
    }

    /// One string attribute of a record's resource, as pdata JSON spells it.
    fn resource_attr(resource: &Value, key: &str) -> Option<String> {
        resource
            .pointer("/resource/attributes")?
            .as_array()?
            .iter()
            .find(|kv| kv.get("key").and_then(Value::as_str) == Some(key))?
            .pointer("/value/stringValue")?
            .as_str()
            .map(str::to_string)
    }
}

/// Reads the output file until `ready` accepts it, polling while the collector's file exporter
/// flushes in the background. Panics with the run instructions when the deadline passes.
fn wait_for_stored(path: &Path, tag: &str, ready: impl Fn(&Stored) -> bool) -> Stored {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        let stored = Stored::read(path);
        if !stored.lines.is_empty() && ready(&stored) {
            return stored;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "{tag}: the collector output {} never satisfied the expectation within {DEADLINE:?}; \
         is an OpenTelemetry Collector running this config? See docs/observability/conformance.md",
        path.display()
    );
}

/// A throwaway data dir, removed on drop like the exporter tests' `Root`.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-conformance-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(dir.join("sessions/c1a0dec0")).unwrap();
    Root(dir)
}

/// One conformance demo toggle, set on creation and cleared when the scope leaves — a panic
/// unwinds through the drop, so a demo never leaves redaction skipped or ids broken for sibling
/// tests under `--include-ignored`.
struct DemoToggle(&'static std::sync::atomic::AtomicBool);

impl DemoToggle {
    fn set(flag: &'static std::sync::atomic::AtomicBool) -> DemoToggle {
        flag.store(true, Ordering::SeqCst);
        DemoToggle(flag)
    }
}

impl Drop for DemoToggle {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn append(path: &Path, line: &Value) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn chrono_now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The colonies the trace fixtures belong to, with the metadata the builder reads: the same table
/// the traces tests use, so the spans this run expects are the spans those tests pin in goldens.
fn colonies() -> BTreeMap<String, ColonyPolicy> {
    crate::traces::tests::colonies()
}

/// Seeds a data dir: every secret canary in the harness log (the exporter tests' wire-sweep
/// setup), the content canary in an activity `detail` (content tier, so the closed gate must drop
/// it), a gateway line for the metrics push, and the three trace fixtures with the outcomes that
/// close them.
fn seed(root: &Root, canaries: &Canaries) {
    for canary in &canaries.all {
        append(
            &root.0.join("sessions/c1a0dec0/harness.jsonl"),
            &serde_json::json!({"type": "harness_log", "origin": "host", "level": "info", "message": canary.text, "ts": chrono_now()}),
        );
    }
    append(
        &root.0.join("sessions/c1a0dec0/harness.jsonl"),
        &serde_json::json!({"type": "harness_log", "origin": "host", "level": "info", "message": "conformance: a plain harness line", "ts": chrono_now()}),
    );
    append(
        &root.0.join("activity.jsonl"),
        &serde_json::json!({"seq": 1, "ts": chrono_now(), "kind": "question", "actor": "you", "detail": format!("asked: {}", canaries.content)}),
    );
    append(
        &root.0.join("activity.jsonl"),
        &crate::traces::tests::outcome("c1a0dec0", "merged", "2026-09-24T09:32:00Z"),
    );
    append(
        &root.0.join("activity.jsonl"),
        &crate::traces::tests::outcome("a1b2c3d4", "merged", "2026-09-24T09:40:58Z"),
    );
    append(&root.0.join("sessions/c1a0dec0/gateway.jsonl"), &gateway_line());
    for (colony, fixture) in [
        ("c1a0dec0", "claude-code-events.jsonl"),
        ("a1b2c3d4", "data-v0.1.9-a1b2c3d4-events.jsonl"),
        ("e5f60718", "data-v0.1.9-e5f60718-events.jsonl"),
    ] {
        std::fs::create_dir_all(root.0.join(format!("sessions/{colony}"))).unwrap();
        for line in crate::traces::tests::fixture(fixture) {
            append(&root.0.join(format!("sessions/{colony}/events.jsonl")), &line);
        }
    }
}

/// Writes the contract for `endpoint`: the backlog read from the start with no age limit (the
/// fixtures are months old), metrics on, `fleet-1` so the fleet attribute is distinct from the
/// host id, and the colonies table above.
fn contract(root: &Root, endpoint: &str, edit: impl FnOnce(&mut Settings)) -> PathBuf {
    let mut settings = Settings {
        endpoint: endpoint.to_string(),
        stream_metrics: true,
        start_from: contract::START_BACKLOG.into(),
        max_backlog_days: 0,
        ..Settings::default()
    };
    edit(&mut settings);
    let c = Contract {
        contract: contract::CONTRACT,
        mothership_version: env!("CARGO_PKG_VERSION").into(),
        host_id: crate::traces::tests::HOST.into(),
        fleet_id: "fleet-1".into(),
        data_dir: root.0.clone(),
        settings,
        policy: colonies(),
        relay_members: Vec::new(),
    };
    let path = contract::path(&root.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}

/// A span as the four fields the round trip must preserve: name, trace id, span id, parent span
/// id — the ids in lowercase hex, the OTLP/JSON mapping's required form.
fn span_key(span: &crate::proto::trace::v1::Span) -> [String; 4] {
    [
        span.name.clone(),
        hex(&span.trace_id),
        hex(&span.span_id),
        hex(&span.parent_span_id),
    ]
}

fn stored_span_key(span: &Value) -> [String; 4] {
    let id = |key: &str| span.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    [id("name"), id("traceId"), id("spanId"), id("parentSpanId")]
}

/// The gateway line c1a0dec0 seeds: a fixed ts, so its `chat` span — whose id is derived from the
/// line — is the same on both sides of the round trip, whatever the wall clock says.
fn gateway_line() -> Value {
    serde_json::json!({"type": "gateway_request", "ts": "2026-09-24T09:31:00Z", "provider": "openrouter", "model": "qwen3-coder", "status": 200, "duration_ms": 120, "queue_ms": 1, "input_tokens": 10, "output_tokens": 5})
}

/// The spans the seeded fixtures must produce, computed by the same `Run` the traces tests check
/// against their goldens: the ids this run must find again in the collector's stored output.
fn expected_spans() -> BTreeSet<[String; 4]> {
    let table = colonies();
    let run = crate::traces::tests::run(&table);
    let now = crate::traces::tests::NOW;
    let mut state = crate::traces::Traces::default();
    let mut items = Vec::new();
    for (colony, fixture, outcomes) in [
        ("c1a0dec0", "claude-code-events.jsonl", "2026-09-24T09:32:00Z"),
        ("a1b2c3d4", "data-v0.1.9-a1b2c3d4-events.jsonl", "2026-09-24T09:40:58Z"),
        ("e5f60718", "data-v0.1.9-e5f60718-events.jsonl", ""),
    ] {
        let activity: Vec<Value> = if outcomes.is_empty() {
            Vec::new()
        } else {
            vec![crate::traces::tests::outcome(colony, "merged", outcomes)]
        };
        let gateway: Vec<Value> = if colony == "c1a0dec0" {
            vec![gateway_line()]
        } else {
            Vec::new()
        };
        items.extend(run.feed_all(
            &mut state,
            colony,
            &crate::traces::tests::fixture(fixture),
            &gateway,
            &activity,
            now,
        ));
    }
    crate::traces::tests::spans(&items).iter().map(span_key).collect()
}

/// The closed-gate secret sweep over everything the collector stored: every canary, in every
/// encoded form, per stored line (a line is one parsed request, so the testkit's decoder sees
/// exactly what the collector saw), plus the content canary nowhere in the raw bytes.
fn assert_no_canaries(stored: &Stored, canaries: &Canaries) {
    for line in stored.bytes.split(|b| *b == b'\n') {
        if !line.is_empty() {
            crate::testkit::assert_absent(line, canaries.secrets(), "the collector stored a leak");
        }
    }
    let text = String::from_utf8_lossy(&stored.bytes);
    assert!(
        !text.contains(&canaries.content),
        "the content canary reached the wire with the gate closed"
    );
}

/// What one exporter phase asserts the real collector stored, on top of the shared sweeps below.
fn assert_common(tag: &str, stored: &Stored, expected: &BTreeSet<[String; 4]>) {
    let stored_spans: BTreeSet<[String; 4]> = stored.records("traces").iter().map(|(_, s)| stored_span_key(s)).collect();
    assert_eq!(
        stored_spans, *expected,
        "{tag}: the stored spans are not the fixtures' spans, ids and parents included"
    );
    for signal in ["traces", "logs"] {
        let Some((resource, _)) = stored.records(signal).first().copied() else {
            panic!("{tag}: no stored {signal} records");
        };
        for (key, expect) in [
            ("service.name", "colonizer-mothership"),
            ("service.instance.id", crate::traces::tests::HOST),
            ("colonizer.fleet.id", "fleet-1"),
        ] {
            assert_eq!(
                Stored::resource_attr(resource, key).as_deref(),
                Some(expect),
                "{tag}: resource attribute {key}"
            );
        }
        assert_eq!(
            Stored::resource_attr(resource, "service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "{tag}: resource attribute service.version"
        );
    }
    let names: BTreeSet<String> = stored
        .records("metrics")
        .iter()
        .filter_map(|(_, m)| m.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    for name in ["colonizer.gateway.requests", "colonizer.observability.bytes"] {
        assert!(names.contains(name), "{tag}: metric {name} missing: {names:?}");
    }
}

/// One protocol leg: a fresh data dir through the real exporter and transport, then the counts
/// and the shared assertions against what the collector stored for it. `baseline` is advanced to
/// the stored output the phase ended with.
async fn export_phase(
    tag: &str,
    endpoint: &str,
    protocol: &str,
    canaries: &Canaries,
    expected: &BTreeSet<[String; 4]>,
    output: &Path,
    baseline: &mut Stored,
) {
    let root = root(tag);
    seed(&root, canaries);
    let path = contract(&root, endpoint, |s| s.protocol = protocol.into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(
        x.tick().await,
        crate::exporter::TICK,
        "{tag}: the collector did not acknowledge the whole batch ({})",
        x.status.state
    );
    let before: Vec<usize> = ["logs", "traces", "metrics"].iter().map(|s| baseline.count(s)).collect();
    let want: Vec<usize> = ["logs", "traces", "metrics"]
        .iter()
        .enumerate()
        .map(|(n, signal)| before[n] + x.status.signals[*signal].records_sent as usize)
        .collect();
    assert!(want.iter().zip(&before).all(|(w, b)| w > b), "{tag}: nothing was sent");
    let ready = |s: &Stored| {
        ["logs", "traces", "metrics"]
            .iter()
            .enumerate()
            .all(|(n, signal)| s.count(signal) >= want[n])
    };
    let stored = wait_for_stored(output, tag, ready);
    for (n, signal) in ["logs", "traces", "metrics"].iter().enumerate() {
        assert_eq!(stored.count(signal), want[n], "{tag}: stored {signal} records vs sent");
    }
    assert_common(tag, &stored, expected);
    assert_no_canaries(&stored, canaries);
    *baseline = stored;
}

/// The content gate open — test-only, the one place content ever leaves this crate: the same
/// mapper the exporter uses, over an activity line whose `detail` carries every secret canary and
/// the content canary. What must reach the collector: the content canary verbatim, every secret
/// only as its `[REDACTED:<kind>]` mark.
async fn gate_open_phase(endpoint: &str, canaries: &Canaries, output: &Path, baseline: &mut Stored) {
    let policy = Policy::new(PolicyConfig::default(), ContentGate::open(), None);
    let table = colonies();
    let mapper = Mapper {
        policy: &policy,
        host_id: crate::traces::tests::HOST,
        colonies: &table,
        now_unix_nanos: crate::exporter::now_nanos(),
    };
    let mut detail: Vec<String> = canaries.all.iter().map(|c| c.text.clone()).collect();
    detail.push(format!("asked: {}", canaries.content));
    let line =
        serde_json::json!({"seq": 7, "ts": chrono_now(), "kind": "question", "actor": "you", "detail": detail.join(" | ")});
    let item = mapper
        .log(Source::Activity, None, &line, &[0u8; 32])
        .expect("an activity line maps");
    let resource = policy.resource(&[("service.name", "colonizer-conformance".into())]);
    let mut batcher = Batcher::new(
        &resource,
        BatchConfig {
            encoding: crate::encode::Encoding::Json,
            ..BatchConfig::default()
        },
    );
    batcher.push(item);
    let requests = batcher.finish().requests;
    assert_eq!(requests.len(), 1, "one record is one request");
    let settings = Settings {
        endpoint: endpoint.to_string(),
        protocol: "http/json".into(),
        compression: "none".into(),
        ..Settings::default()
    };
    let transport = Transport::new(&settings, std::path::Path::new("."), Vec::new()).unwrap();
    match transport.send(&requests[0]).await {
        Outcome::Sent { rejected, .. } => assert_eq!(rejected, 0, "the collector refused records"),
        other => panic!("gate-open: the collector refused the request: {other:?}"),
    }
    let want = baseline.count("logs") + 1;
    let stored = wait_for_stored(output, "gate-open", |s| s.count("logs") >= want);
    let text = String::from_utf8_lossy(&stored.bytes);
    assert!(
        text.contains(&canaries.content),
        "the open gate did not carry the content canary"
    );
    for line in stored.bytes.split(|b| *b == b'\n') {
        if !line.is_empty() {
            crate::testkit::assert_absent(line, canaries.secrets(), "gate-open");
        }
    }
    for canary in &canaries.all {
        for mark in &canary.marks {
            let mark = format!("[REDACTED:{mark}]");
            assert!(text.contains(&mark), "gate-open: redaction left no {mark}");
        }
    }
    *baseline = stored;
}

/// Every marker string of a golden: the names, event names and ids a round trip through the file
/// receiver must keep.
fn markers(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if matches!(key.as_str(), "name" | "eventName" | "traceId" | "spanId" | "unit")
                    && v.as_str().is_some_and(|s| !s.is_empty())
                {
                    out.push(v.as_str().unwrap().to_string());
                }
                markers(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| markers(v, out)),
        _ => {}
    }
}

/// The `otlpjsonfile` leg: the crate's committed OTLP/JSON goldens, appended as NDJSON into the
/// receiver's input files, must reappear in its own output — a real receiver accepted our exact
/// encoding bytes. This stands in for epic #850's file provider, which is not built yet; the
/// goldens are byte-identical to what it would write.
fn golden_leg(golden_dir: &Path, file_output: &Path) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/otlp");
    let mut want = Vec::new();
    for (signal, files) in [
        ("traces", &["traces-simple.json", "traces-subagents.json"][..]),
        ("logs", &["logs.json", "logs-mothership.json"][..]),
        ("metrics", &["metrics.json"][..]),
    ] {
        let mut ndjson = String::new();
        for file in files {
            let golden: Value =
                serde_json::from_str(&std::fs::read_to_string(fixtures.join(file)).unwrap_or_else(|e| panic!("{}: {e}", file)))
                    .unwrap();
            markers(&golden, &mut want);
            ndjson.push_str(&golden.to_string());
            ndjson.push('\n');
        }
        std::fs::write(golden_dir.join(format!("golden-{signal}.jsonl")), ndjson).unwrap();
    }
    wait_for_stored(file_output, "golden", |s| {
        let text = String::from_utf8_lossy(&s.bytes);
        want.iter().all(|m| text.contains(m))
    });
}

/// `OTEL_CONFORMANCE_DEMO=break-ids`: with encode.rs's `BREAK_IDS_FOR_CONFORMANCE` flipped, the
/// OTLP/JSON request carries trace and span ids in a spec-violating decimal form, so this phase's
/// spans cannot all be stored with their lowercase-hex ids — the property every normal phase
/// asserts — and the run fails, demonstrating the check bites. Reached only behind the env var.
async fn demo_break_ids(endpoint: &str, canaries: &Canaries, expected: &BTreeSet<[String; 4]>, output: &Path, baseline: &Stored) {
    let _break_ids = DemoToggle::set(&crate::encode::BREAK_IDS_FOR_CONFORMANCE);
    let root = root("demo-ids");
    seed(&root, canaries);
    let path = contract(&root, endpoint, |s| s.protocol = "http/json".into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    let _ = x.tick().await;
    let start = Instant::now();
    let mut stored = Stored::read(output);
    while start.elapsed() < DEMO_DEADLINE {
        stored = Stored::read(output);
        if stored.count("traces") > baseline.count("traces") {
            std::thread::sleep(Duration::from_millis(500));
            stored = Stored::read(output);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let from = baseline.lines.len().min(stored.lines.len());
    let mut stored_keys = BTreeSet::new();
    for line in &stored.lines[from..] {
        for resource in line.get("resourceSpans").and_then(Value::as_array).into_iter().flatten() {
            for scope in resource.get("scopeSpans").and_then(Value::as_array).into_iter().flatten() {
                for s in scope.get("spans").and_then(Value::as_array).into_iter().flatten() {
                    stored_keys.insert(stored_span_key(s));
                }
            }
        }
    }
    let ok = stored_keys.is_superset(expected);
    if !ok {
        panic!(
            "DEMO (OTEL_CONFORMANCE_DEMO=break-ids): only {} of the phase's spans were stored with \
             their lowercase-hex ids; encode.rs's BREAK_IDS_FOR_CONFORMANCE hook emitted \
             spec-violating ids, the receiver refused them, and the conformance property failed — \
             the red run issue #864's acceptance criteria ask to see. Unset OTEL_CONFORMANCE_DEMO \
             for a normal run.",
            stored_keys.len()
        );
    }
    panic!(
        "DEMO (break-ids): the collector stored every span as lowercase hex anyway; the demo did not reproduce the failure it exists to demonstrate"
    );
}

/// `OTEL_CONFORMANCE_DEMO=leak-canary`: with policy's `SKIP_REDACTION_FOR_CONFORMANCE` flipped,
/// the export-time redaction pass is skipped, so a seeded canary secret reaches the collector —
/// and its stored output — unredacted, which the sweep every normal phase runs must catch.
/// Reached only behind the env var.
async fn demo_leak_canary(endpoint: &str, canaries: &Canaries, output: &Path, baseline: &Stored) {
    let _skip_redaction = DemoToggle::set(&crate::policy::SKIP_REDACTION_FOR_CONFORMANCE);
    let root = root("demo-leak");
    for canary in &canaries.all {
        append(
            &root.0.join("sessions/c1a0dec0/harness.jsonl"),
            &serde_json::json!({"type": "harness_log", "origin": "host", "level": "info", "message": canary.text, "ts": chrono_now()}),
        );
    }
    let path = contract(&root, endpoint, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, crate::exporter::TICK, "the collector accepted the leak");
    let want = baseline.count("logs") + x.status.signals["logs"].records_sent as usize;
    let stored = wait_for_stored(output, "demo-leak", |s| s.count("logs") >= want);
    let text = String::from_utf8_lossy(&stored.bytes);
    if canaries.secrets().any(|secret| text.contains(secret)) {
        panic!(
            "DEMO (OTEL_CONFORMANCE_DEMO=leak-canary): a canary secret reached the collector's \
             stored output unredacted; policy's SKIP_REDACTION_FOR_CONFORMANCE hook skipped the \
             export-time redaction pass and the conformance sweep caught it — the red run issue \
             #864's acceptance criteria ask to see. Unset OTEL_CONFORMANCE_DEMO for a normal run."
        );
    }
    panic!(
        "DEMO (leak-canary): no canary reached the stored output; the demo did not reproduce the failure it exists to demonstrate"
    );
}

/// The conformance run itself: one sequential test, so the cfg(test) demo toggles cannot race a
/// sibling. Ignored by default — it needs the real collector the CI job (or the local commands in
/// this module's doc) starts.
#[tokio::test]
#[ignore = "exports to a real OpenTelemetry Collector; CI starts one via .github/workflows/observability-conformance.yml — docs/observability/conformance.md has the local commands"]
async fn the_real_collector_stores_what_we_export() {
    let endpoint = env_or("OTEL_CONFORMANCE_ENDPOINT", "http://127.0.0.1:4318");
    #[cfg(feature = "otlp-grpc")]
    let grpc_endpoint = env_or("OTEL_CONFORMANCE_GRPC_ENDPOINT", "http://127.0.0.1:4317");
    let output = PathBuf::from(env_or(
        "OTEL_CONFORMANCE_OUTPUT",
        "/tmp/otel-conformance/collector-output.json",
    ));
    let file_output = PathBuf::from(env_or(
        "OTEL_CONFORMANCE_FILE_OUTPUT",
        "/tmp/otel-conformance/file-receiver-output.json",
    ));
    let golden_dir = PathBuf::from(env_or("OTEL_CONFORMANCE_GOLDEN_DIR", "/tmp/otel-conformance"));
    // A golden input left by an earlier local run would sit at an offset the receiver already
    // read; this run rewrites them, so start from no files.
    for signal in ["traces", "metrics", "logs"] {
        let _ = std::fs::remove_file(golden_dir.join(format!("golden-{signal}.jsonl")));
    }
    let canaries = Canaries::new();
    let expected = expected_spans();
    assert!(!expected.is_empty(), "the fixtures produced no spans to expect");
    let mut baseline = Stored::read(&output);

    export_phase(
        "protobuf",
        &endpoint,
        "http/protobuf",
        &canaries,
        &expected,
        &output,
        &mut baseline,
    )
    .await;
    export_phase("json", &endpoint, "http/json", &canaries, &expected, &output, &mut baseline).await;
    #[cfg(feature = "otlp-grpc")]
    export_phase("grpc", &grpc_endpoint, "grpc", &canaries, &expected, &output, &mut baseline).await;

    gate_open_phase(&endpoint, &canaries, &output, &mut baseline).await;
    golden_leg(&golden_dir, &file_output);

    match std::env::var("OTEL_CONFORMANCE_DEMO").as_deref() {
        Ok("break-ids") => demo_break_ids(&endpoint, &canaries, &expected, &output, &baseline).await,
        Ok("leak-canary") => demo_leak_canary(&endpoint, &canaries, &output, &baseline).await,
        _ => {}
    }
}
