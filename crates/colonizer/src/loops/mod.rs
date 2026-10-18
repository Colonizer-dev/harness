//! Loops: a saved prompt on a repository that launches a colony on a schedule — the mothership's
//! version of Claude Code's `/loop`. A loop runs on a fixed cadence (every N minutes, daily,
//! weekly, monthly, every N days) or self-paced. On a module that serves the loop tools (its
//! manifest says `loop_tools`, issue #643) a self-paced colony names its own next run with
//! `loop_next`, and any colony can end its loop with `loop_stop`; on the others the brief names
//! neither. One run at a time: a tick that finds the previous run still live skips and says so.
//! A map loop instead keeps a repository — or every repository of an org — mapped: each firing
//! draws one map, exactly as the Map view would. Loops are operator configuration, saved to
//! `<config_dir>/loops.json`; their runs are ordinary colonies tagged `origin: "loop:<id>"`
//! (a map loop's: `map:loop:<id>`). One loop is built in: **Disk cleanup** (`disk_cleanup.rs`,
//! id `disk-cleanup`), present on every install and off until the owner switches it on; it runs
//! in-process housekeeping instead of launching a colony.

pub(crate) mod pr_labels;
pub mod templates;

use crate::schedule::{Cadence, next_run_after};
use crate::sessions::{self, NewSession, Session, SessionStatus};
use crate::{
    ApiResult, App, Shared,
    api_tokens::ScopedToken,
    client_error,
    util::{short_id, valid_repo, write_atomic},
};
use anyhow::Result;
use axum::http::HeaderMap;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};

/// The origin tag a loop's colonies carry.
pub const ORIGIN_PREFIX: &str = "loop:";
/// Bounds on the delay a self-paced colony may ask for with `loop_next`.
pub const NEXT_MIN_MINUTES: u64 = 15;
pub const NEXT_MAX_MINUTES: u64 = 24 * 60;
/// How soon a tick that skipped a self-paced loop (its run still live) tries again.
const SKIP_RETRY_MINUTES: i64 = 15;
/// How far apart an org-wide map loop's launches sit: one repository at a time, so mapping the
/// whole org takes its cycle gently instead of all at once.
pub const MAP_STAGGER_MINUTES: i64 = 10;
const MAX_PROMPT: usize = 20_000;
/// How long after a run that failed for an infrastructure reason it may be re-run, when the loop
/// names no window of its own (issue #881): long enough to ride out a short outage, short enough
/// that the re-run is still about the same work.
pub const DEFAULT_RETRY_FAILED_RUNS: i64 = 60;

/// The loop a colony belongs to, from its origin tag.
pub fn loop_id_of(origin: &str) -> Option<&str> {
    origin.strip_prefix(ORIGIN_PREFIX).filter(|id| !id.is_empty())
}

/// The last colony a loop launched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub session: String,
    pub at: DateTime<Utc>,
    /// Whether this run is a re-run of the one before it (issue #881). The re-run replaces
    /// `session` and keeps the original `at`, so the schedule does not move; the scheduler re-runs
    /// only while this is false, so one failure earns at most one re-run.
    #[serde(default)]
    pub retried: bool,
    /// The run's outcome for the loop list, filled by [`list`] and not persisted: the colony's
    /// status, with a failed run's class on it (`failed (transient_infra)`). An old `loops.json`
    /// has no such field, and `None` means the colony is gone.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// What a loop launches: an ordinary colony working from its prompt (`colony`), architecture-map
/// refreshes (`map`), which ignore the prompt, or — for the one built-in loop only — the
/// mothership's own disk cleanup (`disk_cleanup`), which launches nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopKind {
    #[default]
    Colony,
    Map,
    DiskCleanup,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    pub id: String,
    pub name: String,
    pub org: String,
    pub repo: String,
    /// The id of the scoped API token that created this loop (issue #627, api_tokens.rs), when one
    /// did: every run is admitted against the token's org/repo limits, caps and budget and marked
    /// as its external input, only this token may edit or run the loop, and revoking the token
    /// ends it at its next run. `None` for anything the owner made. The token itself is never
    /// stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_token: Option<String>,
    /// A map loop over the whole org (`owner/*`): the repositories still to map in the current
    /// cycle. Server-owned — never taken from the API, emptied when the loop is updated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<String>,
    pub prompt: String,
    pub cadence: Cadence,
    /// What the loop launches: `colony` (the default, so a loop saved before kinds existed reads
    /// back as one) or `map`.
    #[serde(default)]
    pub kind: LoopKind,
    /// The loop's work is GitHub's (issue #778: triage, CI flakes, merged PRs): before it launches
    /// anything the mothership checks it can reach the repository, and the colony it starts gets the
    /// read-only context under `/colonizer/github` and the host-proxied write tools. `false` — the
    /// default, so a loop saved before the field existed reads back as one — for every other loop.
    #[serde(default)]
    pub needs_github: bool,
    /// The operator's UTC offset when the loop was saved, so the cockpit can show local times; the
    /// cadence itself is UTC.
    #[serde(default)]
    pub tz_offset_minutes: i32,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subagent_model: Option<String>,
    pub autopilot: bool,
    #[serde(default)]
    pub max_runs: Option<u32>,
    /// Minutes after a run started within which a run that failed for an infrastructure reason is
    /// run once more (issue #881). `None` — absent on a loop saved before this field — means the
    /// default [`DEFAULT_RETRY_FAILED_RUNS`]; `Some(0)` switches the re-run off.
    #[serde(default)]
    pub retry_failed_runs: Option<u32>,
    #[serde(default)]
    pub end_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    /// When it runs next; `None` once it has ended (stopped, run out, or past its end date).
    #[serde(default)]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub runs: u32,
    #[serde(default)]
    pub last_run: Option<LastRun>,
    /// The last thing the loop did or was told: a skip, the colony's chosen next run, why it ended.
    #[serde(default)]
    pub last_note: Option<String>,
    /// Why the loop ended, when it has.
    #[serde(default)]
    pub ended_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    /// The built-in disk-cleanup loop's settings, history and attention (disk_cleanup.rs); absent
    /// on every other loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_cleanup: Option<crate::disk_cleanup::State>,
}

