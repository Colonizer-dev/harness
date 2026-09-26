//! Loops: a saved prompt on a repository that launches a colony on a schedule — the mothership's
//! version of Claude Code's `/loop`. A loop runs on a fixed cadence (every N minutes, daily, weekly,
//! monthly) or self-paced, where each colony names its own next run with the `loop_next` tool, and
//! any colony can end its loop with `loop_stop`. One run at a time: a tick that finds the previous
//! run still live skips and says so. Loops are operator configuration, saved to
//! `<config_dir>/loops.json`; their runs are ordinary colonies tagged `origin: "loop:<id>"`.

use crate::schedule::{Cadence, next_run_after};
use crate::sessions::{self, NewSession, Session, SessionStatus};
use crate::{
    ApiResult, App, Shared, client_error,
    util::{short_id, valid_repo, write_atomic},
};
use anyhow::Result;
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
const MAX_PROMPT: usize = 20_000;

/// The loop a colony belongs to, from its origin tag.
pub fn loop_id_of(origin: &str) -> Option<&str> {
    origin.strip_prefix(ORIGIN_PREFIX).filter(|id| !id.is_empty())
}

/// The last colony a loop launched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub session: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    pub id: String,
    pub name: String,
    pub org: String,
    pub repo: String,
    pub prompt: String,
    pub cadence: Cadence,
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
}

impl Loop {
    pub fn self_paced(&self) -> bool {
        matches!(self.cadence, Cadence::SelfPaced {})
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
    status == SessionStatus::Queued || status.busy()
}

/// The loop's run that is still in flight, if any.
pub fn live_run<'a>(sessions: &'a [Session], loop_id: &str) -> Option<&'a Session> {
    let origin = format!("{ORIGIN_PREFIX}{loop_id}");
    sessions
        .iter()
        .find(|s| s.origin.as_deref() == Some(origin.as_str()) && in_flight(s.status))
}

/// A self-paced colony's `loop_next`: the delay clamped to [15 min, 24 h].
pub fn clamp_next(delay_minutes: u64) -> u64 {
    delay_minutes.clamp(NEXT_MIN_MINUTES, NEXT_MAX_MINUTES)
}

/// What a colony launched by a loop is told about it, after the loop's own prompt.
pub fn loop_instructions(l: &Loop, run: u32) -> String {
    let pacing = if l.self_paced() {
        format!(
            "This loop is self-paced: before you finish, call loop_next with how many minutes from now the next run should start ({NEXT_MIN_MINUTES} to {NEXT_MAX_MINUTES}) and why. If you don't, it runs again in 24 hours."
        )
    } else {
        "It runs on a fixed schedule; you don't need to schedule the next run.".to_string()
    };
    format!(
        "{prompt}\n\n---\nYou are run {run} of the loop \"{name}\" on {repo}. {pacing} If the loop's goal is met, or it should not run again, call loop_stop with the reason. Publish only when this run changed something worth a pull request; a run that finds nothing to do ends without one.",
        prompt = l.prompt.trim(),
        name = l.name,
        repo = l.repo,
    )
}

pub struct LoopStore {
    loops: RwLock<Vec<Loop>>,
    file: PathBuf,
    persist: Mutex<()>,
}

