//! A rotation- and truncation-safe jsonl reader with a durable cursor: the lowest layer of the
//! observability exporter (#842). It reads whole JSON lines out of one live file and its single
//! rolled-over predecessor, never consuming past the last newline it has seen, and reports every
//! way the stream can break — a rotation it fell behind, a truncated file, a deleted file, a line
//! too long to hold — as a [`Gap`] instead of silently skipping or failing. The caller commits the
//! returned [`Cursor`] to disk after it has handled the batch, so a crash between read and commit
//! replays the batch rather than losing it.
//!
//! Nothing here takes a lock or touches the async runtime: it is plain `std::fs`, meant to run on a
//! `spawn_blocking` thread. Later slices (#843, the multi-source tailer) wire it in.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

/// The largest chunk read from the file at once. A line is assembled from these; the buffer never
/// holds more than one line (`max_line_bytes`), never the whole file.
const CHUNK: usize = 64 * 1024;

/// Identifies a file across renames. On unix it is the device and inode; elsewhere a best-effort
/// `(length, creation time)` that a rename preserves but a copy does not (see docs/observability/
/// tailer.md). Taken from the opened handle's metadata, never a separate `stat` of the path, so a
/// rename between the open and the look cannot misidentify it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileId {
    /// The device number (unix), or the file's length when first seen (elsewhere).
    pub(crate) dev: u64,
    /// The inode (unix), or the creation time in nanoseconds since the epoch (elsewhere).
    pub(crate) ino: u64,
}

impl FileId {
    /// The identity of an opened file, from its own handle.
    #[cfg(unix)]
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }

    /// The identity of an opened file, from its own handle. Best effort on platforms without an
    /// inode: a `(length, creation time)` pair a rename preserves but a copy does not.
    #[cfg(not(unix))]
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
        let created = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        FileId {
            dev: meta.len(),
            ino: created,
        }
    }
}

/// Where a reader got to in one file: which file, how far in, and a fingerprint of the last line it
/// consumed. `file_id == None` means "not bound yet": the next read starts the live file at 0 and
/// reports no gap.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file_id: Option<FileId>,
    #[serde(default)]
    pub(crate) offset: u64,
    /// FNV-1a 64-bit hash of the last consumed line's bytes: a stable fingerprint a consumer can
    /// use to tell where a batch picked up. `None` until a line has been consumed (or after a gap
    /// abandoned the file it named).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_key: Option<u64>,
}

/// How much one batch may hold. `max_bytes` bounds the batch (a single line bigger than it is still
/// read, up to `max_line_bytes`); `max_lines` bounds the line count; `max_line_bytes` is the
/// longest line that counts as a line — anything longer is skipped as [`GapReason::OversizedLine`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) max_bytes: u64,
    pub(crate) max_lines: usize,
    pub(crate) max_line_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_bytes: 4 * 1024 * 1024,
            max_lines: 10_000,
            max_line_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Why a batch has a hole in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GapReason {
    /// The cursor's file is gone and neither the live file nor the rolled file is it: a rotation
    /// happened while the reader was behind, so whatever the rotated-away file held is lost.
    RotatedPast,
    /// The file the cursor was in is shorter than the cursor: it was truncated in place and reading
    /// restarts from the top.
    Truncated,
    /// The live file is missing and the rolled file does not match the cursor: the file was
    /// deleted.
    Deleted,
    /// One line was longer than `max_line_bytes` and was skipped, not parsed.
    OversizedLine,
    /// The persisted state could not be used (unknown version, unreadable or corrupt); the reader
    /// starts from nothing.
    StateReset,
}

impl GapReason {
    /// The wire name, as it serialises.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            GapReason::RotatedPast => "rotated_past",
            GapReason::Truncated => "truncated",
            GapReason::Deleted => "deleted",
            GapReason::OversizedLine => "oversized_line",
            GapReason::StateReset => "state_reset",
        }
    }
}

/// A hole in the stream, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Gap {
    pub(crate) reason: GapReason,
}

impl Gap {
    pub(crate) fn new(reason: GapReason) -> Self {
        Gap { reason }
    }
}

