//! The backfill engine (#858) against a fake OTLP collector and the v0.1.9 fixtures: every line
//! once and a clean stop, the live stream untouched beside it, the rate limit under a paused
//! clock, cancel, a dry run that counts a real run's numbers, the closed content gate, and a
//! restart that resumes from the backfill cursors.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::exporter::Exporter;
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use prost::Message;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

/// What the collector saw: the path and the request decompressed.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    body: Vec<u8>,
    status: u16,
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
                        body: raw,
                        status,
                    });
                    (StatusCode::from_u16(status).unwrap(), HeaderMap::new(), reply)
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

    /// Every accepted log record's `colonizer.record.id`, in order: deterministic, so a line the
    /// replay sends twice appears twice.
    fn accepted_ids(&self) -> Vec<String> {
        self.seen("/v1/logs")
            .into_iter()
            .filter(|s| (200..300).contains(&s.status))
            .filter_map(|s| ExportLogsServiceRequest::decode(s.body.as_slice()).ok())
            .flat_map(|r| r.resource_logs)
            .flat_map(|r| r.scope_logs)
            .flat_map(|s| s.log_records)
            .filter_map(|record| {
                let kv = record.attributes.iter().find(|kv| kv.key == "colonizer.record.id")?;
                match kv.value.as_ref()?.value.as_ref()? {
                    crate::proto::common::v1::any_value::Value::StringValue(s) => Some(s.clone()),
                    _ => None,
                }
            })
            .collect()
    }

    /// How many spans the collector accepted.
    fn span_count(&self) -> usize {
        self.seen("/v1/traces")
            .into_iter()
            .filter(|s| (200..300).contains(&s.status))
            .filter_map(|s| ExportTraceServiceRequest::decode(s.body.as_slice()).ok())
            .flat_map(|r| r.resource_spans)
            .flat_map(|r| r.scope_spans)
            .map(|s| s.spans.len())
            .sum()
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
    let dir = std::env::temp_dir().join(format!("colonizer-backfill-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

fn append(path: &Path, line: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn chrono_now() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339()
}

fn now_ts() -> String {
    chrono::DateTime::from_timestamp_nanos(now_nanos() as i64).to_rfc3339()
}

/// One harness log line of colony `c0ffee12`.
fn harness_line(message: &str) -> Value {
    serde_json::json!({"type": "harness_log", "origin": "host", "level": "info", "message": message, "ts": chrono_now()})
}

fn harness(root: &Root, message: &str) {
    append(&root.0.join("sessions/c0ffee12/harness.jsonl"), &harness_line(message));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// One colony `c0ffee12`, content export never opted in.
fn one_colony() -> BTreeMap<String, ColonyPolicy> {
    BTreeMap::from([(
        "c0ffee12".to_string(),
        ColonyPolicy {
            org: "acme".into(),
            repo: "acme/widgets".into(),
            status: Some("running".into()),
            ..ColonyPolicy::default()
        },
    )])
}

/// The v0.1.9 fixture plus the install-wide ledgers a v0.1.9 data dir also holds, and the
/// contract's colony list as the mothership builds it from `sessions.json`.
fn fixture(tag: &str) -> (Root, BTreeMap<String, ColonyPolicy>) {
    let root = root(tag);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../colonizer/tests/fixtures/data-v0.1.9");
    copy_dir(&fixture, &root.0);
    let ts = now_ts();
    for (file, line) in [
        (
            "activity.jsonl",
            serde_json::json!({"seq": 1, "ts": ts, "kind": "colony.launch", "actor": "you", "colony": "a1b2c3d4"}),
        ),
        (
            "spend.jsonl",
            serde_json::json!({"ts": ts, "kind": "turn", "session": "a1b2c3d4", "cost_usd": 0.1}),
        ),
        (
            "decisions.jsonl",
            serde_json::json!({"ts": ts, "point": "recovery.retry", "mode": "shadow", "options": 3}),
        ),
        (
            "routing.jsonl",
            serde_json::json!({"ts": ts, "point": "routing.tier", "pick": "low"}),
        ),
        (
            "jev_ladder.jsonl",
            serde_json::json!({"ts": ts, "kind": "decision", "tool": "Read", "action": "keep"}),
        ),
        (
            "jev_focus.jsonl",
            serde_json::json!({"ts": ts, "kind": "focus", "session": "a1b2c3d4", "candidates": 2}),
        ),
        (
            "logs/mothership.jsonl",
            serde_json::json!({"ts": ts, "level": "info", "target": "colonizer::queue", "message": "slot freed"}),
        ),
    ] {
        append(&root.0.join(file), &line);
    }
    let sessions: Vec<Value> = serde_json::from_slice(&std::fs::read(root.0.join("sessions.json")).unwrap()).unwrap();
    let colonies: BTreeMap<String, ColonyPolicy> = sessions
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_string(),
                serde_json::from_value(s.clone()).unwrap(),
            )
        })
        .collect();
    (root, colonies)
}

/// Writes the contract for `url` and returns its path. `start_from = now` is the live default the
/// backfill runs beside; the backfill's own start is its bounds, not this setting.
fn contract(root: &Root, url: &str, colonies: &BTreeMap<String, ColonyPolicy>, edit: impl FnOnce(&mut Settings)) -> PathBuf {
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
        policy: colonies.clone(),
        relay_members: Vec::new(),
    };
    let path = contract::path(&root.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}

/// Drives ticks to completion, waiting as long as each one asks.
async fn run_to_done(b: &mut Backfill) {
    while !b.done() {
        let wait = b.tick().await;
        tokio::time::sleep(wait).await;
    }
}

fn n(summary: &Value, key: &str) -> u64 {
    summary[key]
        .as_u64()
        .unwrap_or_else(|| panic!("{key} is not a number: {summary}"))
}

#[tokio::test]
async fn a_backfill_over_the_v019_fixtures_sends_every_line_once_and_stops() {
    let (root, colonies) = fixture("fixtures");
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &colonies, |_| {});
    let mut b = Backfill::new(&path, Vec::new(), &Options::default()).unwrap();
    run_to_done(&mut b).await;
    assert!(b.done());
    let summary = b.summary();
    assert_eq!(summary["state"], "complete");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["dry_run"], false);
    assert_eq!(summary["window"]["since"], serde_json::json!(null));
    assert_eq!(summary["window"]["until"], serde_json::json!(null));
    assert_eq!(summary["repo"], serde_json::json!(null));
    assert_eq!(summary["error"], serde_json::json!(null));
    assert_eq!(summary["sources_done"], summary["sources_total"]);
    assert_eq!(summary["skipped"]["malformed"], serde_json::json!(0));
    assert_eq!(summary["skipped"]["out_of_window"], serde_json::json!(0));
    assert_eq!(summary["skipped"]["refused"], serde_json::json!(0));

    // The fixture's five colony lines and the appendix's seven install lines, each exactly once:
    // the record ids are deterministic, so a line the replay crossed would appear twice.
    let ids = collector.accepted_ids();
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "no record sent twice");
    assert_eq!(ids.len(), 12, "{ids:?}");
    assert_eq!(n(&summary, "records_sent"), 12);
    assert_eq!(summary["per_signal"]["logs"]["records"], serde_json::json!(12));
    assert!(summary["per_signal"]["logs"]["ledger_bytes"].as_u64().unwrap() > 0);
    assert!(summary["per_signal"]["logs"]["encoded_bytes"].as_u64().unwrap() > 0);
    assert!(summary["bytes_sent"].as_u64().unwrap() > 0);
    // No outcome line closes a colony, so the replay of the traces sources builds no span.
    assert_eq!(summary["per_signal"]["traces"]["records"], serde_json::json!(0));
    assert!(summary["per_signal"]["traces"]["ledger_bytes"].as_u64().unwrap() > 0);

    // Both signals read the ledgers they tail — `events` and `activity` once per signal — so the
    // per-org tally counts line reads, not records: five colony lines plus the three events lines
    // read again for traces, and seven install lines plus `activity` read again for traces.
    assert_eq!(summary["per_org"]["acme"]["records"], serde_json::json!(8));
    assert_eq!(summary["per_org"]["(install)"]["records"], serde_json::json!(8));
    let org_bytes: u64 = ["acme", "(install)"]
        .iter()
        .map(|org| summary["per_org"][org]["ledger_bytes"].as_u64().unwrap())
        .sum();
    let signal_bytes = summary["per_signal"]["logs"]["ledger_bytes"].as_u64().unwrap()
        + summary["per_signal"]["traces"]["ledger_bytes"].as_u64().unwrap();
    assert_eq!(org_bytes, signal_bytes);

    // A second run of the same completed plan sends nothing.
    let before = collector.accepted_ids().len();
    let mut again = Backfill::new(&path, Vec::new(), &Options::default()).unwrap();
    run_to_done(&mut again).await;
    assert_eq!(again.summary()["state"], "complete");
    assert_eq!(n(&again.summary(), "records_sent"), 12, "the counters it adopted");
    assert_eq!(collector.accepted_ids().len(), before, "nothing is replayed");
}

