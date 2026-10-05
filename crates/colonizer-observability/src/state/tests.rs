//! The state file's tests: missing, corrupt and unknown-version files, and the atomic write.

use crate::cursor::{Cursor, FileId, GapReason};
use crate::state::{CursorKey, DIR, Signal, State, VERSION, state_file};
use serde_json::json;
use std::path::PathBuf;

/// A temp directory that removes itself.
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempRoot {
    let dir = std::env::temp_dir().join(format!("colonizer-state-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    TempRoot(dir)
}

fn key(i: usize) -> CursorKey {
    CursorKey {
        destination_hash: format!("dest{i}"),
        signal: Signal::Logs,
        relative_path: format!("file{i}.jsonl"),
    }
}

#[test]
fn a_missing_state_file_is_an_empty_state_with_no_gap() {
    let root = temp("missing");
    let (state, gap) = State::load(&root.0);
    assert!(gap.is_none());
    assert!(state.cursors.is_empty());
    assert_eq!(state.version, VERSION);
}

#[test]
fn a_corrupt_state_file_resets_with_one_gap() {
    let root = temp("corrupt");
    std::fs::create_dir_all(root.0.join(DIR)).unwrap();
    std::fs::write(state_file(&root.0), b"{ this is not json").unwrap();
    let (state, gap) = State::load(&root.0);
    assert_eq!(gap.map(|gap| gap.reason), Some(GapReason::StateReset));
    assert!(state.cursors.is_empty());
}

#[test]
fn an_unknown_version_resets_with_one_gap() {
    let root = temp("version");
    std::fs::create_dir_all(root.0.join(DIR)).unwrap();
    std::fs::write(state_file(&root.0), br#"{"version":99,"cursors":[],"extra":{}}"#).unwrap();
    let (state, gap) = State::load(&root.0);
    assert_eq!(gap.map(|gap| gap.reason), Some(GapReason::StateReset));
    assert!(state.cursors.is_empty());
}

#[test]
fn the_first_commit_after_load_writes_without_force() {
    let root = temp("first-commit");
    let (mut state, gap) = State::load(&root.0);
    assert!(gap.is_none());
    state.set_cursor(
        key(0),
        Cursor {
            file_id: Some(FileId { dev: 3, ino: 4 }),
            offset: 11,
            last_key: None,
        },
    );

    assert!(state.commit(&root.0, false).unwrap(), "the first commit after load writes");
    assert!(!state.commit(&root.0, false).unwrap(), "a second within a second is skipped");
    assert!(state.commit(&root.0, true).unwrap(), "a forced commit writes again");
}

#[test]
fn commit_writes_once_a_second_and_round_trips_every_signal() {
    let root = temp("commit");
    let mut state = State::default();
    let signals = [Signal::Logs, Signal::Traces, Signal::Metrics];
    for (i, signal) in signals.into_iter().enumerate() {
        let mut k = key(i);
        k.signal = signal;
        state.set_cursor(
            k,
            Cursor {
                file_id: Some(FileId { dev: 1, ino: 2 }),
                offset: 7,
                last_key: Some(9),
            },
        );
    }
    state.extra.insert("span".to_string(), json!({"id": "a"}));

    assert!(state.commit(&root.0, true).unwrap(), "the first commit writes");
    assert!(!state.commit(&root.0, false).unwrap(), "a second within a second is skipped");
    assert!(state.commit(&root.0, true).unwrap(), "a forced commit writes again");

    // The cursors are an array (JSON object keys must be strings) and everything round-trips.
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(state_file(&root.0)).unwrap()).unwrap();
    assert_eq!(raw["version"], VERSION);
    assert!(raw["cursors"].is_array());

    let (reloaded, gap) = State::load(&root.0);
    assert!(gap.is_none());
    assert_eq!(reloaded.cursors.len(), signals.len());
    for (i, signal) in signals.into_iter().enumerate() {
        let mut k = key(i);
        k.signal = signal;
        assert_eq!(reloaded.cursor(&k).offset, 7);
        assert_eq!(reloaded.cursor(&k).last_key, Some(9));
    }
    assert_eq!(reloaded.extra.get("span"), Some(&json!({"id": "a"})));
}
