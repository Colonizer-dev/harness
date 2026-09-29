//! Fleet export and import (issue #687): a machine's stats, logs and colony history travel to
//! another fleet member as one zstd-compressed tar. The bundle is built from an explicit
//! allowlist of paths — the config dir is never read by the library here, and nothing secret
//! bearing is ever staged — and the transfer is chunked and resumable: the receiver persists a
//! cursor after every chunk, so a crashed or cancelled import continues where it stopped, and a
//! re-sent chunk is a no-op rather than a duplicate.
//!
//! The CLI half is `colonizer fleet export` / `colonizer fleet import`, both of which run
//! locally off `Settings::from_env()` — no mothership needed.

use crate::sessions::{Session, SessionStatus};
use crate::{Settings, util};
use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _, Write as _};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

/// What a bundle calls itself; import refuses anything else.
pub const BUNDLE_FORMAT: &str = "colonizer-fleet-export";
/// The only bundle version this build writes. Import accepts this and anything older, refuses
/// anything newer (written by a build that knows fields this one would drop).
pub const BUNDLE_VERSION: u32 = 1;
/// Raw bytes per chunk on the wire; each chunk is one independent zstd frame.
pub const DEFAULT_CHUNK_SIZE: usize = 512 * 1024;
/// A chunk may never decompress to more than this: one chunk is a slice of a single log, history
/// or stats file, never a zip bomb's worth.
const MAX_CHUNK_RAW: u64 = 16 * 1024 * 1024;

const MANIFEST_NAME: &str = "manifest.json";
const HISTORY_FILE: &str = "history/sessions.jsonl";
/// The log ledgers a session directory may contribute, by exact name (`sessions/runtime.rs`).
const LOG_BASENAMES: [&str; 3] = ["events.jsonl", "harness.jsonl", "gateway.jsonl"];
/// The data-dir files the stats category carries. Nothing else under the data dir is ever read.
const STAT_FILES: [&str; 5] = [
    "routing.jsonl",
    "spend.jsonl",
    "activity.jsonl",
    "provider-usage.json",
    "provider-quota.json",
];

// ── Join hook (#686) ──
// Nothing here talks to a mothership; these are the seams #686's join flow calls, in order. The
// join dialog calls `preview` (or `preview_for`) and shows the member what would leave — nothing
// is read for the wire before that. Once the member confirms and picks `Categories`, the member
// streams `chunks_for` chunk by chunk and the fleet side commits each with `apply_chunk` (or a
// whole bundle moves at once with `import_bundle`). A cancelled or crashed transfer leaves a
// valid partial import — the cursor is persisted after every chunk — and the next attempt
// resumes at `ImportCursor::offset`.

// ── Origins ──

/// Where an export came from: the machine's stable host id and hostname, exactly the pair fleet
/// peers already show each other ("archlinux (f8832acd-…)", `fleet.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub host: String,
    pub name: String,
}

impl Origin {
    /// This machine's origin for the CLI, from settings: the persisted `<config_dir>/host_id`
    /// (created on first use, exactly what `runtime::host_id` does), so a bundle's `origin_host`
    /// matches the id the fleet claims carry. That id is not a secret — peers already display it
    /// — and it is the only config-dir file the export path ever touches; the library functions
    /// below read no config dir at all. A host id that cannot be persisted is an error: a new one
    /// minted per run would make every export look like a different machine.
    pub fn for_config(config_dir: &Path) -> std::io::Result<Origin> {
        let host = match util::read_trimmed(&config_dir.join("host_id")) {
            Some(id) if is_safe_segment(&id) => id,
            _ => {
                let id = uuid::Uuid::new_v4().to_string();
                std::fs::create_dir_all(config_dir)?;
                util::write_private(&config_dir.join("host_id"), id.as_bytes())
                    .map_err(|e| std::io::Error::other(format!("persisting host_id: {e:#}")))?;
                id
            }
        };
        Ok(Origin { host, name: host_name() })
    }

    /// The machine's origin read without ever writing to the config dir: the persisted host id
    /// when one exists, else the hostname. What [`preview`] and [`export_bundle`] resolve when
    /// the caller holds only a data dir.
    #[allow(dead_code)] // The join hook (#686) drives this seam; the unit tests exercise it until then.
    pub fn for_machine() -> Origin {
        let name = host_name();
        let host = util::read_trimmed(&default_config_dir().join("host_id"))
            .filter(|id| is_safe_segment(id))
            .unwrap_or_else(|| name.clone());
        Origin { host, name }
    }
}

fn default_config_dir() -> PathBuf {
    match util::env_nonempty("COLONIZER_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/colonizer"),
    }
}