impl LoopStore {
    pub fn new(config_dir: &FsPath) -> Self {
        let file = config_dir.join("loops.json");
        Self {
            loops: RwLock::new(load(&file)),
            file,
            persist: Mutex::new(()),
        }
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.loops.read().await)?;
        write_atomic(&self.file, &data).await
    }

    pub async fn get(&self, id: &str) -> Option<Loop> {
        self.loops.read().await.iter().find(|l| l.id == id).cloned()
    }

    /// Applies `f` to the loop named `id`, then saves. `None` when there is no such loop.
    async fn update<R>(&self, id: &str, f: impl FnOnce(&mut Loop) -> R) -> Option<(Loop, R)> {
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
    prompt: String,
    cadence: Cadence,
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
    #[serde(default)]
    end_at: Option<DateTime<Utc>>,
    #[serde(default)]
    enabled: Option<bool>,
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
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(bad("a loop needs a name of 1 to 80 characters"));
    }
    let repo = req.repo.trim().to_string();
    if !valid_repo(&repo) {
        return Err(bad(&format!("invalid repository name {repo:?}")));
    }
    let org = repo.split('/').next().unwrap_or_default().to_string();
    let prompt = req.prompt.trim().to_string();
    if prompt.is_empty() || prompt.len() > MAX_PROMPT {
        return Err(bad("a loop needs a prompt of 1 to 20,000 characters"));
    }
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
    Ok(Loop {
        id,
        name,
        org,
        repo,
        prompt,
        next_run_at: enabled.then(|| next_run_after(&req.cadence, now)),
        cadence: req.cadence,
        tz_offset_minutes: req.tz_offset_minutes.unwrap_or(0),
        model,
        subagent_model,
        autopilot: req.autopilot.unwrap_or(true),
        max_runs: req.max_runs,
        end_at: req.end_at,
        enabled,
        runs: 0,
        last_run: None,
        last_note: None,
        ended_reason: None,
        created_at,
    })
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Loop>> {
    Json(app.loops.loops.read().await.clone())
}

pub async fn create(State(app): State<Shared>, Json(req): Json<NewLoop>) -> ApiResult<Loop> {
    let now = Utc::now();
    let l = loop_from(&app, req, format!("loop_{}", short_id()), now, now)?;
    app.loops.loops.write().await.push(l.clone());
    app.loops.save().await?;
    Ok(Json(l))
}

/// Replaces a loop's settings; its id, creation time, run count and last run are kept, and the next
/// run is recomputed. Re-enabling an ended loop clears why it ended.
pub async fn update(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<NewLoop>) -> ApiResult<Loop> {
    let now = Utc::now();
    let existing = app
        .loops
        .get(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    let mut l = loop_from(&app, req, id.clone(), existing.created_at, now)?;
    l.runs = existing.runs;
    l.last_run = existing.last_run;
    l.last_note = existing.last_note;
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

pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
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

/// Starts the loop's next run now, whatever its schedule — still one run at a time.
pub async fn run_now(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let l = app
        .loops
        .get(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such loop"))?;
    if let Some(live) = live_run(&app.sessions.read().await, &id) {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("the loop's previous run ({}) is still live; one run at a time", live.id),
        ));
    }
    launch(&app, &l, Utc::now()).await.map(Json)
}

/// The loop's colonies, newest first.
pub async fn runs(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Vec<Session>> {
    if app.loops.get(&id).await.is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no such loop"));
    }
    let origin = format!("{ORIGIN_PREFIX}{id}");
    let mut runs: Vec<Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.origin.as_deref() == Some(origin.as_str()))
        .cloned()
        .collect();
    runs.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    Ok(Json(runs))
}

/// Launches one run through the normal admission path and books the next.
async fn launch(app: &Shared, l: &Loop, now: DateTime<Utc>) -> Result<Session, crate::AppError> {
    let run = l.runs + 1;
    let body = json!({
        "repo": l.repo,
        "title": format!("{} (loop, run {run})", l.name),
        "instructions": loop_instructions(l, run),
        "autopilot": l.autopilot,
        "allow_duplicate": true,
        "origin": format!("{ORIGIN_PREFIX}{}", l.id),
        "model_override": l.model,
        "subagent_model_override": l.subagent_model,
    });
    let req: NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), None, Json(req)).await?;
    app.loops.update(&l.id, |x| x.record_run(&session.id, now)).await;
    Ok(session)
}

