//! The send queue: which colonies go to the brain, when, and what became of each.
//!
//! **Triggers.** A sweep runs every minute, and at once whenever a notification goes out
//! (notify.rs) or the settings change. It looks at every colony of a repository whose rule sends,
//! in a state where a turn has ended — `idle`, `pr_opened`, `merged`, `closed`, `no_changes`,
//! `stopped`, `failed` — and active since the rule was switched on, or sent before. A colony whose
//! fingerprint (the record's schema and collector versions, its status, pull request, cost and the
//! size of its event logs) differs from the one last sent is queued — so an upgrade that changes
//! the record re-sends every colony already sent, a batch per pass like any other. Older colonies
//! never sent wait for the backfill button, which queues them on request.
//!
//! **Sending.** One colony at a time, a pause between each (the rate limit), and never more than a
//! batch per pass: the record and the note are built at send time from the colony as it is then,
//! so a colony queued twice is sent once. A record whose bytes match the last one sent is marked
//! sent without a request. The brain dedupes the source on `(scope, sha256(body))` and the note on
//! its slug, so a retry after a lost answer stores nothing twice. A transport failure, a 429 or a
//! 5xx backs off (30 s doubling to an hour) and gives up after eight tries; a refusal the same
//! request would get again fails at once; a 401 stops the queue and asks for a reconnect.
//!
//! **Persistence.** `<config>/living-brain-queue.json` (0600, written atomically): one entry per
//! colony ever queued, so a restart loses nothing and the cockpit can say what happened. It names
//! colonies and repositories and never holds a record, a note or the token.

use super::{
    Saved,
    client::{Client, LbError},
    load, record, token,
};
use crate::{ApiResult, App, Shared, client_error, sessions::SessionStatus, util};
use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path as FsPath, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

/// The sweep's period when nothing wakes it.
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// The pause between two colonies' sends — the rate limit, at most 30 colonies a minute.
const SEND_GAP: Duration = Duration::from_secs(2);
/// The most colonies one pass sends; the rest wait for the next.
const BATCH: usize = 20;
/// Backoff for a retryable failure: 30 s doubling to an hour; failed after this many tries.
const FIRST_BACKOFF_SECS: i64 = 30;
const MAX_BACKOFF_SECS: i64 = 3600;
const MAX_ATTEMPTS: u32 = 8;
/// Entries kept: the oldest sent ones are pruned past this.
const MAX_ENTRIES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryState {
    Pending,
    Sent,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) repo: String,
    pub(crate) state: EntryState,
    /// Why it was queued last: the colony's status word, or `backfill`.
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) queued_at: Option<String>,
    #[serde(default)]
    pub(crate) attempts: u32,
    /// Unix seconds: not before.
    #[serde(default)]
    pub(crate) next_at: i64,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    /// The fingerprint it was queued at, and the one last sent.
    #[serde(default)]
    pub(crate) pending_fp: Option<String>,
    #[serde(default)]
    pub(crate) fingerprint: Option<String>,
    /// SHA-256 of the last record and note sent, so an unchanged colony costs no request.
    #[serde(default)]
    pub(crate) body_sha: Option<String>,
    #[serde(default)]
    pub(crate) note_sha: Option<String>,
    #[serde(default)]
    pub(crate) source_id: Option<String>,
    #[serde(default)]
    pub(crate) note_url: Option<String>,
    #[serde(default)]
    pub(crate) sent_at: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Queue {
    #[serde(default)]
    pub(crate) entries: BTreeMap<String, Entry>,
    /// RFC3339: the last colony the brain accepted.
    #[serde(default)]
    pub(crate) last_sync: Option<String>,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
}

pub(crate) fn queue_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("living-brain-queue.json")
}

/// Every read-modify-write of the queue file. Never held across an await.
static STORE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
/// One pass at a time.
static PASS: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Asks for a pass now (a notification went out, a setting changed). Cheap; never blocks.
pub(crate) fn wake() {
    WAKE.notify_one();
}

