//! The multi-source tailer: every line of every source of a v0.1.9 data dir yielded once, a colony
//! launched mid-run, fairness and the rate limit under a paused clock, the backlog guard, gap
//! records for rotation and deletion, drained colonies, and the start position.

use super::*;
use crate::contract::Settings;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

const DEST: &str = "dest0001";

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-tailer-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

fn now_nanos() -> u64 {
    crate::exporter::now_nanos()
}

fn now_ts() -> String {
    chrono::DateTime::from_timestamp_nanos(now_nanos() as i64).to_rfc3339()
}

fn append(path: &Path, line: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

fn colony(status: &str) -> ColonyPolicy {
    ColonyPolicy {
        org: "acme".into(),
        repo: "acme/widgets".into(),
        status: Some(status.into()),
        ..ColonyPolicy::default()
    }
}

/// One tick against `state`, then commits what it read as the exporter does after an ack.
fn tick(
    tailer: &mut Tailer,
    root: &Root,
    settings: &Settings,
    colonies: &BTreeMap<String, ColonyPolicy>,
    state: &mut State,
) -> Tailed {
    let tailed = {
        let inputs = Inputs {
            data_dir: &root.0,
            settings,
            colonies,
            state,
            destination: DEST,
            now_unix_nanos: now_nanos(),
        };
        tailer.collect(&inputs)
    };
    for (key, cursor) in &tailed.cursors {
        state.set_cursor(key.clone(), cursor.clone());
    }
    for key in &tailed.removed {
        state.cursors.remove(key);
    }
    if !tailed.gone.is_empty() {
        let mut all = gone(state);
        all.extend(tailed.gone.iter().cloned());
        state.extra.insert(GONE_KEY.into(), serde_json::json!(all));
    }
    tailed
}

fn logs_only() -> Settings {
    Settings {
        stream_metrics: false,
        max_backlog_days: 0,
        ..Settings::default()
    }
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

/// Every line of every ledger under `root`, as `(relative path, line)`.
fn every_line(root: &Path) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                for line in std::fs::read_to_string(&path).unwrap().lines() {
                    out.push((rel.clone(), serde_json::from_str::<Value>(line).unwrap()));
                }
            }
        }
    }
    out.sort_by(|a, b| (a.0.as_str(), a.1.to_string()).cmp(&(b.0.as_str(), b.1.to_string())));
    out
}

/// A record's ledger path, from its source and colony.
fn path_of(record: &Record) -> String {
    let file = match record.source {
        Source::Harness => "harness.jsonl",
        Source::Events => "events.jsonl",
        Source::Gateway => "gateway.jsonl",
        Source::Findings => "findings.jsonl",
        Source::Activity => return "activity.jsonl".into(),
        Source::Spend => return "spend.jsonl".into(),
        Source::Decisions => return "decisions.jsonl".into(),
        Source::Routing => return "routing.jsonl".into(),
        Source::JevLadder => return "jev_ladder.jsonl".into(),
        Source::JevFocus => return "jev_focus.jsonl".into(),
        Source::Mothership => return "logs/mothership.jsonl".into(),
        Source::ExportGap => unreachable!("not a ledger"),
    };
    format!("sessions/{}/{file}", record.colony.as_deref().unwrap())
}

#[test]
fn a_v019_data_dir_yields_every_line_of_every_source_once_and_finds_a_new_colony() {
    let root = root("fixture");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../colonizer/tests/fixtures/data-v0.1.9");
    copy_dir(&fixture, &root.0);
    // The install-wide ledgers a v0.1.9 data dir also holds.
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

    // The contract's colony list, as the mothership builds it from `sessions.json`. A v0.1.9
    // session predates #472, so its sensitivity is unset.
    let sessions: Vec<Value> = serde_json::from_slice(&std::fs::read(root.0.join("sessions.json")).unwrap()).unwrap();
    let mut colonies: BTreeMap<String, ColonyPolicy> = sessions
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_string(),
                serde_json::from_value(s.clone()).unwrap(),
            )
        })
        .collect();
    assert_eq!(colonies.len(), 2);
    for policy in colonies.values() {
        assert_eq!(policy.sensitivity, None, "a pre-#472 session has no sensitivity");
        assert_eq!(policy.org, "acme");
    }
    let mut newer = sessions[0].clone();
    newer["sensitivity"] = serde_json::json!("secret");
    let parsed: ColonyPolicy = serde_json::from_value(newer).unwrap();
    assert_eq!(parsed.sensitivity.as_deref(), Some("secret"), "and parsed when it is set");

    let settings = logs_only();
    let mut tailer = Tailer::default();
    let mut state = State::default();
    let mut seen: Vec<(String, Value)> = Vec::new();
    for _ in 0..3 {
        let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
        assert!(tailed.gaps.is_empty(), "{:?}", tailed.gaps);
        seen.extend(tailed.records.iter().map(|r| (path_of(r), r.line.clone())));
    }
    seen.sort_by(|a, b| (a.0.as_str(), a.1.to_string()).cmp(&(b.0.as_str(), b.1.to_string())));
    assert_eq!(seen, every_line(&root.0), "every line once, nothing else");

    // A colony launched now is read on the next tick after the mothership lists it.
    append(
        &root.0.join("sessions/f00dcafe/harness.jsonl"),
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "booted", "ts": now_ts()}),
    );
    assert!(
        tick(&mut tailer, &root, &settings, &colonies, &mut state).records.is_empty(),
        "not listed yet"
    );
    colonies.insert("f00dcafe".into(), colony("running"));
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert_eq!(tailed.records.len(), 1);
    assert_eq!(tailed.records[0].colony.as_deref(), Some("f00dcafe"));
    assert_eq!(tailed.records[0].line["message"], "booted");
}

