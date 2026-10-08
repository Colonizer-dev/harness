//! The multi-source tailer (#843): every source of the registry, read fairly within a byte budget,
//! with the colony list from the contract, gap records for every hole, and colonies that are done
//! left alone.
//!
//! - **Colonies** come from the contract's colony list, plus any colony the state still holds a
//!   read position for (so a colony that just left the list is still drained or reported deleted).
//!   A colony launched while the exporter runs is read on the next tick after the mothership lists
//!   it.
//! - **Drained:** a colony whose status is finished (`merged`, `no_changes`, `stopped`, `failed`)
//!   and whose every file has been read to the end and left untouched for [`DRAIN_GRACE`] is not
//!   looked at again — one `stat` of its directory per tick, no file opened — until its status or
//!   its directory's mtime changes.
//! - **Deleted:** a colony whose directory is gone while the state still holds read positions in
//!   it yields one `export_gap` record (reason `deleted`, and whether `archive/` holds a bundle of
//!   it), its read positions are dropped, and it is never stat'ed again.
//! - **Fairness:** each tick shares the byte budget and the record cap equally among the files
//!   with something to read, in passes, starting one file further along each tick, so one chatty
//!   colony cannot starve the rest. A token bucket holds the read rate to `max_read_mib_per_sec`
//!   across ticks.
//! - **Backlog guard:** lines older than `max_backlog_days` are skipped and counted per line, and
//!   each run of skipped lines yields one `export_gap` (reason `backlog`, the bytes and lines
//!   skipped). A cursor gap (`rotated_past`, `truncated`, `deleted`, `oversized_line`) yields one
//!   too.
//!
//! Nothing here touches the mothership's session state or any lock of it: the exporter is its own
//! process and reads only the ledgers and the contract.

use crate::contract::ColonyPolicy;
use crate::cursor::{Cursor, GapReason, Limits, read_batch};
use crate::policy::Source;
use crate::sources::{self, SourceFile};
use crate::state::{CursorKey, Signal, State};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The most records one tick yields; the rest wait on disk for the next tick.
pub(crate) const MAX_ITEMS_PER_TICK: usize = 20_000;
/// The longest line read as a line; anything longer is skipped with an `oversized_line` gap.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// The smallest byte share a file gets in a pass, so a pass over many files still reads whole lines.
const MIN_SHARE: u64 = 16 * 1024;
/// How long a finished colony's files must sit unchanged before the colony counts as drained, so a
/// line written just after the status flipped is still read.
pub(crate) const DRAIN_GRACE: Duration = Duration::from_secs(60);
/// The statuses a colony is finished in. `pr_opened` and `closed` are not: their pull request is
/// still watched and their ledgers still grow.
const FINISHED: [&str; 4] = ["merged", "no_changes", "stopped", "failed"];
/// The key of the deleted colonies' list in `state.json`'s `extra`.
pub(crate) const GONE_KEY: &str = "gone";

/// One line read from one source, before mapping.
#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub source: Source,
    pub signal: Signal,
    /// The colony a per-colony file belongs to (or a gap record is about).
    pub colony: Option<String>,
    pub line: Value,
    /// The SHA-256 of the line's raw bytes: the record id's key where the source has no `seq`.
    pub digest: [u8; 32],
}

/// What one tick read: the records, the gap records, and what to commit once they are delivered.
#[derive(Debug, Default)]
pub(crate) struct Tailed {
    pub records: Vec<Record>,
    /// `export_gap` records (stream `meta`), mapped like any other record.
    pub gaps: Vec<Record>,
    pub cursors: Vec<(CursorKey, Cursor)>,
    /// The read positions of deleted colonies, dropped on commit.
    pub removed: Vec<CursorKey>,
    /// Colonies found deleted this tick, never stat'ed again once committed.
    pub gone: Vec<String>,
    /// Drop counts by reason (`backlog`, `malformed`, `gap_<reason>`).
    pub drops: BTreeMap<String, u64>,
    /// Bytes read from the ledgers this tick.
    pub bytes_read: u64,
}

