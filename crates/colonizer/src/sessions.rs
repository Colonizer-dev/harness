//! Interactive sessions: one worktree + microVM + agent per task, bridged to browsers.
//!
//! Harness ⇄ VM traffic goes to `colonizer-agentd` over the private mesh (or a loopback port when the
//! mesh module is disabled). Agent events are persisted per session and fanned out to every open
//! browser; browser commands are forwarded to the agent.

use crate::{
    client_error,
    config::{setting, setting_str, setting_u64, ModulesConfig},
    findings, github, memory,
    modules::{schema_for, AgentModule},
    orgs, providers, resolve_guest_claude_bin,
    sandbox::{self, BootSpec, Mount, Secret},
    util::{
        append_line, faults::{self, Op}, random_token, read_trimmed, short_id, truncate, valid_repo, write_atomic,
        write_private,
    },
    watchdog::Activity,
    ApiResult, App, Shared, CLAUDE_API_HOST,
};
use anyhow::{bail, Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::Response,
    Json,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{broadcast, mpsc, watch, Mutex},
};
use tokio_tungstenite::{
    tungstenite::{self, client::IntoClientRequest},
    WebSocketStream,
};

const AGENTD_PORT: u16 = 7070;
const MAX_LOGS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Waiting for a free slot: no microVM, no worktree, nothing claimed yet.
    Queued,
    Starting,
    Running,
    WaitingForAnswer,
    Idle,
    Publishing,
    PrOpened,
    /// The pull request was merged; nothing left to watch.
    Merged,
    /// The pull request was closed without merging; GitHub lets it be reopened, so it is still watched.
    Closed,
    NoChanges,
    Stopped,
    Failed,
}

impl SessionStatus {
    /// A microVM is expected to be running.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::WaitingForAnswer | Self::Idle)
    }
}

/// How far the last publish got, persisted on the session so a retry continues from there instead of
/// starting over (and so browsers can show the progress). The publish itself re-derives the truth from
/// git and the remote; this is the durable record of what was confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStage {
    /// The worktree's changes are committed on the colony's branch.
    Committed,
    /// The branch is on origin.
    Pushed,
    /// The pull request is open.
    PrOpened,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeshInfo {
    pub name: String,
    pub ip: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub repo: String,
    /// The GitHub org (repository owner) whose workspace this colony belongs to.
    #[serde(default)]
    pub org: String,
    /// `None` for an open session that starts from the repository alone.
    pub issue: Option<u64>,
    pub issue_title: String,
    #[serde(default)]
    pub instructions: String,
    pub status: SessionStatus,
    pub branch: String,
    pub base: Option<String>,
    pub worktree: String,
    pub git_admin_dir: Option<String>,
    pub sandbox: String,
    pub mesh: Option<MeshInfo>,
    #[serde(default)]
    pub local_port: Option<u16>,
    pub agent: String,
    #[serde(default)]
    pub autopilot: bool,
    pub pr_url: Option<String>,
    /// How far the last publish got; left in place when a publish failed, so a retry knows where to look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_stage: Option<PublishStage>,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    /// Tokens per model from the last turn end, cumulative: `{model: {input_tokens, output_tokens, cache_read_tokens,
    /// cache_write_tokens}}`. `cost_usd` covers only the Claude models among them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<Value>,
    #[serde(default)]
    pub cleaned_up: bool,
    /// Set by the watchdog: `{reason, since, nudges}`.
    #[serde(default)]
    pub attention: Option<Value>,
    /// Last agent progress (filled from the runtime for live colonies).
    #[serde(default)]
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Where the last launch's time went: `{total_ms, phases: [{name, ms}]}`.
    /// Set when a colony finishes booting, and replaced on resume.
    #[serde(default)]
    pub boot_timing: Option<Value>,
    /// The app directory this colony's mounts came from. An update keeps that
    /// directory until no live colony still names it (`update::sweep_slots`).
    #[serde(default)]
    pub app_slot: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// In-memory state for a session's event fan-out and agent link.
pub struct Runtime {
    events: broadcast::Sender<Arc<Broadcast>>,
    commands: mpsc::UnboundedSender<Value>,
    commands_rx: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    last_seq: AtomicU64,
    logs: Mutex<VecDeque<Value>>,
    open_question: Mutex<Option<String>>,
    /// `pr.md` as of the last turn end, so autopilot publishes only when a turn wrote it.
    pr_mark: Mutex<Option<(std::time::SystemTime, u64)>>,
    interrupted: std::sync::atomic::AtomicBool,
    stop: watch::Sender<bool>,
    file_lock: Mutex<()>,
    /// Serialises findings, so the per-colony cap holds when two arrive together.
    findings_lock: Mutex<()>,
    events_path: PathBuf,
    logs_path: PathBuf,
    pub activity: Mutex<Activity>,
}

struct Broadcast {
    seq: Option<u64>,
    json: String,
}

impl Runtime {
    fn load(dir: &std::path::Path) -> Self {
        let events_path = dir.join("events.jsonl");
        let logs_path = dir.join("harness.jsonl");
        let last_seq = std::fs::read_to_string(&events_path)
            .ok()
            .and_then(|content| content.lines().rev().find_map(|l| serde_json::from_str::<Value>(l).ok()?["seq"].as_u64()))
            .unwrap_or(0);
        let logs: VecDeque<Value> = std::fs::read_to_string(&logs_path)
            .map(|content| {
                let all: Vec<Value> = content.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
                all.into_iter().rev().take(MAX_LOGS).rev().collect()
            })
            .unwrap_or_default();
        let (commands, commands_rx) = mpsc::unbounded_channel();
        Self {
            events: broadcast::channel(1024).0,
            commands,
            commands_rx: Mutex::new(Some(commands_rx)),
            last_seq: AtomicU64::new(last_seq),
            logs: Mutex::new(logs),
            open_question: Mutex::new(None),
            pr_mark: Mutex::new(github::pr_description_mark(&dir.join("out"))),
            interrupted: std::sync::atomic::AtomicBool::new(false),
            stop: watch::channel(false).0,
            file_lock: Mutex::new(()),
            findings_lock: Mutex::new(()),
            events_path,
            logs_path,
            activity: Mutex::new(Activity::new(Utc::now())),
        }
    }

    fn broadcast(&self, seq: Option<u64>, json: String) {
        let _ = self.events.send(Arc::new(Broadcast { seq, json }));
    }

    /// Queues a command for the agent (sent once the agent link is connected).
    pub fn send_command(&self, command: Value) {
        let _ = self.commands.send(command);
    }
}

/// A session as shown to browsers: live colonies carry their last agent activity.
async fn with_activity(app: &App, mut session: Session) -> Session {
    if session.status.is_live() {
        let rt = app.runtimes.lock().await.get(&session.id).cloned();
        if let Some(rt) = rt {
            session.last_activity_at = Some(rt.activity.lock().await.last);
        }
    }
    session
}

/// Session-scoped harness log (shown in the UI next to agent events).
pub struct SessionLogger {
    app: Shared,
    id: String,
}

impl SessionLogger {
    pub async fn info(&self, message: impl Into<String>) {
        self.app.session_log(&self.id, "info", message.into()).await
    }
    pub async fn error(&self, message: impl Into<String>) {
        self.app.session_log(&self.id, "error", message.into()).await
    }
}

impl App {
    pub fn sessions_file(&self) -> PathBuf {
        self.cfg.data_dir.join("sessions.json")
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.cfg.data_dir.join("sessions").join(id)
    }

    pub async fn session(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.iter().find(|s| s.id == id).cloned()
    }

    pub async fn update_session<R>(&self, id: &str, f: impl FnOnce(&mut Session) -> R) -> Option<(Session, R)> {
        let (session, result) = {
            let mut sessions = self.sessions.write().await;
            let session = sessions.iter_mut().find(|s| s.id == id)?;
            let result = f(session);
            session.updated_at = Utc::now();
            (session.clone(), result)
        };
        if let Err(e) = self.persist_sessions().await {
            // The change in memory is real, and the broadcast below tells the truth about it —
            // hiding it would make the UI more wrong, not less. But the saved list now lags, so
            // the gap is recorded loudly, in the app alert and in the colony's own log.
            self.storage_failed("save the session list", &e).await;
            self.session_log(id, "error", format!("could not save the session list: {e:#}")).await;
        }
        let rt = self.runtimes.lock().await.get(id).cloned();
        if let Some(rt) = rt {
            let view = with_activity(self, session.clone()).await;
            rt.broadcast(None, json!({"type": "session", "session": view}).to_string());
        }
        Some((session, result))
    }

    async fn persist_sessions(&self) -> Result<()> {
        let _guard = self.session_persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.sessions.read().await).context("could not serialize the session list")?;
        write_atomic(&self.sessions_file(), &data).await
    }

    pub async fn runtime(&self, id: &str) -> Arc<Runtime> {
        let mut runtimes = self.runtimes.lock().await;
        runtimes.entry(id.to_string()).or_insert_with(|| Arc::new(Runtime::load(&self.session_dir(id)))).clone()
    }

    pub async fn session_log(&self, id: &str, level: &str, message: String) {
        let entry = json!({"type": "harness_log", "level": level, "message": message, "ts": Utc::now()});
        let rt = self.runtime(id).await;
        let persisted = {
            let _guard = rt.file_lock.lock().await;
            append_line(&rt.logs_path, &entry.to_string()).await.err()
        };
        if let Some(e) = persisted {
            // Recorded here, not by calling session_log again — that would recurse — and outside
            // the guard, as in handle_agent_event. The frame still reaches every open browser
            // below; the console and the app alert keep the gap.
            eprintln!("sessions: could not append to {}: {e:#}", rt.logs_path.display());
            self.storage_failed("append to the harness log", &e).await;
        }
        let mut logs = rt.logs.lock().await;
        logs.push_back(entry.clone());
        while logs.len() > MAX_LOGS {
            logs.pop_front();
        }
        rt.broadcast(None, entry.to_string());
    }

    fn logger(self: &Arc<Self>, id: &str) -> SessionLogger {
        SessionLogger { app: self.clone(), id: id.to_string() }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewSession {
    repo: String,
    #[serde(default)]
    issue: Option<u64>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    instructions: String,
    /// Omitted uses the publish module's `autopilot` setting.
    #[serde(default)]
    autopilot: Option<bool>,
}

/// Whether colonies may file validated findings as issues. On unless switched off in Settings.
fn findings_enabled(app: &App, modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, "file_findings").and_then(Value::as_bool).unwrap_or(true)
}

/// The container image a colony boots: the stack preset's, unless modules.json names one.
fn colony_image(app: &App, modules: &ModulesConfig) -> String {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let preset = setting_str(&modules.sandbox, &schema, "preset");
    let settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(&preset));
    setting_str(&settings, &schema, "image")
}

fn autopilot_default(app: &App, modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, "autopilot").and_then(Value::as_bool).unwrap_or(false)
}

