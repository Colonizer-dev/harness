//! The durable half of the tailer (#842): where each stream's cursor lives, and the one atomic
//! write that commits a batch. The file is `<data_dir>/observability/state.json`.

use super::cursor::{Cursor, Gap, GapReason};
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The directory under the data dir the state lives in.
pub(crate) const DIR: &str = "observability";
/// The state file's name.
pub(crate) const FILE: &str = "state.json";
/// The only version this code writes; any other is treated as unusable and reset.
pub(crate) const VERSION: u32 = 1;
/// The shortest gap between two non-forced commits.
const COMMIT_INTERVAL: Duration = Duration::from_secs(1);

/// Which stream a cursor belongs to. Per signal, so a stalled logs source does not hold back a
/// traces source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Signal {
    Logs,
    Traces,
    Metrics,
}

/// A cursor's key: one file of one stream to one destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CursorKey {
    pub(crate) destination_hash: String,
    pub(crate) signal: Signal,
    pub(crate) relative_path: String,
}

/// Every stream's cursor, plus room for anything later work wants to commit in the same write —
/// open-span state, kept in `extra` so a cursor and the span it belongs to land together.
#[derive(Clone, Debug)]
pub(crate) struct State {
    pub(crate) version: u32,
    pub(crate) cursors: BTreeMap<CursorKey, Cursor>,
    pub(crate) extra: BTreeMap<String, Value>,
    /// When this state was last written. Not serialised: it only rate-limits `commit`.
    last_write: Option<Instant>,
}

impl Default for State {
    fn default() -> Self {
        State {
            version: VERSION,
            cursors: BTreeMap::new(),
            extra: BTreeMap::new(),
            last_write: None,
        }
    }
}

impl State {
    /// The cursor for a stream, or an unbound one if it has none.
    pub(crate) fn cursor(&self, key: &CursorKey) -> Cursor {
        self.cursors.get(key).cloned().unwrap_or_default()
    }

    /// Records where a stream got to.
    pub(crate) fn set_cursor(&mut self, key: CursorKey, cursor: Cursor) {
        self.cursors.insert(key, cursor);
    }

    /// Reads the state, never failing: a missing file is an empty state with no gap; an unknown
    /// version, unreadable file or corrupt JSON is an empty state with one [`GapReason::StateReset`].
    pub(crate) fn load(data_dir: &Path) -> (State, Option<Gap>) {
        let bytes = match std::fs::read(state_file(data_dir)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (State::default(), None),
            Err(_) => return (State::default(), Some(Gap::new(GapReason::StateReset))),
        };
        match serde_json::from_slice::<State>(&bytes) {
            Ok(state) if state.version == VERSION => (state, None),
            _ => (State::default(), Some(Gap::new(GapReason::StateReset))),
        }
    }

    /// Writes the state atomically, at most once a second unless `force`. Returns whether it wrote.
    pub(crate) async fn commit(&mut self, data_dir: &Path, force: bool) -> anyhow::Result<bool> {
        let now = Instant::now();
        if !force
            && let Some(last) = self.last_write
            && now.duration_since(last) < COMMIT_INTERVAL
        {
            return Ok(false);
        }
        tokio::fs::create_dir_all(data_dir.join(DIR)).await?;
        util::write_atomic(&state_file(data_dir), &serde_json::to_vec(&*self)?).await?;
        self.last_write = Some(now);
        Ok(true)
    }
}

/// The state file's path: `<data>/observability/state.json`.
pub(crate) fn state_file(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR).join(FILE)
}

/// JSON object keys must be strings, so `cursors` is written as an array of entries even though it
/// is a `BTreeMap` in memory.
impl Serialize for State {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Entry<'a> {
            destination_hash: &'a str,
            signal: Signal,
            relative_path: &'a str,
            cursor: &'a Cursor,
        }
        #[derive(Serialize)]
        struct File<'a> {
            version: u32,
            cursors: Vec<Entry<'a>>,
            extra: &'a BTreeMap<String, Value>,
        }
        let cursors = self
            .cursors
            .iter()
            .map(|(key, cursor)| Entry {
                destination_hash: &key.destination_hash,
                signal: key.signal,
                relative_path: &key.relative_path,
                cursor,
            })
            .collect();
        File {
            version: self.version,
            cursors,
            extra: &self.extra,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for State {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<State, D::Error> {
        #[derive(Deserialize)]
        struct Entry {
            destination_hash: String,
            signal: Signal,
            relative_path: String,
            #[serde(default)]
            cursor: Cursor,
        }
        #[derive(Deserialize)]
        struct File {
            version: u32,
            #[serde(default)]
            cursors: Vec<Entry>,
            #[serde(default)]
            extra: BTreeMap<String, Value>,
        }
        let file = File::deserialize(deserializer)?;
        let cursors = file
            .cursors
            .into_iter()
            .map(|entry| {
                (
                    CursorKey {
                        destination_hash: entry.destination_hash,
                        signal: entry.signal,
                        relative_path: entry.relative_path,
                    },
                    entry.cursor,
                )
            })
            .collect();
        Ok(State {
            version: file.version,
            cursors,
            extra: file.extra,
            last_write: None,
        })
    }
}

#[cfg(test)]
mod tests;
