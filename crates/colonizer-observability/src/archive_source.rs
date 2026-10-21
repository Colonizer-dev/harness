//! The local archive as a source (#859): what the mothership's `archive.rs` tars and zstds into
//! `<data>/archive/<org>/<repo>/<yyyy>/<mm>/` when a colony ends, read back here without extracting
//! anything to disk. Only colonies that are gone live — no `sessions/<id>/` directory, the disk
//! copy's own sign — come out of the archive: the disk reader always wins, so the two never overlap
//! the way the backfill's bounds keep the live exporter and its replay apart.
//!
//! - [`discover`] walks the `.json` sidecars (each written only once its bundle is fully in place,
//!   so a half-written revision is invisible), keeps the newest revision per colony, and answers the
//!   colony's identity: the bundle's `mothership` host id keys its record and trace ids, and a
//!   synthesized [`ColonyPolicy`] carries its org, repo, agent, status and `created_at`. The
//!   synthesis is fail-closed: `sensitivity` is unknown and `content` false, so an archived colony
//!   exports structure only (P3).
//! - [`read_bundle`] streams one bundle's `events/harness/gateway/findings.jsonl` entries line by
//!   line and yields the same per-line triples the disk reader does — parsed JSON, the SHA-256 of
//!   the raw line bytes, `len + 1` — so record ids are byte-identical to a disk replay of the same
//!   session. Nothing else in a bundle is ever read, and nothing that smells like a credential is
//!   so much as unpacked.
//! - [`stream`] wraps the bundles in a blocking producer: parsed lines leave in modest chunks
//!   through a bounded channel, so the reader blocks while the consumer's budget is spent and
//!   memory never holds more than a few chunks whatever the bundle weighs.

use crate::backfill::MAX_LINE_BYTES;
use crate::contract::ColonyPolicy;
use crate::policy::Source;
use crate::sources::{self, COLONY_SOURCES};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// A chunk of parsed lines is flushed at this many lines …
const CHUNK_LINES: usize = 128;
/// … or this many raw bytes, whichever comes first, so a chunk stays modest even when the lines are
/// large. Times the channel's capacity, this is the reader's whole footprint.
const CHUNK_BYTES: u64 = 128 * 1024;
/// The chunks that may wait for the consumer, at most: the reader's backpressure and its bound.
const CHANNEL_CHUNKS: usize = 4;
/// The read size off a bundle entry; the disk reader's own `CHUNK`.
const READ: usize = 64 * 1024;

/// The sidecar record next to every bundle, as far as this crate reads it
/// (crates/colonizer/src/archive.rs `IndexRecord`): the identity fields only, unknown fields
/// ignored, every field defaulted so a sidecar from a newer mothership still parses.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct IndexRecord {
    pub(crate) session: String,
    pub(crate) repo: String,
    pub(crate) org: String,
    pub(crate) status: String,
    pub(crate) agent: String,
    /// RFC 3339, as the colony policy's own `created_at`.
    pub(crate) created_at: String,
    /// The stable host id of the mothership that wrote the bundle: the record and trace ids of the
    /// colony's lines are keyed by it, exactly as a live export on that host would key them.
    pub(crate) mothership: String,
    pub(crate) revision: u32,
    /// The bundle's path under the archive root, `/`-separated.
    pub(crate) bundle: String,
}

/// One colony the archive can supply: its id, its host, its synthesized policy and its bundle.
#[derive(Clone, Debug)]
pub(crate) struct ArchivedColony {
    pub(crate) colony: String,
    pub(crate) host: String,
    pub(crate) policy: ColonyPolicy,
    pub(crate) bundle: PathBuf,
}

