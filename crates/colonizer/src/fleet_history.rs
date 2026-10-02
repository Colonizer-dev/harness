//! Fleet history (issue #762, the owner's side): what the members' history push stored under
//! `<data_dir>/fleet-ingest/<member_id>/` (`fleet_sync.rs`), made readable on the owner.
//!
//! - `GET /api/fleet/history` lists every member's synced colonies, newest finish first, with
//!   filters (member, repo, status, a finish-date range), the colony list's cursor pagination
//!   (`limit`, `cursor` = the last entry's `key`, `next_cursor` null at the end), and totals per
//!   member and per repository over the filtered set.
//! - `GET /api/fleet/history/{member}/{row_id}` is one colony's record and its logs' list.
//! - `GET /api/fleet/history/{member}/{row_id}/logs/{name}` streams one stored log.
//!
//! All three are owner-only: `api_tokens::classify` leaves every route it does not name to the
//! owner, so a scoped token — a member's fleet token included — reads 403. A removed member's
//! directory stays readable and its entries carry `member_removed: true`; its name survives in
//! `member.json`, written beside its rows at ingest.
//!
//! Logs are served as the member sent them. Redaction is the member's, before sending (#761); this
//! branch has no owner-side redaction pass to run on top.
//!
//! Retention: the reclaim tick (`reclaim.rs`) prunes rows received more than
//! `COLONIZER_FLEET_INGEST_RETENTION_DAYS` (default 90, `0` keeps everything) ago, then every
//! payload no remaining row references that is itself older than the cutoff.