/// 0600 temp beside the target, then a rename: a reader never sees half a file.
pub(crate) fn write_private_atomic(path: &FsPath, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        util::short_id()
    ));
    if let Err(e) = util::write_private(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

pub(crate) fn load_queue(config_dir: &FsPath) -> Queue {
    match std::fs::read(queue_file(config_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "living-brain: the queue file could not be parsed; starting an empty one");
            Queue::default()
        }),
        Err(_) => Queue::default(),
    }
}

fn save_queue(config_dir: &FsPath, queue: &Queue) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    write_private_atomic(&queue_file(config_dir), &serde_json::to_vec_pretty(queue)?)
}

/// Load, change, prune, save — under [`STORE`].
pub(crate) fn with_queue<T>(config_dir: &FsPath, change: impl FnOnce(&mut Queue) -> T) -> Result<T> {
    let _lock = STORE.lock().expect("the Living Brain queue lock");
    let mut queue = load_queue(config_dir);
    let out = change(&mut queue);
    prune(&mut queue);
    save_queue(config_dir, &queue)?;
    Ok(out)
}

fn prune(queue: &mut Queue) {
    if queue.entries.len() <= MAX_ENTRIES {
        return;
    }
    let mut sent: Vec<(String, String)> = queue
        .entries
        .iter()
        .filter(|(_, e)| e.state == EntryState::Sent)
        .map(|(id, e)| (e.sent_at.clone().unwrap_or_default(), id.clone()))
        .collect();
    sent.sort();
    let over = queue.entries.len() - MAX_ENTRIES;
    for (_, id) in sent.into_iter().take(over) {
        queue.entries.remove(&id);
    }
}

/// Empties the queue: on disconnect, nothing about this account stays behind.
pub(crate) fn clear(config_dir: &FsPath) -> Result<()> {
    let _lock = STORE.lock().expect("the Living Brain queue lock");
    save_queue(config_dir, &Queue::default())
}

/// The states a colony is left in when its run is over or waiting — the collector's settled set.
pub(crate) fn sendable(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Idle
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Parked
            | SessionStatus::PrOpened
            | SessionStatus::Merged
            | SessionStatus::Closed
            | SessionStatus::NoChanges
            | SessionStatus::Stopped
            | SessionStatus::Failed
    )
}

/// How long the event logs must be quiet before a colony is sent: a colony in a settled state can
/// still be mid-write (a resume, a late subagent), and half a run is not worth a version.
pub(crate) const QUIET: Duration = Duration::from_secs(30 * 60);

/// Settled: a settled status, and nothing written to the event logs for [`QUIET`].
pub(crate) fn settled(status: SessionStatus, last_write: Option<std::time::SystemTime>, now: std::time::SystemTime) -> bool {
    sendable(status) && last_write.is_none_or(|last| now.duration_since(last).is_ok_and(|quiet| quiet >= QUIET))
}

