//! Red-team runs (§6.7): N hunters with distinct briefs raid one repository at once. A run launches
//! only while no colony is live — the "nest is empty" — either now (start-now) or, for an armed run,
//! when the background tick sees the nest empty. The tick walks runs armed → running (or waiting
//! while every launched hunter is still queued) → draining → done; a stop marks the run `stopped`
//! and stops the hunters it started that are still live or queued. A run with findings that lands
//! `done` launches one synthesis colony to merge the hunters' findings into a single ranked report.

#[cfg(not(test))]
use crate::sessions::{self, NewSession};
use crate::{
    ApiResult, App, Shared, client_error, findings,
    sessions::{Session, SessionStatus},
    util::{short_id, truncate, valid_repo, write_atomic},
};
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
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};

/// How wide a swarm may be.
const MAX_SWARM: usize = 8;

/// The `Session.origin` a hunter carries (§6.7): how the run's stop finds its colonies, and how the
/// event origin resolver (`events.rs`) reads a hunter's launch back.
pub(crate) const REDTEAM_ORIGIN: &str = "redteam";
/// The swarm size when the request does not name one.
const DEFAULT_SWARM: usize = 3;
/// The module a hunter runs when the request names none.
const DEFAULT_MODULE: &str = "general";
/// The modules a run may name. Only the built-in one exists until module manifests land (#216); an
/// unknown name is refused rather than run as if it were the default.
const KNOWN_MODULES: &[&str] = &[DEFAULT_MODULE];
/// Who hunts: the built-in swarm of colony hunters. The external hunter modules (Strix, Shannon —
/// hunters.rs) install and probe, but a run does not drive their scans yet, so naming one is refused
/// with the reason rather than quietly running the swarm instead.
const DEFAULT_HUNTER: &str = "swarm";

/// The eight focus areas a run's hunters are drawn from, cycled as `i % 8`. Each brief names its own
/// focus and lists the others, so the swarm keeps out of one another's way.
const FOCUSES: [&str; 8] = [
    "error handling and edge cases",
    "concurrency and race conditions",
    "input validation and injection",
    "resource leaks and exhaustion",
    "auth and permission boundaries",
    "core-flow logic errors",
    "silent failures and swallowed errors",
    "API and contract mismatches",
];

/// Where a run is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedTeamState {
    /// Created with `arm`, waiting for the gate to open.
    Armed,
    /// Hunters launched but still all queued for a parallel slot.
    Waiting,
    Running,
    /// No hunter is live but some are still in flight (publishing, PR open) or queued.
    Draining,
    Done,
    Stopped,
}

impl RedTeamState {
    /// Whether the run is over; the tick and a stop never touch it again.
    pub fn over(self) -> bool {
        matches!(self, Self::Done | Self::Stopped)
    }
}

/// A run's findings tally, read from the hunters' findings ledgers: each counts distinct findings,
/// not ledger lines, so a finding that went validated → filed → fix colony is one found, one validated
/// and one filed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counts {
    pub found: u32,
    pub validated: u32,
    pub rejected: u32,
    pub filed: u32,
    /// Distinct defects in the linked synthesis report; `None` until a synthesis finishes.
    pub merged: Option<u32>,
}

/// Where a run's synthesis is, independent of the run's own state (which stays `done`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynthesisState {
    /// Not yet launched, or launched and still queued for a parallel slot.
    #[default]
    Pending,
    Running,
    Done,
    /// The launch failed, the colony died, or it ended without a report; `reason` says which.
    Failed,
}

/// A done run's synthesis: one judge colony merging the hunters' findings into a single ranked
/// report (`redteam-report.jsonl` in its own session directory).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Synthesis {
    pub state: SynthesisState,
    /// The current (newest) synthesis colony.
    pub session_id: Option<String>,
    /// Host path of the newest successful report; a retry keeps the previous one until it is beaten.
    pub report: Option<String>,
    /// Why the synthesis failed; `None` otherwise.
    pub reason: Option<String>,
    /// Earlier synthesis colony ids, oldest first; their reports stay on disk.
    pub superseded: Vec<String>,
}

impl Synthesis {
    /// A launch in progress: a retry must not start a second colony. `pending` without a colony is
    /// a launch stranded by a crash — nothing will finish it, so it counts as retryable, not in
    /// flight.
    fn in_flight(&self) -> bool {
        self.session_id.is_some() && matches!(self.state, SynthesisState::Pending | SynthesisState::Running)
    }
}

/// One hunter colony of a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hunter {
    pub session_id: String,
    pub title: String,
    pub module: String,
    /// The model tier the hunter booted on; `null` until #216 wires version reporting.
    pub version: Option<String>,
    pub focus: String,
}

/// A red-team run, persisted in `data/redteam.json`; the container-level `#[serde(default)]` keeps a
/// file written by an earlier version loadable, the same convention `Session` uses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedTeamRun {
    pub id: String,
    pub repo: String,
    /// The repository owner (org) the run raids.
    pub org: String,
    pub state: RedTeamState,
    pub swarm_size: usize,
    pub modules: Vec<String>,
    /// When false, hunters are briefed never to open, merge or autofix anything, with autopilot off.
    pub autofix: bool,
    pub hunters: Vec<Hunter>,
    pub counts: Counts,
    /// The synthesis phase, `None` until a run with findings lands `done` (and forever `None` for
    /// stopped, found-nothing or already-done runs).
    pub synthesis: Option<Synthesis>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Human-readable, set while `armed`/`waiting`: why the run has not launched yet.
    pub gate_reason: Option<String>,
    /// Who hunts: `swarm` (colony hunters). Empty in files written before hunters were named.
    pub hunter: String,
    /// The hunters' orchestrator model, when the operator named one; `None` uses routing.
    pub model: Option<String>,
    /// The hunters' subagent model, when the operator named one.
    pub subagent_model: Option<String>,
    /// The schedule that started this run, if one did.
    pub schedule_id: Option<String>,
}

impl Default for RedTeamRun {
    fn default() -> Self {
        Self {
            id: String::new(),
            repo: String::new(),
            org: String::new(),
            state: RedTeamState::Armed,
            swarm_size: 0,
            modules: Vec::new(),
            autofix: false,
            hunters: Vec::new(),
            counts: Counts::default(),
            synthesis: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            started_at: None,
            ended_at: None,
            gate_reason: None,
            hunter: DEFAULT_HUNTER.to_string(),
            model: None,
            subagent_model: None,
            schedule_id: None,
        }
    }
}

/// The runs under an app, saved to `data/redteam.json` like the session list is to `sessions.json`:
/// atomic save, load at construction. Kept small on purpose — the file is young, so an unreadable
/// one starts (on stderr) with no runs rather than being salvaged record by record.
pub struct RedTeamStore {
    runs: RwLock<Vec<RedTeamRun>>,
    file: PathBuf,
    /// Serialises saves so two writers cannot interleave temp files.
    persist: Mutex<()>,
    /// Recurring runs, saved to `<config_dir>/redteam-schedules.json`: operator configuration, so it
    /// lives with the other settings rather than with the run history.
    schedules: RwLock<Vec<RedTeamSchedule>>,
    schedules_file: PathBuf,
    persist_schedules: Mutex<()>,
}

impl RedTeamStore {
    pub fn new(data_dir: &FsPath, config_dir: &FsPath) -> Self {
        let file = data_dir.join("redteam.json");
        let schedules_file = config_dir.join("redteam-schedules.json");
        Self {
            runs: RwLock::new(load_runs(&file)),
            file,
            persist: Mutex::new(()),
            schedules: RwLock::new(load_schedules(&schedules_file)),
            schedules_file,
            persist_schedules: Mutex::new(()),
        }
    }

    async fn save_schedules(&self) -> Result<()> {
        let _guard = self.persist_schedules.lock().await;
        let data = serde_json::to_vec_pretty(&*self.schedules.read().await)?;
        write_atomic(&self.schedules_file, &data).await
    }

    pub(crate) async fn save(&self) -> Result<()> {
        let _guard = self.persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.runs.read().await)?;
        write_atomic(&self.file, &data).await
    }

    /// Applies `f` to the run named `id` under the write lock, then saves the list.
    pub(crate) async fn update<R>(&self, id: &str, f: impl FnOnce(&mut RedTeamRun) -> R) -> Option<(RedTeamRun, R)> {
        let (run, result) = {
            let mut runs = self.runs.write().await;
            let run = runs.iter_mut().find(|r| r.id == id)?;
            let result = f(run);
            (run.clone(), result)
        };
        if let Err(e) = self.save().await {
            eprintln!("redteam: could not save the red-team runs list: {e:#}");
        }
        Some((run, result))
    }
}

/// `data/redteam.json` → the runs it holds. A missing file is a first run.
fn load_runs(path: &FsPath) -> Vec<RedTeamRun> {
    let data = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!(
                "redteam: could not read {}: {e}; starting with no red-team runs",
                path.display()
            );
            return Vec::new();
        }
        Ok(data) => data,
    };
    let mut runs = match serde_json::from_slice::<Vec<RedTeamRun>>(&data) {
        Ok(runs) => runs,
        Err(e) => {
            eprintln!(
                "redteam: could not parse {}: {e}; starting with no red-team runs",
                path.display()
            );
            Vec::new()
        }
    };
    // `create` never stores an empty module list — it coerces one to the default — but this file
    // is trusted on load, and a run carrying an empty one would panic the tick on `i % 0` the
    // moment it launched the swarm. Read it the way `create` would have written it.
    for run in &mut runs {
        if run.modules.is_empty() {
            run.modules = vec![DEFAULT_MODULE.to_string()];
        }
    }
    runs
}

/// The gate: how many colonies of every org are live right now.
async fn live_count(app: &Shared) -> usize {
    app.sessions.read().await.iter().filter(|s| s.status.is_live()).count()
}

/// What an armed run's `gate_reason` says while colonies are live.
fn gate_reason(live: usize) -> String {
    let colony = if live == 1 { "colony is" } else { "colonies are" };
    format!("{live} {colony} live — waiting for the nest to empty")
}

/// What a start-now create answers with when the gate is closed: the live count, named, and why.
fn refused_message(live: usize) -> String {
    let colony = if live == 1 { "colony is" } else { "colonies are" };
    format!("{live} {colony} live — a red-team run can only start when the nest is empty")
}

/// What a `waiting` run's `gate_reason` says: every hunter is queued for the parallel limit.
fn wait_reason(n: usize) -> String {
    format!("all {n} hunters are queued for a free parallel slot — waiting for room to start")
}

// ---------------------------------------------------------------------------
// Hunter briefs and launch
// ---------------------------------------------------------------------------

/// The runner brief one hunter gets, as the request body of `POST /api/sessions`. Hunters are
/// numbered from 1 in both the title and the brief, so the role line matches the UI.
fn hunter_brief(run: &RedTeamRun, i: usize, n: usize) -> Value {
    let focus = FOCUSES[i % FOCUSES.len()];
    let others: Vec<&str> = FOCUSES
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i % FOCUSES.len())
        .map(|(_, f)| *f)
        .collect();
    let module = &run.modules[i % run.modules.len()];
    let fix = if run.autofix {
        "You may fix the bugs you find — autofix is on for this run, so your work is expected to \
         include the fix itself."
    } else {
        "NEVER open, merge or autofix anything unless explicitly told to. You are here to find and \
         report bugs, not to change the code."
    };
    let instructions = format!(
        "You are red-team hunter {} of {n} raiding {}, using the {module} module.\n\
         \n\
         Your assignment is {focus}.\n\
         \n\
         The other hunters in this swarm cover: {}. Stay strictly inside your own assignment and do not\n\
         duplicate theirs. A bug that belongs to another focus is theirs, not yours — note it if you find\n\
         it, and move on.\n\
         \n\
         Hunt aggressively for concrete, demonstrable bugs in your assignment. Reproduce each one before\n\
         you report it.\n\
         \n\
         Report what you find with the findings tool.\n\
         \n\
         {fix}",
        i + 1,
        run.repo,
        others.join("; "),
    );
    json!({
        "repo": run.repo,
        "issue": null,
        "title": format!("Red-team hunter {}/{}: {focus}", i + 1, n),
        "instructions": instructions,
        "autopilot": run.autofix,
        "allow_duplicate": true,
        "model_tier": null,
        "model_override": run.model,
        "subagent_model_override": run.subagent_model,
        "after": null,
        "origin": REDTEAM_ORIGIN,
    })
}

