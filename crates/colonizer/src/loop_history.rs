//! Per-loop run history (issue #1199): one record per run of every loop, built-in or custom, kept
//! for 90 days, and `GET /api/loops/{id}/history?days=` to read it back as per-day buckets and a
//! run list. The Loops page's 7-day strip on each card and its 7/30/90-day detail charts read this.
//!
//! A record holds what the run did (an outcome, a one-line summary, counts by kind) and the ids of
//! the colonies it dispatched. **Cost is never stored**: it is summed from those colonies'
//! `total_cost_usd` when the history is read, so a colony that is still working, or finishes
//! later, is priced at what it has spent by then. A colony loop's own run is recorded when it
//! launches with no outcome of its own — its outcome is its colony's status at read time.
//!
//! The built-in loops have fixed ids ([`MERGE_TRAIN`], [`SUPPLY_CHAIN`], [`TS_ANY`], [`DOCS`],
//! [`DISK_CLEANUP`]); a custom loop's id is its own. Dry runs are never recorded: they change
//! nothing. The file is `<config_dir>/loop-history.json`.

use crate::{
    ApiResult, App, Shared,
    api_tokens::ScopedToken,
    client_error,
    loops::LoopKind,
    sessions::{Session, SessionStatus},
    util::write_atomic,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};
use tokio::sync::{Mutex, RwLock};

pub const MERGE_TRAIN: &str = "merge-train";
pub const SUPPLY_CHAIN: &str = "supply-chain";
pub const TS_ANY: &str = "ts-any";
pub const DOCS: &str = "docs";
pub const DISK_CLEANUP: &str = crate::disk_cleanup::LOOP_ID;
/// The built-in loops that are not in `loops.json`: their history is served under these ids.
const BUILTIN_IDS: [&str; 5] = [MERGE_TRAIN, SUPPLY_CHAIN, TS_ANY, DOCS, DISK_CLEANUP];

/// How long a record is kept.
pub const RETENTION_DAYS: i64 = 90;
/// The most records one loop keeps: an hourly loop makes 2160 in 90 days.
const MAX_PER_LOOP: usize = 4000;
/// The most runs one answer lists, newest first; the buckets always cover the whole range.
const MAX_RUNS_LISTED: usize = 200;
const FILE: &str = "loop-history.json";

/// How a run went. `Running` exists only in an answer: a colony loop's run whose colony is still
/// at work. `Skipped` is a run that found nothing to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Partial,
    Failed,
    Skipped,
    Running,
}

/// One run of one loop.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub loop_id: String,
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// `schedule`, `manual`, `low_disk`, `retry`.
    pub trigger: String,
    /// `None` for a colony loop's run: its colony's status says how it went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    pub summary: String,
    /// What the run did, by kind: `merged`, `red`, `dispatched`, `critical`, `bytes`…
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    /// The colonies the run dispatched or resumed; their spend is the run's cost.
    #[serde(default)]
    pub colonies: Vec<String>,
}

impl RunRecord {
    /// A run that launched one colony: its outcome is that colony's.
    pub fn launched(loop_id: &str, at: DateTime<Utc>, trigger: &str, session: &Session) -> Self {
        RunRecord {
            loop_id: loop_id.to_string(),
            at,
            finished_at: None,
            trigger: trigger.to_string(),
            outcome: None,
            summary: format!("started a colony on {}", session.repo),
            counts: BTreeMap::from([("colonies".to_string(), 1)]),
            colonies: vec![session.id.clone()],
        }
    }

    /// A run that could not start.
    pub fn refused(loop_id: &str, at: DateTime<Utc>, why: &str) -> Self {
        RunRecord {
            loop_id: loop_id.to_string(),
            at,
            finished_at: Some(at),
            trigger: "schedule".to_string(),
            outcome: Some(Outcome::Failed),
            summary: why.to_string(),
            counts: BTreeMap::new(),
            colonies: Vec::new(),
        }
    }
}

/// The store: every record of every loop, newest last.
pub struct HistoryStore {
    file: PathBuf,
    records: RwLock<Vec<RunRecord>>,
    persist: Mutex<()>,
}

