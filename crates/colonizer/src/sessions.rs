//! Interactive sessions: one worktree + microVM + agent per task, bridged to browsers.
//!
//! Harness ⇄ VM traffic goes to `colonizer-agentd` over the private mesh (or a loopback port when the
//! mesh module is disabled). Agent events are persisted per session and fanned out to every open
//! browser; browser commands are forwarded to the agent.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModulesConfig, setting, setting_str},
    diagnosis, github,
    modules::{AgentModule, schema_for},
    orgs,
    protocol::QuestionRisk,
    restack, spend,
    stack::Stacked,
    store::SessionStore,
    util::{append_line, read_trimmed, short_id, truncate, valid_repo},
    watchdog::Activity,
};
// The boot sequence itself lives in boot.rs; re-exported here because lifecycle and queue reach
// `boot` through their `sessions::*` glob imports.
pub(crate) use crate::boot::boot;
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

pub(crate) const AGENTD_PORT: u16 = 7070;
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

/// How many changed paths a colony keeps: enough to place it in a monorepo's packages, bounded so a
/// sweeping change cannot bloat sessions.json.
pub const CHANGED_PATHS_CAP: usize = 500;

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
    /// The files the colony's pull request changed (first [`CHANGED_PATHS_CAP`]), read from GitHub
    /// once the PR is open and again when it merges; empty until then. The cockpit maps them to a
    /// monorepo's packages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    /// The task in one plain sentence (≤ 120 chars), written by a cheap model from the issue or
    /// instructions after launch and again from the pull request once it opens (`summaries.rs`).
    /// `None` until then, or when summaries are off or the model could not be reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
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
    /// Whether this colony is a waiter in the successor queue for its issue (issue #321): admitted
    /// `Queued` behind the colony holding the issue instead of refused, it starts only when the
    /// issue's holder is its own. False the moment it is promoted; the holder's claim on GitHub
    /// stays the waiter's whole wait.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim_wait: bool,
    /// How this colony's completion claims are verified (issue #328): the `verify` configuration
    /// resolved at launch — `auto` (the default), `none`, or an explicit test command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// The verdict attached to the colony's last completion claim, and everything behind it.
    /// `None` until a claim has been verified (verify.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<crate::verify::Verification>,
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
    /// The image this colony's microVM booted from, exactly as `msb run` received it: the one a
    /// verification's fresh-checkout run reuses. `null` on colonies booted before the field
    /// existed, which fall back to the configured image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_image: Option<String>,
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
            changed_paths: Vec::new(),
            ci_state: None,
            summary: None,
            publish_stage: None,
            publishing_holds_slot: false,
            needs_rebase: false,
            rebase_orphaned: false,
            queued_behind: None,
            claim_wait: false,
            verify: None,
            verification: None,
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
            boot_image: None,
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
    /// The open question: its id, the questions themselves — which autonomous mode needs to answer
    /// among the options the agent offered — and the question's risk class, which its ceiling reads.
    pub(crate) open_question: Mutex<Option<(String, Vec<Value>, QuestionRisk)>>,
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
    /// Serialises completion-claim verifications (verify.rs): a second claim that lands mid-run
    /// queues behind it and then verifies the newer state, never concurrent with it.
    pub(crate) verify_lock: Mutex<()>,
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
    /// The question a colony is waiting on, if it is waiting on one, with its risk class.
    pub(crate) async fn open_question(&self) -> Option<(String, Vec<Value>, QuestionRisk)> {
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
        // The replayed open question: its id, its questions, when it was asked, its risk class.
        type Replayed = (String, Vec<Value>, Option<DateTime<Utc>>, QuestionRisk);
        let mut open_question: Option<Replayed> = None;
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
                    let risk = QuestionRisk::from_wire(v.get("risk"));
                    open_question = Some((id.to_string(), questions, asked, risk));
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
                    .map(|(id, questions, _, risk)| (id.clone(), questions.clone(), *risk)),
            ),
            pr_mark: Mutex::new(github::pr_description_mark(&dir.join("out"))),
            interrupted: std::sync::atomic::AtomicBool::new(false),
            stop: watch::channel(false).0,
            retired: watch::channel(false).0,
            file_lock: Mutex::new(()),
            findings_lock: Mutex::new(()),
            verify_lock: Mutex::new(()),
            events_path,
            logs_path,
            activity: Mutex::new({
                let now = Utc::now();
                let mut activity = Activity::new(now);
                // A question with no readable timestamp starts its wait now, as the live path does.
                activity.question_since = open_question.map(|(_, _, asked, _)| asked.unwrap_or(now));
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
    pub(crate) fn routing_file(&self) -> PathBuf {
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
            (session.clone(), (returned, before.status), result)
        };
        let (returned, status_before) = returned;
        if returned {
            spend::record_returned(self, &session.org).await;
        }
        // The activity log hears the same edge, once: a write that leaves the status alone
        // (cleanup, the app slot, a restart re-marking a stopped colony) records nothing.
        crate::activity::record_transition(self, status_before, &session).await;
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
        // Through the session store (the default local one), not the file helper directly: the
        // path and bytes are identical, and the write goes by the same name every backend will
        // answer it by.
        crate::store::LocalDirStore::new(self.cfg.data_dir.clone())
            .write_index(&data)
            .await?;
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
    /// How this colony's completion claims are verified: `auto` (the default), `none`, or an
    /// explicit test command. Omitted uses the publish module's `verify` setting.
    #[serde(default)]
    pub verify: Option<String>,
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
    /// Wait politely for an issue another local colony holds instead of being refused: the colony
    /// is admitted `Queued` behind the holder and starts when the issue becomes its own (issue
    /// #321). Off by default; `allow_duplicate` wins when both are set, and a claim another
    /// mothership holds is still refused either way.
    #[serde(default)]
    pub queue_behind_holder: bool,
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
/// Whether `s` is in one of the states that hold its issue against a second colony: still live or
/// queued somewhere, or published with its pull request open and waiting to be read. The shared
/// predicate behind [`issue_held_by`] and the boot-time claim reconcile (`claims.rs`).
pub(crate) fn holds_issue(s: &Session) -> bool {
    matches!(
        s.status,
        SessionStatus::Queued
            | SessionStatus::Starting
            | SessionStatus::Running
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Idle
            | SessionStatus::Publishing
            | SessionStatus::PrOpened
    )
}

/// The colony effectively holding `issue`: the first holding session that is not a `claim_wait`
/// waiter, else — once the holder is gone and only waiters remain — the oldest waiter (issue #321).
/// The oldest-first tiebreak is what keeps a waiter queue honest: a fresh launch is refused naming,
/// or queues behind, the waiter whose turn is next, never one that arrived later, and a waiter can
/// never be jumped by a later one.
pub(crate) fn issue_held_by(sessions: &[Session], repo: &str, issue: u64) -> Option<Session> {
    let holding = |s: &Session| holds_issue(s) && s.repo == repo && s.issue == Some(issue);
    sessions
        .iter()
        .find(|s| holding(s) && !s.claim_wait)
        .or_else(|| sessions.iter().filter(|s| holding(s)).min_by_key(|s| s.created_at))
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

/// The authoritative duplicate-colony claim, run while the admission write lock is held: the
/// pre-check in `create` reads under a read lock, so two launches can both pass it before either
/// inserts — this re-check closes that window, and the loser gets its holder back for a 409.
/// `Ok` carries the admitted colony, whether it queued, and how many were already waiting;
/// `Err` carries the colony already holding the issue, and nothing is inserted.
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
fn try_claim_session(
    sessions: &mut Vec<Session>,
    room: bool,
    mut session: Session,
    repo: &str,
    issue: Option<u64>,
    allow_duplicate: bool,
    queue_behind_holder: bool,
    wait_for_parent: bool,
) -> Result<(Session, bool, usize), Session> {
    // Issue #321: a launch that asked to wait its turn is not refused when the issue is held — it
    // is admitted as a `claim_wait` waiter behind whoever effectively holds it, however full or
    // empty the queue. The holder's mark on GitHub stays; the waiter never claims over it.
    let mut queued_for_holder = false;
    if let (Some(number), false) = (issue, allow_duplicate)
        && let Some(held) = issue_held_by(sessions, repo, number)
    {
        if !queue_behind_holder {
            return Err(held);
        }
        queued_for_holder = true;
        session.claim_wait = true;
        session.queued_behind = Some(held.id);
    }
    // A colony still waiting for its parent's branch queues even when a slot is free: booting now
    // would branch from the default branch, which is exactly what stacking exists to avoid. A
    // queued colony holds no slot, so nothing is wasted by the wait.
    // Issue #453: a newcomer queued behind a live same-repo colony for overlap stays queued even
    // with a free slot, and never carries a parent — it still branches fresh from the default
    // branch when the queue starts it. A holder that finished between the scan and this lock
    // releases it at once, clearing the stale pointer.
    let overlap_held = !queued_for_holder
        && session.parent.is_none()
        && session
            .queued_behind
            .as_deref()
            .is_some_and(|holder| sessions.iter().any(|s| s.id == holder && s.status.is_live()));
    if !overlap_held && !queued_for_holder {
        session.queued_behind = None;
    }
    session.status = if room && !wait_for_parent && !overlap_held && !queued_for_holder {
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

/// Whether new colonies publish automatically: the publish module's `autopilot` setting. Takes the
/// agent list rather than the app, so usage.rs can report the same default.
pub(crate) fn autopilot_default(agents: &[AgentModule], modules: &ModulesConfig) -> bool {
    let schema = schema_for("publish", &modules.publish.provider, agents);
    setting(&modules.publish, &schema, "autopilot")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// How new colonies' completion claims are verified (issue #328): the publish module's `verify`
/// setting — `auto`, `none`, or an explicit test command.
pub(crate) fn verify_default(agents: &[AgentModule], modules: &ModulesConfig) -> String {
    let schema = schema_for("publish", &modules.publish.provider, agents);
    setting_str(&modules.publish, &schema, "verify")
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
    // Which agent module the colony launches on: this org's pick, else the install's (issue #201).
    // Recorded on the session, so boot re-resolves from that and a later change moves new colonies only.
    let agent = app
        .agents
        .iter()
        .find(|a| a.id == orgs::effective_agent_module(&app.org_settings(owner), &modules))
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
    // The id becomes a path under <config>/claude-accounts, so refuse anything that is not a plain
    // account id rather than joining it. The org override and the stored default are validated where
    // they are saved; the request's explicit field is the one id that arrived over the API.
    if !crate::claude_accounts::valid_id(&claude_account) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "Claude account ids are lowercase letters, digits and dashes, 1-40 characters",
        ));
    }
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
    // Issue #321: a launch that asks to `queue_behind_holder` waits for a local holder instead of
    // being refused. GitHub is checked either way: a conflict attributable to one of this
    // mothership's own colonies on the issue is the holder being queued behind, while a merged PR
    // or a claim another mothership holds refuses the launch as ever — cross-mothership queueing
    // is out of scope.
    let mut queue_behind_holder = false;
    if let (Some(issue), false) = (req.issue, req.allow_duplicate)
        && let Some(held) = issue_held_by(&app.sessions.read().await, &repo, issue)
    {
        if !req.queue_behind_holder {
            return Err(client_error(StatusCode::CONFLICT, &duplicate_message(&held, issue)));
        }
        queue_behind_holder = true;
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
        let conflict = if let Some(info) = crate::claims::remote_result_or_fallback(checked) {
            if queue_behind_holder {
                // The waiter tolerates only a claim of ours — the holder it queues behind, or
                // another colony on this mothership; `claim_wait_conflict` refuses the rest.
                let sessions = app.sessions.read().await;
                let ours: Vec<&str> = sessions
                    .iter()
                    .filter(|s| s.repo == repo && s.issue == Some(issue))
                    .map(|s| s.id.as_str())
                    .collect();
                crate::claims::claim_wait_conflict(Some(&info), issue, &ours)
            } else {
                Some(crate::claims::remote_conflict_message(&info, issue))
            }
        } else {
            None
        };
        if let Some(message) = conflict {
            return Err(client_error(StatusCode::CONFLICT, &message));
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
        changed_paths: Vec::new(),
        ci_state: None,
        summary: None,
        publish_stage: None,
        // A fresh colony is starting or queued, never publishing: the flag is inert.
        publishing_holds_slot: false,
        needs_rebase: false,
        rebase_orphaned: false,
        queued_behind,
        // Set by admission when the launch waits for the issue's holder (issue #321).
        claim_wait: false,
        verify: Some(
            req.verify
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map_or_else(|| verify_default(&app.agents, &modules), str::to_string),
        ),
        verification: None,
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
        boot_image: None,
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
                // The request's flag, not the pre-lock reading above: a holder appearing between the
                // two is `try_claim_session`'s call, and it refuses with the holder named if the
                // launch never asked to queue.
                req.queue_behind_holder,
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
    // the background, never failing the launch. A `claim_wait` waiter (issue #321) publishes
    // nothing — the holder's mark is the issue's claim until the waiter is promoted and takes it.
    if crate::claims::should_check_remote(session.issue, req.allow_duplicate)
        && !session.claim_wait
        && let Some(issue) = session.issue
    {
        crate::claims::spawn_publish(app.clone(), repo.clone(), issue, id.clone());
    }
    if queued {
        if let Some(holder) = session.queued_behind.as_deref() {
            let why = if session.claim_wait {
                format!("queued behind colony {holder}: it holds this issue, so this colony starts once the issue is its own")
            } else {
                format!(
                    "queued behind colony {holder}: it is working in the same repository, so this colony starts once it finishes"
                )
            };
            app.session_log(&id, "info", why).await;
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
    app.mark_org_known(owner, pending_avatar.as_deref()).await;
    // A one-line summary of the task, written by a cheap model off the launch path (summaries.rs).
    tokio::spawn(crate::summaries::summarize_colony(app.clone(), session.id.clone()));
    Ok(Json(session))
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
    via: Option<axum::Extension<crate::auth::Via>>,
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    let via = via.map(|axum::Extension(via)| via);
    Ok(ws.on_upgrade(move |socket| events_socket(app, id, rt, query.since.unwrap_or(0), query.epoch, via, socket)))
}

/// One replayable line of events.jsonl: the decoded line and its seq, or `None` to skip it.
/// Decoded one chunk at a time, as `Runtime::load` reads the same file: a non-UTF-8 line or a
/// line outside the JSON contract costs itself, not the rest of the transcript.
fn replay_line(chunk: &[u8]) -> Option<(u64, &str)> {
    let line = std::str::from_utf8(chunk).ok()?;
    let seq = serde_json::from_str::<Value>(line).ok().and_then(|v| v["seq"].as_u64())?;
    Some((seq, line))
}

async fn events_socket(
    app: Shared,
    id: String,
    rt: Arc<Runtime>,
    since: u64,
    client_epoch: Option<u64>,
    via: Option<crate::auth::Via>,
    socket: WebSocket,
) {
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
                Some(Ok(Message::Text(body))) => client_command(&app, &id, &rt, via, body.as_str()).await,
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

async fn client_command(app: &Shared, id: &str, rt: &Arc<Runtime>, via: Option<crate::auth::Via>, body: &str) {
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
    let answered = forward["type"] == "answer";
    let _ = rt.commands.send(forward);
    if answered {
        crate::activity::record_answer(app, &s, via).await;
    }
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
            changed_paths: Vec::new(),
            ci_state: None,
            summary: None,
            publish_stage: None,
            // A bare `publishing` fixture is a live-origin claim, so it holds its slot; tests for
            // a stopped-origin publish flip this off.
            publishing_holds_slot: status == SessionStatus::Publishing,
            needs_rebase: false,
            rebase_orphaned: false,
            queued_behind: None,
            claim_wait: false,
            verify: None,
            verification: None,
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
            boot_image: None,
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
        let (admitted, queued, _) = try_claim_session(
            &mut sessions,
            true,
            queued_behind("holder"),
            "acme/repo",
            None,
            false,
            false,
            false,
        )
        .expect("no issue race");
        assert!(
            queued && admitted.status == SessionStatus::Queued,
            "held behind the live colony"
        );
        assert_eq!(admitted.queued_behind.as_deref(), Some("holder"));
        // The holder published: the same pointer releases at once, pointing nowhere stale.
        live_holder.status = SessionStatus::PrOpened;
        let mut sessions = vec![live_holder];
        let (admitted, queued, _) = try_claim_session(
            &mut sessions,
            true,
            queued_behind("holder"),
            "acme/repo",
            None,
            false,
            false,
            false,
        )
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
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            queue_behind_holder: false,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
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
            let claimed = try_claim_session(&mut sessions, true, retry, "acme/repo", Some(7), false, false, false);
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
        let refused = try_claim_session(&mut sessions, true, blocked, "acme/repo", Some(7), false, false, false);
        assert!(
            matches!(&refused, Err(held) if held.id == "first"),
            "a live holder refuses a second claim without allow_duplicate"
        );
        assert_eq!(sessions.len(), 1, "the refused claim inserted nothing");

        let mut second = colony("acme", SessionStatus::Starting);
        second.id = "second".into();
        second.issue = Some(7);
        let admitted = try_claim_session(&mut sessions, true, second, "acme/repo", Some(7), true, false, false);
        assert!(
            admitted.is_ok(),
            "allow_duplicate lets a second colony start on an issue another still holds"
        );
        assert_eq!(sessions.len(), 2, "the admitted duplicate is inserted alongside the holder");
    }

    /// A `claim_wait` waiter for issue 7, queued behind `holder`, created `ago_secs` ago so the
    /// oldest-first tiebreak is deterministic.
    fn waiter_on_issue(id: &str, holder: &str, ago_secs: i64) -> Session {
        let mut s = colony("acme", SessionStatus::Queued);
        s.id = id.into();
        s.issue = Some(7);
        s.claim_wait = true;
        s.queued_behind = Some(holder.into());
        s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
        s
    }

    #[test]
    fn a_launch_asked_to_queue_waits_behind_the_holder_instead_of_being_refused() {
        // Issue #321: the polite third option — the default refuses, `allow_duplicate` duplicates,
        // `queue_behind_holder` admits the launch as a waiter behind the holder, even with a slot
        // free: its turn comes when the queue gets to it, not before.
        let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
        let mut polite = colony("acme", SessionStatus::Starting);
        polite.id = "polite".into();
        polite.issue = Some(7);
        let (admitted, queued, _) = try_claim_session(&mut sessions, true, polite, "acme/repo", Some(7), false, true, false)
            .expect("a waiter is admitted, not refused");
        assert!(
            queued && admitted.status == SessionStatus::Queued,
            "queued even with a free slot"
        );
        assert!(admitted.claim_wait, "the colony is a waiter for its issue");
        assert_eq!(admitted.queued_behind.as_deref(), Some("holder"), "queued behind the holder");
        assert_eq!(sessions.len(), 2, "the waiter is inserted");
        // And the holder still holds the issue: the waiter claims nothing while it waits.
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("holder".to_string()),
            "the waiter does not take the hold over by waiting"
        );
    }

    #[test]
    fn the_default_still_refuses_and_allow_duplicate_still_wins_over_queueing() {
        // The two existing launches are unchanged, and `allow_duplicate` takes precedence when a
        // request sets both: it starts now, it does not wait its turn.
        let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
        let mut plain = colony("acme", SessionStatus::Starting);
        plain.id = "plain".into();
        plain.issue = Some(7);
        assert!(
            matches!(
                try_claim_session(&mut sessions, true, plain, "acme/repo", Some(7), false, false, false),
                Err(held) if held.id == "holder"
            ),
            "a launch that did not ask to queue is refused as ever"
        );
        let mut duplicate = colony("acme", SessionStatus::Starting);
        duplicate.id = "duplicate".into();
        duplicate.issue = Some(7);
        let (admitted, queued, _) = try_claim_session(&mut sessions, true, duplicate, "acme/repo", Some(7), true, true, false)
            .expect("allow_duplicate bypasses the hold");
        assert!(!queued && admitted.status == SessionStatus::Starting, "starts, not waits");
        assert!(!admitted.claim_wait, "a duplicate is no waiter");
    }

    #[test]
    fn with_only_waiters_left_the_oldest_one_holds_the_issue() {
        // The holder is gone; queue order decides. A fresh launch is refused naming the oldest
        // waiter — starting ahead of it would jump the queue, and waving the newcomer through
        // would duplicate the first waiter's work the moment its turn came.
        let sessions = vec![
            waiter_on_issue("first", "holder", 100),
            waiter_on_issue("second", "holder", 50),
        ];
        let held = issue_held_by(&sessions, "acme/repo", 7).expect("a waiter holds the issue once the holder is gone");
        assert_eq!(held.id, "first", "the oldest waiter holds it");
        let message = duplicate_message(&held, 7);
        assert!(
            message.contains("colony first is already on #7") && message.contains("allow_duplicate"),
            "{message}"
        );
        // A polite launch queues behind that same waiter, and the atomic claim refuses the
        // default one for it.
        let mut sessions = sessions;
        let mut fresh = colony("acme", SessionStatus::Starting);
        fresh.id = "fresh".into();
        fresh.issue = Some(7);
        assert!(
            matches!(
                try_claim_session(&mut sessions, true, fresh.clone(), "acme/repo", Some(7), false, false, false),
                Err(held) if held.id == "first"
            ),
            "a fresh default launch is refused naming the oldest waiter"
        );
        let (admitted, _, _) = try_claim_session(&mut sessions, true, fresh, "acme/repo", Some(7), false, true, false)
            .expect("a polite launch waits");
        assert_eq!(admitted.queued_behind.as_deref(), Some("first"), "behind the oldest waiter");
    }

    #[test]
    fn a_real_holder_outranks_the_waiters_however_young_it_is() {
        // The first non-waiter holder wins whatever the creation order: waiters only take over
        // once there is no holder left at all.
        let mut holder = on_issue("holder", 7, SessionStatus::Running);
        holder.created_at = Utc::now();
        let sessions = vec![waiter_on_issue("old-waiter", "gone", 200), holder];
        assert_eq!(
            issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
            Some("holder".to_string()),
            "the holder keeps the issue; the waiter keeps waiting"
        );
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
                    try_claim_session(guard, room, fresh, "acme/repo", Some(7), false, false, false)
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
        let question = r#"{"seq":3,"ts":"2026-09-21T09:30:00.000Z","type":"question","question_id":"q1","questions":[{"header":"pin","options":[]}],"risk":"read_only"}"#;

        // Asked and never answered: the question is open, and its wait started when it was asked.
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}}\n{question}\n"),
        )
        .unwrap();
        let rt = Runtime::load(&dir);
        let (id, questions, risk) = rt
            .open_question
            .try_lock()
            .unwrap()
            .clone()
            .expect("the question is still open");
        assert_eq!(id, "q1");
        assert_eq!(questions, vec![json!({"header": "pin", "options": []})]);
        assert_eq!(
            risk,
            QuestionRisk::ReadOnly,
            "the risk class rides out the restart with the question"
        );
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

    /// The risk class folds on replay exactly as the live path folds it (`events.rs`): a question
    /// from an older runner, with no `risk` on disk, restarts as a workspace write; a class a
    /// future runner knows stays above every ceiling, and the judge must keep refusing it.
    #[test]
    fn a_restored_question_risk_folds_the_same_way_the_live_path_does() {
        let dir = std::env::temp_dir().join(format!("colonizer-open-question-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stored = |risk: &str| format!(r#"{{"seq":1,"type":"question","question_id":"q1","questions":[]{risk}}}"#);
        for (line, expected, why) in [
            (stored(""), QuestionRisk::WorkspaceWrite, "an older runner left the field out"),
            (stored(r#","risk":null"#), QuestionRisk::WorkspaceWrite, "null is absent"),
            (
                stored(r#","risk":"unknown_string_from_a_newer_runner""#),
                QuestionRisk::Unknown,
                "a string outside the vocabulary",
            ),
            (stored(r#","risk":3"#), QuestionRisk::Unknown, "not a string at all"),
            (
                stored(r#","risk":{"note":"trust me"}"#),
                QuestionRisk::Unknown,
                "not a string at all",
            ),
        ] {
            std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
            let rt = Runtime::load(&dir);
            let (_, _, risk) = rt.open_question.try_lock().unwrap().clone().expect("still open");
            assert_eq!(risk, expected, "{why}: for {line}");
        }
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
                verify: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                queue_behind_holder: false,
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
                verify: None,
                autofix: None,
                automerge: None,
                allow_duplicate: false,
                queue_behind_holder: false,
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
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            queue_behind_holder: false,
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
}