#[test]
fn one_huge_colony_cannot_starve_nine_small_ones() {
    let root = root("fair");
    let mut colonies = BTreeMap::new();
    let ts = now_ts();
    // 10 MB of events in one colony.
    let filler = "x".repeat(900);
    let mut big = String::new();
    let mut seq = 0;
    while big.len() < 10 * 1024 * 1024 {
        seq += 1;
        big.push_str(&serde_json::json!({"seq": seq, "type": "progress", "message": filler, "ts": ts}).to_string());
        big.push('\n');
    }
    std::fs::create_dir_all(root.0.join("sessions/big00000")).unwrap();
    std::fs::write(root.0.join("sessions/big00000/events.jsonl"), &big).unwrap();
    colonies.insert("big00000".to_string(), colony("running"));
    for i in 0..9 {
        let id = format!("small{i:03}");
        for n in 0..5 {
            append(
                &root.0.join(format!("sessions/{id}/events.jsonl")),
                &serde_json::json!({"seq": n, "type": "progress", "message": "small", "ts": ts}),
            );
        }
        colonies.insert(id, colony("running"));
    }

    let settings = logs_only();
    let mut tailer = Tailer::default();
    let mut state = State::default();
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    for i in 0..9 {
        let id = format!("small{i:03}");
        let n = tailed
            .records
            .iter()
            .filter(|r| r.colony.as_deref() == Some(id.as_str()))
            .count();
        assert_eq!(n, 5, "{id} drained on the first tick");
    }
    let busy = tailed
        .records
        .iter()
        .filter(|r| r.colony.as_deref() == Some("big00000"))
        .count();
    assert!(busy > 0, "the big colony progresses too");
    let budget = settings.max_read_mib_per_sec * 1024 * 1024;
    assert!(tailed.bytes_read <= budget + 1024, "{} > {budget}", tailed.bytes_read);
}

#[tokio::test(start_paused = true)]
async fn the_read_rate_holds_across_ticks() {
    let root = root("rate");
    let ts = now_ts();
    let filler = "y".repeat(1000);
    let mut text = String::new();
    for seq in 0..12_000 {
        text.push_str(&serde_json::json!({"seq": seq, "type": "progress", "message": filler, "ts": ts}).to_string());
        text.push('\n');
    }
    std::fs::create_dir_all(root.0.join("sessions/c0ffee12")).unwrap();
    std::fs::write(root.0.join("sessions/c0ffee12/events.jsonl"), &text).unwrap();
    let colonies = BTreeMap::from([("c0ffee12".to_string(), colony("running"))]);
    let settings = Settings {
        max_read_mib_per_sec: 1,
        ..logs_only()
    };
    let rate = 1024 * 1024;
    let line = 1100;
    let mut tailer = Tailer::default();
    let mut state = State::default();

    let first = tick(&mut tailer, &root, &settings, &colonies, &mut state).bytes_read;
    assert!(first > rate / 2 && first <= rate + line, "{first}");
    let again = tick(&mut tailer, &root, &settings, &colonies, &mut state).bytes_read;
    assert!(again <= line, "no time passed, no tokens: {again}");

    let mut total = first + again;
    for _ in 0..4 {
        tokio::time::advance(Duration::from_millis(500)).await;
        total += tick(&mut tailer, &root, &settings, &colonies, &mut state).bytes_read;
    }
    // One full bucket plus two seconds of refill, give or take a line per tick.
    assert!(total <= 3 * rate + 6 * line, "{total}");
    assert!(total >= 2 * rate, "{total}");
}

