//! The session storage interface ([`SessionStore`]) and its two reference backends: the default
//! [`LocalDirStore`], byte for byte today's on-disk layout, and [`MemoryObjectStore`], a stand-in
//! for a remote object store. [`migrate`] copies one store's colonies into another and verifies
//! the copy. Issue #325's first slice: same semantics, swappable store. The per-operation
//! contract and the migration procedure are in docs/session-store.md.

use crate::config::Settings;
use crate::util;
use anyhow::{Error, Result, bail};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

/// The one file every backend keeps under this exact name: the session index.
pub(crate) const INDEX: &str = "sessions.json";

/// The per-session prefix every file name is resolved under.
pub(crate) const SESSIONS: &str = "sessions";

/// The future every operation answers with: boxed and `Send`, so the trait stays object-safe and
/// `migrate` can drive any backend — including ones not written yet — as `&dyn SessionStore`.
pub(crate) type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;

pub(crate) trait SessionStore: Send + Sync {
    /// The index's bytes, or `None` before the first write (a first run).
    fn read_index(&self) -> StoreFuture<'_, Option<Vec<u8>>>;
    /// Replaces the index whole; `Ok` means every later read sees exactly these bytes.
    fn write_index<'a>(&'a self, bytes: &'a [u8]) -> StoreFuture<'a, ()>;
    /// Every session id that has a session, sorted.
    fn list_sessions(&self) -> StoreFuture<'_, Vec<String>>;
    /// One per-session file's bytes, or `None` when the session has no such file.
    fn read_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<Vec<u8>>>;
    /// Creates or replaces one file whole — the index's guarantees, per file.
    fn write_file<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()>;
    /// Appends one line (newline added), creating the file if needed; at least once per writer,
    /// and readers replay it with the `seq` rule (drop repeats, per file). Every backend stores the
    /// line through [`ledger_line`] (#761), so no event, log or ledger line reaches a store with a
    /// credential in it, whichever caller wrote it.
    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()>;
    /// The session's files, relative and `/`-separated (`vm/token`), sorted, recursive.
    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>>;
    /// Removes a session and everything in it; removing an unknown id is `Ok`.
    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()>;
    /// Moves an unusable index aside to `sessions.json.corrupt-<unix-ts>` — a forward-moving
    /// stamp, so an aside from an earlier second is never overwritten — and returns that name;
    /// `None` when there is no index to quarantine.
    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>>;

    // ---- Derived operations (issue #610). Each has a default built from the nine above, so a
    // backend is correct by implementing those alone; a backend overrides one only to do it
    // cheaper (a seek instead of a whole read, a rename instead of a copy).

    /// The end of one file: at most `max` bytes, cut forward to a line start so only whole lines
    /// come back. `None` when the file is absent. What the cockpit's diagnosis and the colony map
    /// read from a long event log, without reading all of it.
    fn read_tail<'a>(&'a self, id: &'a str, name: &'a str, max: u64) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { Ok(self.read_file(id, name).await?.map(|bytes| whole_line_tail(&bytes, max))) })
    }
    /// A window of one file ending at byte `end` (the end of the file when `None`): at most `max`
    /// bytes, cut forward to a line start so only whole lines come back, plus the byte offset the
    /// window starts at. How the chat pages an event log backwards without reading all of it
    /// (issue #1210); a backend overrides the default (a whole read, sliced) with a seek.
    fn read_before<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        end: Option<u64>,
        max: u64,
    ) -> StoreFuture<'a, Option<(Vec<u8>, u64)>> {
        Box::pin(async move { Ok(self.read_file(id, name).await?.map(|bytes| window_before(&bytes, end, max))) })
    }
    /// One file's size and, where the backend keeps one, its last-modified time; `None` if absent.
    fn stat<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<FileStat>> {
        Box::pin(async move {
            let bytes = self.read_file(id, name).await?;
            Ok(bytes.map(|b| FileStat {
                len: b.len() as u64,
                modified: None,
            }))
        })
    }
    /// [`SessionStore::write_file`] for a secret (`vm/token`, `vm/mesh-authkey`, `issue.json`): the
    /// local store writes it owner-only (0600) through `util::write_private`; a remote backend has
    /// no mode bits and protects the bytes by access control, so its default is a plain write.
    fn write_private<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        self.write_file(id, name, bytes)
    }
    /// Removes one file; removing an absent file is `Ok`.
    fn remove_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, ()>;
    /// Renames one file within a session (`events.jsonl` to `events-N.jsonl` on a resume). The
    /// default copies then removes, so a crash between the two leaves both, never neither.
    fn rename_file<'a>(&'a self, id: &'a str, from: &'a str, to: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let Some(bytes) = self.read_file(id, from).await? else {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!("no {from} in session {id}")));
            };
            self.write_file(id, to, &bytes).await?;
            self.remove_file(id, from).await
        })
    }
}

/// The run numbers of a session's rotated event logs (`events-N.jsonl`, top level only), sorted
/// ascending: the archives a resume leaves behind, read by listing the session through the store.
pub(crate) async fn event_archives(store: &dyn SessionStore, id: &str) -> io::Result<Vec<u64>> {
    let mut runs: Vec<u64> = store
        .list_files(id)
        .await?
        .iter()
        .filter_map(|name| name.strip_prefix("events-")?.strip_suffix(".jsonl")?.parse().ok())
        .collect();
    runs.sort_unstable();
    Ok(runs)
}

/// What [`SessionStore::stat`] reports about one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStat {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// The window of `bytes` that [`SessionStore::read_before`] answers with: whole lines, at most `max`
/// bytes, ending at `end` (or the end of the data), and the offset it starts at.
pub(crate) fn window_before(bytes: &[u8], end: Option<u64>, max: u64) -> (Vec<u8>, u64) {
    let end = end.map_or(bytes.len(), |e| usize::try_from(e).unwrap_or(usize::MAX).min(bytes.len()));
    let mut start = end.saturating_sub(usize::try_from(max).unwrap_or(usize::MAX));
    if start > 0 && bytes[start - 1] != b'\n' {
        start = match bytes[start..end].iter().position(|b| *b == b'\n') {
            Some(at) => start + at + 1,
            None => end,
        };
    }
    (bytes[start..end].to_vec(), start as u64)
}

/// The last `max` bytes of `bytes`, cut forward past the first newline unless the cut already falls
/// on a line start — the whole-lines tail every backend's `read_tail` answers with.
pub(crate) fn whole_line_tail(bytes: &[u8], max: u64) -> Vec<u8> {
    let start = bytes.len().saturating_sub(usize::try_from(max).unwrap_or(usize::MAX));
    if start == 0 || bytes[start - 1] == b'\n' {
        return bytes[start..].to_vec();
    }
    match bytes[start..].iter().position(|b| *b == b'\n') {
        Some(at) => bytes[start + at + 1..].to_vec(),
        None => Vec::new(),
    }
}

/// One appended line as a backend stores it: redacted field by field (a JSON line stays valid
/// JSON) or as text, by the shared redactor (#761). Callers that also broadcast the line redact it
/// themselves first; for them this is a no-op that keeps a clean line byte for byte. Redacting here
/// as well means a new writer to `events.jsonl`, `harness.jsonl`, `findings.jsonl` or any later
/// ledger cannot forget to.
pub(crate) fn ledger_line(line: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    crate::redact::redact_jsonl(line)
}