/// The hostname, `runtime::probe_hostname`'s sources without the async: `/proc` where it exists,
/// the `hostname` command elsewhere, `unknown` when neither answers.
fn host_name() -> String {
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// One path segment safe to join under a directory we own: non-empty ASCII letters, digits,
/// `-`, `_` and `.`, never a leading dot (no `..`, nothing hidden). Host ids, session ids and
/// every tar entry segment are checked with this before anything is opened.
fn is_safe_segment(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

// ── Categories, manifest and the history projection ──

/// Which parts of a machine's data an export carries. All on by default; the join dialog (#686)
/// turns off whatever the member declines before anything is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Categories {
    pub history: bool,
    pub logs: bool,
    pub stats: bool,
}

impl Default for Categories {
    fn default() -> Self {
        Categories {
            history: true,
            logs: true,
            stats: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    History,
    Logs,
    Stats,
}

impl Category {
    fn name(self) -> &'static str {
        match self {
            Category::History => "history",
            Category::Logs => "logs",
            Category::Stats => "stats",
        }
    }
}

/// What a bundle holds: computed without writing anything by [`preview`], written as the
/// bundle's first entry by [`export_bundle`], and checked entry by entry on import.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub origin_host: String,
    pub origin_name: String,
    pub created_at: DateTime<Utc>,
    pub categories: ManifestCategories,
    pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestCategories {
    pub history: CategoryStats,
    pub logs: CategoryStats,
    pub stats: CategoryStats,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CategoryStats {
    pub included: bool,
    /// Sessions for history, files for logs and stats.
    pub count: u64,
    /// Min `created_at` / max `updated_at` of the exported sessions; null for stats and when
    /// nothing was exported.
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub category: String,
    pub bytes: u64,
    pub sha256: String,
}

/// The colony record that travels, an allowlist projection of [`Session`]: the launch record and
/// its outcome, never the fields that name this machine's paths or carry secrets — no
/// `launched_by_token`, `instructions`, `worktree`, `sandbox`, `mesh`, `agent_session`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportedSession {
    /// `<origin_host>:<original_id>` — two machines' `session-abc` stay distinct records.
    pub id: String,
    pub origin_host: String,
    pub original_id: String,
    pub repo: String,
    pub org: String,
    pub issue: Option<u64>,
    pub issue_title: String,
    pub status: SessionStatus,
    pub branch: String,
    pub base: Option<String>,
    pub pr_url: Option<String>,
    pub pr_opened_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub routed_cost_usd: Option<f64>,
    pub model_tier: Option<String>,
    pub model_usage: Option<Value>,
    pub model_routing: Option<Value>,
    pub agent: String,
    pub boot_timing: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ImportedSession {
    fn of(origin: &Origin, s: &Session) -> ImportedSession {
        ImportedSession {
            id: format!("{}:{}", origin.host, s.id),
            origin_host: origin.host.clone(),
            original_id: s.id.clone(),
            repo: s.repo.clone(),
            org: s.org.clone(),
            issue: s.issue,
            issue_title: s.issue_title.clone(),
            status: s.status,
            branch: s.branch.clone(),
            base: s.base.clone(),
            pr_url: s.pr_url.clone(),
            pr_opened_at: s.pr_opened_at,
            merged_at: s.merged_at,
            summary: s.summary.clone(),
            error: s.error.clone(),
            cost_usd: s.cost_usd,
            routed_cost_usd: s.routed_cost_usd,
            model_tier: s.model_tier.clone(),
            model_usage: s.model_usage.clone(),
            model_routing: s.model_routing.clone(),
            agent: s.agent.clone(),
            boot_timing: s.boot_timing.clone(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

// ── Export ──

/// One file staged for the bundle. Bytes are held so the manifest's hashes describe exactly what
/// the tar will carry, even where a source file grows between the hash pass and the write.
struct Staged {
    path: String,
    category: Category,
    bytes: Vec<u8>,
}

/// Computes the manifest — counts, time range, bytes per category — reading only the allowlisted
/// paths, and writes nothing.
#[allow(dead_code)] // The join hook (#686) drives this seam; the unit tests exercise it until then.
pub fn preview(data_dir: &Path, cats: &Categories) -> anyhow::Result<Manifest> {
    preview_for(data_dir, &Origin::for_machine(), cats)
}

/// [`preview`] with the origin named, for a caller that already holds it (the CLI, the tests).
pub fn preview_for(data_dir: &Path, origin: &Origin, cats: &Categories) -> anyhow::Result<Manifest> {
    Ok(collect(data_dir, origin, cats)?.0)
}

/// Writes the bundle to `out` (a zstd-compressed tar whose first entry is `manifest.json`) and
/// returns the same manifest [`preview`] computed.
#[allow(dead_code)] // The join hook (#686) drives this seam; the unit tests exercise it until then.
pub fn export_bundle(data_dir: &Path, cats: &Categories, out: &Path) -> anyhow::Result<Manifest> {
    export_bundle_for(data_dir, &Origin::for_machine(), cats, out)
}

/// [`export_bundle`] with the origin named.
pub fn export_bundle_for(data_dir: &Path, origin: &Origin, cats: &Categories, out: &Path) -> anyhow::Result<Manifest> {
    let (manifest, staged) = collect(data_dir, origin, cats)?;
    // A temp name beside the target, renamed into place: no reader ever sees a half-written bundle.
    let tmp = out.with_file_name(format!(
        ".{}.{}.tmp",
        out.file_name().and_then(|n| n.to_str()).unwrap_or("bundle"),
        util::short_id()
    ));
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = out.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::File::create(&tmp)?;
        let mut zst = zstd::Encoder::new(file, 3)?;
        {
            let mut tar = tar::Builder::new(&mut zst);
            append_entry(&mut tar, MANIFEST_NAME, &serde_json::to_vec_pretty(&manifest)?)?;
            for file in &staged {
                append_entry(&mut tar, &file.path, &file.bytes)?;
            }
            tar.finish()?;
        }
        zst.finish()?.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, out)?;
    Ok(manifest)
}

fn append_entry<R: std::io::Write>(tar: &mut tar::Builder<R>, path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(Utc::now().timestamp() as u64);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)
}

/// Reads only the allowlisted source paths and stages their bytes. The session set always comes
/// from `sessions.json` — it drives the logs category too, so `--no-history` still carries logs.
fn collect(data_dir: &Path, origin: &Origin, cats: &Categories) -> anyhow::Result<(Manifest, Vec<Staged>)> {
    let sessions = read_sessions(data_dir)?;
    let mut staged: Vec<Staged> = Vec::new();

    if cats.history {
        let mut lines = String::new();
        for s in &sessions {
            lines.push_str(&serde_json::to_string(&ImportedSession::of(origin, s))?);
            lines.push('\n');
        }
        staged.push(Staged {
            path: HISTORY_FILE.into(),
            category: Category::History,
            bytes: lines.into_bytes(),
        });
    }
    if cats.logs {
        for s in &sessions {
            collect_session_logs(data_dir, s, &mut staged)?;
        }
    }
    if cats.stats {
        for name in STAT_FILES {
            match std::fs::read(data_dir.join(name)) {
                Ok(bytes) => staged.push(Staged {
                    path: format!("stats/{name}"),
                    category: Category::Stats,
                    bytes,
                }),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        staged.push(Staged {
            path: "stats/summary.json".into(),
            category: Category::Stats,
            bytes: serde_json::to_vec(&summary(&sessions))?,
        });
    }

    let (from, to) = range_of(&sessions);
    let entry = |file: &Staged| FileEntry {
        path: file.path.clone(),
        category: file.category.name().into(),
        bytes: file.bytes.len() as u64,
        sha256: sha256_hex(&file.bytes),
    };
    let bytes_of = |c: Category| {
        staged
            .iter()
            .filter(|f| f.category == c)
            .map(|f| f.bytes.len() as u64)
            .sum::<u64>()
    };
    // The session span belongs to history and logs; the stats files carry no session times.
    let stats_for = |included: bool, c: Category, span: bool| CategoryStats {
        included,
        count: if included {
            staged.iter().filter(|f| f.category == c).count() as u64
        } else {
            0
        },
        from: if included && span { from } else { None },
        to: if included && span { to } else { None },
        bytes: if included { bytes_of(c) } else { 0 },
    };
    let manifest = Manifest {
        format: BUNDLE_FORMAT.into(),
        version: BUNDLE_VERSION,
        origin_host: origin.host.clone(),
        origin_name: origin.name.clone(),
        created_at: Utc::now(),
        categories: ManifestCategories {
            history: stats_for(cats.history, Category::History, true),
            logs: stats_for(cats.logs, Category::Logs, true),
            stats: stats_for(cats.stats, Category::Stats, false),
        },
        files: staged.iter().map(entry).collect(),
    };
    Ok((manifest, staged))
}

/// The exported sessions' span: min `created_at`, max `updated_at`.
fn range_of(sessions: &[Session]) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let from = sessions.iter().map(|s| s.created_at).min();
    let to = sessions.iter().map(|s| s.updated_at).max();
    (from, to)
}

/// The colony records from the data dir's `sessions.json`; missing on a fresh install means none.
fn read_sessions(data_dir: &Path) -> anyhow::Result<Vec<Session>> {
    match std::fs::read(data_dir.join("sessions.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// A session directory's allowlisted files: the three log ledgers by exact name and whatever the
/// transcripts directory holds (regular files only — symlinks are skipped, never followed).
/// Where a live file is gone, the same name comes from the session's newest archive bundle (#496).
fn collect_session_logs(data_dir: &Path, s: &Session, staged: &mut Vec<Staged>) -> anyhow::Result<()> {
    if !is_safe_segment(&s.id) {
        return Ok(());
    }
    let dir = data_dir.join("sessions").join(&s.id);
    let mut taken: Vec<String> = Vec::new();
    for name in LOG_BASENAMES {
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => {
                taken.push(name.to_string());
                staged.push(Staged {
                    path: format!("logs/{}/{}", s.id, name),
                    category: Category::Logs,
                    bytes,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut transcripts = Vec::new();
    walk_regular_files(&dir.join("transcripts"), "", &mut transcripts);
    transcripts.sort();
    for rel in &transcripts {
        match std::fs::read(dir.join("transcripts").join(rel)) {
            Ok(bytes) => {
                taken.push(format!("transcripts/{rel}"));
                staged.push(Staged {
                    path: format!("logs/{}/transcripts/{}", s.id, rel),
                    category: Category::Logs,
                    bytes,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let missing: Vec<String> = LOG_BASENAMES
        .iter()
        .map(|n| n.to_string())
        .chain(transcripts.iter().map(|rel| format!("transcripts/{rel}")))
        .filter(|name| !taken.contains(name))
        .collect();
    if !missing.is_empty() {
        archive_session_files(data_dir, s, &missing, staged);
    }
    Ok(())
}

/// Every regular file under `dir`, as paths relative to it. Symlinks (and anything not a plain
/// directory or file) are skipped: `file_type` on a `DirEntry` never follows them.
fn walk_regular_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{}", entry.file_name().to_string_lossy())
        };
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk_regular_files(&entry.path(), &rel, out),
            Ok(t) if t.is_file() => out.push(rel),
            _ => {}
        }
    }
}

/// Reads the missing allowlisted names out of a session's newest archive bundle. The archive
/// layout mirrors `archive.rs` (`archive/<org>/<repo>/<yyyy>/<mm>/<id>[.rN].tar.zst`) and the
/// entries inside carry session-dir-relative names, so the same allowlist decides. Minimal on
/// purpose: no index sidecars, no month hunting — the exact month the session record names.
fn archive_session_files(data_dir: &Path, s: &Session, missing: &[String], staged: &mut Vec<Staged>) {
    let Some(bundle) = latest_archive_bundle(data_dir, s) else {
        return;
    };
    let Ok(file) = std::fs::File::open(&bundle) else { return };
    let Ok(zst) = zstd::Decoder::new(file) else { return };
    let mut tar = tar::Archive::new(zst);
    let Ok(entries) = tar.entries() else { return };
    for entry in entries.flatten() {
        let mut entry = entry;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Ok(name) = entry.path().map(|p| p.to_string_lossy().into_owned()) else {
            continue;
        };
        if !missing.contains(&name) {
            continue;
        }
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            staged.push(Staged {
                path: format!("logs/{}/{}", s.id, name),
                category: Category::Logs,
                bytes,
            });
        }
    }
}

fn latest_archive_bundle(data_dir: &Path, s: &Session) -> Option<PathBuf> {
    let repo = s.repo.rsplit('/').next().unwrap_or("repo");
    let dir = data_dir
        .join("archive")
        .join(sanitize_segment(&s.org))
        .join(sanitize_segment(repo))
        .join(format!("{:04}", s.created_at.year()))
        .join(format!("{:02}", s.created_at.month()));
    let mut found = None;
    for revision in 1..64 {
        let stem = if revision == 1 {
            s.id.clone()
        } else {
            format!("{}.r{revision}", s.id)
        };
        let path = dir.join(format!("{stem}.tar.zst"));
        if path.exists() {
            found = Some(path);
        } else {
            break;
        }
    }
    found
}

/// `archive.rs`'s own segment mapping, restated here so the fallback finds the same directories
/// without archive.rs growing a pub(crate) helper for one reader.
fn sanitize_segment(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    out.truncate(64);
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// The computed stats snapshot: fleet-level sums over the colony records, kept small — the
/// per-session detail already travels in the history lines.
fn summary(sessions: &[Session]) -> Value {
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut usage: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let (mut cost, mut routed) = (0.0, 0.0);
    let mut boots: Vec<f64> = Vec::new();
    for s in sessions {
        let status = serde_json::to_value(s.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string));
        if let Some(status) = status {
            *by_status.entry(status).or_default() += 1;
        }
        cost += s.cost_usd.unwrap_or(0.0);
        routed += s.routed_cost_usd.unwrap_or(0.0);
        if let Some(models) = s.model_usage.as_ref().and_then(Value::as_object) {
            for (model, fields) in models {
                let totals = usage.entry(model.clone()).or_default();
                if let Some(fields) = fields.as_object() {
                    for (field, n) in fields {
                        if let Some(n) = n.as_u64() {
                            *totals.entry(field.clone()).or_default() += n;
                        }
                    }
                }
            }
        }
        if let Some(ms) = s.boot_timing.as_ref().and_then(|t| t.get("total_ms")).and_then(Value::as_f64) {
            boots.push(ms);
        }
    }
    json!({
        "sessions": sessions.len(),
        "by_status": by_status,
        "cost_usd": cost,
        "routed_cost_usd": routed,
        "model_usage": usage,
        "boot_ms_mean": if boots.is_empty() { Value::Null } else { json!(boots.iter().sum::<f64>() / boots.len() as f64) },
    })
}

// ── Chunks ──

/// One slice of a bundle file on the wire: the raw bytes `[offset, offset + raw_len)` of the
/// bundle-relative `path`, zstd-compressed in `data` (standard padded base64 on the wire).
/// `total` is the file's full raw size, so the receiver knows when a file is done.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub path: String,
    pub offset: u64,
    pub raw_len: u64,
    pub total: u64,
    #[serde(with = "b64_bytes")]
    pub data: Vec<u8>,
}

/// `Chunk::data` on the wire is standard padded base64 — `util`'s own encoder, so no new crate.
mod b64_bytes {
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::util::b64_encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        crate::util::b64_decode(&s).ok_or_else(|| serde::de::Error::custom("not valid base64"))
    }
}

/// Streams `file` as chunks of `chunk_size` raw bytes starting at `from_offset` — a resumed
/// transfer starts at `cursor.offset(rel)` — each an independent zstd frame. Reading is lazy: a
/// chunk is read and compressed only when the iterator is asked for it.
#[allow(dead_code)] // The join hook (#686) drives this seam; the unit tests exercise it until then.
pub fn chunks_for(file: &Path, rel: &str, from_offset: u64, chunk_size: usize) -> impl Iterator<Item = std::io::Result<Chunk>> {
    let rel_checked = check_entry_path(rel)
        .map(|_| ())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e:#}")));
    let (total, error) = match (rel_checked, std::fs::metadata(file)) {
        (Err(e), _) => (0, Some(e)),
        (_, Err(e)) => (0, Some(e)),
        (Ok(()), Ok(meta)) => (meta.len(), None),
    };
    Chunks {
        file: file.to_path_buf(),
        rel: rel.to_string(),
        offset: from_offset,
        total,
        chunk_size: chunk_size.max(1),
        reader: None,
        error,
    }
}

struct Chunks {
    file: PathBuf,
    rel: String,
    offset: u64,
    total: u64,
    chunk_size: usize,
    reader: Option<std::fs::File>,
    error: Option<std::io::Error>,
}

impl Iterator for Chunks {
    type Item = std::io::Result<Chunk>;

    fn next(&mut self) -> Option<std::io::Result<Chunk>> {
        if let Some(e) = self.error.take() {
            return Some(Err(e));
        }
        if self.offset >= self.total {
            return None;
        }
        if self.reader.is_none() {
            use std::io::Seek as _;
            match std::fs::File::open(&self.file).and_then(|mut f| {
                f.seek(std::io::SeekFrom::Start(self.offset))?;
                Ok(f)
            }) {
                Ok(reader) => self.reader = Some(reader),
                Err(e) => {
                    self.offset = self.total;
                    return Some(Err(e));
                }
            }
        }
        let want = (self.total - self.offset).min(self.chunk_size as u64) as usize;
        let mut raw = vec![0u8; want];
        if let Err(e) = self.reader.as_mut().expect("opened above").read_exact(&mut raw) {
            self.offset = self.total;
            return Some(Err(e));
        }
        self.offset += want as u64;
        Some(zstd::stream::encode_all(&raw[..], 3).map(|data| Chunk {
            path: self.rel.clone(),
            offset: self.offset - want as u64,
            raw_len: want as u64,
            total: self.total,
            data,
        }))
    }
}

// ── Import ──

/// Where one file's import stands. `prefix_sha256` hashes every committed byte: a bundle whose
/// file no longer hashes to it was rewritten upstream (the log rotated or the export was taken
/// from a different point), and that file restarts from zero instead of appending onto bytes
/// that were never its own.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileProgress {
    pub offset: u64,
    pub prefix_sha256: String,
}

/// A per-member import's progress, persisted (atomically) after every chunk at
/// `<data_dir>/fleet-imports/<origin_host>/cursor.json`. Load, resume at [`ImportCursor::offset`].
#[derive(Default, Serialize, Deserialize)]
pub struct ImportCursor {
    pub origin_host: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub files: BTreeMap<String, FileProgress>,
    /// The committed prefix's running sha256 per file, this run only (never serialized): seeded
    /// from disk when a file is first touched, so applying a chunk hashes just that chunk.
    #[serde(skip)]
    digests: BTreeMap<String, ring::digest::Context>,
}

impl std::fmt::Debug for ImportCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportCursor")
            .field("origin_host", &self.origin_host)
            .field("updated_at", &self.updated_at)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

impl ImportCursor {
    /// Bytes of `path` already committed: where a resumed transfer starts.
    pub fn offset(&self, path: &str) -> u64 {
        self.files.get(path).map(|f| f.offset).unwrap_or(0)
    }

    pub fn load(dest_root: &Path) -> ImportCursor {
        std::fs::read(dest_root.join("cursor.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dest_root: &Path) -> std::io::Result<()> {
        atomic_write(&dest_root.join("cursor.json"), &serde_json::to_vec(self)?)
    }
}

/// What [`apply_chunk`] did with a chunk: appended (carrying the new committed offset) or
/// recognized as bytes already committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Appended { committed: u64 },
    Duplicate,
}

/// Commits one chunk under `dest_root` — the member's `fleet-imports/<origin_host>/` — and
/// persists the cursor. This is the resumability contract: a chunk at the committed offset is
/// appended; one entirely below it is a `Duplicate` (a re-send on resume, never written twice);
/// one past it is a gap error, and the sender must back up to `cursor.offset(chunk.path)`.
/// `.json` snapshots assemble under a temp name and are renamed into place only when complete.
pub fn apply_chunk(dest_root: &Path, cursor: &mut ImportCursor, chunk: &Chunk) -> std::io::Result<Applied> {
    check_entry_path(&chunk.path).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e:#}")))?;
    let committed = cursor.offset(&chunk.path);
    if chunk.offset > committed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "gap in {}: chunk at offset {}, {} committed",
                chunk.path, chunk.offset, committed
            ),
        ));
    }
    if chunk.offset < committed {
        if chunk.offset + chunk.raw_len > committed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("chunk in {} overlaps the {} committed bytes", chunk.path, committed),
            ));
        }
        return Ok(Applied::Duplicate);
    }
    if chunk.raw_len > MAX_CHUNK_RAW {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("chunk in {} claims {} raw bytes, over the cap", chunk.path, chunk.raw_len),
        ));
    }
    // Bounded decode: the frame is read through a `Take` one byte past `raw_len`, so a crafted
    // chunk cannot decompress to gigabytes before the length is checked.
    let mut raw = Vec::new();
    zstd::Decoder::new(&chunk.data[..])?
        .take(chunk.raw_len.saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() as u64 != chunk.raw_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "chunk in {} decompressed to {} bytes, manifest says {}",
                chunk.path,
                raw.len(),
                chunk.raw_len
            ),
        ));
    }
    let dest = working_path(dest_root, &chunk.path);
    reject_symlink(dest_root, &dest)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&dest)?;
    // A crash between the append and the cursor save leaves the file longer than the cursor
    // claims: rewrite that tail instead of appending a second copy of it.
    if file.metadata()?.len() > committed {
        file.set_len(committed)?;
    }
    file.seek(std::io::SeekFrom::Start(committed))?;
    file.write_all(&raw)?;
    file.sync_all()?;
    // The committed prefix's hash, kept incrementally: the digest is seeded once from what the
    // file already held when this run first touches it, then only the appended bytes are hashed.
    let prefix = {
        let ctx = cursor.digests.entry(chunk.path.clone()).or_insert_with(|| {
            let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
            let held = std::fs::read(&dest).unwrap_or_default();
            ctx.update(&held[..committed.min(held.len() as u64) as usize]);
            ctx
        });
        ctx.update(&raw);
        util::hex(ctx.clone().finish().as_ref())
    };
    cursor.files.insert(
        chunk.path.clone(),
        FileProgress {
            offset: committed + raw.len() as u64,
            prefix_sha256: prefix,
        },
    );
    cursor.updated_at = Some(Utc::now());
    cursor.save(dest_root)?;
    if is_snapshot(&chunk.path) && cursor.offset(&chunk.path) >= chunk.total {
        rename_snapshot(dest_root, &chunk.path)?;
    }
    Ok(Applied::Appended {
        committed: cursor.offset(&chunk.path),
    })
}

