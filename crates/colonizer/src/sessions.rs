//! Interactive sessions: one worktree + microVM + agent per task, bridged to browsers.
//!
//! Harness ⇄ VM traffic goes to `colonizer-agentd` over the private mesh (or a loopback port when the
//! mesh module is disabled). Agent events are persisted per session and fanned out to every open
//! browser; browser commands are forwarded to the agent.

use crate::{
    ApiResult, App, CLAUDE_API_HOST, Shared, client_error,
    config::{ModulesConfig, setting, setting_str, setting_u64},
    github, memory,
    modules::{AgentModule, schema_for},
    orgs, providers, resolve_guest_claude_bin,
    sandbox::{self, BootSpec, Mount, Secret},
    spend,
    stack::{self, Stacked},
    util::{append_line, random_token, read_trimmed, short_id, truncate, valid_repo, write_atomic, write_private},
    watchdog::Activity,
};
#[allow(unused_imports)]
use crate::{events::*, lifecycle::*, publish::*, queue::*};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::Response,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex, broadcast, mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, client::IntoClientRequest},
};

const AGENTD_PORT: u16 = 7070;
const MAX_LOGS: usize = 200;

/// Fixed failure messages the harness records verbatim. They double as the closed vocabulary
/// usage.rs buckets failures with: `Session.error` itself is free text and is never sent anywhere.
pub(crate) const AGENTD_NOT_READY: &str = "the agent daemon in the microVM did not become ready";
/// Recorded when a live colony's microVM is gone once the harness has restarted (`recover`).
pub(crate) const VM_GONE_AFTER_RESTART: &str = "the microVM was not running when the harness restarted";
/// Recorded when a live colony's microVM stops on its own (its max session length) or the host stopped it.
pub(crate) const VM_STOPPED_EARLY: &str =
    "the microVM stopped (its max session length, or the host stopped it); press Resume to continue";
/// Recorded for a colony that was mid-publish when the harness restarted.
pub(crate) const PUBLISH_LOST_TO_RESTART: &str = "the harness restarted while publishing; the worktree is intact, publish again";

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

    /// Whether this status holds a microVM slot against the parallel limit: the same "busy"
    /// predicate `queue::has_room` counts. `Publishing` keeps its slot because the colony is working
    /// on the worktree while the microVM is torn down; a queued colony holds nothing.
    pub fn busy(self) -> bool {
        self.is_live() || self == Self::Publishing
    }

    /// The states a colony is left in when its run is over — stopped or failed, or done with its
    /// pull request opened, merged, closed or nothing to push. PrOpened is included: the work is
    /// out, whatever a reviewer does next. This is the status set the spend journal's `returned`
    /// edge keys on, so [`App::update_session`] can tell the first transition into one of them.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::PrOpened | Self::Merged | Self::Closed | Self::NoChanges | Self::Stopped | Self::Failed
        )
    }

    /// The name the API serialises, for messages that name a colony's state back to a person.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForAnswer => "waiting_for_answer",
            Self::Idle => "idle",
            Self::Publishing => "publishing",
            Self::PrOpened => "pr_opened",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::NoChanges => "no_changes",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

impl Session {
    /// The colony's whole model spend: Claude's own estimate (`cost_usd`) plus everything the gateway
    /// routed and priced (`routed_cost_usd`).
    pub fn total_cost_usd(&self) -> f64 {
        self.cost_usd.unwrap_or_default() + self.routed_cost_usd.unwrap_or_default()
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

/// What linked a fix colony to the finding that spawned it: the hunter colony that filed the
/// finding, the finding's title, and the issue URL it was filed as. The review runs on this colony's
/// pull request and reports back to the hunter's ledger and event stream.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FixFor {
    pub session: String,
    pub title: String,
    pub issue: Option<String>,
}

/// A colony record, as persisted in `sessions.json`. The container-level `#[serde(default)]` is what
/// keeps a sessions.json written by an older version loadable: a field added here defaults instead of
/// making every existing file unparseable on upgrade. New fields need no annotation of their own.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub id: String,
    pub repo: String,
    /// The GitHub org (repository owner) whose workspace this colony belongs to.
    pub org: String,
    /// `None` for an open session that starts from the repository alone.
    pub issue: Option<u64>,
    pub issue_title: String,
    pub instructions: String,
    pub status: SessionStatus,
    pub branch: String,
    pub base: Option<String>,
    /// The colony this one is stacked on, when it was created with `after`: that colony's branch is
    /// this colony's starting point and the base its pull request targets. `None` for the
    /// overwhelming majority, which branch from the repository's default branch as always.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Who launched the colony when the operator did not: `Some("burn_down")` marks a colony the
    /// burn-down scheduler auto-launched, so the global stop can find it and the UI can label it.
    /// `None` for anything a person started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub worktree: String,
    pub git_admin_dir: Option<String>,
    pub sandbox: String,
    pub mesh: Option<MeshInfo>,
    pub local_port: Option<u16>,
    pub agent: String,
    pub autopilot: bool,
    /// Whether a filed finding from this colony spawns a fix colony. `None` until the operator
    /// answers, and the publish module's `autofix` setting decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<bool>,
    /// Whether a fix colony's review-passing pull request merges itself. `None` until the operator
    /// answers, and the publish module's `automerge` setting decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automerge: Option<bool>,
    /// The finding this colony was spawned to fix, and the hunter that filed it; `None` when a
    /// colony started on an issue or free, no colony is fixing anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_for: Option<FixFor>,
    pub pr_url: Option<String>,
    /// How far the last publish got; left in place when a publish failed, so a retry knows where to look.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_stage: Option<PublishStage>,
    pub error: Option<String>,
    /// What Claude Code itself reports at turn end: an estimate over the Claude models only. Routed
    /// providers report tokens but no dollars; the gateway prices those into `routed_cost_usd`, and
    /// [`Session::total_cost_usd`] is the two added up.
    pub cost_usd: Option<f64>,
    /// Tokens per model from the last turn end, cumulative: `{model: {input_tokens, output_tokens, cache_read_tokens,
    /// cache_write_tokens}}` — every model the colony used, priced or not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_usage: Option<Value>,
    /// The tier this colony was started on, when the operator named one instead of letting the rule choose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_tier: Option<String>,
    /// The routing decision this colony booted with: the tier, the tier the rule would have chosen, and
    /// the signals behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_routing: Option<Value>,
    /// Dollars the gateway recorded for responses it routed to providers (everything but Claude, whose
    /// own cost lands above). Kept on the session so spend survives a restart and reaches the UI.
    pub routed_cost_usd: Option<f64>,
    /// What the colony leaves on the host — its worktree plus its session directory — as last measured by
    /// the host-disk check, which runs only when a host-disk quota applies to the colony. Not the
    /// microVM's root disk, which is a separate limit (microsandbox's `--root-disk`).
    pub host_disk_bytes: Option<u64>,
    pub cleaned_up: bool,
    /// Set by the watchdog: `{reason, since, nudges}`.
    pub attention: Option<Value>,
    /// Last agent progress (filled from the runtime for live colonies).
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Where the last launch's time went: `{total_ms, phases: [{name, ms}]}`.
    /// Set when a colony finishes booting, and replaced on resume.
    pub boot_timing: Option<Value>,
    /// How this colony's microVM was sized at boot: vCPUs and memory exactly as `msb run` received
    /// them. microsandbox/agentd expose no guest CPU% or RSS metrics today — agentd serves only
    /// health, events, pty and shutdown — so these are the only per-colony numbers about the VM, and
    /// guest figures are omitted rather than faked (issue #205). `null` on colonies booted before
    /// this field existed.
    pub boot_cpus: Option<u64>,
    pub boot_memory: Option<String>,
    /// The app directory this colony's mounts came from. An update keeps that
    /// directory until no live colony still names it (`update::sweep_slots`).
    pub app_slot: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for Session {
    /// Every field inert: nothing live, nothing claimed, and the epoch for the timestamps, so a colony
    /// whose record lacked a field never looks freshly touched. Written by hand rather than derived
    /// because `DateTime<Utc>` has no `Default` — which is also why the per-field `#[serde(default)]`
    /// this replaces could never cover the whole record.
    fn default() -> Self {
        Self {
            id: String::new(),
            repo: String::new(),
            org: String::new(),
            issue: None,
            issue_title: String::new(),
            instructions: String::new(),
            // Stopped, not Queued: the queue starts queued colonies on its first tick, and `recover`
            // reverts live statuses; a colony of unknown state must wait for the operator instead.
            status: SessionStatus::Stopped,
            branch: String::new(),
            base: None,
            parent: None,
            origin: None,
            worktree: String::new(),
            git_admin_dir: None,
            sandbox: String::new(),
            mesh: None,
            local_port: None,
            agent: String::new(),
            autopilot: false,
            autofix: None,
            automerge: None,
            fix_for: None,
            pr_url: None,
            publish_stage: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            model_tier: None,
            model_routing: None,
            routed_cost_usd: None,
            host_disk_bytes: None,
            cleaned_up: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            app_slot: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            updated_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }
}

/// In-memory state for a session's event fan-out and agent link.
pub struct Runtime {
    pub(crate) events: broadcast::Sender<Arc<Broadcast>>,
    pub(crate) commands: mpsc::UnboundedSender<Value>,
    pub(crate) commands_rx: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    /// The last *agentd* seq persisted, and only agentd events advance it: it is the dedupe cursor
    /// the reconnect guard compares against (`events.rs` `handle_agent_event`) and the `?since=`
    /// rank agentd replays from. Host chain events (`validation.rs` `emit_chain`) live in the file's
    /// seq space but never move this cursor, so one cursor cannot push the other past an event that
    /// has not arrived yet — the shared cursor that used to do both is exactly what dropped the next
    /// real agentd event when a host chain event consumed its rank.
    pub(crate) agent_seq: AtomicU64,
    /// The highest `seq` written to `events.jsonl` so far, agentd and host chain lines together. It
    /// is the monotonic rank a reconnecting browser replays above, and the file's seq at this rank
    /// the browser uses as its own `?since=`. Agentd events whose own seq would regress it are
    /// renumbered to one past it (with their true seq kept in `a_seq`), so the file never holds two
    /// lines out of order.
    pub(crate) last_seq: AtomicU64,
    pub(crate) logs: Mutex<VecDeque<Value>>,
    /// The open question: its id, and the questions themselves, which autonomous mode needs to
    /// answer among the options the agent offered.
    pub(crate) open_question: Mutex<Option<(String, Vec<Value>)>>,
    /// `pr.md` as of the last turn end, so autopilot publishes only when a turn wrote it.
    pub(crate) pr_mark: Mutex<Option<(std::time::SystemTime, u64)>>,
    pub(crate) interrupted: std::sync::atomic::AtomicBool,
    pub(crate) stop: watch::Sender<bool>,
    pub(crate) file_lock: Mutex<()>,
    /// Serialises findings, so the per-colony cap holds when two arrive together.
    pub(crate) findings_lock: Mutex<()>,
    pub(crate) events_path: PathBuf,
    pub(crate) logs_path: PathBuf,
    pub activity: Mutex<Activity>,
    /// A read failure from `load`, already worded to name the file and the consequence that
    /// restarts, carried until the first caller that has an `App` to report it with. Leaving it
    /// silent is what issue #107 was about.
    pub(crate) load_error: Mutex<Option<String>>,
}

/// Reads a JSONL file as raw bytes, leaving UTF-8 decoding to the caller's per-line pass. A file
/// that is not there is not a failure — a colony that has never emitted an event has no
/// `events.jsonl` — but anything else is handed back for the caller to report.
fn read_jsonl(path: &std::path::Path) -> (Vec<u8>, Option<std::io::Error>) {
    match std::fs::read(path) {
        Ok(bytes) => (bytes, None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(e)),
    }
}

pub(crate) struct Broadcast {
    pub(crate) seq: Option<u64>,
    pub(crate) json: String,
}

impl Runtime {
    /// The question a colony is waiting on, if it is waiting on one.
    pub async fn open_question(&self) -> Option<(String, Vec<Value>)> {
        self.open_question.lock().await.clone()
    }

    fn load(dir: &std::path::Path) -> Self {
        let events_path = dir.join("events.jsonl");
        let logs_path = dir.join("harness.jsonl");
        let (events_bytes, events_err) = read_jsonl(&events_path);
        // Decoded one line at a time, as agentd reads its own store (colonizer-agentd/src/store.rs): a
        // final line torn inside a multi-byte character then costs that line and not the whole file. A
        // whole-file failure here would silently reset the reconnect cursor, and the colony would replay
        // and duplicate its entire transcript.
        let last_seq = events_bytes
            .split(|b| *b == b'\n')
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(std::str::from_utf8(line).ok()?).ok()?["seq"].as_u64())
            .unwrap_or(0);
        // The reconnect cursor is agentd's, not the file's: only lines the runner wrote count, each
        // at its own seq (a line `handle_agent_event` renumbered because it collided with a host
        // chain event keeps its true seq in `a_seq`). Host chain events are cut out by their type —
        // the five this build emits and the protocol reserves — so a restart mid-life asks agentd to
        // replay exactly the events it has missed, and cannot skip the ones that never landed.
        let agent_seq = events_bytes
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_str::<Value>(std::str::from_utf8(line).ok()?).ok())
            .filter(|v| {
                v.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| !crate::validation::is_host_chain_type(t))
            })
            .filter_map(|v| v.get("a_seq").and_then(Value::as_u64).or_else(|| v["seq"].as_u64()))
            .max()
            .unwrap_or(0);
        let (logs_bytes, logs_err) = read_jsonl(&logs_path);
        // The take counts parsed entries, not raw split segments: `append_line` ends every entry
        // with a newline, so a well-formed file always yields one empty trailing segment, and
        // taking segments first would keep one entry too few.
        let logs: VecDeque<Value> = logs_bytes
            .split(|b| *b == b'\n')
            .rev()
            .filter_map(|line| serde_json::from_str(std::str::from_utf8(line).ok()?).ok())
            .take(MAX_LOGS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // Each file's message names its own consequence: only a failed events.jsonl restarts the
        // reconnect cursor, only a failed harness.jsonl restarts the log ring.
        let mut read_errors = Vec::new();
        if let Some(e) = events_err {
            read_errors.push(format!(
                "could not read the saved events ({}: {e}); \
                 the reconnect cursor restarts, so events already on disk may be recorded a second time",
                events_path.display()
            ));
        }
        if let Some(e) = logs_err {
            read_errors.push(format!(
                "could not read the saved colony log ({}: {e}); the log history starts over",
                logs_path.display()
            ));
        }
        let (commands, commands_rx) = mpsc::unbounded_channel();
        Self {
            events: broadcast::channel(1024).0,
            commands,
            commands_rx: Mutex::new(Some(commands_rx)),
            agent_seq: AtomicU64::new(agent_seq),
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
            load_error: Mutex::new((!read_errors.is_empty()).then_some(read_errors.join("; "))),
        }
    }