fn status_word(status: SessionStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The colony as far as the brain could tell a difference: status, pull request, cost and how much
/// its event logs hold. Cheap — one stat per log — and stable while nothing happens.
async fn fingerprint(app: &App, s: &crate::sessions::Session) -> (String, Option<std::time::SystemTime>) {
    let mut events = 0u64;
    let mut last_write = None;
    if let Ok(files) = app.store().list_files(&s.id).await {
        for name in files.iter().filter(|n| is_event_log(n)) {
            if let Ok(Some(stat)) = app.store().stat(&s.id, name).await {
                events += stat.len;
                last_write = last_write.max(stat.modified);
            }
        }
    }
    let fp = fingerprint_text(
        &status_word(s.status),
        s.pr_url.as_deref().unwrap_or(""),
        s.cost_usd.unwrap_or(0.0) + s.routed_cost_usd.unwrap_or(0.0),
        events,
    );
    (fp, last_write)
}

/// The fingerprint's text. The record's schema and collector versions lead it: a colony sent before
/// an upgrade that changed what a record carries (schema 3 nested its subagents) differs from its
/// last-sent fingerprint once, and is queued and sent again in the new shape.
pub(crate) fn fingerprint_text(status: &str, pr_url: &str, cost: f64, events: u64) -> String {
    format!(
        "v{}.{}|{status}|{pr_url}|{cost:.6}|{events}",
        record::SCHEMA_VERSION,
        record::COLLECTOR_VERSION,
    )
}

/// Whether the sweep looks at a colony of a sending repository: active since the rule went on, or
/// sent before — a colony the backfill sent is older than the rule, and must still be re-sent when
/// an upgrade changes its record.
pub(crate) fn sweep_candidate(rule_since: Option<&str>, updated_at: DateTime<Utc>, sent_before: bool) -> bool {
    sent_before || since_ok(rule_since, updated_at)
}

fn is_event_log(name: &str) -> bool {
    !name.contains('/') && name.starts_with("events") && name.ends_with(".jsonl")
}

pub(crate) fn since_ok(rule_since: Option<&str>, updated_at: DateTime<Utc>) -> bool {
    match rule_since.and_then(|s| DateTime::parse_from_rfc3339(s).ok()) {
        Some(since) => updated_at >= since,
        None => false,
    }
}

/// Queues `id` unless it is already pending or was sent at this fingerprint.
pub(crate) fn enqueue(queue: &mut Queue, id: &str, repo: &str, reason: &str, fp: &str, now: DateTime<Utc>) -> bool {
    match queue.entries.get_mut(id) {
        Some(e) if e.state == EntryState::Pending => {
            e.pending_fp = Some(fp.to_owned());
            false
        }
        Some(e) if e.state == EntryState::Sent && e.fingerprint.as_deref() == Some(fp) => false,
        // A failed entry is retried only when the colony changed since, or by the retry button.
        Some(e) if e.state == EntryState::Failed && e.pending_fp.as_deref() == Some(fp) => false,
        Some(e) => {
            e.state = EntryState::Pending;
            e.reason = reason.to_owned();
            e.queued_at = Some(now.to_rfc3339());
            e.attempts = 0;
            e.next_at = 0;
            e.last_error = None;
            e.pending_fp = Some(fp.to_owned());
            true
        }
        None => {
            queue.entries.insert(
                id.to_owned(),
                Entry {
                    repo: repo.to_owned(),
                    state: EntryState::Pending,
                    reason: reason.to_owned(),
                    queued_at: Some(now.to_rfc3339()),
                    attempts: 0,
                    next_at: 0,
                    last_error: None,
                    pending_fp: Some(fp.to_owned()),
                    fingerprint: None,
                    body_sha: None,
                    note_sha: None,
                    source_id: None,
                    note_url: None,
                    sent_at: None,
                },
            );
            true
        }
    }
}

/// The colonies this pass should queue: of a sending repository, ended a turn, active since the
/// rule went on, changed since last sent.
async fn sweep(app: &App, saved: &Saved) {
    if !saved.rules.iter().any(|r| r.send) {
        return;
    }
    let sent: std::collections::BTreeSet<String> = load_queue(&app.cfg.config_dir)
        .entries
        .into_iter()
        .filter(|(_, e)| e.state == EntryState::Sent)
        .map(|(id, _)| id)
        .collect();
    let candidates: Vec<crate::sessions::Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| sendable(s.status))
        .filter(|s| {
            saved
                .rule_for(&s.repo)
                .is_some_and(|r| r.send && sweep_candidate(r.since.as_deref(), s.updated_at, sent.contains(&s.id)))
        })
        .cloned()
        .collect();
    let mut found = Vec::with_capacity(candidates.len());
    let wall = std::time::SystemTime::now();
    for s in &candidates {
        let (fp, last_write) = fingerprint(app, s).await;
        if settled(s.status, last_write, wall) {
            found.push((s.id.clone(), s.repo.clone(), status_word(s.status), fp));
        }
    }
    let now = Utc::now();
    if let Err(e) = with_queue(&app.cfg.config_dir, |q| {
        for (id, repo, reason, fp) in &found {
            enqueue(q, id, repo, reason, fp, now);
        }
    }) {
        tracing::error!(error = %format!("{e:#}"), "living-brain: the queue could not be saved");
    }
}

