//! The local log archive (issue #496): when a colony ends, its session directory is tarred and
//! zstd-compressed to `<data_dir>/archive/<org>/<repo>/<yyyy>/<mm>/`, one revision per distinct
//! content. The terminal edge archives off-path, delete archives first, retention only runs
//! through `POST /api/archive/retention` — a dry run plans, applying needs the preview's own list.

use crate::sessions::Session;
use crate::store::SessionStore;
use crate::{ApiResult, App, Shared, client_error};
use anyhow::Context;
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Datelike, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The archive root under the data dir.
fn archive_root(app: &App) -> PathBuf {
    app.cfg.data_dir.join("archive")
}

/// One path segment safe on any filesystem: anything but letters, digits, `-`, `_` and `.` becomes
/// `-`, leading dots are dropped, and the result is at most 64 bytes and never empty.
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

/// The bundle key's directory, from the session's own `org`/`repo` (the segment is `repo`'s last
/// part) and `created_at`, so every revision of one colony lands beside the first.
fn rel_dir_of(s: &Session) -> String {
    let repo = s.repo.rsplit('/').next().unwrap_or("repo");
    format!(
        "{}/{}/{:04}/{:02}",
        sanitize_segment(&s.org),
        sanitize_segment(repo),
        s.created_at.year(),
        s.created_at.month()
    )
}

/// Revision 1 is the plain id; later revisions add `.r2`, `.r3` — never overwriting the bundle before.
fn stem_of(id: &str, revision: u32) -> String {
    if revision <= 1 {
        id.to_string()
    } else {
        format!("{id}.r{revision}")
    }
}

/// `<id>.json` is revision 1's sidecar, `<id>.r2.json` revision 2's.
fn revision_of(name: &str, id: &str) -> Option<u32> {
    let rest = name.strip_prefix(id)?.strip_prefix('.')?;
    if rest == "json" {
        return Some(1);
    }
    rest.strip_prefix('r')?.strip_suffix(".json")?.parse().ok()
}

/// `.r2`, `.r17` — the revision suffix, digits only after the `.r`.
fn is_revision_suffix(rest: &str) -> bool {
    match rest.strip_prefix(".r") {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Whether `name` is a bundle or sidecar of exactly session `id` — `abc.tar.zst`, `abc.r2.tar.zst`
/// and their `.json` sidecars, never `abcd.tar.zst` or `abc.def.tar.zst`.
fn is_bundle_file(name: &str, id: &str) -> bool {
    let stem = name
        .strip_suffix(".tar.zst")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or_default();
    stem == id || stem.strip_prefix(id).is_some_and(is_revision_suffix)
}

/// The sidecar JSON next to every bundle: the index record for one archived revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct IndexRecord {
    pub session: String,
    pub repo: String,
    pub org: String,
    pub issue: Option<u64>,
    pub title: String,
    pub status: String,
    pub pr_url: Option<String>,
    pub cost_usd: Option<f64>,
    /// Tokens per model, straight off the session record.
    pub model_usage: Option<Value>,
    /// The model tier and agent kind the colony ran on.
    pub model_tier: Option<String>,
    pub agent: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: DateTime<Utc>,
    /// The stable host id of the mothership that wrote the bundle.
    pub mothership: String,
    pub revision: u32,
    /// The bundle's path under the archive root, `/`-separated.
    pub bundle: String,
    pub bytes: u64,
    /// What the session directory looked like when archived; a revision is written only when it changes.
    pub fingerprint: String,
}

/// The session directory read through the session store (#325), so a future remote backend serves
/// the archive like everything else, plus its fingerprint. `None` when there is no session directory.
async fn snapshot(data_dir: &Path, id: &str) -> anyhow::Result<Option<(Vec<(String, u64, Vec<u8>)>, String)>> {
    let dir = data_dir.join("sessions").join(id);
    match tokio::fs::metadata(&dir).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let store = crate::store::LocalDirStore::new(data_dir);
    let (mut files, mut total, mut newest) = (Vec::new(), 0u64, 0u64);
    for name in store.list_files(id).await? {
        let Some(bytes) = store.read_file(id, &name).await? else {
            continue;
        };
        let mtime = tokio::fs::metadata(dir.join(&name))
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            // Nanoseconds: a same-size rewrite inside the second that wrote the last bundle must
            // still read as a change.
            .map_or(0, |d| d.as_nanos() as u64);
        total += bytes.len() as u64;
        newest = newest.max(mtime);
        files.push((name, mtime, bytes));
    }
    let fingerprint = format!("{} files, {total} bytes, newest mtime {newest}", files.len());
    Ok(Some((files, fingerprint)))
}

/// The newest revision's sidecar record under `dir`, if any. The sidecars are the revision
/// ledger: each is written after its bundle is fully in place, so a crash mid-archive leaves
/// no sidecar and no revision to find.
async fn latest_revision(dir: &Path, id: &str) -> anyhow::Result<Option<IndexRecord>> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut newest: Option<(u32, PathBuf)> = None;
    while let Some(entry) = entries.next_entry().await? {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let Some(revision) = revision_of(&name, id) else { continue };
        if newest.as_ref().is_none_or(|(n, _)| revision > *n) {
            newest = Some((revision, dir.join(name)));
        }
    }
    let Some((_, path)) = newest else { return Ok(None) };
    Ok(serde_json::from_slice(&tokio::fs::read(&path).await?).ok())
}