    pub(crate) fn broadcast(&self, seq: Option<u64>, json: String) {
        let _ = self.events.send(Arc::new(Broadcast { seq, json }));
    }

    /// Queues a command for the agent (sent once the agent link is connected).
    pub fn send_command(&self, command: Value) {
        let _ = self.commands.send(command);
    }
}

/// A session as shown to browsers: live colonies carry their last agent activity.
pub(crate) async fn with_activity(app: &App, mut session: Session) -> Session {
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

    /// Every per-task model routing decision, one JSON line each. Kept in the data dir rather than a
    /// session's directory: the record has to outlive cleanup, so the rule can be judged across
    /// colonies instead of disappearing with each one.
    fn routing_file(&self) -> PathBuf {
        self.cfg.data_dir.join("routing.jsonl")
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.cfg.data_dir.join("sessions").join(id)
    }

    pub async fn session(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.iter().find(|s| s.id == id).cloned()
    }

    pub async fn update_session<R>(&self, id: &str, f: impl FnOnce(&mut Session) -> R) -> Option<(Session, R)> {
        let (session, returned, result) = {
            let mut sessions = self.sessions.write().await;
            let session = sessions.iter_mut().find(|s| s.id == id)?;
            // Told apart before the closure runs, so the journal can hear once about the crossing
            // into a terminal state — PrOpened, merged, closed, stopped or failed — and never about
            // the later updates inside it. That one hearing is the run's `returned` edge, filed
            // against the day it happened, so it survives the cleanup or delete that forgets the
            // colony itself.
            let was_terminal = session.status.is_terminal();
            let result = f(session);
            let returned = !was_terminal && session.status.is_terminal();
            session.updated_at = Utc::now();
            (session.clone(), returned, result)
        };
        if returned {
            spend::record_returned(self, &session.org).await;
        }
        self.persist_and_broadcast(&session).await;
        Some((session, result))
    }

    /// Everything `update_session` does after letting go of the write lock: persist the list to disk and
    /// tell every open browser the session changed. Claims made directly under `with_slot` (the queue, a
    /// queued resume) call this too, or the web UI would stop updating.
    pub(crate) async fn persist_and_broadcast(&self, session: &Session) {
        if let Err(e) = self.persist_sessions().await {
            // The change in memory is real, and the broadcast below tells the truth about it —
            // hiding it would make the UI more wrong, not less. But the saved list now lags, so
            // the gap is recorded loudly, in the app alert and in the colony's own log.
            self.storage_failed("save the session list", &e).await;
            self.session_log(&session.id, "error", format!("could not save the session list: {e:#}"))
                .await;
        }
        let rt = self.runtimes.lock().await.get(&session.id).cloned();
        if let Some(rt) = rt {
            let view = with_activity(self, session.clone()).await;
            rt.broadcast(None, json!({"type": "session", "session": view}).to_string());
        }
    }

    pub(crate) async fn persist_sessions(&self) -> Result<()> {
        let _guard = self.session_persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.sessions.read().await).context("could not serialize the session list")?;
        write_atomic(&self.sessions_file(), &data).await
    }

    pub async fn runtime(&self, id: &str) -> Arc<Runtime> {
        let mut runtimes = self.runtimes.lock().await;
        runtimes
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Runtime::load(&self.session_dir(id))))
            .clone()
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

    /// Reports a read failure from `Runtime::load`, once, the first time the colony's runtime is
    /// actually used. Not done inside `runtime()`: `session_log` calls back into it, and the
    /// runtimes map is locked there. The message is built per file in `load` — this only puts it
    /// on the record.
    pub(crate) async fn report_load_error(&self, id: &str, rt: &Runtime) {
        let Some(message) = rt.load_error.lock().await.take() else {
            return;
        };
        let err = anyhow::Error::msg(message.clone());
        self.storage_failed("read the colony's saved history", &err).await;
        self.session_log(id, "error", message).await;
    }

    pub(crate) fn logger(self: &Arc<Self>, id: &str) -> SessionLogger {
        SessionLogger {
            app: self.clone(),
            id: id.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewSession {
    pub repo: String,
    #[serde(default)]
    pub issue: Option<u64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub instructions: String,
    /// Omitted uses the publish module's `autopilot` setting.
    #[serde(default)]
    pub autopilot: Option<bool>,
    /// Whether a filed finding from this colony spawns a fix colony; omitted uses the publish
    /// module's `autofix` setting.
    #[serde(default)]
    pub autofix: Option<bool>,
    /// Whether a fix colony's review-passing pull request merges itself; omitted uses the publish
    /// module's `automerge` setting, which is only read when autofix is also on.
    #[serde(default)]
    pub automerge: Option<bool>,
    /// Start a colony on an issue another colony already holds. Off by default: see `issue_held_by`.
    #[serde(default)]
    pub allow_duplicate: bool,
    /// Run this colony on a named model tier — `low`, `medium` or `high` — instead of the one the
    /// routing rule picks for the task.
    #[serde(default)]
    pub model_tier: Option<String>,
    /// Stack this colony on another one: it queues until that colony has pushed its branch, then
    /// starts from that branch instead of the default one, and its pull request is a diff against it.
    #[serde(default)]
    pub after: Option<String>,
    /// Who is asking, when the operator is not: the burn-down scheduler tags its colonies
    /// `Some("burn_down")` so `POST /api/burn-down/stop` can find them again.
    #[serde(default)]
    pub origin: Option<String>,
}

/// A colony that makes a second one on the same issue a mistake rather than a retry: one still
/// live or queued, or one whose pull request is open and waiting to be read.
///
/// On 2026-09-16/17 `FindsYou-Work/app` issue #7 drew **four** colonies — two of them ten seconds
/// apart, a double submission — and issue #13 drew two. Three of the four wrote a complete,
/// working implementation of the same feature; one was merged and the rest were closed unread.
/// Nothing here refuses the retry that matters: a colony that stopped, failed, found no changes,
/// or whose pull request is merged or closed leaves the issue free.
fn issue_held_by(sessions: &[Session], repo: &str, issue: u64) -> Option<Session> {
    sessions
        .iter()
        .find(|s| {
            s.repo == repo
                && s.issue == Some(issue)
                && matches!(
                    s.status,
                    SessionStatus::Queued
                        | SessionStatus::Starting
                        | SessionStatus::Running
                        | SessionStatus::WaitingForAnswer
                        | SessionStatus::Idle
                        | SessionStatus::Publishing
                        | SessionStatus::PrOpened
                )
        })
        .cloned()
}

/// Whether colonies may file validated findings as issues. On unless switched off in Settings.
pub(crate) fn findings_enabled(app: &App, modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, "file_findings")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// The publish module's boolean setting `key`, its schema default when nothing is set.
async fn publish_bool(app: &App, key: &str) -> bool {
    let modules = app.modules.read().await.clone();
    let schema = schema_for("publish", &modules.publish.provider, &app.agents);
    setting(&modules.publish, &schema, key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether a finding that passes validation spawns a fix colony: the launch choice on the session,
/// or the publish module's `autofix` setting.
pub(crate) async fn autofix_enabled(app: &App, s: &Session) -> bool {
    match s.autofix {
        Some(on) => on,
        None => publish_bool(app, "autofix").await,
    }
}

/// Whether a fix colony's review-passing pull request merges itself: the launch choice on the
/// session, or the publish module's `automerge` setting. An explicit choice always counts — a fix
/// colony's `automerge` was written from its hunter's decision at creation, so gating it on the fix
/// colony's own autofix (which is `Some(false)`, to stop it cascading further colonies) would
/// quietly undo the hunter's opt-in. Only the module *default* is gated on autofix: a default
/// automerge while default-autofix is off is a setting nobody can have meant.
pub(crate) async fn automerge_enabled(app: &App, s: &Session) -> bool {
    match s.automerge {
        Some(on) => on,
        None => autofix_enabled(app, s).await && publish_bool(app, "automerge").await,
    }
}

/// The container image a colony boots: what the given stack's preset names, unless modules.json sets
/// an image of its own. The stack may be the configured one rather than a detected one, so it goes
/// through [`crate::presets::resolved`] — a no-op for callers holding a repository in hand, and the
/// fallback for the ones (the Setup pane, telemetry) that pass `auto` with nothing to detect from.
/// Takes the agent list rather than the app, so usage.rs can resolve the same image for its
/// changed-from-default check without an `App`.
pub(crate) fn colony_image(agents: &[AgentModule], modules: &ModulesConfig, stack: &str) -> String {
    let schema = schema_for("sandbox", &modules.sandbox.provider, agents);
    let settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(crate::presets::resolved(stack)));
    setting_str(&settings, &schema, "image")
}

/// The stack a colony boots and the line that explains the choice. An explicit preset or an org pin
/// is the operator's decision and needs no explaining, so it comes back with no message; the two
/// detection outcomes both carry one, because a wrong guess has to be diagnosable from the session
/// log alone. Pure, so the rule is testable apart from the directory read and the logging that
/// [`resolve_stack`] wraps around it — the way `queue::has_room` and `watchdog::decide` are written.
fn stack_choice(configured: &str, detected: Option<&crate::presets::Detected>) -> (String, Option<String>) {
    if configured != crate::presets::AUTO {
        return (configured.to_string(), None);
    }
    match detected {
        Some(found) => {
            let image = crate::presets::find(found.stack).map(|p| p.image).unwrap_or_default();
            (
                found.stack.to_string(),
                Some(format!("detected {} from {}, using {}", found.stack, found.marker, image)),
            )
        }
        None => (
            crate::presets::AUTO_FALLBACK.to_string(),
            Some(format!(
                "no stack marker found in the repository; using the {} stack",
                crate::presets::AUTO_FALLBACK
            )),
        ),
    }
}

/// The stack one colony boots: the configured one, or, when that is `auto`, what the repository's own
/// marker files say. The answer is always concrete — `auto` never comes back out of this — and both
/// detection branches log, because a wrong guess has to be diagnosable from the session log alone,
/// without re-running anything.
async fn resolve_stack(
    modules: &ModulesConfig,
    schema: &Value,
    org: &orgs::OrgSettings,
    worktree: &std::path::Path,
    log: &SessionLogger,
) -> String {
    let configured = orgs::effective_stack(modules, schema, org);
    // Only an `auto` install pays for the directory listing.
    let detected = (configured == crate::presets::AUTO)
        .then(|| crate::presets::detect_in(worktree))
        .flatten();
    let (stack, message) = stack_choice(&configured, detected.as_ref());
    if let Some(message) = message {
        log.info(message).await;
    }
    stack
}

/// Whether new colonies publish automatically: the publish module's `autopilot` setting. Takes the
/// agent list rather than the app, so usage.rs can report the same default.
pub(crate) fn autopilot_default(agents: &[AgentModule], modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, agents);
    setting(&modules.publish, &schema, "autopilot")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub async fn create(State(app): State<Shared>, Json(req): Json<NewSession>) -> ApiResult<Session> {
    let repo = req.repo.trim().to_string();
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    // A switched-off workspace refuses new work but nothing else: colonies it already has stay
    // listed, queueable and resumable, and its settings survive for the day it is switched back on.
    let owner = repo.split('/').next().unwrap_or_default();
    if !orgs::org_enabled(&app.org_settings(owner)) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("the {owner} workspace is switched off; turn it back on in its org settings to start a colony there"),
        ));
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
    if let Err(e) = app.cfg.linux_binary("bin/colonizer-agentd") {
        return Err(client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")));
    }
    // A tier the rule does not know would silently fall back to the rule's own choice, which is not
    // what an operator naming one asked for — refuse the launch instead.
    let model_tier = match req.model_tier.as_deref() {
        None => None,
        Some(raw) => match crate::routing::Tier::parse(raw) {
            Some(tier) => Some(tier.as_str().to_string()),
            None => {
                return Err(client_error(
                    StatusCode::BAD_REQUEST,
                    &format!("unknown model tier \"{raw}\"; use low, medium or high"),
                ));
            }
        },
    };
    // Stacking: `after` names the colony whose branch this one must build on. Whitespace is refused
    // rather than read as nothing — an operator who typed something there meant to stack. A parent
    // that can never provide a branch is refused now, with the reason; one that has not pushed yet
    // is allowed, and the colony queues for it — the queue, not this handler, waits for the branch.
    let after = match req.after.as_deref() {
        None => None,
        Some(raw) => {
            let id = raw.trim();
            if id.is_empty() {
                return Err(client_error(StatusCode::BAD_REQUEST, "`after` names no colony to stack on"));
            }
            Some(id.to_string())
        }
    };
    let (parent, wait_for_parent) = match after {
        None => (None, false),
        Some(parent_id) => {
            let source = app.session(&parent_id).await;
            match source.as_ref() {
                None => {
                    return Err(client_error(
                        StatusCode::NOT_FOUND,
                        &format!("there is no colony `{parent_id}` to stack on"),
                    ));
                }
                Some(source) => {
                    // A stacked colony builds on its parent's branch, and a branch belongs to one
                    // repository: refused here, like every other un-stackable parent, rather than
                    // let the boot die later inside `create_worktree` with a raw git error.
                    if source.repo != repo {
                        return Err(client_error(
                            StatusCode::CONFLICT,
                            &format!(
                                "colony `{parent_id}` is on {}, not {repo}: a stacked colony builds \
                                 on its parent's branch, and a branch belongs to one repository",
                                source.repo
                            ),
                        ));
                    }
                    match stack::stacked_on(&parent_id, Some(source)) {
                        Stacked::Refuse(reason) => return Err(client_error(StatusCode::CONFLICT, &reason)),
                        Stacked::Wait => (Some(parent_id), true),
                        Stacked::Ready(_) => (Some(parent_id), false),
                    }
                }
            }
        }
    };
    if let (Some(issue), false) = (req.issue, req.allow_duplicate)
        && let Some(held) = issue_held_by(&app.sessions.read().await, &repo, issue)
    {
        let where_it_is = match held.pr_url.as_deref() {
            Some(url) => format!("its pull request is open at {url}"),
            None => format!("it is {}", held.status.as_str()),
        };
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!(
                "colony {} is already on #{issue} and {where_it_is}. Starting a second one duplicates its \
                 work: read that colony first, or pass allow_duplicate to start another anyway.",
                held.id
            ),
        ));
    }
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    // Past the limit a colony waits its turn rather than being refused; `run_queue` starts it later.
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Resolved before the admission lock: `org_settings` reads the orgs file with blocking IO.
    let org_limit = orgs::org_max_parallel(&app.org_settings(owner));

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
        status: SessionStatus::Starting, // decided by admission, just before the push
        branch: format!("colonizer/{slug}"),
        base: None,
        parent: parent.clone(),
        origin: req.origin.clone(),
        worktree: app
            .cfg
            .data_dir
            .join("worktrees")
            .join(owner)
            .join(name)
            .join(&slug)
            .display()
            .to_string(),
        git_admin_dir: None,
        sandbox: format!("colonizer-{id}"),
        mesh: None,
        local_port: None,
        agent: agent.id.clone(),
        autopilot: req.autopilot.unwrap_or_else(|| autopilot_default(&app.agents, &modules)),
        autofix: req.autofix,
        automerge: req.automerge,
        fix_for: None,
        pr_url: None,
        publish_stage: None,
        error: None,
        cost_usd: None,
        model_usage: None,
        model_tier,
        model_routing: None,
        routed_cost_usd: None,
        host_disk_bytes: None,
        cleaned_up: false,
        attention: None,
        last_activity_at: None,
        boot_timing: None,
        boot_cpus: None,
        boot_memory: None,
        app_slot: None,
        created_at: now,
        updated_at: now,
    };
    let dir = app.session_dir(&id);
    tokio::fs::create_dir_all(dir.join("vm")).await?;
    tokio::fs::create_dir_all(dir.join("out")).await?;
    // The room check and the push share one write lock, so two launches colliding on the last free slot
    // cannot both take it. Counted before the push, so this colony is never waiting behind itself.
    let (session, queued, waiting) = with_slot(&app.sessions, owner, max_parallel, org_limit, |sessions, room| {
        let mut session = session;
        // A colony still waiting for its parent's branch queues even when a slot is free: booting now
        // would branch from the default branch, which is exactly what stacking exists to avoid. A
        // queued colony holds no slot, so nothing is wasted by the wait.
        session.status = if room && !wait_for_parent {
            SessionStatus::Starting
        } else {
            SessionStatus::Queued
        };
        let queued = session.status == SessionStatus::Queued;
        let waiting = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
        sessions.push(session.clone());
        (session, queued, waiting)
    })
    .await;
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
    // Heard about by the append-only spend journal now, before the colony does anything else: the
    // `launched` edge has to survive the cleanup or delete that will forget this record.
    spend::record_launched(&app, owner).await;
    if queued {
        if let (true, Some(parent_id)) = (wait_for_parent, session.parent.as_deref()) {
            app.session_log(
                &id,
                "info",
                format!("queued behind colony {parent_id}: it starts once that colony has pushed its branch"),
            )
            .await;
        } else {
            let ahead = if waiting == 0 {
                String::new()
            } else {
                format!(", behind {waiting} already waiting")
            };
            app.session_log(&id, "info", format!("queued: the parallel limit is {max_parallel}{ahead}"))
                .await;
        }
    } else {
        tokio::spawn(boot(app.clone(), id, false));
    }
    // Adoption by use: a colony started here is the operator's answer to "do you want this org?", so
    // the org counts as seen and no prompt later asks about one they are already working in. The
    // avatar from the pending sighting is recorded with it, so the org does not fall back to its
    // initial for the minutes until the next refresh re-records it.
    let pending_avatar = app.new_orgs.read().await.get(owner).cloned().flatten();
    app.mark_org_known(owner, pending_avatar.as_deref());
    Ok(Json(session))
}

