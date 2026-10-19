//! The exporter's health (#849), written to `<data>/observability/status.json` for the mothership's
//! status API. It holds counts, states and scrubbed error strings, never a header or a record.
//!
//! States, overall and per signal:
//! - `starting`: no request has been answered yet.
//! - `running` (per signal `ok`): the last request was acknowledged.
//! - `retrying` (per signal `backing_off`): the network, a timeout, 408, 429, 5xx or a status that
//!   says nothing about the records; the batch is kept and retried at `next_retry_unix`.
//! - `auth_failed`: the backend refused the credential (401, 403, 407). Nothing is dropped and
//!   nothing is committed; the batch is retried with backoff until the key works again.
//! - `off` (per signal): the signal's stream is switched off.
//! - `stopped`: a clean shutdown.

use crate::cursor::{Cursor, FileId};
use crate::sources::SourceFile;
use crate::state::{Signal, State};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub contract: u32,
    pub state: &'static str,
    /// The base endpoint (the mothership refuses credentials and query strings in it).
    pub endpoint: String,
    pub heartbeat_unix: u64,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    /// When the held batch is next tried, while `retrying` or `auth_failed`.
    pub next_retry_unix: Option<u64>,
    /// Failed requests in a row, across signals; reset by the next acknowledged one.
    pub consecutive_failures: u64,
    pub exported: u64,
    /// Request bytes acknowledged since the add-on started (after gzip).
    pub bytes_sent: u64,
    /// Ledger bytes not yet acknowledged, from the committed cursors (an estimate).
    pub backlog_bytes: u64,
    pub dropped: BTreeMap<String, u64>,
    pub export_failures: u64,
    /// The last `partial_success` a backend answered with: what it rejected and why.
    pub last_partial_success: Option<PartialSuccess>,
    pub signals: BTreeMap<&'static str, SignalStatus>,
}

/// One signal's health.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SignalStatus {
    pub state: &'static str,
    /// The URL this signal is sent to.
    pub endpoint: String,
    pub consecutive_failures: u64,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    pub records_sent: u64,
    pub bytes_sent: u64,
    /// Ledger bytes of this signal not yet behind a committed cursor (its share of the status's
    /// global `backlog_bytes`).
    pub backlog_bytes: u64,
    /// Records of this signal dropped rather than sent: refused by the backend, past the backlog
    /// window, oversized, malformed, or lost to a hole. The per-signal mirror of the `dropped` map.
    pub records_dropped: u64,
    /// Holes in what this signal could read (rotation, truncation, backlog expiry, a deleted
    /// colony): one per `export_gap` record the tailer produced for it.
    pub gaps: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PartialSuccess {
    pub at_unix: u64,
    pub signal: &'static str,
    pub rejected: u64,
    pub message: Option<String>,
}