fn sha256(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The colony's events, archives first, each file deduplicated by `seq`.
async fn read_events(app: &App, id: &str) -> Vec<Value> {
    let names: Vec<String> = app
        .store()
        .list_files(id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|n| is_event_log(n))
        .collect();
    let mut events = Vec::new();
    for name in record::order_event_files(&names) {
        if let Ok(Some(bytes)) = app.store().read_file(id, &name).await {
            events.extend(record::parse_events(&bytes));
        }
    }
    events
}

/// What one send ended as.
enum Outcome {
    Sent {
        body_sha: String,
        note_sha: String,
        source_id: Option<String>,
        note_url: Option<String>,
    },
    Unchanged,
    Dropped,
    Failed(LbError),
}

async fn send_one(app: &App, saved: &Saved, client: &Client, id: &str, entry: &Entry) -> Outcome {
    let Some(session) = app.sessions.read().await.iter().find(|s| s.id == id).cloned() else {
        return Outcome::Dropped; // the colony is gone
    };
    let Some(rule) = saved.rule_for(&session.repo).filter(|r| r.send) else {
        return Outcome::Dropped; // switched off since it was queued
    };
    let events = read_events(app, id).await;
    let scrub = record::Scrubber::new(std::env::var("HOME").ok(), crate::secrets::saved_values(app));
    let Some(collected) = record::collect(&record::row(&session), &events) else {
        return Outcome::Dropped; // a row with no repository
    };
    let (built, body) = record::prepare(collected, rule.detail, &scrub);
    let body_sha = sha256(&body);
    let project = record::project_of(&built.repo);
    // The note names the source it summarises; for an unchanged body that is the id we hold.
    if entry.body_sha.as_deref() == Some(body_sha.as_str()) && entry.note_url.is_some() {
        let note = super::note::body(&built, entry.source_id.as_deref(), &scrub);
        if entry.note_sha.as_deref() == Some(sha256(&note).as_str()) {
            return Outcome::Unchanged;
        }
    }
    let source = match client
        .post_source(&record::source_path(&built), &body, project.as_deref())
        .await
    {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(e),
    };
    let source_id = source.get("id").and_then(Value::as_str).map(str::to_owned);
    let note = super::note::body(&built, source_id.as_deref(), &scrub);
    let title = record::source_path(&built);
    match client
        .keyed_note(&record::note_slug(id), &title, &note, project.as_deref())
        .await
    {
        Ok(v) => Outcome::Sent {
            body_sha,
            note_sha: sha256(&note),
            source_id,
            note_url: v.get("url").and_then(Value::as_str).map(str::to_owned),
        },
        Err(e) => Outcome::Failed(e),
    }
}

pub(crate) fn backoff(attempts: u32) -> i64 {
    (FIRST_BACKOFF_SECS << attempts.saturating_sub(1).min(20)).min(MAX_BACKOFF_SECS)
}

/// One pass: sweep, then send what is due. Answers how many colonies the brain accepted.
pub(crate) async fn pass(app: &App) -> usize {
    let _pass = PASS.lock().await;
    let saved = load(&app.cfg.config_dir);
    let Some(token) = token(&app.cfg.config_dir) else { return 0 };
    if saved.auth_error.is_some() {
        return 0; // a revoked token: nothing goes until the person reconnects
    }
    sweep(app, &saved).await;
    let now = Utc::now().timestamp();
    let due: Vec<(String, Entry)> = {
        let _lock = STORE.lock().expect("the Living Brain queue lock");
        let mut due: Vec<(String, Entry)> = load_queue(&app.cfg.config_dir)
            .entries
            .into_iter()
            .filter(|(_, e)| e.state == EntryState::Pending && e.next_at <= now)
            .collect();
        due.sort_by(|a, b| a.1.queued_at.cmp(&b.1.queued_at));
        due.truncate(BATCH);
        due
    };
    let client = Client::new(&saved.server_url, Some(token));
    let mut accepted = 0;
    for (i, (id, entry)) in due.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(SEND_GAP).await;
        }
        let outcome = send_one(app, &saved, &client, id, entry).await;
        let stamp = Utc::now();
        let mut stop = false;
        let result = with_queue(&app.cfg.config_dir, |q| {
            let Some(e) = q.entries.get_mut(id) else { return };
            if e.state != EntryState::Pending {
                return; // cleared or retried while this was in flight
            }
            match outcome {
                Outcome::Dropped => {
                    q.entries.remove(id);
                }
                Outcome::Unchanged => {
                    e.state = EntryState::Sent;
                    e.fingerprint = e.pending_fp.clone();
                    e.last_error = None;
                }
                Outcome::Sent {
                    body_sha,
                    note_sha,
                    source_id,
                    note_url,
                } => {
                    e.state = EntryState::Sent;
                    e.fingerprint = e.pending_fp.clone();
                    e.body_sha = Some(body_sha);
                    e.note_sha = Some(note_sha);
                    e.source_id = source_id.or(e.source_id.take());
                    e.note_url = note_url.or(e.note_url.take());
                    e.sent_at = Some(stamp.to_rfc3339());
                    e.last_error = None;
                    e.attempts = 0;
                    q.last_sync = Some(stamp.to_rfc3339());
                    q.last_error = None;
                    accepted += 1;
                }
                Outcome::Failed(err) => {
                    e.attempts += 1;
                    e.last_error = Some(err.message.clone());
                    q.last_error = Some(err.message.clone());
                    if err.unauthorized() {
                        stop = true; // stays pending; the reconnect sends it
                    } else if err.permanent() || e.attempts >= MAX_ATTEMPTS {
                        e.state = EntryState::Failed;
                    } else {
                        e.next_at = stamp.timestamp() + backoff(e.attempts);
                    }
                }
            }
        });
        if let Err(e) = result {
            tracing::error!(error = %format!("{e:#}"), "living-brain: the queue could not be saved");
        }
        if stop {
            let _ = super::update(&app.cfg.config_dir, |s| {
                s.auth_error = Some("Living Brain refused the saved token (401); reconnect in Settings → Living Brain".into());
            });
            break;
        }
    }
    accepted
}