impl Tailed {
    fn drop(&mut self, reason: &str, n: u64) {
        if n > 0 {
            *self.drops.entry(reason.to_string()).or_default() += n;
        }
    }
}

/// What the tailer reads against: the contract's colonies and settings, and the committed state.
pub(crate) struct Inputs<'a> {
    pub data_dir: &'a Path,
    pub settings: &'a crate::contract::Settings,
    pub colonies: &'a BTreeMap<String, ColonyPolicy>,
    pub state: &'a State,
    pub destination: &'a str,
    pub now_unix_nanos: u64,
}

/// A token bucket over bytes read: refilled at the rate, holding at most one second's worth.
#[derive(Debug, Default)]
struct Bucket {
    tokens: f64,
    last: Option<tokio::time::Instant>,
}

impl Bucket {
    /// The bytes that may be read now, at `rate` bytes a second.
    fn available(&mut self, rate: u64) -> u64 {
        let now = tokio::time::Instant::now();
        let cap = rate as f64;
        self.tokens = match self.last {
            None => cap,
            Some(last) => (self.tokens + now.duration_since(last).as_secs_f64() * cap).min(cap),
        };
        self.last = Some(now);
        self.tokens.max(0.0) as u64
    }

    /// Takes `n` bytes; a line bigger than what was left goes into debt, paid back by the next
    /// refills.
    fn spend(&mut self, n: u64) {
        self.tokens -= n as f64;
    }
}

/// A drained colony: what it looked like when it was found drained.
#[derive(Clone, Debug, PartialEq)]
struct Drained {
    status: Option<String>,
    dir_mtime: Option<SystemTime>,
}

/// The tailer's in-memory state across ticks. Everything durable is in [`State`].
#[derive(Debug, Default)]
pub(crate) struct Tailer {
    rotation: usize,
    bucket: Bucket,
    drained: BTreeMap<String, Drained>,
    /// Bytes and lines of the backlog run each logs source is in the middle of skipping.
    skipping: BTreeMap<String, (u64, u64)>,
}

/// How a file looks before it is read.
enum Look {
    /// Not there, and nothing was ever read from it.
    Missing,
    /// Read to the end, last modified at this time.
    AtEnd(Option<SystemTime>),
    /// Something to read (or a rotation to follow).
    Pending,
}

fn look(file: &SourceFile, cursor: &Cursor) -> Look {
    match std::fs::metadata(&file.live) {
        Err(_) if cursor.file_id.is_none() => Look::Missing,
        Err(_) => Look::Pending,
        // A rolled predecessor means a rotation may need following, so the reader decides.
        Ok(_) if file.rolled.is_some() && cursor.file_id.is_some() && rolled_exists(file) => Look::Pending,
        Ok(meta) if cursor.file_id.is_some() && meta.len() == cursor.offset => Look::AtEnd(meta.modified().ok()),
        Ok(_) => Look::Pending,
    }
}

/// Whether `cursor` sits at the end of the live file: nothing more to read this tick.
fn caught_up(file: &SourceFile, cursor: &Cursor) -> bool {
    match std::fs::metadata(&file.live) {
        Ok(meta) => cursor.file_id == Some(crate::cursor::FileId::of(&meta)) && cursor.offset >= meta.len(),
        // No live file: whatever was there is read (or reported as a gap).
        Err(_) => true,
    }
}

fn rolled_exists(file: &SourceFile) -> bool {
    file.rolled.as_ref().is_some_and(|p| p.exists())
}

/// The ids of the colonies the state holds read positions for, under this destination.
fn colonies_with_cursors(state: &State, destination: &str) -> BTreeSet<String> {
    state
        .cursors
        .keys()
        .filter(|k| k.destination_hash == destination)
        .filter_map(|k| sources::colony_of(&k.relative_path).map(str::to_string))
        .collect()
}