impl HistoryStore {
    pub fn new(config_dir: &std::path::Path) -> Self {
        let file = config_dir.join(FILE);
        let mut records: Vec<RunRecord> = match std::fs::read(&file) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
                eprintln!("loop history: could not parse {}: {e}; starting empty", file.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        prune(&mut records, Utc::now());
        HistoryStore {
            file,
            records: RwLock::new(records),
            persist: Mutex::new(()),
        }
    }

    async fn save(&self) {
        let _guard = self.persist.lock().await;
        let data = match serde_json::to_vec(&*self.records.read().await) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("loop history: could not encode: {e}");
                return;
            }
        };
        if let Some(dir) = self.file.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        if let Err(e) = write_atomic(&self.file, &data).await {
            eprintln!("loop history: could not save {}: {e:#}", self.file.display());
        }
    }

    pub(crate) async fn push(&self, record: RunRecord) {
        {
            let mut records = self.records.write().await;
            records.push(record);
            prune(&mut records, Utc::now());
        }
        self.save().await;
    }

    async fn of(&self, loop_id: &str) -> Vec<RunRecord> {
        self.records
            .read()
            .await
            .iter()
            .filter(|r| r.loop_id == loop_id)
            .cloned()
            .collect()
    }
}

/// Drops what is past retention, then each loop's oldest records past its cap.
fn prune(records: &mut Vec<RunRecord>, now: DateTime<Utc>) {
    let cutoff = now - Duration::days(RETENTION_DAYS);
    records.retain(|r| r.at >= cutoff);
    let mut per: HashMap<String, usize> = HashMap::new();
    for r in records.iter() {
        *per.entry(r.loop_id.clone()).or_default() += 1;
    }
    let mut over: HashMap<String, usize> = per
        .into_iter()
        .filter_map(|(id, n)| n.checked_sub(MAX_PER_LOOP).filter(|o| *o > 0).map(|o| (id, o)))
        .collect();
    if !over.is_empty() {
        records.retain(|r| match over.get_mut(&r.loop_id) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        });
    }
}

/// Records a run. Never fails the run it describes.
pub(crate) async fn record(app: &App, record: RunRecord) {
    app.loop_history.push(record).await;
}

// --- what each built-in loop's report says about how the run went -----------------------------

fn count(counts: &mut BTreeMap<String, u64>, key: &str, n: usize) {
    if n > 0 {
        counts.insert(key.to_string(), n as u64);
    }
}

/// The merge-train loop's run: moved, stuck and idle pull requests.
pub(crate) fn from_merge_loop(report: &crate::merge_loop::Report) -> RunRecord {
    use crate::merge_loop::Action as A;
    let mut counts = BTreeMap::new();
    let mut colonies = Vec::new();
    let items: Vec<_> = report.repos.iter().flat_map(|r| r.items.iter()).collect();
    let n = |a: A| items.iter().filter(|i| i.action == a).count();
    for (key, action) in [
        ("merged", A::Merged),
        ("updated", A::Updated),
        ("rebased", A::Rebased),
        ("rerun", A::Rerun),
        ("redo_dispatched", A::RedoDispatched),
        ("resolving", A::Resolving),
        ("red", A::Red),
        ("needs_redo", A::NeedsRedo),
        ("waiting", A::Waiting),
        ("skipped", A::Skipped),
    ] {
        count(&mut counts, key, n(action));
    }
    for i in &items {
        if matches!(i.action, A::RedoDispatched | A::Resolving) && !i.session.is_empty() && !colonies.contains(&i.session) {
            colonies.push(i.session.clone());
        }
    }
    let moved = n(A::Merged) + n(A::Updated) + n(A::Rebased) + n(A::Rerun) + n(A::RedoDispatched) + n(A::Resolving);
    let stuck = n(A::Red) + n(A::NeedsRedo) + usize::from(report.stopped.is_some());
    let outcome = if stuck == 0 && moved == 0 {
        Outcome::Skipped
    } else if stuck == 0 {
        Outcome::Ok
    } else if moved == 0 {
        Outcome::Failed
    } else {
        Outcome::Partial
    };
    RunRecord {
        loop_id: MERGE_TRAIN.to_string(),
        at: report.started_at.unwrap_or_else(Utc::now),
        finished_at: report.finished_at,
        trigger: "run".to_string(),
        outcome: Some(outcome),
        summary: report.summary.clone(),
        counts,
        colonies,
    }
}