/// A Windows drive prefix (`C:`), refused wherever an id or a name component would go.
fn is_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Refuses a session id that is not a single plain path component — no `/`, `\`, `..` or NUL,
/// not empty, no drive prefix. The wall that keeps host paths (worktrees among them) out.
pub(crate) fn check_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || is_drive_prefix(id)
    {
        return Err(invalid("session id", id));
    }
    Ok(())
}

/// Refuses a file name that is not relative with only normal components (`vm/token` is fine):
/// no absolute path, no empty or `.` or `..` component, no `\`, no NUL byte, no drive prefix.
pub(crate) fn check_name(name: &str) -> io::Result<()> {
    let normal = !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !is_drive_prefix(part));
    if !normal {
        return Err(invalid("file name", name));
    }
    Ok(())
}

/// The error both checks reject with: a bad name is the caller's mistake, not an I/O failure.
fn invalid(what: &str, value: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("invalid {what}: {value:?}"))
}

/// The local working copy's layout, named in one place. Every mothership keeps one under its data
/// dir — the default store *is* it, and a remote backend caches through it — because a microVM
/// mounts its session's `vm/` and `out/` from a host path. Code outside this module asks for these
/// paths only for what the store API cannot carry: a mount, a file handed to a subprocess by path,
/// a disk measurement (the allowlist in `the_session_layout_is_touched_only_through_the_store`).
pub(crate) fn local_sessions_root(data_dir: &Path) -> PathBuf {
    data_dir.join(SESSIONS)
}

/// One session's directory in the local working copy (see [`local_sessions_root`]).
pub(crate) fn local_session_dir(data_dir: &Path, id: &str) -> PathBuf {
    local_sessions_root(data_dir).join(id)
}

/// The index's path in the local working copy, for messages and the pre-update backup.
pub(crate) fn local_index(data_dir: &Path) -> PathBuf {
    data_dir.join(INDEX)
}

/// The default backend, byte for byte today's layout: `<root>/sessions.json` and one directory
/// per colony at `<root>/sessions/<id>/`. Writes go through `util::write_atomic` and appends
/// through `util::append_line`, so `util::faults` and today's durability carry over unchanged.
pub(crate) struct LocalDirStore {
    root: PathBuf,
}

impl LocalDirStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn index_path(&self) -> PathBuf {
        local_index(&self.root)
    }

    /// One session's directory, refusing anything that is not a single plain component.
    fn dir(&self, id: &str) -> io::Result<PathBuf> {
        check_id(id)?;
        Ok(local_session_dir(&self.root, id))
    }

    /// One file's path, refusing any name that is not relative and traversal-free.
    fn path(&self, id: &str, name: &str) -> io::Result<PathBuf> {
        check_name(name)?;
        Ok(self.dir(id)?.join(name))
    }

    /// Replaces `path` whole through `util::write_atomic` — the fault seam applies as before.
    async fn write_over(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        util::write_atomic(path, bytes).await.map_err(into_io_error)
    }

    /// Creates a validated path's parent (`vm/`, `out/`), the way the launch path does today.
    async fn create_parent(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().expect("a validated name always has a parent");
        tokio::fs::create_dir_all(parent).await
    }
}

impl SessionStore for LocalDirStore {
    fn read_index(&self) -> StoreFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { read_optional(&self.index_path()).await })
    }

    fn write_index<'a>(&'a self, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            tokio::fs::create_dir_all(&self.root).await?;
            self.write_over(&self.index_path(), bytes).await
        })
    }

    fn list_sessions(&self) -> StoreFuture<'_, Vec<String>> {
        Box::pin(async move {
            let mut ids = Vec::new();
            let mut entries = match tokio::fs::read_dir(self.root.join(SESSIONS)).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ids),
                Err(e) => return Err(e),
            };
            while let Some(entry) = entries.next_entry().await? {
                // `file_type` does not follow symlinks, and a name no operation could accept is
                // not a colony: skip both rather than hand out an unfetchable id.
                if !entry.file_type().await?.is_dir() {
                    continue;
                }
                if let Some(id) = entry.file_name().into_string().ok().filter(|n| check_id(n).is_ok()) {
                    ids.push(id);
                }
            }
            ids.sort();
            Ok(ids)
        })
    }

    fn read_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { read_optional(&self.path(id, name)?).await })
    }

    fn write_file<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path(id, name)?;
            self.create_parent(&path).await?;
            self.write_over(&path, bytes).await
        })
    }

    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path(id, name)?;
            self.create_parent(&path).await?;
            let line = ledger_line(line);
            let text = std::str::from_utf8(&line).map_err(|_| not_utf8(&path))?;
            util::append_line(&path, text).await.map_err(into_io_error)
        })
    }

    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>> {
        Box::pin(async move {
            let mut files = Vec::new();
            // Depth-first from the session's directory, carrying each file's path relative to it.
            let mut pending = vec![(self.dir(id)?, String::new())];
            while let Some((dir, prefix)) = pending.pop() {
                let mut entries = match tokio::fs::read_dir(&dir).await {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                while let Some(entry) = entries.next_entry().await? {
                    let Ok(name) = entry.file_name().into_string() else { continue };
                    let kind = entry.file_type().await?;
                    // Never follow a symlink: a swapped link must not smuggle a path in.
                    if kind.is_symlink() {
                        continue;
                    }
                    // The child is `dir/name`: `dir` already carries `prefix`, so joining the
                    // relative path instead would double it and silently skip anything two or more
                    // levels down.
                    let child = dir.join(&name);
                    let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                    if kind.is_dir() {
                        pending.push((child, relative));
                    } else {
                        files.push(relative);
                    }
                }
            }
            files.sort();
            Ok(files)
        })
    }

    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            match tokio::fs::remove_dir_all(self.dir(id)?).await {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        })
    }

    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>> {
        Box::pin(async move {
            match tokio::fs::metadata(self.index_path()).await {
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
                Ok(_) => {}
            }
            // The same move-aside the mothership's startup does, stamp and fault seam included.
            let saved = crate::move_corrupt_aside(&self.index_path()).map_err(into_io_error)?;
            Ok(saved.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
    }

    fn read_before<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        end: Option<u64>,
        max: u64,
    ) -> StoreFuture<'a, Option<(Vec<u8>, u64)>> {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let path = self.path(id, name)?;
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            let len = file.metadata().await?.len();
            let end = end.map_or(len, |e| e.min(len));
            // Seek to one byte before the budget: that byte says whether the cut is a line start.
            let from = end.saturating_sub(max.saturating_add(1));
            file.seek(io::SeekFrom::Start(from)).await?;
            let mut bytes = Vec::new();
            file.take(end - from).read_to_end(&mut bytes).await?;
            if from == 0 {
                return Ok(Some(window_before(&bytes, None, max)));
            }
            let tail = &bytes[1..];
            if bytes[0] == b'\n' {
                return Ok(Some((tail.to_vec(), from + 1)));
            }
            Ok(Some(match tail.iter().position(|b| *b == b'\n') {
                Some(at) => (tail[at + 1..].to_vec(), from + 1 + at as u64 + 1),
                None => (Vec::new(), end),
            }))
        })
    }

    fn read_tail<'a>(&'a self, id: &'a str, name: &'a str, max: u64) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let path = self.path(id, name)?;
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            // Seek to one byte before the budget: that byte says whether the cut is a line start.
            let len = file.metadata().await?.len();
            let start = len.saturating_sub(max.saturating_add(1));
            file.seek(io::SeekFrom::Start(start)).await?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).await?;
            if start == 0 {
                return Ok(Some(whole_line_tail(&bytes, max)));
            }
            // `bytes[0]` is the probe byte before the budget; the tail proper starts after it.
            let tail = &bytes[1..];
            if bytes[0] == b'\n' {
                return Ok(Some(tail.to_vec()));
            }
            Ok(Some(match tail.iter().position(|b| *b == b'\n') {
                Some(at) => tail[at + 1..].to_vec(),
                None => Vec::new(),
            }))
        })
    }

    fn stat<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<FileStat>> {
        Box::pin(async move {
            match tokio::fs::metadata(self.path(id, name)?).await {
                Ok(meta) if meta.is_file() => Ok(Some(FileStat {
                    len: meta.len(),
                    modified: meta.modified().ok(),
                })),
                Ok(_) => Ok(None),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    fn write_private<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let path = self.path(id, name)?;
            self.create_parent(&path).await?;
            util::write_private(&path, bytes).map_err(into_io_error)
        })
    }

    fn remove_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            match tokio::fs::remove_file(self.path(id, name)?).await {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        })
    }

    fn rename_file<'a>(&'a self, id: &'a str, from: &'a str, to: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let (from, to) = (self.path(id, from)?, self.path(id, to)?);
            self.create_parent(&to).await?;
            // The same fault seam the direct rename honoured, so a failed rotation stays testable.
            util::faults::check(&from, util::faults::Op::Rename)?;
            tokio::fs::rename(from, to).await
        })
    }
}

/// The error a non-UTF-8 append line is rejected with; appends are text by contract (they are
/// newline-joined log lines, and the memory backend keys and replays them as such).
fn not_utf8(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("an appended line must be UTF-8: {}", path.display()),
    )
}

/// Reads a file, treating "not there" as `None` — a missing index or file is a state, not an error.
async fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A reference backend that models a remote object store: a flat namespace of whole objects in
/// a [`BTreeMap`], keyed like the local layout minus its directories — `sessions.json` and
/// `sessions/<id>/<name>`. It is the proof that [`SessionStore`] is enough off the local disk:
/// whole-object puts, prefix listings, and one read-modify-write per append (safe under the
/// one-writer-per-session rule; a real backend adds a conditional put on the etag). What the
/// local layout had that a flat namespace does not is tabulated in docs/session-store.md.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Default)]
pub(crate) struct MemoryObjectStore {
    objects: std::sync::Mutex<BTreeMap<String, Vec<u8>>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl MemoryObjectStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The map, locked — poisoning is a panic, as everywhere in this crate. The guard never
    /// crosses an `await`; every operation is one synchronous read-modify-write.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.objects.lock().expect("poisoned")
    }

    /// The map's key for one session file, after both names are validated.
    fn key(id: &str, name: &str) -> io::Result<String> {
        check_id(id)?;
        check_name(name)?;
        Ok(format!("{SESSIONS}/{id}/{name}"))
    }

    /// Reads any object by its full key. Test-only: the trait deliberately cannot address a raw
    /// key — a quarantined index is exactly what callers must not read back as an index.
    #[cfg(test)]
    fn read_raw(&self, key: &str) -> Option<Vec<u8>> {
        self.lock().get(key).cloned()
    }
}

impl SessionStore for MemoryObjectStore {
    fn read_index(&self) -> StoreFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(self.lock().get(INDEX).cloned()) })
    }

    fn write_index<'a>(&'a self, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        // One whole-object put: the index appears with all of its bytes or not at all.
        Box::pin(async {
            self.lock().insert(INDEX.to_owned(), bytes.to_vec());
            Ok(())
        })
    }

    fn list_sessions(&self) -> StoreFuture<'_, Vec<String>> {
        Box::pin(async {
            let prefix = format!("{SESSIONS}/");
            let locked = self.lock();
            let ids: BTreeSet<String> = locked
                .keys()
                .filter_map(|key| key.strip_prefix(&prefix)?.split('/').next().map(str::to_owned))
                .collect();
            Ok(ids.into_iter().collect())
        })
    }

    fn read_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let key = Self::key(id, name)?;
            Ok(self.lock().get(&key).cloned())
        })
    }

    fn write_file<'a>(&'a self, id: &'a str, name: &'a str, bytes: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let key = Self::key(id, name)?;
            self.lock().insert(key, bytes.to_vec());
            Ok(())
        })
    }

    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            // Read-modify-write under the lock, held only inside this block: there is no O_APPEND
            // on an object store, and one writer per session is what keeps a lost update impossible.
            let key = Self::key(id, name)?;
            let line = ledger_line(line);
            let mut objects = self.lock();
            let object = objects.entry(key).or_default();
            object.extend_from_slice(&line);
            object.push(b'\n');
            Ok(())
        })
    }

    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>> {
        Box::pin(async move {
            check_id(id)?;
            let prefix = format!("{SESSIONS}/{id}/");
            let locked = self.lock();
            let mut files: Vec<String> = locked
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix).map(String::from))
                .collect();
            files.sort();
            Ok(files)
        })
    }

    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            check_id(id)?;
            let prefix = format!("{SESSIONS}/{id}/");
            self.lock().retain(|key, _| !key.starts_with(&prefix));
            Ok(())
        })
    }

    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>> {
        Box::pin(async move {
            let mut objects = self.lock();
            let Some(bytes) = objects.remove(INDEX) else { return Ok(None) };
            let name = format!("{INDEX}.corrupt-{}", quarantine_stamp());
            objects.insert(name.clone(), bytes);
            Ok(Some(name))
        })
    }

    fn remove_file<'a>(&'a self, id: &'a str, name: &'a str) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let key = Self::key(id, name)?;
            self.lock().remove(&key);
            Ok(())
        })
    }
}

/// The unix-seconds stamp every quarantine appends — the same shape the mothership's startup
/// move-aside uses. One-second resolution: an aside from an earlier second is never overwritten.
pub(crate) fn quarantine_stamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Copies every colony from `src` to `dst` and verifies the copy, leaving `src` untouched: a
/// migration only ever reads its source, so rollback is pointing the harness back at it.
///
/// Idempotent and resumable. The destination may be empty, hold part of this source's copy (an
/// interrupted run), or hold all of it (a finished run); anything else — an index that differs from
/// the source's, or a colony the source does not have — is someone else's store and is refused
/// before anything is written. A file already in the destination with the same bytes is skipped, so
/// a re-run copies only what is missing or changed, and a destination file the source's colony no
/// longer has is removed. Files are copied and verified (every listing, and every file's SHA-256)
/// first and the index is written last, so every failure before the final index check leaves `dst`
/// index-less — visibly unfinished, not half-written. With `dry_run`, nothing is written: the report
/// counts what would move.
pub(crate) async fn migrate(src: &dyn SessionStore, dst: &dyn SessionStore, dry_run: bool) -> io::Result<MigrationReport> {
    let index = src.read_index().await?;
    let sessions = src.list_sessions().await?;
    let theirs = dst.read_index().await?;
    if theirs.is_some() && theirs != index {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the destination already holds a different session index; migrate into an empty store, or into one an \
             earlier run of this migration was copying",
        ));
    }
    let foreign: Vec<String> = dst
        .list_sessions()
        .await?
        .into_iter()
        .filter(|id| !sessions.contains(id))
        .collect();
    if !foreign.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "the destination already holds colonies the source does not ({}); migrate into an empty store",
                foreign.join(", ")
            ),
        ));
    }
    let mut report = MigrationReport {
        already_done: theirs.is_some(),
        ..MigrationReport::default()
    };
    let mut manifest = ring::digest::Context::new(&ring::digest::SHA256);
    for id in &sessions {
        let names = src.list_files(id).await?;
        for name in &names {
            let Some(bytes) = src.read_file(id, name).await? else {
                continue;
            };
            report.files += 1;
            report.bytes += bytes.len() as u64;
            manifest.update(format!("{id}/{name} {}\n", sha256_hex(&bytes)).as_bytes());
            if dst.read_file(id, name).await?.as_deref() == Some(bytes.as_slice()) {
                report.skipped += 1;
                continue;
            }
            report.copied += 1;
            report.copied_bytes += bytes.len() as u64;
            if !dry_run {
                put_like(dst, id, name, &bytes).await?;
            }
        }
        // A file an interrupted run copied that the source's colony no longer has would fail the
        // listing check below; it is not the source's, so it goes.
        for stale in dst.list_files(id).await?.into_iter().filter(|n| !names.contains(n)) {
            report.removed += 1;
            if !dry_run {
                dst.remove_file(id, &stale).await?;
            }
        }
        report.sessions += 1;
    }
    let Some(index) = index else {
        report.checksum = hex(manifest.finish().as_ref());
        return Ok(report);
    };
    report.bytes += index.len() as u64;
    manifest.update(format!("{INDEX} {}\n", sha256_hex(&index)).as_bytes());
    report.checksum = hex(manifest.finish().as_ref());
    if dry_run {
        return Ok(report);
    }
    // The copy proves itself before the index exists: a failed verification leaves dst with
    // orphaned files but no index, which every reader treats as a first-run store.
    verify_copy(src, dst, &sessions).await?;
    // The index goes last: it is what turns the copy into a store worth switching to.
    if !report.already_done {
        dst.write_index(&index).await?;
    }
    if dst.read_index().await?.as_deref() != Some(index.as_slice()) {
        let why = "verification failed: the destination's index does not match the source's";
        return Err(io::Error::other(why));
    }
    Ok(report)
}

/// Writes one migrated file the way its kind is written: a credential (`vm/token`, the gateway
/// token, the stored issue) owner-only through [`SessionStore::write_private`], the rest plainly.
async fn put_like(dst: &dyn SessionStore, id: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
    if crate::archive::is_credential_file(name) || name == "issue.json" {
        dst.write_private(id, name, bytes).await
    } else {
        dst.write_file(id, name, bytes).await
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What one migration moved (or would move). `sessions`, `files` and `bytes` are the whole source
/// (the index's bytes included); `copied` and `skipped` split its files into those written this run
/// and those the destination already held byte for byte, and `removed` counts destination files the
/// source no longer has. `checksum` is the SHA-256 of the source's manifest — every file's path and
/// SHA-256, then the index's — the figure two runs over the same source agree on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct MigrationReport {
    pub sessions: usize,
    pub files: usize,
    pub bytes: u64,
    pub copied: usize,
    pub copied_bytes: u64,
    pub skipped: usize,
    pub removed: usize,
    /// The destination already held this source's index: an earlier run finished, and this one
    /// only re-verified it.
    pub already_done: bool,
    pub checksum: String,
}

/// Reads the copied files back out of `dst` and refuses to call the copy done unless the
/// listings match and every file's SHA-256 matches. (The index is verified in `migrate` itself,
/// after it is written.)
async fn verify_copy(src: &dyn SessionStore, dst: &dyn SessionStore, sessions: &[String]) -> io::Result<()> {
    let copied = dst.list_sessions().await?;
    if copied != sessions {
        let why = format!("verification failed: destination holds {copied:?}, source holds {sessions:?}");
        return Err(io::Error::other(why));
    }
    for id in sessions {
        let names = src.list_files(id).await?;
        if dst.list_files(id).await? != names {
            let why = format!("verification failed: colony {id}'s files do not match");
            return Err(io::Error::other(why));
        }
        for name in names {
            let want = src.read_file(id, &name).await?.map(|b| sha256_hex(&b));
            if dst.read_file(id, &name).await?.map(|b| sha256_hex(&b)) != want {
                let why = format!("verification failed: colony {id}'s {name} does not match its checksum");
                return Err(io::Error::other(why));
            }
        }
    }
    Ok(())
}

/// `colonizer migrate-store`: the directory-to-directory form that predates `colonizer sessions
/// migrate` (kept so scripts written against it still run). It copies this machine's colonies into
/// another local store, or with `--dry-run` counts what would move, and never changes the setting:
/// the switch it prints is the operator's to make. `from` defaults to the configured data dir.
pub(crate) async fn cli_migrate(cfg: &Settings, from: Option<PathBuf>, to: PathBuf, dry_run: bool, json: bool) -> Result<()> {
    use crate::store_config::{same_dir, summary};
    let from = from.unwrap_or_else(|| cfg.data_dir.clone());
    if same_dir(&from, &to) {
        bail!(
            "--from and --to name the same store ({}); there is nothing to copy",
            to.display()
        );
    }
    // A migration only ever reads its source, so what must not happen is a running mothership
    // writing that source while it is copied — the single-writer rule.
    if same_dir(&from, &cfg.data_dir) {
        crate::store_config::refuse_while_serving(cfg).await?;
    }
    let src = LocalDirStore::new(from.clone());
    let dst = LocalDirStore::new(to.clone());
    let report = migrate(&src, &dst, dry_run).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let (from_text, to_text) = (from.display().to_string(), to.display().to_string());
    println!("{}", summary(&report, dry_run, &from_text, &to_text));
    if !dry_run {
        // The copy never touched the source, so this is a switch, not a move: it is the operator's
        // to make, and rollback is pointing the mothership back at what it wrote before.
        println!(
            "point the mothership at the new store with COLONIZER_DATA_DIR={} (the old store is untouched; roll back by pointing back at it)",
            to.display()
        );
    }
    Ok(())
}