#[tokio::test]
async fn live_records_keep_flowing_while_backfill_runs() {
    let (root, colonies) = fixture("live");
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &colonies, |_| {});
    // The live exporter settles the destination first: `start_from = now` binds every existing
    // file to its end, which makes those positions the backfill's bounds.
    let mut live = Exporter::new(&path, Vec::new()).unwrap();
    live.tick().await;
    assert!(collector.bodies().is_empty(), "the live stream sends no history");
    let state_before = std::fs::read(crate::state::state_file(&root.0)).unwrap();

    // Two lines land while the backfill runs: they are past every bound, the live stream's own.
    for i in 0..2 {
        append(
            &root.0.join("sessions/e5f60718/harness.jsonl"),
            &harness_line(&format!("live {i}")),
        );
    }
    let mut b = Backfill::new(&path, Vec::new(), &Options::default()).unwrap();
    run_to_done(&mut b).await;
    assert_eq!(b.summary()["state"], "complete");
    assert_eq!(n(&b.summary(), "records_sent"), 12, "the history, never the live lines");
    assert_eq!(
        std::fs::read(crate::state::state_file(&root.0)).unwrap(),
        state_before,
        "the backfill never writes the live state"
    );

    // The live stream keeps flowing over its own half of the files.
    live.tick().await;
    let bodies = collector.bodies();
    assert_eq!(bodies.len(), 14, "{bodies:?}");
    assert_eq!(&bodies[12..], ["live 0", "live 1"]);
}