/// The background loop: a pass every minute, and one at once on every [`wake`].
pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = WAKE.notified() => {}
                () = tokio::time::sleep(SWEEP_EVERY) => {}
            }
            pass(&app).await;
        }
    });
}

/// The queue as the cockpit shows it: counts, the last sync and error, and the latest entries.
pub(crate) fn summary(config_dir: &FsPath) -> Value {
    let queue = {
        let _lock = STORE.lock().expect("the Living Brain queue lock");
        load_queue(config_dir)
    };
    let count = |state| queue.entries.values().filter(|e| e.state == state).count();
    let mut recent: Vec<(&String, &Entry)> = queue.entries.iter().collect();
    let when = |e: &Entry| e.sent_at.clone().max(e.queued_at.clone()).unwrap_or_default();
    recent.sort_by_key(|(_, e)| std::cmp::Reverse(when(e)));
    let items: Vec<Value> = recent
        .into_iter()
        .take(50)
        .map(|(id, e)| {
            json!({
                "colony": id,
                "repo": e.repo,
                "state": e.state,
                "reason": e.reason,
                "attempts": e.attempts,
                "last_error": e.last_error,
                "queued_at": e.queued_at,
                "sent_at": e.sent_at,
                "note_url": e.note_url,
            })
        })
        .collect();
    json!({
        "pending": count(EntryState::Pending),
        "sent": count(EntryState::Sent),
        "failed": count(EntryState::Failed),
        "last_sync": queue.last_sync,
        "last_error": queue.last_error,
        "items": items,
    })
}