#[derive(Debug, PartialEq)]
enum Autopilot {
    Publish,
    Wait(&'static str),
    /// Flags the colony for the maintainer.
    Hold(&'static str),
}

/// What autopilot does when a turn ends; writing `pr.md` during the turn is the agent's signal that it's done.
fn autopilot_step(errored: bool, interrupted: bool, open_question: bool, pr_written: bool) -> Autopilot {
    if open_question {
        Autopilot::Wait("a question is open")
    } else if interrupted {
        Autopilot::Wait("the turn was interrupted")
    } else if errored {
        Autopilot::Hold("the agent's turn ended with an error")
    } else if !pr_written {
        Autopilot::Wait("the agent didn't write or update its PR description this turn")
    } else {
        Autopilot::Publish
    }
}

/// Whether another colony can start right now. A queued colony holds no microVM, so it counts towards
/// neither the global limit nor the org's.
fn has_room(sessions: &[Session], org: &str, max_parallel: usize, org_limit: Option<u64>) -> bool {
    let busy = |s: &&Session| s.status.is_live() || s.status == SessionStatus::Publishing;
    if sessions.iter().filter(busy).count() >= max_parallel {
        return false;
    }
    match org_limit {
        Some(limit) => (sessions.iter().filter(busy).filter(|s| s.org == org).count() as u64) < limit,
        None => true,
    }
}

pub async fn create(State(app): State<Shared>, Json(req): Json<NewSession>) -> ApiResult<Session> {
    let repo = req.repo.trim().to_string();
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let modules = app.modules.read().await.clone();
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == modules.agent.provider)
        .ok_or_else(|| client_error(StatusCode::BAD_REQUEST, "the selected agent module is not installed"))?;
    if agent.needs_claude && app.claude_cred().is_none() {
        return Err(client_error(StatusCode::BAD_REQUEST, "log in with Claude in Settings first"));
    }
    if let Err(e) = app.cfg.asset("bin/colonizer-agentd") {
        return Err(client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")));
    }
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    // Past the limit a colony waits its turn rather than being refused; `run_queue` starts it later.
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    let existing = app.sessions.read().await.clone();
    let queued = !has_room(&existing, owner, max_parallel, orgs::org_max_parallel(&app.org_settings(owner)));

    let id = short_id();
    let slug = match req.issue {
        Some(number) => format!("issue-{number}-{id}"),
        None => format!("session-{id}"),
    };
    let title = match (req.title.trim(), req.issue) {
        ("", None) => "Open session".to_string(),
        (title, _) => truncate(title, 300),
    };
    let now = Utc::now();
    let session = Session {
        id: id.clone(),
        repo: repo.clone(),
        org: owner.to_string(),
        issue: req.issue,
        issue_title: title,
        instructions: truncate(req.instructions.trim(), 20_000),
        status: if queued { SessionStatus::Queued } else { SessionStatus::Starting },
        branch: format!("colonizer/{slug}"),
        base: None,
        worktree: app.cfg.data_dir.join("worktrees").join(owner).join(name).join(&slug).display().to_string(),
        git_admin_dir: None,
        sandbox: format!("colonizer-{id}"),
        mesh: None,
        local_port: None,
        agent: agent.id.clone(),
        autopilot: req.autopilot.unwrap_or_else(|| autopilot_default(&app, &modules)),
        pr_url: None,
        publish_stage: None,
        error: None,
        cost_usd: None,
        model_usage: None,
        cleaned_up: false,
        attention: None,
        last_activity_at: None,
        boot_timing: None,
        app_slot: None,
        created_at: now,
        updated_at: now,
    };
    let dir = app.session_dir(&id);
    tokio::fs::create_dir_all(dir.join("vm")).await?;
    tokio::fs::create_dir_all(dir.join("out")).await?;
    app.sessions.write().await.push(session.clone());
    if let Err(e) = app.persist_sessions().await {
        // Nothing has been reported as done yet — no boot, no log line, no reply — so the record
        // comes back out rather than leaving a colony only memory has heard of, and the caller
        // hears that the write never happened. The directories created above go with it; the
        // removal is best effort, and a failure there is reported, not swallowed.
        app.sessions.write().await.retain(|s| s.id != id);
        let e = e.context("could not save the new colony; nothing was created");
        app.storage_failed("save the session list", &e).await;
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!(
                    "sessions: could not remove the unused colony directory {}: {e}",
                    dir.display()
                )
            }
        }
        return Err(e.into());
    }
    app.runtime(&id).await;
    if queued {
        let waiting = existing.iter().filter(|s| s.status == SessionStatus::Queued).count();
        let ahead = if waiting == 0 { String::new() } else { format!(", behind {waiting} already waiting") };
        app.session_log(&id, "info", format!("queued: the parallel limit is {max_parallel}{ahead}")).await;
    } else {
        tokio::spawn(boot(app.clone(), id, false));
    }
    Ok(Json(session))
}

async fn boot(app: Shared, id: String, resume: bool) {
    if let Err(e) = boot_inner(&app, &id, resume).await {
        let message = format!("{e:#}");
        let Some(s) = app.session(&id).await else { return };
        if s.status != SessionStatus::Starting {
            return; // stopped by the user while starting; the stop handler cleaned up
        }
        app.session_log(&id, "error", format!("session failed to start: {message}")).await;
        teardown_vm(&app, &s).await;
        app.update_session(&id, |s| {
            s.status = SessionStatus::Failed;
            s.error = Some(truncate(&message, 2000));
        })
        .await;
    }
}

async fn ensure_starting(app: &App, id: &str) -> Result<Session> {
    match app.session(id).await {
        Some(s) if s.status == SessionStatus::Starting => Ok(s),
        _ => bail!("session was stopped while starting"),
    }
}