/// The dependencies and supply-chain loop's run.
pub(crate) fn from_supply_chain(report: &crate::supply_chain_loop::Report) -> RunRecord {
    let mut counts = BTreeMap::new();
    for (severity, n) in &report.counts {
        count(&mut counts, severity, *n);
    }
    count(&mut counts, "dispatched", report.dispatched.len());
    count(&mut counts, "skipped", report.skipped.len());
    count(&mut counts, "attention", report.attention.len());
    let errored = report.repos.iter().filter(|r| r.error.is_some()).count();
    let outcome = if report.repos.is_empty() {
        Outcome::Skipped
    } else if errored == report.repos.len() {
        Outcome::Failed
    } else if errored > 0 || !report.attention.is_empty() {
        Outcome::Partial
    } else {
        Outcome::Ok
    };
    RunRecord {
        loop_id: SUPPLY_CHAIN.to_string(),
        at: report.started_at,
        finished_at: Some(report.finished_at),
        trigger: report.trigger.clone(),
        outcome: Some(outcome),
        summary: report.summary(),
        counts,
        colonies: report.dispatched.iter().filter_map(|d| d.session.clone()).collect(),
    }
}

/// The "TypeScript: remove any" loop's run.
pub(crate) fn from_ts_any(report: &crate::ts_any_loop::Report) -> RunRecord {
    let mut counts = BTreeMap::new();
    count(&mut counts, "explicit_any", report.total);
    count(&mut counts, "dispatched", report.dispatched.len());
    count(&mut counts, "skipped", report.skipped.len());
    count(&mut counts, "flagged", report.attention.len());
    let counted: Vec<_> = report.repos.iter().filter(|r| r.typescript).collect();
    let errored = counted.iter().filter(|r| r.error.is_some()).count();
    let outcome = if report.repos.is_empty() || counted.is_empty() {
        Outcome::Skipped
    } else if errored == counted.len() {
        Outcome::Failed
    } else if errored > 0 || !report.attention.is_empty() {
        Outcome::Partial
    } else {
        Outcome::Ok
    };
    RunRecord {
        loop_id: TS_ANY.to_string(),
        at: report.started_at,
        finished_at: Some(report.finished_at),
        trigger: report.trigger.clone(),
        outcome: Some(outcome),
        summary: report.summary(),
        counts,
        colonies: report.dispatched.iter().filter_map(|d| d.session.clone()).collect(),
    }
}

/// The Docs & README loop's run.
pub(crate) fn from_docs(report: &crate::docs_loop::Report) -> RunRecord {
    use crate::docs_loop::Action as A;
    let mut counts = BTreeMap::new();
    let n = |a: A| report.repos.iter().filter(|r| r.action == a).count();
    count(&mut counts, "dispatched", n(A::Dispatched));
    count(&mut counts, "clean", n(A::Clean));
    count(&mut counts, "skipped", n(A::Skipped));
    count(&mut counts, "report_only", n(A::ReportOnly));
    count(&mut counts, "failed", n(A::Error));
    let findings: usize = report.repos.iter().map(|r| r.findings.len() + r.more).sum();
    count(&mut counts, "findings", findings);
    let idle = n(A::Skipped) + n(A::ReportOnly);
    let outcome = if report.repos.is_empty() || idle == report.repos.len() {
        Outcome::Skipped
    } else if n(A::Error) == report.repos.len() {
        Outcome::Failed
    } else if n(A::Error) > 0 {
        Outcome::Partial
    } else {
        Outcome::Ok
    };
    RunRecord {
        loop_id: DOCS.to_string(),
        at: report.at,
        finished_at: Some(report.at),
        trigger: report.trigger.clone(),
        outcome: Some(outcome),
        summary: report.summary(),
        counts,
        colonies: report.repos.iter().filter_map(|r| r.colony.clone()).collect(),
    }
}

