//! The tailer's tests: incremental appends, restarts, rotation, truncation, deletion, oversized
//! lines, an activity-style rotation, and a seeded randomised interleaving.

use super::*;
use crate::state::{self, CursorKey, Signal, State};
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// A temp directory that removes itself.
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempRoot {
    let dir = std::env::temp_dir().join(format!("colonizer-tailer-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    TempRoot(dir)
}

/// One JSON line for a sequence number.
fn line(seq: u64) -> String {
    format!("{{\"seq\":{seq}}}\n")
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// Replaces the rolled file with the live one and leaves a fresh, empty live file behind. The new
/// live is created before the rename, so its identity can never reuse the inode the rename frees —
/// the test must exercise the real identity change, not an inode accident.
fn rotate(live: &Path, rolled: &Path) {
    let tmp = live.with_extension("tmp");
    let _ = std::fs::File::create(&tmp);
    let _ = std::fs::rename(live, rolled);
    let _ = std::fs::rename(&tmp, live);
}

fn seqs(batch: &Batch) -> Vec<u64> {
    batch.lines.iter().map(|line| line["seq"].as_u64().unwrap()).collect()
}

fn reasons(batch: &Batch) -> Vec<GapReason> {
    batch.gaps.iter().map(|gap| gap.reason).collect()
}

fn key() -> CursorKey {
    CursorKey {
        destination_hash: "dest".to_string(),
        signal: Signal::Logs,
        relative_path: "x.jsonl".to_string(),
    }
}

fn persist(dir: &Path, state: &State) {
    let path = state::state_file(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(state).unwrap()).unwrap();
}

// ---------------------------------------------------------------------------
// Appends, restarts.
// ---------------------------------------------------------------------------

#[test]
fn appends_are_picked_up_incrementally_and_a_half_line_waits() {
    let root = temp("incremental");
    let live = root.0.join("x.jsonl");
    let limits = Limits::default();

    // A file that does not exist yet: an unbound cursor, an empty batch, no gap.
    let empty = read_batch(&live, None, &Cursor::default(), &limits).unwrap();
    assert!(empty.lines.is_empty() && empty.gaps.is_empty());
    let mut cursor = empty.cursor;

    append(&live, &line(1));
    let batch = read_batch(&live, None, &cursor, &limits).unwrap();
    assert_eq!(seqs(&batch), vec![1]);
    cursor = batch.cursor;

    // Half a line: not consumed until its newline arrives.
    append(&live, "{\"seq\":2");
    let half = read_batch(&live, None, &cursor, &limits).unwrap();
    assert!(half.lines.is_empty(), "a half line is not a line yet");
    assert_eq!(half.cursor.offset, cursor.offset, "the half line is not consumed");
    cursor = half.cursor;

    append(&live, "}\n");
    let batch = read_batch(&live, None, &cursor, &limits).unwrap();
    assert_eq!(seqs(&batch), vec![2]);
}

#[test]
fn restart_replays_what_a_crash_left_uncommitted() {
    let root = temp("restart");
    let live = root.0.join("x.jsonl");
    let limits = Limits::default();
    append(&live, &line(1));
    append(&live, &line(2));
    append(&live, &line(3));

    // Read the first batch and commit it.
    let first = read_batch(&live, None, &Cursor::default(), &limits).unwrap();
    assert_eq!(seqs(&first), vec![1, 2, 3]);
    let mut state = State::default();
    state.set_cursor(key(), first.cursor.clone());
    persist(&root.0, &state);

    // Reload: nothing new, so nothing comes back.
    let (reloaded, gap) = State::load(&root.0);
    assert!(gap.is_none());
    let nothing = read_batch(&live, None, &reloaded.cursor(&key()), &limits).unwrap();
    assert!(nothing.lines.is_empty());

    // Append one more and commit it; the committed tail is not re-read.
    append(&live, &line(4));
    let second = read_batch(&live, None, &reloaded.cursor(&key()), &limits).unwrap();
    assert_eq!(seqs(&second), vec![4]);
    state.set_cursor(key(), second.cursor.clone());
    persist(&root.0, &state);

    // A crash between read and commit: read a batch, do not persist, reload the old state, and the
    // same lines come back rather than being lost.
    append(&live, &line(5));
    let uncommitted = read_batch(&live, None, &second.cursor, &limits).unwrap();
    assert_eq!(seqs(&uncommitted), vec![5]);
    let (old, _) = State::load(&root.0);
    let again = read_batch(&live, None, &old.cursor(&key()), &limits).unwrap();
    assert_eq!(seqs(&again), vec![5], "uncommitted lines are re-read after a crash");
}

// ---------------------------------------------------------------------------
// Rotation.
// ---------------------------------------------------------------------------

#[test]
fn lines_around_a_rotation_are_yielded_exactly_once() {
    let root = temp("rotation");
    let live = root.0.join("x.jsonl");
    let rolled = root.0.join("x.jsonl.1");
    let limits = Limits::default();
    append(&live, &line(1));
    append(&live, &line(2));

    let first = read_batch(&live, Some(&rolled), &Cursor::default(), &limits).unwrap();
    assert_eq!(seqs(&first), vec![1, 2]);
    let cursor = first.cursor;

    // A line lands, the file rotates, and a line lands in the fresh live one.
    append(&live, &line(3));
    rotate(&live, &rolled);
    append(&live, &line(4));

    // The cursor is in the rolled file (the old live): finish it, then continue into the live one.
    let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
    assert_eq!(seqs(&batch), vec![3, 4]);
    assert!(batch.gaps.is_empty());

    // Nothing is left.
    let done = read_batch(&live, Some(&rolled), &batch.cursor, &limits).unwrap();
    assert!(done.lines.is_empty() && done.gaps.is_empty());
}

#[test]
fn double_rotation_while_behind_yields_exactly_one_rotated_past_gap() {
    let root = temp("double");
    let live = root.0.join("x.jsonl");
    let rolled = root.0.join("x.jsonl.1");
    let limits = Limits::default();
    append(&live, &line(1));
    let cursor = read_batch(&live, Some(&rolled), &Cursor::default(), &limits).unwrap().cursor;

    append(&live, &line(2));
    rotate(&live, &rolled);
    append(&live, &line(3));
    rotate(&live, &rolled);
    append(&live, &line(4));

    let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
    assert_eq!(reasons(&batch), vec![GapReason::RotatedPast]);
    assert_eq!(seqs(&batch), vec![4]);
}

/// The activity log's own rotation (`crates/colonizer/src/activity.rs`): append to the live file,
/// and once it passes its size limit rename it over `activity.jsonl.1` so the next append starts a
/// fresh live file. Reading after each append must see every line once.
#[test]
fn an_activity_style_rotation_yields_every_line_exactly_once() {
    let root = temp("activity");
    let live = root.0.join("activity.jsonl");
    let rolled = root.0.join("activity.jsonl.1");
    let limits = Limits::default();
    let mut cursor = Cursor::default();
    let mut seen: Vec<u64> = Vec::new();
    let mut gaps = 0usize;

    // A limit small enough that the log rolls over several times across the run.
    for seq in 1..=12u64 {
        if std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0) > 20 {
            let _ = std::fs::rename(&live, &rolled);
        }
        append(&live, &line(seq));
        let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
        seen.extend(seqs(&batch));
        gaps += batch.gaps.len();
        cursor = batch.cursor;
    }
    let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
    seen.extend(seqs(&batch));
    gaps += batch.gaps.len();

    seen.sort_unstable();
    assert_eq!(seen, (1..=12).collect::<Vec<_>>(), "each seq once, in order");
    assert_eq!(gaps, 0, "reading after each append keeps up with every rotation");
}

// ---------------------------------------------------------------------------
// Truncation, deletion, oversized lines, malformed lines.
// ---------------------------------------------------------------------------

#[test]
fn truncation_yields_one_gap_and_restarts() {
    let root = temp("truncation");
    let live = root.0.join("x.jsonl");
    let limits = Limits::default();
    for i in 1..=3 {
        append(&live, &line(i));
    }
    let cursor = read_batch(&live, None, &Cursor::default(), &limits).unwrap().cursor;

    // Truncate in place (same inode), then write a fresh line.
    std::fs::File::create(&live).unwrap();
    append(&live, &line(9));

    let batch = read_batch(&live, None, &cursor, &limits).unwrap();
    assert_eq!(reasons(&batch), vec![GapReason::Truncated]);
    assert_eq!(seqs(&batch), vec![9]);
}

#[test]
fn a_rolled_file_shorter_than_the_cursor_yields_one_gap_and_restarts() {
    let root = temp("rolled-truncated");
    let live = root.0.join("x.jsonl");
    let rolled = root.0.join("x.jsonl.1");
    let limits = Limits::default();
    append(&live, &line(1));
    append(&live, &line(2));
    let cursor = read_batch(&live, Some(&rolled), &Cursor::default(), &limits).unwrap().cursor;

    // Rotate, then truncate the rolled file below the cursor's offset (it was shortened in place
    // before it became the rolled file), and leave a fresh line in the live file.
    rotate(&live, &rolled);
    std::fs::File::create(&rolled).unwrap();
    append(&rolled, &line(7));
    append(&live, &line(8));

    // The rolled file is shorter than the cursor named: one gap, restart it from the top, then read
    // on into the live file.
    let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
    assert_eq!(reasons(&batch), vec![GapReason::Truncated]);
    assert_eq!(seqs(&batch), vec![7, 8]);
}

#[test]
fn deletion_yields_one_gap_then_stays_silent() {
    let root = temp("deletion");
    let live = root.0.join("x.jsonl");
    let limits = Limits::default();
    append(&live, &line(1));
    let cursor = read_batch(&live, None, &Cursor::default(), &limits).unwrap().cursor;

    std::fs::remove_file(&live).unwrap();
    let batch = read_batch(&live, None, &cursor, &limits).unwrap();
    assert_eq!(reasons(&batch), vec![GapReason::Deleted]);
    assert_eq!(batch.cursor.file_id, None, "the cursor is unbound after a deletion");

    // A repeated call on the missing file does not repeat the gap.
    let again = read_batch(&live, None, &batch.cursor, &limits).unwrap();
    assert!(again.lines.is_empty() && again.gaps.is_empty());
}

#[test]
fn an_oversized_line_is_skipped_with_one_gap_and_the_rest_is_kept() {
    let root = temp("oversized");
    let live = root.0.join("x.jsonl");
    let limits = Limits {
        max_bytes: 8192,
        max_lines: 1000,
        max_line_bytes: 256,
    };
    append(&live, &line(1));
    let big = "x".repeat(400);
    append(&live, &format!("{{\"seq\":2,\"pad\":\"{big}\"}}\n"));
    append(&live, &line(3));

    let batch = read_batch(&live, None, &Cursor::default(), &limits).unwrap();
    assert_eq!(seqs(&batch), vec![1, 3]);
    assert_eq!(reasons(&batch), vec![GapReason::OversizedLine]);
}

#[test]
fn an_oversized_line_without_its_newline_is_not_consumed_yet() {
    let root = temp("oversized-half");
    let live = root.0.join("x.jsonl");
    let limits = Limits {
        max_bytes: 8192,
        max_lines: 1000,
        max_line_bytes: 256,
    };
    append(&live, &line(1));
    let cursor = read_batch(&live, None, &Cursor::default(), &limits).unwrap().cursor;

    // Half an oversized line, no newline: nothing is consumed and no gap is raised.
    append(&live, &"y".repeat(400));
    let waiting = read_batch(&live, None, &cursor, &limits).unwrap();
    assert!(waiting.lines.is_empty());
    assert!(waiting.gaps.is_empty(), "no newline yet: nothing skipped");
    assert_eq!(waiting.cursor.offset, cursor.offset, "nothing consumed");

    // The newline arrives: one gap, nothing else.
    append(&live, "\n");
    let skipped = read_batch(&live, None, &cursor, &limits).unwrap();
    assert!(skipped.lines.is_empty());
    assert_eq!(reasons(&skipped), vec![GapReason::OversizedLine]);
}

#[test]
fn a_rolled_file_ending_in_an_oversized_fragment_still_drains() {
    let root = temp("rolled-oversized");
    let live = root.0.join("x.jsonl");
    let rolled = root.0.join("x.jsonl.1");
    let limits = Limits {
        max_bytes: 8192,
        max_lines: 1000,
        max_line_bytes: 256,
    };

    // Bind the cursor to the live file, then leave an oversized fragment (no newline) as its tail.
    append(&live, &line(1));
    let cursor = read_batch(&live, Some(&rolled), &Cursor::default(), &limits).unwrap().cursor;
    append(&live, &"z".repeat(400));

    // Rotate: the oversized fragment is now the rolled file's final, unterminated line, and a line
    // waits in the fresh live file.
    rotate(&live, &rolled);
    append(&live, &line(2));

    // The rolled file must drain — one oversized_line gap for its final fragment — and only then can
    // the cursor reach the live line. A tailer that stalls inside the rolled file never yields it.
    let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
    assert_eq!(reasons(&batch), vec![GapReason::OversizedLine]);
    assert_eq!(seqs(&batch), vec![2], "the live line is reached once the rolled file drains");
    assert_eq!(
        batch.cursor.file_id,
        Some(FileId::of(&std::fs::metadata(&live).unwrap())),
        "the cursor moved on to the live file"
    );

    // It stays drained: a repeated call yields nothing and raises nothing.
    let again = read_batch(&live, Some(&rolled), &batch.cursor, &limits).unwrap();
    assert!(again.lines.is_empty() && again.gaps.is_empty());
}

#[test]
fn malformed_and_blank_lines_are_skipped_not_fatal() {
    let root = temp("malformed");
    let live = root.0.join("x.jsonl");
    append(&live, &line(1));
    append(&live, "\n   \n");
    append(&live, "not json\n");
    append(&live, &line(2));
    let batch = read_batch(&live, None, &Cursor::default(), &Limits::default()).unwrap();
    assert_eq!(seqs(&batch), vec![1, 2]);
    assert_eq!(batch.malformed, 1);
    assert!(batch.gaps.is_empty());
}

#[test]
fn gap_reasons_name_themselves_snake_case() {
    let all = [
        GapReason::RotatedPast,
        GapReason::Truncated,
        GapReason::Deleted,
        GapReason::OversizedLine,
        GapReason::StateReset,
    ];
    let names: Vec<&str> = all.iter().map(|reason| reason.as_str()).collect();
    assert_eq!(
        names,
        ["rotated_past", "truncated", "deleted", "oversized_line", "state_reset"]
    );
    for reason in all {
        assert_eq!(
            serde_json::to_value(Gap::new(reason)).unwrap(),
            json!({"reason": reason.as_str()})
        );
    }
}

// ---------------------------------------------------------------------------
// A large file, and a seeded randomised interleaving.
// ---------------------------------------------------------------------------

#[test]
fn a_hundred_thousand_lines_come_out_once_within_the_budget() {
    let root = temp("hundred-thousand");
    let live = root.0.join("x.jsonl");
    let max_bytes = 4 * 1024 * 1024;
    let limits = Limits {
        max_bytes,
        max_lines: usize::MAX,
        max_line_bytes: max_bytes as usize,
    };
    let mut body = String::with_capacity(6 * 1024 * 1024);
    for i in 0..100_000u64 {
        body.push_str(&format!("{{\"seq\":{i},\"pad\":\"{}\"}}\n", "p".repeat(40)));
    }
    std::fs::write(&live, &body).unwrap();

    let mut cursor = Cursor::default();
    let mut seen = Vec::with_capacity(100_000);
    let mut batches = 0;
    loop {
        let batch = read_batch(&live, None, &cursor, &limits).unwrap();
        assert!(
            batch.bytes_read <= max_bytes,
            "a batch stayed within its budget: {}",
            batch.bytes_read
        );
        assert!(batch.lines.len() <= limits.max_lines);
        batches += 1;
        for line in &batch.lines {
            seen.push(line["seq"].as_u64().unwrap());
        }
        if batch.lines.is_empty() {
            break;
        }
        cursor = batch.cursor;
    }
    assert!(batches > 1, "the file did not fit one batch");
    assert_eq!(seen.len(), 100_000);
    assert_eq!(seen, (0..100_000).collect::<Vec<_>>());
}

fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    Line(u64),
    RotatedPast,
}

fn read_into(live: &Path, rolled: &Path, cursor: &mut Cursor, limits: &Limits, events: &mut Vec<Event>, rotations: &mut u32) {
    let batch = read_batch(live, Some(rolled), cursor, limits).unwrap();
    accept(&batch, events, *rotations);
    *cursor = batch.cursor;
    // Holding the live file again means the reader is caught up with the writer: a file can only be
    // lost to a rotation once two fresh rotations happen from here.
    if caught_up(live, cursor) {
        *rotations = 0;
    }
}

/// Whether the cursor sits in the live file, i.e. on the writer's current generation.
fn caught_up(live: &Path, cursor: &Cursor) -> bool {
    std::fs::metadata(live)
        .ok()
        .is_some_and(|meta| cursor.file_id == Some(FileId::of(&meta)))
}

fn record(batch: &Batch, events: &mut Vec<Event>) {
    for gap in &batch.gaps {
        assert_eq!(
            gap.reason,
            GapReason::RotatedPast,
            "the simulation only loses lines to rotation"
        );
        events.push(Event::RotatedPast);
    }
    for line in &batch.lines {
        events.push(Event::Line(line["seq"].as_u64().unwrap()));
    }
}

/// Files a batch, asserting a `rotated_past` gap only ever follows the two rotations it takes to
/// lose a file: one rotation is survivable through the rolled file, so a gap after fewer is a bug.
fn accept(batch: &Batch, events: &mut Vec<Event>, rotations: u32) {
    if batch.gaps.iter().any(|gap| gap.reason == GapReason::RotatedPast) {
        assert!(
            rotations >= 2,
            "a rotated_past gap after only {rotations} rotation(s) since the reader held the live file"
        );
    }
    record(batch, events);
}

/// No line twice, in order, and every skipped sequence number covered by a `rotated_past` gap at
/// the point it was skipped.
fn check_events(events: &[Event], max_seq: u64) {
    let mut last = 0u64;
    let mut expected = 1u64;
    let mut pending = false;
    for event in events {
        match *event {
            Event::RotatedPast => pending = true,
            Event::Line(seq) => {
                assert!(seq > last, "a line was yielded twice or out of order: {seq} after {last}");
                if seq > expected {
                    assert!(pending, "sequence {expected}..{seq} was skipped without a rotated_past gap");
                }
                pending = false;
                expected = seq + 1;
                last = seq;
            }
        }
    }
    assert!(
        expected > max_seq || pending,
        "trailing sequences {expected}..={max_seq} were skipped without a rotated_past gap"
    );
}

fn simulation(seed: u64, steps: usize) {
    let root = temp(&format!("sim-{seed}"));
    let live = root.0.join("live.jsonl");
    let rolled = root.0.join("live.jsonl.1");
    // Small limits, so a batch is short, reads are frequent and rotations have to be handled.
    let limits = Limits {
        max_bytes: 512,
        max_lines: 4,
        max_line_bytes: 4096,
    };
    let mut rng = seed | 1;
    let mut cursor = Cursor::default();
    // A line being written in two parts: the sequence number of the line whose opening fragment is
    // already on disk, waiting for its closing `}\n`.
    let mut half: Option<u64> = None;
    let mut events: Vec<Event> = Vec::new();
    // Rotations since the reader last held the live file; a `rotated_past` gap needs two of them.
    let mut rotations = 0u32;
    // Bind the cursor before the randomised steps: a fresh reader starts where it starts, so a line
    // rotated into the rolled file before the first read is not its to replay.
    append(&live, &line(1));
    read_into(&live, &rolled, &mut cursor, &limits, &mut events, &mut rotations);
    let mut next_seq = 2u64;
    let mut max_seq = 1u64;

    for _ in 0..steps {
        let roll = next_rand(&mut rng) % 100;
        // Never rotate while a line is half-written; a read may land between the halves.
        if let Some(seq) = half.take() {
            if roll < 80 {
                append(&live, "}\n");
                max_seq = max_seq.max(seq);
            } else {
                read_into(&live, &rolled, &mut cursor, &limits, &mut events, &mut rotations);
                half = Some(seq);
            }
            continue;
        }
        match roll {
            0..=11 => {
                // Read straight after a rotation, so the write of a fresh live file always happens
                // while the old one still exists, which keeps its identity distinct from anything
                // the cursor could name.
                rotations += 1;
                rotate(&live, &rolled);
                read_into(&live, &rolled, &mut cursor, &limits, &mut events, &mut rotations);
            }
            12..=27 => {
                read_into(&live, &rolled, &mut cursor, &limits, &mut events, &mut rotations);
            }
            _ => {
                let seq = next_seq;
                next_seq += 1;
                if roll.is_multiple_of(7) {
                    append(&live, &format!("{{\"seq\":{seq}"));
                    half = Some(seq);
                } else {
                    append(&live, &format!("{{\"seq\":{seq}}}\n"));
                    max_seq = max_seq.max(seq);
                }
            }
        }
    }
    if let Some(seq) = half.take() {
        append(&live, "}\n");
        max_seq = max_seq.max(seq);
    }

    // Drain everything left. Bounded, so a tailer that keeps making no real progress fails instead
    // of spinning.
    let mut drained = 0u32;
    loop {
        drained += 1;
        assert!(drained <= 100_000, "the drain did not terminate");
        let before = cursor.clone();
        let batch = read_batch(&live, Some(&rolled), &cursor, &limits).unwrap();
        accept(&batch, &mut events, rotations);
        cursor = batch.cursor;
        if caught_up(&live, &cursor) {
            rotations = 0;
        }
        if (batch.lines.is_empty() && batch.gaps.is_empty()) || cursor == before {
            break;
        }
    }

    // The highest sequence written is the highest yielded: nothing at the tail was lost.
    let highest = events
        .iter()
        .filter_map(|event| match *event {
            Event::Line(seq) => Some(seq),
            Event::RotatedPast => None,
        })
        .max();
    assert_eq!(highest, Some(max_seq), "the highest sequence written was not yielded");
    check_events(&events, max_seq);
}

#[test]
fn a_seeded_random_interleaving_never_yields_a_line_twice() {
    for seed in [1u64, 2, 3, 5, 8, 13, 21, 34] {
        simulation(seed, 4000);
    }
}