/// The colonies the archive holds that the disk does not: every sidecar under
/// `<data>/archive/**.json`, grouped by session with only the newest revision kept, less every id
/// in `skip` (the colonies with a live `sessions/<id>/` directory — disk wins) and anything
/// `--repo` did not name. An id that cannot be a colony, or a `bundle` path that is not a plain
/// relative path, is skipped: the sidecar is data, and data does not pick what gets opened.
pub(crate) fn discover(data_dir: &Path, repo: Option<&str>, skip: &BTreeSet<String>) -> BTreeMap<String, ArchivedColony> {
    let mut newest: BTreeMap<String, IndexRecord> = BTreeMap::new();
    for path in sidecars(&data_dir.join("archive")) {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(record) = serde_json::from_slice::<IndexRecord>(&bytes) else {
            continue;
        };
        if !sources::is_colony_id(&record.session) || within(&record.bundle).is_none() {
            continue;
        }
        // A newer revision replaces an older; an older or equal one (two sidecars racing) is dropped.
        if newest.get(&record.session).is_none_or(|kept| kept.revision < record.revision) {
            newest.insert(record.session.clone(), record);
        }
    }
    let mut out = BTreeMap::new();
    for (session, record) in newest {
        if skip.contains(&session) || repo.is_some_and(|r| !record.repo.eq_ignore_ascii_case(r)) {
            continue;
        }
        let policy = ColonyPolicy {
            org: record.org.clone(),
            repo: record.repo.clone(),
            // Fail closed (P3): the archive knows nothing about sensitivity or an org's opt-in, so
            // an archived colony exports structure only, whatever the contract says.
            sensitivity: None,
            content: false,
            status: Some(record.status.clone()),
            agent: record.agent.clone(),
            created_at: Some(record.created_at.clone()),
            ..ColonyPolicy::default()
        };
        out.insert(
            session.clone(),
            ArchivedColony {
                colony: session,
                host: record.mothership.clone(),
                policy,
                bundle: data_dir.join("archive").join(&record.bundle),
            },
        );
    }
    out
}

/// Every `.json` sidecar under `root`, depth-first: the walk of the mothership's own
/// `collect_index` — a subtree that will not read is skipped, symlinks are never followed.
fn sidecars(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = dir.join(entry.file_name());
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(t) if t.is_file() && path.extension().is_some_and(|e| e == "json") => out.push(path),
                _ => {}
            }
        }
    }
    out
}

/// The bundle's real path, refusing anything that is not a plain relative path — the same refusal
/// `archive.rs`'s `within` makes of a delete, for the same reason: the sidecar is data.
fn within(rel: &str) -> Option<PathBuf> {
    (!rel.starts_with('/') && rel.split('/').all(|part| !part.is_empty() && part != "." && part != ".."))
        .then(|| PathBuf::from(rel))
}

/// Whether a session-dir-relative, `/`-separated path is a credential that must never leave the
/// session directory: the semantics of `archive.rs`'s `is_credential_file`, repeated here because
/// the add-on cannot link the mothership. The two must stay in step.
pub(crate) fn is_credential_file(rel: &str) -> bool {
    const CREDENTIAL_FILES: [&str; 4] = ["vm/token", "gateway-token", "vm/mesh-authkey", "vm/session.json"];
    if CREDENTIAL_FILES.contains(&rel) {
        return true;
    }
    let lower = rel.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name.contains("token")
        || name.contains("authkey")
        || name.ends_with(".key")
        || name.ends_with(".pem")
        || lower
            .split('/')
            .any(|part| part.starts_with("secrets") || part.starts_with("colony-secrets"))
}

/// One line out of a bundle: the source it came from and the same triple the disk reader feeds the
/// mapper — the parsed line, the SHA-256 of its raw bytes (newline excluded) and its size on disk
/// (newline included), so a record id built here matches one built there.
pub(crate) struct ArchiveLine {
    pub(crate) source: Source,
    pub(crate) line: Value,
    pub(crate) digest: [u8; 32],
    pub(crate) size: u64,
}

/// The session ledgers a bundle is read for, by entry name: exactly the four the disk path tails,
/// nothing else a bundle holds (`out/pr.md`, transcripts, rotated `events-N.jsonl` included).
fn source_of(name: &str) -> Option<Source> {
    COLONY_SOURCES
        .iter()
        .find(|(_, file)| *file == name)
        .map(|(source, _)| *source)
}

/// Streams one bundle's four ledgers to `on_line`, whole JSON lines only. Nothing is extracted to
/// disk, no entry but the four ledgers is ever read, and an entry whose name is a credential is
/// skipped on its header alone. Line semantics are the disk reader's (`cursor.rs`): a blank line is
/// skipped silently, a line that is neither UTF-8 nor JSON counts `malformed`, a line longer than
/// [`MAX_LINE_BYTES`] is skipped (counted malformed, where the disk path reports an oversized gap)
/// and a trailing fragment is a final line, the way the disk path treats a rolled file's.
/// `on_line` returning `false` stops the read — the consumer is gone.
pub(crate) fn read_bundle(
    bundle: &Path,
    on_line: &mut dyn FnMut(ArchiveLine) -> bool,
    malformed: &mut u64,
) -> std::io::Result<()> {
    let zstd = zstd::Decoder::new(std::fs::File::open(bundle)?)?;
    let mut tar = tar::Archive::new(zstd);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        // The name check comes before any read of the entry's data: a credential file is never
        // unpacked, and neither is anything but the four ledgers.
        if is_credential_file(&name) {
            continue;
        }
        let Some(source) = source_of(&name) else {
            continue;
        };
        // One line at a time, never the whole entry: the buffer holds at most one line.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; READ];
        loop {
            if let Some(i) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=i).collect();
                if !consume(&line[..line.len() - 1], source, on_line, malformed) {
                    return Ok(());
                }
                continue;
            }
            if buf.len() > MAX_LINE_BYTES {
                skip_oversized(&mut entry, &mut buf, &mut chunk, malformed);
                continue;
            }
            let n = entry.read(&mut chunk)?;
            if n == 0 {
                // The bundle's file is final: a trailing fragment is its last line.
                if !buf.is_empty() && !consume(&buf, source, on_line, malformed) {
                    return Ok(());
                }
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
    Ok(())
}