#[tokio::test(start_paused = true)]
async fn the_backlog_guard_yields_one_gap_per_run_and_then_only_fresh_lines() {
    let root = root("backlog");
    let path = root.0.join("sessions/c0ffee12/harness.jsonl");
    let old = "2020-01-01T00:00:00Z";
    let message = "z".repeat(1000);
    let mut skipped_bytes = 0u64;
    // About 2 MiB of old lines: more than one tick's budget at 1 MiB/s.
    for i in 0..2000 {
        let line = serde_json::json!({"type": "harness_log", "level": "info", "message": format!("{i}{message}"), "ts": old});
        skipped_bytes += line.to_string().len() as u64 + 1;
        append(&path, &line);
    }
    for i in 0..3 {
        append(
            &path,
            &serde_json::json!({"type": "harness_log", "level": "info", "message": format!("fresh {i}"), "ts": now_ts()}),
        );
    }
    let colonies = BTreeMap::from([("c0ffee12".to_string(), colony("running"))]);
    let settings = Settings {
        max_backlog_days: 7,
        max_read_mib_per_sec: 1,
        ..logs_only()
    };
    let mut tailer = Tailer::default();
    let mut state = State::default();
    let mut gaps = Vec::new();
    let mut fresh = Vec::new();
    let mut backlog = 0;
    for _ in 0..5 {
        let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
        gaps.extend(tailed.gaps);
        fresh.extend(tailed.records.into_iter().map(|r| r.line["message"].clone()));
        backlog += tailed.drops.get("backlog").copied().unwrap_or(0);
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert_eq!(gaps.len(), 1, "one gap for the whole run: {gaps:?}");
    let gap = &gaps[0].line;
    assert_eq!(gap["type"], "export_gap");
    assert_eq!(gap["reason"], "backlog");
    assert_eq!(gap["file"], "sessions/c0ffee12/harness.jsonl");
    assert_eq!(gap["colony"], "c0ffee12");
    assert_eq!(gap["bytes"], skipped_bytes);
    assert_eq!(gap["lines"], 2000);
    assert_eq!(gaps[0].source, Source::ExportGap);
    assert_eq!(backlog, 2000, "and every line counted");
    assert_eq!(fresh, ["fresh 0", "fresh 1", "fresh 2"]);

    // `0` disables the guard: everything is yielded, no gap.
    let settings = Settings {
        max_backlog_days: 0,
        max_read_mib_per_sec: 64,
        ..logs_only()
    };
    let mut tailer = Tailer::default();
    let mut state = State::default();
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert!(tailed.gaps.is_empty());
    assert_eq!(tailed.records.len(), 2003);
}

#[test]
fn a_rotation_past_the_cursor_yields_a_gap_record() {
    let root = root("rotated");
    let live = root.0.join("activity.jsonl");
    let rolled = root.0.join("activity.jsonl.1");
    let line = |seq: u64| serde_json::json!({"seq": seq, "ts": now_ts(), "kind": "colony.launch", "actor": "you"});
    append(&live, &line(1));
    let colonies = BTreeMap::new();
    let settings = logs_only();
    let mut tailer = Tailer::default();
    let mut state = State::default();
    assert_eq!(tick(&mut tailer, &root, &settings, &colonies, &mut state).records.len(), 1);

    // Two rotations while the exporter was behind: the file the cursor named is gone.
    append(&live, &line(2));
    std::fs::rename(&live, &rolled).unwrap();
    append(&live, &line(3));
    std::fs::rename(&live, &rolled).unwrap();
    append(&live, &line(4));
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert_eq!(tailed.gaps.len(), 1, "{:?}", tailed.gaps);
    assert_eq!(tailed.gaps[0].line["reason"], "rotated_past");
    assert_eq!(tailed.gaps[0].line["file"], "activity.jsonl");
    assert_eq!(tailed.drops.get("gap_rotated_past"), Some(&1));
    let seqs: Vec<_> = tailed.records.iter().map(|r| r.line["seq"].clone()).collect();
    assert_eq!(seqs, [4]);
}

#[test]
fn a_deleted_colony_yields_one_gap_with_its_archived_flag_and_is_never_looked_at_again() {
    let root = root("deleted");
    let mut colonies = BTreeMap::new();
    for id in ["kept0001", "arch0001", "gone0001"] {
        append(
            &root.0.join(format!("sessions/{id}/harness.jsonl")),
            &serde_json::json!({"type": "harness_log", "level": "info", "message": id, "ts": now_ts()}),
        );
        colonies.insert(id.to_string(), colony("running"));
    }
    // `arch0001` was archived before it was deleted (archive.rs's sidecar layout).
    let sidecar = root.0.join("archive/acme/widgets/2026/10/arch0001.json");
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, br#"{"session": "arch0001", "revision": 1}"#).unwrap();
    // A sidecar of another colony whose id merely starts the same way does not count.
    std::fs::write(sidecar.with_file_name("gone0001x.json"), br#"{"session": "gone0001x"}"#).unwrap();

    let settings = logs_only();
    let mut tailer = Tailer::default();
    let mut state = State::default();
    assert_eq!(tick(&mut tailer, &root, &settings, &colonies, &mut state).records.len(), 3);

    std::fs::remove_dir_all(root.0.join("sessions/arch0001")).unwrap();
    std::fs::remove_dir_all(root.0.join("sessions/gone0001")).unwrap();
    // The mothership drops one of them from its list at once; the other is still listed.
    colonies.remove("gone0001");
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    let mut gaps: Vec<_> = tailed.gaps.iter().map(|g| g.line.clone()).collect();
    gaps.sort_by_key(|g| g["colony"].to_string());
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert_eq!(gaps[0]["colony"], "arch0001");
    assert_eq!(gaps[0]["reason"], "deleted");
    assert_eq!(gaps[0]["archived"], true);
    assert_eq!(gaps[1]["colony"], "gone0001");
    assert_eq!(gaps[1]["archived"], false);
    assert_eq!(tailed.gone.len(), 2);
    assert!(
        state
            .cursors
            .keys()
            .all(|k| !k.relative_path.contains("arch0001") && !k.relative_path.contains("gone0001")),
        "their read positions are dropped"
    );

    // Never looked at again: a directory that reappears under the id is not even opened.
    append(
        &root.0.join("sessions/arch0001/harness.jsonl"),
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "back", "ts": now_ts()}),
    );
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert!(tailed.gaps.is_empty());
    assert!(tailed.records.is_empty(), "{:?}", tailed.records);
}