/// One read: the lines, where to resume, and everything that went wrong.
#[derive(Clone, Debug)]
pub(crate) struct Batch {
    pub(crate) lines: Vec<Value>,
    /// The SHA-256 of each line's raw bytes (newline excluded), parallel to `lines`: the record id's
    /// key for a source with no `seq` (docs/design/observability.md, Identity).
    pub(crate) digests: Vec<[u8; 32]>,
    /// Each line's size on disk, newline included, parallel to `lines`: what a skipped line costs.
    pub(crate) sizes: Vec<u64>,
    pub(crate) cursor: Cursor,
    pub(crate) gaps: Vec<Gap>,
    /// Whole lines that would not parse as JSON (or UTF-8), skipped rather than fatal.
    pub(crate) malformed: u64,
    /// Bytes consumed from the file(s) this batch, i.e. how far `cursor` advanced.
    pub(crate) bytes_read: u64,
}

impl Batch {
    fn empty() -> Self {
        Batch {
            lines: Vec::new(),
            digests: Vec::new(),
            sizes: Vec::new(),
            cursor: Cursor::default(),
            gaps: Vec::new(),
            malformed: 0,
            bytes_read: 0,
        }
    }
}

/// Reads the next batch of whole JSON lines from `path` (the live file) and, while the cursor is
/// still in it, `rolled` (the previous generation). Opens both read-only; the caller owns any
/// locking and commits the returned cursor. `NotFound` on the live file is a deleted stream, not an
/// error; any other open error propagates.
pub(crate) fn read_batch(path: &Path, rolled: Option<&Path>, cursor: &Cursor, limits: &Limits) -> io::Result<Batch> {
    let mut scan = Scan {
        limits,
        out: Batch::empty(),
        lines_used: 0,
        last_key: cursor.last_key,
    };

    let Some(mut live) = open_opt(path)? else {
        // The live file is not there.
        return Ok(match cursor.file_id {
            // Not bound yet, nothing to lose: an empty batch, no gap. Repeated calls stay silent.
            None => scan.out,
            // Bound: the cursor's file may survive as the rolled one.
            Some(fid) => match rolled_file_id(rolled, fid)? {
                Some(mut roll) => {
                    let end = scan.scan_file(&mut roll, cursor.offset, true)?;
                    scan.out.cursor = Cursor {
                        file_id: Some(fid),
                        offset: end,
                        last_key: scan.last_key,
                    };
                    scan.out
                }
                None => {
                    scan.out.gaps.push(Gap::new(GapReason::Deleted));
                    scan.out.cursor = Cursor::default();
                    scan.out
                }
            },
        });
    };

    let live_id = FileId::of(&live.metadata()?);
    match cursor.file_id {
        // Not bound: start the live file at 0 with no gap.
        None => {
            let end = scan.scan_file(&mut live, 0, false)?;
            scan.bind(live_id, end);
        }
        Some(fid) if fid == live_id => {
            let len = live.metadata()?.len();
            let start = if len < cursor.offset {
                // Same file, shorter than the cursor: truncated in place, restart at the top.
                scan.out.gaps.push(Gap::new(GapReason::Truncated));
                scan.last_key = None;
                0
            } else {
                cursor.offset
            };
            let end = scan.scan_file(&mut live, start, false)?;
            scan.bind(live_id, end);
        }
        // A different file: the cursor may be in the rolled one.
        Some(fid) => match rolled_file_id(rolled, fid)? {
            Some(mut roll) => {
                // Finish the rolled file (its trailing fragment is final). Only once it is fully
                // drained may the cursor move on to the live file — a batch that stopped inside the
                // rolled file (on its line or byte budget) stays there, or its tail would be lost.
                let roll_len = roll.metadata()?.len();
                let start = if cursor.offset > roll_len {
                    // The rolled file is shorter than the cursor named: it was truncated in place
                    // (before it became the rolled file). Restart it from the top, with a gap.
                    scan.out.gaps.push(Gap::new(GapReason::Truncated));
                    scan.last_key = None;
                    0
                } else {
                    cursor.offset
                };
                let end = scan.scan_file(&mut roll, start, true)?;
                if end < roll_len {
                    scan.out.cursor = Cursor {
                        file_id: Some(fid),
                        offset: end,
                        last_key: scan.last_key,
                    };
                } else {
                    // The rolled file is drained. A fresh file's first line has no predecessor to
                    // fingerprint, so drop it before binding to the live file; reading the live file
                    // sets a new one.
                    scan.last_key = None;
                    let live_end = if scan.room() {
                        scan.scan_file(&mut live, 0, false)?
                    } else {
                        0
                    };
                    scan.bind(live_id, live_end);
                }
            }
            None => {
                // Neither file is the cursor's: one rotation too many.
                scan.out.gaps.push(Gap::new(GapReason::RotatedPast));
                scan.last_key = None;
                let end = scan.scan_file(&mut live, 0, false)?;
                scan.bind(live_id, end);
            }
        },
    }
    Ok(scan.out)
}

