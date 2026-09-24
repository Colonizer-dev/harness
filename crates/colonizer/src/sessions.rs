//! Interactive sessions: one worktree + microVM + agent per task, bridged to browsers.
//!
//! Harness ⇄ VM traffic goes to `colonizer-agentd` over the private mesh (or a loopback port when the
//! mesh module is disabled). Agent events are persisted per session and fanned out to every open
//! browser; browser commands are forwarded to the agent.

use crate::{
    ApiResult, App, CLAUDE_API_HOST, Shared, client_error,
    config::{ModulesConfig, setting, setting_str, setting_u64},
    diagnosis, github, memory,
    modules::{AgentModule, schema_for},
    orgs, providers, resolve_guest_claude_bin, restack,
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
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
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
    /// predicate `queue::has_room` counts, under the live-origin assumption — every `Publishing`
    /// here counts as claimed from a live colony. A publish claimed from a stopped, failed or
    /// no-changes colony boots nothing and holds nothing, so prefer [`Session::holds_slot`], which
    /// knows the claim's origin, wherever the session record is at hand.
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

    /// Drop the attention flag the watchdog or autopilot set (`stalled`, `nudges_exhausted`,
    /// `autopilot_held`) as the colony stops or fails. The flag is only meaningful while the
    /// colony is live — it says someone should look at it — and without this a stopped colony
    /// keeps it in `sessions.json` forever. Every terminal transition calls this and notes the
    /// returned flag in the colony's log, so the history survives the clear. Returns what was
    /// removed, if anything.
    pub(crate) fn clear_attention(&mut self) -> Option<Value> {
        self.attention.take()
    }

    /// Whether this colony holds a microVM slot against the parallel limit — the predicate
    /// `queue::has_room` counts. Any live colony holds one, and so does a publish claimed from a
    /// live colony: the teardown inside the publish frees the microVM, but the slot stays claimed
    /// until the push lands, so nothing boots into the half-published worktree. A publish claimed
    /// from a stopped, failed or no-changes colony boots nothing (host-side push only) and holds
    /// nothing, so publishing a stopped colony never takes a slot another colony is waiting for.
    pub fn holds_slot(&self) -> bool {
        self.status.is_live() || (self.status == SessionStatus::Publishing && self.publishing_holds_slot)
    }
}

/// The one-line history note for an attention flag a terminal transition just removed: the reason
/// it was set, so the colony's log still says what the flag meant after the flag itself is gone.
/// `None` when there was no flag, so callers only log when something was actually cleared.
pub(crate) fn cleared_attention_message(attention: &Option<Value>) -> Option<String> {
    let attention = attention.as_ref()?;
    let reason = attention.get("reason").and_then(Value::as_str).unwrap_or("unknown");
    Some(format!(
        "clearing the attention flag ({reason}): the colony is not running, so nothing is waiting on it any more"
    ))
}

/// Startup migration, run in `serve` next to the org backfill and before `recover`: colonies
/// persisted as finished while still carrying an attention flag predate the clearing every
/// terminal transition now does. A finished colony that still carries one looks like it needs
/// attention it no longer does, so drop the flag from every terminal colony that has one — except a
/// quota-parked colony, whose flag is its resume ticket: stripping it would strand the colony,
/// parked with no reason for the queue to ever requeue. Returns how many flags were cleared.
pub(crate) fn clear_stale_attention(sessions: &mut [Session]) -> usize {
    let mut cleared = 0;
    for s in sessions.iter_mut() {
        if s.status.is_terminal() && s.attention.is_some() {
            let quota_parked = s
                .attention
                .as_ref()
                .is_some_and(|a| a["reason"].as_str() == Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON));
            if quota_parked {
                continue;
            }
            s.attention = None;
            cleared += 1;
        }
    }
    cleared
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

/// The `publishing_holds_slot` of a row written before the flag existed: holding, because every
/// publish held its slot back then, and a row of unknown origin must not silently free one.
fn publishing_holds_slot_legacy() -> bool {
    true
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
    /// Stack against the parent's branch instead of queueing for its merge (see `NewSession::stack`):
    /// the colony started from that branch while it was still open, and its pull request targets it.
    /// A child that queued for the merge starts from the default branch instead and reads false.
    #[serde(default)]
    pub stack: bool,
    /// The sha this colony's worktree branched from, recorded at boot for a stacked child: the
    /// parent's remote ref disappears once its branch is deleted, so the publish-time restack
    /// cannot re-derive it and reads this instead. `None` for unstacked colonies.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_fork: Option<String>,
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
    /// When the pull request merged: GitHub's `mergedAt` when it reported one, else the moment the
    /// watcher saw the merge. `None` until a merge is observed; colonies merged before the field
    /// existed gain it from the startup backfill, best effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<DateTime<Utc>>,
    /// When the pull request was opened, as GitHub reports it (`createdAt`); set by the PR watcher
    /// and, for colonies merged before it existed, by the startup backfill. With `merged_at` it
    /// gives the PR cycle time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_opened_at: Option<DateTime<Utc>>,
    /// The pull request's checks in one word (`success`, `failure`, `pending`, `no_checks`), as last
    /// read by the PR watcher; a settled verdict survives a later `pending` reading once merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_state: Option<crate::github::CiState>,
    /// How far the last publish got; left in place when a publish failed, so a retry knows where to look.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_stage: Option<PublishStage>,
    /// Whether this colony's `publishing` claim holds a parallel slot: true when claimed from a live
    /// colony, false when claimed from a stopped, failed or no-changes one (see `holds_slot`). Set
    /// at the single claim site in `publish.rs`; terminal states hold nothing either way, so it is
    /// never cleared. Missing on rows written before the flag existed, which were all claimed under
    /// the old every-publish-holds rule — so those default to holding, the conservative reading.
    #[serde(default = "publishing_holds_slot_legacy")]
    pub publishing_holds_slot: bool,
    /// Whether the colony's pull request fell behind its base or conflicts with it, and the
    /// watcher's auto-rebase could not finish on its own: set when a rebase is aborted, fails, or
    /// has no colony left to run in, cleared when a rebase lands or the PR reads clean again. The
    /// cockpit reads it as "needs rebase".
    #[serde(default)]
    pub needs_rebase: bool,
    /// Whether `needs_rebase` was set because the colony behind the pull request is gone (finished
    /// and reclaimed, mid-teardown, or otherwise not live) rather than woken to fix it itself —
    /// issue #453's case for a notification, since nothing is left running that will ever clear
    /// `needs_rebase` on its own. Cleared wherever `needs_rebase` is cleared.
    #[serde(default)]
    pub rebase_orphaned: bool,
    /// The live same-repo colony a fresh colony queued behind for overlap (issue #453): it starts
    /// once that colony is no longer live. `None` once started; stacked colonies never carry one —
    /// they already wait on their parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_behind: Option<String>,
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
    /// The orchestrator model the operator named at launch (a Claude alias or ID, or
    /// `<provider>/<model>`), replacing whatever routing would pick. A launch record, like `model_tier`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    /// The subagent model the operator named at launch, replacing the agent module's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_model_override: Option<String>,
    /// The Claude account this colony bills to (issue #95): the per-colony choice, else the org's
    /// override, else the install default, resolved at launch. Like `model_tier`, a launch record.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_account: Option<String>,
    /// The routing decision this colony booted with: the tier, the tier the rule would have chosen, and
    /// the signals behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_routing: Option<Value>,
    /// The providers this colony may spend on through the gateway (issue #409): the ids its model
    /// settings actually route to, as computed at boot. `proxy` refuses a request for any other
    /// configured provider — providers.json is mothership-wide, and one colony's token must not
    /// open another colony's provider. Empty for Claude-only colonies. `None` for a session saved
    /// before the field existed: a colony already running across the upgrade keeps the access it
    /// booted with, and its next boot derives and enforces the set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    /// Dollars the gateway recorded for responses it routed to providers (everything but Claude, whose
    /// own cost lands above). Kept on the session so spend survives a restart and reaches the UI.
    pub routed_cost_usd: Option<f64>,
    /// What the colony leaves on the host — its worktree plus its session directory — as last measured by
    /// the host-disk check, which runs only when a host-disk quota applies to the colony. Not the
    /// microVM's root disk, which is a separate limit (microsandbox's `--root-disk`).
    pub host_disk_bytes: Option<u64>,
    pub cleaned_up: bool,
    /// Operator opt-out of automatic worktree reclamation; manual cleanup still works.
    pub keep_worktree: bool,
    /// Set by the watchdog: `{reason, since, nudges}`.
    pub attention: Option<Value>,
    /// Last agent progress (filled from the runtime for live colonies).
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Where the last launch's time went: `{total_ms, phases: [{name, ms}]}`.
    /// Cleared when a colony is claimed for a (re)boot, then `{phases}` with the phases done so far
    /// while it boots; `total_ms` appears only once the boot finishes. A boot that stops part way
    /// keeps its `{phases}`.
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
    /// When this boot attempt's retry clock started, in unix seconds. Set before the first
    /// pre-worktree step and cleared once the worktree exists, so a harness restart resumes the
    /// same retry budget instead of starting a new one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_attempt_started_at: Option<u64>,
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
            stack: false,
            stack_fork: None,
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
            merged_at: None,
            pr_opened_at: None,
            ci_state: None,
            publish_stage: None,
            publishing_holds_slot: false,
            needs_rebase: false,
            rebase_orphaned: false,
            queued_behind: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            model_routing: None,
            allowed_providers: None,
            routed_cost_usd: None,
            host_disk_bytes: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            app_slot: None,
            boot_attempt_started_at: None,
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
    /// Set once, by `resume` on the retired run's Runtime only: pre-existing event sockets hold
    /// that Runtime and can never see the new run's events, so they close and reconnect into the
    /// new epoch. Distinct from `stop`, which `teardown_vm` also sets on a plain stop where
    /// sockets stay open on purpose.
    pub(crate) retired: watch::Sender<bool>,
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
        //
        // The same pass restores the open question. It is otherwise set only while live events are
        // handled (`events.rs`), and agentd replays only what is past the cursor, so after a restart a
        // question asked before it would be forgotten: the judge would never answer it and autopilot
        // would publish over it. The last question with no later `question_answered` for its id is
        // still open, with its own timestamp as the start of the wait.
        let mut agent_seq = 0;
        let mut open_question: Option<(String, Vec<Value>, Option<DateTime<Utc>>)> = None;
        for v in events_bytes
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_str::<Value>(std::str::from_utf8(line).ok()?).ok())
        {
            let Some(kind) = v.get("type").and_then(Value::as_str) else {
                continue;
            };
            if crate::validation::is_host_chain_type(kind) {
                continue;
            }
            if let Some(seq) = v.get("a_seq").and_then(Value::as_u64).or_else(|| v["seq"].as_u64()) {
                agent_seq = agent_seq.max(seq);
            }
            let question_id = v.get("question_id").and_then(Value::as_str);
            match (kind, question_id) {
                ("question", Some(id)) => {
                    let questions = v.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
                    let asked = v
                        .get("ts")
                        .and_then(Value::as_str)
                        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                        .map(|ts| ts.with_timezone(&Utc));
                    open_question = Some((id.to_string(), questions, asked));
                }
                ("question_answered", Some(id)) if open_question.as_ref().is_some_and(|(open, ..)| open == id) => {
                    open_question = None;
                }
                _ => {}
            }
        }
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
            open_question: Mutex::new(
                open_question
                    .as_ref()
                    .map(|(id, questions, _)| (id.clone(), questions.clone())),
            ),
            pr_mark: Mutex::new(github::pr_description_mark(&dir.join("out"))),
            interrupted: std::sync::atomic::AtomicBool::new(false),
            stop: watch::channel(false).0,
            retired: watch::channel(false).0,
            file_lock: Mutex::new(()),
            findings_lock: Mutex::new(()),
            events_path,
            logs_path,
            activity: Mutex::new({
                let now = Utc::now();
                let mut activity = Activity::new(now);
                // A question with no readable timestamp starts its wait now, as the live path does.
                activity.question_since = open_question.map(|(_, _, asked)| asked.unwrap_or(now));
                activity
            }),
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

