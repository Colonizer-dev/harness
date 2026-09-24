//! The disk layer under the answer cache, the conditional-request cache and the avatar cache
//! (`<data_dir>/cache/{answers,http,img}`). One file per key, named by the key's sha256, so a
//! restarted mothership serves what it last knew at once and refreshes behind it instead of
//! rescanning every workspace before it can answer.
//!
//! Every file is written to a temporary name and renamed into place, so a crash leaves either the
//! old entry or the new one. A file that does not parse, or that holds another key (a hash
//! collision, a half-copied cache), is deleted and reads as a miss. Reads touch the file's mtime;
//! when the directory grows past its cap the least recently used files go first, down to 90% of it.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// The answer cache's cap: roughly 200 MB of JSON.
pub const ANSWERS_MAX_BYTES: u64 = 200 * 1024 * 1024;
/// Response bodies kept for conditional requests (ETag / Last-Modified).
pub const HTTP_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// Proxied avatars.
pub const IMG_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// A response body larger than this is not kept for a conditional re-request: the npm packument of
/// a package with thousands of versions is not worth a disk slot to save one download.
pub const HTTP_MAX_BODY: usize = 2 * 1024 * 1024;

/// Milliseconds since the epoch, the timestamps the entries carry.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The file name a key is stored under: its sha256, in hex.
pub fn key_hash(key: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, key.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One stored answer or response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiskEntry {
    /// The full key, checked on load so a collision or a stray file never answers for another key.
    pub key: String,
    /// When the value was computed (the computation's start), in ms since the epoch.
    pub fetched_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// The commit the value was computed at, for answers keyed by a repository's sha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub value: Value,
}

impl DiskEntry {
    pub fn new(key: impl Into<String>, value: Value) -> Self {
        DiskEntry {
            key: key.into(),
            fetched_at: now_ms(),
            etag: None,
            last_modified: None,
            sha: None,
            value,
        }
    }
}

/// A directory of cache files with an LRU cap.
pub struct DiskCache {
    dir: PathBuf,
    max_bytes: u64,
    /// The directory's size as last measured plus what was written since; `None` until the first
    /// write measures it.
    size: Mutex<Option<u64>>,
}

impl DiskCache {
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64) -> Self {
        DiskCache {
            dir: dir.into(),
            max_bytes,
            size: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(key_hash(key))
    }

    /// The raw bytes stored for `key`, touching the file so eviction sees it as recently used.
    pub fn load_bytes(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.path(key);
        let bytes = std::fs::read(&path).ok()?;
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    /// The JSON entry stored for `key`; a corrupt file or one holding another key is removed.
    pub fn load(&self, key: &str) -> Option<DiskEntry> {
        let bytes = self.load_bytes(key)?;
        match serde_json::from_slice::<DiskEntry>(&bytes) {
            Ok(entry) if entry.key == key => Some(entry),
            _ => {
                self.remove(key);
                None
            }
        }
    }

    /// Writes `bytes` for `key` atomically (temporary file, then rename), then evicts if over the cap.
    pub fn store_bytes(&self, key: &str, bytes: &[u8]) -> Result<()> {
        std::fs::create_dir_all(&self.dir).with_context(|| format!("could not create {}", self.dir.display()))?;
        let path = self.path(key);
        let tmp = self.dir.join(format!(".{}.{}.tmp", key_hash(key), crate::util::short_id()));
        std::fs::write(&tmp, bytes).with_context(|| format!("could not write {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("could not move the cache entry to {}", path.display()));
        }
        let over = {
            let mut size = self.size.lock().unwrap_or_else(|p| p.into_inner());
            let total = match *size {
                Some(s) => s + bytes.len() as u64,
                None => self.measure(),
            };
            *size = Some(total);
            total > self.max_bytes
        };
        if over {
            self.evict();
        }
        Ok(())
    }

    pub fn store(&self, entry: &DiskEntry) -> Result<()> {
        self.store_bytes(&entry.key, &serde_json::to_vec(entry)?)
    }

    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.path(key));
    }

    /// Every cache file with its size and last use.
    fn files(&self) -> Vec<(SystemTime, u64, PathBuf)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                meta.is_file()
                    .then(|| (meta.modified().unwrap_or(UNIX_EPOCH), meta.len(), e.path()))
            })
            .collect()
    }

    fn measure(&self) -> u64 {
        self.files().iter().map(|(_, len, _)| len).sum()
    }

    /// Deletes least-recently-used files until the directory is under 90% of its cap. Temporary
    /// files an interrupted write left behind go too, once they are an hour old.
    pub fn evict(&self) {
        let mut files = self.files();
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        files.retain(|(at, _, path)| {
            let tmp = path.extension().is_some_and(|x| x == "tmp");
            if tmp && *at < hour_ago {
                let _ = std::fs::remove_file(path);
            }
            !tmp
        });
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        let target = self.max_bytes / 10 * 9;
        if total > self.max_bytes {
            files.sort_by_key(|(at, ..)| *at);
            for (_, len, path) in files {
                if total <= target {
                    break;
                }
                if std::fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(len);
                }
            }
        }
        *self.size.lock().unwrap_or_else(|p| p.into_inner()) = Some(total);
    }
}