/// The colonies found deleted earlier, as committed in `state.json`.
pub(crate) fn gone(state: &State) -> BTreeSet<String> {
    state
        .extra
        .get(GONE_KEY)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Whether `archive/` holds a bundle of colony `id`: a sidecar named `<id>.json` or `<id>.r<N>.json`
/// whose `session` is the id (crates/colonizer/src/archive.rs). Symlinks are never followed.
pub(crate) fn archived(data_dir: &Path, id: &str) -> bool {
    let mut pending = vec![data_dir.join("archive")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else { continue };
            let revision = stem
                .strip_prefix(id)
                .is_some_and(|rest| rest.is_empty() || rest.strip_prefix(".r").is_some_and(|n| n.parse::<u32>().is_ok()));
            if kind.is_file()
                && revision
                && let Ok(bytes) = std::fs::read(&path)
                && let Ok(record) = serde_json::from_slice::<Value>(&bytes)
                && record.get("session").and_then(Value::as_str) == Some(id)
            {
                return true;
            }
        }
    }
    false
}

/// An `export_gap` record. `bytes` and `lines` are what was skipped, when known.
pub(crate) fn gap_record(
    now_unix_nanos: u64,
    reason: &str,
    colony: Option<&str>,
    file: Option<&str>,
    bytes: Option<u64>,
    lines: Option<u64>,
    archived: Option<bool>,
) -> Record {
    let ts = chrono::DateTime::from_timestamp_nanos(now_unix_nanos as i64).to_rfc3339();
    let mut line = json!({"type": "export_gap", "ts": ts, "reason": reason});
    let fields = [
        ("colony", colony.map(Value::from)),
        ("file", file.map(Value::from)),
        ("bytes", bytes.map(Value::from)),
        ("lines", lines.map(Value::from)),
        ("archived", archived.map(Value::from)),
    ];
    for (key, value) in fields {
        if let Some(value) = value {
            line[key] = value;
        }
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, line.to_string().as_bytes());
    let mut bytes32 = [0u8; 32];
    bytes32.copy_from_slice(digest.as_ref());
    Record {
        source: Source::ExportGap,
        signal: Signal::Logs,
        colony: colony.map(str::to_string),
        line,
        digest: bytes32,
    }
}