/// The seam over one hunter's creation.
///
/// Production rides `sessions::create`: the same validation any colony gets (repo shape, org switch,
/// agent module, Claude login, agentd binary) and the queue's admission, so a hunter may come back
/// `queued` when the parallel limit is full — which is exactly what the run's `waiting` state covers.
///
/// Tests swap this for a stub that pushes a session straight in, because a `test_app` has no agent
/// module, Claude credential or agentd binary to satisfy; the stub still respects the queue's
/// `has_room` admission so `waiting` is reachable.
#[cfg(not(test))]
async fn launch_hunter(app: Shared, brief: Value) -> Result<Session, String> {
    let req: NewSession = serde_json::from_value(brief).map_err(|e| format!("could not build the hunter brief: {e}"))?;
    match sessions::create(State(app), Json(req)).await {
        Ok(Json(session)) => Ok(session),
        Err(e) => Err(e.message()),
    }
}

#[cfg(test)]
async fn launch_hunter(app: Shared, brief: Value) -> Result<Session, String> {
    use crate::sessions::tests::colony;
    let id = short_id();
    let repo = brief["repo"].as_str().unwrap_or_default().to_string();
    let (owner, name) = match repo.split_once('/') {
        Some((owner, name)) => (owner.to_string(), name.to_string()),
        None => return Err("no repository".to_string()),
    };
    let mut session = colony(&owner, SessionStatus::Starting);
    session.id = id.clone();
    session.repo = repo;
    session.branch = format!("colonizer/session-{id}");
    session.sandbox = format!("colonizer-{id}");
    session.origin = Some(REDTEAM_ORIGIN.into());
    session.worktree = app
        .cfg
        .data_dir
        .join("worktrees")
        .join(&owner)
        .join(&name)
        .join(&id)
        .display()
        .to_string();
    session.issue_title = brief["title"].as_str().unwrap_or_default().to_string();
    session.instructions = brief["instructions"].as_str().unwrap_or_default().to_string();
    session.autopilot = brief["autopilot"].as_bool().unwrap_or(false);
    let modules = app.modules.read().await.clone();
    let max_parallel = crate::orgs::global_max_parallel(&modules) as usize;
    let org_settings = app.org_settings(&owner);
    let org_limit = crate::orgs::org_max_parallel(&org_settings);
    let repo_limit = crate::queue::repo_limit(&modules, &org_settings);
    {
        let mut sessions = app.sessions.write().await;
        if !crate::queue::has_room(&sessions, &owner, &session.repo, max_parallel, org_limit, repo_limit) {
            session.status = SessionStatus::Queued;
        }
        sessions.push(session.clone());
    }
    tokio::fs::create_dir_all(app.session_dir(&id)).await.unwrap();
    app.persist_sessions().await.map_err(|e| format!("{e:#}"))?;
    Ok(session)
}

/// Create the run's hunters and attach them. The launch is recorded first — the run leaves `armed`,
/// `started_at` fills and the record is persisted — before the first hunter exists, so a crash
/// mid-swarm cannot leave `redteam.json` saying `armed` and let the next tick launch a duplicate
/// swarm on restart; a run caught half-launched drains to `done` instead (missing hunters count as
/// ended). A second update lands the created hunters, classifying the run by their actual statuses:
/// every hunter still queued means `waiting`, any hunter starting or live means `running` (§6.7). A
/// hunter that fails to create is recorded on stderr, and the hunters that did launch stay part of
/// the run. A stop that lands mid-launch wins: the create loop checks the recorded state before each
/// hunter and aborts, and the terminal-state guard refuses the attach write.
pub(crate) async fn launch_run(app: &Shared, run: &mut RedTeamRun) {
    let id = run.id.clone();
    let n = run.swarm_size;
    let flipped = app
        .redteam
        .update(&id, |r| {
            if r.state.over() {
                return false; // a stop beat this launch; its state wins
            }
            r.state = RedTeamState::Running;
            r.started_at = Some(Utc::now());
            r.gate_reason = None;
            true
        })
        .await
        .is_some_and(|(_, flipped)| flipped);
    if flipped {
        // Mirror the persisted flip into the run handed to the caller, so a caller that writes it
        // back later cannot resurrect an `armed` run with a stale copy.
        run.state = RedTeamState::Running;
        run.started_at = Some(Utc::now());
        run.gate_reason = None;
    }
    let mut hunters = Vec::with_capacity(n);
    let mut any_live = false;
    for i in 0..n {
        // A stop may have landed while this swarm was being created: its terminal state wins, and
        // no more hunters launch.
        if app
            .redteam
            .runs
            .read()
            .await
            .iter()
            .find(|r| r.id == id)
            .is_some_and(|r| r.state.over())
        {
            break;
        }
        let brief = hunter_brief(run, i, n);
        match launch_hunter(app.clone(), brief).await {
            Ok(session) => {
                if session.status.is_live() {
                    any_live = true;
                }
                hunters.push(Hunter {
                    session_id: session.id,
                    title: session.issue_title,
                    module: run.modules[i % run.modules.len()].clone(),
                    version: None,
                    focus: FOCUSES[i % FOCUSES.len()].to_string(),
                });
            }
            Err(message) => eprintln!(
                "redteam: hunter {}/{} of run {} could not be created: {message}",
                i + 1,
                n,
                run.id
            ),
        }
    }
    // The attach: the created hunters land in the record, and the state is settled from their
    // statuses — an all-queued launch is `waiting`, one with a hunter starting or live is `running`.
    run.hunters = hunters;
    run.gate_reason = None;
    if flipped {
        run.state = if any_live {
            RedTeamState::Running
        } else {
            RedTeamState::Waiting
        };
    }
    app.redteam
        .update(&id, |r| {
            if r.state.over() {
                return; // a stop beat this launch; its state wins
            }
            *r = run.clone();
        })
        .await;
}

/// A hunter whose work is over — or whose session is gone (missing counts as ended).
fn is_ended(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Merged | SessionStatus::Closed | SessionStatus::NoChanges | SessionStatus::Stopped | SessionStatus::Failed
    )
}

fn finish(run: &mut RedTeamRun) {
    run.state = RedTeamState::Done;
    run.ended_at = Some(Utc::now());
    run.gate_reason = None;
}

/// One step of the run's state machine, purely a function of the hunter statuses it can see, so
/// tests drive it without I/O. Launching (the `armed` branch) creates sessions, so the tick handles
/// it and only hands the wait state down here.
fn advance_state(run: &mut RedTeamRun, sessions: &[Session]) {
    let status_of = |sid: &str| {
        sessions
            .iter()
            .find(|s| s.id == sid)
            .map(|s| s.status)
            .unwrap_or(SessionStatus::Stopped)
    };
    let total = run.hunters.len();
    let live_hunters = run.hunters.iter().filter(|h| status_of(&h.session_id).is_live()).count();
    let queued_all = total > 0 && run.hunters.iter().all(|h| status_of(&h.session_id) == SessionStatus::Queued);
    let all_ended = run.hunters.iter().all(|h| is_ended(status_of(&h.session_id)));
    match run.state {
        RedTeamState::Armed => {}
        RedTeamState::Waiting => {
            if live_hunters > 0 {
                run.state = RedTeamState::Running;
                run.gate_reason = None;
            } else if all_ended {
                finish(run);
            } else if queued_all {
                run.gate_reason = Some(wait_reason(total));
            } else {
                // Mixed queued and in-flight: let `running` classify it.
                run.state = RedTeamState::Running;
                run.gate_reason = None;
            }
        }
        RedTeamState::Running => {
            run.gate_reason = None;
            if live_hunters > 0 {
                // still underway
            } else if queued_all {
                run.state = RedTeamState::Waiting;
                run.gate_reason = Some(wait_reason(total));
            } else if all_ended {
                finish(run);
            } else {
                // No hunter live and at least one not done: publishing, PR open, queued — draining.
                run.state = RedTeamState::Draining;
            }
        }
        RedTeamState::Draining => {
            if all_ended {
                finish(run);
            }
        }
        RedTeamState::Done | RedTeamState::Stopped => {}
    }
}

/// The run's findings counts from its hunters' ledgers (`sessions/<id>/findings.jsonl`, §6.6). The
/// ledger has one line per stage a finding reached — validated, rejected, filed, duplicate, then the
/// fix colony, its review and merge — all carrying the finding's title, so lines are grouped by title
/// within each hunter's ledger and each finding is counted once per state it reached. Filed counts a
/// finding that matched an open issue too: either way it is on GitHub. Legacy lines with no `state`
/// are read by what they carry (`findings::records` does that).
fn counts_for(app: &App, run: &RedTeamRun) -> Counts {
    let mut counts = Counts::default();
    for hunter in &run.hunters {
        let record = app.session_dir(&hunter.session_id).join("findings.jsonl");
        // Title → the states that finding reached, in first-seen order of titles.
        let mut reached: Vec<(String, Vec<String>)> = Vec::new();
        for line in findings::records(&record) {
            let at = match reached.iter().position(|(title, _)| *title == line.title) {
                Some(at) => at,
                None => {
                    reached.push((line.title.clone(), Vec::new()));
                    reached.len() - 1
                }
            };
            if let Some(state) = line.state {
                reached[at].1.push(state);
            }
        }
        for (_, states) in &reached {
            let has = |wanted: &[&str]| states.iter().any(|s| wanted.contains(&s.as_str()));
            counts.found += 1;
            counts.validated += u32::from(has(&["validated"]));
            counts.rejected += u32::from(has(&["rejected"]));
            counts.filed += u32::from(has(&["filed", "duplicate"]));
        }
    }
    // `merged` counts the synthesis report's lines, not ledger lines, so it is carried over from
    // the run: a retry keeps the previous count until the new report lands.
    counts.merged = run.counts.merged;
    counts
}

// ---------------------------------------------------------------------------
// Synthesis: one judge colony merges the hunters' findings into one report
// ---------------------------------------------------------------------------

/// The file a synthesis colony writes in its own `out`.
const REPORT_NAME: &str = "redteam-report.jsonl";
/// More than this in a merged report is a judge that has lost the plot; the read refuses it.
const REPORT_CAP: u64 = 1_000_000;
/// The synthesis brief stays under this, below the 20,000 characters a session's instructions are
/// truncated to (sessions.rs `create`).
const SYNTHESIS_BRIEF_CAP: usize = 19_000;

/// One line of a merged report: one distinct defect, ranked most severe first. Written by the
/// synthesis colony, so every field is read tolerantly and a line without a title costs itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergedDefect {
    pub defect: String,
    pub severity: String,
    pub reproduction: String,
    pub steps: String,
    pub files: Vec<String>,
    /// The hunter session ids that reported this defect.
    pub hunters: Vec<String>,
    pub merged_from: u32,
    pub validation: String,
}

