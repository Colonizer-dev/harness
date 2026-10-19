//! The `file` provider (#850): what lands on disk, in what shape, with which modes; rotation and
//! the size caps; fsync-before-commit through the fault seam; the disk-low marker; the secret
//! canaries; and the stop/resume contract — nothing grows after a stop, and a fresh exporter
//! resumes from the kept cursors.

use super::*;
use crate::contract;
use crate::contract::{ColonyPolicy, Contract, Settings};
use crate::encode::Request;
use crate::exporter::Exporter;
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::common::v1::{AnyValue, any_value};
use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use crate::state::State;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-file-sink-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(dir.join("sessions/c0ffee12")).unwrap();
    Root(dir)
}

fn append(path: &Path, line: &serde_json::Value) {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn harness(root: &Root, message: &str) {
    let line = serde_json::json!({
        "type": "harness_log", "origin": "host", "level": "info", "message": message,
        "ts": chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339(),
    });
    append(&root.0.join("sessions/c0ffee12/harness.jsonl"), &line);
}

/// A `file`-provider contract: no endpoint, the streams the edit leaves on, and the backlog start
/// so the lines written before the exporter exists are read.
fn file_contract(root: &Root, edit: impl FnOnce(&mut Settings)) -> PathBuf {
    let mut settings = Settings {
        provider: "file".into(),
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

/// The default sink directory, `<data>/observability/otlp`.
fn default_dir(root: &Root) -> PathBuf {
    sink_dir(&root.0, "").unwrap()
}

/// Every sink file in `dir`, sorted by name.
fn sink_files(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    names.sort();
    names
}

fn sink_bytes(dir: &Path) -> u64 {
    sink_files(dir).iter().map(|p| std::fs::metadata(p).unwrap().len()).sum()
}

fn committed_offset(root: &Root) -> Option<u64> {
    let (state, _) = State::load(&root.0);
    state
        .cursors
        .iter()
        .find(|(k, _)| k.relative_path == "sessions/c0ffee12/harness.jsonl")
        .map(|(_, c)| c.offset)
}

fn file_size(root: &Root) -> u64 {
    std::fs::metadata(root.0.join("sessions/c0ffee12/harness.jsonl"))
        .unwrap()
        .len()
}

/// The non-empty lines of every sink file, in order.
fn lines(dir: &Path) -> Vec<Vec<u8>> {
    sink_files(dir)
        .iter()
        .flat_map(|p| std::fs::read(p).unwrap())
        .fold(Vec::new(), |mut out: Vec<Vec<u8>>, b| {
            match out.last_mut() {
                Some(last) if !last.ends_with(b"\n") => last.push(b),
                _ => out.push(vec![b]),
            }
            out
        })
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect()
}

/// One small logs request, for driving the sink directly.
fn logs_request(body: &str) -> Request {
    Request::Logs(ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    body: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(body.into())),
                    }),
                    ..LogRecord::default()
                }],
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    })
}