// --- conditional requests -------------------------------------------------------------------------

/// One HTTP response as `gh api -i` prints it: the status line, the headers, a blank line, the body.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: String,
}

/// Reads `gh api -i` output. `gh` prints the head even for a 304 (and then exits 1, with
/// `gh: HTTP 304` on stderr), so the caller parses stdout whatever the exit status. Only the first
/// response is read: the helpers never combine `-i` with `--paginate`.
pub fn parse_gh_include(out: &str) -> Option<HttpResponse> {
    let mut lines = out.split_inclusive('\n');
    let status_line = lines.next()?.trim_end();
    let mut parts = status_line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;
    let mut consumed = status_line.len() + 1;
    let (mut etag, mut last_modified) = (None, None);
    for line in lines {
        consumed += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("etag") {
                etag = Some(value);
            } else if name.eq_ignore_ascii_case("last-modified") {
                last_modified = Some(value);
            }
        }
    }
    let body = out.get(consumed.min(out.len())..).unwrap_or_default().to_string();
    Some(HttpResponse {
        status,
        etag,
        last_modified,
        body,
    })
}

/// The `If-None-Match` / `If-Modified-Since` headers a stored response earns its re-request.
pub fn conditional_headers(entry: &DiskEntry) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(etag) = &entry.etag {
        out.push(("If-None-Match", etag.clone()));
    }
    if let Some(lm) = &entry.last_modified {
        out.push(("If-Modified-Since", lm.clone()));
    }
    out
}