/// Parse a merged report: lines that do not parse to a defect with a title are skipped, the rest
/// stay in file order (the judge writes them most severe first).
fn parse_report(content: &str) -> Vec<MergedDefect> {
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<MergedDefect>(line).ok())
        .filter(|d| !d.defect.is_empty())
        .collect()
}

/// One hunter finding as the synthesis brief carries it: the ledger's latest verdict, plus the
/// body and evidence of the raw `finding` event, which the ledger does not hold.
struct HunterFinding {
    title: String,
    verdict: Verdict,
    body: String,
    evidence: String,
}

/// A finding's latest ledger verdict: its state, severity and reason.
type Verdict = (Option<String>, Option<String>, Option<String>);

/// A hunter's findings, grouped by title in first-seen order (the way `counts_for` counts them)
/// with the latest ledger verdict per title and prose from the hunter's last raw `finding` event.
/// The events log goes through the bounded regular-file reader: a live VM appending to it cannot
/// grow the read past the cap, and a planted symlink cannot turn it into a host-file read.
fn hunter_findings(app: &App, hunter: &Hunter) -> Vec<HunterFinding> {
    let dir = app.session_dir(&hunter.session_id);
    let mut titles: Vec<String> = Vec::new();
    let mut verdicts: HashMap<String, Verdict> = HashMap::new();
    for line in findings::records(&dir.join("findings.jsonl")) {
        if !titles.contains(&line.title) {
            titles.push(line.title.clone());
        }
        let verdict = verdicts.entry(line.title).or_default();
        if line.state.is_some() {
            verdict.0 = line.state;
        }
        if line.severity.is_some() {
            verdict.1 = line.severity;
        }
        if line.reason.is_some() {
            verdict.2 = line.reason;
        }
    }
    let mut prose: HashMap<String, (String, String)> = HashMap::new();
    if let Ok(content) = crate::github::read_regular_file(&dir.join("events.jsonl"), 2_000_000) {
        for line in content.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if event["type"] == "finding" && event["title"].is_string() {
                let body = event["body"].as_str().unwrap_or_default().to_string();
                let evidence = event["evidence"].as_str().unwrap_or_default().to_string();
                prose.insert(event["title"].as_str().unwrap().to_string(), (body, evidence));
            }
        }
    }
    titles
        .into_iter()
        .map(|title| {
            let (body, evidence) = prose.remove(&title).unwrap_or_default();
            let verdict = verdicts.remove(&title).unwrap_or_default();
            HunterFinding {
                title,
                verdict,
                body,
                evidence,
            }
        })
        .collect()
}

/// The brief the synthesis judge gets, as the request body of `POST /api/sessions`. Colonies cannot
/// mount host files, so the hunters' ledgers travel inline — each finding's verdict, body and
/// evidence cut to an equal share of what the intro, the task and the per-hunter headers leave of
/// the budget — with each ledger's host path named for the record. The task is a merge only.
fn synthesis_brief(app: &App, run: &RedTeamRun) -> Value {
    let mut text = format!(
        "You are the synthesis judge for red-team run {} on {}. A colony cannot read files on the\n\
         mothership host, so every hunter's findings ledger is quoted inline below, its host path\n\
         named for the record.\n",
        run.id, run.repo,
    );
    let per_hunter: Vec<(&Hunter, Vec<HunterFinding>)> = run.hunters.iter().map(|h| (h, hunter_findings(app, h))).collect();
    let task = "\nYour task\n\
         \n\
         Merge the findings above into one deduplicated report of the real defects. The same defect\n\
         reported by several hunters is ONE line: list every hunter session id that reported it in\n\
         \"hunters\" and count the findings merged into the line in \"merged_from\". Keep the best\n\
         reproduction per defect in \"steps\" — you may re-check it against the checked-out repository —\n\
         and mark anything not reproduced \"reproduction\": \"unconfirmed\". Rank the lines most severe\n\
         first. Carry each line's \"validation\" from the ledgers above: \"validated\" when any finding\n\
         merged into it was validated, \"rejected\" when all of them were, otherwise \"unvalidated\" —\n\
         stated explicitly on every line, never implied.\n\
         \n\
         Write ONLY /harness/out/redteam-report.jsonl, one JSON object per line, one line per distinct\n\
         defect, exactly this shape:\n\
         {\"defect\": \"<one-line title>\", \"severity\": \"critical|high|medium|low\", \"reproduction\":\n\
         \"reproduced|unconfirmed\", \"steps\": \"<the best reproduction>\", \"files\": [\"<path>\"],\n\
         \"hunters\": [\"<hunter session id>\"], \"merged_from\": 2, \"validation\":\n\
         \"validated|rejected|unvalidated\"}\n\
         \n\
         File nothing, open no issue or pull request, do not write /harness/out/pr.md, do not change\n\
         the code, and do not use the findings tool.\n";
    let total: usize = per_hunter.iter().map(|(_, list)| list.len()).sum();
    let share = SYNTHESIS_BRIEF_CAP.saturating_sub(text.len() + task.len() + 400 * per_hunter.len()) / total.max(1);
    for (hunter, list) in &per_hunter {
        text.push_str(&format!(
            "\nHunter session {}, focus: {} — ledger: {}\n",
            hunter.session_id,
            hunter.focus,
            app.session_dir(&hunter.session_id).join("findings.jsonl").display(),
        ));
        for finding in list {
            let (state, severity, reason) = &finding.verdict;
            let verdict = match state {
                None => "no ledger state".to_string(),
                Some(state) => format!(
                    "state {state}{}{}",
                    severity.as_deref().map(|s| format!(", severity {s}")).unwrap_or_default(),
                    reason
                        .as_deref()
                        .map(|r| format!(", reason: {}", truncate(r, 200)))
                        .unwrap_or_default(),
                ),
            };
            let mut block = format!("- \"{}\" — {verdict}\n", truncate(&finding.title, 200));
            let rest = share.saturating_sub(block.len() + 40);
            let body_max = rest * 3 / 5;
            block.push_str(&format!("  body: {}\n", truncate(&finding.body, body_max)));
            block.push_str(&format!("  evidence: {}\n", truncate(&finding.evidence, rest - body_max)));
            text.push_str(&block);
        }
    }
    text.push_str(task);
    json!({
        "repo": run.repo,
        "issue": null,
        "title": format!("Red-team synthesis: {}", run.repo),
        "instructions": text,
        "autopilot": false,
        "autofix": false,
        "automerge": false,
        "allow_duplicate": true,
        "model_tier": null,
        "model_override": run.model,
        "subagent_model_override": run.subagent_model,
        "after": null,
    })
}

/// Serializes a synthesis launch from its record to its attach, so a retry or tick racing one queues
/// here instead of slipping into the record-to-attach gap and launching a second judge.
static SYNTHESIS_LAUNCH: Mutex<()> = Mutex::const_new(());

/// Launch the synthesis colony of a done run: record the pending phase first — with the run's whole
/// state, persist-first like `launch_run`, so a stop, retry or tick racing this one cannot launch
/// two — then launch and attach. A launch failure lands `failed`; a previous report and
/// `counts.merged` stay until a new synthesis beats them.
async fn launch_synthesis(app: &Shared, run: &mut RedTeamRun) {
    let id = run.id.clone();
    let _launch = SYNTHESIS_LAUNCH.lock().await;
    if run.synthesis.as_ref().is_some_and(Synthesis::in_flight) {
        return; // already pending or running: a retry is answered with the run unchanged
    }
    let old = run.synthesis.clone().unwrap_or_default();
    let mut superseded = old.superseded.clone();
    superseded.extend(old.session_id);
    run.synthesis = Some(Synthesis {
        state: SynthesisState::Pending,
        session_id: None,
        report: old.report,
        reason: None,
        superseded,
    });
    let recorded = app
        .redteam
        .update(&id, |r| {
            if r.state == RedTeamState::Stopped || r.synthesis.as_ref().is_some_and(Synthesis::in_flight) {
                return false; // a stop beat this launch, or a concurrent retry or tick won it
            }
            *r = run.clone(); // done state and pending phase land together, before any colony exists
            true
        })
        .await
        .is_some_and(|(_, recorded)| recorded);
    if recorded {
        // A stop may have landed since the record: its terminal state wins, and no judge launches
        // onto a stopped run.
        let stopped = !app
            .redteam
            .runs
            .read()
            .await
            .iter()
            .find(|r| r.id == id)
            .is_some_and(|r| r.state == RedTeamState::Done);
        if stopped {
            app.redteam
                .update(&id, |r| {
                    if let Some(s) = &mut r.synthesis {
                        s.state = SynthesisState::Failed;
                        s.reason = Some("the run was stopped".to_string());
                    }
                })
                .await;
        } else {
            let launched = launch_hunter(app.clone(), synthesis_brief(app, run)).await;
            attach_synthesis(app, &id, launched).await;
        }
    }
    // Fold the stored phase back into the caller's run, so a later write cannot clobber the launch.
    if let Some(r) = app.redteam.runs.read().await.iter().find(|r| r.id == id) {
        run.synthesis = r.synthesis.clone();
    }
}

/// Land a launched judge — or a launch failure — into the run's synthesis. A stop that landed while
/// the colony was being created wins: the judge is not attached, the synthesis fails, and the colony
/// is stopped through the real path, so no judge is left orbiting a stopped run.
async fn attach_synthesis(app: &Shared, id: &str, launched: Result<Session, String>) {
    let created = launched.clone().ok();
    app.redteam
        .update(id, move |r| match launched {
            Ok(session) => {
                let Some(s) = &mut r.synthesis else { return };
                if r.state == RedTeamState::Done {
                    s.session_id = Some(session.id);
                } else {
                    // A stop beat the attach: it wins, and the judge stays unattached.
                    s.state = SynthesisState::Failed;
                    s.reason = Some("the run was stopped".to_string());
                }
            }
            Err(message) => {
                if let Some(s) = &mut r.synthesis {
                    s.state = SynthesisState::Failed;
                    s.reason = Some(message);
                }
            }
        })
        .await;
    // The judge was created but a stop refused the attach: stop it like any other hunter.
    if let Some(session) = created {
        let attached = app.redteam.runs.read().await.iter().find(|r| r.id == id).is_some_and(|r| {
            r.synthesis
                .as_ref()
                .is_some_and(|s| s.session_id.as_deref() == Some(session.id.as_str()))
        });
        if !attached && let Err(e) = crate::lifecycle::stop(State(app.clone()), Path(session.id.clone())).await {
            eprintln!("redteam: could not stop the unattached judge {}: {}", session.id, e.message());
        }
    }
}

