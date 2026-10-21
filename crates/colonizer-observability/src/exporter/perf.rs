//! The performance acceptance tests of #849, ignored by default so CI never runs them; the numbers
//! go into the pull request instead. Each prints what it measured. Run them with
//! `cargo test -p colonizer-observability --locked --release -- --ignored --nocapture <name>`
//! (a release build measures the shipped throughput; a debug build passes too, it only drains
//! slower — the mapping, not the loop, is what the optimizer buys).
//!
//! - [`a_one_gib_backlog_drains_within_the_read_cap_in_bounded_memory`]: a synthetic backlog of
//!   about 1 GiB drains at (or under) the configured `max_read_mib_per_sec`, in bounded memory,
//!   with every line accounted for.
//! - [`event_appends_stay_fast_while_the_exporters_receiver_hangs`]: the write path's per-append
//!   latency, with the exporter parked on a receiver that accepts and never answers.

use super::*;
use crate::contract::{ColonyPolicy, Settings};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use prost::Message;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A collector that answers 200 to everything and counts the log records it accepted, keeping
/// none of them: a 1 GiB drain may not be held a second time, in the test, once the exporter has
/// let go of it.
struct Counter {
    url: String,
    records: Arc<AtomicU64>,
    requests: Arc<AtomicU64>,
}

impl Counter {
    async fn start() -> Counter {
        let records = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(AtomicU64::new(0));
        let counted = records.clone();
        let seen = requests.clone();
        let logs = move |headers: HeaderMap, body: Bytes| {
            let counted = counted.clone();
            let seen = seen.clone();
            async move {
                let mut raw = body.to_vec();
                if headers.get("content-encoding").is_some_and(|v| v == "gzip") {
                    let mut out = Vec::new();
                    flate2::read::GzDecoder::new(raw.as_slice()).read_to_end(&mut out).unwrap();
                    raw = out;
                }
                let n = ExportLogsServiceRequest::decode(raw.as_slice())
                    .map(|r| {
                        r.resource_logs
                            .iter()
                            .flat_map(|l| l.scope_logs.iter())
                            .map(|s| s.log_records.len() as u64)
                            .sum()
                    })
                    .unwrap_or(0);
                counted.fetch_add(n, Ordering::Relaxed);
                seen.fetch_add(1, Ordering::Relaxed);
                (StatusCode::OK, "")
            }
        };
        let ok = move |_headers: HeaderMap, _body: Bytes| async { (StatusCode::OK, "") };
        let app = axum::Router::new()
            .route("/v1/logs", axum::routing::post(logs))
            .route("/v1/traces", axum::routing::post(ok))
            .route("/v1/metrics", axum::routing::post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Counter { url, records, requests }
    }
}

/// A collector that accepts a request, reads its body, and then never answers, so the exporter's
/// send truly hangs (its timeout is set generously for the run). `hits` counts the requests it is
/// holding; sending `true` on the returned channel lets them all go.
async fn hanging_collector() -> (String, Arc<AtomicU64>, tokio::sync::watch::Sender<bool>) {
    let hits = Arc::new(AtomicU64::new(0));
    let (release, released) = tokio::sync::watch::channel(false);
    let handler = move |hits: Arc<AtomicU64>, release: tokio::sync::watch::Receiver<bool>| {
        move |_headers: HeaderMap, _body: Bytes| {
            let hits = hits.clone();
            let mut release = release.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                while !*release.borrow_and_update() {
                    if release.changed().await.is_err() {
                        break;
                    }
                }
                (StatusCode::OK, "")
            }
        }
    };
    let app = axum::Router::new()
        .route("/v1/logs", axum::routing::post(handler(hits.clone(), released.clone())))
        .route("/v1/traces", axum::routing::post(handler(hits.clone(), released.clone())))
        .route("/v1/metrics", axum::routing::post(handler(hits.clone(), released)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, hits, release)
}

/// A temp dir removed on drop, panic included.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-exporter-perf-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(dir.join("sessions/c0ffee12")).unwrap();
    Root(dir)
}

/// Writes the contract for `url`: the backlog is read from the start, metrics are off, and `edit`
/// sets the rest.
fn contract(root: &Root, url: &str, edit: impl FnOnce(&mut Settings)) -> PathBuf {
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

/// The process's peak RSS so far, in KiB: `VmHWM` of `/proc/self/status`. The high-water mark never
/// falls, so a transient peak between two reads is still seen by the later one.
fn peak_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .expect("a VmHWM line")
}

/// The bytes free on `dir`'s filesystem, from `df -k <dir>`: a std shell-out, like the sibling
/// tests' `ps`. `None` when it cannot be read, in which case the caller assumes there is room.
fn free_bytes(dir: &Path) -> Option<u64> {
    let out = std::process::Command::new("df").arg("-k").arg(dir).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains('%'))?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    fields.get(3)?.parse::<u64>().ok().map(|kib| kib * 1024)
}

