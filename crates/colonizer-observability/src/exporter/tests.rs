//! The exporter against a fake OTLP collector on 127.0.0.1: batching, retry with backoff, commit
//! only after an ack, resuming from the cursors after a restart, bisecting a refused request,
//! streams that are off, metrics, the test event, and headers that are sent but never logged.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use prost::Message;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// What the collector saw: the path, the headers, and the request decompressed.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
    status: u16,
}

type Respond = Arc<dyn Fn(&str, &[u8]) -> (u16, Vec<u8>) + Send + Sync>;
/// A scripted answer that also sees the request headers and can set response headers.
type RespondFull = Arc<dyn Fn(&str, &HeaderMap, &[u8]) -> (u16, Vec<(&'static str, String)>, Vec<u8>) + Send + Sync>;

struct Collector {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Collector {
    async fn start(respond: Respond) -> Collector {
        Collector::start_full(Arc::new(move |path, _, body| {
            let (status, reply) = respond(path, body);
            (status, Vec::new(), reply)
        }))
        .await
    }

    async fn start_full(respond: RespondFull) -> Collector {
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        let handler = move |path: &'static str| {
            let log = log.clone();
            let respond = respond.clone();
            move |headers: HeaderMap, body: Bytes| {
                let log = log.clone();
                let respond = respond.clone();
                async move {
                    let mut raw = body.to_vec();
                    if headers.get("content-encoding").is_some_and(|v| v == "gzip") {
                        let mut out = Vec::new();
                        flate2::read::GzDecoder::new(raw.as_slice()).read_to_end(&mut out).unwrap();
                        raw = out;
                    }
                    let (status, extra, reply) = respond(path, &headers, &raw);
                    log.lock().unwrap().push(Seen {
                        path: path.to_string(),
                        headers,
                        body: raw,
                        status,
                    });
                    let mut out = HeaderMap::new();
                    for (name, value) in extra {
                        out.insert(name, value.parse().unwrap());
                    }
                    (StatusCode::from_u16(status).unwrap(), out, reply)
                }
            }
        };
        let app = axum::Router::new()
            .route("/v1/logs", axum::routing::post(handler("/v1/logs")))
            .route("/v1/traces", axum::routing::post(handler("/v1/traces")))
            .route("/v1/metrics", axum::routing::post(handler("/v1/metrics")))
            .route("/loki/api/v1/push", axum::routing::post(handler("/loki/api/v1/push")));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Collector { url, seen }
    }

    fn seen(&self, path: &str) -> Vec<Seen> {
        self.seen.lock().unwrap().iter().filter(|s| s.path == path).cloned().collect()
    }

    /// Every log record body received, in order.
    fn bodies(&self) -> Vec<String> {
        Self::record_bodies(&self.seen("/v1/logs"))
    }

    /// Every log record body the collector answered 2xx to, in order.
    fn accepted(&self) -> Vec<String> {
        let ok: Vec<Seen> = self
            .seen("/v1/logs")
            .into_iter()
            .filter(|s| (200..300).contains(&s.status))
            .collect();
        Self::record_bodies(&ok)
    }

    fn record_bodies(seen: &[Seen]) -> Vec<String> {
        seen.iter()
            .filter_map(|s| ExportLogsServiceRequest::decode(s.body.as_slice()).ok())
            .flat_map(|r| r.resource_logs)
            .flat_map(|r| r.scope_logs)
            .flat_map(|s| s.log_records)
            .filter_map(|r| match r.body?.value? {
                crate::proto::common::v1::any_value::Value::StringValue(s) => Some(s),
                _ => None,
            })
            .collect()
    }
}

fn ok() -> Respond {
    Arc::new(|_, _| (200, Vec::new()))
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-exporter-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(dir.join("sessions/c0ffee12")).unwrap();
    Root(dir)
}

fn harness(root: &Root, message: &str) {
    let line =
        serde_json::json!({"type": "harness_log", "origin": "host", "level": "info", "message": message, "ts": chrono_now()});
    append(&root.0.join("sessions/c0ffee12/harness.jsonl"), &line);
}

fn append(path: &Path, line: &serde_json::Value) {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn chrono_now() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339()
}

/// Writes the contract for `url` and returns its path.
fn contract(root: &Root, url: &str, edit: impl FnOnce(&mut Settings)) -> PathBuf {
    // These tests write their lines before the exporter starts, so they read the backlog; the
    // `start_from = now` default has tests of its own below.
    let mut settings = Settings {
        endpoint: url.to_string(),
        stream_metrics: false,
        start_from: contract::START_BACKLOG.into(),
        ..Settings::default()
    };
    edit(&mut settings);
    let c = Contract {
        contract: contract::CONTRACT,
        mothership_version: env!("CARGO_PKG_VERSION").into(),
        host_id: "host-1".into(),
        fleet_id: String::new(),
        data_dir: root.0.clone(),
        settings,
        policy: BTreeMap::from([(
            "c0ffee12".to_string(),
            ColonyPolicy {
                org: "acme".into(),
                repo: "acme/widgets".into(),
                status: Some("running".into()),
                ..ColonyPolicy::default()
            },
        )]),
        relay_members: Vec::new(),
    };
    let path = contract::path(&root.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}

fn committed_offset(root: &Root) -> Option<u64> {
    let (state, _) = State::load(&root.0);
    state
        .cursors
        .iter()
        .find(|(k, _)| k.relative_path == "sessions/c0ffee12/harness.jsonl")
        .map(|(_, c)| c.offset)
}

#[tokio::test]
async fn batches_retry_with_backoff_commit_on_ack_and_resume_after_a_restart() {
    let root = root("retry");
    // Two 503s, then accept.
    let calls = Arc::new(Mutex::new(0u32));
    let counter = calls.clone();
    let collector = Collector::start(Arc::new(move |_, _| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        if *n <= 2 { (503, b"busy".to_vec()) } else { (200, Vec::new()) }
    }))
    .await;
    for i in 0..5 {
        harness(&root, &format!("line {i}"));
    }
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();

    let first = x.tick().await;
    assert!(
        first >= Duration::from_millis(700) && first <= Duration::from_millis(1300),
        "{first:?}"
    );
    assert_eq!(committed_offset(&root), None, "nothing is committed before an ack");
    assert_eq!(x.status.state, "retrying");
    // The backoff is honoured: an early tick sends nothing.
    let early = x.tick().await;
    assert!(early > Duration::ZERO);
    assert_eq!(collector.seen("/v1/logs").len(), 1);

    x.backoff = x.backoff.map(|(_, d)| (Instant::now() - d, d));
    let second = x.tick().await;
    assert!(second >= Duration::from_millis(1500), "the delay doubles: {second:?}");
    x.backoff = x.backoff.map(|(_, d)| (Instant::now() - d, d));
    assert_eq!(x.tick().await, TICK, "accepted");
    assert_eq!(collector.seen("/v1/logs").len(), 3, "the same batch, three times");
    assert_eq!(
        collector.bodies()[collector.bodies().len() - 5..],
        ["line 0", "line 1", "line 2", "line 3", "line 4"]
    );
    x.shutdown();
    let size = std::fs::metadata(root.0.join("sessions/c0ffee12/harness.jsonl"))
        .unwrap()
        .len();
    assert_eq!(committed_offset(&root), Some(size), "committed after the ack");
    assert_eq!(x.aggregates.export_failures, 2);

    // A restart resumes from the cursor: only the new line goes out.
    harness(&root, "after restart");
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    y.tick().await;
    let bodies = collector.bodies();
    assert_eq!(bodies.len(), 16, "three tries of five, then only the new line: {bodies:?}");
    assert_eq!(bodies.last().unwrap(), "after restart");
}

#[tokio::test]
async fn a_refused_request_is_bisected_down_to_the_bad_record() {
    let root = root("bisect");
    let collector = Collector::start(Arc::new(|_, body: &[u8]| {
        let req = ExportLogsServiceRequest::decode(body).unwrap();
        let poisoned = format!("{req:?}").contains("poison");
        if poisoned {
            (400, b"bad record".to_vec())
        } else {
            (200, Vec::new())
        }
    }))
    .await;
    for msg in ["a", "b", "poison", "c", "d"] {
        harness(&root, msg);
    }
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, TICK, "a refusal is not retried");
    let mut delivered = collector.accepted();
    delivered.sort();
    assert_eq!(delivered, ["a", "b", "c", "d"], "every good record got through, once");
    assert_eq!(x.aggregates.dropped.get("refused"), Some(&1), "exactly one record dropped");
    assert_eq!(x.aggregates.dropped.len(), 1, "{:?}", x.aggregates.dropped);
    let size = std::fs::metadata(root.0.join("sessions/c0ffee12/harness.jsonl"))
        .unwrap()
        .len();
    assert_eq!(
        committed_offset(&root),
        Some(size),
        "the cursor moves past the dropped record"
    );
}

#[tokio::test]
async fn streams_that_are_off_send_nothing() {
    let root = root("off");
    let collector = Collector::start(ok()).await;
    harness(&root, "hello");
    append(
        &root.0.join("activity.jsonl"),
        &serde_json::json!({"seq": 1, "kind": "outcome.merged", "actor": "you"}),
    );
    let path = contract(&root, &collector.url, |s| {
        s.stream_operational = false;
        s.stream_activity = false;
        s.stream_metrics = false;
    });
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    for _ in 0..3 {
        x.tick().await;
    }
    assert!(collector.seen.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn old_lines_past_the_backlog_limit_are_dropped_and_counted() {
    let root = root("backlog");
    let collector = Collector::start(ok()).await;
    append(
        &root.0.join("sessions/c0ffee12/harness.jsonl"),
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "ancient", "ts": "2020-01-01T00:00:00Z"}),
    );
    harness(&root, "fresh");
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    // The skipped run goes out as one `export_gap` record, whose body is its reason.
    assert_eq!(collector.bodies(), ["backlog", "fresh"]);
    assert_eq!(x.aggregates.dropped.get("backlog"), Some(&1));
}

/// Every number point of a push as `(metric, colonizer.signal, value)`: the exporter's own
/// families are per signal, so a test has to see the attribute, not just the metric's name.
fn pushed_points(req: &ExportMetricsServiceRequest) -> Vec<(&str, String, i64)> {
    use crate::proto::common::v1::any_value;
    use crate::proto::metrics::v1::{metric, number_data_point};
    let mut out = Vec::new();
    for resource in &req.resource_metrics {
        for scope in &resource.scope_metrics {
            for m in &scope.metrics {
                let points = match m.data.as_ref() {
                    Some(metric::Data::Sum(s)) => &s.data_points,
                    Some(metric::Data::Gauge(g)) => &g.data_points,
                    _ => continue,
                };
                for p in points {
                    let signal = p
                        .attributes
                        .iter()
                        .find(|kv| kv.key == "colonizer.signal")
                        .and_then(|kv| kv.value.as_ref().and_then(|v| v.value.as_ref()))
                        .and_then(|v| match v {
                            any_value::Value::StringValue(s) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    if let Some(number_data_point::Value::AsInt(value)) = p.value.as_ref() {
                        out.push((m.name.as_str(), signal, *value));
                    }
                }
            }
        }
    }
    out
}

/// Every data point's start time. The series are cumulative, so one push's points all start
/// together and the push after it starts where the first did — or the backend sees a reset on
/// every interval.
fn pushed_start_times(req: &ExportMetricsServiceRequest) -> Vec<u64> {
    use crate::proto::metrics::v1::metric;
    let mut out = Vec::new();
    for resource in &req.resource_metrics {
        for scope in &resource.scope_metrics {
            for m in &scope.metrics {
                match m.data.as_ref() {
                    Some(metric::Data::Sum(s)) => out.extend(s.data_points.iter().map(|p| p.start_time_unix_nano)),
                    Some(metric::Data::Gauge(g)) => out.extend(g.data_points.iter().map(|p| p.start_time_unix_nano)),
                    Some(metric::Data::Histogram(h)) => out.extend(h.data_points.iter().map(|p| p.start_time_unix_nano)),
                    _ => {}
                }
            }
        }
    }
    out
}

#[tokio::test]
async fn metrics_are_pushed_from_the_gateway_ledger() {
    let root = root("metrics");
    let collector = Collector::start(ok()).await;
    append(
        &root.0.join("sessions/c0ffee12/gateway.jsonl"),
        &serde_json::json!({"type": "gateway_request", "ts": chrono_now(), "provider": "openrouter", "model": "qwen3-coder", "status": 200, "duration_ms": 120, "queue_ms": 1, "input_tokens": 10, "output_tokens": 5}),
    );
    let path = contract(&root, &collector.url, |s| s.stream_metrics = true);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    let metrics = collector.seen("/v1/metrics");
    assert_eq!(metrics.len(), 1);
    let first = ExportMetricsServiceRequest::decode(metrics[0].body.as_slice()).unwrap();
    let text = format!("{first:?}");
    for name in [
        "colonizer.gateway.requests",
        "colonizer.gateway.duration",
        "colonizer.colonies",
        "colonizer.queue.depth",
        // The exporter's own families: this tick's log export is accounted already, and every
        // discovered signal has a backlog series, a live zero included.
        "colonizer.observability.bytes",
        "colonizer.observability.last_success",
        "colonizer.observability.backlog",
    ] {
        assert!(text.contains(name), "{name} missing");
    }
    let points = pushed_points(&first);
    assert!(
        points
            .iter()
            .any(|(n, s, v)| *n == "colonizer.observability.bytes" && s == "logs" && *v > 0),
        "{points:?}"
    );
    assert!(
        points
            .iter()
            .any(|(n, s, v)| *n == "colonizer.observability.last_success" && s == "logs" && *v > 0),
        "{points:?}"
    );
    assert!(
        points
            .iter()
            .any(|(n, s, _)| *n == "colonizer.observability.backlog" && s == "metrics"),
        "{points:?}"
    );
    // The next interval's push carries the first one's success, and the same start.
    x.last_metrics = None;
    x.tick().await;
    let metrics = collector.seen("/v1/metrics");
    assert_eq!(metrics.len(), 2);
    let second = ExportMetricsServiceRequest::decode(metrics[1].body.as_slice()).unwrap();
    let points = pushed_points(&second);
    assert!(
        points
            .iter()
            .any(|(n, s, v)| *n == "colonizer.observability.bytes" && s == "metrics" && *v > 0),
        "{points:?}"
    );
    assert!(
        points
            .iter()
            .any(|(n, s, v)| *n == "colonizer.observability.last_success" && s == "metrics" && *v > 0),
        "{points:?}"
    );
    let mut starts = pushed_start_times(&first);
    starts.extend(pushed_start_times(&second));
    assert!(!starts.is_empty());
    assert!(starts.iter().all(|t| *t == starts[0]), "the cumulative stream never restarts");

    // Restarted, the series continue from what was committed rather than starting over.
    x.shutdown();
    let y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.aggregates.gateway.values().next().unwrap().requests, 1);
    assert!(
        y.aggregates.sent_bytes.values().all(|b| *b > 0),
        "the self-metrics survived the restart: {:?}",
        y.aggregates.sent_bytes
    );
}

#[tokio::test]
async fn a_failing_metrics_push_does_not_hold_up_logs() {
    let root = root("metrics-fail");
    // The first metrics push is refused; everything else, before and after, is accepted.
    let calls = Arc::new(Mutex::new(0u32));
    let counter = calls.clone();
    let collector = Collector::start(Arc::new(move |path, _| {
        if path == "/v1/metrics" {
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n == 1 {
                return (503, b"no".to_vec());
            }
        }
        (200, Vec::new())
    }))
    .await;
    harness(&root, "hello");
    let path = contract(&root, &collector.url, |s| s.stream_metrics = true);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();

    // The refused push holds nothing back: the tick answers TICK and the log line is delivered.
    assert_eq!(x.tick().await, TICK);
    assert_eq!(collector.accepted(), ["hello"]);
    assert_eq!(collector.seen("/v1/metrics").len(), 1, "the push was attempted");
    assert_eq!(x.aggregates.export_failures, 1, "the failure is counted");
    assert!(
        x.aggregates.dropped.is_empty(),
        "nothing was dropped for it: {:?}",
        x.aggregates.dropped
    );
    assert!(
        *x.aggregates.sent_bytes.get("logs").unwrap() > 0,
        "the log export is accounted"
    );
    assert!(x.aggregates.last_success.contains_key("logs"));
    assert_eq!(committed_offset(&root), Some(file_size(&root)), "the cursors still commit");

    // The next interval's push succeeds, and its own accounting lands with it.
    x.last_metrics = None;
    assert_eq!(x.tick().await, TICK);
    assert!(
        *x.aggregates.sent_bytes.get("metrics").unwrap() > 0,
        "the push is accounted too"
    );
    assert!(x.aggregates.last_success.contains_key("metrics"));
    assert_eq!(x.aggregates.export_failures, 1, "a failed push is superseded, never retried");
    assert_eq!(collector.seen("/v1/logs").len(), 1, "the log line was never held or resent");

    // A restart resends nothing: the cursors of the acknowledged line are committed.
    x.shutdown();
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    y.tick().await;
    assert_eq!(collector.accepted(), ["hello"]);

    // The per-signal backlog reaches the status page: a line written and not yet read is logs'.
    harness(&root, "written but unread");
    y.write_status(true);
    let status: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(status_path(&root.0)).unwrap()).unwrap();
    assert!(status["signals"]["logs"]["backlog_bytes"].as_u64().unwrap() > 0, "{status}");
    assert_eq!(
        status["backlog_bytes"], status["signals"]["logs"]["backlog_bytes"],
        "{status}"
    );
}

/// A restart between flushes must not rewind the sums the backend already saw: with the ledgers
/// quiet, only the interval's pushes touch the aggregates, so their own accounting has to reach
/// `state.json` on its own — not wait for a flush that has nothing to commit.
#[tokio::test]
async fn the_pushs_own_accounting_survives_a_restart_with_nothing_to_flush() {
    let root = root("metrics-restart");
    let collector = Collector::start(ok()).await;
    harness(&root, "hello");
    let path = contract(&root, &collector.url, |s| s.stream_metrics = true);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, TICK);
    // A second interval passes with no new lines: the only change is the push's own accounting.
    x.last_metrics = None;
    assert_eq!(x.tick().await, TICK);
    assert_eq!(collector.seen("/v1/metrics").len(), 2, "two pushes went out");
    let start = x.aggregates.start_unix_nanos;
    let logs = x.aggregates.sent_bytes.get("logs").copied().unwrap();
    let metrics = x.aggregates.sent_bytes.get("metrics").copied().unwrap();
    assert!(logs > 0, "{:?}", x.aggregates.sent_bytes);
    assert!(metrics > 0, "{:?}", x.aggregates.sent_bytes);
    x.shutdown();
    let y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.aggregates.start_unix_nanos, start, "the cumulative stream does not restart");
    assert_eq!(y.aggregates.sent_bytes.get("logs"), Some(&logs), "the flush's bytes survive");
    assert_eq!(
        y.aggregates.sent_bytes.get("metrics"),
        Some(&metrics),
        "the pushes' own bytes were committed, not only the flush's: {:?}",
        y.aggregates.sent_bytes
    );
    assert_eq!(
        y.aggregates.last_success.get("metrics"),
        x.aggregates.last_success.get("metrics"),
        "the last push's success survives too"
    );
}