/// Whether two session records hold the same content, ignoring `updated_at`. Compared through
/// their JSON projections rather than a derived `PartialEq`: floats like `cost_usd` make
/// structural equality brittle, and the projection keeps the check to exactly what persists.
/// A serialization failure falls back to "changed", so a write is never skipped on doubt.
fn session_contents_equal(before: &Session, after: &Session) -> bool {
    let (Ok(Value::Object(mut a)), Ok(Value::Object(mut b))) = (serde_json::to_value(before), serde_json::to_value(after)) else {
        return false;
    };
    a.remove("updated_at");
    b.remove("updated_at");
    a == b
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
            let before = session.clone();
            let result = f(session);
            if session_contents_equal(&before, session) {
                // The closure changed nothing: leave `updated_at` alone and skip the persist and
                // broadcast, or every no-op poll rewrites sessions.json and wakes every browser.
                // There can be no terminal transition either, so no spend edge is recorded.
                // `updated_at` itself is excluded from the comparison above.
                let session = session.clone();
                return Some((session, result));
            }
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

    /// Records a background measurement (e.g. `host_disk_bytes`) without
    /// bumping `updated_at`: a measurement is not activity, and the stall
    /// readout falls back to `updated_at` for colonies with no events yet, so
    /// stamping every watch tick would pin them at "just now" forever.
    /// Persists and broadcasts only when the value actually changed.
    pub async fn record_measurement(&self, id: &str, f: impl FnOnce(&mut Session)) {
        let session = {
            let mut sessions = self.sessions.write().await;
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return;
            };
            let before = session.clone();
            f(session);
            if session_contents_equal(&before, session) {
                return;
            }
            session.clone()
        };
        self.persist_and_broadcast(&session).await;
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
        write_atomic(&self.sessions_file(), &data).await?;
        // The session list is written on nearly every state change, so its saves are the signal
        // that the disk is taking writes again after a failure.
        self.storage_succeeded().await;
        Ok(())
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

    /// Notes an attention flag a terminal transition just cleared in the colony's log, so the
    /// reason it was set survives the clear. Silent when there was no flag.
    pub(crate) async fn note_cleared_attention(&self, id: &str, attention: Option<Value>) {
        if let Some(message) = cleared_attention_message(&attention) {
            self.session_log(id, "info", message).await;
        }
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
    /// Run the orchestrator on this model (a Claude alias or ID, or `<provider>/<model>` naming a
    /// configured provider) instead of the one routing picks. Red-team hunters use it.
    #[serde(default)]
    pub model_override: Option<String>,
    /// Run the colony's subagents on this model instead of the agent module's `subagent_model`.
    #[serde(default)]
    pub subagent_model_override: Option<String>,
    /// Bill this colony to a named Claude account instead of the org's override or install default.
    #[serde(default)]
    pub claude_account: Option<String>,
    /// How a colony created with `after` relates to its parent. By default the colony queues until
    /// the parent's pull request merges, then starts from the fresh default branch — so a parent
    /// that merges with delete-branch never strands the child's pull request. Only `stack: true`
    /// branches from the parent's branch while it is still open, with the pull request targeting it.
    #[serde(default)]
    pub after: Option<String>,
    /// Stack this colony against its parent's branch instead of queueing for the parent's merge:
    /// the colony starts as soon as the parent has pushed its branch, and its pull request targets
    /// that branch. Off by default: a child that only needs the parent's work merged waits for it.
    #[serde(default)]
    pub stack: bool,
    /// Who is asking, when the operator is not: the burn-down scheduler tags its colonies
    /// `Some("burn_down")` so `POST /api/burn-down/stop` can find them again.
    #[serde(default)]
    pub origin: Option<String>,
    /// Opt in to overlap-aware queueing: queue behind a live same-repo colony that's already
    /// touching files, instead of developing against the same paths at once. Off by default —
    /// most callers would rather start immediately than have an unrelated colony's edits hold
    /// them up. See `overlap_queue_target`.
    #[serde(default)]
    pub serialize: Option<bool>,
}

/// A launch-time model choice, trimmed: empty is none, and a `<provider>/<model>` must name a
/// configured provider, so a typo is refused at launch instead of failing inside the colony.
pub(crate) fn launch_model(app: &crate::App, raw: Option<&str>, what: &str) -> Result<Option<String>, crate::AppError> {
    let Some(model) = raw.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    if let Some((provider, name)) = model.split_once('/')
        && (name.is_empty() || !app.providers().iter().any(|p| p.id == provider))
    {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("the {what} {model:?} names no configured provider \"{provider}\""),
        ));
    }
    Ok(Some(model.to_string()))
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

/// The 409 message for a second colony on an issue another colony still holds: the holder, where
/// its work stands, and the way out. One function, so the fast-path pre-check and the authoritative
/// in-lock re-check refuse with the same words.
fn duplicate_message(held: &Session, issue: u64) -> String {
    let where_it_is = match held.pr_url.as_deref() {
        Some(url) => format!("its pull request is open at {url}"),
        None => format!("it is {}", held.status.as_str()),
    };
    format!(
        "colony {} is already on #{issue} and {where_it_is}. Starting a second one duplicates its \
         work: read that colony first, or pass allow_duplicate to start another anyway.",
        held.id
    )
}

/// The authoritative duplicate-colony claim, run while the admission write lock is held: the
/// pre-check in `create` reads under a read lock, so two launches can both pass it before either
/// Issue #453: the colony a fresh same-repo colony queues behind for overlap, if any — the oldest
/// live same-repo colony that already has a worktree, and so may be touching files. Pure, so the
/// rule is testable apart from the file scan that [`overlap_queue_target`] wraps around it.
pub(crate) fn overlap_holder(sessions: &[Session], repo: &str) -> Option<String> {
    sessions
        .iter()
        .filter(|s| s.repo == repo && s.status.is_live() && s.git_admin_dir.is_some())
        .min_by_key(|s| s.created_at)
        .map(|s| s.id.clone())
}

/// Issue #453: who a fresh colony queues behind, if anyone — the [`overlap_holder`], and only while
/// some live sibling still has touched files. This is not a real overlap check: the newcomer's own
/// file set is unknown until it boots, so any live touched files queue it
/// (`rebase::should_queue_behind_live_colony`, the conservative reading). Best effort with a short
/// overall budget: an unreadable worktree reads as untouched, never as a reason to queue.
async fn overlap_queue_target(sessions: &[Session], repo: &str) -> Option<String> {
    let holder = overlap_holder(sessions, repo)?;
    let siblings: Vec<(String, String)> = sessions
        .iter()
        .filter(|s| s.repo == repo && s.status.is_live() && s.git_admin_dir.is_some())
        .map(|s| (s.worktree.clone(), s.base.clone().unwrap_or_else(|| "main".to_string())))
        .collect();
    let mut live_files = Vec::new();
    let scan = async {
        for (worktree, base) in &siblings {
            live_files.extend(crate::rebase::touched_files(std::path::Path::new(worktree), &format!("origin/{base}")).await);
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), scan).await;
    live_files.sort();
    crate::rebase::should_queue_behind_live_colony(&live_files).then_some(holder)
}

