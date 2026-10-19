//! The `file` provider (#850): the same OTLP/JSON export requests the OTLP provider POSTs, written
//! to capped local files instead — one request per line — so a collector beside the mothership
//! (the OTel Collector, Alloy, Vector) can pick them up with its `otlpjsonfile` receiver, and an
//! air-gapped install still keeps its telemetry. Nothing leaves the machine
//! (docs/observability/file-output.md).
//!
//! - **One line, one request:** each line is a complete OTLP/JSON export request, uncompressed
//!   (never protobuf, never gzip), exactly the bytes the OTLP provider would have sent.
//! - **Files:** `<dir>/otlp-<signal>-<yyyymmdd>.jsonl` per signal per UTC day. A file that would
//!   pass the per-file cap rolls to `otlp-<signal>-<yyyymmdd>.<n>.jsonl`; every name the sink
//!   writes matches `otlp-*.jsonl`.
//! - **Caps:** the per-file cap is one eighth of `max_mb`, at least 1 MiB — enough files per day
//!   for the trim to be fine-grained, few enough for a glob to stay short. The total of the sink's
//!   own files is held at or under `max_mb` by deleting the oldest first, and nothing else in the
//!   directory is ever touched.
//! - **fsync before ack:** the line is written and fsynced before [`Outcome::Sent`] returns, so the
//!   exporter's cursor commit only ever follows a durable line. A write or fsync error is a retry:
//!   nothing is committed and the batch replays.
//! - **The disk guard:** while the mothership's `<data>/observability/disk-low` marker stands, the
//!   sink writes nothing at all and answers a retry whose `reason` is `disk`, so the health shows
//!   the signal `backing_off` for exactly as long as the disk is low.

use crate::contract::Settings;
use crate::encode::{Encoding, Request};
use crate::transport::Outcome;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 1 MiB: the unit of `max_mb` and the floor of the per-file cap.
const MIB: u64 = 1024 * 1024;
/// The disk-low marker's name, under `<data>/observability/`: written by the mothership while its
/// free-space verdict is low or a storage alert stands, removed when neither does. The add-on
/// cannot see that state, so it crosses as a file.
pub(crate) const DISK_LOW: &str = "disk-low";
/// The message a paused sink answers with; the `reason` field carries the machine-readable part.
const PAUSED: &str = "the mothership reports low disk; the file provider is paused";

/// Where the `file` provider writes: `dir` as saved by the module — relative to the data dir, or
/// empty for the default `<data>/observability/otlp`. An absolute path or a `..` component is
/// refused: the sink writes under the data dir or nowhere.
pub fn sink_dir(data_dir: &Path, dir: &str) -> Result<PathBuf, String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Ok(data_dir.join("observability").join("otlp"));
    }
    let relative = Path::new(dir);
    if relative.is_absolute() || relative.components().any(|c| c.as_os_str() == "..") {
        return Err(format!(
            "the file provider's dir {dir:?} must be relative to the data dir, with no `..`; leave it empty for the default"
        ));
    }
    Ok(data_dir.join(relative))
}

/// The sink's directory as a cursor key: the exporter keys its cursors on the effective
/// destination, so a `dir` change starts fresh cursors exactly as an endpoint change does.
pub fn destination(data_dir: &Path, dir: &str) -> String {
    sink_dir(data_dir, dir).map(|d| d.display().to_string()).unwrap_or_default()
}

/// One file destination (#850). Stateless on purpose: the current file for a signal is found on
/// every write by listing the directory, so a restart (or a file trimmed away behind the sink's
/// back) is never a wrong append.
pub(crate) struct FileSink {
    dir: PathBuf,
    /// `<data>/observability/disk-low`, the mothership's disk guard's marker.
    marker: PathBuf,
    /// The total cap across the sink's own files: `max_mb` MiB.
    max_bytes: u64,
    /// The per-file rotation cap: `max_bytes / 8`, at least 1 MiB.
    file_cap: u64,
}

impl FileSink {
    pub(crate) fn new(data_dir: &Path, settings: &Settings) -> Result<FileSink, String> {
        let dir = sink_dir(data_dir, &settings.dir)?;
        let max_bytes = settings.max_mb.max(1) * MIB;
        let file_cap = (max_bytes / 8).max(MIB);
        Ok(FileSink {
            marker: data_dir.join("observability").join(DISK_LOW),
            dir,
            max_bytes,
            file_cap,
        })
    }