#[tokio::test]
async fn headers_are_sent_but_never_logged() {
    let root = root("headers");
    let secret = format!("hc-{}", crate::testkit::unique());
    // A backend that echoes the credential back in its error must not get it into status.json.
    let echo = secret.clone();
    let collector = Collector::start(Arc::new(move |_, _| (401, format!("bad key {echo}").into_bytes()))).await;
    harness(&root, "hello");
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, vec![("x-honeycomb-team".into(), secret.clone())]).unwrap();
    x.tick().await;
    x.write_status(true);

    let seen = collector.seen("/v1/logs");
    assert_eq!(seen[0].headers.get("x-honeycomb-team").unwrap(), secret.as_str());
    assert_eq!(seen[0].headers.get("content-type").unwrap(), "application/x-protobuf");
    let status = std::fs::read_to_string(status_path(&root.0)).unwrap();
    assert!(status.contains("401"), "{status}");
    assert!(
        !status.contains(&secret),
        "the header value leaked into status.json: {status}"
    );
    assert!(!format!("{:?}", x.transport).contains(&secret));
    let state = std::fs::read_to_string(crate::state::state_file(&root.0)).unwrap_or_default();
    assert!(!state.contains(&secret));
}

#[tokio::test]
async fn json_protocol_and_the_test_event() {
    let root = root("test-event");
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, |s| {
        s.protocol = "http/json".into();
        s.compression = "none".into();
    });
    let c = contract::load(&path).unwrap();
    let result = send_test(&c, Vec::new()).await;
    assert_eq!(result["ok"], true, "{result}");
    let logs = collector.seen("/v1/logs");
    assert_eq!(logs[0].headers.get("content-type").unwrap(), "application/json");
    let body: serde_json::Value = serde_json::from_slice(&logs[0].body).unwrap();
    assert_eq!(
        body.pointer("/resourceLogs/0/scopeLogs/0/logRecords/0/eventName"),
        Some(&serde_json::json!("colonizer.test"))
    );
    assert_eq!(collector.seen("/v1/metrics").len(), 1);

    // Nothing listening: the test says so instead of hanging or panicking.
    let dead = contract(&root, "http://127.0.0.1:9", |s| s.timeout_secs = 2);
    let result = send_test(&contract::load(&dead).unwrap(), Vec::new()).await;
    assert_eq!(result["ok"], false, "{result}");
}