/// Writes the tar+zstd to a temp name beside the bundle, then hard-links it under the revision's
/// name: the link only lands in a free name, so an existing bundle is never overwritten and a
/// racing writer bumps to the next revision. No reader ever sees a half-written bundle.
fn write_bundle(dir: &Path, id: &str, mut revision: u32, files: Vec<(String, u64, Vec<u8>)>) -> anyhow::Result<(u64, u32)> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.{}.tmp", stem_of(id, revision), crate::util::short_id()));
    let write = || -> anyhow::Result<()> {
        let f = std::fs::File::create(&tmp).with_context(|| format!("could not create {}", tmp.display()))?;
        let mut zst = zstd::Encoder::new(f, 3).context("could not start the zstd stream")?;
        {
            let mut tar = tar::Builder::new(&mut zst);
            for (name, mtime, bytes) in &files {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(*mtime);
                header.set_cksum();
                tar.append_data(&mut header, name, bytes.as_slice())?;
            }
            tar.finish()?;
        }
        zst.finish()?.sync_all()?;
        Ok(())
    };
    let out = write().and_then(|()| {
        loop {
            let target = dir.join(format!("{}.tar.zst", stem_of(id, revision)));
            match std::fs::hard_link(&tmp, &target) {
                Ok(()) => {
                    break std::fs::metadata(&target)
                        .map(|m| (m.len(), revision))
                        .context("could not size the bundle");
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => revision += 1,
                Err(e) => {
                    break Err(anyhow::Error::from(e).context(format!("could not publish {}", target.display())));
                }
            }
        }
    });
    // The temp name always goes: on success the published link holds the inode.
    let _ = std::fs::remove_file(&tmp);
    out
}

/// Archives one colony's session directory as a new revision and answers the bundle's path under
/// the archive root. `None` when there is nothing to do: no session directory, or a fingerprint
/// that already matches the newest bundle.
pub(crate) async fn archive_session(app: &App, s: &Session) -> anyhow::Result<Option<String>> {
    archive_data(&app.cfg.data_dir, s, &crate::runtime::host_id(app)).await
}