impl Loop {
    pub fn self_paced(&self) -> bool {
        matches!(self.cadence, Cadence::SelfPaced {})
    }

    /// Whether the loop maps every repository of the org (`owner/*`), one per firing.
    pub fn org_wide(&self) -> bool {
        self.kind == LoopKind::Map && self.repo.ends_with("/*")
    }

    /// The window within which a run that failed for an infrastructure reason is run once more, or
    /// `None` when the loop has that off (issue #881).
    fn retry_window(&self) -> Option<ChronoDuration> {
        match self.retry_failed_runs {
            Some(0) => None,
            Some(minutes) => Some(ChronoDuration::minutes(minutes as i64)),
            None => Some(ChronoDuration::minutes(DEFAULT_RETRY_FAILED_RUNS)),
        }
    }

    /// Ends the loop with a reason: disabled, nothing scheduled.
    fn end(&mut self, reason: String) {
        self.enabled = false;
        self.next_run_at = None;
        self.last_note = Some(reason.clone());
        self.ended_reason = Some(reason);
    }

    /// After a run started at `now`: count it, book the next one, and end the loop if it has run
    /// out of runs or time.
    fn record_run(&mut self, session: &str, now: DateTime<Utc>) {
        self.runs += 1;
        self.last_run = Some(LastRun {
            session: session.to_string(),
            at: now,
            retried: false,
            outcome: None,
        });
        self.next_run_at = Some(next_run_after(&self.cadence, now));
        self.check_limits();
    }

    fn check_limits(&mut self) {
        if let Some(max) = self.max_runs
            && self.runs >= max
        {
            self.end(format!("finished: ran {max} {}", if max == 1 { "time" } else { "times" }));
            return;
        }
        if let (Some(end), Some(next)) = (self.end_at, self.next_run_at)
            && next > end
        {
            self.end(format!("finished: its end date {} passed", end.format("%Y-%m-%d %H:%M UTC")));
        }
    }
}

/// What a tick does with a due loop.
#[derive(Debug, PartialEq)]
pub enum Tick {
    Launch,
    /// Its previous run is still live: skip, and try again at the given time.
    Skip {
        until: DateTime<Utc>,
        note: String,
    },
}

/// Whether a due loop launches now or waits for its live run. Pure, for the tests.
pub fn plan_tick(l: &Loop, live_run: Option<&str>, now: DateTime<Utc>) -> Tick {
    match live_run {
        None => Tick::Launch,
        Some(session) => {
            let until = if l.self_paced() {
                now + ChronoDuration::minutes(SKIP_RETRY_MINUTES)
            } else {
                next_run_after(&l.cadence, now)
            };
            Tick::Skip {
                until,
                note: format!(
                    "skipped at {}: the previous run ({session}) is still live",
                    now.format("%H:%M UTC")
                ),
            }
        }
    }
}

/// The loops due at `now`: enabled, and their next run is not in the future.
pub fn due(loops: &[Loop], now: DateTime<Utc>) -> Vec<String> {
    loops
        .iter()
        .filter(|l| l.enabled && l.next_run_at.is_some_and(|at| at <= now))
        .map(|l| l.id.clone())
        .collect()
}

/// Whether a colony still counts as the loop's current run: anything not finished with.
fn in_flight(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Queued | SessionStatus::Blocked) || status.busy()
}

/// The loop's run that is still in flight, if any.
pub fn live_run<'a>(sessions: &'a [Session], loop_id: &str) -> Option<&'a Session> {
    let origin = format!("{ORIGIN_PREFIX}{loop_id}");
    sessions
        .iter()
        .find(|s| s.origin.as_deref() == Some(origin.as_str()) && in_flight(s.status))
}

/// The loop's run that is still in flight, whichever kind it launches: a colony with origin
/// `loop:<id>`, or — for a map loop on one repository — a mapping colony with origin
/// `map:loop:<id>`. An org-wide map loop deliberately maps the next repository while the previous
/// is still drawing, so it is never held by one.
fn live_run_for<'a>(sessions: &'a [Session], l: &Loop) -> Option<&'a Session> {
    if l.kind == LoopKind::DiskCleanup {
        return None; // in-process: its own lock keeps it to one run at a time
    }
    if l.kind != LoopKind::Map {
        return live_run(sessions, &l.id);
    }
    if l.org_wide() {
        return None;
    }
    let origin = crate::maps::loop_origin(&l.id);
    sessions
        .iter()
        .find(|s| s.origin.as_deref() == Some(origin.as_str()) && in_flight(s.status))
}

/// A self-paced colony's `loop_next`: the delay clamped to [15 min, 24 h].
pub fn clamp_next(delay_minutes: u64) -> u64 {
    delay_minutes.clamp(NEXT_MIN_MINUTES, NEXT_MAX_MINUTES)
}

/// What a colony launched by a loop is told about it, after the loop's own prompt. `loop_tools`
/// says whether the agent module the colony launches on serves the loop MCP tools (issue #643):
/// with them the colony paces and stops the loop itself; without, the brief never mentions the
/// tools and a self-paced loop simply comes round again in 24 hours. `only` names the sources this
/// one run was narrowed to (a run-now `params.only`); a scheduled run names none.
pub fn loop_instructions(l: &Loop, run: u32, loop_tools: bool, only: &[String]) -> String {
    let pacing = if l.self_paced() && loop_tools {
        format!(
            "This loop is self-paced: before you finish, call loop_next with how many minutes from now the next run should start ({NEXT_MIN_MINUTES} to {NEXT_MAX_MINUTES}) and why. If you don't, it runs again in 24 hours."
        )
    } else if l.self_paced() {
        "This loop is self-paced: it runs again in 24 hours.".to_string()
    } else {
        "It runs on a fixed schedule; you don't need to schedule the next run.".to_string()
    };
    let mut says = vec![format!("You are run {run} of the loop \"{}\" on {}.", l.name, l.repo), pacing];
    if l.needs_github {
        says.push(format!(
            "This loop works on GitHub, and you have no GitHub token: read what the mothership fetched for you under {dir} — issues.json (open issues touched since last run), ci-failures.json (failed runs on the default branch) and merged-prs.json (pull requests merged since last run), each with a \"since\" timestamp; do not try `gh` yourself.",
            dir = crate::loop_github::CONTEXT_DIR
        ));
    }
    if !only.is_empty() {
        let named = only.iter().map(|id| format!("`{id}`")).collect::<Vec<_>>().join(", ");
        says.push(format!(
            "This run was started with parameters: only = {named} — work only on those."
        ));
    }
    if loop_tools {
        says.push("If the loop's goal is met, or it should not run again, call loop_stop with the reason.".to_string());
    }
    says.push(
        "Publish only when this run changed something worth a pull request; a run that finds nothing to do ends without one."
            .to_string(),
    );
    format!("{prompt}\n\n---\n{brief}", prompt = l.prompt.trim(), brief = says.join(" "))
}