impl Tailer {
    /// The colonies to read this tick: the contract's list and those with read positions, minus
    /// the deleted ones. A deleted colony is reported here, once.
    fn colonies(&mut self, inputs: &Inputs, out: &mut Tailed) -> Vec<String> {
        let gone = gone(inputs.state);
        let with_cursors = colonies_with_cursors(inputs.state, inputs.destination);
        let candidates: BTreeSet<&String> = inputs.colonies.keys().chain(with_cursors.iter()).collect();
        let mut active = Vec::new();
        for id in candidates {
            if gone.contains(id) || !sources::is_colony_id(id) {
                continue;
            }
            let dir = inputs.data_dir.join("sessions").join(id);
            match std::fs::metadata(&dir) {
                Ok(meta) => {
                    let status = inputs.colonies.get(id).and_then(|p| p.status.clone());
                    let now = Drained {
                        status,
                        dir_mtime: meta.modified().ok(),
                    };
                    match self.drained.get(id) {
                        Some(then) if *then == now => {}
                        _ => {
                            self.drained.remove(id);
                            active.push(id.clone());
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && with_cursors.contains(id) => {
                    out.removed.extend(
                        inputs
                            .state
                            .cursors
                            .keys()
                            .filter(|k| {
                                k.destination_hash == inputs.destination
                                    && sources::colony_of(&k.relative_path) == Some(id.as_str())
                            })
                            .cloned(),
                    );
                    out.gaps.push(gap_record(
                        inputs.now_unix_nanos,
                        GapReason::Deleted.as_str(),
                        Some(id),
                        Some(&format!("sessions/{id}")),
                        None,
                        None,
                        Some(archived(inputs.data_dir, id)),
                    ));
                    out.drop("gap_deleted", 1);
                    out.gone.push(id.clone());
                    self.drained.remove(id);
                }
                // Listed but not created yet, or unreadable: nothing to read this tick.
                Err(_) => {}
            }
        }
        active
    }

    /// Reads what is new in every source, within this tick's budget.
    pub fn collect(&mut self, inputs: &Inputs) -> Tailed {
        let mut out = Tailed::default();
        let settings = inputs.settings;
        let colonies = self.colonies(inputs, &mut out);
        let files = sources::discover(inputs.data_dir, settings, &colonies);
        let rate = settings.max_read_mib_per_sec.max(1).saturating_mul(1024 * 1024);
        let mut budget = self.bucket.available(rate);
        let cutoff = match settings.max_backlog_days {
            0 => 0,
            days => inputs
                .now_unix_nanos
                .saturating_sub(days.saturating_mul(86_400 * 1_000_000_000)),
        };

        // A colony stays a drain candidate only while every one of its files is at its end and old.
        let mut candidates: BTreeMap<&str, bool> = colonies
            .iter()
            .filter(|id| {
                inputs
                    .colonies
                    .get(id.as_str())
                    .and_then(|p| p.status.as_deref())
                    .is_some_and(|s| FINISHED.contains(&s))
            })
            .map(|id| (id.as_str(), true))
            .collect();
        let grace_cutoff = SystemTime::now().checked_sub(DRAIN_GRACE);

        let n = files.len();
        let start = if n == 0 { 0 } else { self.rotation % n };
        self.rotation = self.rotation.wrapping_add(1);
        let mut cursors: BTreeMap<usize, Cursor> = BTreeMap::new();
        let mut active: Vec<usize> = Vec::new();
        for i in 0..n {
            let at = (start + i) % n;
            let file = &files[at];
            let cursor = inputs.state.cursor(&key(inputs.destination, file));
            let pending = match look(file, &cursor) {
                Look::Missing => false,
                Look::AtEnd(mtime) => {
                    let old = matches!((mtime, grace_cutoff), (Some(m), Some(g)) if m < g);
                    if !old && let Some(c) = file.colony.as_deref().and_then(|c| candidates.get_mut(c)) {
                        *c = false;
                    }
                    false
                }
                Look::Pending => true,
            };
            if pending {
                if let Some(c) = file.colony.as_deref().and_then(|c| candidates.get_mut(c)) {
                    *c = false;
                }
                cursors.insert(at, cursor);
                active.push(at);
            }
        }

        while budget > 0 && out.records.len() < MAX_ITEMS_PER_TICK && !active.is_empty() {
            let share_bytes = (budget / active.len() as u64).max(MIN_SHARE);
            let share_lines = ((MAX_ITEMS_PER_TICK - out.records.len()) / active.len()).max(1);
            let mut again = Vec::new();
            for at in active {
                if budget == 0 || out.records.len() >= MAX_ITEMS_PER_TICK {
                    break;
                }
                let file = &files[at];
                let cursor = cursors[&at].clone();
                let limits = Limits {
                    max_bytes: share_bytes.min(budget),
                    max_lines: share_lines.min(MAX_ITEMS_PER_TICK - out.records.len()),
                    max_line_bytes: MAX_LINE_BYTES,
                };
                let Ok(batch) = read_batch(&file.live, file.rolled.as_deref(), &cursor, &limits) else {
                    continue;
                };
                budget = budget.saturating_sub(batch.bytes_read);
                self.bucket.spend(batch.bytes_read);
                out.bytes_read += batch.bytes_read;
                let full = !caught_up(file, &batch.cursor);
                self.take(inputs, file, &batch, cutoff, full, &mut out);
                if batch.cursor != cursor {
                    cursors.insert(at, batch.cursor.clone());
                    if full && batch.bytes_read > 0 {
                        again.push(at);
                    }
                }
            }
            active = again;
        }

        for (at, cursor) in cursors {
            let file = &files[at];
            let k = key(inputs.destination, file);
            if inputs.state.cursor(&k) != cursor {
                out.cursors.push((k, cursor));
            }
        }
        for (id, drained) in candidates {
            if drained {
                let dir = inputs.data_dir.join("sessions").join(id);
                self.drained.insert(
                    id.to_string(),
                    Drained {
                        status: inputs.colonies.get(id).and_then(|p| p.status.clone()),
                        dir_mtime: std::fs::metadata(&dir).and_then(|m| m.modified()).ok(),
                    },
                );
            }
        }
        out
    }

    /// Files one batch: its gaps as gap records and counts, its lines as records, and the lines
    /// past the backlog window as one `backlog` gap per run.
    fn take(
        &mut self,
        inputs: &Inputs,
        file: &SourceFile,
        batch: &crate::cursor::Batch,
        cutoff: u64,
        full: bool,
        out: &mut Tailed,
    ) {
        let logs = file.signal == Signal::Logs;
        let colony = file.colony.as_deref();
        for gap in &batch.gaps {
            out.drop(&format!("gap_{}", gap.reason.as_str()), 1);
            if logs && gap.reason != GapReason::StateReset {
                out.gaps.push(gap_record(
                    inputs.now_unix_nanos,
                    gap.reason.as_str(),
                    colony,
                    Some(&file.relative),
                    None,
                    None,
                    None,
                ));
            }
        }
        out.drop("malformed", batch.malformed);
        let run_key = file.relative.clone();
        for ((line, digest), size) in batch.lines.iter().zip(&batch.digests).zip(&batch.sizes) {
            if cutoff > 0 && crate::map::ts_nanos(line).is_some_and(|ts| ts < cutoff) {
                out.drop("backlog", 1);
                if logs {
                    let run = self.skipping.entry(run_key.clone()).or_default();
                    run.0 += size;
                    run.1 += 1;
                }
                continue;
            }
            if logs {
                self.end_run(inputs, file, out);
            }
            out.records.push(Record {
                source: file.source,
                signal: file.signal,
                colony: file.colony.clone(),
                line: line.clone(),
                digest: *digest,
            });
        }
        // Caught up with the file: a run still open ends here.
        if logs && !full {
            self.end_run(inputs, file, out);
        }
    }

    /// Ends `file`'s backlog run, if it is in one, with its one `backlog` gap.
    fn end_run(&mut self, inputs: &Inputs, file: &SourceFile, out: &mut Tailed) {
        if let Some((bytes, lines)) = self.skipping.remove(&file.relative) {
            out.gaps.push(gap_record(
                inputs.now_unix_nanos,
                "backlog",
                file.colony.as_deref(),
                Some(&file.relative),
                Some(bytes),
                Some(lines),
                None,
            ));
        }
    }
}

/// A file's cursor key under `destination`.
pub(crate) fn key(destination: &str, file: &SourceFile) -> CursorKey {
    CursorKey {
        destination_hash: destination.to_string(),
        signal: file.signal,
        relative_path: file.relative.clone(),
    }
}

/// Binds every existing file's cursor to its end, for a destination that starts `now`. A file that
/// does not exist yet gets no cursor, so it is read from its first line once it appears.
pub(crate) fn seed_at_end(inputs: &Inputs, state: &mut State) {
    let colonies: Vec<String> = inputs.colonies.keys().cloned().collect();
    for file in sources::discover(inputs.data_dir, inputs.settings, &colonies) {
        if let Ok(Some(cursor)) = crate::cursor::at_end(&file.live) {
            state.set_cursor(key(inputs.destination, &file), cursor);
        }
    }
}

#[cfg(test)]
mod tests;