/// The disk-cleanup loop's run.
pub(crate) fn from_disk_cleanup(report: &crate::disk_cleanup::RunReport) -> RunRecord {
    let mut counts = BTreeMap::new();
    let items: usize = report.categories.iter().map(|c| c.count).sum();
    let failed: usize = report.categories.iter().map(|c| c.failed.len()).sum();
    count(&mut counts, "items", items);
    count(&mut counts, "failed", failed);
    if report.bytes > 0 {
        counts.insert("bytes".to_string(), report.bytes);
    }
    let outcome = if failed > 0 && report.bytes == 0 {
        Outcome::Failed
    } else if failed > 0 || report.attention.is_some() {
        Outcome::Partial
    } else if items == 0 {
        Outcome::Skipped
    } else {
        Outcome::Ok
    };
    RunRecord {
        loop_id: DISK_CLEANUP.to_string(),
        at: report.at,
        finished_at: Some(report.at),
        trigger: report.trigger.clone(),
        outcome: Some(outcome),
        summary: report.summary(),
        counts,
        colonies: Vec::new(),
    }
}

// --- reading it back --------------------------------------------------------------------------

/// A colony's status as its loop run's outcome.
fn outcome_of_colony(status: Option<SessionStatus>) -> Outcome {
    match status {
        None => Outcome::Ok,
        Some(SessionStatus::Failed) => Outcome::Failed,
        Some(SessionStatus::Stopped | SessionStatus::Closed) => Outcome::Partial,
        Some(SessionStatus::NoChanges) => Outcome::Skipped,
        Some(SessionStatus::PrOpened | SessionStatus::Merged) => Outcome::Ok,
        // Queued, starting, working, waiting, publishing or parked: not over.
        Some(_) => Outcome::Running,
    }
}

/// One run as the API answers it.
#[derive(Debug, Serialize)]
struct RunView {
    at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<DateTime<Utc>>,
    trigger: String,
    outcome: Outcome,
    summary: String,
    counts: BTreeMap<String, u64>,
    colonies: Vec<String>,
    cost_usd: f64,
}

/// One day: how many runs, by outcome, and what their colonies cost.
#[derive(Debug, Default, Serialize, PartialEq)]
struct Bucket {
    day: String,
    runs: u32,
    ok: u32,
    partial: u32,
    failed: u32,
    skipped: u32,
    running: u32,
    colonies: u32,
    cost_usd: f64,
}

