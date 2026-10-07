//! Fleet history push (issue #762): a member drains its finished colonies' history to its fleet's
//! owner, over the fleet-scoped token its pairing minted (`fleet_members.rs`, #769).
//!
//! The export bundle (#687, `fleet_export.rs`) moves a machine's history by hand, as one file. This
//! is the push path beside it: a queue that drains on its own while the machine belongs to a fleet,
//! and that survives being cut off at any point.
//!
//! - **Rows** are colony records, the same allowlist projection the bundle carries
//!   ([`ImportedSession`]): one per finished colony, keyed `<origin_host>:<session id>`.
//! - **Payloads** are the colony's log ledgers, content-addressed by SHA-256. Every payload a row
//!   references is uploaded — and acknowledged — before the row is sent, so the owner never holds
//!   a row whose logs it lacks; the owner refuses such a row by name besides.
//! - A row counts as sent only once the owner acknowledges it. The drain state
//!   (`<data_dir>/fleet-sync.json`) records each acknowledged row's fingerprint, so a changed row
//!   (a pull request merged later) is sent again, and a drain killed mid-batch re-sends only what
//!   was never acknowledged — the owner upserts by row id, so a re-send is never a duplicate.
//! - Batches are capped by row count and by body bytes. A batch the owner refuses without naming a
//!   row is split in half until the bad row stands alone; a row that exhausts its attempts is
//!   retired — listed, never retried, never blocking the rows behind it.
//! - Failures are states, not errors: a 401 trades the membership's refresh credential for a fresh
//!   fleet token once per drain (`POST /api/fleet/peer/refresh`) and retries; a second 401, or a
//!   refresh the owner refuses, stops the drain and asks for attention. A 403 means this machine
//!   was removed from the fleet and stops syncing — it never refreshes, and the owner's tombstone
//!   refuses a removed member's refresh besides. A 429 or 503 waits out its `Retry-After`. Nothing
//!   local is ever deleted by any of them.
//!
//! Joining is not consent. The push is off for every new membership until the member's operator
//! has seen the preview (`GET /api/fleet/sync/preview`) and turned it on
//! (`POST /api/fleet/sync/consent`); a leave and a re-join start it off again. Until then the
//! background task sends nothing and the manual trigger refuses with `consent_required`.
//!
//! On the owner, what a member pushes lands under `<data_dir>/fleet-ingest/<member_id>/`: the
//! payloads by hash in `payloads/`, the rows in `sessions.json`, upserted by id.

use crate::fleet_export::{self, ImportedSession, LOG_BASENAMES, Origin};
use crate::repo_identity::RepoIdentity;
use crate::{ApiResult, AppError, Shared, client_error, util};
use anyhow::{Context as _, Result};
use axum::{
    Json,
    body::Bytes,
    extract::{Path as UrlPath, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

/// Rows per batch, and body bytes per batch, unless the config says otherwise.
pub const DEFAULT_MAX_ROWS: usize = 100;
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
/// What the owner accepts in one rows request: more rows or a bigger body is a 413.
pub const MAX_BATCH_ROWS: usize = 500;
pub const MAX_BATCH_BODY: usize = 4 * 1024 * 1024;
/// The largest payload that travels. A bigger log is listed on its row as `omitted`.
pub const MAX_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;
/// How many drains a refused row gets before it is retired.
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// How often the background task drains while this machine belongs to a fleet.
const DRAIN_INTERVAL: Duration = Duration::from_secs(300);
/// How long after startup the first drain waits, so it does not compete with recovery.
const FIRST_DRAIN_DELAY: Duration = Duration::from_secs(20);
/// One request's ceiling: a payload is up to [`MAX_PAYLOAD_BYTES`] over a private network.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// A 429 or 503 with no usable `Retry-After` waits this long; a longer one is capped.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(30);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);
/// How much of an owner's answer is read.
const MAX_ANSWER: usize = 1024 * 1024;
/// The member's drain state, in the data dir.
const STATE_FILE: &str = "fleet-sync.json";
/// The owner's per-member ingest root, in its data dir.
pub(crate) const INGEST_DIR: &str = "fleet-ingest";
pub(crate) const ROWS_FILE: &str = "sessions.json";
/// `{"rows":[` and `]}` around the rows of one batch.
const BODY_OVERHEAD: usize = 11;

// ---------------------------------------------------------------------------
// The wire.
// ---------------------------------------------------------------------------

/// One log a row references: the ledger's name in the session directory, and its content hash —
/// the payload's key on the owner. `omitted` is a log too large to travel: named, never uploaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRef {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub omitted: bool,
}

/// One row on the wire: the colony record, keyed by its namespaced id, and the payloads it needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestRow {
    pub id: String,
    pub record: ImportedSession,
    #[serde(default)]
    pub payloads: Vec<PayloadRef>,
}

/// The body of `POST /api/fleet/peer/rows`.
#[derive(Deserialize)]
pub struct RowsBody {
    rows: Vec<IngestRow>,
}

/// The owner's answer to a rows batch: which rows it holds now, and which it refused, by name.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RowsAnswer {
    #[serde(default)]
    pub accepted: Vec<String>,
    #[serde(default)]
    pub rejected: Vec<RejectedRow>,
}

/// A row the owner refused by name. `missing_payloads` lists the hashes it does not hold, which
/// the member forgets having sent, so its next attempt uploads them again.
#[derive(Debug, Serialize, Deserialize)]
pub struct RejectedRow {
    pub id: String,
    pub error: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_payloads: Vec<String>,
}

// ---------------------------------------------------------------------------
// The member's drain state.
// ---------------------------------------------------------------------------