#[test]
fn a_version_mismatch_is_refused() {
    let root = root("refused");
    let path = contract(&root, "http://127.0.0.1:9", |_| {});
    let mut c: Contract = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    c.mothership_version = "0.0.1".into();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    let err = Exporter::new(&path, Vec::new()).err().unwrap();
    assert!(err.starts_with("refused"), "{err}");
}

/// Lets the next tick past its backoff without sleeping.
fn skip_backoff(x: &mut Exporter) {
    x.backoff = x.backoff.map(|(_, d)| (Instant::now() - d, d));
}

fn file_size(root: &Root) -> u64 {
    std::fs::metadata(root.0.join("sessions/c0ffee12/harness.jsonl"))
        .unwrap()
        .len()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// A collector that answers `status` unless the request carries `x-api-key: good`, or `open` is set.
async fn guarded(status: u16, open: Arc<std::sync::atomic::AtomicBool>) -> Collector {
    Collector::start_full(Arc::new(move |_, headers, _| {
        let good = headers.get("x-api-key").is_some_and(|v| v == "good");
        if good || open.load(std::sync::atomic::Ordering::SeqCst) {
            (200, Vec::new(), Vec::new())
        } else {
            (status, Vec::new(), b"invalid api key".to_vec())
        }
    }))
    .await
}

#[tokio::test]
async fn a_401_keeps_every_record_and_delivers_them_after_the_key_is_fixed() {
    let root = root("auth401");
    let collector = guarded(401, Arc::default()).await;
    for msg in ["a", "b", "c", "d", "e"] {
        harness(&root, msg);
    }
    let path = contract(&root, &collector.url, |_| {});
    let wrong = "wrong-key-value".to_string();
    let mut x = Exporter::new(&path, vec![("x-api-key".into(), wrong.clone())]).unwrap();

    let wait = x.tick().await;
    assert!(wait >= Duration::from_millis(700), "backs off: {wait:?}");
    assert_eq!(x.status.state, "auth_failed");
    assert_eq!(x.status.signals["logs"].state, "auth_failed");
    assert_eq!(collector.seen("/v1/logs").len(), 1, "one request, never bisected");
    assert!(x.aggregates.dropped.is_empty(), "nothing dropped: {:?}", x.aggregates.dropped);
    assert_eq!(committed_offset(&root), None, "nothing committed");

    // Paused: an early tick sends nothing; after the backoff the same batch is tried again, whole.
    x.tick().await;
    assert_eq!(collector.seen("/v1/logs").len(), 1);
    skip_backoff(&mut x);
    harness(&root, "during the pause");
    x.tick().await;
    assert_eq!(collector.seen("/v1/logs").len(), 2);
    assert_eq!(x.status.consecutive_failures, 2);
    assert!(x.status.next_retry_unix.is_some());
    assert!(x.aggregates.dropped.is_empty());
    assert_eq!(committed_offset(&root), None);

    x.write_status(true);
    let status = std::fs::read_to_string(status_path(&root.0)).unwrap();
    let json: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(json["state"], "auth_failed");
    assert_eq!(json["endpoint"], collector.url.as_str());
    assert!(json["last_error"].as_str().unwrap().contains("401"), "{status}");
    assert!(json["backlog_bytes"].as_u64().unwrap() >= file_size(&root) - 1, "{status}");
    assert!(!status.contains(&wrong), "the header value leaked: {status}");

    // The operator fixes the key: the mothership restarts the add-on (here mid-pause, without a
    // clean shutdown) and every record, including the one written during the pause, arrives.
    drop(x);
    let mut y = Exporter::new(&path, vec![("x-api-key".into(), "good".into())]).unwrap();
    assert_eq!(y.tick().await, TICK);
    assert_eq!(
        sorted(collector.accepted()),
        ["a", "b", "c", "d", "during the pause", "e"],
        "zero loss, no duplicates among the accepted"
    );
    assert!(y.aggregates.dropped.is_empty(), "{:?}", y.aggregates.dropped);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    assert_eq!(y.status.state, "running");
}

#[tokio::test]
async fn a_403_holds_the_batch_and_resumes_once_the_backend_accepts_the_key() {
    let root = root("auth403");
    let open: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let collector = guarded(403, open.clone()).await;
    for msg in ["a", "b", "c"] {
        harness(&root, msg);
    }
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, vec![("x-api-key".into(), "stale".into())]).unwrap();
    x.tick().await;
    skip_backoff(&mut x);
    x.tick().await;
    assert_eq!(x.status.state, "auth_failed");
    assert_eq!(collector.seen("/v1/logs").len(), 2, "retried whole, never bisected");
    assert!(x.aggregates.dropped.is_empty());
    assert_eq!(committed_offset(&root), None);

    // The key gets its permission back; the same process delivers the held batch.
    open.store(true, std::sync::atomic::Ordering::SeqCst);
    skip_backoff(&mut x);
    assert_eq!(x.tick().await, TICK);
    assert_eq!(sorted(collector.accepted()), ["a", "b", "c"]);
    assert_eq!(x.status.state, "running");
    assert_eq!(x.status.signals["logs"].state, "ok");
    assert_eq!(x.status.consecutive_failures, 0);
    assert!(x.status.bytes_sent > 0);
    assert!(x.aggregates.dropped.is_empty());
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
}