use crate::fleet_export::{ImportedSession, is_safe_segment};
use crate::fleet_sync::{INGEST, INGEST_DIR, PayloadRef, ROWS_FILE};
use crate::sessions::SessionStatus;
use crate::{AppError, Shared, client_error, util};
use axum::{
    Json,
    body::Body,
    extract::{Path as UrlPath, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The member's name, beside its rows.
const MEMBER_FILE: &str = "member.json";
/// Days a received row is kept, unless `COLONIZER_FLEET_INGEST_RETENTION_DAYS` says otherwise.
pub const DEFAULT_RETENTION_DAYS: u64 = 90;
/// The colony list's page sizes (`sessions/api.rs`).
const DEFAULT_PAGE_LIMIT: usize = 20;
const MAX_PAGE_LIMIT: usize = 100;

/// One row as `post_rows` stored it.
#[derive(Clone, Deserialize)]
struct StoredRow {
    record: ImportedSession,
    #[serde(default)]
    payloads: Vec<PayloadRef>,
    received_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
struct MemberNote {
    id: String,
    name: String,
}

/// One member's ingest directory, read.
struct MemberHistory {
    id: String,
    name: String,
    removed: bool,
    rows: BTreeMap<String, StoredRow>,
}

fn ingest_root(data_dir: &Path) -> PathBuf {
    data_dir.join(INGEST_DIR)
}

/// A member's rows; a row that no longer parses is skipped rather than hiding the rest.
fn read_rows(dir: &Path) -> BTreeMap<String, StoredRow> {
    let stored: BTreeMap<String, Value> = std::fs::read(dir.join(ROWS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    stored
        .into_iter()
        .filter_map(|(id, v)| serde_json::from_value(v).ok().map(|row| (id, row)))
        .collect()
}

/// Every member directory, named from the current member list, else from its `member.json`, else
/// by its id; a directory whose member is gone is `removed`.
fn read_all(data_dir: &Path, current: &[(String, String)]) -> Vec<MemberHistory> {
    let Ok(entries) = std::fs::read_dir(ingest_root(data_dir)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(id) = entry.file_name().into_string() else { continue };
        if !is_safe_segment(&id) || !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        out.push(read_member(data_dir, &id, current));
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn read_member(data_dir: &Path, id: &str, current: &[(String, String)]) -> MemberHistory {
    let dir = ingest_root(data_dir).join(id);
    let live = current.iter().find(|(m, _)| m == id).map(|(_, name)| name.clone());
    let noted = || {
        std::fs::read(dir.join(MEMBER_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<MemberNote>(&b).ok())
            .map(|n| n.name)
    };
    MemberHistory {
        id: id.to_string(),
        removed: live.is_none(),
        name: live.or_else(noted).unwrap_or_else(|| id.to_string()),
        rows: read_rows(&dir),
    }
}

/// Records the member's current name beside its rows (called by `post_rows` after a store).
pub(crate) async fn note_member(app: &Shared, root: &Path) {
    let Some(id) = root.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let names = app.fleet_members.member_names().await;
    let Some((_, name)) = names.iter().find(|(m, _)| m == id) else {
        return;
    };
    let path = root.join(MEMBER_FILE);
    let current = tokio::fs::read(&path)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<MemberNote>(&b).ok());
    if current.is_some_and(|n| &n.name == name) {
        return;
    }
    let note = MemberNote {
        id: id.to_string(),
        name: name.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&note)
        && let Err(e) = util::write_atomic(&path, &bytes).await
    {
        eprintln!("fleet history: could not record member {id}'s name: {e:#}");
    }
}

fn status_name(status: SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn merged(r: &ImportedSession) -> bool {
    r.status == SessionStatus::Merged || r.merged_at.is_some()
}

/// The cursor and the detail route's address of one entry: `<member_id>/<row id>`.
fn key_of(member: &str, row: &str) -> String {
    format!("{member}/{row}")
}

fn entry_json(m: &MemberHistory, id: &str, row: &StoredRow) -> Value {
    json!({
        "key": key_of(&m.id, id),
        "member_id": m.id,
        "member_name": m.name,
        "member_removed": m.removed,
        "id": id,
        "received_at": row.received_at,
        "record": row.record,
        "payloads": row.payloads,
    })
}

/// The query `GET /api/fleet/history` takes. Every field stays a string so a malformed one is this
/// route's own 400.
#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    member: Option<String>,
    repo: Option<String>,
    status: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

/// An RFC 3339 instant, or a bare `YYYY-MM-DD` day — its start, or with `end_of_day` its end.
fn parse_when(raw: &str, end_of_day: bool) -> Option<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Some(at.with_timezone(&Utc));
    }
    let day = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let at = if end_of_day {
        day.and_hms_milli_opt(23, 59, 59, 999)?
    } else {
        day.and_hms_opt(0, 0, 0)?
    };
    Some(at.and_utc())
}

fn bad(message: &str) -> AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

#[derive(Default, Serialize)]
struct Totals {
    colonies: usize,
    merged: usize,
    /// The sum of the costs the rows carry; null when none carries one.
    cost_usd: Option<f64>,
}

impl Totals {
    fn add(&mut self, r: &ImportedSession) {
        self.colonies += 1;
        self.merged += usize::from(merged(r));
        if let Some(cost) = r.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
    }
}

/// The history view over what is on disk: pure over the members read, so the rules are testable.
fn history_view(members: &[MemberHistory], q: &HistoryQuery) -> Result<Value, AppError> {
    let limit = match q.limit.as_deref() {
        None => DEFAULT_PAGE_LIMIT,
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|_| bad(&format!("`limit` must be an integer between 1 and {MAX_PAGE_LIMIT}")))?
            .clamp(1, MAX_PAGE_LIMIT),
    };
    let since = match q.since.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(parse_when(raw, false).ok_or_else(|| bad("`since` must be RFC 3339 or YYYY-MM-DD"))?),
    };
    let until = match q.until.as_deref() {
        None | Some("") => None,
        Some(raw) => Some(parse_when(raw, true).ok_or_else(|| bad("`until` must be RFC 3339 or YYYY-MM-DD"))?),
    };
    let nonempty = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    let (member, repo, status) = (nonempty(&q.member), nonempty(&q.repo), nonempty(&q.status));

    let mut repos = BTreeSet::new();
    let mut hits: Vec<(&MemberHistory, &String, &StoredRow)> = Vec::new();
    for m in members {
        for (id, row) in &m.rows {
            repos.insert(row.record.repo.clone());
            let r = &row.record;
            let keep = member.as_deref().is_none_or(|x| x == m.id)
                && repo.as_deref().is_none_or(|x| x == r.repo)
                && status.as_deref().is_none_or(|x| x == status_name(r.status))
                && since.is_none_or(|t| r.updated_at >= t)
                && until.is_none_or(|t| r.updated_at <= t);
            if keep {
                hits.push((m, id, row));
            }
        }
    }
    // Newest finish first; ties by key, so pages are stable.
    hits.sort_by(|a, b| {
        b.2.record
            .updated_at
            .cmp(&a.2.record.updated_at)
            .then_with(|| key_of(&a.0.id, a.1).cmp(&key_of(&b.0.id, b.1)))
    });

    let mut total = Totals::default();
    let mut by_member: BTreeMap<&str, Totals> = BTreeMap::new();
    let mut by_repo: BTreeMap<&str, Totals> = BTreeMap::new();
    for (m, _, row) in &hits {
        total.add(&row.record);
        by_member.entry(&m.id).or_default().add(&row.record);
        by_repo.entry(&row.record.repo).or_default().add(&row.record);
    }

    let start = match q.cursor.as_deref().filter(|c| !c.is_empty()) {
        None => 0,
        Some(cursor) => {
            hits.iter()
                .position(|(m, id, _)| key_of(&m.id, id) == cursor)
                .ok_or_else(|| bad("`cursor` names no entry in this list; read a page and send back its `next_cursor`"))?
                + 1
        }
    };
    let end = (start + limit).min(hits.len());
    let next_cursor = (end < hits.len()).then(|| key_of(&hits[end - 1].0.id, hits[end - 1].1));
    let page: Vec<Value> = hits[start..end].iter().map(|(m, id, row)| entry_json(m, id, row)).collect();

    let member_rows: Vec<Value> = members
        .iter()
        .map(|m| {
            let t = by_member.remove(m.id.as_str()).unwrap_or_default();
            json!({"member_id": m.id, "name": m.name, "removed": m.removed,
                   "colonies": t.colonies, "merged": t.merged, "cost_usd": t.cost_usd})
        })
        .filter(|v| v["colonies"].as_u64() != Some(0))
        .collect();
    let repo_rows: Vec<Value> = by_repo
        .into_iter()
        .map(|(repo, t)| json!({"repo": repo, "colonies": t.colonies, "merged": t.merged, "cost_usd": t.cost_usd}))
        .collect();
    Ok(json!({
        "colonies": page,
        "next_cursor": next_cursor,
        "stats": {"total": total, "members": member_rows, "repos": repo_rows},
        "members": members.iter().map(|m| json!({"id": m.id, "name": m.name, "removed": m.removed})).collect::<Vec<_>>(),
        "repos": repos,
        "retention_days": retention_days(),
    }))
}

async fn load(app: &Shared) -> Vec<MemberHistory> {
    let current = app.fleet_members.member_names().await;
    let data_dir = app.cfg.data_dir.clone();
    tokio::task::spawn_blocking(move || read_all(&data_dir, &current))
        .await
        .unwrap_or_default()
}

/// `GET /api/fleet/history`: every member's synced colonies, filtered and paged, with totals.
pub async fn list(State(app): State<Shared>, Query(q): Query<HistoryQuery>) -> Result<Json<Value>, AppError> {
    let members = load(&app).await;
    history_view(&members, &q).map(Json)
}

/// One member's stored row, or the 404 an unknown member or row gets.
async fn find(app: &Shared, member: &str, row_id: &str) -> Result<(MemberHistory, StoredRow), AppError> {
    let missing = || client_error(StatusCode::NOT_FOUND, "no such fleet history entry");
    if !is_safe_segment(member) {
        return Err(missing());
    }
    let dir = ingest_root(&app.cfg.data_dir).join(member);
    if !tokio::fs::try_exists(&dir).await.unwrap_or(false) {
        return Err(missing());
    }
    let current = app.fleet_members.member_names().await;
    let id = member.to_string();
    let data_dir = app.cfg.data_dir.clone();
    let m = tokio::task::spawn_blocking(move || read_member(&data_dir, &id, &current))
        .await
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
    let row = m.rows.get(row_id).cloned().ok_or_else(missing)?;
    Ok((m, row))
}

/// `GET /api/fleet/history/{member}/{row_id}`: one colony's record, and each of its logs with
/// whether the owner holds it.
pub async fn detail(
    State(app): State<Shared>,
    UrlPath((member, row_id)): UrlPath<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let (m, row) = find(&app, &member, &row_id).await?;
    let mut out = entry_json(&m, &row_id, &row);
    let payloads = ingest_root(&app.cfg.data_dir).join(&m.id).join("payloads");
    let mut logs = Vec::new();
    for p in &row.payloads {
        let stored = !p.omitted && tokio::fs::try_exists(payloads.join(&p.sha256)).await.unwrap_or(false);
        logs.push(json!({"name": p.name, "sha256": p.sha256, "bytes": p.bytes, "omitted": p.omitted, "stored": stored}));
    }
    out["logs"] = json!(logs);
    Ok(Json(out))
}

/// `GET /api/fleet/history/{member}/{row_id}/logs/{name}`: one of the colony's logs, streamed from
/// its stored payload as the member sent it.
pub async fn log(
    State(app): State<Shared>,
    UrlPath((member, row_id, name)): UrlPath<(String, String, String)>,
) -> Result<Response, AppError> {
    let (m, row) = find(&app, &member, &row_id).await?;
    let missing = || client_error(StatusCode::NOT_FOUND, "this colony has no such stored log");
    let payload = row
        .payloads
        .iter()
        .find(|p| p.name == name && !p.omitted)
        .ok_or_else(missing)?;
    if payload.sha256.len() != 64 || !payload.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(missing());
    }
    let path = ingest_root(&app.cfg.data_dir)
        .join(&m.id)
        .join("payloads")
        .join(&payload.sha256);
    let file = tokio::fs::File::open(&path).await.map_err(|_| missing())?;
    let stream = futures_util::stream::unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt as _;
        let mut buf = vec![0u8; 64 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_LENGTH, payload.bytes)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))
}

// ---------------------------------------------------------------------------
// Retention.
// ---------------------------------------------------------------------------

/// Days to keep a received row: `COLONIZER_FLEET_INGEST_RETENTION_DAYS`, default 90; `0` keeps
/// everything; garbage means the default.
pub fn retention_days() -> u64 {
    parse_retention_days(util::env_nonempty("COLONIZER_FLEET_INGEST_RETENTION_DAYS").as_deref())
}

pub fn parse_retention_days(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// What one prune removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub rows: usize,
    pub payloads: usize,
}

/// Drops rows received before `now - days`, then every payload no remaining row references whose
/// file is older than the cutoff too — so an upload whose row is still on its way is never taken.
/// A member directory left with nothing is removed. `days == 0` keeps everything.
pub async fn prune(data_dir: &Path, days: u64, now: DateTime<Utc>) -> Pruned {
    if days == 0 {
        return Pruned::default();
    }
    let Some(cutoff) =
        chrono::Duration::try_days(days.min(i64::MAX as u64 / 86_400) as i64).and_then(|d| now.checked_sub_signed(d))
    else {
        return Pruned::default();
    };
    let _one = INGEST.lock().await;
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || prune_blocking(&data_dir, cutoff))
        .await
        .unwrap_or_default()
}