pub struct LoopStore {
    pub(crate) loops: RwLock<Vec<Loop>>,
    file: PathBuf,
    persist: Mutex<()>,
}

impl LoopStore {
    pub fn new(config_dir: &FsPath) -> Self {
        let file = config_dir.join("loops.json");
        let mut loops = load(&file);
        // Every install has the disk-cleanup loop, off until its owner switches it on. It is saved
        // on its own, in `disk-cleanup.json`, so `loops.json` never holds a kind an older build
        // cannot read: a downgrade loses the cleanup's settings, never the operator's loops.
        loops.retain(|l| l.kind != LoopKind::DiskCleanup);
        if let Some(saved) = crate::disk_cleanup::load_saved(&config_dir.join(crate::disk_cleanup::FILE)) {
            loops.push(saved);
        }
        crate::disk_cleanup::ensure_builtin(&mut loops, Utc::now());
        Self {
            loops: RwLock::new(loops),
            file,
            persist: Mutex::new(()),
        }
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.persist.lock().await;
        let (builtin, data) = {
            let loops = self.loops.read().await;
            let (builtin, rest): (Vec<&Loop>, Vec<&Loop>) = loops.iter().partition(|l| l.kind == LoopKind::DiskCleanup);
            (
                builtin.first().map(serde_json::to_vec_pretty).transpose()?,
                serde_json::to_vec_pretty(&rest)?,
            )
        };
        write_atomic(&self.file, &data).await?;
        if let Some(builtin) = builtin
            && let Some(dir) = self.file.parent()
        {
            write_atomic(&dir.join(crate::disk_cleanup::FILE), &builtin).await?;
        }
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Option<Loop> {
        self.loops.read().await.iter().find(|l| l.id == id).cloned()
    }

    /// Applies `f` to the loop named `id`, then saves. `None` when there is no such loop.
    pub(crate) async fn update<R>(&self, id: &str, f: impl FnOnce(&mut Loop) -> R) -> Option<(Loop, R)> {
        let out = {
            let mut loops = self.loops.write().await;
            let l = loops.iter_mut().find(|l| l.id == id)?;
            let r = f(l);
            (l.clone(), r)
        };
        if let Err(e) = self.save().await {
            eprintln!("loops: could not save {}: {e:#}", self.file.display());
        }
        Some(out)
    }
}

fn load(path: &FsPath) -> Vec<Loop> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("loops: could not read {}: {e}; no loops loaded", path.display());
            Vec::new()
        }
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
            eprintln!("loops: could not parse {}: {e}; no loops loaded", path.display());
            Vec::new()
        }),
    }
}

#[derive(Clone, Deserialize)]
pub struct NewLoop {
    name: String,
    repo: String,
    /// A map loop needs no prompt: it launches the same mapping as the Map view. `default`, so its
    /// request may leave it out.
    #[serde(default)]
    prompt: String,
    cadence: Cadence,
    #[serde(default)]
    kind: LoopKind,
    /// Whether the loop's work is GitHub's (issue #778); see [`Loop::needs_github`].
    #[serde(default)]
    needs_github: bool,
    #[serde(default)]
    tz_offset_minutes: Option<i32>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    autopilot: Option<bool>,
    #[serde(default)]
    max_runs: Option<u32>,
    /// Minutes within which a run that failed for an infrastructure reason is run once more; 0
    /// switches the re-run off (issue #881).
    #[serde(default)]
    retry_failed_runs: Option<u32>,
    #[serde(default)]
    end_at: Option<DateTime<Utc>>,
    #[serde(default)]
    enabled: Option<bool>,
    /// The built-in disk-cleanup loop's settings; ignored on every other loop, and when absent the
    /// built-in keeps the ones it has.
    #[serde(default)]
    disk_cleanup: Option<crate::disk_cleanup::Settings>,
}

/// `owner/*`: every repository of the org, which only a map loop may hold. The owner part must
/// pass the same rules as any repository's.
fn is_org_wildcard(repo: &str) -> bool {
    repo.strip_suffix("/*").is_some_and(|owner| valid_repo(&format!("{owner}/x")))
}

