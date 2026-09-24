//! The session storage interface ([`SessionStore`]) and its two reference backends: the default
//! [`LocalDirStore`], byte for byte today's on-disk layout, and [`MemoryObjectStore`], a stand-in
//! for a remote object store. [`migrate`] copies one store's colonies into another and verifies
//! the copy. Issue #325's first slice: same semantics, swappable store. The per-operation
//! contract and the migration procedure are in docs/session-store.md.

// Most of this module has no caller in the harness yet: this slice wires only `persist_sessions`
// through `LocalDirStore::write_index`, and the tests drive the rest — later slices of #325.
#![allow(dead_code)]

use crate::util;
use anyhow::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

/// The one file every backend keeps under this exact name: the session index.
const INDEX: &str = "sessions.json";

/// The per-session prefix every file name is resolved under.
const SESSIONS: &str = "sessions";

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
    /// and readers replay it with the `seq` rule (drop repeats, per file).
    fn append<'a>(&'a self, id: &'a str, name: &'a str, line: &'a [u8]) -> StoreFuture<'a, ()>;
    /// The session's files, relative and `/`-separated (`vm/token`), sorted, recursive.
    fn list_files<'a>(&'a self, id: &'a str) -> StoreFuture<'a, Vec<String>>;
    /// Removes a session and everything in it; removing an unknown id is `Ok`.
    fn remove_session<'a>(&'a self, id: &'a str) -> StoreFuture<'a, ()>;
    /// Moves an unusable index aside to `sessions.json.corrupt-<unix-ts>` — a forward-moving
    /// stamp, so an aside from an earlier second is never overwritten — and returns that name;
    /// `None` when there is no index to quarantine.
    fn quarantine_index(&self) -> StoreFuture<'_, Option<String>>;
}