/// inserts — this re-check closes that window, and the loser gets its holder back for a 409.
/// `Ok` carries the admitted colony, whether it queued, and how many were already waiting;
/// `Err` carries the colony already holding the issue, and nothing is inserted.
#[allow(clippy::result_large_err)]
fn try_claim_session(
    sessions: &mut Vec<Session>,
    room: bool,
    mut session: Session,
    repo: &str,
    issue: Option<u64>,
    allow_duplicate: bool,
    wait_for_parent: bool,
) -> Result<(Session, bool, usize), Session> {
    if let (Some(number), false) = (issue, allow_duplicate)
        && let Some(held) = issue_held_by(sessions, repo, number)
    {
        return Err(held);
    }
    // A colony still waiting for its parent's branch queues even when a slot is free: booting now
    // would branch from the default branch, which is exactly what stacking exists to avoid. A
    // queued colony holds no slot, so nothing is wasted by the wait.
    // Issue #453: a newcomer queued behind a live same-repo colony for overlap stays queued even
    // with a free slot, and never carries a parent — it still branches fresh from the default
    // branch when the queue starts it. A holder that finished between the scan and this lock
    // releases it at once, clearing the stale pointer.
    let overlap_held = session.parent.is_none()
        && session
            .queued_behind
            .as_deref()
            .is_some_and(|holder| sessions.iter().any(|s| s.id == holder && s.status.is_live()));
    if !overlap_held {
        session.queued_behind = None;
    }
    session.status = if room && !wait_for_parent && !overlap_held {
        SessionStatus::Starting
    } else {
        SessionStatus::Queued
    };
    let queued = session.status == SessionStatus::Queued;
    let waiting = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
    sessions.push(session.clone());
    Ok((session, queued, waiting))
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

#[allow(clippy::result_large_err)]
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
    // The colony's account: the request's explicit choice, else the org's override, else the
    // install default. Resolved before the gate so the refusal can name the account that is missing.
    let claude_account = crate::claude_accounts::resolve_account(
        req.claude_account.as_deref(),
        app.org_settings(owner)
            .agent
            .as_ref()
            .and_then(|a| a.claude_account.as_deref()),
        &crate::claude_accounts::load_meta(&app.cfg.config_dir),
    );
    if agent.needs_claude && app.claude_cred_for(Some(&claude_account)).is_none() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("log in with Claude in Settings first (account '{claude_account}')"),
        ));
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
    let model_override = launch_model(&app, req.model_override.as_deref(), "model")?;
    let subagent_model_override = launch_model(&app, req.subagent_model_override.as_deref(), "subagent model")?;
    // Relating to the parent (`after`): by default the colony queues until the parent's pull request
    // merges and then starts from the fresh default branch; `stack: true` branches from the parent's
    // branch as soon as it is pushed instead. Whitespace is refused rather than read as nothing — an
    // operator who typed something there meant to relate. A parent that can never provide what the
    // mode needs is refused now, with the reason; one that has not gotten there yet is allowed, and
    // the colony queues for it — the queue, not this handler, does the waiting.
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
                    match restack::queue_decision(&parent_id, Some(source), req.stack) {
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
        return Err(client_error(StatusCode::CONFLICT, &duplicate_message(&held, issue)));
    }
    // A second mothership shares no memory with this one, so the local guard above cannot see its
    // colonies: the issue itself carries the claim (see claims.rs). A failed lookup degrades to the
    // local guard rather than refusing the launch.
    if crate::claims::should_check_remote(req.issue, req.allow_duplicate)
        && let Some(issue) = req.issue
    {
        let checked = crate::claims::check_remote_claim(&app, &repo, issue).await;
        if let Err(e) = &checked {
            eprintln!("claims: remote duplicate check for #{issue} in {repo} failed ({e:#}); falling back to the local guard");
        }
        if let Some(info) = crate::claims::remote_result_or_fallback(checked) {
            return Err(client_error(
                StatusCode::CONFLICT,
                &crate::claims::remote_conflict_message(&info, issue),
            ));
        }
    }
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    // Past the limit a colony waits its turn rather than being refused; `run_queue` starts it later.
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Resolved before the admission lock: `org_settings` reads the orgs file with blocking IO.
    let org_settings = app.org_settings(owner);
    let org_limit = orgs::org_max_parallel(&org_settings);
    let repo_limit = crate::queue::repo_limit(&modules, &org_settings);

    // Issue #453: overlap-aware queueing — while a live same-repo colony still has touched files,
    // a newcomer queues behind it instead of developing against the same paths at once. Opt-in via
    // `serialize`: most launches would rather start immediately than have an unrelated colony's
    // edits hold them up. Stacked colonies already wait on their parent, so the scan is skipped
    // for them regardless.
    let queued_behind = if req.serialize == Some(true) && parent.is_none() {
        overlap_queue_target(&app.sessions.read().await, &repo).await
    } else {
        None
    };

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
        stack: req.stack,
        stack_fork: None,
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
        merged_at: None,
        pr_opened_at: None,
        ci_state: None,
        publish_stage: None,
        // A fresh colony is starting or queued, never publishing: the flag is inert.
        publishing_holds_slot: false,
        needs_rebase: false,
        rebase_orphaned: false,
        queued_behind,
        error: None,
        cost_usd: None,
        model_usage: None,
        model_tier,
        model_override,
        subagent_model_override,
        claude_account: Some(claude_account),
        model_routing: None,
        // Filled in at boot, once the colony's model settings resolve to actual providers.
        allowed_providers: None,
        routed_cost_usd: None,
        host_disk_bytes: None,
        cleaned_up: false,
        keep_worktree: false,
        attention: None,
        last_activity_at: None,
        boot_timing: None,
        boot_cpus: None,
        boot_memory: None,
        app_slot: None,
        boot_attempt_started_at: None,
        created_at: now,
        updated_at: now,
    };
    let dir = app.session_dir(&id);
    tokio::fs::create_dir_all(dir.join("vm")).await?;
    tokio::fs::create_dir_all(dir.join("out")).await?;
    // The room check and the push share one write lock, so two launches colliding on the last free slot
    // cannot both take it. Counted before the push, so this colony is never waiting behind itself.
    // The duplicate-issue check is re-checked here too: the fast-path pre-check above reads under a
    // read lock, so two launches can both pass it before either inserts — the loser is refused with
    // the same 409 inside the lock, where check and insert are one atomic step.
    let claimed = with_slot(
        &app.sessions,
        owner,
        &repo,
        max_parallel,
        org_limit,
        repo_limit,
        |sessions, room| {
            try_claim_session(
                sessions,
                room,
                session,
                &repo,
                req.issue,
                req.allow_duplicate,
                wait_for_parent,
            )
        },
    )
    .await;
    let (session, queued, waiting) = match claimed {
        Ok(admitted) => admitted,
        Err(held) => {
            // The colony directories created above belong to a colony that never was; take them back
            // out, best effort, before refusing.
            let _ = tokio::fs::remove_dir_all(&dir).await;
            let issue = req.issue.unwrap_or_default();
            return Err(client_error(StatusCode::CONFLICT, &duplicate_message(&held, issue)));
        }
    };
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
    // The launch claims the issue on GitHub itself, so a second mothership sees it: best effort in
    // the background, never failing the launch.
    if crate::claims::should_check_remote(session.issue, req.allow_duplicate)
        && let Some(issue) = session.issue
    {
        crate::claims::spawn_publish(app.clone(), repo.clone(), issue, id.clone());
    }
    if queued {
        if let Some(holder) = session.queued_behind.as_deref() {
            app.session_log(
                &id,
                "info",
                format!(
                    "queued behind colony {holder}: it is working in the same repository, so this colony starts once it finishes"
                ),
            )
            .await;
        } else if let (true, Some(parent_id)) = (wait_for_parent, session.parent.as_deref()) {
            let why = if session.stack {
                format!("queued behind colony {parent_id}: it starts once that colony has pushed its branch")
            } else {
                format!("queued behind colony {parent_id}: it starts once that colony's pull request merges")
            };
            app.session_log(&id, "info", why).await;
        } else {
            let ahead = if waiting == 0 {
                String::new()
            } else {
                format!(", behind {waiting} already waiting")
            };
            let limits = crate::queue::limits_message(max_parallel, org_limit, repo_limit);
            app.session_log(&id, "info", format!("queued: {limits}{ahead}")).await;
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
            // The status moved out from under this task — usually a stop that ran before
            // `sandbox::boot` created the microVM, so the stop's `msb rm` removed nothing
            // and this boot's teardown below is the only reaper left for the orphan. The
            // old code returned here assuming the stop handler had cleaned up, leaking a
            // running `colonizer-{id}` no reaper ever removes. Re-check under the
            // lifecycle lock, which the stop handler holds across its claim+teardown: if
            // the colony is back to `Starting` a newer boot/resume claimed it (both share
            // the deterministic sandbox name, so tearing down here could kill its fresh
            // VM), and if it went live a stale boot must not touch the live VM — return
            // in both cases. Otherwise the colony is still not live (Stopped/Failed): reap
            // the orphan (`msb rm --force` on a missing name no-ops).
            let lifecycle = app.session_lock(&id).await;
            let _guard = lifecycle.lock().await;
            let Some(s) = app.session(&id).await else { return };
            if !stale_boot_needs_teardown(s.status) {
                return;
            }
            teardown_vm(&app, &s).await;
            return;
        }
        app.session_log(&id, "error", format!("session failed to start: {message}"))
            .await;
        teardown_vm(&app, &s).await;
        let mut attention = None;
        app.update_session(&id, |s| {
            s.status = SessionStatus::Failed;
            s.error = Some(truncate(&message, 2000));
            attention = s.clear_attention();
        })
        .await;
        app.note_cleared_attention(&id, attention).await;
        // A colony that never got going frees the issue for a retry, on GitHub as well as locally.
        if let Some(s) = app.session(&id).await {
            crate::claims::spawn_release_if_needed(app.clone(), &s);
        }
    }
}