#[tokio::test(start_paused = true)]
async fn the_rate_limit_holds_under_a_paused_clock() {
    let root = root("paused");
    for i in 0..1200 {
        harness(&root, &format!("line {i}"));
    }
    // A dead endpoint proves the point: a dry run never opens a connection.
    let path = contract(&root, "http://127.0.0.1:9", &one_colony(), |_| {});
    let mut b = Backfill::new(
        &path,
        Vec::new(),
        &Options {
            rate: 500,
            dry_run: true,
            ..Options::default()
        },
    )
    .unwrap();
    let mut per_tick = Vec::new();
    while !b.done() {
        let wait = b.tick().await;
        per_tick.push(n(&b.progress(), "records_sent"));
        if wait > Duration::ZERO {
            tokio::time::advance(wait).await;
        }
    }
    assert_eq!(per_tick, vec![500, 1000, 1200], "one rate bucket a second, the tail shorter");
    assert_eq!(b.summary()["state"], "complete");
    assert_eq!(n(&b.summary(), "records_sent"), 1200);
    assert!(
        !progress_path(&root.0).exists(),
        "a dry run writes nothing, not even backfill.json"
    );
}

#[tokio::test]
async fn cancel_stops_within_one_tick() {
    let root = root("cancel");
    for i in 0..1500 {
        harness(&root, &format!("line {i}"));
    }
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &one_colony(), |_| {});
    let mut b = Backfill::new(
        &path,
        Vec::new(),
        &Options {
            rate: 500,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(b.tick().await, TICK);
    assert_eq!(n(&b.progress(), "records_sent"), 500);
    // Let the bucket refill, then one more bounded pass before the cancel.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    b.tick().await;
    assert_eq!(n(&b.progress(), "records_sent"), 1000);
    assert!(!b.done());

    b.cancel();
    assert_eq!(b.tick().await, Duration::ZERO, "the cancel lands on the very next tick");
    assert!(b.done());
    let summary = b.summary();
    assert_eq!(summary["state"], "cancelled");
    assert_eq!(summary["ok"], true);
    assert_eq!(n(&summary, "records_sent"), 1000, "only what was acknowledged");

    // Nothing more ever goes out, and the stop is recorded for the next run to resume from.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(b.tick().await, Duration::ZERO);
    assert_eq!(collector.accepted_ids().len(), 1000);
    let progress = std::fs::read_to_string(progress_path(&root.0)).unwrap();
    assert!(progress.contains("\"cancelled\""), "{progress}");
    assert_eq!(n(&b.summary(), "records_sent"), 1000);
}

#[tokio::test]
async fn a_dry_run_counts_what_a_real_run_sends() {
    let (root, colonies) = fixture("dry");
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &colonies, |_| {});
    let mut dry = Backfill::new(
        &path,
        Vec::new(),
        &Options {
            dry_run: true,
            ..Options::default()
        },
    )
    .unwrap();
    run_to_done(&mut dry).await;
    let dry_summary = dry.summary();
    assert_eq!(dry_summary["state"], "complete");
    assert_eq!(dry_summary["dry_run"], true);
    assert!(!progress_path(&root.0).exists(), "a dry run writes no file");
    assert!(collector.seen.lock().unwrap().is_empty(), "and sends nothing");

    let mut real = Backfill::new(&path, Vec::new(), &Options::default()).unwrap();
    run_to_done(&mut real).await;
    let real_summary = real.summary();
    assert_eq!(real_summary["dry_run"], false);
    for key in ["records", "ledger_bytes", "encoded_bytes"] {
        assert_eq!(
            dry_summary["per_signal"]["logs"][key], real_summary["per_signal"]["logs"][key],
            "per_signal.{key}"
        );
    }
    assert_eq!(dry_summary["per_org"], real_summary["per_org"]);
    assert_eq!(dry_summary["records_sent"], real_summary["records_sent"]);
    assert_eq!(collector.accepted_ids().len() as u64, n(&real_summary, "records_sent"));
    assert_eq!(
        collector.span_count() as u64,
        real_summary["per_signal"]["traces"]["records"].as_u64().unwrap_or(0)
    );
}