/// The store speaks `io::Result`; the fs helpers speak `anyhow`. The kind of a bare `io::Error` (an
/// injected fault) is what callers and tests act on — a full disk must still read `StorageFull` —
/// so it is carried across; the message is the whole `{e:#}` chain, so a wrapped write keeps its
/// "could not append to …/events.jsonl: Permission denied" rather than collapsing to the os error
/// alone, which is the text a caller reporting it with `{e:#}` means to show.
fn into_io_error(e: Error) -> io::Error {
    match e.downcast_ref::<io::Error>() {
        Some(io) => io::Error::new(io.kind(), format!("{e:#}")),
        None => io::Error::other(format!("{e:#}")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::util::faults::{Op, inject};
    use crate::{sessions::Session, util};
    use serde_json::Value;
    use std::io::ErrorKind;

    /// A throwaway store root, as in sessions.rs.
    pub(crate) fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("colonizer-store-{tag}-{}", util::short_id()))
    }

    pub(crate) fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    pub(crate) fn event(seq: u64) -> Vec<u8> {
        format!(r#"{{"seq":{seq},"type":"progress","message":"step {seq}"}}"#).into_bytes()
    }

    pub(crate) fn lines(events: &[Vec<u8>]) -> Vec<u8> {
        events.iter().flat_map(|e| [e.as_slice(), b"\n"].concat()).collect()
    }

    /// `write_file` + `unwrap`, so seeds read as one line per file.
    pub(crate) async fn put(s: &dyn SessionStore, id: &str, name: &str, bytes: &[u8]) {
        s.write_file(id, name, bytes).await.unwrap()
    }

    /// A backend with a label for assert messages.
    type Labeled<'s> = (&'static str, Box<dyn SessionStore + 's>);

    /// Both reference backends; the local one's temp root comes back so the caller can clean up.
    fn backends(tag: &str) -> (PathBuf, Vec<Labeled<'_>>) {
        let root = temp_root(tag);
        let stores: Vec<Labeled<'_>> = vec![
            ("local", Box::new(LocalDirStore::new(&root))),
            ("memory", Box::new(MemoryObjectStore::new())),
        ];
        (root, stores)
    }

    // Unwrapping read shorthands, so each promise below stays a one-line assert (rustfmt spreads
    // any macro argument whose content runs past `fn_call_width` over five lines).
    async fn idx(s: &dyn SessionStore) -> Option<Vec<u8>> {
        s.read_index().await.unwrap()
    }
    async fn file(s: &dyn SessionStore, id: &str, name: &str) -> Option<Vec<u8>> {
        s.read_file(id, name).await.unwrap()
    }
    async fn ids(s: &dyn SessionStore) -> Vec<String> {
        s.list_sessions().await.unwrap()
    }
    async fn files(s: &dyn SessionStore, id: &str) -> Vec<String> {
        s.list_files(id).await.unwrap()
    }

    /// The read side of the append contract, and the whole reason `seq` exists: drop any line
    /// whose `seq` is at or below the last one seen. Per file — a resume rotates `events.jsonl`
    /// to `events-N.jsonl` and the new file's `seq` restarts at 1, so the rule never spans files.
    fn replay(log: &[u8]) -> Vec<Vec<u8>> {
        let mut kept = Vec::new();
        let mut last: u64 = 0;
        for line in std::str::from_utf8(log).unwrap().lines() {
            let seq: u64 = serde_json::from_str::<Value>(line).unwrap()["seq"].as_u64().unwrap();
            if seq > last {
                last = seq;
                kept.push(line.as_bytes().to_vec());
            }
        }
        kept
    }

    /// Everything a backend must do, run against both of them below.
    pub(crate) async fn contract(s: &dyn SessionStore, label: &str) {
        assert_eq!(idx(s).await, None, "{label}: an empty store reads as a first run");
        // The index is replaced whole: a later read sees exactly the newest bytes.
        s.write_index(br#"[{"id":"a"}]"#).await.unwrap();
        assert_eq!(idx(s).await.unwrap(), br#"[{"id":"a"}]"#, "{label}: the index round-trips");
        s.write_index(br#"[{"id":"a"},{"id":"b"}]"#).await.unwrap();
        let whole = idx(s).await.unwrap();
        assert_eq!(whole, br#"[{"id":"a"},{"id":"b"}]"#, "{label}: replaced whole");
        // Files: nested names round-trip byte for byte; missing names read as None.
        s.write_file("a", "vm/token", b"tok-244-bits").await.unwrap();
        let token = file(s, "a", "vm/token").await.unwrap();
        assert_eq!(token, b"tok-244-bits", "{label}: nested files round-trip");
        assert_eq!(file(s, "a", "no/such/file").await, None, "{label}: missing reads as None");
        // Appends: one line each with the newline added. The repeated seq 1 models agentd's
        // re-fetch after a reconnect, and the replay rule drops it on read.
        s.append("a", "events.jsonl", &event(1)).await.unwrap();
        s.append("a", "events.jsonl", &event(1)).await.unwrap();
        s.append("a", "events.jsonl", &event(2)).await.unwrap();
        let raw = file(s, "a", "events.jsonl").await.unwrap();
        assert_eq!(raw, lines(&[event(1), event(1), event(2)]), "{label}: one line per append");
        assert_eq!(replay(&raw), vec![event(1), event(2)], "{label}: replay drops the repeat");
        // The listing is the whole session, nested files included, sorted.
        s.write_file("a", "harness.jsonl", b"{}\n").await.unwrap();
        s.write_file("a", "out/deep/er/note.md", b"deep").await.unwrap();
        let listed = files(s, "a").await;
        assert_eq!(
            listed,
            ["events.jsonl", "harness.jsonl", "out/deep/er/note.md", "vm/token"],
            "{label}: sorted, however deep"
        );
        assert!(files(s, "ghost").await.is_empty(), "{label}: unknown session lists as empty");
        // A second session shows up, sorted.
        s.write_file("b", "out/pr.md", b"# Title\n").await.unwrap();
        assert_eq!(ids(s).await, ["a", "b"], "{label}: sessions list sorted");
        // Quarantine takes the index out from under every reader.
        let aside = s.quarantine_index().await.unwrap().expect("an index to quarantine");
        assert!(aside.starts_with("sessions.json.corrupt-"), "{label}: {aside}");
        assert_eq!(idx(s).await, None, "{label}: quarantined is not the index");
        // Removal takes the whole session, and is idempotent.
        s.remove_session("b").await.unwrap();
        s.remove_session("b").await.unwrap();
        assert_eq!(ids(s).await, ["a"], "{label}: removal is idempotent");
        assert_eq!(file(s, "b", "out/pr.md").await, None, "{label}: removal takes the files");
        derived(s, label).await;
    }

    /// The derived operations (#610), each against the promise in its doc comment.
    pub(crate) async fn derived(s: &dyn SessionStore, label: &str) {
        let log = b"one\ntwo\nthree\n";
        s.write_file("c", "log.jsonl", log).await.unwrap();
        let tail = |max: u64| async move { s.read_tail("c", "log.jsonl", max).await.unwrap().unwrap() };
        assert_eq!(tail(100).await, log, "{label}: a budget past the file is the whole file");
        assert_eq!(tail(6).await, b"three\n", "{label}: a cut on a line start keeps that line");
        assert_eq!(tail(8).await, b"three\n", "{label}: a mid-line cut drops the partial line");
        assert_eq!(tail(3).await, b"", "{label}: no whole line fits");
        assert_eq!(s.read_tail("c", "absent", 10).await.unwrap(), None, "{label}: absent is None");
        let stat = s.stat("c", "log.jsonl").await.unwrap().unwrap();
        assert_eq!(stat.len, log.len() as u64, "{label}: stat's length");
        assert_eq!(s.stat("c", "absent").await.unwrap(), None, "{label}: stat of nothing");
        // A rename moves the bytes; renaming nothing is an error, not a silent no-op.
        s.rename_file("c", "log.jsonl", "log-1.jsonl").await.unwrap();
        assert_eq!(file(s, "c", "log.jsonl").await, None, "{label}: the old name is gone");
        assert_eq!(file(s, "c", "log-1.jsonl").await.unwrap(), log, "{label}: renamed whole");
        let err = s.rename_file("c", "log.jsonl", "log-2.jsonl").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound, "{label}: {err}");
        // A private write reads back like any other; removing a file is idempotent.
        s.write_private("c", "vm/token", b"secret").await.unwrap();
        assert_eq!(
            file(s, "c", "vm/token").await.unwrap(),
            b"secret",
            "{label}: private round-trip"
        );
        s.remove_file("c", "vm/token").await.unwrap();
        s.remove_file("c", "vm/token").await.unwrap();
        assert_eq!(files(s, "c").await, ["log-1.jsonl"], "{label}: removed, idempotently");
        let refused = s.remove_file("c", "../escape").await.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::InvalidInput, "{label}: names are checked");
        s.remove_session("c").await.unwrap();
    }

    /// A host path — the shape every worktree has — and every traversal spelling, refused wherever
    /// an id or a file name is taken, with nothing written anywhere.
    pub(crate) async fn refuses_host_paths(s: &dyn SessionStore, label: &str) {
        let worktree = "/home/u/.local/share/colonizer/worktrees/a1b2c3d4";
        for id in [worktree, "../victim", "a/../b", "C:", ""] {
            let refused = [
                ("write_file", s.write_file(id, "x", b"").await.err()),
                ("read_file", s.read_file(id, "x").await.err()),
                ("append", s.append(id, "x", b"").await.err()),
                ("list_files", s.list_files(id).await.err()),
                ("remove_session", s.remove_session(id).await.err()),
            ];
            for (op, err) in refused {
                let kind = err.unwrap().kind();
                assert_eq!(kind, ErrorKind::InvalidInput, "{label}: {op} id {id:?}");
            }
        }
        for name in [
            "/etc/passwd",
            "../escape",
            "vm/../token",
            "vm\\token",
            "",
            "out/",
            "C:/passwd",
        ] {
            let refused = [
                ("write_file", s.write_file("safe", name, b"x").await.err()),
                ("append", s.append("safe", name, b"x").await.err()),
            ];
            for (op, err) in refused {
                let kind = err.unwrap().kind();
                assert_eq!(kind, ErrorKind::InvalidInput, "{label}: {op} name {name:?}");
            }
        }
        assert!(ids(s).await.is_empty(), "{label}: nothing was written");
        assert_eq!(idx(s).await, None, "{label}: the index is untouched");
        // The wall is on the shape, not on writing: a plain id and name go through.
        s.write_file("safe", "vm/token", b"tok").await.unwrap();
        assert_eq!(ids(s).await, ["safe"], "{label}: plain names are accepted");
    }

    #[tokio::test]
    async fn the_contract_holds_for_both_reference_backends() {
        let (root, backends) = backends("contract");
        for (label, store) in &backends {
            contract(store.as_ref(), label).await;
        }
        cleanup(&root);
    }

    #[tokio::test]
    async fn host_paths_are_refused_by_both_reference_backends() {
        let (root, backends) = backends("refuse");
        for (label, store) in &backends {
            refuses_host_paths(store.as_ref(), label).await;
        }
        cleanup(&root);
    }

    /// #761 at the store layer: whichever caller appends, a credential in the line is stored only
    /// as its mark, a JSON line stays valid JSON, and a clean line is stored byte for byte.
    #[tokio::test]
    async fn both_reference_backends_redact_every_appended_line() {
        let (root, backends) = backends("redact");
        let token = "ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5";
        for (label, s) in &backends {
            let leaked = format!(r#"{{"seq":1,"type":"text","text":"echo {token}"}}"#);
            s.append("abc", "events.jsonl", leaked.as_bytes()).await.unwrap();
            s.append("abc", "events.jsonl", &event(2)).await.unwrap();
            let plain = format!("plain text {token}");
            s.append("abc", "harness.jsonl", plain.as_bytes()).await.unwrap();
            let text = String::from_utf8(file(s.as_ref(), "abc", "events.jsonl").await.unwrap()).unwrap();
            assert!(!text.contains(token), "{label}: {text}");
            let first: Value = serde_json::from_str(text.lines().next().unwrap()).expect("still JSON");
            assert_eq!(first["text"], "echo [REDACTED:github_token]", "{label}");
            assert_eq!(
                text.lines().nth(1).unwrap().as_bytes(),
                event(2),
                "{label}: a clean line is kept"
            );
            let harness = String::from_utf8(file(s.as_ref(), "abc", "harness.jsonl").await.unwrap()).unwrap();
            assert_eq!(harness, "plain text [REDACTED:github_token]\n", "{label}");
        }
        cleanup(&root);
    }

    #[tokio::test]
    async fn a_quarantined_index_keeps_its_bytes() {
        // Local: the aside is a file next to the index, with the bytes as they were found.
        let root = temp_root("quarantine");
        let local = LocalDirStore::new(&root);
        local.write_index(b"not really json").await.unwrap();
        let aside = local.quarantine_index().await.unwrap().unwrap();
        assert_eq!(std::fs::read(root.join(&aside)).unwrap(), b"not really json");
        assert!(!root.join("sessions.json").exists());
        // A second quarantine with no index to move says so.
        assert_eq!(local.quarantine_index().await.unwrap(), None);
        cleanup(&root);
        // Memory: the aside keeps its bytes, under its own key, and is not the index.
        let memory = MemoryObjectStore::new();
        memory.write_index(b"not really json").await.unwrap();
        let aside = memory.quarantine_index().await.unwrap().unwrap();
        assert_eq!(memory.read_raw(&aside).unwrap(), b"not really json");
        assert_eq!(memory.read_index().await.unwrap(), None);
    }

    /// Two colonies with the files a live one actually has, plus the index over them. Returns the
    /// ids so a test can walk what it planted.
    pub(crate) async fn seed(s: &dyn SessionStore) -> Vec<String> {
        let moon = b"{\n  \"session_id\": \"a1b2c3d4\"\n}\n";
        put(s, "a1b2c3d4", "events.jsonl", &lines(&[event(1), event(2)])).await;
        put(s, "a1b2c3d4", "vm/session.json", moon).await;
        put(s, "a1b2c3d4", "out/pr.md", b"# Colonize the moon\n\nCloses #1.\n").await;
        put(s, "e5f60718", "events.jsonl", &lines(&[event(1)])).await;
        put(s, "e5f60718", "findings.jsonl", b"{\"title\":\"a finding\"}\n").await;
        s.write_index(br#"[{"id":"a1b2c3d4"},{"id":"e5f60718"}]"#).await.unwrap();
        vec!["a1b2c3d4".into(), "e5f60718".into()]
    }

    /// The bytes `seed` put into a store: the figure the migration tests must report.
    pub(crate) async fn seeded_volume(s: &dyn SessionStore) -> u64 {
        let mut bytes = s.read_index().await.unwrap().unwrap().len() as u64;
        for id in s.list_sessions().await.unwrap() {
            for name in s.list_files(&id).await.unwrap() {
                bytes += s.read_file(&id, &name).await.unwrap().unwrap().len() as u64;
            }
        }
        bytes
    }

    /// The migration procedure, end to end: local to the remote-shaped store and back to a new
    /// local root, with every listing and byte compared across all three.
    #[tokio::test]
    async fn a_migration_round_trips_local_to_memory_and_back_byte_for_byte() {
        let source_root = temp_root("mig-src");
        let source = LocalDirStore::new(&source_root);
        let ids = seed(&source).await;

        let remote = MemoryObjectStore::new();
        let report = migrate(&source, &remote, false).await.unwrap();
        let volume = seeded_volume(&source).await;
        assert_eq!((report.sessions, report.files, report.bytes), (2, 5, volume));

        let target_root = temp_root("mig-dst");
        let target = LocalDirStore::new(&target_root);
        assert_eq!(migrate(&remote, &target, false).await.unwrap(), report);
        // The same store under three addresses: index bytes and every file byte identical.
        assert_eq!(target.read_index().await.unwrap(), source.read_index().await.unwrap());
        for id in &ids {
            assert_eq!(target.list_files(id).await.unwrap(), source.list_files(id).await.unwrap());
            for name in source.list_files(id).await.unwrap() {
                let got = target.read_file(id, &name).await.unwrap();
                assert_eq!(got, source.read_file(id, &name).await.unwrap(), "{id}/{name}");
            }
        }
        cleanup(&source_root);
        cleanup(&target_root);
    }

    #[tokio::test]
    async fn a_dry_run_migration_counts_without_writing() {
        let source_root = temp_root("mig-dry");
        let source = LocalDirStore::new(&source_root);
        seed(&source).await;

        let remote = MemoryObjectStore::new();
        let report = migrate(&source, &remote, true).await.unwrap();
        let volume = seeded_volume(&source).await;
        assert_eq!((report.sessions, report.files, report.bytes), (2, 5, volume));
        // Counted, not moved: the destination is still a first-run store.
        assert_eq!(remote.read_index().await.unwrap(), None);
        assert!(remote.list_sessions().await.unwrap().is_empty());
        // And the source never knew about any of it.
        let index = source.read_index().await.unwrap().unwrap();
        assert_eq!(index, br#"[{"id":"a1b2c3d4"},{"id":"e5f60718"}]"#);
        cleanup(&source_root);
    }

    /// The settings `cli_migrate` reads: only `data_dir` and `bind` matter, and `data_dir` is left
    /// pointing somewhere other than the test's source, so the running-mothership guard is skipped.
    fn cli_settings(data_dir: PathBuf) -> Settings {
        Settings {
            bind: "127.0.0.1:0".into(),
            data_dir,
            config_dir: PathBuf::new(),
            runtime_dir: PathBuf::new(),
            assets: None,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: Vec::new(),
            fleet_peers: Vec::new(),
            bench_pool: None,
        }
    }

    /// `colonizer migrate-store`: `--dry-run` counts without touching the destination, the real run
    /// copies the same colonies in, and a store cannot be its own destination.
    #[tokio::test]
    async fn the_migrate_store_command_dry_runs_then_copies() {
        let source_root = temp_root("cli-src");
        let source = LocalDirStore::new(&source_root);
        seed(&source).await;
        let target_root = temp_root("cli-dst");
        let cfg = cli_settings(temp_root("cli-cfg"));

        // Dry run: nothing lands in the destination, which stays a first-run store.
        cli_migrate(&cfg, Some(source_root.clone()), target_root.clone(), true, false)
            .await
            .unwrap();
        let target = LocalDirStore::new(&target_root);
        assert_eq!(target.read_index().await.unwrap(), None);
        assert!(target.list_sessions().await.unwrap().is_empty(), "a dry run writes nothing");

        // Real run: the copy verifies itself, and the destination now holds the same colonies.
        cli_migrate(&cfg, Some(source_root.clone()), target_root.clone(), false, false)
            .await
            .unwrap();
        assert_eq!(target.read_index().await.unwrap(), source.read_index().await.unwrap());
        assert_eq!(target.list_sessions().await.unwrap(), source.list_sessions().await.unwrap());

        // A store cannot be copied into itself.
        let err = cli_migrate(&cfg, Some(target_root.clone()), target_root.clone(), false, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("the same store"), "{err}");
        cleanup(&source_root);
        cleanup(&target_root);
    }

    #[tokio::test]
    async fn a_migration_refuses_a_destination_that_already_holds_other_colonies() {
        let source_root = temp_root("mig-full-src");
        let source = LocalDirStore::new(&source_root);
        seed(&source).await;

        // A destination in use: its own index over its own colony.
        let target_root = temp_root("mig-full-dst");
        let target = LocalDirStore::new(&target_root);
        put(&target, "f0f0f0f0", "events.jsonl", &lines(&[event(9)])).await;
        target.write_index(br#"[{"id":"f0f0f0f0"}]"#).await.unwrap();

        let err = migrate(&source, &target, false).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");
        assert!(err.to_string().contains("different session index"), "{err}");

        // The same with no index there yet: a colony the source does not have is someone else's.
        target.quarantine_index().await.unwrap();
        let err = migrate(&source, &target, false).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");
        assert!(err.to_string().contains("f0f0f0f0"), "the refusal names the stranger: {err}");

        // Nothing moved, in either direction.
        assert_eq!(target.list_sessions().await.unwrap(), ["f0f0f0f0"]);
        assert_eq!(file(&target, "f0f0f0f0", "events.jsonl").await.unwrap(), lines(&[event(9)]));
        let index = source.read_index().await.unwrap().unwrap();
        assert_eq!(index, br#"[{"id":"a1b2c3d4"},{"id":"e5f60718"}]"#);
        cleanup(&source_root);
        cleanup(&target_root);
    }

    /// Resumable and idempotent: a run cut short by a failed write leaves the destination
    /// index-less; the re-run copies only what is missing and finishes; a third run copies nothing,
    /// re-verifies, and reports the same checksum.
    #[tokio::test]
    async fn an_interrupted_migration_resumes_and_a_finished_one_reruns_idempotently() {
        let source_root = temp_root("mig-resume-src");
        let source = LocalDirStore::new(&source_root);
        seed(&source).await;
        let target_root = temp_root("mig-resume-dst");
        let target = LocalDirStore::new(&target_root);

        let full = || io::Error::new(io::ErrorKind::StorageFull, "the disk is full");
        let fault = inject("e5f60718", Op::Write, full);
        migrate(&source, &target, false).await.unwrap_err();
        drop(fault);
        assert_eq!(
            target.read_index().await.unwrap(),
            None,
            "an interrupted run is visibly unfinished"
        );

        // The dry run of the resume counts what is left: the second colony's two files.
        let dry = migrate(&source, &target, true).await.unwrap();
        assert_eq!((dry.copied, dry.skipped, dry.already_done), (2, 3, false));
        assert_eq!(target.read_index().await.unwrap(), None, "a dry run writes nothing");

        let resumed = migrate(&source, &target, false).await.unwrap();
        assert_eq!(
            (resumed.sessions, resumed.files, resumed.copied, resumed.skipped),
            (2, 5, 2, 3)
        );
        assert_eq!(target.read_index().await.unwrap(), source.read_index().await.unwrap());

        let again = migrate(&source, &target, false).await.unwrap();
        assert_eq!((again.copied, again.skipped, again.already_done), (0, 5, true));
        assert_eq!(again.checksum, resumed.checksum, "the same source, the same checksum");
        assert_eq!(again.checksum.len(), 64, "a SHA-256, hex");
        cleanup(&source_root);
        cleanup(&target_root);
    }

    /// A destination file the source's colony no longer has (an earlier, interrupted run copied it;
    /// the colony then rotated or lost it) is removed, so the copy still verifies; a changed file is
    /// copied over, not skipped.
    #[tokio::test]
    async fn a_resumed_migration_replaces_changed_files_and_drops_stale_ones() {
        let source_root = temp_root("mig-stale-src");
        let source = LocalDirStore::new(&source_root);
        seed(&source).await;
        let target_root = temp_root("mig-stale-dst");
        let target = LocalDirStore::new(&target_root);
        put(&target, "a1b2c3d4", "events.jsonl", b"an older copy\n").await;
        put(&target, "a1b2c3d4", "events-1.jsonl", b"gone from the source\n").await;

        let report = migrate(&source, &target, false).await.unwrap();
        assert_eq!((report.copied, report.skipped, report.removed), (5, 0, 1));
        let listed = target.list_files("a1b2c3d4").await.unwrap();
        assert_eq!(listed, source.list_files("a1b2c3d4").await.unwrap());
        let events = file(&target, "a1b2c3d4", "events.jsonl").await.unwrap();
        assert_eq!(events, lines(&[event(1), event(2)]), "the changed file was copied over");
        cleanup(&source_root);
        cleanup(&target_root);
    }

    /// A migrated credential keeps its owner-only mode in a local destination.
    #[tokio::test]
    async fn a_migration_keeps_credentials_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let source = MemoryObjectStore::new();
        put(&source, "a1b2c3d4", "vm/token", b"tok").await;
        put(&source, "a1b2c3d4", "issue.json", b"{}").await;
        put(&source, "a1b2c3d4", "events.jsonl", &lines(&[event(1)])).await;
        source.write_index(br#"[{"id":"a1b2c3d4"}]"#).await.unwrap();
        let target_root = temp_root("mig-private");
        migrate(&source, &LocalDirStore::new(&target_root), false).await.unwrap();
        let mode = |name: &str| {
            let path = local_session_dir(&target_root, "a1b2c3d4").join(name);
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode("vm/token"), 0o600);
        assert_eq!(mode("issue.json"), 0o600);
        assert_ne!(mode("events.jsonl"), 0o600, "a log is written plainly");
        cleanup(&target_root);
    }

    #[tokio::test]
    async fn a_failed_migration_leaves_the_source_whole_and_the_destination_without_an_index() {
        let source_root = temp_root("mig-fail");
        let source = LocalDirStore::new(&source_root);
        let ids = seed(&source).await;
        let target_root = temp_root("mig-fail-dst");
        let target = LocalDirStore::new(&target_root);

        // The local fault seam (the same one write_atomic already honours) fails the destination's
        // fourth write: the first colony landed, and the index is never reached — it goes last.
        let full = || io::Error::new(io::ErrorKind::StorageFull, "the object store is full");
        let _fault = inject("e5f60718", Op::Write, full);
        let err = migrate(&source, &target, false).await.unwrap_err();
        // The fault fires before write_atomic adds context, so the bare io error — and its kind —
        // survives the anyhow round-trip (into_io_error downcasts it back out).
        assert_eq!(err.kind(), ErrorKind::StorageFull, "{err}");

        // The source is exactly as seeded — a migration never writes it.
        let index = source.read_index().await.unwrap().unwrap();
        assert_eq!(index, br#"[{"id":"a1b2c3d4"},{"id":"e5f60718"}]"#);
        for id in &ids {
            for name in source.list_files(id).await.unwrap() {
                let kept = source.read_file(id, &name).await.unwrap();
                assert!(kept.is_some(), "{id}/{name} still there");
            }
        }
        // The destination has the landed files but no index: an unfinished migration reads as a
        // first run, never as a half-written one. (The failed colony's empty directory may exist —
        // the local store creates parents before writing — but it holds no files.)
        assert_eq!(target.read_index().await.unwrap(), None);
        assert_eq!(target.list_files("a1b2c3d4").await.unwrap().len(), 3);
        assert!(target.list_files("e5f60718").await.unwrap().is_empty());
        cleanup(&source_root);
        cleanup(&target_root);
    }

    /// The checked-in data directory as v0.1.9 wrote it — the shape the default store must keep
    /// reading and writing byte for byte.
    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/data-v0.1.9")
    }

    fn fixture_bytes(relative: &str) -> Vec<u8> {
        std::fs::read(fixture_root().join(relative)).unwrap()
    }

    /// Copies the checked-in fixture tree into a temp root the store can own.
    fn copy_fixture(root: &Path) {
        let mut pending = vec![(fixture_root(), root.to_path_buf())];
        while let Some((from, to)) = pending.pop() {
            std::fs::create_dir_all(&to).unwrap();
            for entry in std::fs::read_dir(&from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    pending.push((entry.path(), target));
                } else {
                    std::fs::copy(entry.path(), &target).unwrap();
                }
            }
        }
    }

    #[tokio::test]
    async fn the_v0_1_9_fixture_reads_and_writes_byte_for_byte() {
        let root = temp_root("fixture");
        copy_fixture(&root);
        let store = LocalDirStore::new(&root);

        let index = fixture_bytes("sessions.json");
        let read = store.read_index().await.unwrap().unwrap();
        assert_eq!(read, index, "the store reads the checked-in index");

        // The bytes are the real thing: today's records parse straight out of the release's file.
        let sessions: Vec<Session> = serde_json::from_slice(&index).unwrap();
        let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, ["a1b2c3d4", "e5f60718"]);
        let worktree = format!("/home/u/.local/share/colonizer/worktrees/{}", ids[0]);
        assert_eq!(sessions[0].worktree, worktree);
        assert_eq!(sessions[1].status, crate::sessions::SessionStatus::Merged);
        // Every fixture file is listed — nested names included — and reads back identical.
        let first = store.list_files("a1b2c3d4").await.unwrap();
        assert_eq!(first, ["events.jsonl", "findings.jsonl", "out/pr.md", "vm/session.json"]);
        let second = store.list_files("e5f60718").await.unwrap();
        assert_eq!(second, ["events.jsonl", "harness.jsonl"]);
        assert_eq!(store.list_sessions().await.unwrap(), ids);
        for id in &ids {
            for name in store.list_files(id).await.unwrap() {
                let got = store.read_file(id, &name).await.unwrap().unwrap();
                assert_eq!(got, fixture_bytes(&format!("sessions/{id}/{name}")), "{id}/{name}");
            }
        }
        // Writing the same index bytes back leaves the file byte-identical on disk.
        store.write_index(&index).await.unwrap();
        assert_eq!(fixture_bytes("sessions.json"), index);
        // A new append lands in the same file, after the lines already there.
        store.append(&ids[0], "events.jsonl", &event(3)).await.unwrap();
        let path = root.join("sessions").join(&ids[0]).join("events.jsonl");
        let mut expected = fixture_bytes("sessions/a1b2c3d4/events.jsonl");
        expected.extend_from_slice(&event(3));
        expected.push(b'\n');
        let log = std::fs::read(&path).unwrap();
        assert_eq!(log, expected, "the appended line continues the fixture's log");
        assert_eq!(replay(&log).len(), 3, "two fixture lines plus the new one replay as three");
        cleanup(&root);
    }

    /// Every file outside this module that names a path in the session layout, how many times, and
    /// what it does there that the store API cannot carry. Everything else — the index, the event
    /// and harness logs, the findings, GitHub and message ledgers, commit links, claims, the stored
    /// issue, the egress record, the colony's tokens — is read and written through `SessionStore`.
    /// A new path into the layout fails `the_session_layout_is_touched_only_through_the_store`
    /// until it is routed through the store or listed here with its reason.
    const LAYOUT_ALLOWLIST: &[(&str, usize, &str)] = &[
        (
            "boot.rs",
            1,
            "the microVM's mounts: vm/ (path policy, vault, memory, mounts list), out/, transcripts/, services/",
        ),
        (
            "decisions.rs",
            1,
            "the VM-written out/pr.md, read for a held publish's secret note",
        ),
        (
            "deja.rs",
            1,
            "copies the VM-written transcripts/ directory into the deja index",
        ),
        (
            "events.rs",
            3,
            "the VM-written out/pr.md, and finding-body.md handed to `gh --body-file` by path",
        ),
        (
            "findings_queue.rs",
            3,
            "the findings queue is host-side bookkeeping under out/, read and rewritten in place",
        ),
        (
            "fleet_export.rs",
            2,
            "`colonizer fleet export` runs with no mothership and reads the local working copy",
        ),
        (
            "fleet_sync.rs",
            1,
            "the fleet drain reads the local working copy's logs without following links",
        ),
        (
            "gateway_audit.rs",
            1,
            "gateway.jsonl is appended from a synchronous Drop on the request path",
        ),
        (
            "github.rs",
            3,
            "the publish staging: out/pr.md, vm/ policy, a temporary git index, pr-body.md for gh",
        ),
        (
            "handoff.rs",
            1,
            "the VM-written transcripts/ directory, read for a handoff export",
        ),
        (
            "lifecycle.rs",
            2,
            "vm/ leftovers after a stop, and the colony's disk measurement",
        ),
        (
            "loops/pr_labels.rs",
            1,
            "the VM-written out/pr-labels, read for the run's pull request labels",
        ),
        ("maps.rs", 1, "the VM-written out/architecture.json"),
        ("publish.rs", 1, "the VM-written out/pr.md"),
        ("reclaim.rs", 2, "disk measurement of the working copy"),
        (
            "redteam.rs",
            3,
            "VM-written out/ reports, and a ledger's host path named in a brief",
        ),
        (
            "server.rs",
            2,
            "creates the working copy's sessions/ and names the index path in startup alerts",
        ),
        ("snapshot.rs", 1, "microVM memory snapshots, written by msb"),
        (
            "switch_agent.rs",
            1,
            "the agent's native transcripts/, read and written by txcript as a directory",
        ),
        (
            "transcript.rs",
            1,
            "the agent's native transcripts/, read by txcript as a directory",
        ),
        ("validation.rs", 1, "review.md handed to `gh --body-file` by path"),
        ("verify.rs", 1, "the verify command's working directory"),
        (
            "sessions/files.rs",
            4,
            "VM-written out/ artifacts, read with the no-follow regular-file reader",
        ),
        ("sessions/launch.rs", 1, "creates vm/ and out/ before the microVM mounts them"),
        (
            "sessions/persist.rs",
            3,
            "App::session_dir and App::sessions_file themselves, and the runtime's out/pr.md mark",
        ),
    ];

    /// The source with `#[cfg(test)]` items cut out, so the check reads what ships. An item is cut
    /// from its attribute to the `}` at the attribute's own indentation, or to its `;` when it is
    /// one line (`#[cfg(test)] use …;`).
    fn shipped(source: &str) -> Vec<&str> {
        let lines: Vec<&str> = source.lines().collect();
        let mut kept = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim_start();
            if !trimmed.starts_with("#[cfg(test)]") {
                kept.push(line);
                i += 1;
                continue;
            }
            let indent = &line[..line.len() - trimmed.len()];
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_start().starts_with("#[") {
                j += 1;
            }
            if j < lines.len() && lines[j].trim_end().ends_with(';') {
                i = j + 1;
                continue;
            }
            let close = format!("{indent}}}");
            while j < lines.len() && lines[j] != close {
                j += 1;
            }
            i = j + 1;
        }
        kept
    }

    /// Every `.rs` file under `dir`, relative and `/`-separated, unit-test files (`tests.rs`) aside.
    fn sources(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                sources(&entry.path(), &rel, out);
            } else if name.ends_with(".rs") && name != "tests.rs" {
                out.push((rel, entry.path()));
            }
        }
    }

    /// The static half of issue #610: outside this module, nothing builds a path into the session
    /// layout by hand (`data_dir.join("sessions…")`), and the layout's accessors — `App::session_dir`,
    /// `local_session_dir`, `local_index`, `local_sessions_root` — appear only where
    /// `LAYOUT_ALLOWLIST` says why, as many times as it says.
    #[test]
    fn the_session_layout_is_touched_only_through_the_store() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&src, "", &mut files);
        let mut found: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_hand = Vec::new();
        let accessors = [".session_dir(", "local_session_dir(", "local_index(", "local_sessions_root("];
        for (rel, path) in &files {
            if rel == "store.rs" {
                continue;
            }
            let source = std::fs::read_to_string(path).unwrap();
            for line in shipped(&source) {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("data_dir.join(\"sessions") {
                    by_hand.push(format!("{rel}: {code}"));
                }
                let hits: usize = accessors.iter().map(|a| code.matches(a).count()).sum();
                if hits > 0 {
                    *found.entry(rel.clone()).or_default() += hits;
                }
            }
        }
        assert!(
            by_hand.is_empty(),
            "session paths built by hand, outside the store: {by_hand:#?}"
        );
        let allowed: BTreeMap<String, usize> = LAYOUT_ALLOWLIST.iter().map(|(f, n, _)| (f.to_string(), *n)).collect();
        assert_eq!(
            found, allowed,
            "a session path is touched outside the store: route the read or write through `App::store()`, or \
             list the file in LAYOUT_ALLOWLIST with what the store API cannot carry there"
        );
    }
}