/// Whether a boot that failed after its colony left `Starting` must still reap the microVM: only
/// when the colony is still not live (Stopped/Failed). A colony back to `Starting` was claimed by
/// a newer boot/resume sharing the deterministic sandbox name, and a live one (Running/Idle/…)
/// owns a VM this stale task must not touch.
pub(crate) fn stale_boot_needs_teardown(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Stopped | SessionStatus::Failed)
}

async fn ensure_starting(app: &App, id: &str) -> Result<Session> {
    match app.session(id).await {
        Some(s) if s.status == SessionStatus::Starting => Ok(s),
        _ => bail!("session was stopped while starting"),
    }
}

/// Closes a boot phase and publishes the breakdown so far, so a colony still `starting` shows which
/// phases it has got through, and a boot that fails keeps them. Written without `total_ms`, which
/// only the finished boot carries.
async fn mark_phase(app: &App, id: &str, timing: &mut crate::timing::Phases, name: &str) {
    timing.mark(name);
    let breakdown = timing.progress_json();
    app.update_session(id, |x| x.boot_timing = Some(breakdown)).await;
}

/// The repository's default branch, with access failures worded the way boots report them.
/// Transient blips ride out the boot retry budget first; only a lasting or permanent failure
/// reaches the caller.
async fn default_base(app: &Shared, repo: &str, log: &SessionLogger, started_at: Option<u64>) -> Result<String> {
    let label = format!("resolving the default branch of {repo}");
    let branch = github::with_boot_retry(&label, Some(log), started_at, || github::default_branch(app, repo)).await;
    match branch {
        Ok(base) => Ok(base),
        Err(e) => Err(github::access_error(app, repo, e).await),
    }
}

/// The network fence a colony boots with: the `public` profile alone — never the broad `host`
/// profile, which allows every host-loopback port and would let the untrusted colony agent drive
/// the cockpit API (127.0.0.1:7878) or any other loopback service (#375) — plus exactly the host
/// ports the colony needs: when the mesh is on, the WireGuard direct-path rules (passed in
/// already-awaited because `direct_path_rules` is async and shells out) and the headscale control
/// port; when any model route exists, the provider gateway port. Explicit `--net-rule` entries
/// are matched before the profile rules, so these allows stand and the default deny closes the
/// rest.
pub(crate) fn colony_network(
    mesh: Option<(Vec<String>, u16)>,
    routes: &providers::ColonyRoutes,
    gateway: std::net::SocketAddr,
) -> (Vec<String>, Vec<String>) {
    let mut rules = mesh
        .map(|(direct_path, control)| {
            let mut rules = direct_path;
            rules.push(format!("allow@host:tcp:{control}"));
            rules
        })
        .unwrap_or_default();
    if !routes.routes.is_empty() {
        rules.push(format!("allow@host:tcp:{}", gateway.port()));
    }
    (vec!["public".to_string()], rules)
}

