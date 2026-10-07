//! The colony record: `Session`, its status and the small types it carries, as `sessions.json` stores
//! them and the API serialises them.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Waiting for a free slot: no microVM, no worktree, nothing claimed yet.
    Queued,
    /// Waiting on the colony it is stacked on, which is stopped or parked (issue #1140): no slot, no
    /// microVM, not failed. It is `Queued` again the moment that colony resumes or finishes, and
    /// re-bases on the default branch when it is gone for good. Neither live nor finished.
    Blocked,
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
    /// Parked (issue #213): the colony is set aside for a reason it may outlive — its provider's
    /// quota ran out, or its autopilot hold timed out — with its worktree and branch kept and its
    /// park record in `parked`. Not live: no slot is held against the parallel limit. Not
    /// terminal either: the run is paused, not over, so no reclaim, no spend `returned` edge and
    /// no automatic cleanup may read it as finished — the worktree may be the only copy of the
    /// work, and the colony is expected to come back.
    Parked,
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
    /// `Parked` is deliberately absent: a parked colony's run is paused, not over — it is expected
    /// to resume, and a `returned` edge here would book the spend twice (once at the park, once at
    /// the real end). Parked is not live either, so it holds no slot; it sits outside both sets.
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
            Self::Blocked => "blocked",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingForAnswer => "waiting_for_answer",
            Self::Idle => "idle",
            Self::Publishing => "publishing",
            Self::PrOpened => "pr_opened",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::NoChanges => "no_changes",
            Self::Parked => "parked",
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

    /// Records that autopilot held this colony's publish for `cause` (issue #1175): the same cause
    /// as the last hold counts one more repeat, a different one starts the count over at one.
    pub(crate) fn note_hold(&mut self, cause: &str) {
        if self.hold_cause.as_deref() == Some(cause) {
            self.hold_cause_repeats = self.hold_cause_repeats.saturating_add(1);
        } else {
            self.hold_cause = Some(cause.to_string());
            self.hold_cause_repeats = 1;
        }
    }

    /// Forgets the hold cause: a publish went ahead, so the next hold is a first one again.
    pub(crate) fn clear_hold_cause(&mut self) {
        self.hold_cause = None;
        self.hold_cause_repeats = 0;
        self.verify_fix_rounds = 0;
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

    /// Whether this colony's pre-warm boot is under way (issue #701): the queue has claimed it for
    /// the boot, whether or not the runner has linked yet.
    pub fn prewarming(&self) -> bool {
        self.prewarm.as_ref().is_some_and(|p| p.started_at.is_some())
    }

    /// The cause of the run that ended just before this boot (issue #756), for a resumed colony:
    /// what [`run_end_cause`](Self::run_end_cause) recorded, or a suspension — the record the claim
    /// has just cleared is kept, transiently, as `was_suspended`, so a colony suspended by a build
    /// older than the field still reads as one.
    pub(crate) fn resume_cause(&self) -> Option<RunEndCause> {
        if self.was_suspended {
            Some(RunEndCause::Suspended)
        } else {
            self.run_end_cause
        }
    }

    /// Take the cause for the resume this boot is making, clearing the durable record (issue #756):
    /// it names only the run just ended, and left behind it would make the next resume read its
    /// own end — or its archive — as this one's. The transient `was_suspended` is left for the
    /// restore below.
    pub(crate) fn take_resume_cause(&mut self) -> Option<RunEndCause> {
        let cause = self.resume_cause();
        self.run_end_cause = None;
        cause
    }

    /// Whether this colony holds a microVM slot against the parallel limit — the predicate
    /// `queue::has_room` counts. Any live colony holds one, and so does a publish claimed from a
    /// live colony: the teardown inside the publish frees the microVM, but the slot stays claimed
    /// until the push lands, so nothing boots into the half-published worktree. A publish claimed
    /// from a stopped, failed or no-changes colony boots nothing (host-side push only) and holds
    /// nothing, so publishing a stopped colony never takes a slot another colony is waiting for.
    pub fn holds_slot(&self) -> bool {
        // A suspended colony's microVM is gone — that is the point (issue #562) — so it holds no
        // slot and the queue can admit someone else until the answer restores it. A colony whose
        // question is being pre-warmed (issue #701) is the exception: its microVM is back, so it
        // holds its slot again, but never ahead of a colony that already holds an answer — the
        // queue admits those first.
        (self.suspended.is_none() || self.prewarming())
            && (self.status.is_live() || (self.status == SessionStatus::Publishing && self.publishing_holds_slot))
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

/// The `publishing_holds_slot` of a row written before the flag existed: holding, because every
/// publish held its slot back then, and a row of unknown origin must not silently free one.
fn publishing_holds_slot_legacy() -> bool {
    true
}

/// Why a colony was suspended, in its [`Suspension`] record and in the log line: the only reason
/// this build suspends is a question waiting past its grace period.
pub(crate) const WAITING_FOR_ANSWER: &str = "waiting_for_answer";

/// How a suspended colony comes back (`Suspension::path`): today only the fallback — a fresh
/// microVM boots, the agent runner resumes the session transcript it kept, and the answer is the
/// next user message. `sandbox::supports_memory_snapshot` is the seam a real snapshot enters at,
/// and would record a different path here.
pub(crate) const SESSION_RESUME: &str = "session_resume";

/// Why a colony's previous run ended (issue #756): recorded when something other than the user cut
/// the run short, so a resumed colony's brief can say so and not take the pinned runner's
/// "stopped by the user" report for its subagents at face value. A user stop leaves it unset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEndCause {
    /// The colony was torn down to free its slot while it waited on its user (issue #562).
    Suspended,
    /// The mothership restarted and the colony's microVM was not running (`lifecycle::recover`).
    Restart,
    /// The microVM stopped on its own, or the host stopped it (`lifecycle::watch_sandboxes`).
    Teardown,
}

/// A colony torn down while it waits on its user (issue #562). The status stays
/// `waiting_for_answer` and the question answerable; `at` is when the microVM came down,
/// `snapshot` is what a real memory snapshot would carry — always `None` today, the pinned
/// microsandbox has none (`sandbox::supports_memory_snapshot`) — and `path` says how the colony
/// comes back ([`SESSION_RESUME`]).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Suspension {
    pub at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,
    pub reason: String,
    pub path: String,
}