pub(crate) async fn boot(app: Shared, id: String, resume: bool) {
    if let Err(e) = boot_inner(&app, &id, resume).await {
        let message = format!("{e:#}");
        let Some(s) = app.session(&id).await else { return };
        if s.status != SessionStatus::Starting {
            return; // stopped by the user while starting; the stop handler cleaned up
        }
        app.session_log(&id, "error", format!("session failed to start: {message}"))
            .await;
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

/// The repository's default branch, with access failures worded the way boots report them.
async fn default_base(app: &Shared, repo: &str) -> Result<String> {
    match github::default_branch(app, repo).await {
        Ok(base) => Ok(base),
        Err(e) => Err(github::access_error(app, repo, e).await),
    }
}

async fn boot_inner(app: &Shared, id: &str, resume: bool) -> Result<()> {
    let log = app.logger(id);
    // Phases close in order and partition the boot; see crates/colonizer/src/timing.rs.
    let mut timing = crate::timing::Phases::new();
    let s = ensure_starting(app, id).await?;
    let modules = app.modules.read().await.clone();
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == s.agent)
        .cloned()
        .context("agent module is not installed")?;

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
    // A resumed colony keeps the base it started from; its branch already exists on top of it. A
    // colony stacked on another one takes the parent's branch, resolved now — so a long wait ends on
    // a fresh answer rather than the one given at create time. The queue only starts a stacked
    // colony once the parent's branch exists, so `Wait` and `Refuse` here mean the parent moved
    // under the queue's feet: fail the boot rather than silently branch from the wrong place. The
    // decision is `stack::boot_base`, pure and tested; here is only its I/O.
    let parent = match s.parent.as_deref() {
        Some(parent_id) => app.session(parent_id).await,
        None => None,
    };
    let mut stacked_on: Option<String> = None;
    let base = match stack::boot_base(s.base.clone().filter(|_| resume), s.parent.as_deref(), parent.as_ref()) {
        stack::BootBase::Kept(base) => {
            // What a stacked colony kept is the parent's branch it started from; the prompt says so.
            if s.parent.is_some() {
                stacked_on = Some(base.clone());
            }
            base
        }
        stack::BootBase::Parent { colony, branch } => {
            log.info(format!("stacked on colony {colony}: branching from its branch {branch}"))
                .await;
            stacked_on = Some(branch.clone());
            branch
        }
        stack::BootBase::Default => default_base(app, &s.repo).await?,
        stack::BootBase::Wait { colony } => {
            bail!("the colony `{colony}` this one is stacked on has no branch to build on yet")
        }
        stack::BootBase::Refuse(reason) => bail!("{reason}"),
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
        log.info(format!("creating worktree on branch {} from origin/{base}", s.branch))
            .await;
        github::create_worktree(app, &bare, &wt, &s.branch, &base).await?
    };
    app.update_session(id, |x| x.git_admin_dir = Some(admin.display().to_string()))
        .await;
    let s = ensure_starting(app, id).await?;

    // The worktree exists by now — freshly checked out or kept from before — so a configured `auto`
    // reads the repository this colony will actually work in, and the choice is on the session log
    // before the VM boots. `org_settings` reads the orgs file with blocking IO; it moves up with the
    // resolution because the org's own pin decides what detection is even asked.
    let org_settings = app.org_settings(&s.org);
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let stack = resolve_stack(&modules, &sandbox_schema, &org_settings, &wt, &log).await;

    timing.mark("git");

    let dir = app.session_dir(id);
    let vm_dir = dir.join("vm");
    let out_dir = dir.join("out");
    // Cloned out of the lock before the awaits below: `touched_files` shells out to git per
    // sibling, and the sessions guard must not be held across that.
    let colonies = app.sessions.read().await.clone();
    let touched = github::touched_files(app, &colonies, &s).await;
    let siblings = github::siblings_of(&colonies, &s, &touched);
    let prompt = github::build_prompt(&s, issue.as_ref(), &base, resume, &siblings, stacked_on.as_deref());
    write_private(&vm_dir.join("token"), random_token().as_bytes())?;
    let agent_choice = orgs::effective_agent(&modules, &org_settings);
    let mut runner_env = agent_env(&agent, &agent_choice);
    // Per-task model routing (routing.rs): the tier comes from the issue in front of the colony
    // unless the operator named one at launch, and the tier's model replaces the module's own when
    // that tier has one. Read off the effective settings, so an org override is honoured.
    let route_settings = crate::routing::RoutingSettings {
        enabled: setting(&agent_choice, &agent.schema, "route_per_task")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        chosen: s.model_tier.as_deref().and_then(crate::routing::Tier::parse),
    };
    let task_labels: Vec<String> = issue
        .as_ref()
        .and_then(|i| i["labels"].as_array())
        .map(|ls| {
            ls.iter()
                .map(|l| l["name"].as_str().unwrap_or_default().trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut task_signals = crate::routing::signals(
        &s.issue_title,
        issue.as_ref().and_then(|i| i["body"].as_str()).unwrap_or(&s.instructions),
        &task_labels,
        // The RESOLVED stack, not the configured preset: `auto` is not a preset the harness
        // knows, so asking `find` about it would call every auto-detected repository unknown
        // and refuse it the cheapest tier — including the ones detection identified exactly.
        crate::presets::find(&stack).is_some(),
    );
    // Jev shadow mode (jev.rs): an optional external classifier's second opinion, fetched here in
    // the async boot path — never inside `routing::decide`, which stays synchronous and pure. Off by
    // default, and a silent no-op without both the setting and a `JEV_API_KEY` secret: it is recorded
    // for later comparison and never changes the tier a colony runs on.
    let jev_enabled = setting(&agent_choice, &agent.schema, "jev_shadow_mode")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    task_signals.jev = crate::jev::shadow_opinion(jev_enabled, &s.issue_title, &task_labels, &task_signals).await;
    let tier_decision = crate::routing::decide(&route_settings, &task_signals);
    let model_low = setting_str(&agent_choice, &agent.schema, "model_low");
    let model = setting_str(&agent_choice, &agent.schema, "model");
    let model_high = setting_str(&agent_choice, &agent.schema, "model_high");
    let routed_model = crate::routing::model_for(tier_decision.tier, &model_low, &model, &model_high);
    let module_model = runner_env
        .get("COLONIZER_MODEL")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !routed_model.is_empty() {
        runner_env.insert("COLONIZER_MODEL".into(), Value::String(routed_model.into()));
    }
    // The runner reads only COLONIZER_MODEL: the tier settings are for the mothership's provider
    // tally, and leaving them in would make the boot probe check providers this colony is not using.
    runner_env.remove("COLONIZER_MODEL_LOW");
    runner_env.remove("COLONIZER_MODEL_HIGH");
    let model_changed = !routed_model.is_empty() && routed_model != module_model;
    let mut message = format!("model routing: {}", tier_decision.reason);
    if model_changed {
        message.push_str(&format!("; running on {routed_model}"));
    }
    if tier_decision.misroute() {
        message.push_str(&format!("; misroute: the rule wants {}", tier_decision.rule.as_str()));
    }
    log.info(message).await;
    // A shadow opinion that disagrees with the rule is worth a low-key note for later promotion
    // analysis; it never blocks boot or looks like an error.
    if tier_decision.jev_agrees() == Some(false) {
        log.info(format!(
            "jev shadow mode: the second opinion says {} where the rule says {}",
            tier_decision.jev.as_ref().map(|jev| jev.tier.as_str()).unwrap_or("?"),
            tier_decision.rule.as_str()
        ))
        .await;
    }
    let record = json!({
        "tier": tier_decision.tier,
        "rule": tier_decision.rule,
        "source": tier_decision.source,
        "score": tier_decision.score,
        "reason": tier_decision.reason,
        "model": if model_changed { json!(routed_model) } else { Value::Null },
        "misroute": tier_decision.misroute(),
        "signals": task_signals,
        "jev": tier_decision.jev,
    });
    app.update_session(id, |x| x.model_routing = Some(record.clone())).await;
    let line = json!({
        "ts": Utc::now(),
        "session": id,
        "repo": s.repo.clone(),
        "issue": s.issue,
        "decision": record,
    })
    .to_string();
    // A lost routing record is a lost measurement, not a failed boot: say so and carry on.
    if let Err(e) = append_line(&app.routing_file(), &line).await {
        log.error(format!("could not save the routing decision: {e:#}")).await;
    }
    let gateway_token = random_token();
    write_private(&app.gateway_token_file(id), gateway_token.as_bytes())?;
    let routing = providers::colony_routes(app, &gateway_token);
    if !routing.routes.is_empty() {
        runner_env.insert(
            "COLONIZER_MODEL_ROUTES".into(),
            Value::String(serde_json::to_string(&routing.routes)?),
        );
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
            app.session_log(
                id,
                "warn",
                format!("model provider {} is unreachable ({error}); {then}", provider.id),
            )
            .await;
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
    // What the colony can and cannot run is part of the agent's brief (runner.mjs), so it names the
    // image this colony actually boots — the resolved stack's, not the configured one — or an agent
    // in a repository detected as Rust would brief itself for a Node machine.
    runner_env.insert(
        "COLONIZER_IMAGE".into(),
        Value::String(colony_image(&app.agents, &modules, &stack)),
    );

    // The private mesh needs the three vendored binaries. Without them a colony is reached on a
    // loopback port rather than failing to boot.
    let mesh_on = modules.mesh_enabled() && app.cfg.assets.as_deref().is_some_and(crate::mesh::binaries_present);
    if modules.mesh_enabled() && !mesh_on {
        app.session_log(
            id,
            "warn",
            "the private mesh is unavailable on this platform; using a loopback port".into(),
        )
        .await;
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
        Mount {
            source: wt.clone(),
            target: "/workspace".into(),
            read_only: false,
        },
        Mount {
            source: bare.clone(),
            target: bare.display().to_string(),
            read_only: true,
        },
        Mount {
            source: vm_dir.clone(),
            target: "/colonizer".into(),
            read_only: true,
        },
        Mount {
            source: out_dir,
            target: "/harness/out".into(),
            read_only: false,
        },
        Mount {
            source: app.cfg.linux_binary("bin/colonizer-agentd")?,
            target: "/opt/colonizer/bin/colonizer-agentd".into(),
            read_only: true,
        },
        Mount {
            source: agent.dir.clone(),
            target: "/opt/colonizer/agent".into(),
            read_only: true,
        },
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
    let plugin_names = crate::plugins::parse_list(
        runner_env
            .get("COLONIZER_PLUGIN_DIRS")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    if !plugin_names.is_empty() {
        let mut targets = Vec::new();
        for name in &plugin_names {
            // The operator's data directory first, then what shipped with the app; the same resolution the
            // skillset list in Settings shows (plugins.rs).
            let source = crate::plugins::resolve(&app.cfg, name)?;
            let target = format!("/opt/colonizer/plugins/{name}");
            mounts.push(Mount {
                source,
                target: target.clone(),
                read_only: true,
            });
            targets.push(target);
        }
        // The runner only ever sees in-VM paths, never the mothership's.
        runner_env.insert("COLONIZER_PLUGIN_DIRS".into(), Value::String(targets.join(",")));
        // Belt and braces for ECC, whose hooks are dropped at staging time. Its
        // own flag is checked only after a hook process has already spawned, so
        // this is the second line of defence, not the first.
        env.push(("ECC_HOOKS_ENABLED".into(), "false".into()));
        log.info(format!(
            "loading {} plugin director{}",
            targets.len(),
            if targets.len() == 1 { "y" } else { "ies" }
        ))
        .await;
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
        match app.cfg.linux_binary("bin/rtk") {
            Ok(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/bin/rtk".into(),
                read_only: true,
            }),
            Err(e) => {
                runner_env.remove("COLONIZER_RTK");
                log.info(format!(
                    "compact command output is switched on, but rtk can't be used ({e:#}); running without it"
                ))
                .await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_HEADROOM") {
        match crate::headroom::installed(app) {
            Some(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/headroom".into(),
                read_only: true,
            }),
            None => {
                runner_env.remove("COLONIZER_HEADROOM");
                log.info("Headroom is switched on, but its bundle isn't downloaded yet (Settings → Agent starts the download); running without it").await;
            }
        }
    }
    if switched_on(&runner_env, "COLONIZER_CAVEMAN") {
        match app
            .cfg
            .asset("vendor/caveman/SKILL.md")
            .and_then(|_| app.cfg.asset("vendor/caveman"))
        {
            Ok(source) => mounts.push(Mount {
                source,
                target: "/opt/colonizer/caveman".into(),
                read_only: true,
            }),
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
                app.session_log(id, "info", format!("shared memory: {} notes from mem0{order}", m.notes))
                    .await;
            }
            Err(e) => {
                app.session_log(
                    id,
                    "warn",
                    format!("shared memory from mem0 is unavailable ({e:#}); this colony starts without it"),
                )
                .await;
                memory::write_empty_scopes(&root, &s.org, &s.repo)?;
            }
        }
    } else if memory_on {
        for (scope, key) in [("global", String::new()), ("org", s.org.clone()), ("repo", s.repo.clone())] {
            // Mount points must exist inside the read-only /colonizer mount.
            std::fs::create_dir_all(vm_dir.join("memory").join(scope))?;
            let source = app.memory.ensure_scope(scope, &key)?;
            mounts.push(Mount {
                source,
                target: format!("/colonizer/memory/{scope}"),
                read_only: true,
            });
        }
    }
    let mut secrets = Vec::new();
    if agent.needs_claude {
        let cred = app.claude_cred().context("log in with Claude in Settings first")?;
        mounts.push(Mount {
            source: resolve_guest_claude_bin(app).await?,
            target: "/opt/claude/bin/claude".into(),
            read_only: true,
        });
        secrets.push(Secret {
            env: cred.env.into(),
            value: cred.value,
            hosts: vec![CLAUDE_API_HOST.into()],
        });
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
        mounts.push(Mount {
            // The guest's own build when the host's is not a Linux one (a Mac builds Mach-O for the
            // mesh it runs itself); otherwise the single vendored copy serves both sides.
            source: app
                .cfg
                .asset("vendor/tailscale-guest")
                .or_else(|_| app.cfg.asset("vendor/tailscale"))?,
            target: "/opt/colonizer/tailscale".into(),
            read_only: true,
        });
        env.push(("COLONIZER_MESH_LOGIN_SERVER".into(), mesh.vm_login_server()));
        env.push(("COLONIZER_MESH_HOSTNAME".into(), s.sandbox.clone()));
        net_profiles.push("host".into());
        net_rules = mesh.direct_path_rules().await;
        app.update_session(id, |x| {
            x.mesh = Some(MeshInfo {
                name: s.sandbox.clone(),
                ip: None,
            })
        })
        .await;
    } else {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        publish = Some((port, AGENTD_PORT));
        app.update_session(id, |x| x.local_port = Some(port)).await;
    }

    // Model providers are reached through the gateway on the mothership.
    if !routing.routes.is_empty() && !net_profiles.iter().any(|p| p == "host") {
        net_profiles.push("host".into());
    }

    // The chosen stack fills in image and machine size — detected from the
    // repository when the configured preset was `auto`, otherwise the one the
    // operator or the org pinned — and anything set explicitly in modules.json
    // still wins. See crates/colonizer/src/presets.rs.
    let sandbox_settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(&stack));
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
    // Record the machine size this launch chose before it is put to work, so the session always
    // shows the microVM `msb run` was handed even when a later phase fails and the boot never
    // completes. Spec values are the only per-colony numbers about the VM — agentd exposes no guest
    // CPU% or RSS — so they are captured here, at source.
    app.update_session(id, |x| {
        x.boot_cpus = Some(spec.cpus);
        x.boot_memory = Some(spec.memory.clone());
    })
    .await;
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
        log.info(format!(
            "pulling {} — this happens once per image, and can take a while",
            spec.image
        ))
        .await;
        if let Err(e) = sandbox::pull(&app.cfg.msb, &spec.image).await {
            // Not fatal: `msb run` will try the pull again and report properly.
            log.info(format!(
                "pre-pull of {} did not finish ({e:#}); the boot will pull it",
                spec.image
            ))
            .await;
        }
    }
    timing.mark("image-pull");

    log.info(format!(
        "booting microVM {} ({}, {} vCPU, {})",
        spec.name, spec.image, spec.cpus, spec.memory
    ))
    .await;
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
        app.update_session(id, |x| {
            x.mesh = Some(MeshInfo {
                name: x.sandbox.clone(),
                ip: Some(node.ip.clone()),
            })
        })
        .await;
    }

    timing.mark("mesh-join");

    let s = ensure_starting(app, id).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match agentd_http(app, &s, "GET", "/v1/health").await {
            Ok((200, _)) => break,
            _ if tokio::time::Instant::now() > deadline => bail!("{}", AGENTD_NOT_READY),
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
pub(crate) fn agent_env(agent: &AgentModule, choice: &crate::config::ModuleChoice) -> Map<String, Value> {
    let mut env = Map::new();
    if let Some(properties) = agent.schema["properties"].as_object() {
        for (key, spec) in properties {
            let (Some(var), Some(value)) = (spec["env"].as_str(), setting(choice, &agent.schema, key)) else {
                continue;
            };
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

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub(crate) async fn dial_agentd(app: &App, s: &Session) -> Result<Box<dyn Io>> {
    if let Some(ip) = s.mesh.as_ref().and_then(|m| m.ip.clone()) {
        let stream = app.mesh().await?.dial(&ip, AGENTD_PORT).await?;
        return Ok(Box::new(stream));
    }
    if let Some(port) = s.local_port {
        return Ok(Box::new(tokio::net::TcpStream::connect(("127.0.0.1", port)).await?));
    }
    bail!("the microVM's address is not known yet")
}

pub(crate) fn agentd_token(app: &App, id: &str) -> Result<String> {
    read_trimmed(&app.session_dir(id).join("vm/token")).context("session token is missing")
}

pub(crate) async fn agentd_http(app: &App, s: &Session, method: &str, path: &str) -> Result<(u16, String)> {
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
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .context("malformed agentd response")?;
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
    tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .context("agentd request timed out")?
}

/// The offset just past the blank line that ends the response headers.
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|at| at + 4)
}

/// `Content-Length` from a response head, if it declares one.
fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        })
        .and_then(|(_, value)| value.trim().parse().ok())
}

pub(crate) async fn agentd_ws(app: &App, s: &Session, path: &str) -> Result<WebSocketStream<Box<dyn Io>>> {
    let token = agentd_token(app, &s.id)?;
    let stream = dial_agentd(app, s).await?;
    let mut request = format!("ws://agentd{path}").into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
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
    let session = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(with_activity(&app, session).await))
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
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    Ok(ws.on_upgrade(move |socket| events_socket(app, id, rt, query.since.unwrap_or(0), socket)))
}

/// Decode one raw `events.jsonl` line for socket replay: bytes up to (not including) `\n`.
/// Returns the event's seq and the line to send. `None` means the line is unreadable
/// (not UTF-8) or carries no seq — the caller counts it and keeps going, so one bad
/// line costs itself and not the rest of the transcript.
fn replay_line(bytes: Vec<u8>) -> Option<(u64, String)> {
    let line = String::from_utf8(bytes).ok()?;
    let seq = serde_json::from_str::<Value>(&line).ok()?.get("seq")?.as_u64()?;
    Some((seq, line))
}

async fn events_socket(app: Shared, id: String, rt: Arc<Runtime>, since: u64, socket: WebSocket) {
    // Before this socket subscribes and the log ring is drained, so its alert lands in the
    // drained history once instead of arriving twice.
    app.report_load_error(&id, &rt).await;
    let (mut tx, mut rx) = socket.split();
    let mut subscription = rt.events.subscribe();
    let text = |s: String| Message::Text(s.into());

    let Some(session) = app.session(&id).await else { return };
    let session = with_activity(&app, session).await;
    if tx
        .send(text(json!({"type": "session", "session": session}).to_string()))
        .await
        .is_err()
    {
        return;
    }
    let logs: Vec<Value> = rt.logs.lock().await.iter().cloned().collect();
    for entry in logs {
        if tx.send(text(entry.to_string())).await.is_err() {
            return;
        }
    }
    let mut replayed = since;
    let mut skipped = 0usize;
    if let Ok(file) = tokio::fs::File::open(&rt.events_path).await {
        // Split on newlines, not `lines()`/`next_line()`: the latter fails the whole read on
        // the first non-UTF-8 line, silently truncating the transcript there. A torn line
        // costs itself — it is counted below — and replay continues past it.
        let mut chunks = BufReader::new(file).split(b'\n');
        while let Ok(Some(bytes)) = chunks.next_segment().await {
            if bytes.is_empty() {
                continue;
            }
            let Some((seq, line)) = replay_line(bytes) else {
                skipped += 1;
                continue;
            };
            if seq > since {
                if tx.send(text(line)).await.is_err() {
                    return;
                }
                replayed = replayed.max(seq);
            }
        }
    }
    if skipped > 0 {
        // The gap lives in events.jsonl, so it is reported via the harness log, not by
        // writing to the event log itself — that would recurse and corrupt the seq stream.
        app.session_log(
            &id,
            "warn",
            format!("skipped {skipped} unreadable event log line(s) during replay; transcript may have a gap"),
        )
        .await;
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

/// Whether a colony can still be sent a message, an answer or an interrupt.
///
/// The microVM has to be up: once a colony is publishing, stopped or finished
/// there is no agent to receive it.
fn accepts_commands(status: SessionStatus) -> bool {
    status.is_live()
}

async fn client_command(app: &Shared, id: &str, rt: &Arc<Runtime>, body: &str) {
    let Ok(command) = serde_json::from_str::<Value>(body) else {
        return;
    };
    let Some(s) = app.session(id).await else { return };
    if !accepts_commands(s.status) {
        // Dropping it silently left the browser showing an answer on its way to an
        // agent that is gone. Send the session back instead: a client whose view is
        // stale corrects itself, and its card stops offering to answer.
        let view = with_activity(app, s.clone()).await;
        rt.broadcast(None, json!({"type": "session", "session": view}).to_string());
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
            let (Some(question_id), true) = (command["question_id"].as_str(), command["answers"].is_object()) else {
                return;
            };
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
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
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
        let _ = socket
            .send(Message::Text(json!({"type": "error", "message": message}).to_string().into()))
            .await;
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
pub(crate) mod tests {
    use crate::util::faults::{self, Op};
    use tokio::sync::RwLock;

    #[test]
    fn commands_only_reach_a_colony_whose_microvm_is_up() {
        use SessionStatus::*;
        for status in [Starting, Running, WaitingForAnswer, Idle] {
            assert!(accepts_commands(status), "{status:?} should accept an answer");
        }
        // Publishing included: the microVM is already gone, and an answer sent then
        // used to vanish while the card kept spinning.
        for status in [Publishing, PrOpened, Merged, Closed, NoChanges, Stopped, Failed, Queued] {
            assert!(!accepts_commands(status), "{status:?} must not accept an answer");
        }
    }

    use super::*;

    #[test]
    fn total_cost_is_claude_plus_routed() {
        let mut s = colony("acme", SessionStatus::Running);
        assert_eq!(s.total_cost_usd(), 0.0);
        s.cost_usd = Some(1.25);
        assert_eq!(s.total_cost_usd(), 1.25);
        s.routed_cost_usd = Some(0.75);
        assert!((s.total_cost_usd() - 2.0).abs() < 1e-9, "Claude and routed spend add up");
    }

    /// sessions.json written before `routed_cost_usd` existed must still load, cost and all.
    #[test]
    fn a_session_saved_before_routed_cost_still_deserialises() {
        let saved = r#"{"id":"c","repo":"acme/repo","issue":null,"issue_title":"","status":"running","branch":"b","base":null,"worktree":"","git_admin_dir":null,"sandbox":"s","mesh":null,"agent":"a","pr_url":null,"error":null,"cost_usd":1.5,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(saved).unwrap();
        assert_eq!(s.routed_cost_usd, None);
        assert!(
            (s.total_cost_usd() - 1.5).abs() < 1e-9,
            "the old cost still counts on its own"
        );
    }

    /// So must one saved before the host-disk check existed, which measured nothing yet.
    #[test]
    fn a_session_saved_before_host_disk_was_measured_still_deserialises() {
        let saved = r#"{"id":"c","repo":"acme/repo","issue":null,"issue_title":"","status":"running","branch":"b","base":null,"worktree":"","git_admin_dir":null,"sandbox":"s","mesh":null,"agent":"a","pr_url":null,"error":null,"cost_usd":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(saved).unwrap();
        assert_eq!(s.host_disk_bytes, None);
    }

    /// So must one saved before the boot spec was recorded: the container-level `#[serde(default)]`
    /// fills the two new fields with null rather than refusing the file.
    #[test]
    fn a_session_saved_before_the_boot_spec_was_recorded_still_deserialises() {
        let saved = r#"{"id":"c","repo":"acme/repo","issue":null,"issue_title":"","status":"running","branch":"b","base":null,"worktree":"","sandbox":"s","mesh":null,"agent":"a","pr_url":null,"error":null,"cost_usd":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(saved).unwrap();
        assert_eq!(s.boot_cpus, None, "an old record has no boot spec to report");
        assert_eq!(s.boot_memory, None);
        // And the round trip back to the wire keeps them or their absence.
        let again: Session = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(again.boot_timing, None);
    }

    /// The container-level `#[serde(default)]` is the whole contract that keeps a sessions.json from
    /// an older version loadable, so it is enforced mechanically: take a fully populated record,
    /// drop one key at a time, and the rest must still load. A field added without a usable default
    /// fails here, naming itself, before it can break anyone's file on upgrade.
    #[test]
    fn every_session_field_tolerates_absence() {
        let mut full = colony("acme", SessionStatus::Running);
        full.id = "c".into();
        full.issue = Some(7);
        full.issue_title = "Fix the deploy".into();
        full.instructions = "do the thing".into();
        full.branch = "b".into();
        full.base = Some("main".into());
        full.parent = Some("source".into());
        full.worktree = "w".into();
        full.git_admin_dir = Some("git".into());
        full.sandbox = "s".into();
        full.mesh = Some(MeshInfo {
            name: "m".into(),
            ip: Some("10.0.0.1".into()),
        });
        full.local_port = Some(7070);
        full.agent = "claude".into();
        full.autopilot = true;
        full.autofix = Some(true);
        full.automerge = Some(false);
        full.fix_for = Some(FixFor {
            session: "hunter".into(),
            title: "something is broken".into(),
            issue: Some("https://github.com/acme/repo/issues/9".into()),
        });
        full.pr_url = Some("https://github.com/acme/repo/pull/1".into());
        full.publish_stage = Some(PublishStage::Pushed);
        full.error = Some("boom".into());
        full.cost_usd = Some(1.5);
        full.model_usage = Some(json!({"claude-x": {"input_tokens": 1}}));
        full.routed_cost_usd = Some(0.25);
        full.host_disk_bytes = Some(1024);
        full.cleaned_up = true;
        full.attention = Some(json!({"reason": "stalled"}));
        full.last_activity_at = Some(Utc::now());
        full.boot_timing = Some(json!({"total_ms": 5}));
        full.boot_cpus = Some(4);
        full.boot_memory = Some("8g".into());
        full.app_slot = Some("slot".into());

        let Value::Object(fields) = serde_json::to_value(&full).unwrap() else {
            panic!("a session should serialize to an object");
        };
        for key in fields.keys() {
            let mut without = fields.clone();
            without.remove(key);
            serde_json::from_value::<Session>(Value::Object(without))
                .unwrap_or_else(|e| panic!("a session without `{key}` should still load: {e}"));
        }
    }

    /// Nothing configured means nothing automatic; the publish module's defaults flow into a session
    /// that did not choose, and a module-default automerge without autofix is nothing, while an
    /// explicit automerge on a fix colony counts on its own — that explicit value IS the hunter's
    /// decision, set at the fix colony's creation.
    #[tokio::test]
    async fn autofix_and_automerge_fall_back_to_the_module_and_count_explicit_choices() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let s = app.session("abc").await.unwrap();
        assert!(!autofix_enabled(&app, &s).await);
        assert!(!automerge_enabled(&app, &s).await);

        // The publish module's defaults flow down when the session did not choose.
        {
            let mut modules = app.modules.write().await;
            modules.publish.settings.insert("autofix".into(), json!(true));
            modules.publish.settings.insert("automerge".into(), json!(true));
        }
        assert!(autofix_enabled(&app, &s).await);
        assert!(automerge_enabled(&app, &s).await);

        // Automerge only counts when autofix is enabled: with autofix off, there are no fix
        // colonies, so the module-default automerge is a setting nobody can have meant.
        {
            let mut modules = app.modules.write().await;
            modules.publish.settings.remove("autofix");
        }
        assert!(!autofix_enabled(&app, &s).await);
        assert!(!automerge_enabled(&app, &s).await);

        // A fix colony carries its automerge explicitly from its hunter, so it counts even though
        // its autofix is `Some(false)` — the flag that stops a fix colony cascading.
        let mut fix = colony("acme", SessionStatus::Idle);
        fix.autofix = Some(false);
        fix.automerge = Some(true);
        assert!(automerge_enabled(&app, &fix).await);
        let _ = std::fs::remove_dir_all(root);
    }

    pub(crate) fn colony(org: &str, status: SessionStatus) -> Session {
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
            parent: None,
            origin: None,
            worktree: String::new(),
            git_admin_dir: None,
            sandbox: String::new(),
            mesh: None,
            local_port: None,
            agent: String::new(),
            autopilot: false,
            autofix: None,
            automerge: None,
            fix_for: None,
            pr_url: None,
            publish_stage: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            model_tier: None,
            model_routing: None,
            routed_cost_usd: None,
            host_disk_bytes: None,
            cleaned_up: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            app_slot: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// The shape of `create`'s admission, shared by the concurrency tests below: check for room and push a
    /// fresh colony in one step.
    pub(crate) async fn admit_create(
        sessions: &RwLock<Vec<Session>>,
        org: &str,
        max_parallel: usize,
        org_limit: Option<u64>,
        id: String,
    ) {
        with_slot(sessions, org, max_parallel, org_limit, |sessions, room| {
            let mut s = colony(
                org,
                if room {
                    SessionStatus::Starting
                } else {
                    SessionStatus::Queued
                },
            );
            s.id = id;
            sessions.push(s);
        })
        .await
    }

    /// The shape of `resume`'s admission: re-check resumability and flip the colony under one lock.
    pub(crate) async fn admit_resume(
        sessions: &RwLock<Vec<Session>>,
        org: &str,
        max_parallel: usize,
        org_limit: Option<u64>,
        id: &str,
    ) {
        with_slot(sessions, org, max_parallel, org_limit, |sessions, room| {
            let Some(s) = sessions.iter_mut().find(|s| s.id == id) else {
                return;
            };
            if can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
                s.status = if room {
                    SessionStatus::Starting
                } else {
                    SessionStatus::Queued
                };
            }
        })
        .await
    }

    pub(crate) fn stopped_colony_with_worktree(org: &str, id: String) -> Session {
        let mut s = colony(org, SessionStatus::Stopped);
        s.id = id;
        s.git_admin_dir = Some("git".into());
        s
    }

    // -- storage failures -----------------------------------------------------------------------

    use crate::tests::test_app;

    /// A throwaway App with one colony in it, over a temp directory (as in memory.rs).
    pub(crate) async fn app_with_colony(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = test_app(&root);
        let mut s = colony("acme", status);
        s.id = id.to_string();
        app.sessions.write().await.push(s);
        tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        (app, root)
    }

    #[tokio::test]
    async fn a_failed_save_is_alerted_and_the_colony_log_shows_the_gap() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let _guard = faults::inject("sessions.json", Op::Write, || {
            std::io::Error::from(std::io::ErrorKind::StorageFull)
        });
        let (s, ()) = app.update_session("abc", |s| s.status = SessionStatus::Idle).await.unwrap();
        assert_eq!(
            s.status,
            SessionStatus::Idle,
            "the in-memory change is kept and still broadcast"
        );
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
        let _guard = faults::inject("harness.jsonl", Op::Append, || {
            std::io::Error::from(std::io::ErrorKind::PermissionDenied)
        });
        app.session_log("abc", "error", "a message".into()).await;
        assert!(
            app.storage_alert.read().await.is_some(),
            "the failure is recorded, not swallowed"
        );
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert_eq!(last["type"], "harness_log");
            assert_eq!(last["message"], "a message", "the frame still goes out to open browsers");
        }
        drop(_guard);
        app.session_log("abc", "info", "recovered".into()).await;
        assert!(
            app.session_dir("abc").join("harness.jsonl").exists(),
            "appends work again once the fault clears"
        );
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
        assert!(
            app.storage_alert.read().await.is_some(),
            "the gap in the event log is not silent"
        );
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            0,
            "last_seq does not advance past a failed append"
        );
        assert!(!app.session_dir("abc").join("events.jsonl").exists(), "the line never landed");
        assert_eq!(
            app.session("abc").await.unwrap().status,
            SessionStatus::Running,
            "the in-memory state still advances"
        );
        drop(_guard);
        handle_agent_event(&app, "abc", &rt, r#"{"seq":2,"type":"status","state":"idle"}"#).await;
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            2,
            "the next event succeeds and jumps past the lost one"
        );
        let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert_eq!(
            events, "{\"seq\":2,\"type\":\"status\",\"state\":\"idle\"}\n",
            "seq 1 stays lost"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_event_log_torn_inside_a_multibyte_character_keeps_the_last_good_seq() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let mut events = Vec::new();
        events.extend_from_slice(b"{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}\n");
        events.extend_from_slice(b"{\"seq\":2,\"type\":\"status\",\"state\":\"idle\"}\n");
        // A final line cut mid-write, inside the first byte of an em-dash — the ordinary debris of a crash.
        events.extend_from_slice(b"{\"seq\":3,\"type\":\"log\",\"message\":\"restarting \xE2");
        tokio::fs::write(app.session_dir("abc").join("events.jsonl"), &events)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            2,
            "the torn line costs itself, not the whole file"
        );
        assert!(
            app.storage_alert.read().await.is_none(),
            "a torn line is a crash's debris, not a storage emergency"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_harness_log_torn_inside_a_multibyte_character_keeps_the_good_lines() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut logs = Vec::new();
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"first\"}\n");
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"second\"}\n");
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"torn \xE2");
        tokio::fs::write(app.session_dir("abc").join("harness.jsonl"), &logs)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        let logs = rt.logs.lock().await;
        assert_eq!(logs.len(), 2, "the torn line is dropped, the good lines stay in order");
        assert_eq!(logs[0]["message"], "first");
        assert_eq!(logs[1]["message"], "second");
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_long_harness_log_keeps_exactly_the_last_max_logs_entries() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut logs = Vec::new();
        for i in 1..=250 {
            logs.extend_from_slice(
                format!("{{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"line {i}\"}}\n").as_bytes(),
            );
        }
        tokio::fs::write(app.session_dir("abc").join("harness.jsonl"), &logs)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        let kept = rt.logs.lock().await;
        assert_eq!(kept.len(), MAX_LOGS, "the ring holds exactly {MAX_LOGS} well-formed entries");
        assert_eq!(
            kept[0]["message"], "line 51",
            "the first kept entry is the first of the last {MAX_LOGS}"
        );
        assert_eq!(kept.back().unwrap()["message"], "line 250", "the newest entry is last");
        drop(kept);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreadable_event_log_alerts_on_first_use_and_admits_the_history_restarts() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        // The fault seam has no read op, but reading a directory is an error here (EISDIR), so an
        // unreadable file is staged as one — both files, to pin that both failures are carried.
        let dir = app.session_dir("abc");
        tokio::fs::create_dir(dir.join("events.jsonl")).await.unwrap();
        tokio::fs::create_dir(dir.join("harness.jsonl")).await.unwrap();
        let rt = app.runtime("abc").await;
        assert!(
            app.storage_alert.read().await.is_none(),
            "the failure waits for a caller that can report it, not the load itself"
        );
        app.report_load_error("abc", &rt).await;
        assert!(
            app.storage_alert.read().await.is_some(),
            "the read failure is recorded, not swallowed"
        );
        let logs = rt.logs.lock().await;
        let last = logs.back().unwrap();
        assert_eq!(last["type"], "harness_log");
        let message = last["message"].as_str().unwrap();
        assert!(message.contains("events.jsonl"), "{message}");
        assert!(message.contains("harness.jsonl"), "{message}");
        assert!(
            message.contains("may be recorded a second time"),
            "the events consequence is said plainly: {message}"
        );
        assert!(
            message.contains("the log history starts over"),
            "the log consequence is said plainly: {message}"
        );
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreadable_event_log_names_only_its_own_consequence() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        // Reading a directory is an error here (EISDIR); harness.jsonl is left alone, so nothing
        // else can overwrite the alert and the report can be read back verbatim.
        tokio::fs::create_dir(app.session_dir("abc").join("events.jsonl"))
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        app.report_load_error("abc", &rt).await;
        let alert = app.storage_alert.read().await.clone().unwrap();
        assert!(
            alert.message.contains("read the colony's saved history"),
            "either file failing is a failure of the colony's saved history: {}",
            alert.message
        );
        let logs = rt.logs.lock().await;
        let message = logs.back().unwrap()["message"].as_str().unwrap();
        assert!(
            message.contains("the reconnect cursor restarts"),
            "the events consequence is named: {message}"
        );
        assert!(
            !message.contains("harness.jsonl") && !message.contains("log history starts over"),
            "a log that loaded fine is not accused of restarting: {message}"
        );
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn replay_continues_past_an_undecodable_line_instead_of_truncating() {
        // The socket replay cannot be spun up here (it owns a websocket), so this drives
        // the same decode — split on newlines, `replay_line` per chunk — over a file with
        // a non-UTF-8 line and a bad-JSON line between two good ones.
        let good = |seq: u64, line: &str| {
            let (got_seq, got_line) = replay_line(line.as_bytes().to_vec()).unwrap();
            assert_eq!(got_seq, seq);
            assert_eq!(got_line, line);
        };
        good(1, r#"{"seq":1,"type":"status","state":"working"}"#);
        assert!(replay_line(vec![0xff, 0xfe, b' ', b'x']).is_none(), "not UTF-8: skipped");
        assert!(replay_line(b"not json at all".to_vec()).is_none(), "bad JSON: skipped");
        assert!(
            replay_line(br#"{"type":"status"}"#.to_vec()).is_none(),
            "no seq: skipped"
        );

        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}\n");
        bytes.extend_from_slice(b"\xff\xfe not utf8\n");
        bytes.extend_from_slice(b"not json at all\n");
        bytes.extend_from_slice(b"{\"seq\":2,\"type\":\"status\",\"state\":\"idle\"}\n");
        tokio::fs::write(app.session_dir("abc").join("events.jsonl"), &bytes)
            .await
            .unwrap();

        let file = tokio::fs::File::open(app.session_dir("abc").join("events.jsonl"))
            .await
            .unwrap();
        let mut chunks = BufReader::new(file).split(b'\n');
        let mut replayed: Vec<(u64, String)> = Vec::new();
        let mut skipped = 0usize;
        while let Ok(Some(chunk)) = chunks.next_segment().await {
            if chunk.is_empty() {
                continue;
            }
            match replay_line(chunk) {
                Some((seq, line)) => replayed.push((seq, line)),
                None => skipped += 1,
            }
        }
        assert_eq!(skipped, 2, "both bad lines are counted, neither stops the replay");
        assert_eq!(
            replayed.len(),
            2,
            "the valid event after the bad lines is still replayed"
        );
        assert_eq!(replayed[0].0, 1);
        assert_eq!(replayed[1].0, 2);
        assert!(replayed[1].1.contains("\"idle\""), "{}", replayed[1].1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony on the issue, in a state where a second one duplicates its work.
    fn on_issue(id: &str, issue: u64, status: SessionStatus) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.issue = Some(issue);
        s
    }

    #[test]
    fn a_live_or_published_colony_holds_its_issue_against_a_second_one() {
        // FindsYou-Work/app #7 drew four colonies and #13 drew two, because nothing asked.
        for status in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::PrOpened,
        ] {
            let sessions = vec![on_issue("first", 7, status)];
            let held = issue_held_by(&sessions, "acme/repo", 7);
            assert_eq!(
                held.map(|s| s.id),
                Some("first".to_string()),
                "a colony that is {} still holds #7",
                status.as_str()
            );
        }
    }

    #[test]
    fn a_finished_colony_leaves_its_issue_free_to_try_again() {
        for status in [
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
            SessionStatus::Stopped,
            SessionStatus::Failed,
        ] {
            let sessions = vec![on_issue("first", 7, status)];
            assert!(
                issue_held_by(&sessions, "acme/repo", 7).is_none(),
                "{} is done with #7, so a retry is not a duplicate",
                status.as_str()
            );
        }
    }

    #[test]
    fn the_hold_is_per_repository_and_per_issue() {
        let sessions = vec![
            on_issue("other-repo", 7, SessionStatus::Running),
            on_issue("other-issue", 8, SessionStatus::Running),
        ];
        let mut elsewhere = sessions.clone();
        elsewhere[0].repo = "acme/different".into();
        assert!(
            issue_held_by(&elsewhere, "acme/repo", 7).is_none(),
            "the same issue number in another repository is a different issue"
        );
        assert!(
            issue_held_by(&sessions, "acme/repo", 9).is_none(),
            "an untouched issue is free"
        );
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("other-repo".to_string()),
            "the colony on this repo's #7 is the one that holds it"
        );
    }

    #[test]
    fn every_status_name_matches_what_the_api_serialises() {
        // The message names the state back to a person, so it must be the name they see in the UI.
        for status in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            SessionStatus::PrOpened,
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
            SessionStatus::Stopped,
            SessionStatus::Failed,
        ] {
            let wire = serde_json::to_value(status).unwrap();
            assert_eq!(
                wire.as_str(),
                Some(status.as_str()),
                "as_str drifted from serde for {status:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_fresh_colony_with_no_stored_events_loads_quietly_at_seq_zero() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let rt = app.runtime("abc").await;
        app.report_load_error("abc", &rt).await;
        assert_eq!(rt.last_seq.load(Ordering::SeqCst), 0);
        assert!(rt.logs.lock().await.is_empty());
        assert!(
            app.storage_alert.read().await.is_none(),
            "a colony that has never emitted an event has no events.jsonl, and that is not a failure"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // -- org workspaces on and off --------------------------------------------------------------

    /// A throwaway App whose config switches the `acme` workspace off, the way an old install's
    /// `orgs.json` plus one settings save leaves it.
    async fn app_with_org_switched_off(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = test_app(&root);
        std::fs::create_dir_all(app.cfg.config_dir.clone()).unwrap();
        std::fs::write(app.cfg.config_dir.join("orgs.json"), r#"{"acme": {"enabled": false}}"#).unwrap();
        let mut s = colony("acme", status);
        s.id = id.to_string();
        s.git_admin_dir = Some("git".into());
        app.sessions.write().await.push(s);
        tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        (app, root)
    }

    #[tokio::test]
    async fn a_switched_off_org_refuses_new_colonies_and_names_the_way_back_on() {
        let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
        let err = create(
            State(app.clone()),
            Json(NewSession {
                repo: "acme/app".into(),
                issue: None,
                title: String::new(),
                instructions: String::new(),
                autopilot: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                model_tier: None,
                after: None,
                origin: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        let message = err.1.to_string();
        assert!(message.contains("acme"), "{message}");
        assert!(message.contains("switched off"), "{message}");
        assert!(
            message.contains("org settings"),
            "the message says what to do about it: {message}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn colonies_of_a_switched_off_org_stay_listed_and_resume() {
        let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
        let listed = list(State(app.clone())).await.0;
        let kept = listed.iter().find(|s| s.id == "kept").unwrap();
        assert_eq!(kept.org, "acme", "the colony is still in the list");
        // The real resume path, not just its gate: the org's switch does not make `resume` refuse
        // the colony — it is claimed and handed to a fresh boot like any other. The boot itself
        // never runs here: the spawned task is dropped with the one-thread test runtime before it
        // is polled, so nothing reaches for GitHub or a microVM.
        let resumed = resume(State(app.clone()), Path("kept".into()))
            .await
            .unwrap_or_else(|e| panic!("resume refused a colony of a switched-off org: {:#}", e.1))
            .0;
        assert_eq!(resumed.id, "kept");
        assert_eq!(
            resumed.status,
            SessionStatus::Starting,
            "the resume claimed the colony and started a boot"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn starting_a_colony_marks_its_org_known_so_the_operator_is_never_asked_about_it() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        // The smallest install `create` insists on: an agent module matching the configured provider
        // and a guest binary that claims to be an ELF.
        let assets = root.join("assets");
        let dir = assets.join("modules/agents/claude-code");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("module.json"), r#"{"id":"claude-code","entry":["run"]}"#).unwrap();
        std::fs::create_dir_all(assets.join("bin")).unwrap();
        std::fs::write(assets.join("bin/colonizer-agentd"), b"\x7fELF padding").unwrap();
        let agent = AgentModule {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            description: String::new(),
            dir,
            entry: vec!["run".into()],
            needs_claude: false,
            schema: json!({}),
        };
        let app = crate::tests::test_app_with_agents(&root, vec![agent], |cfg| cfg.assets = Some(assets));
        // The org is still awaiting an answer when the colony starts, sighting and avatar both.
        *app.new_orgs.write().await =
            std::collections::BTreeMap::from([("acme".to_string(), Some("https://a/acme.png".to_string()))]);

        let created = create(
            State(app.clone()),
            Json(NewSession {
                repo: "acme/app".into(),
                issue: None,
                title: String::new(),
                instructions: String::new(),
                autopilot: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                model_tier: None,
                after: None,
                origin: None,
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(created.org, "acme");
        assert_eq!(
            app.known_orgs().unwrap().get("acme").cloned(),
            Some(crate::orgs::KnownOrg {
                avatar_url: Some("https://a/acme.png".into()),
            }),
            "working in an org is an answer, and the sighting's avatar is recorded with it; the prompt \
             must never ask about it later"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A pin is the operator's decision: an explicit preset and `custom` both come back unchanged,
    /// with nothing to explain, even when the repository carries a marker that would have detected.
    #[test]
    fn a_pinned_stack_skips_detection_and_logs_nothing() {
        let detected = crate::presets::Detected {
            stack: "node",
            marker: "package.json".to_string(),
        };
        for configured in ["rust", crate::presets::CUSTOM] {
            let (stack, message) = stack_choice(configured, Some(&detected));
            assert_eq!(stack, configured, "the pin decides, not the repository");
            assert!(message.is_none(), "{configured} has nothing to explain");
        }
    }

    /// `auto` over a repository with markers takes the detected stack, and the log line names both
    /// the file that decided and the image that will boot.
    #[test]
    fn auto_uses_the_detected_stack_and_names_the_marker_and_image() {
        let detected = crate::presets::Detected {
            stack: "rust",
            marker: "Cargo.toml".to_string(),
        };
        let (stack, message) = stack_choice(crate::presets::AUTO, Some(&detected));
        assert_eq!(stack, "rust");
        assert_eq!(
            message.as_deref(),
            Some("detected rust from Cargo.toml, using rust:1-bookworm"),
            "the log names the stack, the marker that chose it, and the image"
        );
    }

    /// A marker in a subdirectory keeps its repo-relative path in the log, so the log says which
    /// file decided.
    #[test]
    fn a_subdirectory_marker_names_the_file_that_decided() {
        let detected = crate::presets::Detected {
            stack: "node",
            marker: "web/package.json".to_string(),
        };
        let (stack, message) = stack_choice(crate::presets::AUTO, Some(&detected));
        assert_eq!(stack, "node");
        let message = message.expect("detection logs what it saw");
        assert!(
            message.contains("web/package.json"),
            "the log should carry the subdirectory path, got {message:?}"
        );
    }

    /// `auto` over a repository with no markers falls back, and says so rather than silently
    /// picking one.
    #[test]
    fn auto_with_no_marker_found_falls_back_and_says_so() {
        let (stack, message) = stack_choice(crate::presets::AUTO, None);
        assert_eq!(stack, crate::presets::AUTO_FALLBACK);
        assert_eq!(
            message.as_deref(),
            Some("no stack marker found in the repository; using the node stack"),
            "the fallback is logged, not silent"
        );
    }

    // -- stacking (create with `after`) ----------------------------------------------------------

    /// The smallest install `create` insists on, as in the org-known test above: an agent module
    /// matching the configured provider and a guest binary that claims to be an ELF. Shared with
    /// validation.rs, whose fix-colony test creates a session the same way.
    pub(crate) fn app_that_can_create(root: &std::path::Path) -> Shared {
        let assets = root.join("assets");
        let dir = assets.join("modules/agents/claude-code");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("module.json"), r#"{"id":"claude-code","entry":["run"]}"#).unwrap();
        std::fs::create_dir_all(assets.join("bin")).unwrap();
        std::fs::write(assets.join("bin/colonizer-agentd"), b"\x7fELF padding").unwrap();
        let agent = AgentModule {
            id: "claude-code".into(),
            name: "Claude Code".into(),
            description: String::new(),
            dir,
            entry: vec!["run".into()],
            needs_claude: false,
            schema: json!({}),
        };
        crate::tests::test_app_with_agents(root, vec![agent], |cfg| cfg.assets = Some(assets))
    }

    /// A `create` request with nothing but the repo and, where the test names one, the parent.
    fn stack_request(repo: &str, after: Option<String>) -> Json<NewSession> {
        Json(NewSession {
            repo: repo.into(),
            issue: None,
            title: String::new(),
            instructions: String::new(),
            autopilot: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            model_tier: None,
            after,
            origin: None,
        })
    }

    #[tokio::test]
    async fn a_colony_asked_to_stack_on_another_queues_until_that_one_pushes() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut parent = colony("acme", SessionStatus::Running);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);

        let created = create(State(app.clone()), stack_request("acme/app", Some("parent".into())))
            .await
            .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
        assert_eq!(
            created.parent.as_deref(),
            Some("parent"),
            "the colony records what it is stacked on"
        );
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "the parent has not pushed a branch, so the colony queues even though a slot is free"
        );
        assert_eq!(
            created.base, None,
            "the base is the boot's business, resolved fresh when the wait ends"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn create_refuses_to_stack_on_a_colony_that_can_never_lend_a_branch() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut dead = colony("acme", SessionStatus::Failed);
        dead.id = "dead".into();
        dead.branch = "colonizer/issue-1-dead".into();
        dead.repo = "acme/app".into();
        app.sessions.write().await.push(dead);

        let err = create(State(app.clone()), stack_request("acme/app", Some("dead".into())))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "a refusal, like the duplicate-issue one");
        let message = err.1.to_string();
        assert!(message.contains("dead"), "{message}");
        assert!(message.contains("failed"), "it says which reason applies: {message}");
        let sessions = app.sessions.read().await;
        assert_eq!(sessions.len(), 1, "nothing was created");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stacking_on_a_colony_that_does_not_exist_is_refused_as_a_404_naming_it() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let err = create(State(app.clone()), stack_request("acme/app", Some("ghost".into())))
            .await
            .unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::NOT_FOUND,
            "the same answer asking for an unknown colony gets"
        );
        let message = err.1.to_string();
        assert!(message.contains("ghost") && message.contains("no colony"), "{message}");
        assert!(app.sessions.read().await.is_empty(), "nothing was created");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_after_of_nothing_but_whitespace_is_refused_not_read_as_absent() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let err = create(State(app.clone()), stack_request("acme/app", Some("   ".into())))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "the request names nothing stackable");
        assert!(err.1.to_string().contains("`after`"), "{}", err.1);
        assert!(
            app.sessions.read().await.is_empty(),
            "silently starting unstacked would branch from the wrong place without a word"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stacking_on_a_colony_of_another_repository_is_refused_naming_both() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        let mut parent = colony("acme", SessionStatus::PrOpened);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);

        let err = create(State(app.clone()), stack_request("acme/other", Some("parent".into())))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT, "a refusal at create, like the other stack ones");
        let message = err.1.to_string();
        assert!(message.contains("acme/app"), "the parent's repository is named: {message}");
        assert!(message.contains("acme/other"), "and so is this one's: {message}");
        let sessions = app.sessions.read().await;
        assert_eq!(sessions.len(), 1, "nothing was created to fail a boot later");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }
}