/// What a conditional re-request came to: the stored body again (304), or a new one to store.
/// Anything else is an error for the caller, who keeps whatever it had.
pub fn settle(key: &str, stored: Option<DiskEntry>, res: HttpResponse) -> Result<(String, Option<DiskEntry>)> {
    match res.status {
        304 => match stored {
            Some(mut entry) => {
                entry.fetched_at = now_ms();
                let body = entry.value.as_str().unwrap_or_default().to_string();
                Ok((body, Some(entry)))
            }
            None => bail!("304 Not Modified without a stored response"),
        },
        200..=299 => {
            let keep =
                res.status == 200 && res.body.len() <= HTTP_MAX_BODY && (res.etag.is_some() || res.last_modified.is_some());
            let entry = keep.then(|| DiskEntry {
                key: key.to_string(),
                fetched_at: now_ms(),
                etag: res.etag.clone(),
                last_modified: res.last_modified.clone(),
                sha: None,
                value: Value::String(res.body.clone()),
            });
            Ok((res.body, entry))
        }
        status => bail!("HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("colonizer-cache-{name}-{}", crate::util::short_id()))
    }

    #[test]
    fn an_entry_round_trips_through_the_disk() {
        let dir = temp("roundtrip");
        let cache = DiskCache::new(&dir, 1 << 20);
        let mut entry = DiskEntry::new("deps-supply:acme", json!({"risks": [1, 2]}));
        entry.etag = Some("\"abc\"".into());
        entry.sha = Some("deadbeef".into());
        cache.store(&entry).unwrap();
        assert_eq!(cache.load("deps-supply:acme"), Some(entry.clone()));
        // A fresh handle on the same directory (a restarted mothership) reads it too.
        assert_eq!(DiskCache::new(&dir, 1 << 20).load("deps-supply:acme"), Some(entry));
        assert_eq!(cache.load("deps-supply:other"), None);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![key_hash("deps-supply:acme")], "no temporary file is left behind");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupt_or_foreign_file_reads_as_a_miss_and_is_removed() {
        let dir = temp("corrupt");
        let cache = DiskCache::new(&dir, 1 << 20);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(key_hash("a")), b"{not json").unwrap();
        assert_eq!(cache.load("a"), None);
        assert!(!dir.join(key_hash("a")).exists());
        // A valid entry under the wrong name answers for nobody.
        let other = serde_json::to_vec(&DiskEntry::new("b", json!(1))).unwrap();
        std::fs::write(dir.join(key_hash("a")), other).unwrap();
        assert_eq!(cache.load("a"), None);
        assert!(!dir.join(key_hash("a")).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn eviction_drops_the_least_recently_used_files_first() {
        let dir = temp("evict");
        // Each entry is ~1 KB; the cap holds about three.
        let cache = DiskCache::new(&dir, 3500);
        let blob = "x".repeat(1000);
        for (i, key) in ["one", "two", "three"].iter().enumerate() {
            cache.store(&DiskEntry::new(*key, json!(blob))).unwrap();
            let past = SystemTime::now() - Duration::from_secs(100 - i as u64 * 10);
            std::fs::File::options()
                .write(true)
                .open(dir.join(key_hash(key)))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
        // Reading "one" makes it the most recently used.
        assert!(cache.load("one").is_some());
        cache.store(&DiskEntry::new("four", json!(blob))).unwrap();
        assert!(cache.load("two").is_none(), "the least recently used goes first");
        assert!(cache.load("one").is_some() && cache.load("four").is_some());
        assert!(cache.measure() <= 3500);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gh_include_output_parses_for_a_200_and_a_304() {
        let ok = "HTTP/2.0 200 OK\nContent-Type: application/json\nEtag: W/\"abc\"\nLast-Modified: Tue, 01 Sep 2026 10:00:00 GMT\r\n\r\n{\"a\":1}";
        let res = parse_gh_include(ok).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(res.last_modified.as_deref(), Some("Tue, 01 Sep 2026 10:00:00 GMT"));
        assert_eq!(res.body, "{\"a\":1}");
        let not_modified = parse_gh_include("HTTP/2.0 304 Not Modified\nEtag: \"abc\"\n\n").unwrap();
        assert_eq!(not_modified.status, 304);
        assert_eq!(not_modified.body, "");
        assert_eq!(parse_gh_include("gh: HTTP 304\n"), None);
        assert_eq!(parse_gh_include(""), None);
    }

    #[test]
    fn a_304_reuses_the_stored_body_and_a_200_replaces_it() {
        let res = |status: u16, body: &str, etag: Option<&str>| HttpResponse {
            status,
            etag: etag.map(str::to_string),
            last_modified: None,
            body: body.into(),
        };
        let (body, entry) = settle("GET repos/a/b", None, res(200, "{\"v\":1}", Some("\"e1\""))).unwrap();
        assert_eq!(body, "{\"v\":1}");
        let entry = entry.expect("a 200 with an ETag is kept");
        assert_eq!(conditional_headers(&entry), vec![("If-None-Match", "\"e1\"".to_string())]);
        let (again, kept) = settle("GET repos/a/b", Some(entry.clone()), res(304, "", None)).unwrap();
        assert_eq!(again, "{\"v\":1}");
        assert_eq!(kept.unwrap().etag, entry.etag);
        assert!(settle("GET repos/a/b", None, res(304, "", None)).is_err());
        let (_, none) = settle("k", None, res(200, "{}", None)).unwrap();
        assert!(none.is_none(), "nothing to revalidate with, nothing kept");
        let (_, pending) = settle("k", None, res(202, "", Some("\"x\""))).unwrap();
        assert!(pending.is_none(), "a 202 (statistics still computing) is never kept");
        assert!(settle("k", Some(entry), res(502, "", None)).is_err());
    }
}