fn today() -> String {
    utc_ymd(std::time::SystemTime::now())
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn each_line_is_one_otlp_json_request_in_a_private_directory() {
    let root = root("shape");
    // The contract keeps the OTLP defaults (protobuf, gzip) on purpose: the sink writes plain
    // OTLP/JSON whatever they say.
    let path = file_contract(&root, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    // One line per export request, so one append-and-tick per line: a tick batches everything
    // pending into a single request, and that request becomes exactly one line below.
    for i in 0..3 {
        harness(&root, &format!("line {i}"));
        assert_eq!(x.tick().await, crate::exporter::TICK);
    }

    let dir = default_dir(&root);
    let files = sink_files(&dir);
    assert_eq!(files.len(), 1, "one file for one signal on one day: {files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name, format!("otlp-logs-{}.jsonl", today()), "the UTC date names it");
    assert!(
        name.starts_with("otlp-") && name.ends_with(".jsonl"),
        "it matches the documented glob"
    );

    let raw = lines(&dir);
    assert_eq!(raw.len(), 3, "one line per export request: {} lines", raw.len());
    for line in &raw {
        assert_ne!(line.first(), Some(&0x1f), "never gzip");
        // The crate's own decoder: a line is OTLP/JSON or OTLP protobuf, and this is the former.
        let strings = crate::testkit::strings(line);
        assert!(strings.iter().any(|s| s.contains("line ")), "an OTLP request: {strings:?}");
        let value: serde_json::Value = serde_json::from_slice(line).unwrap();
        assert!(value["resourceLogs"][0]["scopeLogs"][0]["logRecords"].is_array(), "{value}");
        // Into the generated message, which is what the collector's `otlpjsonfile` receiver parses
        // a line into: the shape the doc's YAML promises is the shape a receiver accepts.
        let typed: ExportLogsServiceRequest = serde_json::from_slice(line).unwrap();
        assert!(
            typed
                .resource_logs
                .iter()
                .flat_map(|r| &r.scope_logs)
                .flat_map(|s| &s.log_records)
                .any(|r| r
                    .body
                    .as_ref()
                    .and_then(|b| b.value.as_ref())
                    .is_some_and(|v| matches!(v, any_value::Value::StringValue(s) if s.starts_with("line ")))),
            "a receiver's parse keeps the bodies: {typed:?}"
        );
    }
    // The bodies the requests carry, in order — every record of every line.
    let bodies: Vec<String> = raw
        .iter()
        .flat_map(|l| {
            let v: serde_json::Value = serde_json::from_slice(l).unwrap();
            v["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["body"]["stringValue"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(bodies, ["line 0", "line 1", "line 2"]);

    assert_eq!(
        committed_offset(&root),
        Some(file_size(&root)),
        "committed after the fsynced write"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&dir), 0o700, "the directory is private");
        for file in &files {
            assert_eq!(mode(file), 0o600, "{} is owner-only", file.display());
        }
    }

    // A status.json from a healthy file provider carries no `reason`: the field is
    // skip-when-absent, so a reader from before it existed sees the same shape.
    x.write_status(true);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(crate::exporter::status_path(&root.0)).unwrap()).unwrap();
    assert_eq!(status["signals"]["logs"]["state"], "ok");
    assert!(status["signals"]["logs"].get("reason").is_none(), "{status}");
}

#[test]
fn the_sink_dir_refuses_to_escape_the_data_dir() {
    let root = root("dir");
    assert_eq!(sink_dir(&root.0, "").unwrap(), root.0.join("observability/otlp"));
    assert_eq!(sink_dir(&root.0, " my/dir ").unwrap(), root.0.join("my/dir"));
    for escape in ["/etc", "../elsewhere", "a/../../b"] {
        assert!(sink_dir(&root.0, escape).is_err(), "{escape} must be refused");
    }
}

#[tokio::test]
async fn a_file_rolls_at_the_cap_and_the_oldest_is_trimmed_to_the_total() {
    let root = root("rotate");
    let dir = root.0.join("sink");
    std::fs::create_dir_all(&dir).unwrap();
    // One line's length, newline included, measured off the first write.
    let caps = FileSink::with_caps(&root.0, dir.clone(), 1, 1);
    let line_len = match caps.write(&logs_request("probe-0")) {
        Outcome::Sent { bytes, .. } => bytes,
        other => panic!("the probe write must succeed: {other:?}"),
    };
    // A fresh directory per sink, so the probe file never counts against this one's caps.
    let dir2 = root.0.join("sink2");
    let max_bytes = line_len * 9 / 2;
    let file_cap = line_len * 5 / 2;
    let sink = FileSink::with_caps(&root.0, dir2.clone(), max_bytes, file_cap);
    for i in 0..6 {
        let body = format!("probe-{i}");
        assert_eq!(body.len(), 7);
        match sink.write(&logs_request(&body)) {
            Outcome::Sent { bytes, message, .. } => {
                assert_eq!(bytes, line_len);
                assert!(message.is_none(), "{message:?}");
            }
            other => panic!("write {i} must succeed: {other:?}"),
        }
    }
    let files = sink_files(&dir2);
    // Two lines per file (2 × L ≤ 2.5L, a third would pass it), so three files for six lines —
    // and the total cap (4.5L) keeps only the newest two of them.
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            format!("otlp-logs-{}.1.jsonl", today()),
            format!("otlp-logs-{}.2.jsonl", today()),
        ],
        "the unsuffixed file, the oldest, is the one trimmed: {names:?}"
    );
    let total = sink_bytes(&dir2);
    assert!(total <= max_bytes, "{total} > {max_bytes}");
    for file in &files {
        let len = std::fs::metadata(file).unwrap().len();
        assert!(len <= file_cap, "{} is {len} bytes, past the {file_cap} cap", file.display());
    }
    let all = lines(&dir2);
    assert_eq!(all.len(), 4, "the four newest lines survive the trim: {all:?}");
    let text = String::from_utf8_lossy(all.last().unwrap());
    assert!(text.contains("probe-5"), "the newest line is never trimmed: {text}");
    // The oldest surviving line is the trimmed run's newest: no hole before it.
    assert!(
        String::from_utf8_lossy(&all[0]).contains("probe-2"),
        "{}",
        String::from_utf8_lossy(&all[0])
    );
}

#[tokio::test]
async fn a_line_longer_than_the_cap_is_never_split() {
    let root = root("oversize");
    let dir = root.0.join("sink");
    std::fs::create_dir_all(&dir).unwrap();
    let sink = FileSink::with_caps(&root.0, dir.clone(), u64::MAX, 8);
    for i in 0..2 {
        match sink.write(&logs_request(&format!("over-{i}"))) {
            Outcome::Sent { .. } => {}
            other => panic!("write {i} must succeed: {other:?}"),
        }
    }
    // A cap of 8 bytes cannot hold either line: each gets its own file, whole.
    let files = sink_files(&dir);
    assert_eq!(files.len(), 2, "one file per whole line: {files:?}");
    assert_eq!(lines(&dir).len(), 2);
    assert!(std::fs::read(&files[1]).unwrap().len() > 8, "the line is not truncated");
}

#[tokio::test]
async fn a_write_or_fsync_failure_holds_the_batch_and_commits_no_cursor() {
    let root = root("faults");
    harness(&root, "held line");
    let path = file_contract(&root, |_| {});
    let dir = default_dir(&root);

    // The write itself fails: no file appears, nothing is committed.
    let write_fault = faults::inject(&dir.display().to_string(), faults::Op::Write, || {
        std::io::Error::from_raw_os_error(5)
    });
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert!(x.tick().await != crate::exporter::TICK, "held, whatever the jitter");
    assert_eq!(x.status.state, "retrying");
    assert_eq!(committed_offset(&root), None, "no fsync, no commit");
    assert!(!dir.join(format!("otlp-logs-{}.jsonl", today())).exists());
    drop(write_fault);
    x.backoff = None;

    // The write lands but the fsync fails: the outcome is still not Sent, so the cursor waits and
    // the batch replays — the same at-least-once a crash between fsync and commit would give.
    let fsync_fault = faults::inject(&dir.display().to_string(), faults::Op::Fsync, || {
        std::io::Error::from_raw_os_error(5)
    });
    assert!(x.tick().await != crate::exporter::TICK, "held again, whatever the jitter");
    assert_eq!(committed_offset(&root), None, "commit only ever follows an fsync");
    drop(fsync_fault);
    x.backoff = None;

    assert_eq!(x.tick().await, crate::exporter::TICK, "the third try goes through");
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    assert_eq!(lines(&dir).len(), 2, "the replayed batch wrote its line again");
}

#[tokio::test]
async fn the_disk_low_marker_pauses_the_sink_and_names_the_reason() {
    let root = root("guard");
    harness(&root, "held while disk is low");
    let path = file_contract(&root, |_| {});
    let dir = default_dir(&root);
    let marker = root.0.join("observability").join(DISK_LOW);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, br#"{"reason":"disk"}"#).unwrap();

    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert!(x.tick().await != crate::exporter::TICK, "a retry with backoff, not a write");
    assert_eq!(x.status.state, "retrying");
    assert!(!dir.exists(), "nothing was written, not even the directory");
    assert_eq!(committed_offset(&root), None);
    assert_eq!(x.status.signal("logs").state, "backing_off");
    assert_eq!(x.status.signal("logs").reason, Some("disk"));
    x.write_status(true);
    let status: serde_json::Value =
        serde_json::from_slice(&std::fs::read(crate::exporter::status_path(&root.0)).unwrap()).unwrap();
    assert_eq!(status["signals"]["logs"]["state"], "backing_off");
    assert_eq!(status["signals"]["logs"]["reason"], "disk");

    // The marker gone, the same batch flows and the reason clears.
    std::fs::remove_file(&marker).unwrap();
    x.backoff = None;
    assert_eq!(x.tick().await, crate::exporter::TICK);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    assert_eq!(x.status.signal("logs").state, "ok");
    assert_eq!(x.status.signal("logs").reason, None);
    assert_eq!(lines(&dir).len(), 1);
}

#[tokio::test]
async fn no_canary_reaches_the_sink_files() {
    let root = root("canary");
    let canaries = crate::testkit::Canaries::new();
    for canary in &canaries.all {
        harness(&root, &canary.text);
    }
    harness(&root, &canaries.content);
    let path = file_contract(&root, |_| {});
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    x.tick().await;
    let dir = default_dir(&root);
    let all = lines(&dir);
    // One tick may batch the lines into one request — the point here is not the count but that
    // every exported line, however many there are, is scrubbed. The canaries' texts are fed as
    // plain messages, as in the wire test: the messages may arrive, their secret forms may not
    // (the content gate is the conversation tier's, not an operational message's).
    assert!(!all.is_empty(), "the lines were exported");
    for line in &all {
        crate::testkit::assert_absent(line, canaries.secrets(), "sink line");
    }
}

/// The `run` loop of `main.rs`, as it races the stop signal: a stop between ticks leaves nothing
/// further to write, and a fresh exporter resumes from the kept cursors with no line lost.
#[tokio::test(start_paused = true)]
async fn after_stop_no_file_grows_and_a_restart_resumes_from_the_kept_cursor() {
    let root = root("stop");
    harness(&root, "before stop");
    let path = file_contract(&root, |s| s.metrics_interval_secs = 3600);
    let mut x = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(x.tick().await, crate::exporter::TICK);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    harness(&root, "while it runs");
    let dir = default_dir(&root);
    let size_when_stopped = sink_bytes(&dir);

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
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
    // Let the loop tick once (the pending line goes out and commits) and settle into its sleep.
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if committed_offset(&root) == Some(file_size(&root)) {
            break;
        }
    }
    let size_at_stop = sink_bytes(&dir);
    assert!(size_at_stop > size_when_stopped, "the loop did flush the pending line first");
    let cursor_at_stop = committed_offset(&root);

    // Stop: the loop leaves its sleep at once, without waiting the tick out.
    stop_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3600), task)
        .await
        .unwrap()
        .unwrap();

    // Three tick periods pass with two fresh lines on disk: no sink file grows, no cursor moves.
    harness(&root, "after stop 1");
    harness(&root, "after stop 2");
    for _ in 0..3 {
        tokio::time::advance(crate::exporter::TICK).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(sink_bytes(&dir), size_at_stop, "nothing was written after the stop");
    assert_eq!(committed_offset(&root), cursor_at_stop);

    // A fresh exporter from the kept state.json picks up exactly where the old one committed.
    let mut y = Exporter::new(&path, Vec::new()).unwrap();
    assert_eq!(y.tick().await, crate::exporter::TICK);
    assert_eq!(committed_offset(&root), Some(file_size(&root)));
    // Every record of every line, in order: a line may carry several records (one request), but
    // each record arrives exactly once across the restart.
    let bodies: Vec<String> = lines(&dir)
        .iter()
        .flat_map(|l| {
            let v: serde_json::Value = serde_json::from_slice(l).unwrap();
            v["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["body"]["stringValue"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(bodies, ["before stop", "while it runs", "after stop 1", "after stop 2"]);
}