/// A Windows drive prefix (`C:`), refused wherever an id or a name component would go.
fn is_drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Refuses a session id that is not a single plain path component — no `/`, `\`, `..` or NUL,
/// not empty, no drive prefix. The wall that keeps host paths (worktrees among them) out.
fn check_id(id: &str) -> io::Result<()> {
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
fn check_name(name: &str) -> io::Result<()> {
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
        self.root.join(INDEX)
    }

    /// One session's directory, refusing anything that is not a single plain component.
    fn dir(&self, id: &str) -> io::Result<PathBuf> {
        check_id(id)?;
        Ok(self.root.join(SESSIONS).join(id))
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
            let text = std::str::from_utf8(line).map_err(|_| not_utf8(&path))?;
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
                    let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                    if kind.is_dir() {
                        pending.push((dir.join(relative.clone()), relative));
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
#[derive(Default)]
pub(crate) struct MemoryObjectStore {
    objects: std::sync::Mutex<BTreeMap<String, Vec<u8>>>,
}

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
            let mut objects = self.lock();
            let object = objects.entry(key).or_default();
            object.extend_from_slice(line);
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
}

/// The unix-seconds stamp every quarantine appends — the same shape the mothership's startup
/// move-aside uses. One-second resolution: an aside from an earlier second is never overwritten.
fn quarantine_stamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Copies every colony from `src` to `dst` and verifies the copy, leaving `src` untouched: a
/// migration only ever reads its source, so rollback is pointing the harness back at it. The
/// destination must be empty. Files are copied and verified first, and the index is written
/// last, so every failure before the final index-bytes check leaves `dst` index-less — visibly
/// unfinished, not half-written. With `dry_run`, nothing is written.
pub(crate) async fn migrate(src: &dyn SessionStore, dst: &dyn SessionStore, dry_run: bool) -> io::Result<MigrationReport> {
    // Refused before anything is written: a destination that already holds colonies would make
    // the switch resurrect someone else's records beside the copied ones.
    if dst.read_index().await?.is_some() || !dst.list_sessions().await?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the destination already holds a session index or sessions; migrate into an empty store",
        ));
    }
    let mut report = MigrationReport::default();
    let sessions = src.list_sessions().await?;
    for id in &sessions {
        for name in src.list_files(id).await? {
            let Some(bytes) = src.read_file(id, &name).await? else {
                continue;
            };
            report.files += 1;
            report.bytes += bytes.len() as u64;
            if !dry_run {
                dst.write_file(id, &name, &bytes).await?;
            }
        }
        report.sessions += 1;
    }
    let Some(index) = src.read_index().await? else {
        return Ok(report);
    };
    report.bytes += index.len() as u64;
    if dry_run {
        return Ok(report);
    }
    // The copy proves itself before the index exists: a failed verification leaves dst with
    // orphaned files but no index, which every reader treats as a first-run store.
    verify_copy(src, dst, &sessions).await?;
    // The index goes last: it is what turns the copy into a store worth switching to.
    dst.write_index(&index).await?;
    if dst.read_index().await?.as_deref() != Some(index.as_slice()) {
        let why = "verification failed: the destination's index does not match the source's";
        return Err(io::Error::other(why));
    }
    Ok(report)
}

/// What one migration moved (or would move): colonies, files, bytes — the index's bytes too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    pub sessions: usize,
    pub files: usize,
    pub bytes: u64,
}

/// Reads the copied files back out of `dst` and refuses to call the copy done unless the
/// listings match and every byte matches. (The index is verified in `migrate` itself, after it
/// is written.)
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
            let want = src.read_file(id, &name).await?;
            if dst.read_file(id, &name).await? != want {
                let why = format!("verification failed: colony {id}'s {name} does not match");
                return Err(io::Error::other(why));
            }
        }
    }
    Ok(())
}

/// The store speaks `io::Result`; the fs helpers speak `anyhow`. A bare `io::Error` (an injected
/// fault, say) keeps its kind; anything wrapped in context is flattened into the message, because
/// callers report these errors with `{e:#}` — the text they saw before this module.
fn into_io_error(e: Error) -> io::Error {
    match e.downcast::<io::Error>() {
        Ok(io) => io,
        Err(e) => io::Error::other(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::faults::{Op, inject};
    use crate::{sessions::Session, util};
    use serde_json::Value;
    use std::io::ErrorKind;

    /// A throwaway store root, as in sessions.rs.
    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("colonizer-store-{tag}-{}", util::short_id()))
    }

    fn cleanup(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    fn event(seq: u64) -> Vec<u8> {
        format!(r#"{{"seq":{seq},"type":"progress","message":"step {seq}"}}"#).into_bytes()
    }

    fn lines(events: &[Vec<u8>]) -> Vec<u8> {
        events.iter().flat_map(|e| [e.as_slice(), b"\n"].concat()).collect()
    }

    /// `write_file` + `unwrap`, so seeds read as one line per file.
    async fn put(s: &dyn SessionStore, id: &str, name: &str, bytes: &[u8]) {
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
    async fn contract(s: &dyn SessionStore, label: &str) {
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
        let listed = files(s, "a").await;
        assert_eq!(listed, ["events.jsonl", "harness.jsonl", "vm/token"], "{label}: sorted");
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
    }

    /// A host path — the shape every worktree has — and every traversal spelling, refused wherever
    /// an id or a file name is taken, with nothing written anywhere.
    async fn refuses_host_paths(s: &dyn SessionStore, label: &str) {
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
    async fn seed(s: &dyn SessionStore) -> Vec<String> {
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
    async fn seeded_volume(s: &dyn SessionStore) -> u64 {
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

    #[tokio::test]
    async fn a_migration_refuses_a_destination_that_already_holds_colonies() {
        let source_root = temp_root("mig-full-src");
        let source = LocalDirStore::new(&source_root);
        let ids = seed(&source).await;

        // A destination in use: its own colony and its own index.
        let target_root = temp_root("mig-full-dst");
        let target = LocalDirStore::new(&target_root);
        let theirs = seed(&target).await;

        let err = migrate(&source, &target, false).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");
        assert!(err.to_string().contains("migrate into an empty store"), "{err}");

        // Nothing moved, in either direction: both stores still hold exactly what they held.
        let index = target.read_index().await.unwrap().unwrap();
        assert_eq!(index, br#"[{"id":"a1b2c3d4"},{"id":"e5f60718"}]"#);
        for id in &theirs {
            for name in target.list_files(id).await.unwrap() {
                let got = target.read_file(id, &name).await.unwrap();
                assert_eq!(got, source.read_file(id, &name).await.unwrap(), "{id}/{name}");
            }
        }
        for id in &ids {
            assert_eq!(
                source.list_files(id).await.unwrap().len(),
                if id == "a1b2c3d4" { 3 } else { 2 }
            );
        }
        cleanup(&source_root);
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
}
