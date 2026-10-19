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
    /// Why the signal is held, when a machine can act on it: `disk`, while the mothership's disk
    /// guard has the file provider paused (#850). Absent otherwise, so a status.json from before
    /// the field existed reads exactly as it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub records_sent: u64,
    pub bytes_sent: u64,
    /// This signal's ledger bytes not yet behind a committed cursor (an estimate).
    pub backlog_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PartialSuccess {
    pub at_unix: u64,
    pub signal: &'static str,
    pub rejected: u64,
    pub message: Option<String>,
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
        s.reason = None;
        s.records_sent += records;
        s.bytes_sent += bytes;
    }

    /// A failed request; `state` is the signal's new state (`backing_off` or `auth_failed`), and
    /// `reason` the machine-readable hold, when there is one (`disk`, #850).
    pub fn failure(&mut self, signal: &'static str, state: &'static str, message: String, reason: Option<&'static str>) {
        self.consecutive_failures += 1;
        self.last_error = Some(message.clone());
        let s = self.signal(signal);
        s.state = state;
        s.consecutive_failures += 1;
        s.last_error = Some(message);
        s.reason = reason;
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
