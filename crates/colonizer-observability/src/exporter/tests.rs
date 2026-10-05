//! The exporter against a fake OTLP collector on 127.0.0.1: batching, retry with backoff, commit
//! only after an ack, resuming from the cursors after a restart, bisecting a refused request,
//! streams that are off, metrics, the test event, and headers that are sent but never logged.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
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