/// `.json` snapshots (provider usage and quota, the summary) are replaced atomically: their
/// bytes assemble under a `.part` name and are renamed into place only when complete. Everything
/// else — the append-only logs and the history lines — lands in its final path.
fn is_snapshot(path: &str) -> bool {
    path.starts_with("stats/") && path.ends_with(".json")
}

fn working_path(dest_root: &Path, path: &str) -> PathBuf {
    let dest = dest_root.join(path);
    if is_snapshot(path) {
        dest.with_file_name(format!(
            "{}.part",
            dest.file_name().and_then(|n| n.to_str()).unwrap_or("snapshot")
        ))
    } else {
        dest
    }
}

/// Puts a completed snapshot in its final place. Idempotent: a resumed import may find the
/// rename already done, or — for a zero-length snapshot — nothing assembled at all.
fn rename_snapshot(dest_root: &Path, path: &str) -> std::io::Result<()> {
    let dest = dest_root.join(path);
    let part = working_path(dest_root, path);
    if part.exists() {
        std::fs::rename(&part, &dest)
    } else if !dest.exists() {
        atomic_write(&dest, b"")
    } else {
        Ok(())
    }
}

/// Progress reported per chunk, before the chunk is applied.
#[derive(Clone, Debug)]
pub struct Progress {
    pub path: String,
    /// The committed offset once this chunk lands.
    pub offset: u64,
    /// The file's total raw size.
    pub total: u64,
    /// Bytes appended so far, this run.
    pub done: u64,
    /// The bundle's total raw bytes across all files.
    pub bytes: u64,
}