async fn archive_data(data_dir: &Path, s: &Session, mothership: &str) -> anyhow::Result<Option<String>> {
    let Some((files, fingerprint)) = snapshot(data_dir, &s.id).await? else {
        return Ok(None);
    };
    let rel_dir = rel_dir_of(s);
    let dir = data_dir.join("archive").join(&rel_dir);
    // File names are sanitized segments; the record keeps the session's real id.
    let id = sanitize_segment(&s.id);
    let latest = latest_revision(&dir, &id).await?;
    if latest.as_ref().is_some_and(|r| r.fingerprint == fingerprint) {
        return Ok(None);
    }
    let revision = latest.as_ref().map_or(1, |r| r.revision + 1);
    // The tar and the compression are the blocking part; everything around them is already async.
    let (bytes, revision) = {
        let (dir, id) = (dir.clone(), id.clone());
        tokio::task::spawn_blocking(move || write_bundle(&dir, &id, revision, files))
            .await
            .context("the archive task did not finish")??
    };
    let stem = stem_of(&id, revision);
    let rel = format!("{rel_dir}/{stem}.tar.zst");
    let record = IndexRecord {
        session: s.id.clone(),
        repo: s.repo.clone(),
        org: s.org.clone(),
        issue: s.issue,
        title: s.issue_title.clone(),
        status: s.status.as_str().to_string(),
        pr_url: s.pr_url.clone(),
        cost_usd: Some(s.total_cost_usd()),
        model_usage: s.model_usage.clone(),
        model_tier: s.model_tier.clone(),
        agent: s.agent.clone(),
        created_at: s.created_at,
        updated_at: s.updated_at,
        archived_at: Utc::now(),
        mothership: mothership.to_string(),
        revision,
        bundle: rel.clone(),
        bytes,
        fingerprint,
    };
    crate::util::write_atomic(&dir.join(format!("{stem}.json")), &serde_json::to_vec_pretty(&record)?).await?;
    Ok(Some(rel))
}

/// Hook A (issue #496): the colony just crossed onto a terminal status. Snapshot its logs off the
/// update path — an archive must never block or fail a colony that just finished, so the spawned
/// task's whole error path is a printout.
pub(crate) fn spawn_on_end(data_dir: PathBuf, session: Session, mothership: String) {
    tokio::spawn(async move {
        if let Err(e) = archive_data(&data_dir, &session, &mothership).await {
            eprintln!("archive: could not archive {}'s logs: {e:#}", session.id);
        }
    });
}

/// Every regular file under the archive root, depth-first. Symlinks are never followed, and a
/// subtree that will not read is skipped.
async fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(name) = entry.file_name().into_string() else { continue };
            let path = dir.join(&name);
            match entry.file_type().await {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(t) if t.is_file() => files.push(path),
                _ => {}
            }
        }
    }
    files
}

/// Every sidecar record under the archive root. Files without a parseable sidecar — temp names,
/// a reserved-but-unfinished bundle — are invisible here by construction.
async fn collect_index(root: &Path) -> Vec<IndexRecord> {
    let mut records = Vec::new();
    for path in walk_files(root).await {
        if !path.extension().is_some_and(|e| e == "json") {
            continue;
        }
        if let Ok(bytes) = tokio::fs::read(&path).await
            && let Ok(record) = serde_json::from_slice::<IndexRecord>(&bytes)
        {
            records.push(record);
        }
    }
    records
}

/// `GET /api/archive`: the whole archive — the root, how many bundles and bytes it holds, and
/// every index record, newest first.
pub(crate) async fn list(State(app): State<Shared>) -> Json<Value> {
    let root = archive_root(&app);
    let mut records = collect_index(&root).await;
    records.sort_by(|a, b| b.archived_at.cmp(&a.archived_at).then_with(|| b.bundle.cmp(&a.bundle)));
    let (count, bytes) = (records.len(), records.iter().map(|r| r.bytes).sum::<u64>());
    let entries: Vec<Value> = records
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
        .collect();
    Json(json!({"root": root.display().to_string(), "count": count, "bytes": bytes, "entries": entries}))
}