#[test]
fn a_finished_colony_is_drained_and_left_alone_until_it_changes() {
    let root = root("drained");
    let dir = root.0.join("sessions/done0001");
    let path = dir.join("harness.jsonl");
    append(
        &path,
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "merged", "ts": now_ts()}),
    );
    // Older than the grace period.
    let old = SystemTime::now() - DRAIN_GRACE * 2;
    std::fs::File::options()
        .append(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mut colonies = BTreeMap::from([("done0001".to_string(), colony("merged"))]);
    let settings = logs_only();
    let mut tailer = Tailer::default();
    let mut state = State::default();
    assert_eq!(tick(&mut tailer, &root, &settings, &colonies, &mut state).records.len(), 1);
    std::fs::File::options()
        .append(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(tick(&mut tailer, &root, &settings, &colonies, &mut state).records.is_empty());
    assert!(
        tailer.drained.contains_key("done0001"),
        "drained once read to the end and quiet"
    );

    // Its files are no longer opened: an append alone (the directory's mtime unchanged) is not seen.
    let before = std::fs::metadata(&dir).unwrap().modified().unwrap();
    append(
        &path,
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "late", "ts": now_ts()}),
    );
    if std::fs::metadata(&dir).unwrap().modified().unwrap() == before {
        assert!(tick(&mut tailer, &root, &settings, &colonies, &mut state).records.is_empty());
    }
    // A status change (resumed) brings it back, and the line is read.
    colonies.insert("done0001".into(), colony("running"));
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert_eq!(tailed.records.len(), 1);
    assert_eq!(tailed.records[0].line["message"], "late");
    assert!(!tailer.drained.contains_key("done0001"));

    // A colony still watched (`pr_opened`) is never drained.
    colonies.insert("done0001".into(), colony("pr_opened"));
    std::fs::File::options()
        .append(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    tick(&mut tailer, &root, &settings, &colonies, &mut state);
    assert!(!tailer.drained.contains_key("done0001"));
}

#[test]
fn seeding_binds_existing_files_to_their_last_complete_line() {
    let root = root("seed");
    let path = root.0.join("sessions/c0ffee12/harness.jsonl");
    append(
        &path,
        &serde_json::json!({"type": "harness_log", "level": "info", "message": "history", "ts": now_ts()}),
    );
    // A line still being written: no newline yet.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    write!(f, "{{\"type\":\"harness_log\",\"message\":\"part").unwrap();
    drop(f);
    let colonies = BTreeMap::from([("c0ffee12".to_string(), colony("running"))]);
    let settings = logs_only();
    let mut state = State::default();
    {
        let inputs = Inputs {
            data_dir: &root.0,
            settings: &settings,
            colonies: &colonies,
            state: &State::default(),
            destination: DEST,
            now_unix_nanos: now_nanos(),
        };
        seed_at_end(&inputs, &mut state);
    }
    assert_eq!(state.cursors.len(), 1, "only files that exist get a position");

    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(f, "ial\"}}").unwrap();
    drop(f);
    let mut tailer = Tailer::default();
    let tailed = tick(&mut tailer, &root, &settings, &colonies, &mut state);
    let messages: Vec<_> = tailed.records.iter().map(|r| r.line["message"].clone()).collect();
    assert_eq!(messages, ["partial"], "history skipped, the line in progress kept");
}