#[tokio::test]
async fn retry_after_is_honoured_in_seconds_and_as_a_date() {
    let root = root("retry-after");
    let calls = Arc::new(Mutex::new(0u32));
    let counter = calls.clone();
    let collector = Collector::start_full(Arc::new(move |_, _, _| {
        let mut n = counter.lock().unwrap();
        *n += 1;
        match *n {
            1 => (429, vec![("retry-after", "3".to_string())], b"slow down".to_vec()),
            2 => {
                let at = SystemTime::now() + Duration::from_secs(8);
                let date = chrono::DateTime::<chrono::Utc>::from(at).format("%a, %d %b %Y %H:%M:%S GMT");
                (503, vec![("retry-after", date.to_string())], Vec::new())
            }
            _ => (200, Vec::new(), Vec::new()),
        }
    }))
    .await;
    harness(&root, "hello");
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();

    let wait = x.tick().await;
    assert!(
        wait >= Duration::from_secs(3) && wait <= Duration::from_millis(3100),
        "{wait:?}"
    );
    assert_eq!(x.status.state, "retrying");
    let early = x.tick().await;
    assert!(early > Duration::from_secs(2), "still waiting: {early:?}");
    assert_eq!(collector.seen("/v1/logs").len(), 1);

    skip_backoff(&mut x);
    let wait = x.tick().await;
    assert!(
        wait >= Duration::from_secs(6) && wait <= Duration::from_secs(9),
        "an HTTP-date: {wait:?}"
    );
    skip_backoff(&mut x);
    assert_eq!(x.tick().await, TICK);
    assert_eq!(collector.accepted(), ["hello"]);
    assert!(x.aggregates.dropped.is_empty());
}