    /// The same sink with byte-level caps, so a test can rotate without writing mebibytes.
    #[cfg(test)]
    pub(crate) fn with_caps(data_dir: &Path, dir: PathBuf, max_bytes: u64, file_cap: u64) -> FileSink {
        FileSink {
            marker: data_dir.join("observability").join(DISK_LOW),
            dir,
            max_bytes,
            file_cap,
        }
    }

    /// Writes one request as one line and answers as the transport would: `Sent` only after the
    /// line is on disk and fsynced, a retry (reason `disk`) while the marker stands, a retry on any
    /// write or fsync error.
    pub(crate) fn write(&self, request: &Request) -> Outcome {
        if self.marker.try_exists().unwrap_or(false) {
            return Outcome::Retry {
                message: PAUSED.into(),
                after: None,
                reason: Some("disk"),
            };
        }
        let signal = signal_of(request);
        let mut line = request.encode(Encoding::Json);
        line.push(b'\n');
        match self.append(signal, &line) {
            Ok(warning) => Outcome::Sent {
                rejected: 0,
                message: warning,
                bytes: line.len() as u64,
            },
            Err(message) => Outcome::Retry {
                message,
                after: None,
                reason: None,
            },
        }
    }

    /// Appends `line` to the signal's current file, rolling first when it would pass the cap, and
    /// fsyncs before returning. `Ok(Some(message))` is a durable write with a warning to surface.
    fn append(&self, signal: &str, line: &[u8]) -> Result<Option<String>, String> {
        self.ensure_dir()?;
        let date = utc_ymd(SystemTime::now());
        let path = self.target(signal, &date, line.len() as u64);
        let existed = std::fs::metadata(&path).is_ok();
        faults::check(&path, faults::Op::Write).map_err(|e| why(&path, e))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Mode applies at creation only and the umask masks it, so a created file is pinned to
            // 0600 again below.
            options.mode(0o600);
        }
        let say = |e: std::io::Error| why(&path, e);
        let mut file = options.open(&path).map_err(say)?;
        if !existed {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
        }
        file.write_all(line).map_err(say)?;
        faults::check(&path, faults::Op::Fsync).map_err(say)?;
        file.sync_all().map_err(say)?;
        Ok(self.trim(&path))
    }

    /// The file `line` belongs in: today's newest for the signal, or the next in its rotation when
    /// appending would pass the per-file cap. A line longer than the cap still gets a file of its
    /// own — a request is never split — and the write after it rolls again.
    fn target(&self, signal: &str, date: &str, line_len: u64) -> PathBuf {
        match self.newest(signal, date) {
            Some((_, path, len)) if len == 0 || len + line_len <= self.file_cap => path,
            Some((seq, _, _)) => self.dir.join(format!("otlp-{signal}-{date}.{}.jsonl", seq + 1)),
            None => self.dir.join(format!("otlp-{signal}-{date}.jsonl")),
        }
    }

    /// Today's highest-numbered file for `signal`, with its size: the one a new line appends to.
    fn newest(&self, signal: &str, date: &str) -> Option<(u64, PathBuf, u64)> {
        let mut best: Option<(u64, PathBuf, u64)> = None;
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((file_signal, file_date, seq)) = parsed(&name) else {
                continue;
            };
            if file_signal != signal || file_date != date {
                continue;
            }
            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if best.as_ref().is_none_or(|(b, _, _)| seq >= *b) {
                best = Some((seq, entry.path(), len));
            }
        }
        best
    }

    /// Creates the sink's directory (0700) when it does not exist yet.
    fn ensure_dir(&self) -> Result<(), String> {
        if self.dir.try_exists().unwrap_or(false) {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| why(&self.dir, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// Holds the total of the sink's own files (`otlp-*.jsonl`, every signal) at or under
    /// [`FileSink::max_bytes`] by deleting the oldest first. The file just written is never
    /// deleted, and nothing that does not match the sink's naming is ever touched.
    fn trim(&self, just_written: &Path) -> Option<String> {
        /// One sink file: where it is, how big, when changed, and its parsed name's sort order.
        type SinkFile = (PathBuf, u64, SystemTime, (String, String, u64));
        let mut files: Vec<SinkFile> = Vec::new();
        for entry in std::fs::read_dir(&self.dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Only a name the sink itself could have written is the sink's to delete.
            let Some(order) = parsed(&name) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            files.push((entry.path(), meta.len(), meta.modified().unwrap_or(UNIX_EPOCH), order));
        }
        let mut total: u64 = files.iter().map(|(_, len, _, _)| len).sum();
        if total <= self.max_bytes {
            return None;
        }
        // Oldest first. The modified time leads; the parsed name breaks its ties, where the
        // rotation number orders the way the names read (.2 after .1, and both after the plain
        // date — a plain string sort would put .10 before .2, and .1 before the plain date).
        files.sort_by_key(|&(ref path, _, at, ref order)| (at, order.clone(), path.to_string_lossy().into_owned()));
        for (path, len, _, _) in files {
            if total <= self.max_bytes {
                return None;
            }
            if path == just_written {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => total -= len,
                Err(e) => return Some(format!("{}: could not trim it to stay under the cap: {e}", path.display())),
            }
        }
        (total > self.max_bytes).then(|| {
            format!(
                "the sink's files total {total} bytes, over the {} byte cap, and nothing older is left to trim",
                self.max_bytes
            )
        })
    }
}

/// The signal a request belongs to, as its file names spell it.
fn signal_of(request: &Request) -> &'static str {
    match request {
        Request::Logs(_) => "logs",
        Request::Traces(_) => "traces",
        Request::Metrics(_) => "metrics",
    }
}

/// A sink file's name parsed as `(signal, yyyymmdd, rotation)`: `otlp-<signal>-<yyyymmdd>.jsonl`
/// is rotation 0 and `otlp-<signal>-<yyyymmdd>.<n>.jsonl` is `n`. Anything else — a foreign file
/// that merely starts `otlp-` and ends `.jsonl` — is `None`, so it is neither appended to nor
/// trimmed.
fn parsed(name: &str) -> Option<(String, String, u64)> {
    let rest = name.strip_prefix("otlp-")?.strip_suffix(".jsonl")?;
    let at = rest.rfind('-')?;
    let (signal, dated) = rest.split_at(at);
    let (date, rotation) = match dated[1..].split_once('.') {
        Some((date, n)) => (date, n.parse().ok()?),
        None => (&dated[1..], 0),
    };
    if signal.is_empty() || date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((signal.to_string(), date.to_string(), rotation))
}

/// Today's UTC date as `yyyymmdd`, the rotation unit in the file names.
fn utc_ymd(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .map(|d| d.format("%Y%m%d").to_string())
        .unwrap_or_else(|| "19700101".into())
}

fn why(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

/// A test-only seam for injecting filesystem faults into the sink (the shape of the mothership's
/// `util::faults`, which this crate cannot link): a fault fails the write or the fsync of any path
/// containing `path_contains`, until its guard is dropped. Production builds compile `check` away
/// to an inlined no-op.
pub(crate) mod faults {
    /// The step a fault applies to.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Op {
        Write,
        Fsync,
    }

    #[cfg(not(test))]
    #[inline]
    pub fn check(path: &std::path::Path, op: Op) -> std::io::Result<()> {
        let _ = (path, op);
        Ok(())
    }

    #[cfg(test)]
    pub fn check(path: &std::path::Path, op: Op) -> std::io::Result<()> {
        let text = path.display().to_string();
        INJECTED.with_borrow(|faults| {
            faults
                .iter()
                .find(|(contains, wants, _)| *wants == op && text.contains(contains.as_str()))
                .map(|(.., make)| make())
                .map_or(Ok(()), Err)
        })
    }

    /// Injects a fault for every `check` on a path containing `path_contains` with step `op`, until
    /// the returned guard is dropped.
    #[cfg(test)]
    pub fn inject(path_contains: &str, op: Op, make: fn() -> std::io::Error) -> FaultGuard {
        INJECTED.with_borrow_mut(|faults| faults.push((path_contains.to_string(), op, make)));
        FaultGuard {
            contains: path_contains.to_string(),
            op,
        }
    }

    /// Clears the fault injected by `inject` when dropped.
    #[cfg(test)]
    pub struct FaultGuard {
        contains: String,
        op: Op,
    }

    #[cfg(test)]
    impl Drop for FaultGuard {
        fn drop(&mut self) {
            INJECTED.with_borrow_mut(|faults| {
                if let Some(at) = faults
                    .iter()
                    .position(|(contains, op, _)| *contains == self.contains && *op == self.op)
                {
                    faults.remove(at);
                }
            });
        }
    }

    // A thread-local, as in the mothership: `#[tokio::test]` runs on one thread, so parallel tests
    // cannot see each other's faults.
    #[cfg(test)]
    thread_local! {
        static INJECTED: std::cell::RefCell<Vec<Fault>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    #[cfg(test)]
    type Fault = (String, Op, fn() -> std::io::Error);
}

#[cfg(test)]
mod tests;
