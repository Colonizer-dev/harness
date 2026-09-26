//! The colony record: `Session`, its status and the small types it carries, as `sessions.json` stores
//! them and the API serialises them.

use super::*;

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
        // A suspended colony's microVM is gone — that is the point (issue #562) — so it holds no
        // slot and the queue can admit someone else until the answer restores it.
        self.suspended.is_none()
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

/// A user answer that arrived while the colony was suspended and is still undelivered.
/// `question_id` is the question it answered; `prompt` is the user message the resumed runner
/// receives, formatted at answer time while the question text is still known — a manual resume
/// rotates the event log, so boot time would be too late to quote the question.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingAnswer {
    pub question_id: String,
    pub prompt: String,
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
    /// The strictest file-sensitivity class this colony's task touches (sensitivity.rs, issue #472):
    /// `open`, `standard`, `custom` or `restricted`. The gateway refuses a `restricted` colony any
    /// provider not marked `trusted`, independently of `allowed_providers`. `None` for a colony that
    /// booted before this field existed, or whose task named no sensitive path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<String>,
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
            launched_by_token: None,
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
            sensitivity: None,
            routed_cost_usd: None,
            routed_tokens: None,
            host_disk_bytes: None,
            cleaned_up: false,
            keep_worktree: false,
            attention: None,
            suspended: None,
            agent_session: None,
            pending_answer: None,
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
}