/// A cursor bound to the live file just past its last complete line, so nothing already in it is
/// read: where a new destination starts when it starts "now". `None` when the file does not exist
/// (it then starts at 0 once it appears, so nothing written later is missed).
pub(crate) fn at_end(path: &Path) -> io::Result<Option<Cursor>> {
    let Some(mut file) = open_opt(path)? else {
        return Ok(None);
    };
    let meta = file.metadata()?;
    let len = meta.len();
    // Walk back from the end in chunks to the last newline: a trailing fragment is a line still
    // being written, and starts the stream.
    let mut end = len;
    let mut chunk = vec![0u8; CHUNK];
    let mut offset = 0;
    while end > 0 {
        let start = end.saturating_sub(CHUNK as u64);
        let n = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..n])?;
        if let Some(i) = chunk[..n].iter().rposition(|&b| b == b'\n') {
            offset = start + i as u64 + 1;
            break;
        }
        end = start;
    }
    Ok(Some(Cursor {
        file_id: Some(FileId::of(&meta)),
        offset,
        last_key: None,
    }))
}

/// The opened rolled file, when it is the one the cursor named.
fn rolled_file_id(rolled: Option<&Path>, want: FileId) -> io::Result<Option<File>> {
    let Some(path) = rolled else {
        return Ok(None);
    };
    match open_opt(path)? {
        Some(roll) if FileId::of(&roll.metadata()?) == want => Ok(Some(roll)),
        _ => Ok(None),
    }
}

/// Opens a file read-only, `NotFound` as `None` and anything else an error.
fn open_opt(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The mutable state one [`read_batch`] threads through its file scans: the batch it is filling,
/// how much of the budget it has used, and the fingerprint of the last line consumed.
struct Scan<'a> {
    limits: &'a Limits,
    out: Batch,
    lines_used: usize,
    last_key: Option<u64>,
}