/// Writes harness lines of about `record_bytes` until `target_bytes` is reached: one pre-rendered
/// template, its id and timestamp varied per line, through a buffered writer. The timestamps start
/// twelve hours ago and advance a millisecond a line, so every line sits inside the backlog window
/// and none may be dropped by the backlog guard. Returns the lines and bytes written.
fn write_backlog(path: &Path, target_bytes: u64, record_bytes: usize) -> (u64, u64) {
    let filler = "x".repeat(record_bytes.saturating_sub(200));
    let mut w = std::io::BufWriter::with_capacity(1024 * 1024, std::fs::File::create(path).unwrap());
    let base = SystemTime::now() - Duration::from_secs(12 * 3600);
    let (mut lines, mut bytes) = (0u64, 0u64);
    while bytes < target_bytes {
        let at = base + Duration::from_millis(lines);
        let ts = chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339();
        // Rendered by hand, not through serde_json, so a 1 GiB backlog renders in seconds.
        let line = format!(
            "{{\"type\":\"harness_log\",\"origin\":\"host\",\"level\":\"info\",\"message\":\"drain-{lines:09}-{filler}\",\"ts\":\"{ts}\"}}\n"
        );
        w.write_all(line.as_bytes()).unwrap();
        bytes += line.len() as u64;
        lines += 1;
    }
    w.flush().unwrap();
    (lines, bytes)
}

/// Appends `n` event lines of about 4 KiB, one open-append-write at a time as the tests write a
/// ledger, and returns each append's latency in microseconds. The mothership's `handle_agent_event`
/// is not callable from this crate; this is the write path's stand-in.
fn append_timed(path: &Path, n: usize) -> Vec<f64> {
    let filler = "e".repeat(4096);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ts = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();
        let line = format!("{{\"type\":\"user_message\",\"id\":\"m{i:05}\",\"text\":\"{filler}\",\"ts\":\"{ts}\"}}\n");
        let at = Instant::now();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(line.as_bytes()).unwrap();
        drop(f);
        out.push(at.elapsed().as_secs_f64() * 1_000_000.0);
    }
    out
}

/// The p50 and p99 of `samples`, nearest rank.
fn p50_p99(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    let at = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    (at(0.5), at(0.99))
}

/// #849's first performance criterion: a backlog of about 1 GiB (512 MiB when the temp filesystem
/// holds less than 2.5 GiB free) drains at or under the configured `max_read_mib_per_sec`, the
/// drain's peak RSS stays within 64 MiB of its steady state, and every line is accounted for: on
/// the wire, or counted where it was let go.
#[tokio::test]
#[ignore]
async fn a_one_gib_backlog_drains_within_the_read_cap_in_bounded_memory() {
    const CAP_MIB: u64 = 16;
    let root = root("drain");
    let free = free_bytes(&std::env::temp_dir()).unwrap_or(u64::MAX);
    let target = if free >= 2_684_354_560 {
        1024 * 1024 * 1024
    } else {
        println!(
            "only {} MiB free under the temp dir: draining 512 MiB instead of 1 GiB",
            free / 1024 / 1024
        );
        512 * 1024 * 1024
    };
    let ledger = root.0.join("sessions/c0ffee12/harness.jsonl");
    let (lines, bytes) = write_backlog(&ledger, target, 4096);

    let collector = Counter::start().await;
    let path = contract(&root, &collector.url, |s| s.max_read_mib_per_sec = CAP_MIB);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();

    // The warm-up tick: its read, batch and send allocate what every tick allocates, so the
    // high-water mark starts from the drain's steady state rather than the test harness. The clock
    // covers the whole drain, this tick included.
    let started = Instant::now();
    let wait = x.tick().await;
    let baseline = peak_rss_kib();
    tokio::time::sleep(wait).await;

    let key = CursorKey {
        destination_hash: x.destination.clone(),
        signal: Signal::Logs,
        relative_path: "sessions/c0ffee12/harness.jsonl".into(),
    };
    let len = std::fs::metadata(&ledger).unwrap().len();
    let deadline = Instant::now() + Duration::from_secs(280);
    loop {
        let cursor = x.state.cursor(&key);
        if cursor.file_id.is_some() && cursor.offset >= len {
            break;
        }
        assert!(Instant::now() < deadline, "the drain did not finish in 280 s");
        tokio::time::sleep(x.tick().await).await;
    }
    let wall = started.elapsed();
    let peak = peak_rss_kib();
    let growth = peak.saturating_sub(baseline) as f64 / 1024.0;

    // Every line is accounted for: delivered, or dropped and counted (a drop would also be one
    // `export_gap` record per run on the wire, which is why `gaps` joins the count).
    let delivered = collector.records.load(Ordering::Relaxed);
    let dropped: u64 = x.aggregates.dropped.values().sum();
    let gaps = x.status.signals["logs"].gaps;
    assert_eq!(
        delivered + dropped,
        lines + gaps,
        "{} lines written, {} delivered, {} dropped ({:?}), {} gap records",
        lines,
        delivered,
        dropped,
        x.aggregates.dropped,
        gaps
    );
    assert_eq!(x.status.signals["logs"].records_sent, delivered, "the exporter's own count");
    x.write_status(true);
    let snapshot: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(status_path(&root.0)).unwrap()).unwrap();
    assert_eq!(snapshot["signals"]["logs"]["backlog_bytes"].as_u64(), Some(0), "{snapshot}");
    assert_eq!(
        snapshot["signals"]["logs"]["records_dropped"].as_u64(),
        Some(dropped),
        "{snapshot}"
    );

    let effective = bytes as f64 / wall.as_secs_f64() / 1024.0 / 1024.0;
    let floor = bytes as f64 * 0.95 / (CAP_MIB as f64 * 1024.0 * 1024.0);
    assert!(
        wall.as_secs_f64() >= floor,
        "the drain outran its cap: {wall:.1?} for {} bytes at {CAP_MIB} MiB/s",
        bytes
    );
    assert!(
        growth <= 64.0,
        "the drain's peak RSS grew {growth:.1} MiB past its steady state"
    );
    println!(
        "drained {} MiB ({bytes} bytes, {lines} lines) in {:.1?} at {effective:.1} MiB/s effective \
         (cap {CAP_MIB} MiB/s), {} requests, peak RSS +{growth:.1} MiB ({} -> {} KiB)",
        bytes / 1024 / 1024,
        wall.as_secs_f64(),
        collector.requests.load(Ordering::Relaxed),
        baseline,
        peak
    );
}