/// What an import did. `cancelled: true` is still a valid state, not a failure: the cursor was
/// persisted after every chunk, and the next run resumes where this one stopped.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportReport {
    pub origin_host: String,
    pub origin_name: String,
    /// Records now in the member's fleet-import `sessions.json`.
    pub sessions: usize,
    /// Files this run brought to (or found at) completion.
    pub files: usize,
    /// Bytes appended by this run.
    pub bytes: u64,
    pub cancelled: bool,
}

/// Imports a whole bundle into `data_dir`'s `fleet-imports/`. Every file entry runs through the
/// chunk path starting at the cursor's offset, so a bundle bigger than one message, a crash or a
/// `ControlFlow::Break` from `progress` all leave the same thing behind: a valid partial import
/// that resumes. Each file's sha256 is checked against the manifest before anything is written.
pub fn import_bundle(
    data_dir: &Path,
    bundle: &Path,
    mut progress: impl FnMut(&Progress) -> ControlFlow<()>,
) -> Result<ImportReport> {
    let file = std::fs::File::open(bundle)?;
    let mut tar = tar::Archive::new(zstd::Decoder::new(file)?);
    let mut entries = tar.entries()?;
    let first = entries.next().transpose()?.ok_or_else(|| invalid("empty bundle"))?;
    let manifest = manifest_entry(first)?;
    let total_bytes: u64 = manifest.files.iter().map(|f| f.bytes).sum();
    let dest_root = data_dir.join("fleet-imports").join(&manifest.origin_host);
    std::fs::create_dir_all(&dest_root)?;
    let mut cursor = ImportCursor::load(&dest_root);
    cursor.origin_host = manifest.origin_host.clone();
    let listed: BTreeMap<String, FileEntry> = manifest.files.iter().cloned().map(|f| (f.path.clone(), f)).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut report = ImportReport {
        origin_host: manifest.origin_host.clone(),
        origin_name: manifest.origin_name.clone(),
        sessions: 0,
        files: 0,
        bytes: 0,
        cancelled: false,
    };

    for entry in entries {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err(invalid(&format!(
                "bundle entry {:?} is not a regular file",
                entry.path()?.to_string_lossy()
            )));
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        // Bounded read: the entry is taken one byte past what the manifest lists, so a tampered
        // bundle cannot smuggle gigabytes past a small `bytes` claim.
        let mut bytes = Vec::new();
        let want = listed
            .get(&path)
            .map(|f| f.bytes.saturating_add(1))
            .ok_or_else(|| invalid(&format!("{path} is not listed in the manifest")))?;
        (&mut entry).take(want).read_to_end(&mut bytes)?;
        let category = check_entry_path(&path)?;
        let listed = listed.get(&path).expect("listed, checked above");
        seen.insert(path.clone());
        if listed.bytes != bytes.len() as u64 || listed.sha256 != sha256_hex(&bytes) {
            return Err(invalid(&format!("{path} does not match the manifest")));
        }
        if listed.category != category.name() {
            return Err(invalid(&format!(
                "{path} is listed under {}, which it is not",
                listed.category
            )));
        }

        // Resume where the cursor says — unless this file was rewritten upstream (a different
        // prefix than the committed hash, or a shorter one altogether), which restarts it from
        // zero rather than appending onto bytes that were never its own.
        let mut committed = cursor.offset(&path);
        if committed > 0 {
            let prefix_ok = !shrunk_source(&bytes, committed)
                && cursor
                    .files
                    .get(&path)
                    .is_some_and(|f| f.prefix_sha256 == sha256_hex(&bytes[..committed as usize]));
            if !prefix_ok {
                restart_file(&dest_root, &mut cursor, &path)?;
                committed = 0;
            }
        }
        while committed < bytes.len() as u64 {
            let end = (committed + DEFAULT_CHUNK_SIZE as u64).min(bytes.len() as u64);
            let chunk = Chunk {
                path: path.clone(),
                offset: committed,
                raw_len: end - committed,
                total: bytes.len() as u64,
                data: zstd::stream::encode_all(&bytes[committed as usize..end as usize], 3)?,
            };
            if let ControlFlow::Break(()) = progress(&Progress {
                path: path.clone(),
                offset: end,
                total: bytes.len() as u64,
                done: report.bytes,
                bytes: total_bytes,
            }) {
                report.cancelled = true;
                return Ok(report); // the cursor is already on disk; the manifest stays the last completed import
            }
            committed = match apply_chunk(&dest_root, &mut cursor, &chunk)? {
                Applied::Appended { committed } => committed,
                Applied::Duplicate => end,
            };
        }
        if path == HISTORY_FILE {
            // Every line is on disk (the loop above only ends at completion), so merge: a
            // re-import replaces each record by namespaced id, never duplicating one.
            report.sessions = merge_history(&dest_root, &bytes)?;
        }
        if is_snapshot(&path) {
            rename_snapshot(&dest_root, &path)?;
        }
        report.files += 1;
    }
    // Every path the manifest lists must have been in the tar: a bundle missing one is truncated,
    // and the manifest may not be persisted as a completed import over it. The cursor stays — the
    // partial import is still valid and resumable.
    let missing: Vec<String> = listed.keys().filter(|p| !seen.contains(*p)).cloned().collect();
    if let Some(path) = missing.first() {
        return Err(invalid(&format!(
            "{} missing from the bundle ({} of {} listed files absent)",
            path,
            missing.len(),
            listed.len()
        )));
    }
    // The manifest lands last, so it always describes the last fully imported bundle.
    atomic_write(&dest_root.join(MANIFEST_NAME), &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(report)
}

/// The manifest of a bundle on disk, read without importing anything — the join dialog's preview
/// and `fleet import --preview` read this.
pub fn read_manifest(bundle: &Path) -> Result<Manifest> {
    let file = std::fs::File::open(bundle)?;
    let mut tar = tar::Archive::new(zstd::Decoder::new(file)?);
    let mut entries = tar.entries()?;
    let first = entries.next().transpose()?.ok_or_else(|| invalid("empty bundle"))?;
    manifest_entry(first)
}

/// The first bundle entry must be the manifest, and the manifest must pin this format and a
/// version this build understands.
fn manifest_entry(mut entry: tar::Entry<'_, zstd::Decoder<'static, std::io::BufReader<std::fs::File>>>) -> Result<Manifest> {
    if !entry.header().entry_type().is_file() || entry.path()?.to_string_lossy() != MANIFEST_NAME {
        return Err(invalid("the first bundle entry must be manifest.json"));
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.format != BUNDLE_FORMAT {
        return Err(invalid(&format!(
            "not a {BUNDLE_FORMAT} bundle (format {:?})",
            manifest.format
        )));
    }
    if manifest.version > BUNDLE_VERSION {
        return Err(invalid(&format!(
            "bundle version {} is newer than this build understands ({BUNDLE_VERSION})",
            manifest.version
        )));
    }
    if !is_safe_segment(&manifest.origin_host) {
        return Err(invalid(&format!("bad origin_host {:?}", manifest.origin_host)));
    }
    Ok(manifest)
}

/// The committed offset no longer fits the bundle's file: upstream is shorter than what this
/// member already holds, so there is nothing to compare prefixes against.
fn shrunk_source(bytes: &[u8], committed: u64) -> bool {
    committed > bytes.len() as u64
}

/// Truncates a destination whose upstream prefix no longer matches the cursor, so the file
/// re-imports from zero instead of appending onto bytes that were never its own.
fn restart_file(dest_root: &Path, cursor: &mut ImportCursor, path: &str) -> Result<()> {
    let dest = working_path(dest_root, path);
    reject_symlink(dest_root, &dest)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cursor.digests.remove(path);
    std::fs::File::create(&dest)?;
    cursor.files.insert(
        path.to_string(),
        FileProgress {
            offset: 0,
            prefix_sha256: sha256_hex(b""),
        },
    );
    cursor.updated_at = Some(Utc::now());
    cursor.save(dest_root)?;
    Ok(())
}

/// Merges completed history lines into the member's `sessions.json` map: a re-import replaces a
/// record by namespaced id and never duplicates it. Returns the map's size.
fn merge_history(dest_root: &Path, bytes: &[u8]) -> Result<usize> {
    let file = dest_root.join("sessions.json");
    reject_symlink(dest_root, &file)?;
    let mut map: BTreeMap<String, ImportedSession> = std::fs::read(&file)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let session: ImportedSession = serde_json::from_slice(line)?;
        // A record only ever lands under its own namespaced id: the id must be `<member>:<original_id>`
        // for the member directory this import writes, or the line is not this bundle's to merge.
        if session.origin_host != dest_root.file_name().and_then(|n| n.to_str()).unwrap_or("")
            || session.id != format!("{}:{}", session.origin_host, session.original_id)
        {
            return Err(invalid(&format!(
                "history record for {:?} does not match its id {:?}",
                session.original_id, session.id
            )));
        }
        map.insert(session.id.clone(), session);
    }
    atomic_write(&file, &serde_json::to_vec_pretty(&map)?)?;
    Ok(map.len())
}

/// The bundle-relative paths that may ever be written, checked before anything is opened: the
/// one history file, allowlisted log names under a safe session id, transcripts under it, and
/// the allowlisted stats files. Anything else — absolute, `..`, unknown names — is refused, so a
/// crafted bundle cannot write outside `fleet-imports/<origin_host>/`. The path's category comes
/// back, so the import can cross-check it against the manifest's claim.
fn check_entry_path(path: &str) -> Result<Category> {
    let bad = |why: &str| Err(invalid(&format!("refusing bundle path {path:?}: {why}")));
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return bad("not a relative bundle path");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.iter().any(|s| s.is_empty() || s.starts_with('.')) {
        return bad("empty or hidden segment");
    }
    match segments.as_slice() {
        ["history", "sessions.jsonl"] => Ok(Category::History),
        ["logs", id, name] if is_safe_segment(id) && LOG_BASENAMES.contains(name) => Ok(Category::Logs),
        ["logs", id, rest @ ..]
            if is_safe_segment(id)
                && rest.first() == Some(&"transcripts")
                && rest.len() >= 2
                && rest.iter().all(|s| is_safe_segment(s)) =>
        {
            Ok(Category::Logs)
        }
        ["stats", name] if STAT_FILES.contains(name) || *name == "summary.json" => Ok(Category::Stats),
        _ => bad("not under history/, logs/ or stats/"),
    }
}

// ── Small helpers ──

/// Refuses to let an import follow a symlink planted under the member directory: every component
/// from `fleet-imports/<origin_host>/` down must be a real directory, the leaf (if it exists) a
/// real file, so nothing is ever written through a link to somewhere outside.
fn reject_symlink(member_root: &Path, dest: &Path) -> std::io::Result<()> {
    let mut at = member_root.to_path_buf();
    for component in dest
        .strip_prefix(member_root)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination outside the member directory"))?
    {
        at.push(component);
        if let Ok(meta) = std::fs::symlink_metadata(&at)
            && meta.file_type().is_symlink()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is a symlink", at.display()),
            ));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn invalid(why: &str) -> anyhow::Error {
    anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string()))
}