/// Files one line (cursor.rs `Scan::consume`): blank skipped, JSON yielded with its digest and
/// size, anything else malformed. Whether to keep reading.
fn consume(raw: &[u8], source: Source, on_line: &mut dyn FnMut(ArchiveLine) -> bool, malformed: &mut u64) -> bool {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    match std::str::from_utf8(raw)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
    {
        Some(line) => {
            let digest = ring::digest::digest(&ring::digest::SHA256, raw);
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(digest.as_ref());
            on_line(ArchiveLine {
                source,
                line,
                digest: bytes,
                size: raw.len() as u64 + 1,
            })
        }
        None => {
            *malformed += 1;
            true
        }
    }
}

/// The line in hand has run past [`MAX_LINE_BYTES`]: skip to its newline in bounded chunks without
/// ever holding the whole line (cursor.rs `Scan::skip_oversized`).
fn skip_oversized<R: Read>(entry: &mut R, buf: &mut Vec<u8>, chunk: &mut [u8], malformed: &mut u64) {
    buf.clear();
    loop {
        let n = entry.read(chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        if let Some(i) = chunk[..n].iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&chunk[i + 1..n]);
            break;
        }
    }
    *malformed += 1;
}

/// A chunk of parsed lines on their way to the engine, or the producer's end.
pub(crate) enum Message {
    Lines {
        colony: String,
        host: String,
        lines: Vec<ArchiveLine>,
    },
    /// Every bundle is done; `malformed` counts the lines (and bundles) that could not be read.
    Done { malformed: u64 },
}

/// Sends one chunk, if there is one to send. Whether the consumer is still there.
fn flush(tx: &std::sync::mpsc::SyncSender<Message>, colony: &str, host: &str, lines: &mut Vec<ArchiveLine>) -> bool {
    !lines.is_empty()
        && tx
            .send(Message::Lines {
                colony: colony.to_string(),
                host: host.to_string(),
                lines: std::mem::take(lines),
            })
            .is_ok()
}

/// Streams every bundle through `tx`, chunk by chunk. This blocks — the consumers run it on its own
/// thread — and a full channel blocks it in turn, which is the point: the reader waits for the
/// consumer's budget, and neither side holds more than [`CHANNEL_CHUNKS`] chunks. A bundle that
/// cannot be opened or decoded is counted in `malformed` and left behind: the run is a replay, and
/// the next one tries the same bundle again.
pub(crate) fn stream(bundles: &[ArchivedColony], tx: std::sync::mpsc::SyncSender<Message>) {
    let mut malformed = 0u64;
    for colony in bundles {
        let mut lines: Vec<ArchiveLine> = Vec::new();
        let mut bytes = 0u64;
        let mut on_line = |line: ArchiveLine| -> bool {
            bytes += line.size;
            lines.push(line);
            if lines.len() < CHUNK_LINES && bytes < CHUNK_BYTES {
                return true;
            }
            let sent = flush(&tx, &colony.colony, &colony.host, &mut lines);
            bytes = 0;
            sent
        };
        let read = read_bundle(&colony.bundle, &mut on_line, &mut malformed);
        // The lines of a half-read bundle were still good lines; a consumer that is gone ends the
        // whole run, a bundle that would not read is counted and left behind.
        if !flush(&tx, &colony.colony, &colony.host, &mut lines) {
            break;
        }
        if read.is_err() {
            malformed += 1;
        }
    }
    let _ = tx.send(Message::Done { malformed });
}

/// The bounded channel a [`stream`] producer feeds: `CHANNEL_CHUNKS` chunks, the reader's
/// backpressure and memory bound in one.
pub(crate) fn channel() -> (std::sync::mpsc::SyncSender<Message>, std::sync::mpsc::Receiver<Message>) {
    std::sync::mpsc::sync_channel(CHANNEL_CHUNKS)
}

#[cfg(test)]
mod tests;