/// A loop request, validated so a loop cannot hold something that would only fail when it fires.
fn loop_from(
    app: &App,
    req: NewLoop,
    id: String,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Loop, crate::AppError> {
    let bad = |m: &str| client_error(StatusCode::BAD_REQUEST, m);
    if req.kind == LoopKind::DiskCleanup {
        return Err(bad(
            "the disk cleanup loop is built in: switch it on or edit it (id disk-cleanup) instead of making another",
        ));
    }
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(bad("a loop needs a name of 1 to 80 characters"));
    }
    let repo = req.repo.trim().to_string();
    if req.kind != LoopKind::Map && is_org_wildcard(&repo) {
        return Err(bad("owner/* is only for map loops; a colony loop runs on one repository"));
    }
    if !valid_repo(&repo) && !is_org_wildcard(&repo) {
        return Err(bad(&format!("invalid repository name {repo:?}")));
    }
    let org = repo.split('/').next().unwrap_or_default().to_string();
    let prompt = if req.kind == LoopKind::Map {
        // The loop launches the same mapping as the Map view; whatever prompt came with it is ignored.
        String::new()
    } else {
        let prompt = req.prompt.trim().to_string();
        if prompt.is_empty() || prompt.len() > MAX_PROMPT {
            return Err(bad("a loop needs a prompt of 1 to 20,000 characters"));
        }
        prompt
    };
    req.cadence.check().map_err(|e| bad(&e))?;
    if req.max_runs == Some(0) {
        return Err(bad("max_runs must be at least 1, or left out"));
    }
    if req.end_at.is_some_and(|end| end <= now) {
        return Err(bad("the end date is already past"));
    }
    let model = sessions::launch_model(app, req.model.as_deref(), "model")?;
    let subagent_model = sessions::launch_model(app, req.subagent_model.as_deref(), "subagent model")?;
    let enabled = req.enabled.unwrap_or(true);
    // Only a colony loop runs in GitHub's domain; a map refresh or the disk cleanup has no use for
    // the context or the write tools.
    let needs_github = req.needs_github && req.kind == LoopKind::Colony;
    Ok(Loop {
        id,
        name,
        org,
        repo,
        // Only `create` attaches a token; an update keeps the loop's existing one.
        created_by_token: None,
        pending: Vec::new(),
        prompt,
        next_run_at: enabled.then(|| next_run_after(&req.cadence, now)),
        cadence: req.cadence,
        kind: req.kind,
        needs_github,
        tz_offset_minutes: req.tz_offset_minutes.unwrap_or(0),
        model,
        subagent_model,
        autopilot: req.autopilot.unwrap_or(true),
        max_runs: req.max_runs,
        retry_failed_runs: req.retry_failed_runs,
        end_at: req.end_at,
        enabled,
        runs: 0,
        last_run: None,
        last_note: None,
        ended_reason: None,
        created_at,
        disk_cleanup: None,
    })
}

/// The last run's outcome for the loop list (issue #881): the colony's status name, with a failed
/// run's failure class on it — `failed (transient_infra)`. `None` when the colony is gone.
fn run_outcome(sessions: &[Session], id: &str) -> Option<String> {
    let s = sessions.iter().find(|s| s.id == id)?;
    Some(match (s.status, s.failure_class) {
        (SessionStatus::Failed, Some(class)) => format!("{} ({})", s.status.as_str(), class.as_str()),
        _ => s.status.as_str().to_string(),
    })
}

/// Every loop, filtered to the caller's org/repo limits when a scoped token asks — the way the
/// colony list is filtered (issue #627).
pub async fn list(State(app): State<Shared>, scoped: Option<axum::Extension<ScopedToken>>) -> Json<Vec<Loop>> {
    let mut out: Vec<Loop> = {
        let loops = app.loops.loops.read().await;
        // The built-in disk cleanup is the host's housekeeping, not a token's business: owner only.
        loops
            .iter()
            .filter(|l| scoped.is_none() || l.kind != LoopKind::DiskCleanup)
            .filter(|l| scoped.as_ref().is_none_or(|axum::Extension(t)| t.covers(&l.org, &l.repo)))
            .cloned()
            .collect()
    };
    // Enrich each loop's last run with its colony's outcome, for `loop list`'s LAST column. The
    // outcome is not persisted: it is derived here, from the live session store, on every read.
    if out.iter().any(|l| l.last_run.is_some()) {
        let sessions = app.sessions.read().await;
        for l in &mut out {
            if let Some(last) = &mut l.last_run {
                last.outcome = run_outcome(&sessions, &last.session);
            }
        }
    }
    Json(out)
}

pub async fn create(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<ScopedToken>>,
    Json(req): Json<NewLoop>,
) -> ApiResult<Loop> {
    let now = Utc::now();
    let mut l = loop_from(&app, req, format!("loop_{}", short_id()), now, now)?;
    // A scoped launch token may keep its recurring work in a loop (issue #627): the loop records
    // the token, and each run is admitted against the token's limits, caps and budget and marked
    // as its external input, exactly like a launch the token made by hand. A map loop stays the
    // owner's: its runs go through the Map view's path, which carries no token, so refusing one
    // here is what keeps them from escaping both.
    if let Some(token) = scoped.map(|axum::Extension(t)| t) {
        if l.kind == LoopKind::Map {
            return Err(client_error(
                StatusCode::FORBIDDEN,
                "a scoped API token cannot create a map loop; its runs would launch outside the token's caps and marking",
            ));
        }
        if !token.covers(&l.org, &l.repo) {
            return Err(client_error(
                StatusCode::FORBIDDEN,
                &format!("this API token's org/repo limits do not include {}", l.repo),
            ));
        }
        l.created_by_token = Some(token.id);
    }
    app.loops.loops.write().await.push(l.clone());
    app.loops.save().await?;
    Ok(Json(l))
}

/// Replaces a loop's settings; its id, creation time, run count and last run are kept, and the next
/// run is recomputed. Re-enabling an ended loop clears why it ended.
pub async fn update(
    State(app): State<Shared>,
    Path(id): Path<String>,
    scoped: Option<axum::Extension<ScopedToken>>,
    Json(req): Json<NewLoop>,
) -> ApiResult<Loop> {
    let now = Utc::now();
    let existing = app
        .loops
        .get(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    // A token reshapes only the loop it created: firing or editing an owner's loop would launch
    // colonies under the owner's authority, uncapped and unmarked. The refusal reads as an unknown
    // id, the way an out-of-limits colony does, so the token can probe nothing (issue #627).
    if let Some(axum::Extension(token)) = &scoped
        && existing.created_by_token.as_deref() != Some(token.id.as_str())
    {
        return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
    }
    // Whoever edits it, a token's loop keeps its creator and stays a colony loop — a map loop's
    // runs would launch outside the token's caps and marking entirely.
    if existing.created_by_token.is_some() && req.kind == LoopKind::Map {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            "a loop created by an API token cannot become a map loop; delete it and create the map loop as the owner",
        ));
    }
    let mut l = if existing.kind == LoopKind::DiskCleanup {
        // The built-in loop: only its switch, cadence and settings change. A scoped token never
        // gets here (the check above reads it as unknown), so enabling it — and its host-level
        // category — is the owner's alone.
        crate::disk_cleanup::apply_update(
            &existing,
            req.enabled,
            req.cadence,
            req.tz_offset_minutes,
            req.disk_cleanup,
            now,
        )?
    } else {
        loop_from(&app, req, id.clone(), existing.created_at, now)?
    };
    if existing.kind == LoopKind::DiskCleanup {
        {
            let mut loops = app.loops.loops.write().await;
            let Some(slot) = loops.iter_mut().find(|x| x.id == id) else {
                return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
            };
            *slot = l.clone();
        }
        app.loops.save().await?;
        return Ok(Json(l));
    }
    l.runs = existing.runs;
    l.last_run = existing.last_run;
    l.last_note = existing.last_note;
    l.created_by_token = existing.created_by_token;
    // An edit by the token keeps the loop inside its org/repo limits.
    if let Some(axum::Extension(token)) = &scoped
        && !token.covers(&l.org, &l.repo)
    {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            &format!("this API token's org/repo limits do not include {}", l.repo),
        ));
    }
    if !l.enabled {
        l.ended_reason = existing.ended_reason;
    }
    l.check_limits();
    {
        let mut loops = app.loops.loops.write().await;
        let Some(slot) = loops.iter_mut().find(|x| x.id == id) else {
            return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
        };
        *slot = l.clone();
    }
    app.loops.save().await?;
    Ok(Json(l))
}