/// The history answer for `days` days ending at `now`, days cut at the caller's local midnight
/// (`tz_offset_minutes` east of UTC). Pure: `sessions` prices the colonies.
fn view(id: &str, records: &[RunRecord], sessions: &[Session], days: u32, tz_offset_minutes: i32, now: DateTime<Utc>) -> Value {
    let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let shift = Duration::minutes(tz_offset_minutes as i64);
    let today: NaiveDate = (now + shift).date_naive();
    let first = today - Duration::days(days as i64 - 1);
    let mut buckets: Vec<Bucket> = (0..days)
        .map(|i| Bucket {
            day: (first + Duration::days(i as i64)).format("%Y-%m-%d").to_string(),
            ..Bucket::default()
        })
        .collect();
    let run_of = |r: &RunRecord| {
        let outcome = r
            .outcome
            .unwrap_or_else(|| outcome_of_colony(r.colonies.first().and_then(|c| by_id.get(c.as_str())).map(|s| s.status)));
        let cost: f64 = r
            .colonies
            .iter()
            .filter_map(|c| by_id.get(c.as_str()))
            .map(|s| s.total_cost_usd())
            .sum();
        RunView {
            at: r.at,
            finished_at: r.finished_at,
            trigger: r.trigger.clone(),
            outcome,
            summary: r.summary.clone(),
            counts: r.counts.clone(),
            colonies: r.colonies.clone(),
            cost_usd: cost,
        }
    };
    let mut runs: Vec<RunView> = Vec::new();
    for r in records {
        let day = (r.at + shift).date_naive();
        if day < first || day > today {
            continue;
        }
        let run = run_of(r);
        let b = &mut buckets[(day - first).num_days() as usize];
        b.runs += 1;
        match run.outcome {
            Outcome::Ok => b.ok += 1,
            Outcome::Partial => b.partial += 1,
            Outcome::Failed => b.failed += 1,
            Outcome::Skipped => b.skipped += 1,
            Outcome::Running => b.running += 1,
        }
        b.colonies += run.colonies.len() as u32;
        b.cost_usd += run.cost_usd;
        runs.push(run);
    }
    // The latest run whatever the range, so a card can say how the loop last did.
    let last = records.iter().max_by_key(|r| r.at).map(run_of);
    runs.sort_by_key(|r| std::cmp::Reverse(r.at));
    let total = |f: fn(&Bucket) -> u32| buckets.iter().map(f).sum::<u32>();
    let totals = json!({
        "runs": total(|b| b.runs),
        "ok": total(|b| b.ok),
        "partial": total(|b| b.partial),
        "failed": total(|b| b.failed),
        "skipped": total(|b| b.skipped),
        "running": total(|b| b.running),
        "colonies": total(|b| b.colonies),
        "cost_usd": buckets.iter().map(|b| b.cost_usd).sum::<f64>(),
    });
    runs.truncate(MAX_RUNS_LISTED);
    json!({
        "id": id,
        "days": days,
        "from": first.format("%Y-%m-%d").to_string(),
        "to": today.format("%Y-%m-%d").to_string(),
        "retention_days": RETENTION_DAYS,
        "totals": totals,
        "buckets": buckets,
        "last": last,
        "runs": runs,
    })
}

#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    days: Option<u32>,
    tz_offset_minutes: Option<i32>,
}

/// `GET /api/loops/{id}/history?days=7`: the loop's runs for the last 1–90 days, by day and one by
/// one, with each run's outcome, counts, dispatched colonies and what those colonies cost. A scoped
/// token reads a loop inside its org/repo limits; the built-in loops are the owner's.
pub async fn history(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
    scoped: Option<axum::Extension<ScopedToken>>,
) -> ApiResult<Value> {
    let unknown = || client_error(StatusCode::NOT_FOUND, "no such loop");
    if BUILTIN_IDS.contains(&id.as_str()) && id != DISK_CLEANUP {
        if scoped.is_some() {
            return Err(unknown());
        }
    } else {
        let l = app.loops.get(&id).await.ok_or_else(unknown)?;
        if let Some(axum::Extension(token)) = &scoped
            && (!token.covers(&l.org, &l.repo) || l.kind == LoopKind::DiskCleanup)
        {
            return Err(unknown());
        }
    }
    let days = query.days.unwrap_or(7).clamp(1, RETENTION_DAYS as u32);
    let tz = query.tz_offset_minutes.unwrap_or(0).clamp(-14 * 60, 14 * 60);
    let records = app.loop_history.of(&id).await;
    let sessions = app.sessions.read().await;
    Ok(Json(view(&id, &records, &sessions, days, tz, Utc::now())))
}

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new().route("/api/loops/{id}/history", routing::get(history))
}

/// A scoped token reads a loop's history like its runs: a watch.
fn token_scope<'a>(method: &axum::http::Method, segs: &[&'a str]) -> Option<crate::api_tokens::Need<'a>> {
    match segs {
        ["api", "loops", id, "history"] if *method == axum::http::Method::GET && !id.is_empty() => {
            Some(crate::api_tokens::Need::Bare(crate::api_tokens::Scope::Read))
        }
        _ => None,
    }
}

/// This module's feature descriptor (`features.rs`).
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "loop_history",
    routes,
    token_scope: Some(token_scope),
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

#[cfg(test)]
mod tests;