/// Where this member's history push stands — the field member health (#764) shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    /// Never drained for this membership.
    #[default]
    Idle,
    /// The last drain reached the end of the queue.
    Synced,
    /// The owner asked us to wait (429/503); `next_attempt_at` says until when.
    Backoff,
    /// The owner no longer accepts our token (401), and the one refresh this drain allowed did not
    /// help — no refresh credential, the owner refused it, or the fresh token was refused too.
    /// Needs attention: a person re-joins or checks the owner. Background drains stop; a manual one
    /// tries again.
    Unauthorized,
    /// The owner says this machine is not a member (403): removed from the fleet. Syncing stops;
    /// everything local stays.
    Removed,
    /// The owner could not be reached, or answered something unexpected; the next tick retries.
    Error,
    /// A member whose operator has not consented to the push (`POST /api/fleet/sync/consent`):
    /// nothing is sent, by the background task or on demand.
    ConsentRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attempt {
    fingerprint: String,
    count: u32,
    error: String,
}

/// A row that exhausted its attempts: kept out of the queue until the row itself changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Retired {
    pub fingerprint: String,
    pub attempts: u32,
    pub error: String,
    pub at: DateTime<Utc>,
}

/// The persisted drain state: what the owner has acknowledged, what keeps failing, and the last
/// outcome. Keyed to one membership — a different member id starts it over.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DrainState {
    #[serde(default)]
    member_id: Option<String>,
    /// Row id → the fingerprint the owner acknowledged.
    #[serde(default)]
    acked: BTreeMap<String, String>,
    /// Payload hashes the owner acknowledged.
    #[serde(default)]
    payloads: BTreeSet<String>,
    #[serde(default)]
    attempts: BTreeMap<String, Attempt>,
    #[serde(default)]
    pub retired: BTreeMap<String, Retired>,
    #[serde(default)]
    pub status: SyncStatus,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub next_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_drain_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_synced_at: Option<DateTime<Utc>>,
    /// Rows still unsent after the last drain (retired ones not counted) — member health (#764).
    #[serde(default)]
    pub backlog_rows: usize,
    /// Since when every drain has ended with rows still unsent: set by the first drain that leaves
    /// a backlog, cleared by the first that leaves none. A lower bound on the oldest row's wait.
    #[serde(default)]
    pub backlog_since: Option<DateTime<Utc>>,
}

impl DrainState {
    /// Loads the state, never failing: a missing or damaged file starts over, which costs a
    /// re-send and nothing else — the owner upserts by row id.
    pub fn load(data_dir: &Path) -> DrainState {
        let path = data_dir.join(STATE_FILE);
        match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DrainState::default(),
            Err(e) => {
                eprintln!("fleet sync: could not read {} ({e}); starting the drain over", path.display());
                DrainState::default()
            }
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("fleet sync: {} does not parse ({e}); starting the drain over", path.display());
                DrainState::default()
            }),
        }
    }

    async fn save(&self, data_dir: &Path) -> Result<()> {
        util::write_atomic(&data_dir.join(STATE_FILE), &serde_json::to_vec_pretty(self)?).await
    }

    fn skips(&self, id: &str, fingerprint: &str) -> bool {
        self.acked.get(id).is_some_and(|f| f == fingerprint) || self.retired.get(id).is_some_and(|r| r.fingerprint == fingerprint)
    }
}

// ---------------------------------------------------------------------------
// The drain.
// ---------------------------------------------------------------------------

/// Where the drain pushes: the owner, and the membership that lets us.
#[derive(Clone, Debug)]
pub struct Target {
    pub owner_url: String,
    pub member_id: String,
    pub token: String,
    /// The refresh credential the owner handed over with the token (#762): what a 401 trades for
    /// a fresh fleet token. `None` on a membership from before refresh, which re-joins instead.
    pub refresh: Option<String>,
}

/// The drain's knobs; the defaults are what the background task uses.
#[derive(Clone, Debug)]
pub struct DrainConfig {
    pub max_rows: usize,
    pub max_bytes: usize,
    pub max_attempts: u32,
    /// A `Retry-After` up to this long is waited out inside the drain; a longer one ends it in
    /// `Backoff` until then.
    pub max_inline_wait: Duration,
    /// How many times one drain waits inline before it gives up to `Backoff`.
    pub max_waits: u32,
}

impl Default for DrainConfig {
    fn default() -> Self {
        DrainConfig {
            max_rows: DEFAULT_MAX_ROWS,
            max_bytes: DEFAULT_MAX_BYTES,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_inline_wait: Duration::from_secs(60),
            max_waits: 5,
        }
    }
}

/// Time, injectable: the backoff tests run on a clock that never sleeps.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn sleep(&self, wait: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn sleep(&self, wait: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(wait))
    }
}

/// What one drain did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DrainReport {
    pub status: SyncStatus,
    pub detail: Option<String>,
    /// Rows the owner acknowledged this drain.
    pub sent: usize,
    /// Payloads the owner acknowledged this drain.
    pub payloads: usize,
    /// Rows refused this drain that still have attempts left.
    pub failed: Vec<String>,
    /// Rows retired this drain.
    pub retired: Vec<String>,
    /// Rows still waiting after this drain (failed ones included; retired ones not).
    pub pending: usize,
    /// Batches split to isolate a refused row.
    pub splits: usize,
    /// True when the drain did not run: stopped by a 401/403, or backing off.
    pub skipped: bool,
    pub next_attempt_at: Option<DateTime<Utc>>,
    /// The fresh fleet token a 401 refresh handed back this drain, for the caller to store in the
    /// membership. Never serialized: a report is what the cockpit's trigger answers.
    #[serde(skip)]
    pub refreshed_token: Option<String>,
}

/// One row waiting to be sent, with what it takes to send it.
struct Pending {
    row: IngestRow,
    fingerprint: String,
    body_bytes: usize,
    /// Non-omitted payloads: `(sha256, name, path)`.
    files: Vec<(String, String, PathBuf)>,
}