/// Sync `util::write_atomic`: a temp file beside the target, synced, then a rename, so a reader
/// never sees a partial cursor, session map or snapshot — and a crash never loses the old one.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        util::short_id()
    ));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

// ── CLI ──

/// `colonizer fleet export`: print the preview, then write the bundle — unless `--preview`, which
/// prints and writes nothing. Runs on this machine's own data dir; no mothership, no token.
pub(crate) fn cli_export(
    settings: &Settings,
    json: bool,
    out: Option<PathBuf>,
    cats: Categories,
    preview_only: bool,
) -> Result<i32> {
    let origin = Origin::for_config(&settings.config_dir)?;
    print_manifest(&preview_for(&settings.data_dir, &origin, &cats)?, json);
    if preview_only {
        return Ok(crate::cli::EXIT_OK);
    }
    let out = out.unwrap_or_else(|| default_export_name(&origin));
    export_bundle_for(&settings.data_dir, &origin, &cats, &out)?;
    let written = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    if json {
        println!("{}", json!({"bundle": out.display().to_string(), "bytes": written}));
    } else {
        println!("wrote {} ({})", out.display(), util::format_disk_size(written));
    }
    Ok(crate::cli::EXIT_OK)
}

/// `colonizer fleet import`: print the bundle's manifest, then import it into this machine's data
/// dir with progress on stderr. Ctrl-C (or anything else that ends the process) leaves a partial
/// import that resumes on the next run.
pub(crate) fn cli_import(settings: &Settings, file: &Path, preview_only: bool, json: bool) -> Result<i32> {
    print_manifest(&read_manifest(file)?, json);
    if preview_only {
        return Ok(crate::cli::EXIT_OK);
    }
    let report = import_bundle(&settings.data_dir, file, |p| {
        eprint!(
            "\r  {}: {}/{} bytes ({} of {} overall)",
            p.path, p.offset, p.total, p.done, p.bytes
        );
        ControlFlow::Continue(())
    })?;
    eprintln!();
    if report.cancelled {
        println!(
            "cancelled part way — run `colonizer fleet import {}` again to resume",
            file.display()
        );
    } else if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "imported {} sessions, {} files, {} from {} ({})",
            report.sessions,
            report.files,
            util::format_disk_size(report.bytes),
            report.origin_name,
            report.origin_host
        );
    }
    Ok(crate::cli::EXIT_OK)
}