/// Fires every due loop: launches it, or skips while its previous run is still live.
pub(crate) async fn fire_due(app: &Shared, now: DateTime<Utc>) {
    let due_ids = due(&app.loops.loops.read().await, now);
    for id in due_ids {
        let Some(l) = app.loops.get(&id).await else { continue };
        let live = live_run(&app.sessions.read().await, &id).map(|s| s.id.clone());
        match plan_tick(&l, live.as_deref(), now) {
            Tick::Launch => {
                if let Err(e) = launch(app, &l, now).await {
                    let message = e.message().to_string();
                    app.loops
                        .update(&id, |x| {
                            x.last_note = Some(format!("could not start at {}: {message}", now.format("%H:%M UTC")));
                            x.next_run_at = Some(next_run_after(&x.cadence, now));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn a_loop(cadence: Cadence) -> Loop {
        Loop {
            id: "loop_a".into(),
            name: "Triage".into(),
            org: "acme".into(),
            repo: "acme/web".into(),
            prompt: "Triage new issues".into(),
            cadence,
            tz_offset_minutes: 120,
            model: None,
            subagent_model: None,
            autopilot: true,
            max_runs: None,
            end_at: None,
            enabled: true,
            next_run_at: Some(utc(2026, 9, 24, 9, 0)),
            runs: 0,
            last_run: None,
            last_note: None,
            ended_reason: None,
            created_at: utc(2026, 9, 1, 0, 0),
        }
    }

    #[test]
    fn origin_tags_name_their_loop() {
        assert_eq!(loop_id_of("loop:loop_a"), Some("loop_a"));
        assert_eq!(loop_id_of("loop:"), None);
        assert_eq!(loop_id_of("burn_down"), None);
    }

    #[test]
    fn due_loops_are_enabled_and_not_in_the_future() {
        let now = utc(2026, 9, 24, 9, 0);
        let mut later = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        later.id = "later".into();
        later.next_run_at = Some(now + ChronoDuration::minutes(1));
        let mut off = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        off.id = "off".into();
        off.enabled = false;
        let mut ended = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        ended.id = "ended".into();
        ended.next_run_at = None;
        let now_due = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        assert_eq!(due(&[later, off, ended, now_due], now), vec!["loop_a".to_string()]);
    }

    #[test]
    fn a_live_previous_run_skips_the_tick_and_says_so() {
        let now = utc(2026, 9, 24, 9, 0);
        let daily = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        assert_eq!(plan_tick(&daily, None, now), Tick::Launch);
        match plan_tick(&daily, Some("abc123"), now) {
            Tick::Skip { until, note } => {
                assert_eq!(until, utc(2026, 9, 25, 9, 0), "a fixed loop waits for its next slot");
                assert!(note.contains("abc123") && note.contains("still live"), "{note}");
            }
            other => panic!("expected a skip, got {other:?}"),
        }
        let paced = a_loop(Cadence::SelfPaced {});
        match plan_tick(&paced, Some("abc123"), now) {
            Tick::Skip { until, .. } => assert_eq!(until, now + ChronoDuration::minutes(15), "self-paced retries soon"),
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn only_in_flight_colonies_count_as_the_live_run() {
        let mut sessions = Vec::new();
        for (id, status) in [
            ("done", SessionStatus::Merged),
            ("opened", SessionStatus::PrOpened),
            ("stopped", SessionStatus::Stopped),
        ] {
            let mut s = colony("acme", status);
            s.id = id.into();
            s.origin = Some("loop:loop_a".into());
            sessions.push(s);
        }
        assert!(live_run(&sessions, "loop_a").is_none(), "finished runs do not hold the loop");
        let mut other = colony("acme", SessionStatus::Running);
        other.id = "other-loop".into();
        other.origin = Some("loop:loop_b".into());
        sessions.push(other);
        assert!(
            live_run(&sessions, "loop_a").is_none(),
            "another loop's run is not this one's"
        );
        let mut queued = colony("acme", SessionStatus::Queued);
        queued.id = "queued".into();
        queued.origin = Some("loop:loop_a".into());
        sessions.push(queued);
        assert_eq!(live_run(&sessions, "loop_a").map(|s| s.id.as_str()), Some("queued"));
    }

    #[test]
    fn a_run_books_the_next_and_limits_end_the_loop() {
        let now = utc(2026, 9, 24, 9, 0);
        let mut l = a_loop(Cadence::Interval { minutes: 60 });
        l.max_runs = Some(2);
        l.record_run("s1", now);
        assert_eq!((l.runs, l.enabled), (1, true));
        assert_eq!(l.next_run_at, Some(utc(2026, 9, 24, 10, 0)));
        l.record_run("s2", utc(2026, 9, 24, 10, 0));
        assert!(!l.enabled && l.next_run_at.is_none());
        assert_eq!(l.ended_reason.as_deref(), Some("finished: ran 2 times"));

        let mut dated = a_loop(Cadence::Daily { hour: 9, minute: 0 });
        dated.end_at = Some(utc(2026, 9, 25, 8, 0));
        dated.record_run("s1", now);
        assert!(!dated.enabled, "the next run (tomorrow 09:00) is past the end date");
        assert!(dated.ended_reason.unwrap().starts_with("finished: its end date"));
    }

    #[test]
    fn next_delays_are_clamped_and_described() {
        assert_eq!(clamp_next(1), 15);
        assert_eq!(clamp_next(90), 90);
        assert_eq!(clamp_next(10_000), 1440);
        assert_eq!(human_minutes(60), "1 hour");
        assert_eq!(human_minutes(90), "1h 30m");
        assert_eq!(human_minutes(30), "30 minutes");
    }

    #[test]
    fn colonies_are_told_their_run_and_how_to_pace_or_stop() {
        let paced = a_loop(Cadence::SelfPaced {});
        let text = loop_instructions(&paced, 3);
        assert!(text.starts_with("Triage new issues"));
        assert!(text.contains("run 3 of the loop \"Triage\""));
        assert!(text.contains("loop_next") && text.contains("loop_stop"));
        let fixed = loop_instructions(&a_loop(Cadence::Daily { hour: 9, minute: 0 }), 1);
        assert!(!fixed.contains("call loop_next") && fixed.contains("loop_stop"));
    }

    #[tokio::test]
    async fn loops_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("colonizer-loops-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = LoopStore::new(&dir);
        store.loops.write().await.push(a_loop(Cadence::Weekly {
            weekday: 3,
            hour: 9,
            minute: 30,
        }));
        store.save().await.unwrap();
        let again = LoopStore::new(&dir);
        let loaded = again.get("loop_a").await.unwrap();
        assert_eq!(
            loaded,
            a_loop(Cadence::Weekly {
                weekday: 3,
                hour: 9,
                minute: 30
            })
        );
        let raw: Value = serde_json::from_slice(&std::fs::read(dir.join("loops.json")).unwrap()).unwrap();
        assert_eq!(
            raw[0]["cadence"],
            json!({"every": "weekly", "weekday": 3, "hour": 9, "minute": 30})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_colony_paces_and_stops_its_own_loop() {
        let root = std::env::temp_dir().join(format!("colonizer-loops-app-{}", short_id()));
        let app = crate::tests::test_app(&root);
        let mut paced = a_loop(Cadence::SelfPaced {});
        paced.next_run_at = None;
        app.loops.loops.write().await.push(paced);
        let mut s = colony("acme", SessionStatus::Running);
        s.id = "run1".into();
        s.origin = Some("loop:loop_a".into());
        app.sessions.write().await.push(s);

        let before = Utc::now();
        on_next(&app, "run1", 120, "CI reruns at 11").await;
        let l = app.loops.get("loop_a").await.unwrap();
        let next = l.next_run_at.expect("booked");
        assert!(next >= before + ChronoDuration::minutes(119) && next <= Utc::now() + ChronoDuration::minutes(121));
        assert!(l.last_note.unwrap().contains("2 hours: CI reruns at 11"));

        on_stop(&app, "run1", "all flakes fixed").await;
        let l = app.loops.get("loop_a").await.unwrap();
        assert!(!l.enabled && l.next_run_at.is_none());
        assert_eq!(l.ended_reason.as_deref(), Some("stopped by the colony: all flakes fixed"));

        // A colony no loop launched cannot touch loops.
        let mut stray = colony("acme", SessionStatus::Running);
        stray.id = "stray".into();
        app.sessions.write().await.push(stray);
        on_stop(&app, "stray", "nope").await;
        let _ = std::fs::remove_dir_all(root);
    }
}