/// Progress a done run's synthesis against its colony's status: `pending` while queued, `running`
/// while live, `done` when the colony ended and left a readable report (linked, its parsed line
/// count in `counts.merged`), `failed` with a reason otherwise. The run's own state never moves
/// because of synthesis — it stays `done` — and nothing is stored unless something changed.
async fn synthesis_step(app: &Shared, id: &str) {
    let Some(run) = app.redteam.runs.read().await.iter().find(|r| r.id == id).cloned() else {
        return;
    };
    let Some(synthesis) = &run.synthesis else { return };
    if !synthesis.in_flight() {
        return;
    }
    // Mid-launch (pending, no colony yet): leave it for the next tick.
    let Some(sid) = &synthesis.session_id else { return };
    let session = app.sessions.read().await.iter().find(|s| s.id == *sid).cloned();
    let path = app.session_dir(sid).join("out").join(REPORT_NAME);
    let (state, reason, done) = match session.as_ref().map(|s| s.status) {
        None => (SynthesisState::Failed, Some("the synthesis colony is gone".to_string()), None),
        Some(SessionStatus::Queued) => (SynthesisState::Pending, None, None),
        Some(status) if status.is_live() && status != SessionStatus::Idle => (SynthesisState::Running, None, None),
        Some(SessionStatus::Failed) => (
            SynthesisState::Failed,
            Some(
                session
                    .and_then(|s| s.error)
                    .unwrap_or_else(|| "the synthesis colony failed".to_string()),
            ),
            None,
        ),
        Some(SessionStatus::Stopped) => (
            SynthesisState::Failed,
            Some("stopped before writing a report".to_string()),
            None,
        ),
        // Idle counts as live for the nest gate, but a judge gone idle has said all it is going to
        // say; publishing, PR open, merged, closed and no changes are over the same way: the report
        // is either there, or the synthesis failed without one.
        Some(_) => {
            let read = crate::github::read_regular_file(&path, REPORT_CAP);
            match read.ok().map(|c| parse_report(&c)) {
                Some(defects) => (
                    SynthesisState::Done,
                    None,
                    Some((path.display().to_string(), defects.len() as u32)),
                ),
                None => (
                    SynthesisState::Failed,
                    Some("ended without writing redteam-report.jsonl".to_string()),
                    None,
                ),
            }
        }
    };
    let merged = done.as_ref().map(|(_, n)| *n);
    let mut next = synthesis.clone();
    next.state = state;
    next.reason = reason;
    if let Some((path, _)) = &done {
        next.report = Some(path.clone());
    }
    if *synthesis == next && run.counts.merged == merged {
        return;
    }
    app.redteam
        .update(id, |r| {
            let Some(s) = &mut r.synthesis else { return };
            *s = next.clone();
            if let Some(n) = merged {
                r.counts.merged = Some(n);
            }
        })
        .await;
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// One pass over every run. The 5s loop calls this; tests drive it directly. A done run's synthesis
/// steps after its run: `one_step` returns early once a run is over, so the synthesis colony is
/// followed from here.
pub(crate) async fn tick_once(app: &Shared) {
    let ids: Vec<String> = app.redteam.runs.read().await.iter().map(|r| r.id.clone()).collect();
    for id in ids {
        one_step(app, &id).await;
        synthesis_step(app, &id).await;
    }
}

/// The background loop, spawned next to `queue::run_queue`: every 5s, each run steps its state
/// machine against the gate and its hunters' statuses.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        tick_once(&app).await;
    }
}

/// One run through its state machine, launched if armed and the gate is open.
async fn one_step(app: &Shared, id: &str) {
    let sessions = app.sessions.read().await.clone();
    let live = sessions.iter().filter(|s| s.status.is_live()).count();
    let stored = match app.redteam.runs.read().await.iter().find(|r| r.id == id).cloned() {
        Some(run) if !run.state.over() => run,
        _ => return,
    };
    let mut run = stored.clone();
    if run.state == RedTeamState::Armed {
        if live == 0 {
            launch_run(app, &mut run).await;
        } else {
            run.gate_reason = Some(gate_reason(live));
        }
    } else {
        advance_state(&mut run, &sessions);
    }
    run.counts = counts_for(app, &run);
    // Synthesis fires exactly once, at this transition into done and only with something to merge:
    // `stored` is the pre-tick record, so a run already done (or stopped) never synthesizes.
    if stored.state != RedTeamState::Done && run.state == RedTeamState::Done && run.counts.found > 0 {
        launch_synthesis(app, &mut run).await;
    }
    if run != stored {
        let _ = app
            .redteam
            .update(id, |r| {
                if r.state.over() {
                    return; // a stop beat this tick; its state wins
                }
                *r = run.clone();
            })
            .await;
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

pub async fn list(State(app): State<Shared>) -> Json<Vec<RedTeamRun>> {
    Json(app.redteam.runs.read().await.iter().rev().cloned().collect())
}

pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<RedTeamRun> {
    let run = app
        .redteam
        .runs
        .read()
        .await
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such red-team run"))?;
    Ok(Json(run))
}

#[derive(Clone, Deserialize)]
pub struct NewRedTeamRun {
    repo: String,
    /// Who hunts; unset is the swarm.
    #[serde(default)]
    hunter: Option<String>,
    /// The hunters' orchestrator model: a Claude alias or ID, or `<provider>/<model>`.
    #[serde(default)]
    model: Option<String>,
    /// The hunters' subagent model.
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    swarm_size: Option<usize>,
    #[serde(default)]
    modules: Option<Vec<String>>,
    #[serde(default)]
    autofix: Option<bool>,
    /// `false`/unset: start now, refused with a 409 while any colony is live. `true`: create the
    /// run `armed`; the tick launches it the next time the nest is empty.
    #[serde(default)]
    arm: Option<bool>,
}

pub async fn create(State(app): State<Shared>, Json(req): Json<NewRedTeamRun>) -> ApiResult<RedTeamRun> {
    Ok(Json(start(&app, req, None).await?))
}

/// Which hunter a request names, checked: the swarm runs; a known external hunter is refused with
/// why; anything else is unknown.
fn check_hunter(raw: Option<&str>) -> Result<String, String> {
    let hunter = raw.map(str::trim).filter(|h| !h.is_empty()).unwrap_or(DEFAULT_HUNTER);
    if hunter == DEFAULT_HUNTER {
        return Ok(hunter.to_string());
    }
    match crate::hunters::builtin().into_iter().find(|m| m.id == hunter) {
        Some(m) => Err(format!(
            "{} cannot run as a red-team hunter in this build yet: its scans are not driven by runs (install and probe it at /api/hunters/{}); use the swarm",
            m.name, m.id
        )),
        None => Err(format!("unknown hunter {hunter:?}; use \"swarm\"")),
    }
}

/// The one way a run starts — the handler and the scheduler both come through here, so a scheduled
/// run gets exactly the validation and gate a manual one does.
pub(crate) async fn start(app: &Shared, req: NewRedTeamRun, schedule_id: Option<String>) -> Result<RedTeamRun, crate::AppError> {
    let hunter = check_hunter(req.hunter.as_deref()).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let model = crate::sessions::launch_model(app, req.model.as_deref(), "model")?;
    let subagent_model = crate::sessions::launch_model(app, req.subagent_model.as_deref(), "subagent model")?;
    let repo = req.repo.trim().to_string();
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let swarm_size = match req.swarm_size {
        None => DEFAULT_SWARM,
        Some(n) if (1..=MAX_SWARM).contains(&n) => n,
        Some(n) => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                &format!("swarm_size must be 1..={MAX_SWARM}, got {n}"),
            ));
        }
    };
    let modules = req
        .modules
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| vec![DEFAULT_MODULE.to_string()]);
    if let Some(unknown) = modules.iter().find(|m| !KNOWN_MODULES.contains(&m.as_str())) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "unknown red-team module {unknown:?}; known modules: {}",
                KNOWN_MODULES.join(", ")
            ),
        ));
    }
    let autofix = req.autofix.unwrap_or(false);
    let armed = req.arm.unwrap_or(false);
    let live = live_count(app).await;
    // The nest gate is checked before anything is created, so a start-now against live colonies
    // spends nothing.
    if !armed && live > 0 {
        return Err(client_error(StatusCode::CONFLICT, &refused_message(live)));
    }
    let org = repo.split('/').next().unwrap_or_default().to_string();
    let now = Utc::now();
    let id = format!("rt_{}", short_id());
    let mut run = {
        let mut runs = app.redteam.runs.write().await;
        // One run per repository while one is still active, armed or not.
        if runs.iter().any(|r| r.repo == repo && !r.state.over()) {
            return Err(client_error(
                StatusCode::CONFLICT,
                &format!("a red-team run is already active for {repo}"),
            ));
        }
        let run = RedTeamRun {
            id: id.clone(),
            repo: repo.clone(),
            org,
            state: if armed { RedTeamState::Armed } else { RedTeamState::Running },
            swarm_size,
            modules,
            autofix,
            hunters: Vec::new(),
            counts: Counts::default(),
            synthesis: None,
            created_at: now,
            started_at: if armed { None } else { Some(now) },
            ended_at: None,
            gate_reason: if armed && live > 0 { Some(gate_reason(live)) } else { None },
            hunter,
            model,
            subagent_model,
            schedule_id,
        };
        runs.push(run.clone());
        run
    };
    if armed {
        if let Err(e) = app.redteam.save().await {
            app.storage_failed("save the red-team runs list", &e).await;
        }
    } else {
        // Start-now on an empty nest: launch inside the handler; `launch_run` records the launch
        // before any hunter exists, and this last write persists the run with the hunters attached.
        launch_run(app, &mut run).await;
        let _ = app
            .redteam
            .update(&id, |r| {
                if r.state.over() {
                    return; // a stop beat the launch; its state wins
                }
                *r = run.clone();
            })
            .await;
    }
    Ok(run)
}

/// Stop the run and every hunter it started, through the real session stop path: live hunters are
/// stopped and their microVMs removed, queued ones leave the queue; already-finished hunters are not
/// touched. Idempotent once the run is terminal.
pub async fn stop(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<RedTeamRun> {
    let hunter_ids: Vec<String> = {
        let runs = app.redteam.runs.read().await;
        let Some(run) = runs.iter().find(|r| r.id == id) else {
            return Err(client_error(StatusCode::NOT_FOUND, "no such red-team run"));
        };
        if run.state.over() {
            return Ok(Json(run.clone()));
        }
        run.hunters.iter().map(|h| h.session_id.clone()).collect()
    };
    let sessions = app.sessions.read().await.clone();
    for hunter in &hunter_ids {
        let alive_or_queued = sessions
            .iter()
            .find(|s| s.id == *hunter)
            .is_some_and(|s| s.status.is_live() || s.status == SessionStatus::Queued);
        if !alive_or_queued {
            continue;
        }
        if let Err(e) = crate::lifecycle::stop(State(app.clone()), Path(hunter.clone())).await {
            eprintln!("redteam: could not stop hunter {hunter}: {}", e.message());
        }
    }
    app.redteam
        .update(&id, |r| {
            r.state = RedTeamState::Stopped;
            r.ended_at = Some(Utc::now());
            r.gate_reason = None;
        })
        .await;
    let run = app
        .redteam
        .runs
        .read()
        .await
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .expect("the run exists: its stop just updated it");
    Ok(Json(run))
}

/// The stored run with this id, or a 404 for an unknown one.
async fn stored(app: &Shared, id: &str) -> Result<RedTeamRun, crate::AppError> {
    let run = app.redteam.runs.read().await.iter().find(|r| r.id == id).cloned();
    run.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such red-team run"))
}

/// Re-run a done run's synthesis: launches a fresh judge colony, superseding the previous one
/// (whose report stays on disk and linked until the new one finishes). Idempotent while a synthesis
/// is pending or running — the run comes back unchanged, no second colony. **409** unless the run
/// is `done` with findings to merge; **404** for an unknown run.
pub async fn synthesize(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<RedTeamRun> {
    let mut run = stored(&app, &id).await?;
    if run.state != RedTeamState::Done {
        return Err(client_error(
            StatusCode::CONFLICT,
            "synthesis runs only on a done red-team run",
        ));
    }
    if run.counts.found == 0 && run.synthesis.is_none() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "nothing to merge: the hunters found no findings",
        ));
    }
    launch_synthesis(&app, &mut run).await;
    Ok(Json(stored(&app, &id).await.unwrap_or(run)))
}