/// Every finished colony whose row the owner has not acknowledged in its current form, oldest
/// first. The fingerprint covers the record and each log's size and mtime, so an unchanged row is
/// skipped without hashing its logs.
fn collect(data_dir: &Path, origin: &Origin, state: &DrainState) -> Result<Vec<Pending>> {
    let mut sessions = fleet_export::read_sessions(data_dir)?;
    sessions.retain(|s| s.status.is_terminal() && fleet_export::is_safe_segment(&s.id));
    sessions.sort_by(|a, b| a.updated_at.cmp(&b.updated_at).then_with(|| a.id.cmp(&b.id)));
    let mut out = Vec::new();
    let mut identities: BTreeMap<&str, Option<RepoIdentity>> = BTreeMap::new();
    for s in &sessions {
        // The record carries the repo's fleet identity (#763), read from this machine's mirror —
        // the same one `fleet_export::collect` stamps on the rows a bundle carries.
        let identity = identities
            .entry(s.repo.as_str())
            .or_insert_with(|| fleet_export::mirror_identity(data_dir, &s.repo))
            .clone();
        let record = ImportedSession::of(origin, s, identity);
        let dir = crate::store::local_session_dir(data_dir, &s.id);
        let mut logs = Vec::new();
        let mut seed = serde_json::to_vec(&record)?;
        for name in LOG_BASENAMES {
            let path = dir.join(name);
            // `symlink_metadata`: a link planted in the session directory is not followed.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.file_type().is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            seed.extend_from_slice(format!("\0{name}\0{}\0{mtime}", meta.len()).as_bytes());
            logs.push((name, path, meta.len()));
        }
        let fingerprint = sha256_hex(&seed);
        if state.skips(&record.id, &fingerprint) {
            continue;
        }
        let mut payloads = Vec::new();
        let mut files = Vec::new();
        for (name, path, len) in logs {
            // What travels is the redacted log (#761), so the hash, the size and the cap are its.
            let bytes = if len > MAX_PAYLOAD_BYTES {
                None
            } else {
                let Ok(bytes) = outgoing_log(name, &path) else { continue }; // gone since the stat: not referenced
                Some(bytes).filter(|b| b.len() as u64 <= MAX_PAYLOAD_BYTES)
            };
            let Some(bytes) = bytes else {
                payloads.push(PayloadRef {
                    name: name.to_string(),
                    sha256: String::new(),
                    bytes: len,
                    omitted: true,
                });
                continue;
            };
            let sha = sha256_hex(&bytes);
            payloads.push(PayloadRef {
                name: name.to_string(),
                sha256: sha.clone(),
                bytes: bytes.len() as u64,
                omitted: false,
            });
            files.push((sha, name.to_string(), path));
        }
        let row = IngestRow {
            id: record.id.clone(),
            record,
            payloads,
        };
        let body_bytes = serde_json::to_vec(&row)?.len();
        out.push(Pending {
            row,
            fingerprint,
            body_bytes,
            files,
        });
    }
    Ok(out)
}