/// `POST /api/living-brain/queue/retry`: every failed colony back to pending.
pub async fn retry(State(app): State<Shared>) -> ApiResult<Value> {
    let now = Utc::now().to_rfc3339();
    let n = with_queue(&app.cfg.config_dir, |q| {
        let mut n = 0;
        for e in q.entries.values_mut().filter(|e| e.state == EntryState::Failed) {
            e.state = EntryState::Pending;
            e.attempts = 0;
            e.next_at = 0;
            e.queued_at = Some(now.clone());
            n += 1;
        }
        n
    })?;
    wake();
    Ok(Json(json!({ "retried": n, "queue": summary(&app.cfg.config_dir) })))
}

#[derive(Deserialize)]
pub struct BackfillRequest {
    org: String,
    #[serde(default)]
    dry_run: bool,
}

/// `POST /api/living-brain/backfill {"org", "dry_run"}`: the org's existing colonies — whatever
/// their age — in the repositories whose rule sends. A dry run only counts; the real one queues.
pub async fn backfill(State(app): State<Shared>, Json(body): Json<BackfillRequest>) -> ApiResult<Value> {
    let saved = load(&app.cfg.config_dir);
    if token(&app.cfg.config_dir).is_none() {
        return Err(client_error(StatusCode::CONFLICT, "connect Living Brain first"));
    }
    let org = body.org.trim();
    if org.is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "name the org"));
    }
    let in_org: Vec<crate::sessions::Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.org.eq_ignore_ascii_case(org) || s.repo.split('/').next().is_some_and(|o| o.eq_ignore_ascii_case(org)))
        .cloned()
        .collect();
    let mut opted_out = 0usize;
    let mut not_ended = 0usize;
    let mut found = Vec::new();
    for s in &in_org {
        if !saved.rule_for(&s.repo).is_some_and(|r| r.send) {
            opted_out += 1;
        } else {
            let (fp, last_write) = fingerprint(&app, s).await;
            if settled(s.status, last_write, std::time::SystemTime::now()) {
                found.push((s.id.clone(), s.repo.clone(), fp));
            } else {
                not_ended += 1;
            }
        }
    }
    let queue = load_queue(&app.cfg.config_dir);
    let already = found
        .iter()
        .filter(|(id, _, fp)| {
            queue
                .entries
                .get(id)
                .is_some_and(|e| e.state == EntryState::Sent && e.fingerprint.as_deref() == Some(fp))
        })
        .count();
    let mut queued = 0;
    if !body.dry_run {
        let now = Utc::now();
        queued = with_queue(&app.cfg.config_dir, |q| {
            found
                .iter()
                .filter(|(id, repo, fp)| enqueue(q, id, repo, "backfill", fp, now))
                .count()
        })?;
        wake();
    }
    Ok(Json(json!({
        "org": org,
        "dry_run": body.dry_run,
        "eligible": found.len(),
        "already_sent": already,
        "to_send": found.len() - already,
        "queued": queued,
        "not_opted_in": opted_out,
        "still_running": not_ended,
    })))
}

/// `GET /api/living-brain/colonies/{id}`: what became of one colony, and where its page is.
pub async fn colony(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let saved = load(&app.cfg.config_dir);
    let queue = load_queue(&app.cfg.config_dir);
    let sessions_url = format!("{}/wiki.html?view=sessions", saved.server_url.trim_end_matches('/'));
    Ok(Json(match queue.entries.get(&id) {
        Some(e) => json!({
            "state": e.state,
            "note_url": e.note_url,
            "sent_at": e.sent_at,
            "last_error": e.last_error,
            "sessions_url": sessions_url,
        }),
        None => json!({ "state": "none", "note_url": null, "sent_at": null, "last_error": null, "sessions_url": sessions_url }),
    }))
}