pub async fn delete(
    State(app): State<Shared>,
    Path(id): Path<String>,
    scoped: Option<axum::Extension<ScopedToken>>,
) -> ApiResult<Value> {
    // A token deletes only the loop it created; anything else reads as unknown (issue #627).
    if let Some(axum::Extension(token)) = &scoped {
        let mine = app
            .loops
            .get(&id)
            .await
            .is_some_and(|l| l.created_by_token.as_deref() == Some(token.id.as_str()));
        if !mine {
            return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
        }
    }
    if id == crate::disk_cleanup::LOOP_ID {
        return Err(client_error(
            StatusCode::CONFLICT,
            "the disk cleanup loop is built in: switch it off instead of deleting it",
        ));
    }
    let removed = {
        let mut loops = app.loops.loops.write().await;
        let before = loops.len();
        loops.retain(|l| l.id != id);
        before != loops.len()
    };
    if !removed {
        return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
    }
    app.loops.save().await?;
    Ok(Json(json!({"deleted": id})))
}

/// `?dry_run=1` on run-now: the disk-cleanup loop's preview.
#[derive(Deserialize, Default)]
pub struct RunNowQuery {
    #[serde(default)]
    dry_run: Option<String>,
}

impl RunNowQuery {
    fn dry_run(&self) -> bool {
        self.dry_run
            .as_deref()
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on" | ""))
    }
}

/// Run-now's optional body: `{"params": {"only": ["<source id>", …]}}` narrows this one run to
/// those sources (the data-refresh template's shard). No body at all — or an empty one, which is
/// what the cockpit's POST carries — runs the whole due set, exactly as a scheduled run does.
#[derive(Deserialize)]
pub struct RunNowBody {
    #[serde(default)]
    params: Option<RunParams>,
}

/// The parameters a single run may take. Anything else under `params` is refused, so a typo'd body
/// is an error at the door rather than a run that silently ignores it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunParams {
    #[serde(default)]
    only: Vec<String>,
}

/// The `only` ids a run-now body asks for, validated so a bad body is a 400 rather than a colony
/// launched against something it cannot parse. An absent `params` narrows nothing.
fn validated_only(params: Option<RunParams>) -> Result<Vec<String>, crate::AppError> {
    let Some(params) = params else {
        return Ok(Vec::new());
    };
    let bad = |m: String| client_error(StatusCode::BAD_REQUEST, &m);
    if params.only.is_empty() || params.only.len() > 100 {
        return Err(bad("params.only needs 1 to 100 source ids".into()));
    }
    for id in &params.only {
        let plain = id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'));
        let climbs = id.split('/').any(|segment| segment == "..");
        if id.is_empty() || id.chars().count() > 100 || !plain || climbs {
            return Err(bad(format!(
                "params.only: {id:?} is not a source id (1 to 100 characters of letters, digits, `.`, `_`, `:`, `/`, `-`; no `..` segment)"
            )));
        }
    }
    Ok(params.only)
}

/// The `only` ids a run-now body asks for. An empty — or whitespace-only — body is no parameters
/// whatever its content-type (the cockpit's POST sends `Content-Type: application/json` with
/// nothing after it); anything else must parse as the body above, and anything that does not, or
/// that asks for ids that cannot be source ids, is the caller's mistake, named in a 400.
fn only_of(body: &[u8]) -> Result<Vec<String>, crate::AppError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    let body: RunNowBody = serde_json::from_slice(body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e.to_string()))?;
    validated_only(body.params)
}

/// Starts the loop's next run now, whatever its schedule — still one run at a time. A colony or map
/// loop answers the colony it started; the disk-cleanup loop answers its run report, and with
/// `?dry_run=1` a preview of what a run would remove that removes nothing.
pub async fn run_now(
    State(app): State<Shared>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RunNowQuery>,
    scoped: Option<axum::Extension<ScopedToken>>,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    let l = app
        .loops
        .get(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    // A token runs only the loop it created: firing an owner's loop would launch colonies under
    // the owner's authority, uncapped and unmarked. The refusal reads as an unknown id.
    if let Some(axum::Extension(token)) = &scoped
        && l.created_by_token.as_deref() != Some(token.id.as_str())
    {
        return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
    }
    if l.kind == LoopKind::DiskCleanup {
        let report = crate::disk_cleanup::run(&app, "manual", query.dry_run(), crate::disk_cleanup::DiskProbe::Df).await?;
        return Ok(Json(serde_json::to_value(report).unwrap_or(Value::Null)));
    }
    if query.dry_run() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "only the disk cleanup loop has a dry run; a colony loop's run is a colony",
        ));
    }
    let only = only_of(&body)?;
    if let Some(live) = live_run_for(&app.sessions.read().await, &l) {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!(
                "the loop's previous run ({}) is still live; one run at a time — try again after it ends",
                live.id
            ),
        ));
    }
    let started = match l.kind {
        LoopKind::Map => fire_map(&app, &l, Utc::now()).await,
        LoopKind::Colony | LoopKind::DiskCleanup => launch(&app, &l, Utc::now(), &only).await.map(Some),
    };
    match started? {
        Some(session) => {
            crate::loop_history::record(
                &app,
                crate::loop_history::RunRecord::launched(&l.id, Utc::now(), "manual", &session),
            )
            .await;
            Ok(Json(serde_json::to_value(session).unwrap_or(Value::Null)))
        }
        None => Err(client_error(
            StatusCode::CONFLICT,
            "nothing to map right now; the loop's note says why",
        )),
    }
}

