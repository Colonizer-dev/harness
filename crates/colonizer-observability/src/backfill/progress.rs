//! `<data>/observability/backfill.json` (#858): the backfill's own cursors, trace state and
//! counters — the one file a backfill writes, and the one a dry run writes not even. Shaped like
//! the live exporter's `state.json` (same cursor entries, same destination keying), so a reader of
//! one reads the other. A module of the engine's own ([`super`]).

use crate::cursor::Cursor;
use crate::state::{CursorKey, Signal};
use crate::traces::Traces;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `--since`/`--until` window, as the progress file records it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Window {
    pub(crate) since: Option<u64>,
    pub(crate) until: Option<u64>,
}

impl Window {
    pub(crate) fn bounded(&self) -> bool {
        self.since.is_some() || self.until.is_some()
    }

    /// Whether a timestamped line falls inside the window.
    pub(crate) fn keeps(&self, ts: u64) -> bool {
        self.since.is_none_or(|since| ts >= since) && self.until.is_none_or(|until| ts < until)
    }
}

/// One cursor entry of the progress file, shaped exactly like `state.json`'s entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CursorEntry {
    destination_hash: String,
    signal: Signal,
    relative_path: String,
    /// The offset the next run of this plan resumes from.
    pub(crate) cursor: Cursor,
}

impl CursorEntry {
    /// The cursor's key, rebuilt for the map.
    pub(crate) fn key(&self) -> CursorKey {
        CursorKey {
            destination_hash: self.destination_hash.clone(),
            signal: self.signal,
            relative_path: self.relative_path.clone(),
        }
    }

    /// The entry for a committed cursor.
    pub(crate) fn new(key: &CursorKey, cursor: &Cursor) -> CursorEntry {
        CursorEntry {
            destination_hash: key.destination_hash.clone(),
            signal: key.signal,
            relative_path: key.relative_path.clone(),
            cursor: cursor.clone(),
        }
    }
}

/// The whole run's persisted state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Progress {
    pub(crate) version: u32,
    pub(crate) destination: String,
    pub(crate) state: String,
    pub(crate) error: Option<String>,
    pub(crate) window: Window,
    pub(crate) until_offsets: BTreeMap<String, u64>,
    pub(crate) cursors: Vec<CursorEntry>,
    pub(crate) traces: BTreeMap<String, Traces>,
    pub(crate) records_sent: u64,
    pub(crate) bytes_sent: u64,
    pub(crate) started_unix: u64,
    pub(crate) updated_unix: u64,
}

/// `<data>/observability/backfill.json`.
pub fn progress_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("backfill.json")
}