/// Per-signal request-size budgets, held in place of [`crate::batch::DEFAULT_REQUEST_BYTES`] for
/// the rest of the process once a `413` says the default no longer fits the backend. Not persisted:
/// a restart re-reads the config and starts at the default again.
#[derive(Clone, Debug, Default)]
pub struct Caps(BTreeMap<&'static str, usize>);

/// The smallest a `413`-shrunk budget may fall: 16 KiB, six halvings from the 1 MiB default. Below
/// this the resource-and-scope envelope alone would crowd out almost every record, so the shrink
/// path would truncate or drop records rather than fit more per request, and a limit that small is
/// not one a real collector sets. ([`crate::batch::MIN_REQUEST_BYTES`] is the *config's* floor.)
const MIN_CAP_BYTES: usize = 16 * 1024;

impl Caps {
    /// The budget requests of `signal` are built within.
    pub fn get(&self, signal: &'static str) -> usize {
        self.0.get(signal).copied().unwrap_or(crate::batch::DEFAULT_REQUEST_BYTES)
    }

    /// Remembers half `signal`'s budget, floored at [`MIN_CAP_BYTES`].
    pub fn halve(&mut self, signal: &'static str) {
        let next = (self.get(signal) / 2).max(MIN_CAP_BYTES);
        self.0.insert(signal, next);
    }

    /// Whether `signal`'s budget was shrunk, and to what.
    pub fn shrunk_to(&self, signal: &'static str) -> Option<usize> {
        self.0.get(signal).copied()
    }
}

impl Status {
    /// The signal's entry, created on first use.
    pub fn signal(&mut self, name: &'static str) -> &mut SignalStatus {
        self.signals.entry(name).or_insert_with(|| SignalStatus {
            state: "starting",
            ..SignalStatus::default()
        })
    }

    /// An acknowledged request of `records` records and `bytes` wire bytes.
    pub fn success(&mut self, signal: &'static str, now_unix: u64, records: u64, bytes: u64) {
        self.last_success_unix = Some(now_unix);
        self.last_error = None;
        self.next_retry_unix = None;
        self.consecutive_failures = 0;
        self.bytes_sent += bytes;
        let s = self.signal(signal);
        s.state = "ok";
        s.consecutive_failures = 0;
        s.last_success_unix = Some(now_unix);
        s.last_error = None;
        s.records_sent += records;
        s.bytes_sent += bytes;
    }

    /// A failed request; `state` is the signal's new state (`backing_off` or `auth_failed`).
    pub fn failure(&mut self, signal: &'static str, state: &'static str, message: String) {
        self.consecutive_failures += 1;
        self.last_error = Some(message.clone());
        let s = self.signal(signal);
        s.state = state;
        s.consecutive_failures += 1;
        s.last_error = Some(message);
    }

    /// Counts `n` records of `signal` dropped rather than sent: the same counts the `dropped` map
    /// holds by reason, seen per signal.
    pub fn dropped_records(&mut self, signal: &'static str, n: u64) {
        if n > 0 {
            self.signal(signal).records_dropped += n;
        }
    }

    /// Counts `n` holes in what could be read: one per `export_gap` record.
    pub fn counted_gaps(&mut self, signal: &'static str, n: u64) {
        if n > 0 {
            self.signal(signal).gaps += n;
        }
    }
}

/// The bytes of one source not yet behind its committed cursor: the rest of the file the cursor is
/// in, plus the live file when the cursor is still in the rolled one.
pub(crate) fn backlog_bytes(file: &SourceFile, cursor: &Cursor) -> u64 {
    let live = std::fs::metadata(&file.live).ok();
    let live_len = live.as_ref().map_or(0, std::fs::Metadata::len);
    let Some(id) = cursor.file_id else {
        return live_len;
    };
    if live.as_ref().is_some_and(|m| FileId::of(m) == id) {
        return live_len.saturating_sub(cursor.offset);
    }
    let rolled = file.rolled.as_ref().and_then(|p| std::fs::metadata(p).ok());
    match rolled {
        Some(m) if FileId::of(&m) == id => m.len().saturating_sub(cursor.offset) + live_len,
        // The file the cursor named is gone: the reader starts the live file over.
        _ => live_len,
    }
}

/// The backlog split by the signal each discovered file feeds (`logs`, `traces`, `metrics`): the
/// same bytes [`backlog_bytes`] sums, bucketed, for the status page and the backlog gauge. Here
/// rather than in the exporter so the per-file rule and its split stay in one place.
pub(crate) fn backlog_by_signal(files: &[SourceFile], state: &State, destination: &str) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for file in files {
        let signal = match file.signal {
            Signal::Logs => "logs",
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
        };
        let cursor = state.cursor(&crate::tailer::key(destination, file));
        *out.entry(signal.to_string()).or_default() += backlog_bytes(file, &cursor);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_never_shrunk_is_budgeted_at_the_default() {
        let mut caps = Caps::default();
        assert_eq!(caps.get("logs"), crate::batch::DEFAULT_REQUEST_BYTES);
        assert_eq!(caps.get("metrics"), crate::batch::DEFAULT_REQUEST_BYTES);
        assert_eq!(caps.shrunk_to("logs"), None, "never halved");
        caps.halve("logs");
        assert_eq!(
            caps.get("traces"),
            crate::batch::DEFAULT_REQUEST_BYTES,
            "one signal's halving leaves the others alone"
        );
    }

    #[test]
    fn halving_halves_and_floors_at_min_cap_bytes() {
        let mut caps = Caps::default();
        caps.halve("logs");
        assert_eq!(caps.shrunk_to("logs"), Some(crate::batch::DEFAULT_REQUEST_BYTES / 2));
        caps.halve("logs");
        assert_eq!(caps.get("logs"), crate::batch::DEFAULT_REQUEST_BYTES / 4);
        // Well past the floor: six halvings reach it from the 1 MiB default, and it holds.
        for _ in 0..16 {
            caps.halve("logs");
        }
        assert_eq!(caps.get("logs"), MIN_CAP_BYTES);
        assert_eq!(caps.shrunk_to("logs"), Some(MIN_CAP_BYTES));
        caps.halve("logs");
        assert_eq!(caps.get("logs"), MIN_CAP_BYTES, "halving at the floor stays there");
    }
}