#[tokio::test]
async fn a_partial_success_counts_the_rejected_and_keeps_the_message() {
    use crate::proto::collector::logs::v1::{ExportLogsPartialSuccess, ExportLogsServiceResponse};
    let root = root("partial");
    let collector = Collector::start(Arc::new(|_, _| {
        let reply = ExportLogsServiceResponse {
            partial_success: Some(ExportLogsPartialSuccess {
                rejected_log_records: 1,
                error_message: "record 2 has an invalid timestamp".into(),
            }),
        };
        (200, reply.encode_to_vec())
    }))
    .await;
    for msg in ["a", "b", "c"] {
        harness(&root, msg);
    }
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, TICK, "an ack is an ack: not retried");
    assert_eq!(collector.seen("/v1/logs").len(), 1);
    assert_eq!(x.aggregates.dropped.get("rejected"), Some(&1));
    let partial = x.status.last_partial_success.clone().unwrap();
    assert_eq!(partial.rejected, 1);
    assert_eq!(partial.message.as_deref(), Some("record 2 has an invalid timestamp"));
    assert_eq!(x.status.signals["logs"].records_sent, 2);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));

    // The JSON form, with the count as a string.
    let root = self::root("partial-json");
    let collector = Collector::start(Arc::new(|_, _| {
        (
            200,
            br#"{"partialSuccess":{"rejectedLogRecords":"2","errorMessage":"two too big"}}"#.to_vec(),
        )
    }))
    .await;
    harness(&root, "x");
    let path = contract(&root, &collector.url, |s| s.protocol = "http/json".into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    assert_eq!(x.aggregates.dropped.get("rejected"), Some(&2));
    assert_eq!(x.status.last_partial_success.unwrap().message.as_deref(), Some("two too big"));
}

#[tokio::test]
async fn an_outage_with_lines_appended_meanwhile_loses_nothing() {
    let root = root("outage");
    let up: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let collector = guarded(503, up.clone()).await;
    let mut expected = Vec::new();
    for i in 0..3 {
        harness(&root, &format!("before {i}"));
        expected.push(format!("before {i}"));
    }
    let path = contract(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    for tick in 0..4 {
        x.tick().await;
        skip_backoff(&mut x);
        harness(&root, &format!("during {tick}"));
        expected.push(format!("during {tick}"));
    }
    assert!(collector.accepted().is_empty());
    assert_eq!(committed_offset(&root), None);

    // A clean shutdown mid-outage commits nothing it was not acknowledged for.
    x.shutdown();
    assert_eq!(committed_offset(&root), None);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    up.store(true, std::sync::atomic::Ordering::SeqCst);
    for _ in 0..4 {
        skip_backoff(&mut x);
        x.tick().await;
    }
    expected.sort();
    assert_eq!(sorted(collector.accepted()), expected);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    assert!(x.aggregates.dropped.is_empty(), "{:?}", x.aggregates.dropped);
}

#[tokio::test]
async fn no_canary_reaches_the_wire() {
    let root = root("canary");
    let collector = Collector::start(ok()).await;
    let canaries = crate::testkit::Canaries::new();
    for canary in &canaries.all {
        harness(&root, &canary.text);
    }
    harness(&root, &canaries.content);
    let path = contract(&root, &collector.url, |s| s.stream_metrics = true);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    let seen = collector.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.path == "/v1/logs"));
    assert_eq!(collector.bodies().len(), canaries.all.len() + 1, "every line was exported");
    for request in &seen {
        crate::testkit::assert_absent(&request.body, canaries.secrets(), &request.path);
    }
}

/// A Loki contract over `url`: logs only, the push path completed from the base URL.
fn loki(root: &Root, url: &str, edit: impl FnOnce(&mut Settings)) -> PathBuf {
    contract(root, url, |s| {
        s.provider = "loki".into();
        s.url = url.to_string();
        s.stream_traces = false;
        s.stream_metrics = false;
        edit(s);
    })
}