/// Groups rows into batches of at most `max_rows` rows and `max_bytes` body bytes. A row larger
/// than `max_bytes` by itself travels alone — the owner's own limit is the backstop.
fn batches(sizes: &[usize], max_rows: usize, max_bytes: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut bytes = BODY_OVERHEAD;
    for (at, size) in sizes.iter().enumerate() {
        let add = size + usize::from(!current.is_empty());
        if !current.is_empty() && (current.len() >= max_rows.max(1) || bytes + add > max_bytes) {
            out.push(std::mem::take(&mut current));
            bytes = BODY_OVERHEAD;
        }
        bytes += size + usize::from(!current.is_empty());
        current.push(at);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Why a drain stopped early.
enum Stop {
    Unauthorized(String),
    Removed(String),
    Backoff(DateTime<Utc>, String),
    Error(String),
}

/// An owner's answer the drain continues after: success, or a refusal of what was sent.
enum Answer {
    Success(Vec<u8>),
    Refused { status: u16, body: Vec<u8> },
}

/// Whether a row's payloads are all on the owner.
enum Prepared {
    Ready,
    /// A log changed between the scan and the upload: the next drain picks the new one up.
    Changed,
    Failed(String),
}

struct Drainer<'a> {
    data_dir: &'a Path,
    target: &'a Target,
    cfg: &'a DrainConfig,
    clock: &'a dyn Clock,
    http: reqwest::Client,
    state: DrainState,
    report: DrainReport,
    waits: u32,
    /// The fleet token in use: the membership's, until a refresh replaces it.
    token: String,
    /// Whether this drain has spent its one refresh.
    refreshed: bool,
}

impl Drainer<'_> {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.target.owner_url.trim_end_matches('/'))
    }

    /// One request, with 429/503 waited out when the wait is short. The builder is called again
    /// for each try.
    async fn call(&mut self, build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder) -> Result<Answer, Stop> {
        loop {
            let res = build(&self.http)
                .bearer_auth(&self.token)
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|e| Stop::Error(format!("the owner could not be reached: {e}")))?;
            let status = res.status().as_u16();
            match status {
                200..=299 => return Ok(Answer::Success(read_bounded(res).await?)),
                401 if !self.refreshed => {
                    // Once per drain: trade the refresh credential for a fresh token and retry.
                    self.refreshed = true;
                    self.token = self.refresh().await?;
                    self.report.refreshed_token = Some(self.token.clone());
                    continue;
                }
                401 => {
                    return Err(Stop::Unauthorized(
                        "the owner refused this member's refreshed fleet token too; re-join the fleet, or check the owner".into(),
                    ));
                }
                403 => {
                    return Err(Stop::Removed(
                        "the owner says this machine is not a member of its fleet".into(),
                    ));
                }
                429 | 503 => {
                    let now = self.clock.now();
                    let wait = retry_after(res.headers(), now);
                    if wait <= self.cfg.max_inline_wait && self.waits < self.cfg.max_waits {
                        self.waits += 1;
                        self.clock.sleep(wait).await;
                        continue;
                    }
                    let until = now + chrono::Duration::from_std(wait).unwrap_or_default();
                    return Err(Stop::Backoff(
                        until,
                        format!("the owner answered {status}; waiting until {until}"),
                    ));
                }
                400 | 409 | 413 | 422 => {
                    return Ok(Answer::Refused {
                        status,
                        body: read_bounded(res).await.unwrap_or_default(),
                    });
                }
                _ => return Err(Stop::Error(format!("the owner answered {status}"))),
            }
        }
    }

    /// The one refresh a drain allows (#762): the membership's refresh credential for a fresh fleet
    /// token. A 403 is a removal — the owner keeps a removed member's tombstone, and never
    /// refreshes it — so it stops as removed; any other refusal, or no credential at all, stops as
    /// unauthorized. An owner that cannot be reached is an ordinary error the next tick retries.
    async fn refresh(&mut self) -> Result<String, Stop> {
        let Some(secret) = self.target.refresh.clone() else {
            return Err(Stop::Unauthorized(
                "the owner no longer accepts this member's fleet token, and this membership has no refresh credential; re-join the fleet, or check the owner".into(),
            ));
        };
        let url = self.url("/api/fleet/peer/refresh");
        let res = self
            .http
            .post(&url)
            .json(&json!({"member_id": self.target.member_id, "refresh": secret}))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| Stop::Error(format!("the owner could not be reached to refresh the fleet token: {e}")))?;
        let status = res.status().as_u16();
        let body = read_bounded(res).await.unwrap_or_default();
        match status {
            200..=299 => serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v["token"].as_str().map(str::to_string))
                .filter(|token| !token.is_empty())
                .ok_or_else(|| Stop::Unauthorized("the owner's token refresh named no token; re-join the fleet".into())),
            403 => Err(Stop::Removed(format!(
                "the owner refused to refresh the fleet token: {}",
                error_of(&body)
            ))),
            _ => Err(Stop::Unauthorized(format!(
                "the owner no longer accepts this member's fleet token and refused its refresh ({status}): {}; re-join the fleet, or check the owner",
                error_of(&body)
            ))),
        }
    }

    /// Uploads the row's payloads the owner does not yet hold — each acknowledged before the row
    /// that references it is ever sent.
    async fn prepare(&mut self, p: &Pending) -> Result<Prepared, Stop> {
        for (sha, name, path) in &p.files {
            if self.state.payloads.contains(sha) {
                continue;
            }
            let bytes = match tokio::fs::read(path).await {
                // The same redaction `collect` hashed: a log is never uploaded as written (#761).
                Ok(bytes) => crate::archive::redact_for_bundle(name, bytes),
                Err(_) => return Ok(Prepared::Changed),
            };
            if sha256_hex(&bytes) != *sha {
                return Ok(Prepared::Changed);
            }
            let url = self.url(&format!("/api/fleet/peer/payloads/{sha}"));
            match self.call(|c| c.put(&url).body(bytes.clone())).await? {
                Answer::Success(_) => {
                    self.state.payloads.insert(sha.clone());
                    self.report.payloads += 1;
                }
                Answer::Refused { status, body } => {
                    return Ok(Prepared::Failed(format!(
                        "payload {name} refused ({status}): {}",
                        error_of(&body)
                    )));
                }
            }
        }
        Ok(Prepared::Ready)
    }

    fn ack(&mut self, p: &Pending) {
        self.state.acked.insert(p.row.id.clone(), p.fingerprint.clone());
        self.state.attempts.remove(&p.row.id);
        self.state.retired.remove(&p.row.id);
        self.report.sent += 1;
    }

    /// Counts a refusal against the row; the last allowed one retires it.
    fn fail(&mut self, p: &Pending, error: String) {
        let id = p.row.id.clone();
        let count = match self.state.attempts.get(&id) {
            Some(a) if a.fingerprint == p.fingerprint => a.count + 1,
            _ => 1,
        };
        if count >= self.cfg.max_attempts.max(1) {
            self.state.attempts.remove(&id);
            self.state.retired.insert(
                id.clone(),
                Retired {
                    fingerprint: p.fingerprint.clone(),
                    attempts: count,
                    error,
                    at: self.clock.now(),
                },
            );
            self.report.retired.push(id);
        } else {
            self.state.attempts.insert(
                id.clone(),
                Attempt {
                    fingerprint: p.fingerprint.clone(),
                    count,
                    error,
                },
            );
            self.report.failed.push(id);
        }
    }

    /// Sends one batch: payloads first, then the rows. A refusal that names no row splits the
    /// batch in halves until the row that causes it stands alone.
    async fn send(&mut self, pending: &[Pending], batch: Vec<usize>) -> Result<(), Stop> {
        let url = self.url("/api/fleet/peer/rows");
        let mut work = vec![batch];
        while let Some(slice) = work.pop() {
            let mut ready = Vec::new();
            for at in slice {
                match self.prepare(&pending[at]).await? {
                    Prepared::Ready => ready.push(at),
                    Prepared::Changed => {}
                    Prepared::Failed(error) => self.fail(&pending[at], error),
                }
            }
            if ready.is_empty() {
                continue;
            }
            let rows: Vec<&IngestRow> = ready.iter().map(|at| &pending[*at].row).collect();
            let body = serde_json::to_vec(&json!({ "rows": rows })).map_err(|e| Stop::Error(e.to_string()))?;
            let answer = self
                .call(|c| {
                    c.post(&url)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body.clone())
                })
                .await;
            match answer {
                Ok(Answer::Success(bytes)) => {
                    let answer: RowsAnswer = serde_json::from_slice(&bytes)
                        .map_err(|_| Stop::Error("the owner's answer to a batch was not the expected JSON".into()))?;
                    let accepted: BTreeSet<&str> = answer.accepted.iter().map(String::as_str).collect();
                    for at in &ready {
                        if accepted.contains(pending[*at].row.id.as_str()) {
                            self.ack(&pending[*at]);
                        }
                    }
                    for refused in &answer.rejected {
                        let Some(at) = ready.iter().find(|at| pending[**at].row.id == refused.id) else {
                            continue;
                        };
                        for sha in &refused.missing_payloads {
                            self.state.payloads.remove(sha);
                        }
                        self.fail(&pending[*at], refused.error.clone());
                    }
                }
                Ok(Answer::Refused { status, body }) => {
                    let error = format!("the owner refused the batch ({status}): {}", error_of(&body));
                    let named = serde_json::from_slice::<Value>(&body)
                        .ok()
                        .and_then(|v| v["row"].as_str().map(str::to_string))
                        .and_then(|id| ready.iter().position(|at| pending[*at].row.id == id));
                    if let Some(pos) = named {
                        let at = ready.remove(pos);
                        self.fail(&pending[at], error);
                        if !ready.is_empty() {
                            work.push(ready);
                        }
                    } else if ready.len() == 1 {
                        self.fail(&pending[ready[0]], error);
                    } else {
                        let second = ready.split_off(ready.len() / 2);
                        work.push(second);
                        work.push(ready);
                        self.report.splits += 1;
                    }
                }
                Err(stop) => {
                    self.save().await;
                    return Err(stop);
                }
            }
            // Every acknowledgement is on disk before the next batch leaves: a kill from here on
            // re-sends nothing the owner already confirmed.
            self.save().await;
        }
        Ok(())
    }

    async fn save(&self) {
        if let Err(e) = self.state.save(self.data_dir).await {
            eprintln!("fleet sync: could not save the drain state: {e:#}");
        }
    }
}