/// The linked merged report, parsed. **404** when no report is linked or the file is gone.
pub async fn report(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Vec<MergedDefect>> {
    let run = stored(&app, &id).await?;
    let path = run
        .synthesis
        .as_ref()
        .and_then(|s| s.report.as_deref())
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no synthesis report is linked to this run"))?;
    let content = crate::github::read_regular_file(FsPath::new(path), REPORT_CAP)
        .map_err(|_| client_error(StatusCode::NOT_FOUND, "the linked synthesis report is gone"))?;
    Ok(Json(parse_report(&content)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Schedules: a run every week or every month
// ---------------------------------------------------------------------------

pub use crate::schedule::{Cadence, next_run_after};

/// A recurring red-team run over some of an org's repositories.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedTeamSchedule {
    pub id: String,
    pub org: String,
    pub repos: Vec<String>,
    pub hunter: String,
    pub swarm_size: usize,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subagent_model: Option<String>,
    #[serde(default)]
    pub autofix: bool,
    pub cadence: Cadence,
    pub enabled: bool,
    pub next_run_at: DateTime<Utc>,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    /// What the last firing did, per repository: started, or why not.
    #[serde(default)]
    pub last_result: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn load_schedules(path: &FsPath) -> Vec<RedTeamSchedule> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!(
                "redteam: could not read {}: {e}; no red-team schedules loaded",
                path.display()
            );
            Vec::new()
        }
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
            eprintln!(
                "redteam: could not parse {}: {e}; no red-team schedules loaded",
                path.display()
            );
            Vec::new()
        }),
    }
}

/// The schedules due at `now`: enabled, and their next run is not in the future.
pub fn due(schedules: &[RedTeamSchedule], now: DateTime<Utc>) -> Vec<String> {
    schedules
        .iter()
        .filter(|s| s.enabled && s.next_run_at <= now)
        .map(|s| s.id.clone())
        .collect()
}

#[derive(Clone, Deserialize)]
pub struct NewSchedule {
    org: String,
    repos: Vec<String>,
    #[serde(default)]
    hunter: Option<String>,
    #[serde(default)]
    swarm_size: Option<usize>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    autofix: Option<bool>,
    cadence: Cadence,
    #[serde(default)]
    enabled: Option<bool>,
}