/// Why a colony is parked (issue #213), kept on the record so the park survives restarts and
/// resumes land on their feet days later. `at` is when the colony was parked; `reason` is the
/// machine string (`provider_quota_exhausted`, `hold_timeout`); `resets_at` is the RFC3339 moment
/// the upstream said the block lifts, when it named one; `vm_kept` says whether the microVM was
/// still running when the colony parked — true when the `resume` module's `discard_vm` is off, or
/// when the park could not verify the worktree was safe to leave without the VM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Park {
    pub at: DateTime<Utc>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    pub vm_kept: bool,
    /// The risk class of the question the colony was parked on, when it was waiting on one
    /// (issue #876): a hold park records it so the backoff decision can tell a question the judge
    /// may answer from one it never may. `None` for a park with no open question, or a quota park.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_risk: Option<crate::protocol::QuestionRisk>,
}

/// A user answer that arrived while the colony was suspended and is still undelivered.
/// `question_id` is the question it answered; `prompt` is the user message the resumed runner
/// receives, formatted at answer time while the question text is still known — a manual resume
/// rotates the event log, so boot time would be too late to quote the question. `answered_at` is
/// when the answer arrived, what the restore pass lines answered colonies up by; records saved
/// before answers kept one carry no time, and the restore pass falls back to the suspension's own.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingAnswer {
    pub question_id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<DateTime<Utc>>,
}

/// A pre-warm request for a suspended colony's question (issue #701): the queue boots the colony
/// through normal admission so the answer lands in an already-running VM. `requested_at` is when
/// someone opened the question, what the queue lines candidates up by and what the timeout counts
/// from once no boot followed; `started_at` is set when the queue claims the colony for the boot
/// (None means the request is still waiting for a slot); `ready_at` is when the runner linked.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Prewarm {
    pub requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<DateTime<Utc>>,
}

/// How many changed paths a colony keeps: enough to place it in a monorepo's packages, bounded so a
/// sweeping change cannot bloat sessions.json.
pub const CHANGED_PATHS_CAP: usize = 500;