/// The loop's colonies, newest first. A scoped token reads the runs of any loop inside its
/// org/repo limits; outside them the loop reads as unknown, never as a refusal (issue #627).
pub async fn runs(
    State(app): State<Shared>,
    Path(id): Path<String>,
    scoped: Option<axum::Extension<ScopedToken>>,
) -> ApiResult<Vec<Session>> {
    let l = app
        .loops
        .get(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    if let Some(axum::Extension(token)) = &scoped
        && (!token.covers(&l.org, &l.repo) || l.kind == LoopKind::DiskCleanup)
    {
        return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
    }
    let origins = [format!("{ORIGIN_PREFIX}{id}"), crate::maps::loop_origin(&id)];
    let mut runs: Vec<Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.origin.as_deref().is_some_and(|o| origins.iter().any(|x| x.as_str() == o)))
        .cloned()
        .collect();
    runs.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    Ok(Json(runs))
}

/// The token a loop's runs run under (issue #627): the loop stores only the creating token's id,
/// and each firing rebuilds the token from the registry — the same view the guard attaches — so
/// its org/repo limits, concurrency cap and budget admit the run and the colony is marked as the
/// token's external input. A revoked token ends the loop and refuses the run: the scheduler must
/// stop trying, and the loop's record says why.
async fn run_token(app: &Shared, l: &Loop) -> Result<Option<ScopedToken>, crate::AppError> {
    let Some(id) = l.created_by_token.as_deref() else {
        return Ok(None);
    };
    match app.api_tokens.scoped(id).await {
        Some(token) => Ok(Some(token)),
        None => {
            app.loops
                .update(&l.id, |x| x.end("its API token was revoked".to_string()))
                .await;
            Err(client_error(
                StatusCode::CONFLICT,
                &format!("the loop's API token ({id}) was revoked; the loop is ended"),
            ))
        }
    }
}

/// Whether a launch must clear the GitHub preflight first, and whether a session is a run of such a
/// loop (issue #778). `loop_from` already drops the need from anything but a colony loop, so this is
/// only ever true for one the operator asked for; pure, so the decision is tested without `gh`.
pub(crate) fn needs_preflight(l: &Loop) -> bool {
    l.needs_github && l.kind == LoopKind::Colony
}

/// Launches a colony loop's run through the normal admission path and books the next. `only` names
/// the sources this run was narrowed to (a run-now `params.only`); a scheduled run narrows nothing.
async fn launch(app: &Shared, l: &Loop, now: DateTime<Utc>, only: &[String]) -> Result<Session, crate::AppError> {
    let run = l.runs + 1;
    let scoped = run_token(app, l).await?;
    let session = start_run(app, l, run, scoped, only).await?;
    app.loops.update(&l.id, |x| x.record_run(&session.id, now)).await;
    Ok(session)
}

/// The colony-creating half of a launch, shared by the tick and a re-run (issue #881): admits the
/// run through the same path a hand launch takes, under the loop's token when it has one. `run` is
/// the number the brief names.
async fn start_run(
    app: &Shared,
    l: &Loop,
    run: u32,
    scoped: Option<ScopedToken>,
    only: &[String],
) -> Result<Session, crate::AppError> {
    // Issue #778: a loop whose work is GitHub's launches no colony until the mothership can reach
    // the repository — otherwise the colony only parks on a question. The note is recorded here so
    // run-now and a re-run (issue #881) get it too; the scheduler's error handler adds the tick's
    // own wording.
    if needs_preflight(l)
        && let Err(e) = crate::loop_github::preflight(app, l).await
    {
        let note = format!("{e:#}");
        app.loops.update(&l.id, |x| x.last_note = Some(note.clone())).await;
        return Err(client_error(StatusCode::CONFLICT, &note));
    }
    // Whether the brief may name the loop tools: the same resolution `sessions::create` is about
    // to launch on — the repository's org's pick, else the install's (issue #643).
    let owner = l.repo.split('/').next().unwrap_or_default();
    let modules = app.modules.read().await.clone();
    let loop_tools = app
        .agents
        .iter()
        .find(|a| a.id == crate::orgs::effective_agent_module(&app.org_settings(owner), &modules))
        .is_some_and(|a| a.loop_tools);
    let body = json!({
        "repo": l.repo,
        "title": format!("{} (loop, run {run})", l.name),
        "instructions": loop_instructions(l, run, loop_tools, only),
        "autopilot": l.autopilot,
        "allow_duplicate": true,
        "origin": format!("{ORIGIN_PREFIX}{}", l.id),
        "model_override": l.model,
        "subagent_model_override": l.subagent_model,
    });
    let req: NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), scoped.map(axum::Extension), HeaderMap::new(), Json(req)).await?;
    Ok(session)
}

/// Re-runs a colony loop's run that ended `Failed` for an infrastructure reason, once (issue #881):
/// within the loop's window a fresh run is launched without `record_run`, so the run count and the
/// next run are untouched. The record's `retried` flag allows one re-run at most, and a live run
/// holds it off until the next tick.
async fn retry_failed_runs(app: &Shared, now: DateTime<Utc>) {
    let sessions = app.sessions.read().await.clone();
    // A snapshot, so the read lock is not held while `retry_run` takes the loops lock to book the
    // re-run.
    let loops = app.loops.loops.read().await.clone();
    for l in loops {
        if !l.enabled {
            continue;
        }
        // Only a plain colony loop is re-run: that is the kind `launch` serves. A map loop's runs
        // are mapping colonies launched another way, and the built-in cleanup runs in-process with
        // no colony at all — neither is `start_run`'s to re-launch.
        if l.kind != LoopKind::Colony {
            continue;
        }
        let Some(last) = &l.last_run else { continue };
        if last.retried {
            continue;
        }
        let Some(window) = l.retry_window() else { continue };
        if now.signed_duration_since(last.at) > window {
            continue;
        }
        let failed_transiently = sessions.iter().find(|s| s.id == last.session).is_some_and(|s| {
            s.status == SessionStatus::Failed && s.failure_class == Some(crate::retry::FailureClass::TransientInfra)
        });
        if !failed_transiently || live_run_for(&sessions, &l).is_some() {
            continue;
        }
        retry_run(app, &l, last, now).await;
    }
}