/// One drain of this member's history to `target`. `force` runs it even when the last one
/// stopped on a 401/403 or is backing off — the manual trigger.
pub async fn drain(
    data_dir: &Path,
    origin: &Origin,
    target: &Target,
    cfg: &DrainConfig,
    clock: &dyn Clock,
    force: bool,
) -> Result<DrainReport> {
    let mut state = DrainState::load(data_dir);
    if state.member_id.as_deref() != Some(target.member_id.as_str()) {
        // A new membership, or the first drain: nothing the last owner acknowledged counts here.
        state = DrainState {
            member_id: Some(target.member_id.clone()),
            ..DrainState::default()
        };
    }
    let now = clock.now();
    let stopped = matches!(state.status, SyncStatus::Unauthorized | SyncStatus::Removed);
    let waiting = state.next_attempt_at.is_some_and(|at| at > now);
    if !force && (stopped || waiting) {
        return Ok(DrainReport {
            status: state.status,
            detail: state.detail.clone(),
            skipped: true,
            next_attempt_at: state.next_attempt_at,
            ..DrainReport::default()
        });
    }
    let pending = {
        let (data_dir, origin, snapshot) = (data_dir.to_path_buf(), origin.clone(), state.clone());
        tokio::task::spawn_blocking(move || collect(&data_dir, &origin, &snapshot))
            .await
            .context("collecting the rows to drain")??
    };
    state.last_drain_at = Some(now);
    state.next_attempt_at = None;
    let http = reqwest::Client::builder()
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut drainer = Drainer {
        data_dir,
        target,
        cfg,
        clock,
        http,
        state,
        report: DrainReport::default(),
        waits: 0,
        token: target.token.clone(),
        refreshed: false,
    };
    let sizes: Vec<usize> = pending.iter().map(|p| p.body_bytes).collect();
    let mut stop = None;
    for batch in batches(&sizes, cfg.max_rows, cfg.max_bytes) {
        if let Err(s) = drainer.send(&pending, batch).await {
            stop = Some(s);
            break;
        }
    }
    let (status, detail, next) = match stop {
        None => {
            drainer.state.last_synced_at = Some(clock.now());
            (SyncStatus::Synced, None, None)
        }
        Some(Stop::Unauthorized(d)) => (SyncStatus::Unauthorized, Some(d), None),
        Some(Stop::Removed(d)) => (SyncStatus::Removed, Some(d), None),
        Some(Stop::Backoff(until, d)) => (SyncStatus::Backoff, Some(d), Some(until)),
        Some(Stop::Error(d)) => (SyncStatus::Error, Some(d), None),
    };
    drainer.state.status = status;
    drainer.state.detail = detail.clone();
    drainer.state.next_attempt_at = next;
    let state = &drainer.state;
    let mut report = drainer.report.clone();
    report.status = status;
    report.detail = detail;
    report.next_attempt_at = next;
    report.pending = pending.iter().filter(|p| !state.skips(&p.row.id, &p.fingerprint)).count();
    drainer.state.backlog_rows = report.pending;
    drainer.state.backlog_since = if report.pending == 0 {
        None
    } else {
        drainer.state.backlog_since.or(Some(now))
    };
    drainer.state.save(data_dir).await?;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Where it runs: a background task on a member, and a manual trigger.
// ---------------------------------------------------------------------------

/// Wakes the background task for a drain now — after consent is given.
static KICK: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// One drain at a time per process.
static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Asks the background task to drain now (it runs only while this machine is a member).
pub(crate) fn kick() {
    KICK.notify_one();
}

/// Whether the background drain is switched off (`COLONIZER_FLEET_SYNC=off`).
fn disabled() -> bool {
    util::env_nonempty("COLONIZER_FLEET_SYNC").is_some_and(|v| matches!(v.as_str(), "0" | "off" | "false" | "no"))
}

/// What the manual trigger and the status say before the operator has consented.
const CONSENT_REQUIRED: &str = "history sync is off for this membership: review GET /api/fleet/sync/preview (or `colonizer fleet sync --preview`), then enable it with POST /api/fleet/sync/consent (or `colonizer fleet sync --enable`)";

/// Why a drain did not start on this mothership.
enum NotRun {
    NotMember,
    NoConsent,
}

/// One drain for the app, only for a member whose operator consented — checked under the drain
/// lock, so a withdrawal is honoured by every drain that starts after it.
async fn drain_app(app: &Shared, force: bool) -> Result<std::result::Result<DrainReport, NotRun>> {
    let _one = RUNNING.lock().await;
    let Some(target) = app.fleet_members.membership().await else {
        return Ok(Err(NotRun::NotMember));
    };
    if app.fleet_members.history_sync().await != Some(true) {
        return Ok(Err(NotRun::NoConsent));
    }
    let origin = Origin::for_config(&app.cfg.config_dir)?;
    let mut report = drain(
        &app.cfg.data_dir,
        &origin,
        &target,
        &DrainConfig::default(),
        &SystemClock,
        force,
    )
    .await?;
    // A refresh rotated the token on the owner: keep the new one, or the next drain's 401 would
    // spend the refresh again on a token that is already gone.
    if let Some(token) = report.refreshed_token.take() {
        app.fleet_members.set_member_token(&target.member_id, &token).await;
    }
    Ok(Ok(report))
}

async fn run(app: Shared) {
    tokio::select! {
        _ = tokio::time::sleep(FIRST_DRAIN_DELAY) => {}
        _ = KICK.notified() => {}
    }
    loop {
        // The fleet's egress floor (#690) rides the same cadence: it governs booting colonies, not
        // history, so it is fetched whatever the history consent.
        if let Err(e) = crate::fleet_policy::refresh(&app).await {
            eprintln!("fleet policy: {e:#}");
        }
        // Off unless this machine has joined a fleet and its operator consented: an owner, a
        // machine alone, or a member that has not said yes pushes nothing.
        if !disabled()
            && let Err(e) = drain_app(&app, false).await
        {
            eprintln!("fleet sync: {e:#}");
        }
        tokio::select! {
            _ = tokio::time::sleep(DRAIN_INTERVAL) => {}
            _ = KICK.notified() => {}
        }
    }
}

pub(crate) fn start_tasks(app: &Shared) {
    tokio::spawn(run(app.clone()));
}

/// The status view for the app: [`status_of`] with the membership and consent filled in.
async fn status_json(app: &Shared) -> Value {
    let target = app.fleet_members.membership().await;
    let consent = app.fleet_members.history_sync().await.unwrap_or(false);
    status_of(&app.cfg.data_dir, target.as_ref(), consent)
}

/// Where a member's push stands: `consent_required` until the operator says yes, then the drain
/// state of the current membership (a previous membership's state reads as `idle`).
fn status_of(data_dir: &Path, target: Option<&Target>, consent: bool) -> Value {
    let state = DrainState::load(data_dir);
    let current = target.is_some_and(|t| state.member_id.as_deref() == Some(t.member_id.as_str()));
    let retired: Vec<Value> = if current {
        state
            .retired
            .iter()
            .map(|(id, r)| json!({"id": id, "error": r.error, "attempts": r.attempts, "at": r.at}))
            .collect()
    } else {
        Vec::new()
    };
    let status = match (target.is_some(), consent, current) {
        (true, false, _) => SyncStatus::ConsentRequired,
        (_, _, true) => state.status,
        _ => SyncStatus::Idle,
    };
    json!({
        "member": target.is_some(),
        "consent": target.is_some() && consent,
        "enabled": target.is_some() && consent && !disabled(),
        "status": status,
        "detail": if status == SyncStatus::ConsentRequired { Some(CONSENT_REQUIRED.to_string()) } else if current { state.detail.clone() } else { None },
        "acknowledged": if current { state.acked.len() } else { 0 },
        "retired": retired,
        "last_drain_at": state.last_drain_at.filter(|_| current),
        "last_synced_at": state.last_synced_at.filter(|_| current),
        "next_attempt_at": state.next_attempt_at.filter(|_| current),
    })
}

/// What a member tells its owner about its history push, inside the reduced `/api/status` the
/// owner polls (issue #764): `{state, backlog_rows, oldest_unsent_age_s, last_error_class,
/// consent}`, or `None` on a machine that is not a member. Counts and classes only — never a row
/// id, a path or the owner's URL. Read from the persisted drain state: no collection, no hashing.
pub(crate) async fn health_summary(app: &Shared) -> Option<Value> {
    let target = app.fleet_members.membership().await?;
    let consent = app.fleet_members.history_sync().await.unwrap_or(false);
    let data_dir = app.cfg.data_dir.clone();
    let state = tokio::task::spawn_blocking(move || DrainState::load(&data_dir)).await.ok()?;
    Some(summary_of(&state, &target, consent, Utc::now()))
}

/// The pure half of [`health_summary`].
fn summary_of(state: &DrainState, target: &Target, consent: bool, now: DateTime<Utc>) -> Value {
    let current = state.member_id.as_deref() == Some(target.member_id.as_str());
    let status = match (consent, current) {
        (false, _) => SyncStatus::ConsentRequired,
        (true, true) => state.status,
        (true, false) => SyncStatus::Idle,
    };
    let last_error_class = match status {
        SyncStatus::Unauthorized => Some("unauthorized"),
        SyncStatus::Removed => Some("forbidden"),
        SyncStatus::Backoff => Some("rate_limited"),
        SyncStatus::Error => Some("error"),
        _ => None,
    };
    let backlog = consent && current;
    json!({
        "state": status,
        "backlog_rows": if backlog { state.backlog_rows } else { 0 },
        "oldest_unsent_age_s": state
            .backlog_since
            .filter(|_| backlog && state.backlog_rows > 0)
            .map(|since| (now - since).num_seconds().max(0)),
        "last_error_class": last_error_class,
        "consent": consent,
    })
}

/// `GET /api/fleet/sync`: where this member's history push stands.
pub async fn view(State(app): State<Shared>) -> Json<Value> {
    Json(status_json(&app).await)
}

/// `POST /api/fleet/sync`: drain now, even past a 401/403 or a backoff, and answer the report.
/// **409** until this mothership is a member whose operator consented.
pub async fn trigger(State(app): State<Shared>) -> Result<Json<DrainReport>, AppError> {
    match drain_app(&app, true).await {
        Ok(Ok(report)) => Ok(Json(report)),
        Ok(Err(NotRun::NotMember)) => Err(client_error(StatusCode::CONFLICT, "this mothership has not joined a fleet")),
        Ok(Err(NotRun::NoConsent)) => Err(client_error(StatusCode::CONFLICT, CONSENT_REQUIRED)),
        Err(e) => Err(client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}"))),
    }
}

/// What the push would send from `data_dir`: every finished colony's row and logs, and how much
/// of that the current membership's owner has not acknowledged yet.
fn preview_of(data_dir: &Path, origin: &Origin, member_id: &str) -> Result<Value> {
    let all = collect(data_dir, origin, &DrainState::default())?;
    let state = DrainState::load(data_dir);
    let current = state.member_id.as_deref() == Some(member_id);
    let (mut payloads, mut payload_bytes, mut omitted, mut row_bytes) = (0usize, 0u64, 0usize, 0u64);
    let (mut pending, mut pending_bytes) = (0usize, 0u64);
    for p in &all {
        let logs: u64 = p.row.payloads.iter().filter(|r| !r.omitted).map(|r| r.bytes).sum();
        payloads += p.files.len();
        omitted += p.row.payloads.iter().filter(|r| r.omitted).count();
        payload_bytes += logs;
        row_bytes += p.body_bytes as u64;
        if !(current && state.skips(&p.row.id, &p.fingerprint)) {
            pending += 1;
            pending_bytes += logs + p.body_bytes as u64;
        }
    }
    Ok(json!({
        "colonies": all.len(),
        "payloads": payloads,
        "payload_bytes": payload_bytes,
        "omitted_payloads": omitted,
        "row_bytes": row_bytes,
        "total_bytes": payload_bytes + row_bytes,
        "pending_colonies": pending,
        "pending_bytes": pending_bytes,
        "includes": "each finished colony's record (repo, issue, branch, pull request, status, summary, cost, model use, timings) and its event, harness and gateway logs",
        "excludes": "running colonies, transcripts, stats, settings, secrets and tokens",
    }))
}

/// `GET /api/fleet/sync/preview`: what enabling the push would send — counts and bytes, read from
/// the same collection the drain sends. Sends nothing. **409** when not a member.
pub async fn preview(State(app): State<Shared>) -> ApiResult<Value> {
    let Some(target) = app.fleet_members.membership().await else {
        return Err(client_error(StatusCode::CONFLICT, "this mothership has not joined a fleet"));
    };
    let origin =
        Origin::for_config(&app.cfg.config_dir).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
    let data_dir = app.cfg.data_dir.clone();
    let mut answer = tokio::task::spawn_blocking(move || preview_of(&data_dir, &origin, &target.member_id))
        .await
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    answer["owner_url"] = json!(target.owner_url);
    Ok(Json(answer))
}

/// The body of `POST /api/fleet/sync/consent`.
#[derive(Deserialize)]
pub struct ConsentBody {
    enabled: bool,
}

/// `POST /api/fleet/sync/consent`: the operator's yes (or no) to pushing this machine's history
/// to the owner, for this membership only. Answers the status; enabling wakes the drain.
pub async fn consent(State(app): State<Shared>, Json(body): Json<ConsentBody>) -> ApiResult<Value> {
    if !app.fleet_members.set_history_sync(body.enabled).await {
        return Err(client_error(StatusCode::CONFLICT, "this mothership has not joined a fleet"));
    }
    if body.enabled {
        kick();
    }
    Ok(Json(status_json(&app).await))
}

// ---------------------------------------------------------------------------
// The owner's ingest routes, on a member's fleet token.
// ---------------------------------------------------------------------------

/// Serializes the read-modify-write of a member's `sessions.json`.
pub(crate) static INGEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The member a request's fleet token belongs to, or the 403 a non-member gets.
async fn ingest_member(
    app: &Shared,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Result<PathBuf, AppError> {
    let Some(axum::Extension(token)) = scoped else {
        return Err(client_error(StatusCode::FORBIDDEN, "this route takes a fleet token"));
    };
    if token.scope != crate::api_tokens::Scope::Fleet {
        return Err(client_error(StatusCode::FORBIDDEN, "this route takes a fleet token"));
    }
    match app.fleet_members.member_for_token(&token.id).await {
        Some(id) if fleet_export::is_safe_segment(&id) => Ok(app.cfg.data_dir.join(INGEST_DIR).join(id)),
        _ => Err(client_error(
            StatusCode::FORBIDDEN,
            "this token belongs to no member of this fleet",
        )),
    }
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `PUT /api/fleet/peer/payloads/{sha256}`: one log, stored by its hash. Idempotent.
pub async fn put_payload(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    UrlPath(sha): UrlPath<String>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let root = ingest_member(&app, scoped).await?;
    if !is_sha256(&sha) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "the payload key is a lowercase hex SHA-256",
        ));
    }
    if sha256_hex(&body) != sha {
        return Err(client_error(StatusCode::BAD_REQUEST, "the payload does not hash to its key"));
    }
    let dir = root.join("payloads");
    let path = dir.join(&sha);
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(StatusCode::NO_CONTENT);
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
    util::write_atomic(&path, &body)
        .await
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a row fails on, if anything, before it is stored.
fn check_row(row: &IngestRow) -> Option<String> {
    let r = &row.record;
    if row.id.is_empty() || row.id.len() > 512 || row.id != r.id {
        return Some("the row id must be its record's id".into());
    }
    if r.id != format!("{}:{}", r.origin_host, r.original_id) {
        return Some("the record id must be <origin_host>:<original_id>".into());
    }
    let mut names = BTreeSet::new();
    for p in &row.payloads {
        if !LOG_BASENAMES.contains(&p.name.as_str()) || !names.insert(p.name.as_str()) {
            return Some(format!("payload name {:?} is not an allowlisted log, or repeats", p.name));
        }
        if !p.omitted && !is_sha256(&p.sha256) {
            return Some(format!("payload {} has no valid sha256", p.name));
        }
    }
    None
}

/// `POST /api/fleet/peer/rows`: upsert a batch of rows by id. Each row is accepted or refused by
/// name — a row whose payloads are not all here yet is refused with the missing hashes — and a
/// batch that cannot be read at all is refused whole, which the member bisects.
pub async fn post_rows(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    Json(body): Json<RowsBody>,
) -> Result<Json<RowsAnswer>, AppError> {
    let root = ingest_member(&app, scoped).await?;
    if body.rows.len() > MAX_BATCH_ROWS {
        return Err(client_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("at most {MAX_BATCH_ROWS} rows per batch"),
        ));
    }
    let mut answer = RowsAnswer::default();
    let mut good = Vec::new();
    for row in body.rows {
        if let Some(error) = check_row(&row) {
            answer.rejected.push(RejectedRow {
                id: row.id,
                error,
                missing_payloads: Vec::new(),
            });
            continue;
        }
        let mut missing = Vec::new();
        for p in row.payloads.iter().filter(|p| !p.omitted) {
            if !tokio::fs::try_exists(root.join("payloads").join(&p.sha256))
                .await
                .unwrap_or(false)
            {
                missing.push(p.sha256.clone());
            }
        }
        if missing.is_empty() {
            good.push(row);
        } else {
            answer.rejected.push(RejectedRow {
                id: row.id,
                error: "the row references payloads this owner does not hold; upload them first".into(),
                missing_payloads: missing,
            });
        }
    }
    if !good.is_empty() {
        let _one = INGEST.lock().await;
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
        let file = root.join(ROWS_FILE);
        let mut stored: BTreeMap<String, Value> = tokio::fs::read(&file)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let now = Utc::now();
        for row in good {
            answer.accepted.push(row.id.clone());
            stored.insert(
                row.id.clone(),
                json!({"record": row.record, "payloads": row.payloads, "received_at": now}),
            );
        }
        let bytes =
            serde_json::to_vec_pretty(&stored).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")))?;
        util::write_atomic(&file, &bytes)
            .await
            .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
        // The member's name, kept beside its rows, so the owner's history view still names a
        // member after it is removed (`fleet_history.rs`).
        crate::fleet_history::note_member(&app, &root).await;
    }
    Ok(Json(answer))
}