#[tokio::test]
async fn loki_provider_pushes_json_streams() {
    let root = root("loki");
    // Loki answers 204 with an empty body; any 2xx commits.
    let collector = Collector::start(Arc::new(|_, _| (204, Vec::new()))).await;
    harness(&root, "hello loki");
    let path = loki(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, TICK);

    let seen = collector.seen("/loki/api/v1/push");
    assert_eq!(
        seen.len(),
        1,
        "one push, and nothing on the OTLP paths: {:?}",
        collector.seen.lock().unwrap()
    );
    assert_eq!(seen[0].headers.get("content-type").unwrap(), "application/json");
    // The contract keeps the default compression, so the push rides gzipped — the body the
    // collector records is the decompressed one.
    assert_eq!(seen[0].headers.get("content-encoding").unwrap(), "gzip");
    let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    let streams = body["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1, "{body}");
    assert_eq!(
        streams[0]["stream"],
        // The line is c0ffee12's, whose policy gives the org — the only way an org label appears.
        serde_json::json!({"level": "info", "org": "acme", "service_name": "colonizer-mothership", "stream": "operational"}),
        "{body}"
    );
    let values = streams[0]["values"].as_array().unwrap();
    assert_eq!(values.len(), 1, "{body}");
    let entry = values[0].as_array().unwrap();
    assert_eq!(entry[1], serde_json::json!("hello loki"), "{values:?}");
    assert!(
        entry[0].as_str().unwrap().chars().all(|c| c.is_ascii_digit()) && !entry[0].as_str().unwrap().is_empty(),
        "the timestamp is a nanosecond string: {values:?}"
    );
    assert_eq!(
        entry[2]["event_name"],
        serde_json::json!("colonizer.harness_log"),
        "{values:?}"
    );
    assert_eq!(committed_offset(&root), Some(file_size(&root)), "committed after the 204");
}

#[tokio::test]
async fn loki_provider_sends_tenant_and_operator_headers() {
    let root = root("loki-headers");
    let collector = Collector::start(ok()).await;
    harness(&root, "with a tenant");
    let path = loki(&root, &collector.url, |s| s.tenant_id = "acme-tenant".into());
    let mut x = Exporter::new(&path, vec![("x-custom".into(), "v".into())]).unwrap();
    assert_eq!(x.tick().await, TICK);
    let seen = collector.seen("/loki/api/v1/push");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].headers.get("x-scope-orgid").unwrap(), "acme-tenant");
    assert_eq!(seen[0].headers.get("x-custom").unwrap(), "v");

    // The operator's own header wins over the setting, whatever its case.
    harness(&root, "with the operator's header");
    let mut y = Exporter::new(&path, vec![("X-Scope-OrgID".into(), "operator-tenant".into())]).unwrap();
    assert_eq!(y.tick().await, TICK);
    let seen = collector.seen("/loki/api/v1/push");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].headers.get("x-scope-orgid").unwrap(), "operator-tenant");
    // Exactly one header of a kind: reqwest would overwrite it, so this also holds if it did not.
    assert_eq!(seen[1].headers.get_all("x-scope-orgid").iter().count(), 1);
}

#[tokio::test]
async fn no_canary_reaches_the_loki_wire() {
    let root = root("loki-canary");
    let collector = Collector::start(ok()).await;
    let canaries = crate::testkit::Canaries::new();
    for canary in &canaries.all {
        harness(&root, &canary.text);
    }
    harness(&root, &canaries.content);
    let path = loki(&root, &collector.url, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    let seen = collector.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one push carries the whole batch: {seen:?}");
    assert!(seen[0].path == "/loki/api/v1/push");
    let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    let lines: usize = body["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["values"].as_array().map(Vec::len).unwrap_or(0))
        .sum();
    assert_eq!(lines, canaries.all.len() + 1, "every line was exported: {body}");
    for request in &seen {
        crate::testkit::assert_absent(&request.body, canaries.secrets(), &request.path);
    }
}

/// The pane's "Send test event" runs `send-test-event` with the module's contract; a Loki one
/// must not fail on a metric point the provider has no path for.
#[tokio::test]
async fn a_loki_test_event_sends_the_log_line_only() {
    let root = root("loki-test-event");
    let collector = Collector::start(ok()).await;
    let path = loki(&root, &collector.url, |_| {});
    let result = send_test(&contract::load(&path).unwrap(), Vec::new()).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["signals"]["logs"]["ok"], true, "{result}");
    assert_eq!(result["signals"].as_object().unwrap().len(), 1, "logs only: {result}");
    assert_eq!(collector.seen("/loki/api/v1/push").len(), 1);
    assert_eq!(collector.seen("/v1/metrics").len(), 0, "a metric point has no Loki path");
}

#[test]
fn loki_destinations_hash_by_their_push_url_and_otlp_keeps_its_hash() {
    // A Loki contract's `endpoint` is empty; unless the push URL is the key, every Loki
    // destination hashes identically and a URL change shares cursor state with the old one.
    let loki = |url: &str| {
        destination_hash(&Contract {
            settings: Settings {
                provider: "loki".into(),
                url: url.into(),
                ..Settings::default()
            },
            ..Contract::default()
        })
    };
    assert_ne!(
        loki("http://loki-a:3100"),
        loki("http://loki-b:3100"),
        "two URLs, two destinations"
    );
    assert_eq!(
        loki("http://loki-a:3100"),
        loki("http://loki-a:3100/"),
        "a trailing slash is nothing"
    );
    assert_eq!(
        loki("http://loki-a:3100"),
        loki("http://loki-a:3100/loki/api/v1/push"),
        "a base and its full push URL are the same destination"
    );
    // The OTLP key is untouched, so an existing install keeps its cursors across the upgrade:
    // sha256("http://127.0.0.1:4318||http/protobuf"), first 8 bytes as hex.
    let otlp = destination_hash(&Contract {
        settings: Settings {
            endpoint: "http://127.0.0.1:4318".into(),
            ..Settings::default()
        },
        ..Contract::default()
    });
    assert_eq!(otlp, "f46ac179053b3768");
}

#[tokio::test]
async fn a_new_destination_starts_now_and_a_restart_never_skips_again() {
    let root = root("start-now");
    let collector = Collector::start(ok()).await;
    harness(&root, "history");
    append(
        &root.0.join("activity.jsonl"),
        &serde_json::json!({"seq": 1, "ts": chrono_now(), "kind": "colony.launch", "actor": "you"}),
    );
    let path = contract(&root, &collector.url, |s| s.start_from = contract::START_NOW.into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    assert!(collector.bodies().is_empty(), "nothing historical: {:?}", collector.bodies());

    harness(&root, "after start");
    x.tick().await;
    assert_eq!(collector.bodies(), ["after start"]);

    // Lines written while the add-on is down are delivered after a restart: the start is settled
    // once per destination, not on every start.
    x.shutdown();
    harness(&root, "while down");
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    y.tick().await;
    assert_eq!(collector.bodies(), ["after start", "while down"]);

    // A colony that appears later is read from its first line.
    std::fs::create_dir_all(root.0.join("sessions/beef0002")).unwrap();
    append(
        &root.0.join("sessions/beef0002/harness.jsonl"),
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "new colony", "ts": chrono_now()}),
    );
    let mut c: Contract = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    c.policy.insert("beef0002".into(), ColonyPolicy::default());
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    y.contract_mtime = None;
    y.tick().await;
    assert_eq!(collector.bodies(), ["after start", "while down", "new colony"]);
}