/// The `POST /api/archive/retention` body. With neither limit set the plan is empty: retention
/// only ever does what an explicit rule asks for, and this API is the only thing that runs it.
#[derive(Debug, Deserialize)]
pub(crate) struct RetentionRequest {
    /// Remove bundles archived more than this many days ago.
    pub keep_days: Option<f64>,
    /// Remove oldest-first until the archive holds at most this many gigabytes.
    pub max_gb: Option<f64>,
    /// Every bundle here is the only copy, so nothing goes unless the rule explicitly allows
    /// deleting the single copy.
    #[serde(default)]
    pub allow_single_copy: bool,
    /// A preview by default; applying is a second, explicit call.
    #[serde(default = "default_true")]
    pub dry_run: bool,
    /// The bundle list the preview returned. Required to apply, and a mismatch refuses the apply:
    /// what is removed is exactly what was shown.
    pub expect: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

/// Negative or non-finite limits are a bad request, not a plan that silently keeps everything.
fn check_limits(req: &RetentionRequest) -> Result<(), (StatusCode, String)> {
    let sane = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && v >= 0.0);
    if sane(req.keep_days) && sane(req.max_gb) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            "keep_days and max_gb must be finite and not negative".into(),
        ))
    }
}

/// The retention plan, as a pure function over the index records so the rules are testable
/// without a disk: bundles archived before `now - keep_days`, plus — oldest first — bundles taken
/// until the archive holds at most `max_gb`. Unless `allow_single_copy` says the one local copy
/// may go, nothing is removed and every bundle the rules would have taken is counted in
/// `kept_single_copy` instead: never delete what exists in only one place unless the rule
/// explicitly allows it.
pub(crate) fn plan_retention(
    records: &[IndexRecord],
    keep_days: Option<f64>,
    max_gb: Option<f64>,
    allow_single_copy: bool,
    now: DateTime<Utc>,
) -> RetentionPlan {
    // A keep_days beyond what a TimeDelta or a DateTime can hold keeps everything: the cutoff
    // falls back to the epoch, before any bundle was archived.
    let cutoff = keep_days.map(|d| {
        let delta = TimeDelta::try_seconds((d * 86_400.0) as i64);
        delta
            .and_then(|delta| now.checked_sub_signed(delta))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    });
    let max_bytes = max_gb.map(|g| (g * 1e9) as u64);
    let mut oldest_first: Vec<&IndexRecord> = records.iter().collect();
    oldest_first.sort_by_key(|r| r.archived_at);
    let mut total = records.iter().map(|r| r.bytes).sum::<u64>();
    let mut picked = Vec::new();
    for r in oldest_first {
        if cutoff.is_some_and(|c| r.archived_at < c) || max_bytes.is_some_and(|cap| total > cap) {
            total = total.saturating_sub(r.bytes);
            picked.push(r.clone());
        }
    }
    if allow_single_copy {
        RetentionPlan {
            remove: picked,
            kept_single_copy: 0,
        }
    } else {
        RetentionPlan {
            remove: Vec::new(),
            kept_single_copy: picked.len(),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct RetentionPlan {
    /// The bundles to remove, oldest first.
    pub remove: Vec<IndexRecord>,
    /// Bundles the rules would have taken but for the single-copy rule.
    pub kept_single_copy: usize,
}

/// Removes the bundle and its sidecar; a name already gone is as good as removed.
async fn remove_with_sidecar(root: &Path, rel: &str) -> Result<(), String> {
    let bundle = within(root, rel)?;
    let sidecar = sidecar_of(&bundle);
    for path in [bundle, sidecar] {
        if let Err(e) = tokio::fs::remove_file(&path).await
            && e.kind() != ErrorKind::NotFound
        {
            return Err(format!("{}: {e}", path.display()));
        }
    }
    Ok(())
}

/// `POST /api/archive/retention`: the plan as JSON — a dry run by default; with `dry_run: false`
/// the `expect` list must equal the recomputed plan, and exactly those bundles and their sidecars
/// are removed, with any removal failure reported rather than swallowed.
pub(crate) async fn retention_reply(app: &App, req: RetentionRequest) -> Result<Value, (StatusCode, String)> {
    check_limits(&req)?;
    let records = collect_index(&archive_root(app)).await;
    let plan = plan_retention(&records, req.keep_days, req.max_gb, req.allow_single_copy, Utc::now());
    let answer = |removed: &[IndexRecord], kept_single_copy: usize| {
        json!({
            "dry_run": req.dry_run,
            "remove": removed.iter().map(|r| json!({"bundle": r.bundle, "session": r.session, "bytes": r.bytes, "archived_at": r.archived_at})).collect::<Vec<_>>(),
            "count": removed.len(),
            "bytes": removed.iter().map(|r| r.bytes).sum::<u64>(),
            "kept_single_copy": kept_single_copy,
        })
    };
    if req.dry_run {
        return Ok(answer(&plan.remove, plan.kept_single_copy));
    }
    let Some(expect) = &req.expect else {
        return Err((
            StatusCode::BAD_REQUEST,
            "applying retention needs expect: the bundle list the preview returned".into(),
        ));
    };
    let planned = plan.remove.iter().map(|r| r.bundle.clone()).collect::<Vec<_>>();
    if *expect != planned {
        return Err((
            StatusCode::CONFLICT,
            "the archive changed since the preview; re-run the dry run and apply its own expect".into(),
        ));
    }
    let mut removed = Vec::new();
    for r in &plan.remove {
        if let Err(e) = remove_with_sidecar(&archive_root(app), &r.bundle).await {
            // What went already stays gone; the failure is reported, never answered as success.
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "retention removed {} of {} bundles, stopped at {}: {e}",
                    removed.len(),
                    plan.remove.len(),
                    r.bundle
                ),
            ));
        }
        removed.push(r.clone());
    }
    Ok(answer(&removed, plan.kept_single_copy))
}