async fn boot_inner(app: &Shared, id: &str, resume: bool) -> Result<()> {
    let log = app.logger(id);
    // Phases close in order and partition the boot; see crates/colonizer/src/timing.rs.
    let mut timing = crate::timing::Phases::new();
    let s = ensure_starting(app, id).await?;
    let modules = app.modules.read().await.clone();
    let agent = app.agents.iter().find(|a| a.id == s.agent).cloned().context("agent module is not installed")?;

    let issue = match s.issue {
        Some(number) => {
            log.info(format!("fetching issue {}#{number}", s.repo)).await;
            match github::fetch_issue(app, &s.repo, number).await {
                Ok(issue) => Some(issue),
                Err(e) => return Err(github::access_error(app, &s.repo, e).await),
            }
        }
        None => None,
    };
    // A resumed colony keeps the base it started from; its branch already exists on top of it.
    let base = match s.base.clone().filter(|_| resume) {
        Some(base) => base,
        None => match github::default_branch(app, &s.repo).await {
            Ok(base) => base,
            Err(e) => return Err(github::access_error(app, &s.repo, e).await),
        },
    };
    let title = issue.as_ref().and_then(|i| i["title"].as_str()).map(String::from);
    app.update_session(id, |x| {
        if let Some(title) = title {
            x.issue_title = title;
        }
        x.base = Some(base.clone());
    })
    .await;
    ensure_starting(app, id).await?;

    timing.mark("issue");

    let bare = app.bare_repo(&s.repo);
    let wt = PathBuf::from(&s.worktree);
    let admin = if resume {
        // The worktree and branch outlive the microVM, so a resumed colony picks them up as they are.
        log.info(format!("resuming on the kept worktree, branch {}", s.branch)).await;
        PathBuf::from(s.git_admin_dir.as_deref().context("this colony has no worktree to resume")?)
    } else {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::sync_repo(app, &s.repo, &bare, &log).await?;
        log.info(format!("creating worktree on branch {} from origin/{base}", s.branch)).await;
        github::create_worktree(app, &bare, &wt, &s.branch, &base).await?
    };
    app.update_session(id, |x| x.git_admin_dir = Some(admin.display().to_string())).await;
    let s = ensure_starting(app, id).await?;

    timing.mark("git");

    let dir = app.session_dir(id);
    let vm_dir = dir.join("vm");
    let out_dir = dir.join("out");
    let prompt = github::build_prompt(&s, issue.as_ref(), &base, resume);
    write_private(&vm_dir.join("token"), random_token().as_bytes())?;
    let org_settings = app.org_settings(&s.org);
    let mut runner_env = agent_env(&agent, &orgs::effective_agent(&modules, &org_settings));
    let gateway_token = random_token();
    write_private(&app.gateway_token_file(id), gateway_token.as_bytes())?;
    let routing = providers::colony_routes(app, &gateway_token);
    if !routing.routes.is_empty() {
        runner_env.insert("COLONIZER_MODEL_ROUTES".into(), Value::String(serde_json::to_string(&routing.routes)?));
    }
    let used = routing.used(&runner_env);
    let probes = futures_util::future::join_all(used.iter().map(|p| crate::gateway::probe(app, p))).await;
    for (provider, health) in used.iter().zip(probes) {
        if health["reachable"] != true {
            let then = match &provider.fallback_model {
                Some(model) => format!("its requests will fall back to {model}"),
                None => "its requests will fail until it is back (set a fallback model to use Claude instead)".into(),
            };
            let error = health["error"].as_str().unwrap_or("unknown error");
            app.session_log(id, "warn", format!("model provider {} is unreachable ({error}); {then}", provider.id)).await;
        }
    }
    timing.mark("providers");

    if findings_enabled(app, &modules) {
        runner_env.insert("COLONIZER_FINDINGS".into(), Value::String("true".into()));
    }
    let memory_on = orgs::effective_memory_enabled(&modules, &org_settings);
    if memory_on {
        runner_env.insert("COLONIZER_MEMORY_DIR".into(), Value::String("/colonizer/memory".into()));
    }
    // What the colony can and cannot run is part of the agent's brief (runner.mjs).
    runner_env.insert("COLONIZER_IMAGE".into(), Value::String(colony_image(app, &modules)));

    // The private mesh needs the vendored tailscale, which has no macOS build yet. Without it a colony
    // is reached on a loopback port rather than failing to boot.
    let mesh_on = modules.mesh_enabled()
        && app.cfg.assets.as_deref().is_some_and(crate::mesh::binaries_present);
    if modules.mesh_enabled() && !mesh_on {
        app.session_log(id, "warn", "the private mesh is unavailable on this platform; using a loopback port".into()).await;
    }
    let mut env: Vec<(String, String)> = vec![
        ("IS_SANDBOX".into(), "1".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("GIT_DIR".into(), admin.display().to_string()),
        ("GIT_WORK_TREE".into(), "/workspace".into()),
        ("GIT_INDEX_FILE".into(), "/tmp/colonizer-git-index".into()),
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "safe.directory".into()),
        ("GIT_CONFIG_VALUE_0".into(), "*".into()),
    ];
    let mut mounts = vec![
        Mount { source: wt.clone(), target: "/workspace".into(), read_only: false },
        Mount { source: bare.clone(), target: bare.display().to_string(), read_only: true },
        Mount { source: vm_dir.clone(), target: "/colonizer".into(), read_only: true },
        Mount { source: out_dir, target: "/harness/out".into(), read_only: false },
        Mount { source: app.cfg.asset("bin/colonizer-agentd")?, target: "/opt/colonizer/bin/colonizer-agentd".into(), read_only: true },
        Mount { source: agent.dir.clone(), target: "/opt/colonizer/agent".into(), read_only: true },
    ];
    // Claude Code plugin directories, mounted read-only from the mothership.
    //
    // Outside /workspace on purpose: publish runs `git add -A`, so a plugin
    // staged inside the worktree would be committed into the pull request.
    // Read-only so one colony cannot edit what the next one loads — the same
    // reason memory scopes are read-only.
    //
    // A setting names a directory, never a path: it is resolved under the
    // mothership's plugins folder, so it cannot reach an arbitrary host path.
    let plugin_names = crate::plugins::parse_list(runner_env.get("COLONIZER_PLUGIN_DIRS").and_then(Value::as_str).unwrap_or_default());
    if !plugin_names.is_empty() {
        let mut targets = Vec::new();
        for name in &plugin_names {
            // The operator's data directory first, then what shipped with the app; the same resolution the
            // skillset list in Settings shows (plugins.rs).
            let source = crate::plugins::resolve(&app.cfg, name)?;
            let target = format!("/opt/colonizer/plugins/{name}");
            mounts.push(Mount { source, target: target.clone(), read_only: true });
            targets.push(target);
        }
        // The runner only ever sees in-VM paths, never the mothership's.
        runner_env.insert("COLONIZER_PLUGIN_DIRS".into(), Value::String(targets.join(",")));
        // Belt and braces for ECC, whose hooks are dropped at staging time. Its
        // own flag is checked only after a hook process has already spawned, so
        // this is the second line of defence, not the first.
        env.push(("ECC_HOOKS_ENABLED".into(), "false".into()));
        log.info(format!("loading {} plugin director{}", targets.len(), if targets.len() == 1 { "y" } else { "ies" })).await;
    }
    if let Some(assets) = app.cfg.assets.as_ref() {
        // Remembered whether or not plugins are on: an update must not remove
        // the directory any of this colony's mounts resolved through.
        let slot = assets.display().to_string();
        app.update_session(id, |x| x.app_slot = Some(slot)).await;
    }

    // Token savings (docs/protocol.md): what a switched-on setting needs inside the colony. When the
    // install lacks it, the setting is off for this colony and the log says why: saving tokens is never
    // the reason a colony doesn't start.
    let switched_on = |env: &Map<String, Value>, key: &str| env.get(key).and_then(Value::as_str) == Some("true");
    if switched_on(&runner_env, "COLONIZER_RTK") {
        match app.cfg.asset("bin/rtk") {
            Ok(source) => mounts.push(Mount { source, target: "/opt/colonizer/bin/rtk".into(), read_only: true }),
            Err(_) => {
                runner_env.remove("COLONIZER_RTK");
                log.info("compact command output is switched on, but rtk isn't installed (scripts/install.sh builds it); running without it").await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_HEADROOM") {
        match crate::headroom::installed(app) {
            Some(source) => mounts.push(Mount { source, target: "/opt/colonizer/headroom".into(), read_only: true }),
            None => {
                runner_env.remove("COLONIZER_HEADROOM");
                log.info("Headroom is switched on, but its bundle isn't downloaded yet (Settings → Agent starts the download); running without it").await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_CAVEMAN") {
        match app.cfg.asset("vendor/caveman/SKILL.md").and_then(|_| app.cfg.asset("vendor/caveman")) {
            Ok(source) => mounts.push(Mount { source, target: "/opt/colonizer/caveman".into(), read_only: true }),
            Err(_) => {
                runner_env.remove("COLONIZER_CAVEMAN");
                log.info("terse replies are switched on, but caveman's ruleset isn't installed (scripts/install.sh stages it); running without it").await;
            }
        }
    }

    // Written only now, after the last change to `runner_env`: the plugin paths and the token-savings
    // switches above are decided here, and a session.json written earlier carried a plugin's bare name
    // instead of its in-VM path, and switches the install could not honour.
    let session_json = json!({
        "session_id": id,
        "workspace": "/workspace",
        "listen": format!("0.0.0.0:{AGENTD_PORT}"),
        "agent": {"module": agent.id, "command": agent.vm_command(), "env": runner_env},
        "initial_prompt": prompt,
    });
    std::fs::write(vm_dir.join("session.json"), serde_json::to_vec_pretty(&session_json)?)?;
    std::fs::write(vm_dir.join("boot.sh"), BOOT_SCRIPT)?;

    if memory_on && memory::uses_mem0(app).await {
        // mem0's notes are written into the session directory, which is already the colony's
        // read-only /colonizer, so there is nothing to mount and nothing of mem0's inside.
        let root = vm_dir.join("memory");
        let task = memory::task_query(&s.issue_title, issue.as_ref(), &s.instructions);
        match memory::materialize_mem0(app, &root, &s.org, &s.repo, &task).await {
            Ok(m) => {
                let order = if m.ranked { ", most relevant to this task first" } else { "" };
                app.session_log(id, "info", format!("shared memory: {} notes from mem0{order}", m.notes)).await;
            }
            Err(e) => {
                app.session_log(id, "warn", format!("shared memory from mem0 is unavailable ({e:#}); this colony starts without it")).await;
                memory::write_empty_scopes(&root, &s.org, &s.repo)?;
            }
        }
    } else if memory_on {
        for (scope, key) in [("global", String::new()), ("org", s.org.clone()), ("repo", s.repo.clone())] {
            // Mount points must exist inside the read-only /colonizer mount.
            std::fs::create_dir_all(vm_dir.join("memory").join(scope))?;
            let source = app.memory.ensure_scope(scope, &key)?;
            mounts.push(Mount { source, target: format!("/colonizer/memory/{scope}"), read_only: true });
        }
    }
    let mut secrets = Vec::new();
    if agent.needs_claude {
        let cred = app.claude_cred().context("log in with Claude in Settings first")?;
        mounts.push(Mount { source: resolve_guest_claude_bin(&app.cfg).await?, target: "/opt/claude/bin/claude".into(), read_only: true });
        secrets.push(Secret { env: cred.env.into(), value: cred.value, hosts: vec![CLAUDE_API_HOST.into()] });
    }
    let mut net_profiles = vec!["public".to_string()];
    let mut net_rules = Vec::new();
    let mut publish = None;
    if mesh_on {
        let mesh = app.mesh().await?;
        log.info("starting the private mesh").await;
        mesh.ensure_started().await?;
        mesh.delete_nodes_named(&s.sandbox).await?;
        write_private(&vm_dir.join("mesh-authkey"), mesh.mint_vm_key().await?.as_bytes())?;
        mounts.push(Mount { source: app.cfg.asset("vendor/tailscale")?, target: "/opt/colonizer/tailscale".into(), read_only: true });
        env.push(("COLONIZER_MESH_LOGIN_SERVER".into(), mesh.vm_login_server()));
        env.push(("COLONIZER_MESH_HOSTNAME".into(), s.sandbox.clone()));
        net_profiles.push("host".into());
        net_rules = mesh.direct_path_rules().await;
        app.update_session(id, |x| x.mesh = Some(MeshInfo { name: s.sandbox.clone(), ip: None })).await;
    } else {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        publish = Some((port, AGENTD_PORT));
        app.update_session(id, |x| x.local_port = Some(port)).await;
    }

    // Model providers are reached through the gateway on the mothership.
    if !routing.routes.is_empty() && !net_profiles.iter().any(|p| p == "host") {
        net_profiles.push("host".into());
    }

    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    // The chosen stack fills in image and machine size; anything set explicitly
    // in modules.json still wins. See crates/colonizer/src/presets.rs.
    let preset = setting_str(&modules.sandbox, &sandbox_schema, "preset");
    let sandbox_settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(&preset));
    let spec = BootSpec {
        name: s.sandbox.clone(),
        image: setting_str(&sandbox_settings, &sandbox_schema, "image"),
        cpus: setting_u64(&sandbox_settings, &sandbox_schema, "cpus").max(1),
        memory: setting_str(&sandbox_settings, &sandbox_schema, "memory"),
        root_disk: setting_str(&sandbox_settings, &sandbox_schema, "root_disk"),
        max_duration: setting_str(&sandbox_settings, &sandbox_schema, "max_duration"),
        workdir: "/workspace".into(),
        mounts,
        env,
        secrets,
        net_profiles,
        net_rules,
        publish,
        command: vec!["sh".into(), "/colonizer/boot.sh".into()],
    };
    timing.mark("mesh-start");

    // `msb run` pulls an uncached image itself, so this is not what makes the
    // download happen — it is what stops it being an unexplained wait. On a cold
    // image the first colony otherwise sits on a spinner for gigabytes with
    // nothing said. Pre-warm from Settings (POST /api/sandbox/pull) to keep the
    // download off the launch path entirely.
    //
    // Hoisting the pull out of `msb run` also splits it out of the vm-boot
    // timing, which #15 could not separate without an extra call on every
    // launch. The cache check is that call, and it is already paid for here.
    if !sandbox::is_cached(&app.cfg.msb, &spec.image).await {
        log.info(format!("pulling {} — this happens once per image, and can take a while", spec.image)).await;
        if let Err(e) = sandbox::pull(&app.cfg.msb, &spec.image).await {
            // Not fatal: `msb run` will try the pull again and report properly.
            log.info(format!("pre-pull of {} did not finish ({e:#}); the boot will pull it", spec.image)).await;
        }
    }
    timing.mark("image-pull");

    log.info(format!("booting microVM {} ({}, {} vCPU, {})", spec.name, spec.image, spec.cpus, spec.memory)).await;
    sandbox::boot(&app.cfg.msb, &spec).await?;
    // The pull is its own phase above, so this is the VM itself — unless the
    // pre-pull failed, in which case `msb run` pulls and this absorbs it.
    timing.mark("vm-boot");
    let s = ensure_starting(app, id).await?;

    if mesh_on {
        log.info("waiting for the microVM to join the private mesh").await;
        let node = app.mesh().await?.wait_online(&s.sandbox, Duration::from_secs(120)).await?;
        let _ = std::fs::remove_file(vm_dir.join("mesh-authkey"));
        log.info(format!("{} joined the mesh at {}", s.sandbox, node.ip)).await;
        app.update_session(id, |x| x.mesh = Some(MeshInfo { name: x.sandbox.clone(), ip: Some(node.ip.clone()) })).await;
    }

    timing.mark("mesh-join");

    let s = ensure_starting(app, id).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match agentd_http(app, &s, "GET", "/v1/health").await {
            Ok((200, _)) => break,
            _ if tokio::time::Instant::now() > deadline => bail!("the agent daemon in the microVM did not become ready"),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    log.info("agent daemon is ready").await;
    timing.mark("agentd");

    log.info(timing.summary()).await;
    let breakdown = timing.to_json();
    app.update_session(id, |x| x.boot_timing = Some(breakdown)).await;

    ensure_starting(app, id).await?;
    start_link(app, id).await;
    Ok(())
}

/// Maps agent settings to runner env vars via each schema property's `env` key.
fn agent_env(agent: &AgentModule, choice: &crate::config::ModuleChoice) -> Map<String, Value> {
    let mut env = Map::new();
    if let Some(properties) = agent.schema["properties"].as_object() {
        for (key, spec) in properties {
            let (Some(var), Some(value)) = (spec["env"].as_str(), setting(choice, &agent.schema, key)) else { continue };
            let value = match value {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            if !value.is_empty() {
                env.insert(var.to_string(), Value::String(value));
            }
        }
    }
    if agent.needs_claude {
        env.insert("COLONIZER_CLAUDE_BIN".into(), Value::String("/opt/claude/bin/claude".into()));
    }
    env
}

const BOOT_SCRIPT: &str = r#"#!/bin/sh
# Generated by colonizer. Runs as the microVM's main process.
set -u
mkdir -p /var/lib/colonizer
# Git metadata is mounted read-only; give git a private, writable index.
if [ -f "${GIT_DIR:-}/index" ]; then cp "$GIT_DIR/index" "$GIT_INDEX_FILE"; fi
export PATH="/opt/claude/bin:$PATH"
if [ -f /colonizer/mesh-authkey ]; then
  mkdir -p /var/lib/tailscale
  /opt/colonizer/tailscale/tailscaled --statedir=/var/lib/tailscale --socket=/run/tailscaled.sock \
    --no-logs-no-support >/var/lib/colonizer/tailscaled.log 2>&1 &
  i=0
  while [ ! -S /run/tailscaled.sock ] && [ "$i" -lt 80 ]; do sleep 0.25; i=$((i + 1)); done
  # --accept-dns=false keeps microsandbox's DNS gateway, which its secret injection relies on.
  /opt/colonizer/tailscale/tailscale --socket=/run/tailscaled.sock up \
    --login-server="$COLONIZER_MESH_LOGIN_SERVER" --auth-key=file:/colonizer/mesh-authkey \
    --hostname="$COLONIZER_MESH_HOSTNAME" --accept-dns=false >>/var/lib/colonizer/tailscaled.log 2>&1 \
    || echo "colonizer: joining the mesh failed" >&2
fi
exec /opt/colonizer/bin/colonizer-agentd --config /colonizer/session.json --token-file /colonizer/token --state-dir /var/lib/colonizer
"#;

async fn start_link(app: &Shared, id: &str) {
    let rt = app.runtime(id).await;
    rt.stop.send_replace(false);
    let Some(commands) = rt.commands_rx.lock().await.take() else { return };
    tokio::spawn(agent_link(app.clone(), id.to_string(), rt, commands));
}

/// Stays connected to agentd's event stream while the session is live, reconnecting with `since`.
async fn agent_link(app: Shared, id: String, rt: Arc<Runtime>, mut commands: mpsc::UnboundedReceiver<Value>) {
    let mut stop = rt.stop.subscribe();
    let mut backoff = Duration::from_secs(1);
    let mut connected_before = false;
    let mut warned = false;
    loop {
        if *stop.borrow() {
            return;
        }
        let Some(s) = app.session(&id).await else { return };
        if !s.status.is_live() {
            return;
        }
        let since = rt.last_seq.load(Ordering::SeqCst);
        match agentd_ws(&app, &s, &format!("/v1/events?since={since}")).await {
            Ok(ws) => {
                let message = if connected_before { "reconnected to the agent" } else { "connected to the agent" };
                app.session_log(&id, "info", message.into()).await;
                connected_before = true;
                warned = false;
                backoff = Duration::from_secs(1);
                if s.status == SessionStatus::Starting {
                    app.update_session(&id, |x| x.status = SessionStatus::Running).await;
                }
                let (mut sink, mut stream) = ws.split();
                loop {
                    tokio::select! {
                        frame = stream.next() => match frame {
                            Some(Ok(tungstenite::Message::Text(text))) => handle_agent_event(&app, &id, &rt, text.as_str()).await,
                            Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                            Some(Ok(_)) => {}
                        },
                        command = commands.recv() => match command {
                            Some(command) => {
                                if sink.send(tungstenite::Message::Text(command.to_string().into())).await.is_err() {
                                    break;
                                }
                            }
                            None => return,
                        },
                        _ = stop.changed() => {
                            let _ = sink.close().await;
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                if backoff >= Duration::from_secs(8) && !warned {
                    app.session_log(&id, "error", format!("can't reach the agent, retrying: {e:#}")).await;
                    warned = true;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

async fn handle_agent_event(app: &Shared, id: &str, rt: &Arc<Runtime>, line: &str) {
    let Ok(event) = serde_json::from_str::<Value>(line) else { return };
    let Some(seq) = event["seq"].as_u64() else { return };
    let persisted = {
        let _guard = rt.file_lock.lock().await;
        if seq <= rt.last_seq.load(Ordering::SeqCst) {
            return; // replayed after a reconnect
        }
        match append_line(&rt.events_path, line).await {
            Ok(()) => {
                rt.last_seq.store(seq, Ordering::SeqCst);
                None
            }
            Err(e) => Some(e),
        }
    };
    if let Some(e) = persisted {
        // The event still reaches every browser below, but the evidence on disk now has a gap, and
        // a gap in the event log must not be silent. `last_seq` stays put, so if the reconnect's
        // re-fetch of this seq arrives before anything else is appended, the append gets another
        // chance — but once a later event succeeds, `last_seq` jumps past the lost one and the gap
        // is permanent. This is a second chance, not a retry that is guaranteed to happen.
        app.storage_failed("append to the colony's event log", &e).await;
        app.session_log(id, "error", format!("could not append to {}: {e:#}", rt.events_path.display())).await;
    }
    rt.broadcast(Some(seq), line.to_string());

    // Progress for the watchdog: anything but status changes and the echo of its own nudges.
    let kind = event["type"].as_str().unwrap_or_default();
    let watchdog_echo = kind == "user_message" && event["id"].as_str().is_some_and(|i| i.starts_with("watchdog-"));
    if kind != "status" && !watchdog_echo {
        {
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now();
            activity.nudges = 0;
            activity.last_nudge = None;
        }
        if app.session(id).await.is_some_and(|s| s.attention.is_some()) {
            app.update_session(id, |x| x.attention = None).await;
        }
    }

    match event["type"].as_str() {
        Some("status") => {
            let state = event["state"].as_str().unwrap_or_default();
            let next = match state {
                "working" => SessionStatus::Running,
                "waiting_for_answer" => SessionStatus::WaitingForAnswer,
                "idle" | "error" | "exited" => SessionStatus::Idle,
                _ => return,
            };
            let error = match state {
                "error" | "exited" => Some(format!("agent {state}{}", event["detail"].as_str().map(|d| format!(": {d}")).unwrap_or_default())),
                _ => None,
            };
            if let Some(current) = app.session(id).await {
                if current.status.is_live() && (current.status != next || error.is_some()) {
                    app.update_session(id, |x| {
                        x.status = next;
                        if error.is_some() {
                            x.error = error;
                        }
                    })
                    .await;
                }
            }
        }
        Some("question") => {
            *rt.open_question.lock().await = event["question_id"].as_str().map(String::from);
            rt.activity.lock().await.question_since = Some(Utc::now());
        }
        Some("question_answered") => {
            *rt.open_question.lock().await = None;
            rt.activity.lock().await.question_since = None;
        }
        Some("memory_proposal") => memory_proposal(app, id, &event).await,
        // Spawned: filing talks to GitHub, and the colony's event stream should not wait on it.
        Some("finding") => {
            tokio::spawn(file_finding(app.clone(), id.to_string(), rt.clone(), event.clone()));
        }
        Some("turn_end") => {
            let cost = event["cost_usd"].as_f64();
            let usage = event.get("model_usage").filter(|u| u.is_object()).cloned();
            if let Some((s, ())) = app.update_session(id, |x| {
                if cost.is_some() {
                    x.cost_usd = cost;
                }
                if usage.is_some() {
                    x.model_usage = usage;
                }
            })
            .await
            {
                let mark = github::pr_description_mark(&app.session_dir(id).join("out"));
                let pr_written = {
                    let mut last = rt.pr_mark.lock().await;
                    let written = mark.is_some() && *last != mark;
                    *last = mark;
                    written
                };
                let interrupted = rt.interrupted.swap(false, Ordering::SeqCst);
                if s.autopilot && s.status.is_live() {
                    let errored = event["is_error"].as_bool().unwrap_or(false);
                    let open_question = rt.open_question.lock().await.is_some();
                    match autopilot_step(errored, interrupted, open_question, pr_written) {
                        Autopilot::Publish => {
                            app.session_log(id, "info", "autopilot: the agent finished and wrote its PR description, publishing".into()).await;
                            tokio::spawn(publish_session(app.clone(), id.to_string()));
                        }
                        Autopilot::Wait(reason) => app.session_log(id, "info", format!("autopilot: not publishing yet, {reason}")).await,
                        Autopilot::Hold(reason) => {
                            app.session_log(id, "warn", format!("autopilot: not publishing, {reason}; press Create PR when the work is ready")).await;
                            app.update_session(id, |x| {
                                x.attention = Some(json!({"reason": "autopilot_held", "since": Utc::now(), "nudges": 0}));
                            })
                            .await;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// A colony proposed a shared-memory note: queue it for review (or store it when review is off).
async fn memory_proposal(app: &Shared, id: &str, event: &Value) {
    let Some(s) = app.session(id).await else { return };
    let modules = app.modules.read().await.clone();
    if !orgs::effective_memory_enabled(&modules, &app.org_settings(&s.org)) {
        app.session_log(id, "info", "ignored a memory proposal: shared memory is off for this org".into()).await;
        return;
    }
    let scope = event["scope"].as_str().unwrap_or("repo");
    let key = match scope {
        "org" => s.org.clone(),
        "repo" => s.repo.clone(),
        _ => String::new(),
    };
    let tags: Vec<String> =
        event["tags"].as_array().map(|tags| tags.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    let source = json!({"session_id": s.id, "repo": s.repo});
    let title = event["title"].as_str().unwrap_or_default();
    let content = event["content"].as_str().unwrap_or_default();
    let note = match memory::draft(scope, &key, title, content, &tags, source) {
        Ok(note) => note,
        Err(e) => {
            app.session_log(id, "error", format!("rejected a memory proposal: {e:#}")).await;
            return;
        }
    };
    let title = note.title.clone();
    let stored = if orgs::memory_requires_review(&modules) {
        app.memory.add_proposal(note).await.map(|proposal| json!(proposal))
    } else {
        match memory::store_note(app, note.clone()).await {
            Ok(note) => {
                let mut value = json!(note);
                value["status"] = json!("approved");
                Ok(value)
            }
            // With review off there is no queue to fall back on, so make one: a store that is down
            // (mem0 unreachable, a rejected key) must not cost the colony its proposal.
            Err(e) => {
                app.session_log(id, "warn", format!("could not store the note ({e:#}); queued it for review instead")).await;
                app.memory.add_proposal(note).await.map(|proposal| json!(proposal))
            }
        }
    };
    match stored {
        Ok(proposal) => {
            let waiting = proposal["status"] == "pending";
            let message = format!(
                "memory: the agent proposed \"{title}\" for {scope} memory{}",
                if waiting { ", waiting for your review" } else { " (review is off, so it is live)" }
            );
            app.session_log(id, "info", message).await;
            let rt = app.runtimes.lock().await.get(id).cloned();
            if let Some(rt) = rt {
                rt.broadcast(None, json!({"type": "memory_proposed", "proposal": proposal}).to_string());
            }
        }
        Err(e) => app.session_log(id, "error", format!("could not store a memory proposal: {e:#}")).await,
    }
}

/// A colony's orchestrator confirmed something outside its task: file it as an issue on the
/// colony's repository, unless findings are off, the colony has hit its cap, or an open issue
/// already has the same title. Every outcome is recorded and logged; none reaches the agent.
async fn file_finding(app: Shared, id: String, rt: Arc<Runtime>, event: Value) {
    let Some(s) = app.session(&id).await else { return };
    let modules = app.modules.read().await.clone();
    if !findings_enabled(&app, &modules) {
        app.session_log(&id, "info", "ignored a finding: filing findings is switched off in Settings".into()).await;
        return;
    }
    let finding = match findings::parse(&event) {
        Ok(finding) => finding,
        Err(e) => {
            app.session_log(&id, "warn", format!("did not file a finding: {e:#}")).await;
            return;
        }
    };
    let _serial = rt.findings_lock.lock().await;
    let dir = app.session_dir(&id);
    let record = dir.join("findings.jsonl");
    if findings::count(&record) >= findings::MAX_PER_COLONY {
        let message = format!(
            "did not file \"{}\": this colony has already filed {} findings, the most one colony may",
            finding.title,
            findings::MAX_PER_COLONY
        );
        app.session_log(&id, "warn", message).await;
        return;
    }
    let outcome = findings::file(&app, &s, &finding, &dir.join("finding-body.md")).await;
    let (level, message, entry) = match &outcome {
        Ok(findings::Filed::Issue(url)) => {
            ("info", format!("filed finding \"{}\" as {url}", finding.title), json!({"title": finding.title, "issue": url}))
        }
        Ok(findings::Filed::Duplicate(url)) => (
            "info",
            format!("did not file \"{}\": {url} is already open with that title", finding.title),
            json!({"title": finding.title, "duplicate_of": url}),
        ),
        Err(e) => ("error", format!("could not file finding \"{}\": {e:#}", finding.title), Value::Null),
    };
    // Only a filed or matched finding counts toward the cap; a GitHub error should not use one up.
    if !entry.is_null() {
        let recorded = append_line(&record, &entry.to_string()).await;
        if let Err(e) = recorded {
            // The finding was still filed on GitHub (that happened above); what failed is the
            // colony's own record of it, so say so instead of letting the gap pass silently.
            app.storage_failed("append to the colony's findings log", &e).await;
            app.session_log(&id, "error", format!("could not record the finding in {}: {e:#}", record.display())).await;
        }
    }
    app.session_log(&id, level, message).await;
}

/// Stops the agent link, asks agentd to shut the runner down, removes the VM and its mesh node.
async fn teardown_vm(app: &Shared, s: &Session) {
    if let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() {
        rt.stop.send_replace(true);
    }
    if s.mesh.as_ref().is_some_and(|m| m.ip.is_some()) || s.local_port.is_some() {
        let _ = tokio::time::timeout(Duration::from_secs(15), agentd_http(app, s, "POST", "/v1/shutdown")).await;
    }
    sandbox::remove(&app.cfg.msb, &s.sandbox).await;
    if s.mesh.is_some() {
        if let Ok(mesh) = app.mesh().await {
            let _ = mesh.delete_nodes_named(&s.sandbox).await;
        }
    }
}

/// Persists a publish checkpoint (and broadcasts it to browsers), so a retry — even after a harness
/// restart — knows how far the last attempt got without re-deriving it.
pub async fn record_publish_stage(app: &App, id: &str, stage: PublishStage) {
    app.update_session(id, |x| x.publish_stage = Some(stage)).await;
}

pub async fn publish_session(app: Shared, id: String) {
    // Checked before claiming and tearing down, so a refused push leaves the colony running.
    let Some(current) = app.session(&id).await else { return };
    if let Err(e) = github::check_publish_branch(&current.branch, current.base.as_deref().unwrap_or_default()) {
        let message = format!("{e:#}");
        app.session_log(&id, "error", format!("not publishing: {message}")).await;
        app.update_session(&id, |x| x.error = Some(message)).await;
        return;
    }
    // The claim captures whether a microVM was live, because the status it leaves behind is `publishing`.
    let Some((s, (claimed, was_live))) = app
        .update_session(&id, |x| {
            let allowed = can_publish(x.status, x.cleaned_up, x.git_admin_dir.is_some());
            // Before the mutation: `publishing` itself is not a live status.
            let was_live = allowed && x.status.is_live();
            if allowed {
                x.status = SessionStatus::Publishing;
                x.error = None;
            }
            (allowed, was_live)
        })
        .await
    else {
        return;
    };
    if !claimed {
        return;
    }
    let log = app.logger(&id);
    // A live status is not the only proof of a sandbox: `stop` marks the colony stopped before the
    // removal it starts, and that removal swallows its errors, so a `stopped`/`failed` colony can
    // still have a microVM. Wherever an agent link is still wired up for the colony, a publish
    // removes the sandbox first, exactly as a stop would have; only a colony with no runtime at all
    // — one that never got far enough to log, or one from before a harness restart — is known to
    // have nothing to remove.
    if was_live {
        log.info("publishing: stopping the agent and removing the microVM").await;
        teardown_vm(&app, &s).await;
    } else if app.runtimes.lock().await.contains_key(&id) {
        log.info("publishing: removing any microVM left behind for this colony").await;
        teardown_vm(&app, &s).await;
    } else {
        // A retry from `failed`/`no_changes` with no runtime has no microVM: the worktree and bare
        // repo on the host are all a publish needs, and claiming to have removed one would be a lie.
        log.info("publishing the kept worktree (no microVM is running)").await;
    }
    match github::publish(&app, &s, &log).await {
        Ok(github::Published::NoChanges) => {
            app.update_session(&id, |x| {
                x.status = SessionStatus::NoChanges;
                x.publish_stage = None;
            })
            .await;
        }
        Ok(github::Published::PullRequest(url)) => {
            app.update_session(&id, |x| {
                x.status = SessionStatus::PrOpened;
                x.pr_url = Some(url);
                x.publish_stage = Some(PublishStage::PrOpened);
            })
            .await;
        }
        Err(e) => {
            let message = format!("{e:#}");
            log.error(format!("publishing failed: {message}")).await;
            app.update_session(&id, |x| {
                x.status = SessionStatus::Failed;
                x.error = Some(truncate(&message, 2000));
            })
            .await;
        }
    }
}

/// Reconnects to microVMs that kept running while the harness was down.
pub async fn recover(app: &Shared) {
    let running = sandbox::running(&app.cfg.msb).await.unwrap_or_default();
    let sessions = app.sessions.read().await.clone();
    for s in sessions {
        if s.status == SessionStatus::Publishing {
            // The kill may have landed between persisting `publishing` and the teardown inside it, so a
            // microVM can still be running — and after a restart nothing would reap it: the runtimes map
            // is empty, so no later publish removes it, and `watch_sandboxes` skips non-live statuses.
            teardown_vm(app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Failed;
                x.error = Some("the harness restarted while publishing; the worktree is intact, publish again".into());
            })
            .await;
            continue;
        }
        if !s.status.is_live() {
            continue;
        }
        let reachable = s.mesh.as_ref().is_some_and(|m| m.ip.is_some()) || s.local_port.is_some();
        if running.contains(&s.sandbox) && reachable && s.status != SessionStatus::Starting {
            if s.mesh.is_some() {
                match app.mesh().await {
                    Ok(mesh) => {
                        if let Err(e) = mesh.ensure_started().await {
                            app.session_log(&s.id, "error", format!("mesh failed to start: {e:#}")).await;
                        }
                    }
                    Err(e) => app.session_log(&s.id, "error", format!("{e:#}")).await,
                }
            }
            app.session_log(&s.id, "info", "harness restarted: reconnecting to the running microVM".into()).await;
            start_link(app, &s.id).await;
        } else {
            teardown_vm(app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Stopped;
                x.error = Some("the microVM was not running when the harness restarted".into());
            })
            .await;
        }
    }
}

/// microsandbox stops a colony's microVM on its own when the sandbox's max session length runs out, and the
/// host can stop one too. Without this the colony keeps whatever status it last had — usually `idle` — and
/// looks alive in the UI while nothing can reach it. Checking once a minute turns that into a `stopped`
/// colony the maintainer can resume, since the worktree outlives the microVM.
pub async fn watch_sandboxes(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        // A failed `msb ls` says nothing about the colonies, so leave them alone until it answers again.
        let Ok(running) = sandbox::running(&app.cfg.msb).await else { continue };
        // Bound to a local first: a read guard in the `for` expression would live for the whole loop and
        // deadlock against update_session's write lock.
        let sessions = app.sessions.read().await.clone();
        for s in sessions {
            if !s.status.is_live() || s.status == SessionStatus::Starting || running.contains(&s.sandbox) {
                continue;
            }
            app.session_log(&s.id, "error", "the microVM stopped; the worktree is kept, so this colony can be resumed".into()).await;
            teardown_vm(&app, &s).await;
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Stopped;
                x.error = Some("the microVM stopped (its max session length, or the host stopped it); press Resume to continue".into());
            })
            .await;
        }
    }
}

/// How often a colony's pull request is checked, at first and at most: this spends the user's GitHub API
/// quota, so a freshly opened PR is noticed within a minute while one sitting for days costs an hour.
const PR_POLL_FIRST: Duration = Duration::from_secs(60);
const PR_POLL_MAX: Duration = Duration::from_secs(60 * 60);

/// Colonies whose pull request still needs watching. `merged` is final; a closed PR can be reopened,
/// so `closed` keeps being watched.
fn pr_watched(status: SessionStatus, has_pr: bool) -> bool {
    has_pr && matches!(status, SessionStatus::PrOpened | SessionStatus::Closed)
}

/// Whether a pull request is due for its next check.
fn pr_due(last_checked: Instant, backoff: Duration, now: Instant) -> bool {
    now.duration_since(last_checked) >= backoff
}

/// The next check interval: doubled after a check with no news, reset when the state actually changed.
/// A failed `gh` call counts as no news, so a broken checkout backs off like an untouched PR.
fn pr_backoff(current: Duration, changed: bool) -> Duration {
    if changed {
        PR_POLL_FIRST
    } else {
        (current * 2).min(PR_POLL_MAX)
    }
}

/// Per-colony poll bookkeeping, in memory only: it never reaches `sessions.json` or the browsers.
struct PrPoll {
    last_checked: Instant,
    backoff: Duration,
    /// Set while `gh` keeps failing, so the reason is logged once per streak, not once per attempt.
    failing: bool,
}

/// Watches the pull requests of `pr_opened` and `closed` colonies, so a merge or close elsewhere turns
/// the colony's badge into `merged` or `closed` instead of leaving it green forever. The colony's own
/// work is already published; this only reads.
pub async fn watch_pull_requests(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut polling: HashMap<String, PrPoll> = HashMap::new();
    loop {
        tick.tick().await;
        // Bound to a local first: a read guard in the `for` expression would live for the whole loop and
        // deadlock against update_session's write lock.
        let sessions = app.sessions.read().await.clone();
        // Forget colonies whose pull request no longer needs watching, so the map cannot grow unboundedly.
        polling.retain(|id, _| sessions.iter().any(|s| &s.id == id && pr_watched(s.status, s.pr_url.is_some())));
        for s in sessions.iter().filter(|s| pr_watched(s.status, s.pr_url.is_some())) {
            let Some(url) = s.pr_url.clone() else { continue };
            let poll = polling
                .entry(s.id.clone())
                .or_insert(PrPoll { last_checked: Instant::now(), backoff: PR_POLL_FIRST, failing: false });
            let now = Instant::now();
            if !pr_due(poll.last_checked, poll.backoff, now) {
                continue;
            }
            poll.last_checked = now;
            match github::pr_state(&app, &url).await {
                Ok(state) => {
                    poll.failing = false;
                    let target = match state {
                        github::PrState::Open => SessionStatus::PrOpened, // also picks a reopened PR back up
                        github::PrState::Merged => SessionStatus::Merged,
                        github::PrState::Closed => SessionStatus::Closed,
                    };
                    // Only a real transition is written: update_session persists and pushes to every
                    // browser even when the closure changes nothing.
                    let mut changed = false;
                    if s.status != target {
                        changed = app
                            .update_session(&s.id, |x| {
                                // Re-checked under the write lock: the colony may have been deleted or
                                // moved on while `gh` was running.
                                let apply = pr_watched(x.status, x.pr_url.is_some()) && x.status != target;
                                if apply {
                                    x.status = target;
                                }
                                apply
                            })
                            .await
                            .is_some_and(|(_, changed)| changed);
                    }
                    if changed {
                        let message = match target {
                            SessionStatus::Merged => "the pull request was merged",
                            SessionStatus::Closed => "the pull request was closed",
                            _ => "the pull request was reopened",
                        };
                        app.session_log(&s.id, "info", message.into()).await;
                    }
                    poll.backoff = pr_backoff(poll.backoff, changed);
                }
                Err(e) => {
                    // No news is no change: leave the colony's status alone and try again later.
                    if !poll.failing {
                        poll.failing = true;
                        app.session_log(&s.id, "warn", format!("checking the pull request failed ({e:#}); will retry")).await;
                    }
                    poll.backoff = pr_backoff(poll.backoff, false);
                }
            }
        }
    }
}

/// Starts queued colonies as slots free up, oldest first. A colony whose org is at its own limit doesn't
/// hold up the ones behind it.
pub async fn run_queue(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        start_queued(&app).await;
    }
}

async fn start_queued(app: &Shared) {
    let modules = app.modules.read().await.clone();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Several slots can free at once, so keep going until nothing else fits.
    loop {
        let sessions = app.sessions.read().await.clone();
        let mut waiting: Vec<&Session> = sessions.iter().filter(|s| s.status == SessionStatus::Queued).collect();
        waiting.sort_by_key(|s| s.created_at);
        let Some(next) = waiting
            .into_iter()
            .find(|s| has_room(&sessions, &s.org, max_parallel, orgs::org_max_parallel(&app.org_settings(&s.org))))
            .cloned()
        else {
            return;
        };
        // Claimed under the write lock, so two ticks can't start the same colony twice.
        let claimed = app
            .update_session(&next.id, |x| {
                let queued = x.status == SessionStatus::Queued;
                if queued {
                    x.status = SessionStatus::Starting;
                }
                queued
            })
            .await
            .is_some_and(|(_, queued)| queued);
        if !claimed {
            return;
        }
        app.session_log(&next.id, "info", "a slot came free; starting".into()).await;
        tokio::spawn(boot(app.clone(), next.id.clone(), false));
    }
}

// ---------------------------------------------------------------------------
// agentd transport
// ---------------------------------------------------------------------------

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn dial_agentd(app: &App, s: &Session) -> Result<Box<dyn Io>> {
    if let Some(ip) = s.mesh.as_ref().and_then(|m| m.ip.clone()) {
        let stream = app.mesh().await?.dial(&ip, AGENTD_PORT).await?;
        return Ok(Box::new(stream));
    }
    if let Some(port) = s.local_port {
        return Ok(Box::new(tokio::net::TcpStream::connect(("127.0.0.1", port)).await?));
    }
    bail!("the microVM's address is not known yet")
}

fn agentd_token(app: &App, id: &str) -> Result<String> {
    read_trimmed(&app.session_dir(id).join("vm/token")).context("session token is missing")
}

async fn agentd_http(app: &App, s: &Session, method: &str, path: &str) -> Result<(u16, String)> {
    let token = agentd_token(app, &s.id)?;
    let request = async {
        let mut stream = dial_agentd(app, s).await?;
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: agentd\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes()).await?;
        // Framed on Content-Length, not on EOF. The reply is complete and says `Connection: close`,
        // but on macOS microsandbox's published-port forwarder does not pass the guest's FIN along,
        // so waiting for the socket to close waits for the timeout instead.
        let mut response = Vec::new();
        let head_end = loop {
            if let Some(at) = find_headers_end(&response) {
                break at;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await? {
                0 => bail!("agentd closed the connection before sending headers"),
                n => response.extend_from_slice(&chunk[..n]),
            }
        };
        let text = String::from_utf8_lossy(&response[..head_end]).into_owned();
        let status = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).context("malformed agentd response")?;
        let length = content_length(&text);
        let mut body = response.split_off(head_end);
        match length {
            // No Content-Length: the body is whatever arrives before the peer hangs up.
            None => {
                stream.read_to_end(&mut body).await?;
            }
            Some(want) => {
                while body.len() < want {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk).await? {
                        0 => break,
                        n => body.extend_from_slice(&chunk[..n]),
                    }
                }
                body.truncate(want);
            }
        }
        anyhow::Ok((status, String::from_utf8_lossy(&body).into_owned()))
    };
    tokio::time::timeout(Duration::from_secs(10), request).await.context("agentd request timed out")?
}

/// The offset just past the blank line that ends the response headers.
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|at| at + 4)
}

/// `Content-Length` from a response head, if it declares one.
fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| line.split_once(':').filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length")))
        .and_then(|(_, value)| value.trim().parse().ok())
}

async fn agentd_ws(app: &App, s: &Session, path: &str) -> Result<WebSocketStream<Box<dyn Io>>> {
    let token = agentd_token(app, &s.id)?;
    let stream = dial_agentd(app, s).await?;
    let mut request = format!("ws://agentd{path}").into_client_request()?;
    request.headers_mut().insert("Authorization", format!("Bearer {token}").parse()?);
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::client_async(request, stream))
        .await
        .context("agentd websocket handshake timed out")??;
    Ok(ws)
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

pub async fn list(State(app): State<Shared>) -> Json<Vec<Session>> {
    let sessions = app.sessions.read().await.clone();
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions.into_iter().rev() {
        out.push(with_activity(&app, session).await);
    }
    Json(out)
}

pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let session = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(with_activity(&app, session).await))
}

pub async fn publish(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !can_publish(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(StatusCode::CONFLICT, "this session can't be published right now"));
    }
    tokio::spawn(publish_session(app.clone(), id.clone()));
    Ok(Json(s))
}

/// A colony can be resumed while its worktree is still on disk and no microVM is running for it.
fn can_resume(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    matches!(status, SessionStatus::Stopped | SessionStatus::Failed) && !cleaned_up && has_worktree
}

/// What a refused resume says: the conditions `can_resume` checks, phrased for the user.
const RESUME_CONFLICT: &str = "this colony can't be resumed: it has to be stopped and still have its worktree";

/// Whether a colony can publish: it needs its worktree on disk, no publish already in flight, and a
/// state a publish makes sense from. A failed or no-changes publish can be retried directly — the
/// worktree, the committed branch and the remote are all still there, so no new microVM is booted.
fn can_publish(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    matches!(
        status,
        SessionStatus::Running
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Idle
            | SessionStatus::Stopped
            | SessionStatus::Failed
            | SessionStatus::NoChanges
    ) && !cleaned_up
        && has_worktree
}

/// Moves a finished microVM's event log aside, so a resumed colony's `seq` numbering starts from 1
/// again. A failure here must stop the resume, not degrade it: agentd keeps its event store inside
/// the microVM, so a resumed colony numbers from 1 regardless, and with the stale log still in
/// place `Runtime::load` picks up the previous life's maximum and drops every new event until the
/// colony has out-produced it.
fn rotate_events(dir: &std::path::Path) -> std::io::Result<()> {
    let events = dir.join("events.jsonl");
    if !events.exists() {
        return Ok(());
    }
    for n in 1..1000 {
        let target = dir.join(format!("events-{n}.jsonl"));
        if !target.exists() {
            // The error goes back to the caller, which refuses the resume rather than carry on; a
            // rotation that silently failed would drop events instead of just replaying old ones.
            faults::check(&events, Op::Rename)?;
            return std::fs::rename(&events, &target);
        }
    }
    // Unreachable in practice — getting here means a colony has been resumed a thousand times
    // without one rotation being reported — but falling out silently would be a no-op that the
    // caller reads as success, and a stale log left in place drops the resumed colony's events.
    // So this is an error like any other failed rotation, and names the directory that filled up.
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "every archive slot in {} is taken (events-1.jsonl through events-999.jsonl), so {} has nowhere to go",
            dir.display(),
            events.display()
        ),
    ))
}

pub async fn resume(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(StatusCode::CONFLICT, RESUME_CONFLICT));
    }
    let previous_status = s.status;
    // Re-checked inside the write, exactly as the publish claim below it is: `failed` is both
    // resumable and publishable now, so a resume landing just after a publish claimed the colony
    // must be refused, not overwrite `publishing` with `starting` and boot an agent onto the
    // worktree while the host is committing and pushing.
    let Some((s, claimed)) = app
        .update_session(&id, |x| {
            let allowed = can_resume(x.status, x.cleaned_up, x.git_admin_dir.is_some());
            if allowed {
                x.status = SessionStatus::Starting;
                x.error = None;
                x.attention = None;
                x.mesh = None;
                x.local_port = None;
            }
            allowed
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    if !claimed {
        return Err(client_error(StatusCode::CONFLICT, RESUME_CONFLICT));
    }
    // The colony is ours: only now is the old agent link dropped and the event log rotated, so a
    // refused resume leaves both as it found them.
    let runtime = app.runtimes.lock().await.remove(&id);
    if let Some(rt) = &runtime {
        rt.stop.send_replace(true);
    }
    // Rotation must not fail silently (see `rotate_events`): with the stale log still in place the
    // resumed colony's events are dropped as already seen. A colony reads as Stopped before
    // `teardown_vm`'s shutdown POST has finished, so its link task can still be draining agentd's
    // last events and appending under the runtime's `file_lock`; hold that lock across the rename so
    // an in-flight append cannot straddle it and resurrect an `events.jsonl` holding the old life's
    // seq. Nothing under the guard may itself take `file_lock` (`session_log` does), so the failure
    // reporting stays outside it.
    let dir = app.session_dir(&id);
    let rotated = {
        let _file_lock = match runtime.as_ref() {
            Some(rt) => Some(rt.file_lock.lock().await),
            None => None,
        };
        rotate_events(&dir)
    };
    if let Err(e) = rotated {
        // The claim already moved this colony to `starting`, so unlike the pre-claim ordering there
        // is something to roll back: put the status back, or the colony is left mid-resume and
        // `can_resume` refuses the retry this error asks for.
        app.update_session(&id, |x| x.status = previous_status).await;
        let e = anyhow::Error::from(e);
        let message = format!(
            "could not move the old event log aside ({e}); the colony was not resumed — move {} aside yourself and try again",
            dir.join("events.jsonl").display()
        );
        app.storage_failed("rotate the old event log", &e).await;
        app.session_log(&id, "error", message.clone()).await;
        return Err(client_error(StatusCode::INTERNAL_SERVER_ERROR, &message));
    }
    app.session_log(&id, "info", "resuming: booting a fresh microVM on the kept worktree".into()).await;
    tokio::spawn(boot(app.clone(), id, true));
    Ok(Json(s))
}

pub async fn stop(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let Some((s, was)) = app
        .update_session(&id, |x| {
            let was = x.status;
            if was.is_live() || was == SessionStatus::Queued {
                x.status = SessionStatus::Stopped;
            }
            was
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    // A queued colony never started, so there is no microVM to remove.
    if was == SessionStatus::Queued {
        app.session_log(&id, "info", "left the queue before it started".into()).await;
        return Ok(Json(app.session(&id).await.unwrap_or(s)));
    }
    if !was.is_live() {
        return Err(client_error(StatusCode::CONFLICT, "session is not running"));
    }
    app.session_log(&id, "info", "stopping: removing the microVM (the worktree is kept)".into()).await;
    teardown_vm(&app, &s).await;
    Ok(Json(app.session(&id).await.unwrap_or(s)))
}

pub async fn cleanup(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if s.status.is_live() || s.status == SessionStatus::Publishing {
        return Err(client_error(StatusCode::CONFLICT, "stop the session first"));
    }
    {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::remove_worktree(&app, &s).await?;
    }
    let (s, ()) = app
        .update_session(&id, |x| x.cleaned_up = true)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(s))
}

/// Whether a colony in this state can be deleted. A live or publishing colony has a microVM or a push in flight.
fn deletable(status: SessionStatus) -> bool {
    !status.is_live() && status != SessionStatus::Publishing
}

/// Forgets a colony: its worktree and local branch (unless already cleaned up), its chat and harness logs, and its
/// record. A pull request it opened, and any branch it pushed, stay on GitHub. Live and publishing colonies must be
/// stopped first.
pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    // Out of the list before any file is touched, so neither the queue nor Resume can start it meanwhile.
    let (at, s) = {
        let mut sessions = app.sessions.write().await;
        let at = sessions.iter().position(|s| s.id == id).ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
        if !deletable(sessions[at].status) {
            return Err(client_error(StatusCode::CONFLICT, "stop the colony first"));
        }
        (at, sessions.remove(at))
    };
    if !s.cleaned_up {
        let removed = {
            let lock = app.repo_lock(&s.repo).await;
            let _guard = lock.lock().await;
            github::remove_worktree(&app, &s).await
        };
        if let Err(e) = removed {
            // Nothing is lost yet: put the record back where it was, so the colony can be cleaned up or retried.
            let mut sessions = app.sessions.write().await;
            let at = at.min(sessions.len());
            sessions.insert(at, s);
            return Err(e.context("could not remove the colony's worktree; nothing was deleted").into());
        }
    }
    if let Err(e) = app.persist_sessions().await {
        // The worktree may already be gone — it is removed before the list is saved — but the colony
        // goes back in the list either way: nothing reports a colony forgotten while the save that
        // forgets it did not happen, and the deletion can be retried once storage works again.
        let cleaned = s.cleaned_up;
        {
            let mut sessions = app.sessions.write().await;
            let at = at.min(sessions.len());
            sessions.insert(at, s);
        }
        let e = e.context(if cleaned {
            "could not save the session list; the colony is kept"
        } else {
            "could not save the session list; the colony is kept, but its worktree was already removed"
        });
        app.storage_failed("save the session list", &e).await;
        return Err(e.into());
    }
    app.runtimes.lock().await.remove(&id);
    let dir = app.session_dir(&id);
    let leftover = match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("{}: {e}", dir.display())),
    };
    Ok(Json(json!({"deleted": id, "leftover": leftover})))
}

#[derive(Deserialize)]
pub struct SinceQuery {
    since: Option<u64>,
}

pub async fn events_ws(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<SinceQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    Ok(ws.on_upgrade(move |socket| events_socket(app, id, rt, query.since.unwrap_or(0), socket)))
}

async fn events_socket(app: Shared, id: String, rt: Arc<Runtime>, since: u64, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let mut subscription = rt.events.subscribe();
    let text = |s: String| Message::Text(s.into());

    let Some(session) = app.session(&id).await else { return };
    let session = with_activity(&app, session).await;
    if tx.send(text(json!({"type": "session", "session": session}).to_string())).await.is_err() {
        return;
    }
    let logs: Vec<Value> = rt.logs.lock().await.iter().cloned().collect();
    for entry in logs {
        if tx.send(text(entry.to_string())).await.is_err() {
            return;
        }
    }
    let mut replayed = since;
    if let Ok(file) = tokio::fs::File::open(&rt.events_path).await {
        let mut lines = BufReader::new(file).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Some(seq) = serde_json::from_str::<Value>(&line).ok().and_then(|v| v["seq"].as_u64()) else { continue };
            if seq > since {
                if tx.send(text(line)).await.is_err() {
                    return;
                }
                replayed = replayed.max(seq);
            }
        }
    }

    loop {
        tokio::select! {
            item = subscription.recv() => match item {
                Ok(item) => {
                    if item.seq.is_some_and(|seq| seq <= replayed) {
                        continue;
                    }
                    if tx.send(text(item.json.clone())).await.is_err() {
                        return;
                    }
                }
                // Too slow to keep up: close so the client reconnects with `since`.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            message = rx.next() => match message {
                Some(Ok(Message::Text(body))) => client_command(&app, &id, &rt, body.as_str()).await,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn client_command(app: &Shared, id: &str, rt: &Arc<Runtime>, body: &str) {
    let Ok(command) = serde_json::from_str::<Value>(body) else { return };
    let Some(s) = app.session(id).await else { return };
    if !s.status.is_live() {
        return;
    }
    let forward = match command["type"].as_str() {
        Some("user_message") => {
            let text = command["text"].as_str().unwrap_or_default().trim();
            if text.is_empty() || text.len() > 100_000 {
                return;
            }
            json!({"type": "user_message", "id": format!("u-{}", short_id()), "text": text})
        }
        Some("answer") => {
            let (Some(question_id), true) = (command["question_id"].as_str(), command["answers"].is_object()) else { return };
            json!({
                "type": "answer",
                "question_id": question_id,
                "answers": command["answers"],
                "response": command.get("response").cloned().unwrap_or(Value::Null),
            })
        }
        Some("interrupt") => {
            rt.interrupted.store(true, Ordering::SeqCst);
            json!({"type": "interrupt"})
        }
        _ => return,
    };
    let _ = rt.commands.send(forward);
}

#[derive(Deserialize)]
pub struct TerminalQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

pub async fn terminal_ws(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    let s = app.session(&id).await.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let cols = query.cols.unwrap_or(80).clamp(10, 500);
    let rows = query.rows.unwrap_or(24).clamp(5, 300);
    Ok(ws.on_upgrade(move |socket| terminal_socket(app, s, cols, rows, socket)))
}

async fn terminal_socket(app: Shared, s: Session, cols: u16, rows: u16, mut socket: WebSocket) {
    let not_ready = match s.status {
        SessionStatus::Starting => Some("the colony is still starting; the terminal opens once its microVM is ready"),
        status if !status.is_live() => Some("the colony's microVM isn't running"),
        _ => None,
    };
    if let Some(message) = not_ready {
        let _ = socket.send(Message::Text(json!({"type": "error", "message": message}).to_string().into())).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    let upstream = match agentd_ws(&app, &s, &format!("/v1/pty?cols={cols}&rows={rows}")).await {
        Ok(ws) => ws,
        Err(e) => {
            let message = json!({"type": "error", "message": format!("can't open a terminal in the microVM: {e:#}")});
            let _ = socket.send(Message::Text(message.to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut down_tx, mut down_rx) = socket.split();
    loop {
        tokio::select! {
            message = down_rx.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if up_tx.send(tungstenite::Message::Binary(bytes.to_vec().into())).await.is_err() { break }
                }
                Some(Ok(Message::Text(body))) => {
                    if up_tx.send(tungstenite::Message::Text(body.as_str().into())).await.is_err() { break }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            message = up_rx.next() => match message {
                Some(Ok(tungstenite::Message::Binary(bytes))) => {
                    if down_tx.send(Message::Binary(bytes.to_vec().into())).await.is_err() { break }
                }
                Some(Ok(tungstenite::Message::Text(body))) => {
                    if down_tx.send(Message::Text(body.as_str().into())).await.is_err() { break }
                }
                Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = up_tx.close().await;
    let _ = down_tx.close().await;
}

#[cfg(test)]
mod tests {

    #[test]
    fn only_colonies_with_nothing_running_can_be_deleted() {
        use SessionStatus::*;
        for status in [Queued, Stopped, Failed, NoChanges, PrOpened, Merged, Closed] {
            assert!(deletable(status), "{status:?}");
        }
        for status in [Starting, Running, WaitingForAnswer, Idle, Publishing] {
            assert!(!deletable(status), "{status:?}");
        }
    }

    use super::*;

    #[test]
    fn autopilot_publishes_only_a_clean_turn_that_wrote_the_pr_description() {
        // (errored, interrupted, open_question, pr_written)
        assert_eq!(autopilot_step(false, false, false, true), Autopilot::Publish);
        assert!(matches!(autopilot_step(false, false, false, false), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(false, false, true, true), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(true, true, false, true), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(true, false, false, true), Autopilot::Hold(_)));
        assert!(matches!(autopilot_step(true, false, false, false), Autopilot::Hold(_)));
    }

    fn colony(org: &str, status: SessionStatus) -> Session {
        Session {
            id: String::new(),
            repo: format!("{org}/repo"),
            org: org.into(),
            issue: None,
            issue_title: String::new(),
            instructions: String::new(),
            status,
            branch: String::new(),
            base: None,
            worktree: String::new(),
            git_admin_dir: None,
            sandbox: String::new(),
            mesh: None,
            local_port: None,
            agent: String::new(),
            autopilot: false,
            pr_url: None,
            publish_stage: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            cleaned_up: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            app_slot: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn the_queue_waits_for_a_slot_and_queued_colonies_hold_none() {
        let running = vec![
            colony("acme", SessionStatus::Running),
            colony("acme", SessionStatus::Idle),
            colony("acme", SessionStatus::Publishing),
        ];
        assert!(!has_room(&running, "acme", 3, None), "publishing still holds its slot");
        assert!(has_room(&running, "acme", 4, None));

        // Queued and finished colonies are not occupying anything.
        let waiting = vec![
            colony("acme", SessionStatus::Queued),
            colony("acme", SessionStatus::Queued),
            colony("acme", SessionStatus::PrOpened),
            colony("acme", SessionStatus::Stopped),
            colony("acme", SessionStatus::Failed),
        ];
        assert!(has_room(&waiting, "acme", 1, None), "a queue of five holds no slots");

        // An org limit applies on top of the global one, and only to that org.
        let mixed = vec![colony("acme", SessionStatus::Running), colony("other", SessionStatus::Running)];
        assert!(!has_room(&mixed, "acme", 5, Some(1)), "acme is at its own limit");
        assert!(has_room(&mixed, "third", 5, Some(1)), "another org still has room");
    }

    #[test]
    fn only_a_stopped_colony_that_still_has_its_worktree_can_be_resumed() {
        assert!(can_resume(SessionStatus::Stopped, false, true));
        assert!(can_resume(SessionStatus::Failed, false, true));
        assert!(!can_resume(SessionStatus::Stopped, true, true), "cleaned up");
        assert!(!can_resume(SessionStatus::Stopped, false, false), "no worktree");
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::PrOpened,
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
        ] {
            assert!(!can_resume(status, false, true), "{status:?}");
        }
    }

    #[test]
    fn only_colonies_with_a_live_pr_are_watched_and_merged_is_never_polled() {
        for (status, watched) in [
            (SessionStatus::PrOpened, true),
            (SessionStatus::Closed, true),
            (SessionStatus::Merged, false),
            (SessionStatus::Queued, false),
            (SessionStatus::Starting, false),
            (SessionStatus::Running, false),
            (SessionStatus::Publishing, false),
            (SessionStatus::NoChanges, false),
            (SessionStatus::Stopped, false),
            (SessionStatus::Failed, false),
        ] {
            assert_eq!(pr_watched(status, true), watched, "{status:?}");
            // Without a pull request there is nothing to ask GitHub about.
            assert!(!pr_watched(status, false), "{status:?}");
        }
    }

    #[test]
    fn pull_request_checks_back_off_until_the_cap_and_reset_on_a_change() {
        assert_eq!(pr_backoff(Duration::from_secs(60), false), Duration::from_secs(120));
        assert_eq!(pr_backoff(Duration::from_secs(1920), false), Duration::from_secs(3600));
        assert_eq!(pr_backoff(Duration::from_secs(3600), false), Duration::from_secs(3600), "capped at an hour");
        assert_eq!(pr_backoff(Duration::from_secs(4000), false), Duration::from_secs(3600));
        // Real news buys a fast next check again (e.g. a closed PR reopened).
        assert_eq!(pr_backoff(Duration::from_secs(3600), true), Duration::from_secs(60));
    }

    #[test]
    fn a_pull_request_is_due_once_its_backoff_has_elapsed() {
        let checked = Instant::now();
        assert!(pr_due(checked - Duration::from_secs(60), Duration::from_secs(60), checked), "a backoff ago is due");
        assert!(!pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(59)));
        assert!(pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(60)));
        assert!(pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(3600)));
    }

    /// A publish is retried from the worktree, not from a microVM: `failed` and `no_changes` colonies
    /// (including ones stranded by the old publish) must be publishable without a resume.
    #[test]
    fn failed_and_no_changes_colonies_can_publish_again_without_a_new_microvm() {
        assert!(can_publish(SessionStatus::Failed, false, true));
        assert!(can_publish(SessionStatus::NoChanges, false, true));
    }

    #[test]
    fn only_a_colony_with_a_worktree_and_no_publish_in_flight_can_publish() {
        use SessionStatus::*;
        for status in [Running, WaitingForAnswer, Idle, Stopped, Failed, NoChanges] {
            assert!(can_publish(status, false, true), "{status:?}");
        }
        // A colony still in the queue or booting has no worktree to publish, one that is publishing is
        // already claimed, and a colony whose PR is open is done.
        for status in [Queued, Starting, Publishing, PrOpened] {
            assert!(!can_publish(status, false, true), "{status:?}");
        }
        for status in [Running, Stopped, Failed, NoChanges] {
            assert!(!can_publish(status, true, true), "cleaned up: {status:?}");
            assert!(!can_publish(status, false, false), "no worktree: {status:?}");
        }
    }

    // -- storage failures -----------------------------------------------------------------------

    use crate::tests::test_app;

    /// A throwaway App with one colony in it, over a temp directory (as in memory.rs).
    async fn app_with_colony(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = test_app(&root);
        let mut s = colony("acme", status);
        s.id = id.to_string();
        app.sessions.write().await.push(s);
        tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        (app, root)
    }

    #[test]
    fn a_failed_event_log_rotation_is_reported_not_swallowed() {
        let dir = std::env::temp_dir().join(format!("colonizer-rotate-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        let _guard = faults::inject("events.jsonl", Op::Rename, || std::io::Error::from_raw_os_error(5));
        assert!(rotate_events(&dir).is_err());
        drop(_guard);
        assert_eq!(std::fs::read_to_string(dir.join("events.jsonl")).unwrap(), "{\"seq\":1}\n", "the log is untouched");
        rotate_events(&dir).unwrap();
        assert!(!dir.join("events.jsonl").exists());
        assert_eq!(std::fs::read_to_string(dir.join("events-1.jsonl")).unwrap(), "{\"seq\":1}\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_failed_event_log_rotation_refuses_the_resume_and_leaves_the_colony_stopped() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into())).await.unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        let _guard = faults::inject("events.jsonl", Op::Rename, || std::io::Error::from_raw_os_error(5));
        let err = resume(State(app.clone()), Path("abc".to_string())).await.unwrap_err();
        let body = err.1.to_string();
        assert!(body.contains("the colony was not resumed"), "{body}");
        assert!(body.contains("aside yourself and try again"), "{body}");
        assert!(app.storage_alert.read().await.is_some(), "the failure is recorded, not swallowed");
        assert_eq!(
            app.session("abc").await.unwrap().status,
            SessionStatus::Stopped,
            "nothing about the colony changed"
        );
        assert_eq!(
            std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap(),
            "{\"seq\":7}\n",
            "the old log is left where it was"
        );
        let log = std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap();
        assert!(log.contains("could not move the old event log aside"), "{log}");
        drop(_guard);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_rotation_with_every_archive_slot_taken_is_an_error_and_leaves_the_log_in_place() {
        let dir = std::env::temp_dir().join(format!("colonizer-rotate-full-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        for n in 1..1000 {
            std::fs::write(dir.join(format!("events-{n}.jsonl")), "").unwrap();
        }
        let message = rotate_events(&dir).unwrap_err().to_string();
        assert!(message.contains("every archive slot"), "{message}");
        assert!(message.contains(&dir.display().to_string()), "the error names the directory that filled up: {message}");
        assert_eq!(
            std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the log is left in place, so the resume stays refused instead of dropping events"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_resume_does_not_rotate_while_another_task_holds_the_runtime_s_file_lock() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.git_admin_dir = Some("/tmp/wt".into())).await.unwrap();
        std::fs::write(app.session_dir("abc").join("events.jsonl"), "{\"seq\":7}\n").unwrap();
        // A runtime in the map, as a just-stopped colony still has while its link task drains.
        let rt = app.runtime("abc").await;
        // Stand in for an in-flight append: the lock another task would hold.
        let append = rt.file_lock.lock().await;
        let resumed = tokio::spawn(resume(State(app.clone()), Path("abc".to_string())));
        // Purely cooperative, so there is no timing bet: each yield lets the resume advance to the
        // lock it cannot take. With the lock held it can neither have finished nor have renamed.
        for _ in 0..64 {
            tokio::task::yield_now().await;
            assert!(!resumed.is_finished(), "the resume waits for the runtime's file_lock");
            assert!(
                app.session_dir("abc").join("events.jsonl").exists(),
                "no rotation while an append holds the lock"
            );
        }
        drop(append);
        if let Err(e) = resumed.await.unwrap() {
            panic!("the resume failed once the lock freed up: {:#}", e.1);
        }
        assert!(
            !app.session_dir("abc").join("events.jsonl").exists(),
            "the rotation went ahead once the lock freed up"
        );
        assert_eq!(std::fs::read_to_string(app.session_dir("abc").join("events-1.jsonl")).unwrap(), "{\"seq\":7}\n");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_save_is_alerted_and_the_colony_log_shows_the_gap() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let _guard = faults::inject("sessions.json", Op::Write, || std::io::Error::from(std::io::ErrorKind::StorageFull));
        let (s, ()) = app.update_session("abc", |s| s.status = SessionStatus::Idle).await.unwrap();
        assert_eq!(s.status, SessionStatus::Idle, "the in-memory change is kept and still broadcast");
        let alert = app.storage_alert.read().await.clone().unwrap();
        assert!(alert.message.contains("save the session list"), "{}", alert.message);
        let log = std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap();
        assert!(log.contains("could not save the session list"), "{log}");
        drop(_guard);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_harness_log_append_still_reaches_the_browser_and_alerts() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let rt = app.runtime("abc").await;
        let _guard = faults::inject("harness.jsonl", Op::Append, || std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        app.session_log("abc", "error", "a message".into()).await;
        assert!(app.storage_alert.read().await.is_some(), "the failure is recorded, not swallowed");
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert_eq!(last["type"], "harness_log");
            assert_eq!(last["message"], "a message", "the frame still goes out to open browsers");
        }
        drop(_guard);
        app.session_log("abc", "info", "recovered".into()).await;
        assert!(app.session_dir("abc").join("harness.jsonl").exists(), "appends work again once the fault clears");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_event_append_does_not_advance_last_seq_and_the_lost_line_stays_lost() {
        // (Injecting a fault only on the first append would promise a retry; in reality the next
        // event succeeds and seq 1 is gone for good, which is what this pins.)
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let rt = app.runtime("abc").await;
        let _guard = faults::inject("events.jsonl", Op::Append, || std::io::Error::from_raw_os_error(5));
        handle_agent_event(&app, "abc", &rt, r#"{"seq":1,"type":"status","state":"working"}"#).await;
        assert!(app.storage_alert.read().await.is_some(), "the gap in the event log is not silent");
        assert_eq!(rt.last_seq.load(Ordering::SeqCst), 0, "last_seq does not advance past a failed append");
        assert!(!app.session_dir("abc").join("events.jsonl").exists(), "the line never landed");
        assert_eq!(app.session("abc").await.unwrap().status, SessionStatus::Running, "the in-memory state still advances");
        drop(_guard);
        handle_agent_event(&app, "abc", &rt, r#"{"seq":2,"type":"status","state":"idle"}"#).await;
        assert_eq!(rt.last_seq.load(Ordering::SeqCst), 2, "the next event succeeds and jumps past the lost one");
        let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert_eq!(events, "{\"seq\":2,\"type\":\"status\",\"state\":\"idle\"}\n", "seq 1 stays lost");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_save_keeps_the_colony_listed_and_the_delete_reports_failure() {
        let (app, root) = app_with_colony("abc", SessionStatus::Stopped).await;
        app.update_session("abc", |s| s.cleaned_up = true).await.unwrap();
        let _guard = faults::inject("sessions.json", Op::Write, || std::io::Error::from(std::io::ErrorKind::StorageFull));
        let result = delete(State(app.clone()), Path("abc".to_string())).await;
        assert!(result.unwrap_err().1.to_string().contains("the colony is kept"));
        assert!(app.session("abc").await.is_some(), "the colony goes back in the list");
        assert!(app.storage_alert.read().await.is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