/// Launches the re-run of `last` and books it as the loop's last run, keeping the original time and
/// marking it retried so no second re-run follows.
async fn retry_run(app: &Shared, l: &Loop, last: &LastRun, now: DateTime<Utc>) {
    let scoped = match run_token(app, l).await {
        Ok(token) => token,
        // run_token ends the loop and records why; nothing to re-run.
        Err(_) => return,
    };
    // Re-checked against a fresh read: the snapshot `retry_failed_runs` filtered on may be older
    // than this launch, and a run started in between (a hand run-now, say) must hold the re-run off.
    if live_run_for(&app.sessions.read().await, l).is_some() {
        return;
    }
    match start_run(app, l, l.runs, scoped, &[]).await {
        Ok(session) => {
            crate::loop_history::record(app, crate::loop_history::RunRecord::launched(&l.id, now, "retry", &session)).await;
            let fresh = session.id.clone();
            app.loops
                .update(&l.id, |x| {
                    x.last_run = Some(LastRun {
                        session: fresh.clone(),
                        at: last.at,
                        retried: true,
                        outcome: None,
                    });
                })
                .await;
            eprintln!(
                "loops: re-running loop {}'s run {}, which failed for an infrastructure reason",
                l.name, last.session
            );
            app.session_log(
                &session.id,
                "info",
                format!(
                    "loop: re-running the run of loop \"{}\" ({}) that failed for an infrastructure reason",
                    l.name, last.session
                ),
            )
            .await;
        }
        Err(e) => {
            // A refusal — a parallel limit, the token's cap — is not the run's end: the next tick
            // tries again while the window is open, and the loop's note says why.
            let message = e.message().to_string();
            app.loops
                .update(&l.id, |x| {
                    x.last_note = Some(format!(
                        "could not re-run the failed run ({}) at {}: {message}",
                        last.session,
                        now.format("%H:%M UTC")
                    ));
                })
                .await;
        }
    }
}

/// One step of an org-wide map loop's cycle. An empty `pending` starts a cycle from the fresh
/// repository list (listed only then, so repositories added later join in); otherwise the next
/// repository in line goes. While repositories remain after it, the loop runs again after
/// [`MAP_STAGGER_MINUTES`]; after the last one, at the cadence's next slot. Pure, so the stagger
/// is tested without a colony.
fn fan_out_step(
    pending: &mut Vec<String>,
    fresh_repos: impl FnOnce() -> Vec<String>,
    cadence: &Cadence,
    now: DateTime<Utc>,
) -> (Option<String>, DateTime<Utc>) {
    if pending.is_empty() {
        *pending = fresh_repos();
    }
    let repo = if pending.is_empty() { None } else { Some(pending.remove(0)) };
    let next = if pending.is_empty() {
        next_run_after(cadence, now)
    } else {
        now + ChronoDuration::minutes(MAP_STAGGER_MINUTES)
    };
    (repo, next)
}

/// One firing of a map loop: map its repository — or, org-wide, the next repository of the cycle —
/// through the same admission path as the Map view, and book the next firing. `Ok(None)` when
/// nothing was mapped; the loop's note says why.
async fn fire_map(app: &Shared, l: &Loop, now: DateTime<Utc>) -> Result<Option<Session>, crate::AppError> {
    if !l.org_wide() {
        let session = crate::maps::launch(app, &l.repo, &crate::maps::loop_origin(&l.id)).await?;
        app.loops.update(&l.id, |x| x.record_run(&session.id, now)).await;
        return Ok(Some(session));
    }
    let mut pending = l.pending.clone();
    // A new cycle re-lists the org, so repositories added since the last one are included.
    let fresh = if pending.is_empty() {
        // A switched-off workspace refuses every colony it would start, so the cycle would spend a
        // stagger per repository saying so: skip the whole cycle with the one note instead.
        if !crate::orgs::org_enabled(&app.org_settings(&l.org)) {
            app.loops
                .update(&l.id, |x| {
                    x.next_run_at = Some(next_run_after(&x.cadence, now));
                    x.last_note = Some(format!("the {} workspace is switched off; nothing mapped this cycle", x.org));
                })
                .await;
            return Ok(None);
        }
        match crate::deps::all_org_repos(app, &l.org).await {
            Ok(repos) => repos,
            Err(e) => {
                app.loops
                    .update(&l.id, |x| {
                        x.next_run_at = Some(next_run_after(&x.cadence, now));
                        x.last_note = Some(format!("could not list {}: {e:#}", x.org));
                    })
                    .await;
                return Ok(None);
            }
        }
    } else {
        Vec::new()
    };
    let (repo, next) = fan_out_step(&mut pending, || fresh, &l.cadence, now);
    // One repository's refusal (archify missing, parallel limits, a disabled org) does not stop the
    // cycle: the note says so and the next firing moves on to the next repository.
    let mut launched = None;
    let mut note = None;
    match repo.as_deref() {
        Some(repo) => match crate::maps::launch(app, repo, &crate::maps::loop_origin(&l.id)).await {
            Ok(session) => launched = Some(session),
            Err(e) => {
                note = Some(format!(
                    "could not map {repo} at {}: {}",
                    now.format("%H:%M UTC"),
                    e.message()
                ))
            }
        },
        None => note = Some(format!("nothing to map in {}", l.org)),
    }
    app.loops
        .update(&l.id, |x| {
            x.pending = pending;
            if let Some(session) = &launched {
                x.record_run(&session.id, now);
            }
            // After record_run, which books the cadence's next slot: until the cycle is done, what
            // comes next is the next repository, a stagger away. A loop the limits just ended has
            // no next run, stagger or not.
            if x.enabled {
                x.next_run_at = Some(next);
            }
            if let Some(note) = note {
                x.last_note = Some(note);
            }
            x.check_limits();
        })
        .await;
    Ok(launched)
}