pub(crate) async fn retention(State(app): State<Shared>, Json(req): Json<RetentionRequest>) -> ApiResult<Value> {
    match retention_reply(&app, req).await {
        Ok(value) => Ok(Json(value)),
        Err((status, message)) => Err(client_error(status, &message)),
    }
}

/// The bundle's real path, refusing anything that does not name a plain relative path under the
/// archive root — the sidecar is data, and data must not pick where on the host a delete lands.
fn within(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let normal = !rel.starts_with('/') && rel.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    if !normal {
        return Err(format!("retention refuses the bundle path {rel:?}"));
    }
    Ok(root.join(rel))
}

/// The sidecar path for a bundle path: the `.tar.zst` suffix becomes `.json`.
pub(crate) fn sidecar_of(bundle: &Path) -> PathBuf {
    let text = bundle.to_string_lossy();
    PathBuf::from(
        text.strip_suffix(".tar.zst")
            .map_or(text.to_string(), |stem| format!("{stem}.json")),
    )
}

/// Every bundle and sidecar for one session id, wherever it sits under the archive root. Used by
/// `DELETE /api/sessions/{id}?purge_logs=true`. Answers how many bundles went (sidecars go with
/// them, uncounted) and, if a name could not be removed, a message naming it — the colony is
/// already gone by then, so the trouble is reported, not fatal.
pub(crate) async fn purge_bundles(app: &App, id: &str) -> (u32, Option<String>) {
    let id = sanitize_segment(id);
    let (mut purged, mut trouble) = (0u32, None);
    for path in walk_files(&archive_root(app)).await {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !is_bundle_file(name, &id) {
            continue;
        }
        if name.ends_with(".tar.zst") {
            purged += 1;
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            // Already gone is as good as removed.
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => trouble = trouble.or_else(|| Some(format!("{}: {e}", path.display()))),
        }
    }
    (purged, trouble)
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/archive", routing::get(list))
        .route("/api/archive/retention", routing::post(retention))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use crate::sessions::tests::app_with_colony;
    use std::io::Write;

    /// One revision on disk, as a real archive would leave it: a log file, then a bundle.
    async fn seed(app: &Shared, id: &str, log: &str) -> String {
        tokio::fs::write(app.session_dir(id).join("events.jsonl"), log).await.unwrap();
        archive_session(app, &app.session(id).await.unwrap()).await.unwrap().unwrap()
    }

    async fn expected_rel(app: &Shared, id: &str) -> String {
        let s = app.session(id).await.unwrap();
        format!("acme/repo/{}/{}.tar.zst", s.created_at.format("%Y/%m"), id)
    }

    #[tokio::test]
    async fn archiving_twice_without_a_change_writes_one_bundle_then_a_revision_on_change() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        let first = seed(&app, "abc", "{\"seq\":1}\n").await;
        assert_eq!(first, expected_rel(&app, "abc").await);
        let bundle = root.join("data/archive").join(&first);
        assert!(bundle.is_file());
        assert!(sidecar_of(&bundle).is_file(), "the sidecar sits beside the bundle");
        let r1 = std::fs::read(&bundle).unwrap();

        // Nothing changed since the bundle: no second one.
        let s = app.session("abc").await.unwrap();
        assert_eq!(archive_session(&app, &s).await.unwrap(), None, "the fingerprint matches");

        // A late write after the run ended: revision 2, and r1 byte for byte untouched.
        std::fs::OpenOptions::new()
            .append(true)
            .open(app.session_dir("abc").join("events.jsonl"))
            .unwrap()
            .write_all(b"{\"seq\":2}\n")
            .unwrap();
        let second = archive_session(&app, &s).await.unwrap().expect("a second revision");
        assert_eq!(second, format!("acme/repo/{}/abc.r2.tar.zst", s.created_at.format("%Y/%m")));
        assert_eq!(
            std::fs::read(root.join("data/archive").join(&first)).unwrap(),
            r1,
            "r1 untouched"
        );
        assert!(root.join("data/archive").join(&second).is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_terminal_edge_spawns_an_archive_of_the_logs() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        seed(&app, "abc", "{\"seq\":1}\n").await;
        app.update_session("abc", |s| s.status = SessionStatus::Stopped)
            .await
            .unwrap();
        let bundle = root.join("data/archive").join(expected_rel(&app, "abc").await);
        for _ in 0..200 {
            if bundle.is_file() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(bundle.is_file(), "the terminal edge left a bundle behind");
        let _ = std::fs::remove_dir_all(root);
    }

    fn record(session: &str, bundle: &str, bytes: u64, archived_at: DateTime<Utc>) -> IndexRecord {
        serde_json::from_value(json!({
            "session": session, "repo": "acme/repo", "org": "acme", "issue": null, "title": "",
            "status": "stopped", "pr_url": null, "cost_usd": null, "model_usage": null,
            "model_tier": null, "agent": "claude", "created_at": archived_at,
            "updated_at": archived_at, "archived_at": archived_at, "mothership": "host",
            "revision": 1, "bundle": bundle, "bytes": bytes, "fingerprint": "",
        }))
        .unwrap()
    }

    fn bundles(plan: &RetentionPlan) -> Vec<&str> {
        plan.remove.iter().map(|r| r.bundle.as_str()).collect()
    }

    #[test]
    fn retention_needs_a_rule_and_single_copy_keeps_everything() {
        let now = Utc::now();
        let records = vec![record("a", "a.tar.zst", 10, now - TimeDelta::hours(1))];
        // No rule, no plan: retention only ever does what an explicit rule asks for.
        assert!(plan_retention(&records, None, None, true, now).remove.is_empty());
        // A day-old bundle is neither older than keep_days nor needed for the size cap.
        assert!(plan_retention(&records, Some(30.0), Some(1.0), true, now).remove.is_empty());
        // An old bundle under the single-copy rule is kept, and counted.
        let plan = plan_retention(&records, Some(0.001), None, false, now);
        assert!(plan.remove.is_empty());
        assert_eq!(plan.kept_single_copy, 1);
    }

    #[test]
    fn keep_days_takes_only_old_bundles_and_max_gb_takes_oldest_first() {
        let now = Utc::now();
        let (old, mid, new) = (now - TimeDelta::days(40), now - TimeDelta::days(20), now - TimeDelta::days(1));
        // Newest first on input, so the ordering the planner answers is its own.
        let records = vec![
            record("new", "n.tar.zst", 4, new),
            record("old", "o.tar.zst", 2, old),
            record("mid", "m.tar.zst", 3, mid),
        ];
        assert_eq!(
            bundles(&plan_retention(&records, Some(30.0), None, true, now)),
            vec!["o.tar.zst"]
        );
        // 4+2+3 = 9 bytes against a 5-byte cap: oldest first until at most 5 remain (o, then m).
        assert_eq!(
            bundles(&plan_retention(&records, None, Some(5.0 / 1e9), true, now)),
            vec!["o.tar.zst", "m.tar.zst"]
        );
        // Both rules at once are the union, still oldest first.
        assert_eq!(
            bundles(&plan_retention(&records, Some(30.0), Some(5.0 / 1e9), true, now)),
            vec!["o.tar.zst", "m.tar.zst"]
        );
    }

    #[test]
    fn a_keep_days_past_the_calendar_falls_back_to_an_epoch_cutoff_that_removes_nothing() {
        let now = Utc::now();
        let records = vec![record("a", "a.tar.zst", 10, now)];
        // i64 seconds hold a trillion days, but a DateTime does not: keep everything anyway.
        let plan = plan_retention(&records, Some(1e12), None, true, now);
        assert!(plan.remove.is_empty(), "an unrepresentable cutoff removes nothing");
    }

    #[tokio::test]
    async fn applying_retention_needs_the_previews_expect_and_removes_exactly_it() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        // Different lengths: the fingerprint is count + size + mtime, and a rewrite that only
        // swapped bytes inside the same timestamp tick would read as "nothing changed".
        let first = seed(&app, "abc", "{\"seq\":1}\n").await;
        let second = seed(&app, "abc", "{\"seq\":2,\"more\":true}\n").await;
        // keep_days 0 makes both bundles older than the cutoff, so the plan takes both.
        let req = |dry_run, expect| RetentionRequest {
            keep_days: Some(0.0),
            max_gb: None,
            allow_single_copy: true,
            dry_run,
            expect,
        };
        let preview = retention_reply(&app, req(true, None)).await.unwrap();
        let listed: Vec<String> = preview["remove"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["bundle"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(listed, vec![first.clone(), second.clone()]);
        assert_eq!(preview["count"], json!(2));
        assert!(root.join("data/archive").join(&first).is_file(), "a dry run removes nothing");

        // Applying against anything but the preview's own list is refused, and removes nothing.
        let (status, _) = retention_reply(&app, req(false, Some(vec![second.clone()])))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(root.join("data/archive").join(&first).is_file());

        // The preview's own list removes exactly those bundles, with their sidecars.
        let out = retention_reply(&app, req(false, Some(listed))).await.unwrap();
        assert_eq!(out["count"], json!(2));
        for bundle in [&first, &second] {
            let path = root.join("data/archive").join(bundle);
            assert!(!path.exists(), "{bundle} is gone");
            assert!(!sidecar_of(&path).exists(), "the sidecar went with {bundle}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn retention_refuses_limits_that_are_negative_or_not_finite() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        for (keep_days, max_gb) in [(Some(-1.0), None), (None, Some(f64::NAN)), (None, Some(f64::INFINITY))] {
            let req = RetentionRequest {
                keep_days,
                max_gb,
                allow_single_copy: true,
                dry_run: true,
                expect: None,
            };
            let (status, message) = retention_reply(&app, req).await.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{keep_days:?} {max_gb:?}");
            assert!(message.contains("finite and not negative"), "{message}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn purge_matches_whole_file_names_only() {
        for name in ["abc.tar.zst", "abc.r2.tar.zst", "abc.json", "abc.r3.json"] {
            assert!(is_bundle_file(name, "abc"), "{name} belongs to abc");
        }
        for name in ["abc.def.tar.zst", "abcd.tar.zst", "abc.r2x.tar.zst", "abc.tmp", "ab.json"] {
            assert!(!is_bundle_file(name, "abc"), "{name} does not belong to abc");
        }
    }

    #[test]
    fn bundle_paths_are_sanitized_but_never_escape() {
        assert_eq!(sanitize_segment("acme"), "acme");
        assert_eq!(sanitize_segment("my org"), "my-org");
        assert_eq!(sanitize_segment(".."), "_");
        assert_eq!(sanitize_segment(".hidden"), "hidden");
        assert_eq!(sanitize_segment(""), "_");
        assert_eq!(rel_dir_of(&Session::default()), "_/_/1970/01");
    }
}