fn print_manifest(manifest: &Manifest, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(manifest).unwrap_or_default());
        return;
    }
    println!(
        "origin {} ({}) — {} bundle v{}",
        manifest.origin_name, manifest.origin_host, BUNDLE_FORMAT, manifest.version
    );
    for (name, stats) in [
        ("history", &manifest.categories.history),
        ("logs", &manifest.categories.logs),
        ("stats", &manifest.categories.stats),
    ] {
        if !stats.included {
            println!("  {name}: not included");
            continue;
        }
        let range = match (stats.from, stats.to) {
            (Some(from), Some(to)) => format!(", {} .. {}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d")),
            _ => String::new(),
        };
        let unit = if name == "history" { "sessions" } else { "files" };
        println!(
            "  {name}: {} {unit}{range}, {}",
            stats.count,
            util::format_disk_size(stats.bytes)
        );
    }
}

fn default_export_name(origin: &Origin) -> PathBuf {
    PathBuf::from(format!(
        "colonizer-export-{}-{}.tar.zst",
        sanitize_segment(&origin.name),
        Local::now().format("%Y%m%d")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use serde_json::Value;

    /// A fresh temp dir, removed when the guard drops — the crate carries no tempfile dependency.
    struct TempDir(PathBuf);

    fn temp(tag: &str) -> (TempDir, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fleet-export-{}-{}-{}", tag, std::process::id(), util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        (TempDir(dir.clone()), dir)
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            repo: "octo/repo".into(),
            org: "octo".into(),
            issue: Some(7),
            issue_title: "fix the thing".into(),
            status: SessionStatus::Running,
            branch: format!("{id}-branch"),
            base: Some("main".into()),
            agent: "claude".into(),
            cost_usd: Some(0.5),
            routed_cost_usd: Some(0.25),
            model_tier: Some("tier2".into()),
            boot_timing: Some(json!({"total_ms": 4000})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ..Default::default()
        }
    }

    /// One seeded colony: a sessions.json holding it, its three log ledgers and a transcript.
    fn seed(data_dir: &Path, id: &str, events: &str) {
        write(
            &data_dir.join("sessions.json"),
            &serde_json::to_string_pretty(&vec![session(id)]).unwrap(),
        );
        write(&data_dir.join("sessions").join(id).join("events.jsonl"), events);
        write(&data_dir.join("sessions").join(id).join("harness.jsonl"), "{\"boot\":true}\n");
        write(&data_dir.join("sessions").join(id).join("gateway.jsonl"), "{\"routed\":1}\n");
        write(
            &data_dir.join("sessions").join(id).join("transcripts").join("a.txt"),
            "transcript\n",
        );
    }

    fn origin(host: &str) -> Origin {
        Origin {
            host: host.into(),
            name: format!("{host}-name"),
        }
    }

    /// Every entry of a bundle on disk, as (name, bytes).
    fn tar_entries(bundle: &Path) -> Vec<(String, Vec<u8>)> {
        let file = std::fs::File::open(bundle).unwrap();
        let mut tar = tar::Archive::new(zstd::Decoder::new(file).unwrap());
        let mut out = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            assert!(entry.header().entry_type().is_file(), "bundles only ever hold regular files");
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            out.push((name, bytes));
        }
        out
    }

    /// Writes (name, bytes) entries as a zstd-compressed tar — for the crafted-bundle tests.
    fn write_bundle(path: &Path, entries: &[(String, Vec<u8>)]) {
        let bytes = tar_bytes(entries);
        std::fs::write(path, zstd::stream::encode_all(&bytes[..], 3).unwrap()).unwrap();
    }

    /// A crafted bundle with a `../` entry. `tar::Builder` refuses to write such a name itself,
    /// so the entry goes in under a same-length placeholder and the raw header is patched (with a
    /// fresh checksum) — exactly the kind of bundle the reader has to refuse.
    fn write_bundle_with_traversal(path: &Path, manifest: &[u8]) {
        let mut tar_bytes = tar_bytes(&[
            (MANIFEST_NAME.into(), manifest.to_vec()),
            ("zz/escaped".into(), b"x".to_vec()),
        ]);
        let at = tar_bytes.windows(10).position(|w| w == b"zz/escaped").unwrap();
        tar_bytes[at..at + 10].copy_from_slice(b"../escaped");
        let header = at - at % 512;
        tar_bytes[header + 148..header + 156].copy_from_slice(b"        ");
        let sum: u32 = tar_bytes[header..header + 512].iter().map(|b| *b as u32).sum();
        tar_bytes[header + 148..header + 156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        std::fs::write(path, zstd::stream::encode_all(&tar_bytes[..], 3).unwrap()).unwrap();
    }

    fn tar_bytes(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_bytes);
            for (name, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, name, bytes.as_slice()).unwrap();
            }
            tar.finish().unwrap();
        }
        tar_bytes
    }

    /// A bundle's manifest: its first entry, read back through the same helper as every entry.
    fn manifest_of(bundle: &Path) -> Manifest {
        serde_json::from_slice(&tar_entries(bundle)[0].1).unwrap()
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    #[test]
    fn export_never_carries_secrets() {
        let (_guard, root) = temp("secrets");
        let (data, config) = (root.join("data"), root.join("config"));
        // Every secret file the config dir can hold, each with a marker that must never travel.
        let secrets = [
            "api-token",
            "api-tokens.json",
            "claude-token",
            "claude-accounts.json",
            "claude-accounts/x.json",
            "providers.json",
            "provider-keys/openai",
            "colony-secrets.json",
            "colony-secrets/prod",
            "github-token",
            "notify-secret",
        ];
        for name in secrets {
            write(&config.join(name), &format!("MARKER-{name}\n"));
        }
        // Decoys in the data dir, and a colony record whose sensitive fields hold markers too —
        // the marker session is what sessions.json holds when the export runs.
        let mut s = session("s1");
        s.launched_by_token = Some("MARKER-launched-by-token".into());
        s.instructions = "MARKER-instructions".into();
        seed(&data, "s1", "event one\n");
        write(&data.join("sessions.json"), &serde_json::to_string_pretty(&vec![s]).unwrap());
        write(&data.join("api-token"), "MARKER-data-api-token\n");
        write(&data.join("sessions/s1/colony-secrets.json"), "MARKER-colony-secrets\n");
        write(&data.join("sessions/s1/instructions.md"), "MARKER-instructions-file\n");
        // The stats files, so the stats category has something real to carry.
        for name in STAT_FILES {
            write(&data.join(name), &format!("{{\"{name}\": 1}}\n"));
        }

        let bundle = root.join("b.tar.zst");
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &bundle).unwrap();
        let entries = tar_entries(&bundle);
        assert_eq!(entries[0].0, MANIFEST_NAME, "manifest.json is the first entry");
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        for name in [
            MANIFEST_NAME,
            HISTORY_FILE,
            "logs/s1/events.jsonl",
            "logs/s1/harness.jsonl",
            "logs/s1/gateway.jsonl",
            "logs/s1/transcripts/a.txt",
            "stats/spend.jsonl",
            "stats/summary.json",
        ] {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
        let markers: Vec<String> = secrets
            .iter()
            .map(|s| format!("MARKER-{s}"))
            .chain(
                [
                    "MARKER-launched-by-token",
                    "MARKER-instructions",
                    "MARKER-data-api-token",
                    "MARKER-colony-secrets",
                    "MARKER-instructions-file",
                ]
                .map(str::to_string),
            )
            .collect();
        for (name, bytes) in &entries {
            let text = String::from_utf8_lossy(bytes);
            for marker in &markers {
                assert!(!name.contains(marker.as_str()), "{name} names a secret");
                assert!(!text.contains(marker.as_str()), "{name} carries {marker}");
            }
        }
        // The manifest itself describes the history range and per-category sizes.
        let manifest = manifest_of(&bundle);
        assert_eq!(manifest.categories.history.count, 1);
        assert!(manifest.categories.history.from.is_some());
        assert!(manifest.categories.stats.from.is_none());
        assert!(manifest.categories.history.bytes > 0);
        assert_eq!(manifest.files.len(), names.len() - 1);
    }

    #[test]
    fn sessions_from_two_machines_stay_distinct() {
        let (_guard, root) = temp("namespaces");
        let (a, b, dest) = (root.join("a"), root.join("b"), root.join("dest"));
        seed(&a, "abc", "from a\n");
        seed(&b, "abc", "from b\n");
        let (bundle_a, bundle_b) = (root.join("a.tar.zst"), root.join("b.tar.zst"));
        export_bundle_for(&a, &origin("hostA"), &Categories::default(), &bundle_a).unwrap();
        export_bundle_for(&b, &origin("hostB"), &Categories::default(), &bundle_b).unwrap();
        import_bundle(&dest, &bundle_a, |_| ControlFlow::<()>::Continue(())).unwrap();
        import_bundle(&dest, &bundle_b, |_| ControlFlow::<()>::Continue(())).unwrap();

        for host in ["hostA", "hostB"] {
            let member = dest.join("fleet-imports").join(host);
            let map: BTreeMap<String, Value> = serde_json::from_slice(&read(&member.join("sessions.json"))).unwrap();
            let record = map
                .get(&format!("{host}:abc"))
                .unwrap_or_else(|| panic!("{host}:abc missing"));
            assert_eq!(record["original_id"], "abc");
            assert_eq!(record["origin_host"], host);
            let events = if host == "hostA" { "from a\n" } else { "from b\n" };
            assert_eq!(read(&member.join("logs/abc/events.jsonl")), events.as_bytes());
        }
    }

    #[test]
    fn reimport_is_idempotent_and_a_second_export_appends() {
        let (_guard, root) = temp("idempotent");
        let (data, dest) = (root.join("data"), root.join("dest"));
        seed(&data, "s1", "line one\n");
        let (v1, v2) = (root.join("v1.tar.zst"), root.join("v2.tar.zst"));
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &v1).unwrap();
        import_bundle(&dest, &v1, |_| ControlFlow::<()>::Continue(())).unwrap();
        let member = dest.join("fleet-imports/hostA");
        let (sessions_before, logs_before, history_before) = (
            read(&member.join("sessions.json")),
            read(&member.join("logs/s1/events.jsonl")),
            read(&member.join("history/sessions.jsonl")),
        );

        // The same bundle again: nothing is appended or duplicated.
        import_bundle(&dest, &v1, |_| ControlFlow::<()>::Continue(())).unwrap();
        assert_eq!(read(&member.join("sessions.json")), sessions_before);
        assert_eq!(read(&member.join("logs/s1/events.jsonl")), logs_before);
        assert_eq!(read(&member.join("history/sessions.jsonl")), history_before);

        // A new event and a new session at the source: only the new bytes land, one record each.
        write(&data.join("sessions/s1/events.jsonl"), "line one\nline two\n");
        let mut sessions: Vec<Session> = serde_json::from_slice(&read(&data.join("sessions.json"))).unwrap();
        sessions.push(session("s2"));
        write(&data.join("sessions.json"), &serde_json::to_string_pretty(&sessions).unwrap());
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &v2).unwrap();
        let report = import_bundle(&dest, &v2, |_| ControlFlow::<()>::Continue(())).unwrap();
        assert_eq!(read(&member.join("logs/s1/events.jsonl")), b"line one\nline two\n");
        // The history file kept v1's line as its prefix and gained only s2's.
        let history_after = read(&member.join("history/sessions.jsonl"));
        assert!(history_after.starts_with(&history_before), "v1's line must survive intact");
        assert_eq!(String::from_utf8_lossy(&history_after).lines().count(), 2);
        let map: BTreeMap<String, Value> = serde_json::from_slice(&read(&member.join("sessions.json"))).unwrap();
        assert_eq!(map.len(), 2, "one record per session, never a duplicate");
        assert_eq!(report.sessions, 2);
    }

    #[test]
    fn chunked_transfer_resumes_and_rejects_gaps() {
        let (_guard, root) = temp("chunks");
        let (dest_root, src) = (root.join("imports"), root.join("events.jsonl"));
        let body: String = (0..40).map(|i| format!("event {i:03} — a line of logs\n")).collect();
        write(&src, &body);
        let path = "logs/s1/events.jsonl";

        // Send the first two chunks, then crash: the cursor is all that survives.
        let mut cursor = ImportCursor::default();
        for chunk in chunks_for(&src, path, 0, 64).take(2) {
            apply_chunk(&dest_root, &mut cursor, &chunk.unwrap()).unwrap();
        }
        let committed = cursor.offset(path);
        assert!(committed > 0 && committed < body.len() as u64);
        drop(cursor);

        // Reload from disk and resume from where the cursor says.
        let mut cursor = ImportCursor::load(&dest_root);
        assert_eq!(cursor.offset(path), committed);
        for chunk in chunks_for(&src, path, cursor.offset(path), 64) {
            apply_chunk(&dest_root, &mut cursor, &chunk.unwrap()).unwrap();
        }
        assert_eq!(read(&dest_root.join(path)), body.as_bytes());

        // A re-sent committed chunk is a no-op; one past the frontier is a gap error.
        let first = chunks_for(&src, path, 0, 64).next().unwrap().unwrap();
        assert_eq!(apply_chunk(&dest_root, &mut cursor, &first).unwrap(), Applied::Duplicate);
        assert_eq!(read(&dest_root.join(path)), body.as_bytes());
        let mut past = first.clone();
        past.offset = cursor.offset(path) + 1;
        assert!(apply_chunk(&dest_root, &mut cursor, &past).is_err());

        // The wire shape: exactly the five contract fields, data as padded standard base64.
        let json: Value = serde_json::to_value(&first).unwrap();
        assert_eq!(
            json.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["path", "offset", "raw_len", "total", "data"]
        );
        assert!(util::b64_decode(json["data"].as_str().unwrap()).is_some());
    }

    #[test]
    fn cancelled_import_resumes_to_a_clean_result() {
        let (_guard, root) = temp("cancel");
        let (data, dest, clean) = (root.join("data"), root.join("dest"), root.join("clean"));
        seed(&data, "s1", "event one\n");
        let bundle = root.join("b.tar.zst");
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &bundle).unwrap();

        // Cancel the moment the first chunk is about to move.
        let report = import_bundle(&dest, &bundle, |_| ControlFlow::Break(())).unwrap();
        assert!(report.cancelled);

        // The finished import of the same bundle equals a clean import elsewhere, byte for byte.
        let resumed = import_bundle(&dest, &bundle, |_| ControlFlow::<()>::Continue(())).unwrap();
        let reference = import_bundle(&clean, &bundle, |_| ControlFlow::<()>::Continue(())).unwrap();
        assert_eq!(resumed.sessions, reference.sessions);
        assert_eq!(
            read(&dest.join("fleet-imports/hostA/sessions.json")),
            read(&clean.join("fleet-imports/hostA/sessions.json"))
        );
        assert_eq!(
            read(&dest.join("fleet-imports/hostA/logs/s1/events.jsonl")),
            read(&clean.join("fleet-imports/hostA/logs/s1/events.jsonl"))
        );
        assert_eq!(
            read(&dest.join("fleet-imports/hostA/manifest.json")),
            read(&clean.join("fleet-imports/hostA/manifest.json"))
        );
    }

    #[test]
    fn a_rewritten_source_log_reimports_from_zero() {
        let (_guard, root) = temp("rotate");
        let (data, dest) = (root.join("data"), root.join("dest"));
        seed(&data, "s1", "original line one\noriginal line two\n");
        let (v1, v2) = (root.join("v1.tar.zst"), root.join("v2.tar.zst"));
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &v1).unwrap();
        import_bundle(&dest, &v1, |_| ControlFlow::<()>::Continue(())).unwrap();

        write(&data.join("sessions/s1/events.jsonl"), "rewritten from scratch\n");
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &v2).unwrap();
        import_bundle(&dest, &v2, |_| ControlFlow::<()>::Continue(())).unwrap();
        let member = dest.join("fleet-imports/hostA");
        assert_eq!(read(&member.join("logs/s1/events.jsonl")), b"rewritten from scratch\n");
        let map: BTreeMap<String, Value> = serde_json::from_slice(&read(&member.join("sessions.json"))).unwrap();
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn crafted_bundles_are_refused() {
        let (_guard, root) = temp("safety");
        let dest = root.join("dest");
        let manifest = serde_json::to_vec(&json!({
            "format": BUNDLE_FORMAT, "version": BUNDLE_VERSION,
            "origin_host": "../evil", "origin_name": "evil", "created_at": Utc::now(),
            "categories": {
                "history": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0},
                "logs": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0},
                "stats": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0}
            },
            "files": []
        }))
        .unwrap();
        let good_origin = serde_json::to_vec(&json!({
            "format": BUNDLE_FORMAT, "version": BUNDLE_VERSION,
            "origin_host": "hostA", "origin_name": "a", "created_at": Utc::now(),
            "categories": {
                "history": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0},
                "logs": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0},
                "stats": {"included": false, "count": 0, "from": null, "to": null, "bytes": 0}
            },
            "files": []
        }))
        .unwrap();

        // A bad origin host never creates its directory.
        let bad_origin = root.join("bad-origin.tar.zst");
        write_bundle(&bad_origin, &[(MANIFEST_NAME.into(), manifest.clone())]);
        assert!(import_bundle(&dest, &bad_origin, |_| ControlFlow::<()>::Continue(())).is_err());
        assert!(!dest.join("fleet-imports").join("..").exists());

        // A traversal entry is refused outright.
        let traversal = root.join("traversal.tar.zst");
        write_bundle_with_traversal(&traversal, &good_origin);
        assert!(import_bundle(&dest, &traversal, |_| ControlFlow::<()>::Continue(())).is_err());
        assert!(!root.join("escaped").exists());

        // A path outside the three categories is refused, as is an unknown stats file.
        for path in [
            "secrets/api-token",
            "stats/not-allowlisted",
            "logs/../events.jsonl",
            "history/other.jsonl",
        ] {
            assert!(check_entry_path(path).is_err(), "{path} must be refused");
        }
        assert!(check_entry_path(HISTORY_FILE).is_ok());
        assert!(check_entry_path("logs/s1/transcripts/deep/x.txt").is_ok());
    }

    #[test]
    fn a_bundle_missing_a_listed_file_is_refused_and_stays_resumable() {
        let (_guard, root) = temp("missing");
        let (data, dest) = (root.join("data"), root.join("dest"));
        seed(&data, "s1", "event one\n");
        // Export for real, then rewrite the tar without one entry the manifest still lists.
        let bundle = root.join("full.tar.zst");
        export_bundle_for(&data, &origin("hostA"), &Categories::default(), &bundle).unwrap();
        let mut entries = tar_entries(&bundle);
        entries.retain(|(name, _)| name != "logs/s1/events.jsonl");
        let truncated = root.join("truncated.tar.zst");
        write_bundle(&truncated, &entries);

        let err = import_bundle(&dest, &truncated, |_| ControlFlow::<()>::Continue(())).unwrap_err();
        assert!(err.to_string().contains("missing from the bundle"), "{err}");
        // The manifest may not describe an incomplete import — but what did land stays valid and
        // resumable: the cursor survives on disk.
        let member = dest.join("fleet-imports/hostA");
        assert!(!member.join(MANIFEST_NAME).exists());
        assert!(member.join("cursor.json").exists());
        assert!(member.join(HISTORY_FILE).exists());
    }

    #[test]
    fn the_published_schema_pins_format_and_version() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../docs/fleet-export.schema.json")).expect("docs/fleet-export.schema.json");
        assert_eq!(schema["properties"]["format"]["const"], json!(BUNDLE_FORMAT));
        assert_eq!(schema["properties"]["version"]["const"], json!(BUNDLE_VERSION));
    }
}