/// A map-refresh loop's colony ended: its outcome is the loop's latest news (`maps.rs` calls this).
pub(crate) async fn note_refresh(app: &App, loop_id: &str, note: &str) {
    app.loops.update(loop_id, |l| l.last_note = Some(note.to_string())).await;
}

/// Fires every due loop: launches it, or skips while its previous run is still live.
pub(crate) async fn fire_due(app: &Shared, now: DateTime<Utc>) {
    // The disk-cleanup loop's free-space trigger books it for now when the disk runs low.
    crate::disk_cleanup::check_trigger(app, now).await;
    let due_ids = due(&app.loops.loops.read().await, now);
    for id in due_ids {
        let Some(l) = app.loops.get(&id).await else { continue };
        if l.kind == LoopKind::DiskCleanup {
            // In the background: a big cleanup must not hold every other loop's minute tick.
            app.loops
                .update(&id, |x| x.next_run_at = Some(next_run_after(&x.cadence, now)))
                .await;
            let app = app.clone();
            tokio::spawn(async move { crate::disk_cleanup::fire(&app, now).await });
            continue;
        }
        let live = live_run_for(&app.sessions.read().await, &l).map(|s| s.id.clone());
        match plan_tick(&l, live.as_deref(), now) {
            Tick::Launch => {
                let started = match l.kind {
                    LoopKind::Map => fire_map(app, &l, now).await,
                    LoopKind::Colony | LoopKind::DiskCleanup => launch(app, &l, now, &[]).await.map(Some),
                };
                if let Ok(Some(session)) = &started {
                    crate::loop_history::record(app, crate::loop_history::RunRecord::launched(&id, now, "schedule", session))
                        .await;
                }
                if let Err(e) = started {
                    let message = e.message().to_string();
                    crate::loop_history::record(
                        app,
                        crate::loop_history::RunRecord::refused(&id, now, &format!("could not start: {message}")),
                    )
                    .await;
                    app.loops
                        .update(&id, |x| {
                            // A launch that ended the loop itself — its API token was revoked —
                            // keeps that end; any other refusal is a skipped tick with the reason,
                            // tried again at the next slot.
                            if x.enabled {
                                x.last_note = Some(format!("could not start at {}: {message}", now.format("%H:%M UTC")));
                                x.next_run_at = Some(next_run_after(&x.cadence, now));
                            }
                            x.check_limits();
                        })
                        .await;
                }
            }
            Tick::Skip { until, note } => {
                app.loops
                    .update(&id, |x| {
                        x.last_note = Some(note);
                        x.next_run_at = Some(until);
                        x.check_limits();
                    })
                    .await;
            }
        }
    }
    // Separate from the due schedule: a run that has already fired may be re-run once when it
    // failed for an infrastructure reason (issue #881).
    retry_failed_runs(app, now).await;
}

/// A colony of a self-paced loop names its next run (`loop_next`).
pub(crate) async fn on_next(app: &Shared, session_id: &str, delay_minutes: u64, reason: &str) {
    let Some(loop_id) = colony_loop(app, session_id).await else {
        return;
    };
    let minutes = clamp_next(delay_minutes);
    let now = Utc::now();
    let note = reason.trim().chars().take(300).collect::<String>();
    let updated = app
        .loops
        .update(&loop_id, |l| {
            if !l.enabled {
                return false;
            }
            if l.self_paced() {
                l.next_run_at = Some(now + ChronoDuration::minutes(minutes as i64));
                l.last_note = Some(format!("the colony chose the next run in {}: {note}", human_minutes(minutes)));
                l.check_limits();
            } else {
                l.last_note = Some(format!(
                    "the colony asked for a run in {} ({note}), but this loop runs on its own schedule",
                    human_minutes(minutes)
                ));
            }
            true
        })
        .await;
    if let Some((_, true)) = updated {
        app.session_log(
            session_id,
            "info",
            format!("loop: next run in {} — {note}", human_minutes(minutes)),
        )
        .await;
    }
}

/// A colony ends its loop (`loop_stop`).
pub(crate) async fn on_stop(app: &Shared, session_id: &str, reason: &str) {
    let Some(loop_id) = colony_loop(app, session_id).await else {
        return;
    };
    let reason = reason.trim().chars().take(300).collect::<String>();
    app.loops
        .update(&loop_id, |l| l.end(format!("stopped by the colony: {reason}")))
        .await;
    app.session_log(session_id, "info", format!("loop: stopped — {reason}")).await;
}

async fn colony_loop(app: &Shared, session_id: &str) -> Option<String> {
    let s = app.session(session_id).await?;
    loop_id_of(s.origin.as_deref()?).map(str::to_string)
}

fn human_minutes(minutes: u64) -> String {
    if minutes.is_multiple_of(60) {
        let h = minutes / 60;
        format!("{h} {}", if h == 1 { "hour" } else { "hours" })
    } else if minutes > 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} minutes")
    }
}

/// Whether a colony was launched by a self-paced loop, for its runner's tool set.
pub(crate) async fn colony_self_paced(app: &App, origin: Option<&str>) -> Option<bool> {
    let id = loop_id_of(origin?)?;
    Some(app.loops.get(id).await.is_some_and(|l| l.self_paced()))
}

/// The loop scheduler, spawned at startup: once a minute, fire whatever is due.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        fire_due(&app, Utc::now()).await;
    }
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    let loop_ticks = app.clone();
    tokio::spawn(async move { run(loop_ticks).await });
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/loops", routing::get(list).post(create))
        .route("/api/loops/{id}", routing::put(update).delete(delete))
        .route("/api/loops/{id}/run-now", routing::post(run_now))
        .route("/api/loops/{id}/runs", routing::get(runs))
}

#[cfg(test)]
mod tests;