#[tokio::test]
async fn start_from_backlog_sends_what_the_ledgers_already_hold() {
    let root = root("start-backlog");
    let collector = Collector::start(ok()).await;
    harness(&root, "history");
    let path = contract(&root, &collector.url, |s| s.start_from = contract::START_BACKLOG.into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    assert_eq!(collector.bodies(), ["history"]);
}

#[tokio::test]
async fn an_existing_install_keeps_its_read_positions_when_start_from_arrives() {
    // State written by a build without `start_from`: cursors, no settled destinations. Upgrading
    // must not skip what that build had not read yet.
    let root = root("start-upgrade");
    let collector = Collector::start(ok()).await;
    harness(&root, "read before");
    let path = contract(&root, &collector.url, |s| s.start_from = contract::START_BACKLOG.into());
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    x.state.extra.remove(DESTINATIONS_KEY);
    x.shutdown();
    harness(&root, "unread at upgrade");
    let path = contract(&root, &collector.url, |s| s.start_from = contract::START_NOW.into());
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    y.tick().await;
    assert_eq!(collector.bodies(), ["read before", "unread at upgrade"]);
}

/// Every span the collector answered 2xx to, in order.
fn accepted_spans(collector: &Collector) -> Vec<crate::proto::trace::v1::Span> {
    collector
        .seen("/v1/traces")
        .into_iter()
        .filter(|s| (200..300).contains(&s.status))
        .filter_map(|s| crate::proto::collector::trace::v1::ExportTraceServiceRequest::decode(s.body.as_slice()).ok())
        .flat_map(|r| r.resource_spans)
        .flat_map(|r| r.scope_spans)
        .flat_map(|s| s.spans)
        .collect()
}

/// The subagent fixture as colony `c0ffee12`'s events.
fn colony_events() -> Vec<serde_json::Value> {
    crate::traces::tests::fixture("subagents-events.jsonl")
}

fn write_events(root: &Root, lines: &[serde_json::Value]) {
    for line in lines {
        append(&root.0.join("sessions/c0ffee12/events.jsonl"), line);
    }
}

fn merged(root: &Root) {
    append(
        &root.0.join("activity.jsonl"),
        &crate::traces::tests::outcome("c0ffee12", "merged", "2026-09-24T10:01:00Z"),
    );
}

/// Traces only, with the fixture's September timestamps inside the backlog window.
fn traces_only(s: &mut Settings) {
    s.stream_operational = false;
    s.stream_activity = false;
    s.max_backlog_days = 0;
}

#[tokio::test]
async fn a_long_colony_crossing_export_ticks_sends_each_span_once_and_the_root_last() {
    let root = root("traces-long");
    let collector = Collector::start(ok()).await;
    let events = colony_events();
    write_events(&root, &events[..10]);
    let path = contract(&root, &collector.url, traces_only);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, TICK);
    let first = accepted_spans(&collector);
    let names: Vec<&str> = first.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "execute_tool Read",
            "execute_tool Grep",
            "execute_tool Task",
            "subagent Explore"
        ],
        "closed spans leave at once; the open turn waits"
    );
    assert!(collector.seen("/v1/logs").is_empty(), "the log streams are off");

    // The next export interval, after a restart mid-turn: the open turn resumes from state.json.
    x.shutdown();
    drop(x);
    write_events(&root, &events[10..]);
    merged(&root);
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.tick().await, TICK);
    y.tick().await;
    let all = accepted_spans(&collector);
    let roots: Vec<_> = all.iter().filter(|s| s.parent_span_id.is_empty()).collect();
    assert_eq!(roots.len(), 1, "the root goes out once, last");
    assert!(all.last().unwrap().parent_span_id.is_empty());
    let ids: std::collections::BTreeSet<_> = all.iter().map(|s| s.span_id.clone()).collect();
    assert_eq!(ids.len(), all.len(), "no span sent twice");

    // One pass over the same lines, by the same builder, gives the same spans, ids and all.
    let colonies = BTreeMap::from([(
        "c0ffee12".to_string(),
        ColonyPolicy {
            org: "acme".into(),
            repo: "acme/widgets".into(),
            status: Some("running".into()),
            ..ColonyPolicy::default()
        },
    )]);
    let run = crate::traces::tests::Run {
        colonies: &colonies,
        policy: policy_for(&contract::load(&path).unwrap()),
        ratio: 1.0,
        max_trace_bytes: crate::traces::DEFAULT_MAX_TRACE_BYTES,
    };
    let builder = run.builder();
    let mut state = crate::traces::Traces::default();
    let mut once = Vec::new();
    for line in &events {
        let digest = crate::traces::tests::digest(line);
        builder.feed(
            &mut state,
            crate::policy::Source::Events,
            Some("c0ffee12"),
            line,
            &digest,
            &mut once,
        );
    }
    let merged_line = crate::traces::tests::outcome("c0ffee12", "merged", "2026-09-24T10:01:00Z");
    let digest = crate::traces::tests::digest(&merged_line);
    builder.feed(
        &mut state,
        crate::policy::Source::Activity,
        None,
        &merged_line,
        &digest,
        &mut once,
    );
    builder.settle(&mut state, &|_| true, &[], now_nanos(), &mut once);
    assert_eq!(all, crate::traces::tests::spans(&once));
    assert_eq!(y.status.signals["traces"].state, "ok");

    // A third process replays nothing.
    y.shutdown();
    let mut z = Exporter::new(&path, Vec::new()).unwrap();
    z.tick().await;
    assert_eq!(accepted_spans(&collector).len(), all.len());
}

#[tokio::test]
async fn a_zero_sample_ratio_drops_every_span_and_no_log_record() {
    let root = root("traces-sampled");
    let collector = Collector::start(ok()).await;
    write_events(&root, &colony_events());
    merged(&root);
    harness(&root, "still logged");
    let path = contract(&root, &collector.url, |s| {
        s.trace_sample_ratio = 0.0;
        s.max_backlog_days = 0;
    });
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    assert!(collector.seen("/v1/traces").is_empty(), "no span of an unsampled colony");
    let bodies = collector.bodies();
    assert!(bodies.contains(&"still logged".to_string()), "{bodies:?}");
    assert!(
        bodies.contains(&"turn_end".to_string()),
        "events are still logged: {bodies:?}"
    );

    let all = root_with_ratio("traces-all", 1.0).await;
    assert_eq!(all, 11, "ratio 1 keeps every span");
}

async fn root_with_ratio(tag: &str, ratio: f64) -> usize {
    let root = root(tag);
    let collector = Collector::start(ok()).await;
    write_events(&root, &colony_events());
    merged(&root);
    let path = contract(&root, &collector.url, |s| {
        traces_only(s);
        s.trace_sample_ratio = ratio;
    });
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    x.tick().await;
    accepted_spans(&collector).len()
}

#[tokio::test]
async fn a_refused_trace_credential_holds_the_spans_and_their_state() {
    let root = root("traces-401");
    let open: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let collector = guarded(401, open.clone()).await;
    write_events(&root, &colony_events());
    merged(&root);
    let path = contract(&root, &collector.url, traces_only);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    assert_eq!(x.status.state, "auth_failed");
    assert_eq!(x.status.signals["traces"].state, "auth_failed");
    assert!(
        x.traces.colonies.is_empty(),
        "the open-span state is not committed before the ack"
    );
    assert!(
        crate::traces::Traces::load(&State::load(&root.0).0.extra, &x.destination)
            .colonies
            .is_empty()
    );

    open.store(true, std::sync::atomic::Ordering::SeqCst);
    skip_backoff(&mut x);
    assert_eq!(x.tick().await, TICK);
    assert_eq!(accepted_spans(&collector).len(), 11, "every span, once the key works");
    let committed = crate::traces::Traces::load(&State::load(&root.0).0.extra, &x.destination);
    assert!(committed.colonies["c0ffee12"].root.emitted);
}