#[tokio::test]
async fn a_colony_without_content_opt_in_never_leaks_a_canary() {
    let root = root("canary");
    let canaries = crate::testkit::Canaries::new();
    for canary in &canaries.all {
        harness(&root, &canary.text);
    }
    harness(&root, &canaries.content);
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &one_colony(), |_| {});
    let mut b = Backfill::new(&path, Vec::new(), &Options::default()).unwrap();
    run_to_done(&mut b).await;
    assert_eq!(b.summary()["state"], "complete");
    let seen = collector.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.path == "/v1/logs"));
    assert_eq!(collector.bodies().len(), canaries.all.len() + 1, "every line was replayed");
    for request in &seen {
        crate::testkit::assert_absent(&request.body, canaries.secrets(), &request.path);
    }
}

#[tokio::test]
async fn a_restart_mid_backfill_resumes_from_the_backfill_cursors() {
    let root = root("resume");
    let messages: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
    for message in &messages {
        harness(&root, message);
    }
    let collector = Collector::start(ok()).await;
    let path = contract(&root, &collector.url, &one_colony(), |_| {});
    let mut first = Backfill::new(
        &path,
        Vec::new(),
        &Options {
            rate: 2,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(first.tick().await, TICK);
    assert_eq!(n(&first.progress(), "records_sent"), 2);
    let progress = std::fs::read_to_string(progress_path(&root.0)).unwrap();
    assert!(
        progress.contains("\"running\""),
        "the crash leaves a running state behind: {progress}"
    );
    drop(first); // a crash, not a cancel: nothing is cleaned up

    let mut second = Backfill::new(
        &path,
        Vec::new(),
        &Options {
            rate: 2,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(n(&second.progress(), "records_sent"), 2, "the counters resume too");
    run_to_done(&mut second).await;
    let summary = second.summary();
    assert_eq!(summary["state"], "complete");
    assert_eq!(n(&summary, "records_sent"), 10);
    let mut bodies = collector.bodies();
    let mut expected = messages.clone();
    bodies.sort();
    expected.sort();
    assert_eq!(bodies, expected, "every line exactly once across the restart");
}

#[test]
fn options_parse_the_flags_and_the_date_grammar() {
    let args = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
    let day_start = chrono::NaiveDate::from_ymd_opt(2026, 9, 24)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_nanos_opt()
        .and_then(|n| u64::try_from(n).ok());
    let day_end = chrono::NaiveDate::from_ymd_opt(2026, 9, 23)
        .unwrap()
        .and_hms_milli_opt(23, 59, 59, 999)
        .unwrap()
        .and_utc()
        .timestamp_nanos_opt()
        .and_then(|n| u64::try_from(n).ok());
    let opts = Options::from_args(&args(&[
        "--contract",
        "/tmp/exporter.json",
        "--since",
        "2026-09-24",
        "--until",
        "2026-09-23",
        "--repo",
        "acme/widgets",
        "--rate",
        "250",
        "--dry-run",
    ]))
    .unwrap();
    assert_eq!(opts.since, day_start, "a bare day starts at midnight");
    assert_eq!(opts.until, day_end, "and ends at the day's end");
    assert_eq!(opts.repo.as_deref(), Some("acme/widgets"));
    assert_eq!(opts.rate, 250);
    assert!(opts.dry_run);

    let full = Options::from_args(&args(&[
        "--since",
        "2026-09-24T09:31:10Z",
        "--until",
        "2026-09-24T10:00:00+02:00",
    ]))
    .unwrap();
    let utc = chrono::DateTime::parse_from_rfc3339("2026-09-24T09:31:10Z")
        .unwrap()
        .timestamp_nanos_opt()
        .and_then(|n| u64::try_from(n).ok());
    assert_eq!(full.since, utc, "RFC 3339 and a naive UTC second parse alike");
    assert_eq!(
        Options::from_args(&args(&["--repo", "r"])).unwrap().repo.as_deref(),
        Some("r")
    );

    for bad in [
        vec!["--since"],              // a value short
        vec!["--wat", "x"],           // an unknown flag
        vec!["--rate", "fast"],       // not a number
        vec!["--since", "yesterday"], // not a date
    ] {
        assert!(Options::from_args(&args(&bad)).is_err(), "{bad:?}");
    }
    assert_eq!(
        Options::from_args(&[]).unwrap().rate,
        DEFAULT_RATE,
        "no --rate is the default"
    );
}