fn prune_blocking(data_dir: &Path, cutoff: DateTime<Utc>) -> Pruned {
    let mut pruned = Pruned::default();
    let Ok(entries) = std::fs::read_dir(ingest_root(data_dir)) else {
        return pruned;
    };
    let cutoff_time = std::time::SystemTime::from(cutoff);
    for entry in entries.flatten() {
        let Ok(id) = entry.file_name().into_string() else { continue };
        if !is_safe_segment(&id) || !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let file = dir.join(ROWS_FILE);
        let mut stored: BTreeMap<String, Value> = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let before = stored.len();
        stored.retain(|_, v| {
            v.get("received_at")
                .and_then(|t| serde_json::from_value::<DateTime<Utc>>(t.clone()).ok())
                .is_none_or(|at| at >= cutoff)
        });
        if stored.len() != before {
            pruned.rows += before - stored.len();
            let written = serde_json::to_vec_pretty(&stored)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    let tmp = dir.join(format!("{ROWS_FILE}.{}.tmp", util::short_id()));
                    std::fs::write(&tmp, bytes)?;
                    std::fs::rename(&tmp, &file)
                });
            if let Err(e) = written {
                eprintln!("fleet history: could not prune {}: {e}", file.display());
                continue;
            }
        }
        let referenced: BTreeSet<String> = stored
            .values()
            .filter_map(|v| v.get("payloads").and_then(Value::as_array))
            .flatten()
            .filter_map(|p| p.get("sha256").and_then(Value::as_str).map(str::to_string))
            .collect();
        let payloads = dir.join("payloads");
        let mut left = 0usize;
        for p in std::fs::read_dir(&payloads).into_iter().flatten().flatten() {
            let name = p.file_name().to_string_lossy().into_owned();
            let old = p
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| t < cutoff_time)
                .unwrap_or(false);
            if !referenced.contains(&name) && old && std::fs::remove_file(p.path()).is_ok() {
                pruned.payloads += 1;
            } else {
                left += 1;
            }
        }
        if stored.is_empty() && left == 0 {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    pruned
}

/// The reclaim tick's call: one prune at the configured retention, logged when it removed anything.
pub(crate) async fn prune_tick(app: &Shared) {
    let pruned = prune(&app.cfg.data_dir, retention_days(), Utc::now()).await;
    if pruned != Pruned::default() {
        eprintln!(
            "fleet history: retention removed {} rows and {} payloads",
            pruned.rows, pruned.payloads
        );
    }
}

/// The owner's history routes. Owner-only: `api_tokens::classify` names none of them.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/fleet/history", get(list))
        .route("/api/fleet/history/{member}/{row_id}", get(detail))
        .route("/api/fleet/history/{member}/{row_id}/logs/{name}", get(log))
}

#[cfg(test)]
mod tests;