/// #849's second performance criterion, as a crate-level proxy: the mothership's
/// `handle_agent_event` is not callable from this crate, so appending a colony's `events.jsonl` on
/// the test's own thread stands in for the write path (the mothership's own append is one
/// open-append-write per line, unsynced — `util::append_line` — which is what is measured here).
/// With the exporter running against a receiver that accepts and never answers, per-append latency
/// over 1 000 events stays within 10 % of the exporter-off baseline. The ratio is checked only
/// when the baseline is measurable at all (a p99 under 50 µs says more about the machine than the
/// exporter); a hanging exporter that truly got in the write path's way would blow far past the
/// generous 500 µs absolute ceiling, which is checked whatever the baseline is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn event_appends_stay_fast_while_the_exporters_receiver_hangs() {
    const EVENTS: usize = 1000;
    const FLOOR_US: f64 = 50.0;

    // The baseline: the same appends with no exporter running.
    let base = root("append-base");
    let baseline = append_timed(&base.0.join("sessions/c0ffee12/events.jsonl"), EVENTS);

    // The hang run: the exporter, on its own thread the way the mothership spawns it as its own
    // process, against a receiver that never answers.
    let hang = root("append-hang");
    let ledger = hang.0.join("sessions/c0ffee12/events.jsonl");
    let (url, hits, release) = hanging_collector().await;
    for i in 0..100 {
        let ts = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&ledger).unwrap();
        writeln!(
            f,
            "{{\"type\":\"user_message\",\"id\":\"seed-{i:03}\",\"text\":\"{}\",\"ts\":\"{ts}\"}}",
            "s".repeat(1024)
        )
        .unwrap();
    }
    // Generous, so the request truly hangs rather than timing out mid-measurement.
    let path = contract(&hang, &url, |s| s.timeout_secs = 120);
    let stop = Arc::new(AtomicBool::new(false));
    let running = stop.clone();
    let exporter = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let mut x = Exporter::new(&path, Vec::new()).unwrap();
            while !running.load(Ordering::Relaxed) {
                let wait = x.tick().await;
                tokio::time::sleep(wait).await;
            }
            x.shutdown();
        });
    });

    // Wait until the exporter is parked on the hanging request: only then do the appends count.
    let mut hung = false;
    for _ in 0..150 {
        if hits.load(Ordering::SeqCst) >= 1 {
            hung = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hung, "the exporter never reached the hanging receiver");
    let during = append_timed(&ledger, EVENTS);

    // Let the hanging requests go and wind the exporter down.
    stop.store(true, Ordering::SeqCst);
    let _ = release.send(true);
    let _ = exporter.join();

    let (b50, b99) = p50_p99(baseline);
    let (h50, h99) = p50_p99(during);
    println!(
        "append latency over {EVENTS} events (µs): baseline p50 {b50:.0} p99 {b99:.0}; \
         receiver hanging p50 {h50:.0} p99 {h99:.0} ({} requests held by the receiver)",
        hits.load(Ordering::SeqCst)
    );
    if b99 >= FLOOR_US {
        assert!(
            h99 <= b99 * 1.1,
            "p99 went from {b99:.0} µs to {h99:.0} µs while the receiver hung"
        );
    }
    assert!(
        h99 <= 500.0,
        "appends took a p99 of {h99:.0} µs while the receiver hung: the exporter is in the way"
    );
}