/// The API routes this module serves: the member's status and trigger (owner-only, like every
/// cockpit route) and the owner's two ingest routes (`fleet` tokens, `api_tokens::classify`).
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::extract::DefaultBodyLimit;
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/api/fleet/sync", get(view).post(trigger))
        .route("/api/fleet/sync/preview", get(preview))
        .route("/api/fleet/sync/consent", post(consent))
        .route(
            "/api/fleet/peer/rows",
            post(post_rows).layer(DefaultBodyLimit::max(MAX_BATCH_BODY)),
        )
        .route(
            "/api/fleet/peer/payloads/{sha256}",
            put(put_payload).layer(DefaultBodyLimit::max(MAX_PAYLOAD_BYTES as usize)),
        )
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// A log's bytes as they leave this machine: read, then through the shared #761 redactor
/// ([`crate::archive::redact_for_bundle`], as the export bundle and the archive do), so a log
/// written before redaction existed never reaches the owner with a secret in it. The owner stores
/// payloads exactly as sent (docs/fleet.md), so this is the only pass they get.
fn outgoing_log(name: &str, path: &Path) -> std::io::Result<Vec<u8>> {
    Ok(crate::archive::redact_for_bundle(name, std::fs::read(path)?))
}

/// How long a 429/503 asks us to wait: `Retry-After` in seconds or as an HTTP date, capped.
fn retry_after(headers: &reqwest::header::HeaderMap, now: DateTime<Utc>) -> Duration {
    let Some(value) = headers.get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()) else {
        return DEFAULT_RETRY_AFTER;
    };
    let value = value.trim();
    let wait = if let Ok(secs) = value.parse::<u64>() {
        Duration::from_secs(secs)
    } else if let Ok(at) = DateTime::parse_from_rfc2822(value) {
        (at.with_timezone(&Utc) - now).to_std().unwrap_or(Duration::ZERO)
    } else {
        DEFAULT_RETRY_AFTER
    };
    wait.min(MAX_RETRY_AFTER)
}

/// The `error` an owner's refusal names, or its first bytes.
fn error_of(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| util::truncate(String::from_utf8_lossy(body).trim(), 200))
}

async fn read_bounded(mut res: reqwest::Response) -> Result<Vec<u8>, Stop> {
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| Stop::Error(format!("the owner's answer could not be read: {e}")))?
    {
        if body.len() + chunk.len() > MAX_ANSWER {
            return Err(Stop::Error("the owner's answer is too large".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