/// One model setting the boot resolved away from what it started with because the gateway would have
/// refused it for the task's sensitivity class (issue #704): `setting` is the model setting's name
/// (`model`, `subagent_model`, `background_model`), `from` the model the setting named, `to` the
/// eligible one it was replaced with, and `reason` why the gateway would refuse the first. Recorded
/// on the session so the cockpit can say what the colony is really running on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSubstitution {
    pub setting: String,
    pub from: String,
    pub to: String,
    pub reason: String,
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
    /// burn-down scheduler auto-launched, so the global stop can find it and the UI can label it;
    /// `Some("redteam")` a red-team hunter, `Some("map")` a mapping colony. `None` for anything a
    /// person started. The event origin resolver (`events.rs`) reads the machine launchers back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The id of the scoped API token that launched this colony (issue #508, api_tokens.rs), when
    /// one did: the concurrency cap and daily budget count a token's own colonies by it, and the
    /// boot resolves the id back to the token's name to mark the instructions as external input.
    /// `None` for anything the owner started. The token itself is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_by_token: Option<String>,
    /// Why this colony is where it is, in placement's words (issue #688): the member and its free
    /// capacity, or that a peer had room but cross-member launch is not built yet. Recorded on a
    /// fresh launch; `None` on colonies written before it existed or re-admitted from the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<String>,
    pub worktree: String,
    pub git_admin_dir: Option<String>,
    pub sandbox: String,
    pub mesh: Option<MeshInfo>,
    pub local_port: Option<u16>,
    /// The guest-local port a dev-server preview is proxied from (previews.rs), set by the owner
    /// through `POST /api/sessions/{id}/preview`; `None` when no preview is open. Cleared wherever
    /// `local_port` and `mesh` are — a claim that boots a fresh microVM — so a stopped colony's
    /// preview is closed rather than pointing at a port nothing serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_port: Option<u16>,
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
    /// Whether the failure that put the colony here has been seen by a person (issue #744): set
    /// by [`App::update_session`] at the crossing itself, cleared by `POST /api/sessions/{id}/seen`,
    /// read by the app badge (`push::needs_you`). A failure already on record was seen long ago.
    #[serde(default)]
    pub unseen_failure: bool,
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
    /// This colony's own place in the start queue (issue #1156), overriding its org's
    /// `queue_priority` while it waits: higher starts first, ties go to the older colony. Set by
    /// `POST /api/sessions/{id}/priority`, `move-to-front` and `move-to-back`; `None` follows the org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    /// How this colony's completion claims are verified (issue #328): the `verify` configuration
    /// resolved at launch — `auto` (the default), `none`, or an explicit test command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// The verdict attached to the colony's last completion claim, and everything behind it.
    /// `None` until a claim has been verified (verify.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<crate::verify::Verification>,
    pub error: Option<String>,
    /// Why a `Blocked` colony waits, in words a person can act on: "waiting on #5 (`c8a6d23c`,
    /// stopped)" (issue #1140). `None` in every other status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// Whether the one automatic message asking the agent to rewrite `/harness/out/pr.md` has been
    /// sent for the "didn't write or update its PR description" case (issue #1140), so a colony is
    /// asked once and then left to park.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pr_rewrite_nudged: bool,
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
    /// open another colony's provider. Empty for Claude-only colonies. `None` reaches no provider:
    /// the set is what the token's access is derived from, and boot records it before the token is
    /// written (issue #681), so a live colony always has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    /// The models this colony may request through the gateway, as `<provider-id>/<model>` pairs
    /// derived at boot from the same model settings `allowed_providers` comes from (issue #681).
    /// `proxy` refuses a request whose body names any other model on an allowed provider, checked
    /// on the requested name before any `model_map` renaming. Empty for Claude-only colonies;
    /// `None` reaches no model, like a `None` [`Session::allowed_providers`] reaches no provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// The strictest file-sensitivity class this colony's task touches (sensitivity.rs, issue #472):
    /// `open`, `standard`, `custom` or `restricted`. The gateway refuses a `restricted` colony any
    /// provider not marked `trusted`, independently of `allowed_providers`. `None` for a colony that
    /// booted before this field existed, or whose task named no sensitive path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<String>,
    /// Model settings the boot replaced with an eligible model because the gateway would have refused
    /// the one they named for this colony's sensitivity class (issue #704). Empty for a colony whose
    /// task named no sensitive path, or whose models all cleared the class's bar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_substitutions: Vec<ModelSubstitution>,
    /// Dollars the gateway recorded for responses it routed to providers (everything but Claude, whose
    /// own cost lands above). Kept on the session so spend survives a restart and reaches the UI.
    pub routed_cost_usd: Option<f64>,
    /// Tokens the gateway routed to providers, counted whether or not the provider priced them — a
    /// prepaid token plan prices nothing, so its dollars never move above but its tokens still spend
    /// the plan. Kept on the session like `routed_cost_usd`; not the same numbers as `model_usage`,
    /// which the runner reports per model at turn end, and never summed with it.
    pub routed_tokens: Option<u64>,
    /// What the colony leaves on the host — its worktree plus its session directory — as last measured by
    /// the host-disk check, which runs only when a host-disk quota applies to the colony. Not the
    /// microVM's root disk, which is a separate limit (microsandbox's `--root-disk`).
    pub host_disk_bytes: Option<u64>,
    pub cleaned_up: bool,
    /// Operator opt-out of automatic worktree reclamation; manual cleanup still works.
    pub keep_worktree: bool,
    /// Set by the watchdog: `{reason, since, nudges}`.
    pub attention: Option<Value>,
    /// The colony is suspended while it waits on its user (issue #562): the microVM is torn down,
    /// the worktree and the agent's session transcript are kept, and the answer re-boots it.
    /// `None` while the colony runs. A suspended colony keeps its `waiting_for_answer` status and
    /// holds no parallel slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended: Option<Suspension>,
    /// Why the run before this boot ended, when something other than the user ended it (issue
    /// #756): set by the suspension, the restart's reap and the sandbox watchdog's teardown, and
    /// cleared by a user stop. `None` for a colony whose last run ended at the user's hand, or that
    /// has never run. The brief reads it to tell a resumed colony its subagents were not stopped by
    /// the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_end_cause: Option<RunEndCause>,
    /// The colony's park record (issue #213), set when the status moves to `parked` and cleared
    /// when it resumes. Persisted in sessions.json, so a colony parked for a quota reset that
    /// lands tomorrow still carries the reason, the upstream reset time and whether its microVM
    /// was kept. `None` for a colony that has never been parked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<Park>,
    /// How many times the hold timeout has auto-resumed this colony (issue #876): the index into
    /// the backoff schedule. On the session, not the park record, because a resume clears `parked`
    /// and a colony that parks again must not start the schedule over.
    #[serde(default)]
    pub hold_resumes: u32,
    /// Why autopilot last held this colony's publish (issue #1175), e.g. `verification: <detail>`,
    /// and how many holds in a row had exactly that cause. A hold that repeats with nothing changed
    /// is not a question a person can answer by waiting: [`Session::note_hold`] counts it, and the
    /// hold-park backoff fails the colony with `publish_blocked` instead of cycling park and resume.
    /// Cleared when a publish goes ahead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_cause: Option<String>,
    #[serde(default)]
    pub hold_cause_repeats: u32,
    /// How many automatic fix rounds a contradicted verification has already sent this colony's
    /// agent (issue #1186). Capped at [`crate::verify::FIX_ROUNDS_MAX`]; cleared when a publish
    /// goes ahead.
    #[serde(default)]
    pub verify_fix_rounds: u32,
    /// How many automatic continues have been scheduled after a transient provider error
    /// (issue #980): the 1-based attempt the colony is backing off for, so the delay already
    /// spent is `provider_retries - 1` into the schedule. Reset to 0 on a successful turn and
    /// when the attempts run out, so a later unrelated error starts a fresh backoff sequence.
    #[serde(default)]
    pub provider_retries: u32,
    /// The agent runner's own session id, as last reported by the `agent_session` event: what a
    /// resumed boot continues. `None` until the first report, and permanently unknown to agents
    /// whose module declares no `session_resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
    /// The user's answer to a suspended colony's question, kept until a boot delivers it: set when
    /// the answer arrives, cleared once the resumed runner is up. It survives a failed boot and a
    /// mothership restart, so an answer is never lost (issue #562).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_answer: Option<PendingAnswer>,
    /// The note the next boot hands a colony whose agent module was switched mid-task (issue #737):
    /// the switch sets it with the new `agent` and `agent_session`, the boot that follows treats it
    /// as a resume trigger and uses it as the turn prompt, and it is cleared once the runner is up —
    /// the same delivery as `pending_answer`, and for the same reason: it survives a failed boot and
    /// a mothership restart, so a switch whose boot has not run is never lost. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_note: Option<String>,
    /// A one-shot note the next resume hands the agent (issue #876): the hold-timeout backoff writes
    /// what to do about the timed-out question here, and an answer given while parked writes the
    /// answer itself. Delivered on a cold resume (the brief) and a warm one (the prompt), then
    /// cleared — like `pending_answer` but with no suspension behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_note: Option<String>,
    /// The colony's pre-warm request (issue #701), set when someone opens a suspended colony's
    /// question and the queue has not started (or has already given up on) the warm-up boot.
    /// `None` unless a request is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prewarm: Option<Prewarm>,
    /// The supply-chain target this colony was launched against (issue #673): a package and the
    /// advisory it was launched to fix. A live colony for one target refuses a second, like an
    /// issue hold. `None` for everything not launched against one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supply_chain: Option<crate::supersede::SupplyChainTarget>,
    /// Every package/advisory pair a supply-chain loop colony was dispatched to fix (issue #832): its
    /// share of the same claim `supply_chain` makes, read by the one duplicate rule (duplicates.rs).
    /// An advisory left empty claims every advisory of its package. Empty for everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supply_chain_targets: Vec<crate::supersede::SupplyChainTarget>,
    /// Set when a same-repo colony's pull request merged over this colony's work (issue #673): what
    /// covered it, why, and whether the operator kept it running anyway. While it stands unkept the
    /// queue and the resume route leave the colony where it is. `None` for a colony no merge covered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<crate::supersede::Supersession>,
    /// Set by the claim that sends a colony to boot (lifecycle's resume, the queue's restore):
    /// whether the colony was suspended when it was claimed (issue #700). The boot reads it for
    /// session.json's `restore` key, so the guest can tell a suspension's restore from a plain
    /// resume — the claim itself has just cleared `suspended`. Transient on purpose: not
    /// persisted, and a harness restart between claim and boot only ever loses it towards "no".
    #[serde(skip)]
    pub was_suspended: bool,
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
    /// How the last boot failure was classed (issue #881): `transient_infra` for a blip the colony
    /// retries on [`Session::boot_retries`] and [`Session::retry_at`], `permanent` for a verdict a
    /// retry cannot fix. Cleared when a boot finally lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<crate::retry::FailureClass>,
    /// How many transient boot failures this colony has retried (issue #881). When it reaches the
    /// retry budget the next failure is permanent. Kept as a record after a successful boot.
    #[serde(default)]
    pub boot_retries: u32,
    /// When a transient boot failure may be tried again (issue #881): the queue holds the colony
    /// `Queued` until this passes. `None` when no retry is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<DateTime<Utc>>,
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
            launched_by_token: None,
            placement: None,
            worktree: String::new(),
            git_admin_dir: None,
            sandbox: String::new(),
            mesh: None,
            local_port: None,
            preview_port: None,
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
            unseen_failure: false,
            queued_behind: None,
            blocked_reason: None,
            pr_rewrite_nudged: false,
            claim_wait: false,
            priority: None,
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
            allowed_models: None,
            sensitivity: None,
            model_substitutions: Vec::new(),
            routed_cost_usd: None,
            routed_tokens: None,
            host_disk_bytes: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            suspended: None,
            run_end_cause: None,
            parked: None,
            hold_resumes: 0,
            hold_cause: None,
            hold_cause_repeats: 0,
            verify_fix_rounds: 0,
            provider_retries: 0,
            agent_session: None,
            pending_answer: None,
            switch_note: None,
            resume_note: None,
            prewarm: None,
            supply_chain: None,
            supply_chain_targets: Vec::new(),
            superseded: None,
            was_suspended: false,
            last_activity_at: None,
            boot_timing: None,
            boot_cpus: None,
            boot_memory: None,
            boot_image: None,
            app_slot: None,
            boot_attempt_started_at: None,
            failure_class: None,
            boot_retries: 0,
            retry_at: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            updated_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::sessions::tests::*;

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

    /// And one saved before a colony could be suspended (issue #562): such a colony was never
    /// suspended, holds no agent session id and keeps no answer.
    #[test]
    fn a_session_saved_before_suspension_existed_still_deserialises() {
        let saved = r#"{"id":"c","repo":"acme/repo","issue":null,"issue_title":"","status":"waiting_for_answer","branch":"b","base":null,"worktree":"","git_admin_dir":"git","sandbox":"s","mesh":null,"agent":"a","pr_url":null,"error":null,"cost_usd":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#;
        let s: Session = serde_json::from_str(saved).unwrap();
        assert_eq!(s.suspended, None);
        assert_eq!(s.agent_session, None);
        assert_eq!(s.pending_answer, None);
        assert_eq!(s.run_end_cause, None, "no recorded cause on a row that predates the field");
    }

    /// The suspension record and the held answer survive the trip to the wire and back intact —
    /// this is the shape sessions.json keeps across a restart, so a suspended colony with an
    /// answer in hand must read back exactly as it was written.
    #[test]
    fn a_suspension_and_a_held_answer_round_trip_through_the_wire() {
        let mut s = colony("acme", SessionStatus::WaitingForAnswer);
        s.id = "c".into();
        s.agent_session = Some("sess_123".into());
        s.suspended = Some(Suspension {
            at: Utc::now(),
            snapshot: None,
            reason: WAITING_FOR_ANSWER.into(),
            path: SESSION_RESUME.into(),
        });
        s.pending_answer = Some(PendingAnswer {
            question_id: "q1".into(),
            prompt: "Q: Which file name?\nA: hello.txt".into(),
            answered_at: Some(Utc::now()),
        });
        let again: Session = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(again.suspended, s.suspended);
        assert_eq!(again.agent_session, s.agent_session);
        assert_eq!(again.pending_answer, s.pending_answer);
        let wire = serde_json::to_value(&s).unwrap();
        assert_eq!(wire["agent_session"], json!("sess_123"));
        assert_eq!(wire["suspended"]["reason"], json!("waiting_for_answer"));
        assert_eq!(wire["suspended"]["path"], json!("session_resume"));
        assert_eq!(wire["pending_answer"]["question_id"], json!("q1"));
        assert!(
            wire["pending_answer"]["answered_at"].is_string(),
            "the answer time rides the wire as RFC3339"
        );
    }

    /// And a held answer saved before answers carried a time (issue #667) still loads: the restore
    /// pass falls back to the suspension's own time for those.
    #[test]
    fn a_held_answer_saved_before_it_kept_a_time_still_deserialises() {
        let saved = r#"{"question_id":"q1","prompt":"Q: Ship it?\nA: yes"}"#;
        let answer: PendingAnswer = serde_json::from_str(saved).unwrap();
        assert_eq!(answer.question_id, "q1");
        assert_eq!(answer.answered_at, None, "no time on the record, none read back");
    }

    /// The park record (issue #213) survives the trip to the wire and back — this is the shape
    /// sessions.json keeps across a restart, so a colony parked for tomorrow's quota reset must
    /// read back exactly as it was written, reset time and all. And a colony that has never been
    /// parked carries no `parked` key on the wire at all.
    #[test]
    fn a_park_record_round_trips_through_the_wire_and_absence_means_never_parked() {
        let mut s = colony("acme", SessionStatus::Parked);
        s.id = "c".into();
        s.parked = Some(Park {
            at: Utc::now(),
            reason: "hold_timeout".into(),
            resets_at: None,
            vm_kept: true,
            question_risk: Some(crate::protocol::QuestionRisk::WorkspaceWrite),
        });
        let again: Session = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(again.parked, s.parked);
        let wire = serde_json::to_value(&s).unwrap();
        assert_eq!(wire["parked"]["reason"], json!("hold_timeout"));
        assert_eq!(wire["parked"]["vm_kept"], json!(true));
        assert!(wire["parked"].get("resets_at").is_none(), "no reset, no field");
        // A parked colony holds no slot: the queue may admit someone else until it resumes.
        assert!(!s.holds_slot());
        assert!(!s.status.is_terminal(), "parked is paused, not over");
        let never: Session = serde_json::from_value(json!({
            "id": "d", "repo": "acme/repo", "status": "running",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(never.parked, None);
        assert!(serde_json::to_value(&never).unwrap().get("parked").is_none());
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
        full.preview_port = Some(5173);
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
        full.allowed_models = Some(vec!["acme/claude-sonnet-5".into()]);
        full.routed_cost_usd = Some(0.25);
        full.host_disk_bytes = Some(1024);
        full.cleaned_up = true;
        full.attention = Some(json!({"reason": "stalled"}));
        full.parked = Some(Park {
            at: Utc::now(),
            reason: "provider_quota_exhausted".into(),
            resets_at: Some("2026-09-28T07:00:00Z".into()),
            vm_kept: false,
            question_risk: None,
        });
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
            SessionStatus::Parked,
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
}