impl Scan<'_> {
    /// Binds the result cursor to a file at `end`, carrying the last line's fingerprint.
    fn bind(&mut self, file_id: FileId, end: u64) {
        self.out.cursor = Cursor {
            file_id: Some(file_id),
            offset: end,
            last_key: self.last_key,
        };
    }

    /// Whether the batch still has room for another line.
    fn room(&self) -> bool {
        self.out.bytes_read < self.limits.max_bytes && self.lines_used < self.limits.max_lines
    }

    /// Reads whole lines from `start`, appending to the batch until the budget fills or the file
    /// ends. Returns the offset just past the last byte consumed. Never consumes past the last `\n`
    /// unless `final_fragment`, which is set only for the rolled file, whose trailing unterminated
    /// bytes are its final line.
    fn scan_file(&mut self, file: &mut File, start: u64, final_fragment: bool) -> io::Result<u64> {
        file.seek(SeekFrom::Start(start))?;
        let mut buf: Vec<u8> = Vec::new();
        let mut head = 0usize; // bytes of `buf` already consumed
        let mut local: u64 = 0; // bytes consumed in this scan, the offset's advance
        let mut chunk = vec![0u8; CHUNK];
        loop {
            if let Some(rel) = buf[head..].iter().position(|&b| b == b'\n') {
                let i = head + rel;
                let total = i + 1 - head;
                // Never split a line, and stop before a line that would overflow the budget — unless
                // it is the batch's first line, which must be read or the reader would stall.
                if self.out.bytes_read > 0 && self.out.bytes_read + total as u64 > self.limits.max_bytes {
                    break;
                }
                if self.lines_used >= self.limits.max_lines {
                    break;
                }
                self.last_key = Some(fnv1a(&buf[head..i]));
                self.consume(&buf[head..i]);
                self.out.bytes_read += total as u64;
                self.lines_used += 1;
                local += total as u64;
                head = i + 1;
                continue;
            }
            // No newline in hand. A line longer than `max_line_bytes` is skipped, not held — but only
            // while the batch still has a line slot for it.
            if buf.len() - head > self.limits.max_line_bytes {
                if self.lines_used >= self.limits.max_lines {
                    break;
                }
                buf.drain(..head);
                head = 0;
                if !self.skip_oversized(file, &mut buf, &mut local, final_fragment)? {
                    break; // the line has no newline yet: leave it for the next call
                }
                continue;
            }
            if self.out.bytes_read > 0 && self.out.bytes_read >= self.limits.max_bytes {
                break;
            }
            if self.lines_used >= self.limits.max_lines {
                break;
            }
            // Compact the consumed prefix, then read one more chunk bounded so the buffer never
            // holds more than a single (possibly `max_line_bytes`-long) line.
            if head > 0 {
                buf.drain(..head);
                head = 0;
            }
            let want = CHUNK.min(self.limits.max_line_bytes.saturating_sub(buf.len())).max(1);
            let n = file.read(&mut chunk[..want])?;
            if n == 0 {
                // EOF. The rolled file's trailing fragment is final and parsed as a last line (a
                // fragment longer than `max_line_bytes` was already handled above, by
                // `skip_oversized`); the live file's is left for the next call, once its newline
                // arrives.
                if final_fragment && !buf.is_empty() && self.lines_used < self.limits.max_lines {
                    self.last_key = Some(fnv1a(&buf));
                    self.consume(&buf);
                    let n = buf.len() as u64;
                    self.out.bytes_read += n;
                    self.lines_used += 1;
                    local += n;
                }
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Ok(start + local)
    }

    /// The line in hand has run past `max_line_bytes`: skip to its newline in bounded chunks without
    /// ever holding the whole line. Any bytes after the newline are kept in `buf` for the next
    /// iteration. Returns whether the file advanced past the oversized line.
    ///
    /// On EOF the line has no newline yet, so nothing is consumed and the next call tries again —
    /// unless this is the final file (the rolled one), whose trailing fragment is its last line: that
    /// fragment is skipped here so the file drains and the cursor can move on to the live file.
    fn skip_oversized(&mut self, file: &mut File, buf: &mut Vec<u8>, local: &mut u64, final_fragment: bool) -> io::Result<bool> {
        let mut discarded = buf.len();
        buf.clear();
        let mut chunk = vec![0u8; CHUNK];
        loop {
            let n = file.read(&mut chunk)?;
            if n == 0 {
                if !final_fragment {
                    return Ok(false); // the line has no newline yet: leave it for the next call
                }
                break; // the rolled file ends here: the fragment was its final line
            }
            if let Some(i) = chunk[..n].iter().position(|&b| b == b'\n') {
                discarded += i + 1;
                buf.extend_from_slice(&chunk[i + 1..n]);
                break;
            }
            discarded += n;
        }
        self.out.gaps.push(Gap::new(GapReason::OversizedLine));
        self.out.bytes_read += discarded as u64;
        self.lines_used += 1;
        *local += discarded as u64;
        Ok(true)
    }

    /// Files one line: a blank line is skipped silently, a valid JSON line is yielded, and anything
    /// else is counted malformed and skipped.
    fn consume(&mut self, line: &[u8]) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        match std::str::from_utf8(line)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            Some(value) => {
                self.out.lines.push(value);
                let digest = ring::digest::digest(&ring::digest::SHA256, line);
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(digest.as_ref());
                self.out.digests.push(bytes);
                self.out.sizes.push(line.len() as u64 + 1);
            }
            None => self.out.malformed += 1,
        }
    }
}

/// FNV-1a, 64-bit: the fingerprint persisted for the last consumed line. Chosen over
/// `DefaultHasher` because it is stable across Rust versions — the value is written to disk — and
/// needs no dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests;