async fn boot_inner(app: &Shared, id: &str, resume: bool) -> Result<()> {
    let log = app.logger(id);
    // Phases close in order and partition the boot; see crates/colonizer/src/timing.rs.
    let mut timing = crate::timing::Phases::new();
    let s = ensure_starting(app, id).await?;
    // An empty breakdown up front, so a boot that stops before its first phase still reads as a boot that stopped.
    app.update_session(id, |x| x.boot_timing = Some(timing.progress_json())).await;
    // The retry clock starts before the first pre-worktree step and is persisted, so a harness
    // restart resumes the same budget instead of starting a new one: `lifecycle::recover`
    // re-queues a boot that died before its worktree existed, carrying this stamp with it.
    let boot_started_at = match s.boot_attempt_started_at {
        Some(started_at) => Some(started_at),
        None => {
            let started_at = github::unix_now();
            app.update_session(id, |x| x.boot_attempt_started_at = Some(started_at)).await;
            Some(started_at)
        }
    };
    let modules = app.modules.read().await.clone();
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == s.agent)
        .cloned()
        .context("agent module is not installed")?;

    let issue = match s.issue {
        Some(number) => {
            let label = format!("fetching issue {}#{number}", s.repo);
            log.info(label.clone()).await;
            let fetched = github::with_boot_retry(&label, Some(&log), boot_started_at, || {
                github::fetch_issue(app, &s.repo, number)
            })
            .await;
            match fetched {
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
    let base = match stack::boot_base(
        s.base.clone().filter(|_| resume),
        s.parent.as_deref(),
        parent.as_ref(),
        s.stack,
    ) {
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
        stack::BootBase::Default => default_base(app, &s.repo, &log, boot_started_at).await?,
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

    mark_phase(app, id, &mut timing, "issue").await;

    let bare = app.bare_repo(&s.repo);
    let wt = PathBuf::from(&s.worktree);
    let admin = if resume {
        // The worktree and branch outlive the microVM, so a resumed colony picks them up as they are.
        log.info(format!("resuming on the kept worktree, branch {}", s.branch)).await;
        PathBuf::from(s.git_admin_dir.as_deref().context("this colony has no worktree to resume")?)
    } else {
        let lock = app.repo_lock(&s.repo).await;
        let _guard = lock.lock().await;
        github::with_boot_retry(
            &format!("syncing the local clone of {}", s.repo),
            Some(&log),
            boot_started_at,
            || github::sync_repo(app, &s.repo, &bare, &log),
        )
        .await?;
        log.info(format!("creating worktree on branch {} from origin/{base}", s.branch))
            .await;
        github::with_boot_retry(
            &format!("creating the worktree for branch {}", s.branch),
            Some(&log),
            boot_started_at,
            || github::create_worktree(app, &bare, &wt, &s.branch, &base),
        )
        .await?
    };
    // A stacked child branched from `origin/<base>` just now; record the sha before later prunes
    // delete the parent's ref, so the publish-time restack knows which commits are the child's own.
    // Best effort: without it the restack falls back to the merge-base while the ref survives.
    let stack_fork = if !resume && stacked_on.is_some() {
        github::fork_sha(app, &bare, &base).await.ok()
    } else {
        None
    };
    app.update_session(id, |x| {
        x.git_admin_dir = Some(admin.display().to_string());
        if stack_fork.is_some() {
            x.stack_fork = stack_fork;
        }
        // The first durable artifact is down; the retry clock has nothing left to budget.
        x.boot_attempt_started_at = None;
    })
    .await;
    let s = ensure_starting(app, id).await?;

    // The worktree exists by now — freshly checked out or kept from before — so a configured `auto`
    // reads the repository this colony will actually work in, and the choice is on the session log
    // before the VM boots. `org_settings` reads the orgs file with blocking IO; it moves up with the
    // resolution because the org's own pin decides what detection is even asked.
    let org_settings = app.org_settings(&s.org);
    let sandbox_schema = schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let stack = resolve_stack(&modules, &sandbox_schema, &org_settings, &wt, &log).await;

    mark_phase(app, id, &mut timing, "git").await;

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
    // A model the operator named at launch beats routing: it is what they asked this colony to run.
    if let Some(model) = s.model_override.as_deref() {
        runner_env.insert("COLONIZER_MODEL".into(), Value::String(model.into()));
    }
    if let Some(model) = s.subagent_model_override.as_deref() {
        runner_env.insert("COLONIZER_SUBAGENT_MODEL".into(), Value::String(model.into()));
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
    // Recorded on the session because the gateway needs it long after boot: every proxied call is
    // checked against this set (issue #409), so a colony's token opens only these providers.
    app.update_session(id, |x| {
        x.allowed_providers = Some(used.iter().map(|p| p.id.clone()).collect())
    })
    .await;
    let probes = futures_util::future::join_all(used.iter().map(|p| crate::gateway::probe_cached(app, p))).await;
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
    mark_phase(app, id, &mut timing, "providers").await;

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
        let mut resolved = Vec::new();
        for name in &plugin_names {
            // The operator's data directory first, then what shipped with the app; the same resolution the
            // skillset list in Settings shows (plugins.rs).
            let source = crate::plugins::resolve(&app.cfg, name)?;
            resolved.push((name.as_str(), source.clone()));
            if let Some(vendored) = crate::plugins::shadowed_vendored(&app.cfg, name) {
                log.info(format!(
                    "skillset {name:?}: the local copy at {} shadows the vendored one at {}",
                    source.display(),
                    vendored.display()
                ))
                .await;
            }
            let target = format!("/opt/colonizer/plugins/{name}");
            mounts.push(Mount {
                source,
                target: target.clone(),
                read_only: true,
            });
            targets.push(target);
        }
        // Two packs answering to the same skill name are ambiguous by
        // construction (docs/skill-packs.md): bail naming both packs.
        crate::plugins::check_skill_uniqueness(&resolved)?;
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
    // Remembered for the secrets below: msb keeps the value host-side and the guest env only
    // holds a placeholder, swapped at the TLS edge for the listed hosts.
    let mut jev_key: Option<String> = None;
    if switched_on(&runner_env, "COLONIZER_JEV_COMPACTION") {
        match jev_compaction(
            app.cfg
                .asset("vendor/fast-jev-compaction/hooks/hooks.json")
                .and_then(|_| app.cfg.asset("vendor/fast-jev-compaction"))
                .ok(),
            crate::jev::api_key(),
        ) {
            Ok((source, key)) => {
                mounts.push(Mount {
                    source,
                    target: "/opt/colonizer/jev-compaction".into(),
                    read_only: true,
                });
                jev_key = Some(key);
            }
            Err(reason) => {
                runner_env.remove("COLONIZER_JEV_COMPACTION");
                log.info(reason).await;
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
    // The vendored node runtime, mounted read-only only when the agent's in-VM command
    // starts with `node` (the claude-code module's `["node","runner.mjs"]` entry, rewritten by
    // `vm_command()` to `["node","/opt/colonizer/agent/runner.mjs"]`). Unlike the token-saving
    // mounts above, a missing or invalid binary fails the boot through the usual `boot_inner`
    // error path: without node the agent cannot start at all.
    if agent_needs_node(&agent.vm_command()) {
        let source = app
            .cfg
            .linux_binary("bin/node-guest")
            .context("node runtime bin/node-guest is missing or unusable; run scripts/install.sh")?;
        mounts.push(node_mount(source));
    }
    let mut secrets = Vec::new();
    if agent.needs_claude {
        // The colony's own account, recorded at launch — never the install default by accident.
        let account = s.claude_account.clone().unwrap_or_else(|| "default".into());
        let cred = app
            .claude_cred_for(s.claude_account.as_deref())
            .with_context(|| format!("log in with Claude in Settings first (account '{account}')"))?;
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
    if let Some(key) = jev_key {
        // The hook reads TYPESAFE_API_KEY; the guest sees only msb's placeholder for it.
        secrets.push(Secret {
            env: "TYPESAFE_API_KEY".into(),
            value: key,
            hosts: vec!["api.typesafe.ai".into()],
        });
    }
    let mut publish = None;
    // The mesh half of the network fence is captured here — `direct_path_rules` is async (it
    // shells out to `ip`/`ifconfig`) — and the fence itself is decided, purely, in
    // `colony_network` once routing is known.
    let mut mesh_net = None;
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
        // Reach the host only where the colony must — the headscale control port and the WireGuard
        // direct path — not every loopback service; see `colony_network` for the fence (#375).
        mesh_net = Some((mesh.direct_path_rules().await, mesh.ports().control));
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
    let (net_profiles, net_rules) = colony_network(mesh_net, &routing, app.cfg.gateway_bind);

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
    mark_phase(app, id, &mut timing, "mesh-start").await;

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
    mark_phase(app, id, &mut timing, "image-pull").await;

    log.info(format!(
        "booting microVM {} ({}, {} vCPU, {})",
        spec.name, spec.image, spec.cpus, spec.memory
    ))
    .await;
    sandbox::boot(&app.cfg.msb, &spec).await?;
    // The pull is its own phase above, so this is the VM itself — unless the
    // pre-pull failed, in which case `msb run` pulls and this absorbs it.
    mark_phase(app, id, &mut timing, "vm-boot").await;
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

    mark_phase(app, id, &mut timing, "mesh-join").await;

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

/// Whether the agent needs the vendored node runtime mounted: its in-VM command starts with
/// `node`. Takes the resolved [`AgentModule::vm_command`] rather than the module, so the rule is
/// testable without a module directory on disk.
pub(crate) fn agent_needs_node(command: &[String]) -> bool {
    command.first().is_some_and(|arg| arg == "node")
}

/// The read-only mount exposing the vendored node binary at its in-VM path.
pub(crate) fn node_mount(source: PathBuf) -> Mount {
    Mount {
        source,
        target: "/opt/node/bin/node".into(),
        read_only: true,
    }
}

/// Decides the Jev compaction switch for one colony: the staged payload and the mothership's
/// TypeSafe key both have to be there. Takes the probe result rather than the app, so tests cover
/// it without a colony on disk. Ok carries the mount source and the key the secret below needs.
pub(crate) fn jev_compaction(
    payload: Option<PathBuf>,
    key: Option<String>,
) -> std::result::Result<(PathBuf, String), &'static str> {
    let source = payload.ok_or(
        "Jev compaction is switched on, but fast-jev-compaction isn't installed (scripts/install.sh stages it); running without it",
    )?;
    let key =
        key.ok_or("Jev compaction is switched on, but the mothership has no TypeSafe key (set JEV_API_KEY); running without it")?;
    Ok((source, key))
}

const BOOT_SCRIPT: &str = r#"#!/bin/sh
# Generated by colonizer. Runs as the microVM's main process.
set -u
mkdir -p /var/lib/colonizer
# Git metadata is mounted read-only; give git a private, writable index.
if [ -f "${GIT_DIR:-}/index" ]; then cp "$GIT_DIR/index" "$GIT_INDEX_FILE"; fi
export PATH="/opt/node/bin:/opt/claude/bin:$PATH"
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

pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<diagnosis::SessionDetail> {
    let session = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(diagnosis::for_session(&app, with_activity(&app, session).await).await))
}

#[derive(Deserialize)]
pub struct SinceQuery {
    since: Option<u64>,
    epoch: Option<u64>,
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
    Ok(ws.on_upgrade(move |socket| events_socket(app, id, rt, query.since.unwrap_or(0), query.epoch, socket)))
}

/// One replayable line of events.jsonl: the decoded line and its seq, or `None` to skip it.
/// Decoded one chunk at a time, as `Runtime::load` reads the same file: a non-UTF-8 line or a
/// line outside the JSON contract costs itself, not the rest of the transcript.
fn replay_line(chunk: &[u8]) -> Option<(u64, &str)> {
    let line = std::str::from_utf8(chunk).ok()?;
    let seq = serde_json::from_str::<Value>(line).ok().and_then(|v| v["seq"].as_u64())?;
    Some((seq, line))
}

async fn events_socket(app: Shared, id: String, rt: Arc<Runtime>, since: u64, client_epoch: Option<u64>, socket: WebSocket) {
    // Before this socket subscribes and the log ring is drained, so its alert lands in the
    // drained history once instead of arriving twice.
    app.report_load_error(&id, &rt).await;
    let (mut tx, mut rx) = socket.split();
    let mut subscription = rt.events.subscribe();
    let mut retired = rt.retired.subscribe();
    let text = |s: String| Message::Text(s.into());

    // First frame on the wire, before the session frame and any replay: the run epoch this
    // connection is attached to, so a tab left open across a resume learns its cursor belongs to
    // a retired run. It carries no `seq` field, so it passes seq filtering like `harness_log`,
    // and old clients ignore the unknown frame.
    let current_epoch = run_epoch_for_dir(&app.session_dir(&id));
    if tx
        .send(text(json!({"type": "run_epoch", "epoch": current_epoch}).to_string()))
        .await
        .is_err()
    {
        return;
    }
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
    // A `since` from a retired run is a rank in that run's per-run numbering, meaningless in the
    // new run: replay from the epoch-adjusted cursor instead, and seed the live dedupe cursor
    // with it so the new run's first events are neither dropped nor duplicated.
    let effective = effective_since(client_epoch, current_epoch, since);
    let mut replayed = effective;
    // Byte-split, never `BufReader::lines()`: `next_line()` reports a non-UTF-8 line as an
    // `InvalidData` error, which reads exactly like EOF here and would silently truncate the
    // replay at the first corrupt line. Split chunks decode (or skip) one at a time instead.
    let mut skipped = 0u64;
    if let Ok(bytes) = tokio::fs::read(&rt.events_path).await {
        for chunk in bytes.split(|b| *b == b'\n') {
            if chunk.is_empty() {
                continue;
            }
            let Some((seq, line)) = replay_line(chunk) else {
                skipped += 1;
                continue;
            };
            if seq > effective {
                if tx.send(text(line.to_string())).await.is_err() {
                    return;
                }
                replayed = replayed.max(seq);
            }
        }
    }
    if skipped > 0 {
        // A gap in the event log must not be silent: the entry lands in the harness log and on
        // the socket (via session_log's broadcast, picked up by the live loop below) so the
        // transcript visibly continues past the gap.
        app.session_log(
            &id,
            "warn",
            format!(
                "skipped {} unreadable {} in {} during replay; the transcript continues past the gap",
                skipped,
                if skipped == 1 { "line" } else { "lines" },
                rt.events_path.display(),
            ),
        )
        .await;
    }
    // The backlog is on the wire: the browser holds its render until this frame, so a long
    // history opens on its latest messages instead of filling in line by line. No `seq`, like
    // `run_epoch`, and old clients ignore the unknown frame.
    if tx
        .send(text(json!({"type": "replay_done", "seq": replayed}).to_string()))
        .await
        .is_err()
    {
        return;
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
            _ = retired.changed() => {
                // The colony resumed: this socket holds the retired run's Runtime and can never
                // see the new run's events, so close and let the client reconnect for the new epoch.
                let _ = tx.send(Message::Close(None)).await;
                return;
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
        Some("set_model") => {
            let Some(model) = set_model_id(command["model"].as_str().unwrap_or_default()) else {
                return;
            };
            json!({"type": "set_model", "model": model})
        }
        _ => return,
    };
    let _ = rt.commands.send(forward);
}

/// The model a `set_model` switches the colony to, trimmed, or `None` to drop the command.
///
/// Only the shape is checked, in the forms a model setting takes (§6.1): a Claude alias or ID,
/// a `[1m]` suffix, `<provider>/<model>`, same characters as a provider's model list. Whether the
/// id resolves is the colony's to find out against the routes it booted with, and a refused switch
/// comes back as a `warn` log.
fn set_model_id(raw: &str) -> Option<&str> {
    // The longest id `/api/models` lists, so every model the picker offers gets through:
    // `<provider id>/<model>`, a provider id of at most 32 bytes (providers.rs `valid_id`) and a
    // model of at most 120 (`valid_model`).
    const MAX_MODEL_ID: usize = 32 + 1 + 120;
    let model = raw.trim();
    let shaped =
        (1..=MAX_MODEL_ID).contains(&model.len()) && model.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/[]".contains(c));
    shaped.then_some(model)
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
    fn a_colony_is_fenced_to_public_with_only_the_host_ports_it_needs() {
        // A non-default gateway port, so a regression to the 41750 fallback shows up as a mismatch.
        let gateway: std::net::SocketAddr = "127.0.0.1:52000".parse().unwrap();
        let control = 41740;
        let wireguard = vec![
            "allow@192.168.1.4:udp:41743".to_string(),
            "allow@10.1.2.3:udp:41743".to_string(),
        ];
        let routed = providers::ColonyRoutes {
            routes: vec![Value::Bool(true)],
            providers: Vec::new(),
        };
        for (mesh_on, has_routes) in [(true, true), (true, false), (false, true), (false, false)] {
            let mesh = mesh_on.then(|| (wireguard.clone(), control));
            let routes = if has_routes {
                &routed
            } else {
                &providers::ColonyRoutes::default()
            };
            let (profiles, rules) = colony_network(mesh.clone(), routes, gateway);
            // `public` alone, never the broad `host` profile (#375) — in every combination, so a
            // future change cannot quietly hand the colony every host-loopback port again.
            assert_eq!(
                profiles,
                vec!["public".to_string()],
                "mesh_on={mesh_on} has_routes={has_routes}"
            );
            assert!(
                !profiles.iter().any(|p| p == "host"),
                "mesh_on={mesh_on} has_routes={has_routes}"
            );
            let mut expected = mesh
                .clone()
                .map(|(mut direct_path, control)| {
                    direct_path.push(format!("allow@host:tcp:{control}"));
                    direct_path
                })
                .unwrap_or_default();
            if has_routes {
                expected.push(format!("allow@host:tcp:{}", gateway.port()));
            }
            assert_eq!(rules, expected, "mesh_on={mesh_on} has_routes={has_routes}");
            // And of the host rules, nothing but the control and gateway ports, TCP only.
            let host_allows = [
                format!("allow@host:tcp:{control}"),
                format!("allow@host:tcp:{}", gateway.port()),
            ];
            for rule in &rules {
                if rule.contains("@host:") {
                    assert!(host_allows.contains(rule), "unexpected host allow {rule:?} in {rules:?}");
                }
            }
        }
    }

    #[test]
    fn set_model_forwards_only_what_a_model_setting_could_hold() {
        for id in [
            "opus",
            "claude-opus-5-5",
            "claude-opus-5-5[1m]",
            "deepseek/deepseek-flash",
            "together/deepseek-ai/DeepSeek-V4.1-Flash",
            "local/qwen3:32b",
        ] {
            assert_eq!(set_model_id(id), Some(id), "{id}");
        }
        assert_eq!(set_model_id("  sonnet\n"), Some("sonnet"), "trimmed");
        assert!(set_model_id(&"m".repeat(128)).is_some());
        // No model id has whitespace, quotes or non-ASCII in it.
        for bad in ["", "   ", "has space", "opus\"}", "opus\n{\"type\":\"shutdown\"}", "claudé"] {
            assert_eq!(set_model_id(bad), None, "{bad:?}");
        }
        // The longest id `/api/models` can list: a 32-byte provider id, `/`, a 120-byte model.
        let longest = format!("{}/{}", "p".repeat(32), "m".repeat(120));
        assert_eq!(set_model_id(&longest), Some(longest.as_str()), "the longest listed id");
        assert_eq!(set_model_id(&format!("{longest}m")), None, "one byte over");
    }

    #[test]
    fn a_stale_boot_only_reaps_a_colony_that_is_still_not_live() {
        use SessionStatus::*;
        // Stopped/Failed mid-boot: the stop's `msb rm` ran before `sandbox::boot`, so the
        // orphaned microVM is this stale task's to reap.
        for status in [Stopped, Failed] {
            assert!(stale_boot_needs_teardown(status), "{status:?} must be reaped");
        }
        // Back to Starting: a newer boot/resume claimed the colony under the same sandbox
        // name — tearing down here could kill its fresh VM.
        assert!(!stale_boot_needs_teardown(Starting));
        // Live now: a stale boot must never touch the live VM.
        for status in [Running, WaitingForAnswer, Idle] {
            assert!(!stale_boot_needs_teardown(status), "{status:?} must be left alone");
        }
        // Terminal without a VM, and queued/publishing which a boot can never stale into:
        // no teardown either way, but never a live VM at risk.
        for status in [Queued, Publishing, PrOpened, Merged, Closed, NoChanges] {
            assert!(!stale_boot_needs_teardown(status), "{status:?}");
        }
    }

    #[test]
    fn total_cost_is_claude_plus_routed() {
        let mut s = colony("acme", SessionStatus::Running);
        assert_eq!(s.total_cost_usd(), 0.0);
        s.cost_usd = Some(1.25);
        assert_eq!(s.total_cost_usd(), 1.25);
        s.routed_cost_usd = Some(0.75);
        assert!((s.total_cost_usd() - 2.0).abs() < 1e-9, "Claude and routed spend add up");
    }

    #[test]
    fn startup_migration_clears_stale_attention_only_on_finished_colonies() {
        fn flagged(status: SessionStatus) -> Session {
            let mut s = colony("acme", status);
            s.attention = Some(json!({"reason": "stalled", "since": Utc::now(), "nudges": 2}));
            s
        }
        let mut sessions = vec![
            flagged(SessionStatus::Stopped),
            flagged(SessionStatus::Failed),
            flagged(SessionStatus::PrOpened),
            flagged(SessionStatus::Running),
            colony("acme", SessionStatus::Stopped),
        ];
        assert_eq!(clear_stale_attention(&mut sessions), 3);
        for s in &sessions[..3] {
            assert!(s.attention.is_none(), "{:?} must not keep a stale attention flag", s.status);
        }
        assert!(
            sessions[3].attention.is_some(),
            "a live colony keeps the flag the watchdog is still managing"
        );
        assert!(
            sessions[4].attention.is_none(),
            "a finished colony without a flag is untouched"
        );
        assert_eq!(clear_stale_attention(&mut sessions), 0, "the migration is idempotent");
    }

    #[test]
    fn startup_migration_keeps_a_quota_parked_colony_resumable() {
        let mut parked = stopped_colony_with_worktree("acme", "parked".into());
        parked.status = SessionStatus::Stopped;
        parked.error = Some("provider quota exhausted (resets 7am (UTC))".into());
        parked.attention =
            Some(json!({"reason": crate::provider_quota::QUOTA_EXHAUSTED_REASON, "since": Utc::now(), "nudges": 0}));
        let mut sessions = vec![parked];
        assert_eq!(
            clear_stale_attention(&mut sessions),
            0,
            "the park reason is the resume ticket, not stale"
        );
        assert_eq!(
            sessions[0].attention.as_ref().and_then(|a| a["reason"].as_str()),
            Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON),
            "a restart must not strand the parked colony"
        );
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

    /// So must one saved before the publish slot flag existed: every publish held its slot back
    /// then, so a bare `publishing` row of unknown origin keeps holding one.
    #[test]
    fn a_publishing_row_saved_before_the_slot_flag_still_holds_its_slot() {
        let saved = r#"{"id":"c","repo":"acme/repo","issue":null,"issue_title":"","status":"publishing","branch":"b","base":null,"worktree":"","git_admin_dir":"git","sandbox":"s","mesh":null,"agent":"a","pr_url":null,"error":null,"cost_usd":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(saved).unwrap();
        assert!(
            s.publishing_holds_slot,
            "unknown origin keeps the old every-publish-holds rule"
        );
        assert!(s.holds_slot());
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
        full.allowed_providers = Some(vec!["acme".into()]);
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
            stack: false,
            stack_fork: None,
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
            merged_at: None,
            pr_opened_at: None,
            ci_state: None,
            publish_stage: None,
            // A bare `publishing` fixture is a live-origin claim, so it holds its slot; tests for
            // a stopped-origin publish flip this off.
            publishing_holds_slot: status == SessionStatus::Publishing,
            needs_rebase: false,
            rebase_orphaned: false,
            queued_behind: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            model_routing: None,
            allowed_providers: None,
            routed_cost_usd: None,
            host_disk_bytes: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            app_slot: None,
            boot_attempt_started_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn overlap_queueing_holds_the_oldest_live_same_repo_colony_with_a_worktree() {
        fn holder(id: &str, repo: &str, status: SessionStatus, ago_secs: i64) -> Session {
            let mut s = colony("acme", status);
            s.id = id.into();
            s.repo = repo.into();
            s.git_admin_dir = Some("git".into());
            s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
            s
        }
        let sessions = vec![
            holder("new", "acme/repo", SessionStatus::Running, 10),
            holder("old", "acme/repo", SessionStatus::Running, 100),
            holder("other-repo", "acme/other", SessionStatus::Running, 200),
            holder("published", "acme/repo", SessionStatus::PrOpened, 300),
            holder("queued", "acme/repo", SessionStatus::Queued, 400),
        ];
        assert_eq!(
            overlap_holder(&sessions, "acme/repo").as_deref(),
            Some("old"),
            "the oldest live holder wins"
        );
        assert_eq!(overlap_holder(&sessions, "acme/other").as_deref(), Some("other-repo"));
        assert_eq!(overlap_holder(&sessions, "acme/empty"), None, "no live worktree, no holder");
        // A live colony that never booted a worktree holds nothing back.
        let mut no_worktree = holder("wt-less", "acme/repo", SessionStatus::Running, 500);
        no_worktree.git_admin_dir = None;
        let mut only_quiet = vec![no_worktree];
        only_quiet.extend(sessions.into_iter().filter(|s| s.repo != "acme/repo" || !s.status.is_live()));
        assert_eq!(overlap_holder(&only_quiet, "acme/repo"), None);
    }

    #[test]
    fn an_overlap_queued_colony_stays_queued_until_its_holder_finishes() {
        fn queued_behind(holder: &str) -> Session {
            let mut s = colony("acme", SessionStatus::Starting);
            s.id = "new".into();
            s.repo = "acme/repo".into();
            s.queued_behind = Some(holder.into());
            s
        }
        let mut live_holder = colony("acme", SessionStatus::Running);
        live_holder.id = "holder".into();
        live_holder.repo = "acme/repo".into();
        // Room and no parent, yet queued: the live holder keeps it waiting, and the pointer stays.
        let mut sessions = vec![live_holder.clone()];
        let (admitted, queued, _) =
            try_claim_session(&mut sessions, true, queued_behind("holder"), "acme/repo", None, false, false)
                .expect("no issue race");
        assert!(
            queued && admitted.status == SessionStatus::Queued,
            "held behind the live colony"
        );
        assert_eq!(admitted.queued_behind.as_deref(), Some("holder"));
        // The holder published: the same pointer releases at once, pointing nowhere stale.
        live_holder.status = SessionStatus::PrOpened;
        let mut sessions = vec![live_holder];
        let (admitted, queued, _) =
            try_claim_session(&mut sessions, true, queued_behind("holder"), "acme/repo", None, false, false)
                .expect("no issue race");
        assert!(
            !queued && admitted.status == SessionStatus::Starting,
            "released once the holder finished"
        );
        assert_eq!(admitted.queued_behind, None);
    }

    /// A `create` request with nothing but the repo and, where the test names one, whether to opt
    /// into overlap-aware queueing.
    fn overlap_request(repo: &str, serialize: Option<bool>) -> Json<NewSession> {
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
            claude_account: None,
            after: None,
            stack: false,
            origin: None,
            serialize,
        })
    }

    /// Issue #453's review: overlap-aware queueing must be opt-in. The same live, touched-file
    /// holder is on the nest both times — only whether the request carries `serialize: true`
    /// decides whether the newcomer queues behind it.
    #[tokio::test]
    async fn overlap_queueing_only_applies_when_the_request_opts_in_with_serialize() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);

        // A live same-repo colony with a real worktree and an untracked file: `touched_files`
        // reads that as something touched via `git status --porcelain`, without needing a remote
        // or any commits.
        let worktree = root.join("holder-worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&worktree)
            .status()
            .expect("git init");
        std::fs::write(worktree.join("touched.txt"), "x").unwrap();

        let mut holder = colony("acme", SessionStatus::Running);
        holder.id = "holder".into();
        holder.repo = "acme/app".into();
        holder.git_admin_dir = Some("git".into());
        holder.worktree = worktree.to_string_lossy().to_string();
        holder.created_at = Utc::now() - chrono::Duration::seconds(100);
        app.sessions.write().await.push(holder);

        // No `serialize` at all: the default stays off, so the newcomer never even scans for an
        // overlap and starts unheld.
        let created = create(State(app.clone()), overlap_request("acme/app", None))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(
            created.queued_behind, None,
            "overlap queueing is opt-in; a plain launch never scans for it"
        );

        // `serialize: false` reads the same as absent.
        let created = create(State(app.clone()), overlap_request("acme/app", Some(false)))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(created.queued_behind, None, "an explicit false is still off");

        // `serialize: true`: the same live, touched-file colony now holds the newcomer behind it.
        let created = create(State(app.clone()), overlap_request("acme/app", Some(true)))
            .await
            .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
        assert_eq!(
            created.queued_behind.as_deref(),
            Some("holder"),
            "serialize: true asks to queue behind a live colony with touched files"
        );
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "queued behind the live holder rather than starting alongside it"
        );

        let _ = std::fs::remove_dir_all(root);
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
        admit_create_in(sessions, &format!("{org}/repo"), max_parallel, org_limit, 32, id).await
    }

    /// `admit_create` for a colony of a named repository, under a per-repository limit as well.
    pub(crate) async fn admit_create_in(
        sessions: &RwLock<Vec<Session>>,
        repo: &str,
        max_parallel: usize,
        org_limit: Option<u64>,
        repo_limit: u64,
        id: String,
    ) {
        let org = repo.split_once('/').map_or(repo, |(org, _)| org);
        with_slot(sessions, org, repo, max_parallel, org_limit, repo_limit, |sessions, room| {
            let mut s = colony(
                org,
                if room {
                    SessionStatus::Starting
                } else {
                    SessionStatus::Queued
                },
            );
            s.id = id;
            s.repo = repo.into();
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
        let repo = format!("{org}/repo");
        with_slot(sessions, org, &repo, max_parallel, org_limit, 32, |sessions, room| {
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
    async fn update_session_with_noop_closure_leaves_updated_at_and_disk_alone() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        // Seed sessions.json so a write would be observable.
        app.persist_sessions().await.unwrap();
        let disk_before = std::fs::read_to_string(app.sessions_file()).unwrap();
        // updated_at only has second precision on disk in spirit; sleep past any clock bump so
        // a spurious bump could not hide behind timestamp granularity.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (after, ()) = app.update_session("abc", |_| {}).await.unwrap();
        assert_eq!(
            after.updated_at, before.updated_at,
            "a closure that changes nothing must not bump updated_at"
        );
        let disk_after = std::fs::read_to_string(app.sessions_file()).unwrap();
        assert_eq!(disk_after, disk_before, "a no-op update must not rewrite sessions.json");
        assert!(
            app.storage_alert.read().await.is_none(),
            "skipping the write must not raise a storage alert"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn update_session_with_real_change_bumps_updated_at_and_persists() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (after, ()) = app.update_session("abc", |s| s.status = SessionStatus::Idle).await.unwrap();
        assert_eq!(after.status, SessionStatus::Idle);
        assert!(after.updated_at > before.updated_at, "a real change must bump updated_at");
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(app.sessions_file()).unwrap()).unwrap();
        let saved = disk.as_array().unwrap().iter().find(|v| v["id"] == "abc").unwrap();
        assert_eq!(saved["status"], "idle", "a real change must reach sessions.json");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn record_measurement_stores_the_number_without_bumping_updated_at() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        assert_eq!(before.host_disk_bytes, None);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        app.record_measurement("abc", |s| s.host_disk_bytes = Some(1024)).await;
        let after = app.session("abc").await.unwrap();
        assert_eq!(after.host_disk_bytes, Some(1024));
        assert_eq!(
            after.updated_at, before.updated_at,
            "a measurement is not activity and must not read as liveness"
        );
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(app.sessions_file()).unwrap()).unwrap();
        let saved = disk.as_array().unwrap().iter().find(|v| v["id"] == "abc").unwrap();
        assert_eq!(
            saved["host_disk_bytes"], 1024,
            "the measurement must still reach sessions.json"
        );
        // A second identical measurement writes nothing.
        let disk_before = std::fs::read_to_string(app.sessions_file()).unwrap();
        app.record_measurement("abc", |s| s.host_disk_bytes = Some(1024)).await;
        assert_eq!(
            std::fs::read_to_string(app.sessions_file()).unwrap(),
            disk_before,
            "an unchanged measurement must not rewrite sessions.json"
        );
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

    #[test]
    fn a_non_utf8_replay_line_costs_only_itself() {
        // The events_socket replay reads this way: byte-split, one tolerant line at a time.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}\n");
        // A line torn mid-write, inside the first byte of an em-dash, mid-file this time.
        bytes.extend_from_slice(b"{\"seq\":2,\"type\":\"log\",\"message\":\"restarting \xE2\"}\n");
        bytes.extend_from_slice(b"this line is not json\n");
        bytes.extend_from_slice(b"{\"seq\":3,\"type\":\"status\",\"state\":\"idle\"}\n");
        let replayed: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|chunk| !chunk.is_empty())
            .filter_map(|chunk| replay_line(chunk).map(|(seq, _)| seq))
            .collect();
        assert_eq!(
            replayed,
            vec![1, 3],
            "the corrupt and non-JSON lines are skipped; the replay reaches past them"
        );
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
    fn a_stopped_or_failed_colony_frees_its_issue_for_an_explicit_retry() {
        // The sweep above covers every terminal state; stopped and failed get their own assertion
        // because they are the retries that matter — a run that died halfway, not one that shipped.
        for status in [SessionStatus::Stopped, SessionStatus::Failed] {
            let sessions = vec![on_issue("dead", 7, status)];
            assert!(
                issue_held_by(&sessions, "acme/repo", 7).is_none(),
                "{} is done with #7, so a retry is not a duplicate",
                status.as_str()
            );
            // And the atomic claim the handler admits with lets that retry through.
            let mut sessions = sessions;
            let mut retry = colony("acme", SessionStatus::Starting);
            retry.id = "retry".into();
            retry.issue = Some(7);
            let claimed = try_claim_session(&mut sessions, true, retry, "acme/repo", Some(7), false, false);
            assert!(claimed.is_ok(), "a retry after {} is admitted, not refused", status.as_str());
        }
    }

    #[test]
    fn allow_duplicate_bypasses_the_hold_that_blocks_a_second_claim() {
        // The holder here is still live, unlike the finished colonies above: without
        // `allow_duplicate` the claim is refused with the holder handed back; passing
        // `allow_duplicate: true` for the same issue is admitted anyway.
        let mut sessions = vec![on_issue("first", 7, SessionStatus::Running)];

        let mut blocked = colony("acme", SessionStatus::Starting);
        blocked.id = "blocked".into();
        blocked.issue = Some(7);
        let refused = try_claim_session(&mut sessions, true, blocked, "acme/repo", Some(7), false, false);
        assert!(
            matches!(&refused, Err(held) if held.id == "first"),
            "a live holder refuses a second claim without allow_duplicate"
        );
        assert_eq!(sessions.len(), 1, "the refused claim inserted nothing");

        let mut second = colony("acme", SessionStatus::Starting);
        second.id = "second".into();
        second.issue = Some(7);
        let admitted = try_claim_session(&mut sessions, true, second, "acme/repo", Some(7), true, false);
        assert!(
            admitted.is_ok(),
            "allow_duplicate lets a second colony start on an issue another still holds"
        );
        assert_eq!(sessions.len(), 2, "the admitted duplicate is inserted alongside the holder");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[allow(clippy::result_large_err)]
    async fn two_simultaneous_claims_on_one_issue_let_exactly_one_through() {
        // The TOCTOU window this guards: two launches both passing the read-locked pre-check before
        // either inserts. Both collide here inside the write lock instead, through the same
        // `try_claim_session` the handler admits with — one is admitted, the other gets its holder.
        let sessions = std::sync::Arc::new(RwLock::new(Vec::new()));
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = Vec::new();
        for i in 0..2 {
            let (sessions, barrier) = (sessions.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let mut fresh = colony("acme", SessionStatus::Starting);
                fresh.id = format!("racer-{i}");
                fresh.issue = Some(7);
                with_slot(&sessions, "acme", "acme/repo", 8, None, 8, |guard, room| {
                    try_claim_session(guard, room, fresh, "acme/repo", Some(7), false, false)
                })
                .await
            }));
        }
        let mut admitted = 0;
        let mut refused = 0;
        for task in tasks {
            match task.await.expect("claim task joined") {
                Ok(_) => admitted += 1,
                Err(_) => refused += 1,
            }
        }
        assert_eq!(admitted, 1, "exactly one racer is admitted");
        assert_eq!(refused, 1, "the other gets the holder back for its 409");
        let done = sessions.read().await;
        assert_eq!(done.len(), 1, "the loser inserted nothing");
        let held = issue_held_by(&done, "acme/repo", 7).expect("the winner holds #7");
        assert!(
            held.id == "racer-0" || held.id == "racer-1",
            "the holder is the admitted racer, not a stranger: {}",
            held.id
        );
        // And the loser's 409 reads the way the handler's does.
        let message = duplicate_message(&held, 7);
        assert!(
            message.contains(&format!("colony {} is already on #7", held.id)) && message.contains("allow_duplicate"),
            "{message}"
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

    #[test]
    fn a_question_still_open_on_disk_is_restored_on_load() {
        let dir = std::env::temp_dir().join(format!("colonizer-open-question-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let question = r#"{"seq":3,"ts":"2026-09-21T09:30:00.000Z","type":"question","question_id":"q1","questions":[{"header":"pin","options":[]}]}"#;

        // Asked and never answered: the question is open, and its wait started when it was asked.
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}}\n{question}\n"),
        )
        .unwrap();
        let rt = Runtime::load(&dir);
        let (id, questions) = rt
            .open_question
            .try_lock()
            .unwrap()
            .clone()
            .expect("the question is still open");
        assert_eq!(id, "q1");
        assert_eq!(questions, vec![json!({"header": "pin", "options": []})]);
        assert_eq!(
            rt.activity.try_lock().unwrap().question_since,
            Some("2026-09-21T09:30:00Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(rt.agent_seq.load(Ordering::SeqCst), 3, "the cursor pass is unchanged");

        // Asked and then answered: nothing is open.
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{question}\n{{\"seq\":4,\"type\":\"question_answered\",\"question_id\":\"q1\",\"answers\":{{}}}}\n"),
        )
        .unwrap();
        let rt = Runtime::load(&dir);
        assert!(rt.open_question.try_lock().unwrap().is_none());
        assert!(rt.activity.try_lock().unwrap().question_since.is_none());
        assert_eq!(rt.agent_seq.load(Ordering::SeqCst), 4);
        let _ = std::fs::remove_dir_all(dir);
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
                model_override: None,
                subagent_model_override: None,
                claude_account: None,
                after: None,
                stack: false,
                origin: None,
                serialize: None,
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
                model_override: None,
                subagent_model_override: None,
                claude_account: None,
                after: None,
                stack: false,
                origin: None,
                serialize: None,
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
    /// Unstacked unless the test says otherwise: the default queues behind the parent's merge.
    fn stack_request(repo: &str, after: Option<String>, stack: bool) -> Json<NewSession> {
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
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            after,
            stack,
            origin: None,
            serialize: None,
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

        let created = create(State(app.clone()), stack_request("acme/app", Some("parent".into()), true))
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

    /// A parent colony for the queue-by-default tests below: open issue, same repository.
    async fn parent_on(app: &Shared, status: SessionStatus) {
        let mut parent = colony("acme", status);
        parent.id = "parent".into();
        parent.branch = "colonizer/issue-1-parent".into();
        parent.repo = "acme/app".into();
        app.sessions.write().await.push(parent);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_an_open_pull_request_queues_for_the_merge() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::PrOpened).await;

        let created = create(State(app.clone()), stack_request("acme/app", Some("parent".into()), false))
            .await
            .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
        assert_eq!(created.parent.as_deref(), Some("parent"));
        assert!(!created.stack, "queueing, not stacking, is the default");
        assert_eq!(
            created.status,
            SessionStatus::Queued,
            "the parent's pull request is still open, so the colony queues even though a slot is free"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_explicit_stack_starts_from_the_open_pull_request() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::PrOpened).await;

        let created = create(State(app.clone()), stack_request("acme/app", Some("parent".into()), true))
            .await
            .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
        assert_eq!(created.parent.as_deref(), Some("parent"));
        assert!(created.stack);
        assert_eq!(
            created.status,
            SessionStatus::Starting,
            "the parent's branch is pushed, so an explicit stack starts at once"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_a_merged_parent_starts_at_once() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::Merged).await;

        let created = create(State(app.clone()), stack_request("acme/app", Some("parent".into()), false))
            .await
            .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
        assert_eq!(
            created.status,
            SessionStatus::Starting,
            "the parent's work is already merged, so there is nothing to wait for"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn by_default_a_colony_behind_a_closed_parent_is_refused_naming_the_stack_flag() {
        let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
        let app = app_that_can_create(&root);
        parent_on(&app, SessionStatus::Closed).await;

        let err = create(State(app.clone()), stack_request("acme/app", Some("parent".into()), false))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        let message = err.1.to_string();
        assert!(message.contains("parent"), "{message}");
        assert!(
            message.contains("stack: true"),
            "the refusal says how to stack anyway: {message}"
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

        let err = create(State(app.clone()), stack_request("acme/app", Some("dead".into()), true))
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
        let err = create(State(app.clone()), stack_request("acme/app", Some("ghost".into()), true))
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
        let err = create(State(app.clone()), stack_request("acme/app", Some("   ".into()), true))
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

        let err = create(State(app.clone()), stack_request("acme/other", Some("parent".into()), true))
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

    /// The node runtime mounts exactly when the agent's in-VM command starts with `node` — the
    /// claude-code module's `["node","runner.mjs"]` entry once `vm_command()` rewrites the script
    /// to its in-VM path — and the mount exposes the binary read-only at its in-VM path.
    #[test]
    fn node_runtime_mounts_only_for_node_agents() {
        let node_cmd = vec!["node".into(), "/opt/colonizer/agent/runner.mjs".into()];
        assert!(agent_needs_node(&node_cmd), "a node entrypoint needs the runtime");
        let mount = node_mount(PathBuf::from("/dist/bin/node-guest"));
        assert_eq!(mount.target, "/opt/node/bin/node");
        assert!(mount.read_only, "the colony must not rewrite its own runtime");

        assert!(
            !agent_needs_node(&["python3".into(), "runner.py".into()]),
            "a non-node agent boots exactly as before, with no new failure mode"
        );
        assert!(!agent_needs_node(&[]), "an empty command needs nothing mounted");
    }

    /// The boot script puts the node bin dir first and keeps the claude entry as-is.
    #[test]
    fn boot_script_puts_node_first() {
        assert!(
            BOOT_SCRIPT.contains(r#"export PATH="/opt/node/bin:/opt/claude/bin:$PATH""#),
            "node first, claude entry unchanged"
        );
    }

    /// The Jev compaction switch mounts the payload only with the staged files and a key to go with them.
    #[test]
    fn jev_compaction_mounts_only_with_a_payload_and_a_key() {
        let payload = Some(PathBuf::from("/dist/vendor/fast-jev-compaction"));
        let (source, key) =
            jev_compaction(payload.clone(), Some("ts-key".into())).expect("a payload and a key switch compaction on");
        assert_eq!(source, payload.clone().unwrap());
        assert_eq!(key, "ts-key");
        assert_eq!(
            jev_compaction(None, Some("ts-key".into())).unwrap_err(),
            "Jev compaction is switched on, but fast-jev-compaction isn't installed (scripts/install.sh stages it); running without it"
        );
        assert_eq!(
            jev_compaction(payload, None).unwrap_err(),
            "Jev compaction is switched on, but the mothership has no TypeSafe key (set JEV_API_KEY); running without it"
        );
    }
}
