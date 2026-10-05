//! The exporter against a fake OTLP collector on 127.0.0.1: batching, retry with backoff, commit
//! only after an ack, resuming from the cursors after a restart, bisecting a refused request,
//! streams that are off, metrics, the test event, and headers that are sent but never logged.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use prost::Message;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// What the collector saw: the path, the headers, and the request decompressed.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

type Respond = Arc<dyn Fn(&str, &[u8]) -> (u16, Vec<u8>) + Send + Sync>;

struct Collector {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Collector {
    async fn start(respond: Respond) -> Collector {
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
                    let (status, reply) = respond(path, &raw);
                    log.lock().unwrap().push(Seen {
                        path: path.to_string(),
                        headers,
                        body: raw,
                    });
                    (StatusCode::from_u16(status).unwrap(), reply)
                }
            }
        };
        let app = axum::Router::new()
            .route("/v1/logs", axum::routing::post(handler("/v1/logs")))
            .route("/v1/metrics", axum::routing::post(handler("/v1/metrics")));
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
        self.seen("/v1/logs")
            .iter()
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
    let mut settings = Settings {
        endpoint: url.to_string(),
        stream_metrics: false,
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
    let delivered: std::collections::BTreeSet<_> = collector
        .seen("/v1/logs")
        .iter()
        .filter(|s| !String::from_utf8_lossy(&s.body).contains("poison"))
        .flat_map(|s| ExportLogsServiceRequest::decode(s.body.as_slice()).unwrap().resource_logs)
        .flat_map(|r| r.scope_logs)
        .flat_map(|s| s.log_records)
        .map(|r| format!("{:?}", r.body))
        .collect();
    assert_eq!(delivered.len(), 4, "every good record got through: {delivered:?}");
    assert_eq!(x.aggregates.dropped.get("refused"), Some(&1));
    assert!(committed_offset(&root).is_some(), "the cursor moves past the dropped record");
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
    assert_eq!(collector.bodies(), vec!["fresh".to_string()]);
    assert_eq!(x.aggregates.dropped.get("backlog"), Some(&1));
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
    let text = format!(
        "{:?}",
        crate::proto::collector::metrics::v1::ExportMetricsServiceRequest::decode(metrics[0].body.as_slice()).unwrap()
    );
    for name in [
        "colonizer.gateway.requests",
        "colonizer.gateway.duration",
        "colonizer.colonies",
        "colonizer.queue.depth",
    ] {
        assert!(text.contains(name), "{name} missing");
    }
    // Restarted, the series continue from what was committed rather than starting over.
    x.shutdown();
    let y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.aggregates.gateway.values().next().unwrap().requests, 1);
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