/// Issue #865's acceptance criterion — a push a real Loki accepts — against a pinned Loki 2.9:
/// start one (nothing here does it for you), then
/// `COLONIZER_TEST_LOKI_URL=http://127.0.0.1:13109 cargo test -p colonizer-observability
/// push_is_accepted_by_a_real_loki -- --ignored --nocapture` and read the marker back with
/// `curl -sG localhost:13109/loki/api/v1/query_range --data-urlencode 'query={service_name="colonizer"}'`.
/// Nothing runs without the variable, so CI never needs a Loki.
#[tokio::test]
#[ignore]
async fn push_is_accepted_by_a_real_loki() {
    let Ok(url) = std::env::var("COLONIZER_TEST_LOKI_URL") else {
        return;
    };
    if url.is_empty() {
        return;
    }
    use crate::proto::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
    use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs, SeverityNumber};
    use crate::proto::resource::v1::Resource;
    let kv = |key: &str, value: &str| KeyValue {
        key: key.into(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
        ..KeyValue::default()
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    let record = |offset: u64, body: &str| LogRecord {
        time_unix_nano: now + offset,
        observed_time_unix_nano: now + offset,
        severity_number: SeverityNumber::Info as i32,
        body: Some(AnyValue {
            value: Some(any_value::Value::StringValue(body.into())),
        }),
        ..LogRecord::default()
    };
    let request = Request::Logs(ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![kv("service.name", "colonizer")],
                ..Resource::default()
            }),
            scope_logs: vec![ScopeLogs {
                scope: Some(InstrumentationScope::default()),
                log_records: vec![
                    record(0, "loki_conformance_marker #865"),
                    record(1, "loki_conformance_marker #865 second line"),
                ],
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    });
    let settings = Settings {
        provider: "loki".into(),
        url,
        structured_metadata: true,
        ..Settings::default()
    };
    let transport = Transport::new(&settings, Path::new("."), Vec::new()).unwrap();
    match transport.send(&request).await {
        Outcome::Sent { rejected, .. } => assert_eq!(rejected, 0, "the push was accepted in full"),
        other => panic!("the real Loki did not accept the push: {other:?}"),
    }
}

/// A server that reads one request per connection, reports its arrival, and then holds the
/// connection open until released — the Collector cannot do this (its responder is synchronous),
/// and a request that hangs is what a stop must cancel.
struct Held {
    url: String,
    /// One entry per complete request that arrived, body only.
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    /// How many requests have arrived; a watch, so a test can await it.
    arrived: tokio::sync::watch::Receiver<u32>,
    release: tokio::sync::watch::Sender<bool>,
}

impl Held {
    async fn start() -> Held {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let (arrived_tx, arrived) = tokio::sync::watch::channel(0u32);
        let (release, release_rx) = tokio::sync::watch::channel(false);
        let seen = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let seen = seen.clone();
                let mut waiting = release_rx.clone();
                let tell = arrived_tx.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 16384];
                    // One request: the headers, then `content-length` bytes of body.
                    let body = loop {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        let Some(head) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&buf[..head]).to_ascii_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if buf.len() >= head + 4 + len {
                            break buf[head + 4..head + 4 + len].to_vec();
                        }
                    };
                    seen.lock().unwrap().push(body);
                    tell.send_modify(|n| *n += 1);
                    // Held open until the test says otherwise; the release latch stays set, so
                    // every later connection is answered at once.
                    while !*waiting.borrow_and_update() {
                        if waiting.changed().await.is_err() {
                            return;
                        }
                    }
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").await;
                });
            }
        });
        Held {
            url,
            requests,
            arrived,
            release,
        }
    }
}

/// The `run` loop of `main.rs` as it races the stop signal (#850): a stop that lands while a
/// request hangs cancels it — the loop leaves without an answer, the batch's cursor is never
/// committed, and a fresh exporter replays it, so every line gets out and none is lost. The
/// cancelled try may have reached the backend before the cancel, so delivery is at-least-once.
#[tokio::test]
async fn stop_cancels_a_request_still_in_flight_and_commits_nothing() {
    let root = root("cancel");
    let mut held = Held::start().await;
    for i in 0..3 {
        harness(&root, &format!("line {i}"));
    }
    // Uncompressed protobuf, so the recorded bodies decode without a gunzip step; the long
    // timeout is what makes "the stop was not awaited" observable.
    let path = contract(&root, &held.url, |s| {
        s.timeout_secs = 120;
        s.compression = "none".into();
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let x = Exporter::new(&path, Vec::new()).unwrap();
    let task = tokio::spawn(async move {
        let mut exporter = x;
        let mut stop_rx = stop_rx;
        loop {
            let wait = tokio::select! {
                wait = exporter.tick() => wait,
                _ = stop_rx.changed() => break,
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = stop_rx.changed() => break,
            }
        }
        exporter.shutdown();
    });

    // The request reaches the server, which never answers it.
    tokio::time::timeout(Duration::from_secs(2), held.arrived.changed())
        .await
        .expect("the request went out")
        .unwrap();
    assert_eq!(held.requests.lock().unwrap().len(), 1);

    // A stop now must break the loop at once — with the tick awaited rather than raced, this
    // would sit out the client's 120 s timeout and the 2 s deadline here would fail.
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert_eq!(committed_offset(&root), None, "the cancelled request committed nothing");
    assert_eq!(x_status_state(&root), "stopped", "a clean shutdown ran after the cancel");

    // Released, the same endpoint answers the replay; every line arrives, once acked.
    held.release.send(true).unwrap();
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.tick().await, TICK, "the replay is acked");
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    let requests = held.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "the cancelled try and the replay");
    for request in &requests {
        let decoded = ExportLogsServiceRequest::decode(request.as_slice()).unwrap();
        let bodies: Vec<&str> = decoded
            .resource_logs
            .iter()
            .flat_map(|r| &r.scope_logs)
            .flat_map(|s| &s.log_records)
            .filter_map(|r| match r.body.as_ref()?.value.as_ref()? {
                crate::proto::common::v1::any_value::Value::StringValue(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(bodies, ["line 0", "line 1", "line 2"], "each try carried the whole batch");
    }
}

/// The `state` of the exporter's last written status.json — set by the cancelled run's shutdown.
fn x_status_state(root: &Root) -> String {
    let bytes = std::fs::read(root.0.join("observability/status.json")).unwrap();
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_string()
}