/// A schedule request, validated the way a run is, so a schedule cannot hold something that would
/// only fail when it fires.
fn schedule_from(
    app: &App,
    req: NewSchedule,
    id: String,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<RedTeamSchedule, crate::AppError> {
    let bad = |m: &str| client_error(StatusCode::BAD_REQUEST, m);
    let org = req.org.trim().to_string();
    if org.is_empty() {
        return Err(bad("org is required"));
    }
    let repos: Vec<String> = req
        .repos
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    if repos.is_empty() {
        return Err(bad("pick at least one repository"));
    }
    for repo in &repos {
        if !valid_repo(repo) {
            return Err(bad(&format!("invalid repository name {repo:?}")));
        }
        if !repo.split('/').next().is_some_and(|owner| owner.eq_ignore_ascii_case(&org)) {
            return Err(bad(&format!("{repo} is not in {org}")));
        }
    }
    let hunter = check_hunter(req.hunter.as_deref()).map_err(|e| bad(&e))?;
    let swarm_size = req.swarm_size.unwrap_or(DEFAULT_SWARM);
    if !(1..=MAX_SWARM).contains(&swarm_size) {
        return Err(bad(&format!("swarm_size must be 1..={MAX_SWARM}, got {swarm_size}")));
    }
    req.cadence.check().map_err(|e| bad(&e))?;
    if !matches!(req.cadence, Cadence::Weekly { .. } | Cadence::Monthly { .. }) {
        return Err(bad("a red-team schedule runs weekly or monthly"));
    }
    let model = crate::sessions::launch_model(app, req.model.as_deref(), "model")?;
    let subagent_model = crate::sessions::launch_model(app, req.subagent_model.as_deref(), "subagent model")?;
    Ok(RedTeamSchedule {
        id,
        org,
        repos,
        hunter,
        swarm_size,
        model,
        subagent_model,
        autofix: req.autofix.unwrap_or(false),
        next_run_at: next_run_after(&req.cadence, now),
        cadence: req.cadence,
        enabled: req.enabled.unwrap_or(true),
        last_run_at: None,
        last_result: None,
        created_at,
    })
}

pub async fn list_schedules(State(app): State<Shared>) -> Json<Vec<RedTeamSchedule>> {
    Json(app.redteam.schedules.read().await.clone())
}

pub async fn create_schedule(State(app): State<Shared>, Json(req): Json<NewSchedule>) -> ApiResult<RedTeamSchedule> {
    let now = Utc::now();
    let schedule = schedule_from(&app, req, format!("rts_{}", short_id()), now, now)?;
    app.redteam.schedules.write().await.push(schedule.clone());
    app.redteam.save_schedules().await?;
    Ok(Json(schedule))
}

/// Replaces a schedule's settings; its id, creation time and last firing are kept, and the next run
/// is recomputed from the (possibly new) cadence.
pub async fn update_schedule(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<NewSchedule>,
) -> ApiResult<RedTeamSchedule> {
    let now = Utc::now();
    let existing = app
        .redteam
        .schedules
        .read()
        .await
        .iter()
        .find(|s| s.id == id)
        .cloned()
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such red-team schedule"))?;
    let mut schedule = schedule_from(&app, req, id.clone(), existing.created_at, now)?;
    schedule.last_run_at = existing.last_run_at;
    schedule.last_result = existing.last_result;
    {
        let mut schedules = app.redteam.schedules.write().await;
        let Some(slot) = schedules.iter_mut().find(|s| s.id == id) else {
            return Err(client_error(StatusCode::NOT_FOUND, "no such red-team schedule"));
        };
        *slot = schedule.clone();
    }
    app.redteam.save_schedules().await?;
    Ok(Json(schedule))
}

pub async fn delete_schedule(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let removed = {
        let mut schedules = app.redteam.schedules.write().await;
        let before = schedules.len();
        schedules.retain(|s| s.id != id);
        before != schedules.len()
    };
    if !removed {
        return Err(client_error(StatusCode::NOT_FOUND, "no such red-team schedule"));
    }
    app.redteam.save_schedules().await?;
    Ok(Json(json!({"deleted": id})))
}

/// Fires every due schedule: one armed run per repository through [`start`], exactly like a manual
/// start with "when the nest is empty", then books the next run. A repository that already has an
/// active run is skipped and says so in `last_result`.
pub(crate) async fn fire_due(app: &Shared, now: DateTime<Utc>) {
    let due_ids = due(&app.redteam.schedules.read().await, now);
    for id in due_ids {
        let Some(schedule) = app.redteam.schedules.read().await.iter().find(|s| s.id == id).cloned() else {
            continue;
        };
        let mut notes = Vec::new();
        for repo in &schedule.repos {
            let req = NewRedTeamRun {
                repo: repo.clone(),
                hunter: Some(schedule.hunter.clone()),
                model: schedule.model.clone(),
                subagent_model: schedule.subagent_model.clone(),
                swarm_size: Some(schedule.swarm_size),
                modules: None,
                autofix: Some(schedule.autofix),
                arm: Some(true),
            };
            match start(app, req, Some(schedule.id.clone())).await {
                Ok(run) => notes.push(format!("{repo}: started {}", run.id)),
                Err(e) => notes.push(format!("{repo}: {}", e.message())),
            }
        }
        {
            let mut schedules = app.redteam.schedules.write().await;
            if let Some(s) = schedules.iter_mut().find(|s| s.id == id) {
                s.last_run_at = Some(now);
                s.last_result = Some(notes.join("; "));
                s.next_run_at = next_run_after(&s.cadence, now);
            }
        }
        if let Err(e) = app.redteam.save_schedules().await {
            eprintln!("redteam: could not save the red-team schedules: {e:#}");
        }
    }
}

/// The schedule loop, spawned next to [`run`]: once a minute, fire whatever is due.
pub async fn run_schedules(app: Shared) {
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
    use crate::tests::test_app;
    use chrono::{Duration as ChronoDuration, TimeZone};

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-redteam-{}", short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }

    async fn push_colony(app: &Shared, id: &str, status: SessionStatus) {
        let mut s = colony("acme", status);
        s.id = id.to_string();
        s.sandbox = format!("colonizer-{id}");
        app.sessions.write().await.push(s);
    }

    /// A create request: repo, swarm, armed or not.
    fn new_run(repo: &str, swarm_size: Option<usize>, arm: bool) -> NewRedTeamRun {
        NewRedTeamRun {
            repo: repo.into(),
            hunter: None,
            model: None,
            subagent_model: None,
            swarm_size,
            modules: None,
            autofix: None,
            arm: Some(arm),
        }
    }

    /// A findings ledger for one hunter, as validation (#211) writes it: one line per stage.
    fn write_ledger(app: &Shared, hunter: &str, lines: &str) {
        std::fs::create_dir_all(app.session_dir(hunter)).unwrap();
        std::fs::write(app.session_dir(hunter).join("findings.jsonl"), lines).unwrap();
    }

    /// How many synthesis colonies the app has launched so far.
    async fn synthesis_colonies(app: &Shared) -> usize {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| s.issue_title.starts_with("Red-team synthesis"))
            .count()
    }

    /// A two-hunter run with findings worth synthesizing: both hunters reported "a" and each one
    /// finding of its own, so `found` is 3 raw findings over 3 distinct defects. Hunter 2's "a"
    /// also has a raw `finding` event with a body far past any brief budget.
    async fn seeded_run(app: &Shared) -> (String, Vec<String>) {
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        let id = run.id.clone();
        let ids: Vec<String> = run.hunters.iter().map(|h| h.session_id.clone()).collect();
        write_ledger(
            app,
            &ids[0],
            "{\"title\":\"a\",\"state\":\"validated\",\"severity\":\"high\"}\n{\"title\":\"b\",\"state\":\"rejected\",\"reason\":\"not a bug\"}\n",
        );
        write_ledger(
            app,
            &ids[1],
            "{\"title\":\"a\",\"state\":\"validated\",\"severity\":\"high\"}\n",
        );
        std::fs::write(
            app.session_dir(&ids[1]).join("events.jsonl"),
            format!(
                "{{\"type\":\"finding\",\"title\":\"a\",\"body\":\"{}\",\"evidence\":\"panic on input 7\"}}\n",
                "x".repeat(40_000)
            ),
        )
        .unwrap();
        (id, ids)
    }

    /// Ends both hunters of a seeded run and runs the tick that lands it done — which also
    /// launches synthesis.
    async fn land_done(app: &Shared, ids: &[String]) {
        for id in ids {
            app.update_session(id, |s| s.status = SessionStatus::Merged).await;
        }
        tick_once(app).await;
    }

    #[tokio::test]
    async fn starting_a_run_while_a_colony_is_live_is_rejected_with_the_live_count() {
        let root = temp_root();
        let app = test_app(&root);
        push_colony(&app, "colony", SessionStatus::Running).await;
        let err = create(State(app.clone()), Json(new_run("acme/repo", None, false)))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        let message = err.message();
        assert!(message.contains("1 colony is live"), "{message}");
        assert!(message.contains("can only start when the nest is empty"), "{message}");
        // Two live colonies are named by their count.
        push_colony(&app, "another", SessionStatus::Idle).await;
        let err = create(State(app.clone()), Json(new_run("acme/repo", None, false)))
            .await
            .unwrap_err();
        assert!(err.message().contains("2 colonies are live"), "{}", err.message());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn starting_on_an_empty_nest_creates_hunters_with_distinct_briefs() {
        let root = temp_root();
        let app = test_app(&root);
        let run = create(State(app.clone()), Json(new_run("acme/repo", None, false)))
            .await
            .unwrap()
            .0;
        assert_eq!(run.swarm_size, 3, "the default swarm is three");
        assert_eq!(run.state, RedTeamState::Running, "start-now launches immediately");
        assert!(run.started_at.is_some());
        assert!(!run.autofix, "autofix defaults to off");
        assert_eq!(run.hunters.len(), 3);
        let sessions = app.sessions.read().await;
        let hunters: Vec<&Session> = run
            .hunters
            .iter()
            .map(|h| {
                sessions
                    .iter()
                    .find(|s| s.id == h.session_id)
                    .expect("the hunter session exists")
            })
            .collect();
        assert_eq!(hunters.len(), 3);
        for h in &run.hunters {
            assert_eq!(h.module, "general", "the default module is general");
        }
        let titles: Vec<&str> = run.hunters.iter().map(|h| h.title.as_str()).collect();
        let unique_titles: std::collections::HashSet<_> = titles.iter().collect();
        assert_eq!(unique_titles.len(), 3, "every hunter gets its own title");
        let mut instructions: Vec<&str> = hunters.iter().map(|s| s.instructions.as_str()).collect();
        instructions.sort();
        for pair in instructions.windows(2) {
            assert_ne!(pair[0], pair[1], "no two hunters share a brief");
        }
        let first = instructions[0].to_string();
        assert!(first.contains("red-team hunter 1"), "{first}");
        assert!(first.contains("NEVER open, merge or autofix"), "{first}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_armed_run_waits_while_colonies_are_live_and_launches_once_the_nest_empties() {
        let root = temp_root();
        let app = test_app(&root);
        push_colony(&app, "live-1", SessionStatus::Running).await;
        push_colony(&app, "live-2", SessionStatus::Idle).await;
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), true)))
            .await
            .unwrap()
            .0;
        assert_eq!(run.state, RedTeamState::Armed, "armed stays armed");
        assert!(run.started_at.is_none());
        assert!(run.hunters.is_empty());
        let reason = run.gate_reason.expect("an armed run while colonies are live says why");
        assert!(reason.contains("2 colonies are live"), "{reason}");
        // Ticks while the nest is still occupied do not launch it.
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Armed);
        assert!(
            run.hunters.is_empty(),
            "the armed run must not launch while colonies are live"
        );
        // The nest empties: the next tick launches the swarm.
        app.sessions.write().await.clear();
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id)).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Running);
        assert_eq!(run.hunters.len(), 2);
        assert!(run.started_at.is_some());
        assert!(run.gate_reason.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn all_queued_hunters_hold_the_run_in_waiting_until_one_starts() {
        let root = temp_root();
        let app = test_app(&root);
        // Fill the global parallel slots (3) with publishing colonies: publishing holds a slot but
        // is not live, so the nest gate stays open and the launch comes back with every hunter
        // queued — and the run is `waiting` from the start, without a tick to demote it.
        for i in 0..3 {
            push_colony(&app, &format!("pub-{i}"), SessionStatus::Publishing).await;
        }
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        assert_eq!(run.hunters.len(), 2);
        assert_eq!(
            run.state,
            RedTeamState::Waiting,
            "an all-queued launch starts waiting, not running"
        );
        assert!(run.started_at.is_some(), "the launch happened even while every hunter queued");
        let ids: Vec<String> = run.hunters.iter().map(|h| h.session_id.clone()).collect();
        // The next tick says why in gate_reason; the state itself was already right at launch.
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Waiting);
        assert!(
            run.gate_reason.as_deref().unwrap_or_default().contains("queued"),
            "{:?}",
            run.gate_reason
        );
        // One hunter gets a slot: the run goes running.
        app.update_session(&ids[0], |s| s.status = SessionStatus::Starting).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id)).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Running, "a started hunter takes the run live");
        assert!(run.gate_reason.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// `create` never stores an empty `modules` (it coerces the empty request field to the
    /// default), but `redteam.json` is trusted on load, so a run written by hand or by an older
    /// build can carry one — and the tick's launch would panic on `i % 0` before the first hunter
    /// brief was built. It must read the way `create` would have written it.
    #[tokio::test]
    async fn a_persisted_run_with_no_modules_is_read_as_the_default_module() {
        let root = temp_root();
        let run = RedTeamRun {
            id: "rt_mods".into(),
            repo: "acme/repo".into(),
            org: "acme".into(),
            state: RedTeamState::Armed,
            swarm_size: 3,
            modules: Vec::new(),
            ..RedTeamRun::default()
        };
        std::fs::write(
            root.join("data").join("redteam.json"),
            serde_json::to_vec(&vec![run]).unwrap(),
        )
        .unwrap();
        let app = test_app(&root);
        assert_eq!(app.redteam.runs.read().await.len(), 1, "the run survived the load");
        tick_once(&app).await;
        let run = get(State(app.clone()), Path("rt_mods".into())).await.unwrap().0;
        assert_eq!(run.modules, vec![DEFAULT_MODULE], "read as the default module");
        assert_eq!(run.state, RedTeamState::Running);
        assert_eq!(run.hunters.len(), 3, "the swarm launched on the default module");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_run_whose_hunter_sessions_are_gone_drains_to_done_instead_of_relaunching() {
        let root = temp_root();
        let app = test_app(&root);
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        assert_eq!(run.state, RedTeamState::Running);
        assert_eq!(run.hunters.len(), 2);
        // A restart: the run was recorded `running` with its hunters on the list before the process
        // died, but the hunter sessions never came back. Reload the store like a fresh boot.
        let restarted = test_app(&root);
        assert_eq!(
            restarted.redteam.runs.read().await.len(),
            1,
            "the run survived the restart in redteam.json"
        );
        // The tick must not re-launch the swarm: the run is no longer `armed`, and missing hunter
        // sessions count as ended, so one pass drains the run to done with no new sessions.
        tick_once(&restarted).await;
        let run = get(State(restarted.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done, "missing hunters count as ended");
        assert!(run.ended_at.is_some());
        assert!(
            restarted.sessions.read().await.is_empty(),
            "a lost swarm is drained, never re-launched"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_run_drains_while_hunters_are_in_flight_and_done_once_they_end() {
        let root = temp_root();
        let app = test_app(&root);
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        let ids: Vec<String> = run.hunters.iter().map(|h| h.session_id.clone()).collect();
        for id in &ids {
            app.update_session(id, |s| s.status = SessionStatus::Running).await;
        }
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Running, "live hunters keep the run running");
        // Both leave the microVM (publishing / PR open): no hunter is live but none has ended.
        app.update_session(&ids[0], |s| s.status = SessionStatus::Publishing).await;
        app.update_session(&ids[1], |s| s.status = SessionStatus::PrOpened).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Draining);
        // Every hunter ends: the run is done, with an ended_at.
        app.update_session(&ids[0], |s| s.status = SessionStatus::Merged).await;
        app.update_session(&ids[1], |s| s.status = SessionStatus::Closed).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id)).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        assert!(run.ended_at.is_some());
        // A done run is terminal: ticks leave it untouched.
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stop_marks_the_run_stopped_and_stops_its_hunters() {
        let root = temp_root();
        let app = test_app(&root);
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        let h1 = run.hunters[0].session_id.clone();
        let h2 = run.hunters[1].session_id.clone();
        app.update_session(&h1, |s| s.status = SessionStatus::Running).await;
        app.update_session(&h2, |s| s.status = SessionStatus::Queued).await;
        let stopped = stop(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(stopped.state, RedTeamState::Stopped);
        assert!(stopped.ended_at.is_some());
        let sessions = app.sessions.read().await;
        for hunter in [&h1, &h2] {
            let session = sessions.iter().find(|s| s.id == *hunter).expect("the hunter session exists");
            assert_eq!(session.status, SessionStatus::Stopped, "hunter {hunter} is stopped");
        }
        drop(sessions);
        // Stopping again is idempotent: the run comes back stopped, unchanged.
        let again = stop(State(app.clone()), Path(run.id.clone())).await.unwrap().0;
        assert_eq!(again.state, RedTeamState::Stopped);
        assert_eq!(again.hunters.len(), 2);
        // Stopping an unknown run is a 404.
        let err = stop(State(app.clone()), Path("rt_nope".into())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn only_one_active_run_per_repo_at_a_time() {
        let root = temp_root();
        let app = test_app(&root);
        let first = create(State(app.clone()), Json(new_run("acme/repo", Some(1), false)))
            .await
            .unwrap()
            .0;
        assert_eq!(first.hunters.len(), 1);
        let err = create(State(app.clone()), Json(new_run("acme/repo", Some(1), true)))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(err.message().contains("already active for acme/repo"), "{}", err.message());
        // A second repo is free to raid at the same time — armed, since this run's first hunter is
        // live and the nest gate is global.
        let other = create(State(app.clone()), Json(new_run("other/repo", Some(1), true)))
            .await
            .unwrap()
            .0;
        assert_eq!(other.repo, "other/repo");
        assert_eq!(other.state, RedTeamState::Armed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_swarm_size_is_validated_and_defaults_to_three() {
        let root = temp_root();
        let app = test_app(&root);
        for bad in [Some(0usize), Some(9)] {
            let err = create(State(app.clone()), Json(new_run("acme/repo", bad, false)))
                .await
                .unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
            assert!(err.message().contains("swarm_size"), "{}", err.message());
        }
        // None → 3, armed so the gate cannot interfere.
        let run = create(State(app.clone()), Json(new_run("acme/repo", None, true)))
            .await
            .unwrap()
            .0;
        assert_eq!(run.swarm_size, 3);
        assert_eq!(run.modules, vec![DEFAULT_MODULE]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unknown_module_is_refused_and_the_known_ones_are_named() {
        let root = temp_root();
        let app = test_app(&root);
        let mut req = new_run("acme/repo", None, true);
        req.modules = Some(vec![DEFAULT_MODULE.into(), "foo".into()]);
        let err = create(State(app.clone()), Json(req)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "unknown red-team module \"foo\"; known modules: general");
        assert!(app.redteam.runs.read().await.is_empty(), "a refused run is not created");

        let mut req = new_run("acme/repo", None, true);
        req.modules = Some(vec![DEFAULT_MODULE.into()]);
        let run = create(State(app.clone()), Json(req)).await.unwrap().0;
        assert_eq!(run.modules, vec![DEFAULT_MODULE]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn run_counts_are_derived_from_the_hunters_findings_records() {
        let root = temp_root();
        let app = test_app(&root);
        let run = create(State(app.clone()), Json(new_run("acme/repo", Some(1), false)))
            .await
            .unwrap()
            .0;
        let hunter = &run.hunters[0].session_id;
        tokio::fs::create_dir_all(app.session_dir(hunter)).await.unwrap();
        // The ledger as the current build writes it: one line per stage, keyed by title. "a" went all
        // the way to a merged fix, "b" matched an open issue, "c" was rejected, "d" was validated but
        // never filed (the cap, or a GitHub error), and "e" failed validation outright. A legacy line
        // with no state is read by what it carries.
        std::fs::write(
            app.session_dir(hunter).join("findings.jsonl"),
            concat!(
                "{\"title\":\"a\",\"state\":\"validated\",\"severity\":\"high\"}\n",
                "{\"title\":\"a\",\"state\":\"filed\",\"issue\":\"https://github.com/acme/repo/issues/1\"}\n",
                "{\"title\":\"a\",\"state\":\"fix_colony\",\"fix_session\":\"f1\",\"issue\":\"https://github.com/acme/repo/issues/1\"}\n",
                "{\"title\":\"a\",\"state\":\"review\",\"review_session\":\"r1\",\"verdict\":\"approve\",\"pr\":\"https://github.com/acme/repo/pull/9\"}\n",
                "{\"title\":\"a\",\"state\":\"merged\",\"pr\":\"https://github.com/acme/repo/pull/9\"}\n",
                "{\"title\":\"b\",\"state\":\"validated\",\"severity\":\"low\"}\n",
                "{\"title\":\"b\",\"state\":\"duplicate\",\"duplicate_of\":\"https://github.com/acme/repo/issues/2\"}\n",
                "{\"title\":\"c\",\"state\":\"rejected\",\"reason\":\"not a bug\"}\n",
                "{\"title\":\"d\",\"state\":\"validated\",\"severity\":\"medium\"}\n",
                "{\"title\":\"e\",\"state\":\"error\",\"reason\":\"model timed out\"}\n",
                "{\"title\":\"legacy\",\"issue\":\"https://github.com/acme/repo/issues/3\"}\n",
            ),
        )
        .unwrap();
        let counts = counts_for(&app, &run);
        assert_eq!(
            counts,
            Counts {
                found: 6,
                validated: 3,
                rejected: 1,
                filed: 3,
                merged: None,
            }
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn synthesis_fires_once_at_done_with_an_inline_brief_and_never_anywhere_else() {
        let root = temp_root();
        let app = test_app(&root);
        let (id, ids) = seeded_run(&app).await;
        for sid in &ids {
            app.update_session(sid, |s| s.status = SessionStatus::Running).await;
        }
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Running);
        assert!(run.synthesis.is_none(), "never mid-run");
        land_done(&app, &ids).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        assert_eq!(run.counts.found, 3, "raw findings: a defect two hunters report counts twice");
        let synthesis = run.synthesis.clone().expect("a done run with findings synthesizes");
        assert!(matches!(synthesis.state, SynthesisState::Pending | SynthesisState::Running));
        assert!(synthesis.session_id.is_some(), "the synthesis colony is attached");
        assert_eq!(synthesis_colonies(&app).await, 1);
        tick_once(&app).await;
        assert_eq!(synthesis_colonies(&app).await, 1, "further ticks never launch a second one");
        // The brief publishes nothing and carries the ledgers inline, cut under the cap.
        let brief = synthesis_brief(&app, &run);
        for key in ["autopilot", "autofix", "automerge"] {
            assert_eq!(brief[key], false, "the judge publishes nothing: {key} is off");
        }
        assert_eq!(brief["allow_duplicate"], true);
        let instructions = brief["instructions"].as_str().unwrap();
        for hunter in &run.hunters {
            assert!(instructions.contains(&hunter.session_id) && instructions.contains(&hunter.focus));
        }
        assert!(instructions.contains("state validated, severity high"), "{instructions}");
        assert!(instructions.contains("state rejected, reason: not a bug"), "{instructions}");
        assert!(instructions.contains("body: xxx") && instructions.contains("panic on input 7"));
        assert!(instructions.contains("…"), "an oversized body is cut with a marker");
        assert!(instructions.chars().count() < 20_000, "fits the instructions cap");
        // The phase moves independently of the run, which stays done: queued for a slot it is
        // still pending (a colony dying fails the synthesis with its error — shown in the retry
        // test below — and the run is untouched either way).
        let sid = run.synthesis.unwrap().session_id.unwrap();
        app.update_session(&sid, |s| s.status = SessionStatus::Queued).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        assert_eq!(run.synthesis.as_ref().unwrap().state, SynthesisState::Pending);
        // A done run that found nothing has nothing to merge — and with the colony gone idle the
        // nest is empty again.
        let empty = create(State(app.clone()), Json(new_run("other/repo", Some(2), false)))
            .await
            .unwrap()
            .0;
        let empty_ids: Vec<String> = empty.hunters.iter().map(|h| h.session_id.clone()).collect();
        land_done(&app, &empty_ids).await;
        let empty_id = empty.id.clone();
        let run = get(State(app.clone()), Path(empty_id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        assert_eq!(run.counts.found, 0);
        assert!(run.synthesis.is_none(), "nothing to merge, no synthesis");
        let err = synthesize(State(app.clone()), Path(empty_id)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(err.message().contains("nothing to merge"), "{}", err.message());
        // And a stop wins too: no synthesis follows a stopped run.
        let stopped = create(State(app.clone()), Json(new_run("third/repo", Some(1), false)))
            .await
            .unwrap()
            .0;
        let stopped = stop(State(app.clone()), Path(stopped.id)).await.unwrap().0;
        assert_eq!(stopped.state, RedTeamState::Stopped);
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(stopped.id)).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Stopped);
        assert!(run.synthesis.is_none(), "never on a stopped run");
        // The stop race, deterministically: the pending record lands while the run is done and a
        // stop lands before the attach — the judge is refused, the synthesis fails with why, and
        // the just-created colony is stopped instead of orbiting a stopped run forever.
        let race = create(State(app.clone()), Json(new_run("fourth/repo", Some(1), false)))
            .await
            .unwrap()
            .0;
        app.redteam
            .update(&race.id, |r| {
                r.state = RedTeamState::Done; // the tick landed done and recorded the phase…
                r.counts.found = 1;
                r.synthesis = Some(Synthesis {
                    state: SynthesisState::Pending,
                    session_id: None,
                    report: None,
                    reason: None,
                    superseded: vec![],
                });
            })
            .await;
        app.redteam.update(&race.id, |r| r.state = RedTeamState::Stopped).await; // …then a stop won
        use crate::sessions::tests::colony;
        let mut judge = colony("fourth", SessionStatus::Starting);
        judge.id = "judge_race".into();
        tokio::fs::create_dir_all(app.session_dir(&judge.id)).await.unwrap();
        app.sessions.write().await.push(judge.clone());
        attach_synthesis(&app, &race.id, Ok(judge)).await;
        let run = get(State(app.clone()), Path(race.id)).await.unwrap().0;
        let synthesis = run.synthesis.expect("the phase outlives the race");
        assert_eq!(synthesis.state, SynthesisState::Failed);
        assert_eq!(synthesis.reason.as_deref(), Some("the run was stopped"));
        assert!(synthesis.session_id.is_none(), "the judge is not attached to a stopped run");
        let sessions = app.sessions.read().await;
        assert_eq!(
            sessions.iter().find(|s| s.id == "judge_race").unwrap().status,
            SessionStatus::Stopped,
            "the unattached judge is stopped, not left live"
        );
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_finished_synthesis_links_its_report_counts_merged_and_a_retry_supersedes_it() {
        let root = temp_root();
        let app = test_app(&root);
        let (id, ids) = seeded_run(&app).await;
        land_done(&app, &ids).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        let first = run
            .synthesis
            .expect("synthesis launched")
            .session_id
            .expect("the colony is attached");
        // The synthesis colony merges the two hunters' shared defect into one line naming both,
        // writes the report (plus a line that parses to nothing) and ends.
        let defect = |title: &str, hunters: &[&str], merged_from: u32, validation: &str| {
            json!({"defect": title, "severity": "high", "reproduction": "reproduced", "steps": "curl it",
                "files": ["src/a.rs"], "hunters": hunters, "merged_from": merged_from, "validation": validation})
            .to_string()
        };
        let (h1, h2) = (ids[0].as_str(), ids[1].as_str());
        let out = app.session_dir(&first).join("out");
        let linked = out.join("redteam-report.jsonl").display().to_string();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("redteam-report.jsonl"),
            format!(
                "{}\n{}\nnot json at all\n",
                defect("sql injection", &[h1, h2], 2, "validated"),
                defect("loop off by one", &[h2], 1, "unvalidated"),
            ),
        )
        .unwrap();
        app.update_session(&first, |s| s.status = SessionStatus::Idle).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        let synthesis = run.synthesis.expect("the synthesis finished");
        assert_eq!(synthesis.state, SynthesisState::Done);
        assert_eq!(synthesis.report.as_deref(), Some(linked.as_str()));
        assert_eq!(run.counts.merged, Some(2), "the unparseable line is skipped");
        assert!(run.counts.merged.unwrap() <= run.counts.found);
        // The linked report parses back out of the read route, both hunters on the shared defect.
        let defects = report(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(defects.len(), 2);
        assert_eq!(defects[0].hunters, ids, "the merged line names both hunters");
        assert_eq!(defects[0].merged_from, 2);
        assert_eq!(defects[0].validation, "validated");
        assert_eq!(defects[1].hunters, vec![ids[1].clone()]);
        // A retry from done supersedes the colony and keeps the old report linked until beaten.
        let run = synthesize(State(app.clone()), Path(id.clone())).await.unwrap().0;
        let synthesis = run.synthesis.unwrap();
        let second = synthesis.session_id.clone().unwrap();
        assert_eq!(synthesis.superseded, vec![first.clone()]);
        assert_eq!(synthesis.report.as_deref(), Some(linked.as_str()));
        assert_eq!(run.counts.merged, Some(2));
        // A second POST while it is in flight is idempotent: the run comes back unchanged.
        let again = synthesize(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(again.synthesis.as_ref().unwrap().session_id.as_deref(), Some(second.as_str()));
        assert_eq!(again.synthesis.as_ref().unwrap().superseded, vec![first.clone()]);
        assert_eq!(
            synthesis_colonies(&app).await,
            2,
            "the idempotent retry launched no second colony"
        );
        // The second colony dies: the synthesis fails with its error, and the first report stays
        // linked — a failed retry never unlinks the newest success, and the run stays done.
        app.update_session(&second, |s| {
            s.status = SessionStatus::Failed;
            s.error = Some("the microVM died".into());
        })
        .await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(run.state, RedTeamState::Done);
        assert_eq!(run.synthesis.as_ref().unwrap().state, SynthesisState::Failed);
        assert_eq!(run.synthesis.as_ref().unwrap().reason.as_deref(), Some("the microVM died"));
        assert_eq!(run.synthesis.as_ref().unwrap().report.as_deref(), Some(linked.as_str()));
        // Retrying the failed one works too: the third colony finishes and its report wins, while
        // the superseded reports stay on disk.
        let run = synthesize(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(
            run.synthesis.as_ref().unwrap().superseded,
            vec![first.clone(), second.clone()]
        );
        let third = run.synthesis.as_ref().unwrap().session_id.clone().unwrap();
        let third_report = app.session_dir(&third).join("out").join("redteam-report.jsonl");
        std::fs::create_dir_all(app.session_dir(&third).join("out")).unwrap();
        let lines: Vec<String> = (0..2)
            .map(|i| defect(&format!("defect {i}"), &["h1"], 1, "validated"))
            .collect();
        std::fs::write(&third_report, lines.join("\n") + "\n").unwrap();
        app.update_session(&third, |s| s.status = SessionStatus::Merged).await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        let synthesis = run.synthesis.unwrap();
        assert_eq!(synthesis.state, SynthesisState::Done);
        assert_eq!(
            synthesis.report.as_deref(),
            Some(third_report.display().to_string()).as_deref()
        );
        assert_eq!(run.counts.merged, Some(2));
        assert!(out.join("redteam-report.jsonl").exists(), "superseded reports stay on disk");
        // A `pending` stranded without a colony — a crash between the record and the attach — is
        // not in flight: the tick leaves it alone and a retry launches a fresh judge instead of
        // queueing behind it forever.
        app.redteam
            .update(&id, |r| {
                if let Some(s) = &mut r.synthesis {
                    let (report, superseded) = (s.report.clone(), s.superseded.clone());
                    *s = Synthesis {
                        state: SynthesisState::Pending,
                        session_id: None,
                        report,
                        reason: None,
                        superseded,
                    };
                }
            })
            .await;
        tick_once(&app).await;
        let run = get(State(app.clone()), Path(id.clone())).await.unwrap().0;
        assert_eq!(run.synthesis.as_ref().unwrap().state, SynthesisState::Pending);
        assert!(run.synthesis.as_ref().unwrap().session_id.is_none());
        let run = synthesize(State(app.clone()), Path(id.clone())).await.unwrap().0;
        let synthesis = run.synthesis.unwrap();
        assert!(synthesis.session_id.is_some(), "the stranded phase is retryable");
        assert_eq!(
            synthesis.superseded,
            vec![first.clone(), second.clone()],
            "a stranded phase had no colony to supersede"
        );
        assert_eq!(
            synthesis.report.as_deref(),
            Some(third_report.display().to_string()).as_deref(),
            "the newest success stays linked"
        );
        assert_eq!(synthesis_colonies(&app).await, 4);
        // Synthesis on a run that is not done is a 409; an unknown run is a 404.
        let armed = create(State(app.clone()), Json(new_run("other/repo", Some(1), true)))
            .await
            .unwrap()
            .0;
        let err = synthesize(State(app.clone()), Path(armed.id)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        let err = synthesize(State(app.clone()), Path("rt_nope".into())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_gate_messages_name_the_live_count() {
        assert_eq!(gate_reason(1), "1 colony is live — waiting for the nest to empty");
        assert_eq!(gate_reason(3), "3 colonies are live — waiting for the nest to empty");
        assert!(refused_message(2).contains("2 colonies are live"));
    }

    /// The fixed JSON shape the web UI codes against: snake_case state, nulls for the not-yet-set
    /// optionals, and the four count keys. A change here is a contract change for the frontend.
    #[test]
    fn the_run_serialises_under_the_fixed_contract_shape() {
        let run = RedTeamRun {
            id: "rt_ab12cd34".into(),
            repo: "acme/repo".into(),
            org: "acme".into(),
            state: RedTeamState::Armed,
            swarm_size: 3,
            modules: vec![DEFAULT_MODULE.into()],
            autofix: false,
            hunters: vec![Hunter {
                session_id: "ab12cd34".into(),
                title: "Red-team hunter 1/3: error handling and edge cases".into(),
                module: "general".into(),
                version: None,
                focus: "error handling and edge cases".into(),
            }],
            counts: Counts::default(),
            synthesis: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            started_at: None,
            ended_at: None,
            gate_reason: None,
            hunter: "swarm".into(),
            model: None,
            subagent_model: None,
            schedule_id: None,
        };
        let value = serde_json::to_value(&run).unwrap();
        assert_eq!(value["hunter"], "swarm");
        assert!(value["model"].is_null() && value["subagent_model"].is_null() && value["schedule_id"].is_null());
        assert_eq!(value["state"], "armed");
        assert_eq!(value["hunters"][0]["module"], "general");
        assert!(value["hunters"][0]["version"].is_null(), "version stays null until #216");
        assert!(value["started_at"].is_null() && value["ended_at"].is_null() && value["gate_reason"].is_null());
        assert!(value["synthesis"].is_null(), "no synthesis before it fires");
        assert_eq!(value["created_at"], "1970-01-01T00:00:00Z", "timestamps are RFC 3339 strings");
        for key in ["found", "validated", "rejected", "filed"] {
            assert_eq!(value["counts"][key], 0, "count key {key}");
        }
        assert!(
            value["counts"]["merged"].is_null(),
            "merged stays null until a synthesis finishes"
        );
        // A synthesised run carries the phase under its fixed keys.
        let mut done = run.clone();
        done.synthesis = Some(Synthesis {
            state: SynthesisState::Done,
            session_id: Some("deadbeef".into()),
            report: Some("/data/sessions/deadbeef/out/redteam-report.jsonl".into()),
            reason: None,
            superseded: vec!["cafe1111".into()],
        });
        done.counts.merged = Some(3);
        let value = serde_json::to_value(&done).unwrap();
        assert_eq!(value["synthesis"]["state"], "done");
        assert_eq!(value["synthesis"]["session_id"], "deadbeef");
        assert_eq!(
            value["synthesis"]["report"],
            "/data/sessions/deadbeef/out/redteam-report.jsonl"
        );
        assert!(value["synthesis"]["reason"].is_null());
        assert_eq!(value["synthesis"]["superseded"][0], "cafe1111");
        assert_eq!(value["counts"]["merged"], 3);
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn weekly_schedules_fire_on_the_next_matching_weekday_after_now() {
        // 2026-09-24 is a Thursday (weekday 3).
        let thu = Cadence::Weekly {
            weekday: 3,
            hour: 9,
            minute: 0,
        };
        assert_eq!(
            next_run_after(&thu, utc(2026, 9, 24, 8, 59)),
            utc(2026, 9, 24, 9, 0),
            "later today"
        );
        assert_eq!(
            next_run_after(&thu, utc(2026, 9, 24, 9, 0)),
            utc(2026, 10, 1, 9, 0),
            "strictly after: exactly now is next week"
        );
        let mon = Cadence::Weekly {
            weekday: 0,
            hour: 2,
            minute: 30,
        };
        assert_eq!(
            next_run_after(&mon, utc(2026, 9, 24, 12, 0)),
            utc(2026, 9, 28, 2, 30),
            "wraps into next week"
        );
        let sun = Cadence::Weekly {
            weekday: 6,
            hour: 23,
            minute: 59,
        };
        assert_eq!(
            next_run_after(&sun, utc(2026, 12, 31, 0, 0)),
            utc(2027, 1, 3, 23, 59),
            "across a year end"
        );
    }

    #[test]
    fn monthly_schedules_clamp_to_the_months_last_day() {
        let d31 = Cadence::Monthly {
            day: 31,
            hour: 6,
            minute: 0,
        };
        assert_eq!(
            next_run_after(&d31, utc(2026, 9, 24, 0, 0)),
            utc(2026, 9, 30, 6, 0),
            "September has 30 days"
        );
        assert_eq!(next_run_after(&d31, utc(2026, 9, 30, 6, 0)), utc(2026, 10, 31, 6, 0));
        assert_eq!(
            next_run_after(&d31, utc(2027, 2, 1, 0, 0)),
            utc(2027, 2, 28, 6, 0),
            "February, common year"
        );
        assert_eq!(
            next_run_after(&d31, utc(2028, 2, 1, 0, 0)),
            utc(2028, 2, 29, 6, 0),
            "February, leap year"
        );
        let d1 = Cadence::Monthly {
            day: 1,
            hour: 0,
            minute: 0,
        };
        assert_eq!(
            next_run_after(&d1, utc(2026, 12, 15, 0, 0)),
            utc(2027, 1, 1, 0, 0),
            "across a year end"
        );
        let d15 = Cadence::Monthly {
            day: 15,
            hour: 12,
            minute: 0,
        };
        assert_eq!(
            next_run_after(&d15, utc(2026, 9, 15, 11, 0)),
            utc(2026, 9, 15, 12, 0),
            "later the same day"
        );
    }

    #[test]
    fn cadences_out_of_range_are_refused() {
        assert!(
            Cadence::Weekly {
                weekday: 7,
                hour: 0,
                minute: 0
            }
            .check()
            .is_err()
        );
        assert!(
            Cadence::Monthly {
                day: 0,
                hour: 0,
                minute: 0
            }
            .check()
            .is_err()
        );
        assert!(
            Cadence::Monthly {
                day: 32,
                hour: 0,
                minute: 0
            }
            .check()
            .is_err()
        );
        assert!(
            Cadence::Weekly {
                weekday: 1,
                hour: 24,
                minute: 0
            }
            .check()
            .is_err()
        );
        assert!(
            Cadence::Weekly {
                weekday: 1,
                hour: 23,
                minute: 60
            }
            .check()
            .is_err()
        );
        assert!(
            Cadence::Monthly {
                day: 31,
                hour: 23,
                minute: 59
            }
            .check()
            .is_ok()
        );
    }

    #[test]
    fn only_enabled_schedules_whose_time_has_come_are_due() {
        let now = utc(2026, 9, 24, 12, 0);
        let schedule = |id: &str, enabled: bool, next: DateTime<Utc>| RedTeamSchedule {
            id: id.into(),
            org: "acme".into(),
            repos: vec!["acme/web".into()],
            hunter: "swarm".into(),
            swarm_size: 3,
            model: None,
            subagent_model: None,
            autofix: false,
            cadence: Cadence::Weekly {
                weekday: 0,
                hour: 0,
                minute: 0,
            },
            enabled,
            next_run_at: next,
            last_run_at: None,
            last_result: None,
            created_at: now,
        };
        let list = vec![
            schedule("past", true, utc(2026, 9, 24, 11, 59)),
            schedule("exactly", true, now),
            schedule("future", true, utc(2026, 9, 24, 12, 1)),
            schedule("off", false, utc(2026, 9, 1, 0, 0)),
        ];
        assert_eq!(due(&list, now), vec!["past".to_string(), "exactly".to_string()]);
    }

    #[test]
    fn external_hunters_are_refused_with_the_reason_and_the_swarm_runs() {
        assert_eq!(check_hunter(None).unwrap(), "swarm");
        assert_eq!(check_hunter(Some(" swarm ")).unwrap(), "swarm");
        let strix = check_hunter(Some("strix")).unwrap_err();
        assert!(strix.contains("Strix") && strix.contains("not driven by runs"), "{strix}");
        assert!(check_hunter(Some("shannon")).unwrap_err().contains("Shannon"));
        assert!(check_hunter(Some("nmap")).unwrap_err().contains("unknown hunter"));
    }

    #[tokio::test]
    async fn a_due_schedule_arms_one_run_per_repo_and_books_the_next_run() {
        let root = temp_root();
        let app = test_app(&root);
        // A live colony keeps the gate shut, so the armed runs stay armed and nothing launches.
        push_colony(&app, "colony", SessionStatus::Running).await;
        let now = utc(2026, 9, 24, 12, 0);
        let req: NewSchedule = serde_json::from_value(json!({
            "org": "acme",
            "repos": ["acme/web", "acme/api"],
            "cadence": {"every": "weekly", "weekday": 3, "hour": 12, "minute": 0}
        }))
        .unwrap();
        let mut schedule = schedule_from(&app, req, "rts_x".into(), now, now - ChronoDuration::minutes(1)).unwrap();
        assert_eq!(schedule.next_run_at, now);
        schedule.enabled = true;
        app.redteam.schedules.write().await.push(schedule);
        fire_due(&app, now).await;
        let runs = app.redteam.runs.read().await.clone();
        assert_eq!(runs.len(), 2);
        assert!(
            runs.iter()
                .all(|r| r.state == RedTeamState::Armed && r.schedule_id.as_deref() == Some("rts_x"))
        );
        let schedule = app.redteam.schedules.read().await[0].clone();
        assert_eq!(schedule.last_run_at, Some(now));
        assert_eq!(schedule.next_run_at, utc(2026, 10, 1, 12, 0));
        assert!(schedule.last_result.unwrap().contains("acme/web: started"));
        // Firing again the same minute is a no-op: nothing is due until next week.
        fire_due(&app, now).await;
        assert_eq!(app.redteam.runs.read().await.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_schedule_for_another_orgs_repo_is_refused() {
        let root = temp_root();
        let app = test_app(&root);
        let now = Utc::now();
        let req: NewSchedule = serde_json::from_value(json!({
            "org": "acme",
            "repos": ["other/web"],
            "cadence": {"every": "monthly", "day": 1, "hour": 0, "minute": 0}
        }))
        .unwrap();
        let err = schedule_from(&app, req, "rts_y".into(), now, now).unwrap_err();
        assert!(err.message().contains("not in acme"), "{}", err.message());
        let _ = std::fs::remove_dir_all(root);
    }
}
