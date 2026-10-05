//! The colony queue: when every parallel slot is taken, a new colony waits instead of being
//! refused, and a loop starts the oldest waiting colony that fits each time a slot frees up.
//!
//! Whether a colony fits is a pure function (`has_room`), so the admission rule can be tested
//! apart from the loop that applies it.

use crate::{Shared, orgs, provider_quota, providers, restack, spend, stack::Stacked};
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::RwLock;

#[allow(unused_imports)]
use crate::{events::*, lifecycle::*, publish::*, sessions::*};

/// Whether another colony of `repo` (in `org`) can start right now. Three limits apply together and the
/// tightest wins: the global one, the org's own if it sets one, and the per-repository one. A queued
/// colony holds no microVM, so it counts towards none of them — and neither does a publish claimed from
/// a stopped, failed or no-changes colony, which boots nothing (`Session::holds_slot`).
pub(crate) fn has_room(
    sessions: &[Session],
    org: &str,
    repo: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    repo_limit: u64,
) -> bool {
    let busy = || sessions.iter().filter(|s| s.holds_slot());
    if busy().count() >= max_parallel {
        return false;
    }
    if org_limit.is_some_and(|limit| busy().filter(|s| s.org == org).count() as u64 >= limit) {
        return false;
    }
    (busy().filter(|s| s.repo == repo).count() as u64) < repo_limit
}

/// The per-repository limit for a colony of an org whose settings are `org`: its own, else the global.
pub(crate) fn repo_limit(modules: &crate::config::ModulesConfig, org: &orgs::OrgSettings) -> u64 {
    orgs::repo_max_parallel(org).unwrap_or_else(|| orgs::global_repo_max_parallel(modules))
}

/// The attention reason an autopilot hold carries, set where the hold is taken (events) and read here.
pub(crate) const AUTOPILOT_HELD_REASON: &str = "autopilot_held";

/// The attention reason a colony parked for outlasting its hold carries. The park record's status is
/// real now (`Parked`, issue #213) — the reason string is what keeps [`resume_quota_parked`] from
/// requeueing these: it only matches its own reason.
pub(crate) const HOLD_TIMEOUT_REASON: &str = "hold_timeout";

/// The `error` a colony is failed with when it stays parked on an unanswered question through every
/// backoff step (issue #876): the question was abandoned and the worktree is kept.
pub(crate) const ABANDONED_QUESTION_REASON: &str = "abandoned_question";

/// The park reason a colony gets while it backs off a transient provider error (issue #980): the
/// slot is released and [`resume_provider_retry_parked`] owns this reason alone, bringing the colony
/// back once the attempt's delay has passed.
pub(crate) const PROVIDER_RETRY_REASON: &str = "provider_retry";

/// The attention reason stamped on a hold-parked colony whose question is above the judge's ceiling
/// (issue #876): the backoff never resumes it, so the queue raises this once for `notify` to read as
/// "a person has to answer this". Distinct from [`HOLD_TIMEOUT_REASON`] so [`hold_park_action`] still
/// sees a hold park.
pub(crate) const HOLD_UNANSWERED_REASON: &str = "hold_parked_unanswered";

/// The delay before each hold-timeout auto-resume (issue #876), indexed by `hold_resumes` and measured
/// from the park's `at`: one hour, then three, then nine.
pub(crate) const HOLD_RESUME_SCHEDULE: [chrono::Duration; 3] = [
    chrono::Duration::hours(1),
    chrono::Duration::hours(3),
    chrono::Duration::hours(9),
];

/// The delay before each automatic retry after a transient provider error (issue #980), indexed by
/// the attempt already spent (`provider_retries - 1`) and measured from the park's `at`: 2, 5, 10
/// then 20 minutes. Four entries, so at most four retries before the colony is held for a person.
pub(crate) const PROVIDER_RETRY_SCHEDULE_MINUTES: [i64; 4] = [2, 5, 10, 20];

/// The one-shot note a backoff resume hands the agent (issue #876): the question timed out unanswered,
/// so it should pick the safe option itself and say what it chose.
pub(crate) const HOLD_RESUME_NOTE: &str = "Your question timed out while you were parked; choose the option marked \
     Recommended, or the one that keeps the PR small, and note the choice in /harness/out/pr.md.";

/// How long a question that holds a tool call in flight (issue #759) keeps its colony's microVM
/// before the colony is suspended anyway. Such a question is exempt from the ordinary grace, since
/// suspending it loses the agent that asked; without a ceiling, a question nobody answers would hold
/// a microVM and a parallel slot for ever. Nothing else bounds that wait: budgets count spend, and
/// a colony blocked on its user spends nothing. Two hours is generous for a human and still frees
/// the slot the same day; never shorter than the ordinary grace.
pub(crate) const BLOCKING_QUESTION_CAP: chrono::Duration = chrono::Duration::minutes(120);

/// Whether this colony's autopilot hold has outlasted its slot (issue #217).
///
/// Slot policy: a colony waiting on a human is not using the CPU, so within the timeout it keeps its
/// slot — it may still be answered and resume in seconds. Past the timeout the queue parks it: the
/// slot is released, the worktree and branch kept, and the colony is resumable. A missing or
/// unparseable `since` never expires: parking on an ambiguous timestamp could park a colony that was
/// only just held, so those keep their slots.
pub(crate) fn hold_expired(session: &Session, now: DateTime<Utc>, timeout: chrono::Duration) -> bool {
    if session.status != SessionStatus::Idle {
        return false;
    }
    let Some(attention) = session.attention.as_ref() else {
        return false;
    };
    if attention.get("reason").and_then(Value::as_str) != Some(AUTOPILOT_HELD_REASON) {
        return false;
    }
    let Some(since) = attention
        .get("since")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
    else {
        return false;
    };
    now - since >= timeout
}

/// Whether this colony is parked specifically by the hold timeout (issue #876), the park shape the
/// backoff resume owns: a quota park carries its own reason and recovery, and an unparked colony no
/// record.
pub(crate) fn hold_parked(s: &Session) -> bool {
    s.status == SessionStatus::Parked && s.parked.as_ref().is_some_and(|p| p.reason == HOLD_TIMEOUT_REASON)
}

/// Whether a colony parked by the automatic provider-error retry (issue #980) is due to resume at
/// `now`: it is parked for [`PROVIDER_RETRY_REASON`] and the delay for the attempt it is backing off
/// (`provider_retries - 1` into [`PROVIDER_RETRY_SCHEDULE_MINUTES`]) has passed since the park's
/// `at`. Pure, so the backoff is testable apart from the tick that acts on it.
pub(crate) fn provider_retry_due(s: &Session, now: DateTime<Utc>) -> bool {
    if s.status != SessionStatus::Parked {
        return false;
    }
    let Some(park) = s.parked.as_ref().filter(|p| p.reason == PROVIDER_RETRY_REASON) else {
        return false;
    };
    let idx = (s.provider_retries.saturating_sub(1) as usize).min(PROVIDER_RETRY_SCHEDULE_MINUTES.len() - 1);
    now >= park.at + chrono::Duration::minutes(PROVIDER_RETRY_SCHEDULE_MINUTES[idx])
}

/// What the queue does with a hold-parked colony on one tick (issue #876), decided as a pure function
/// so the backoff policy is testable apart from the tick that acts on it (the `autopilot_step` pattern).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldParkAction {
    /// Leave it parked: no judge to answer for it, the next step's delay has not come, or another
    /// kind of park entirely.
    Wait,
    /// The next step is due: resume it, carrying [`HOLD_RESUME_NOTE`].
    Resume,
    /// Its question is above the judge's ceiling: never auto-resumed, notify a person once.
    NotifyOnly,
    /// Every step is spent and it parked again: give up, failing it with [`ABANDONED_QUESTION_REASON`].
    GiveUp,
}

/// The hold-timeout backoff's verdict for one colony at `now` (issue #876). A colony parked by the
/// hold timeout while the autonomy judge is configured gets up to [`HOLD_RESUME_SCHEDULE`] resumes,
/// each `schedule[hold_resumes]` after the park, so one that keeps parking on the same unanswered
/// question reaches a person instead of spinning a microVM up for ever. A question above the judge's
/// ceiling is never auto-resumed, only notified; with no judge nothing is auto-resumed, and a park the
/// hold timeout did not make is left alone.
pub(crate) fn hold_park_action(session: &Session, now: DateTime<Utc>, judge: Option<&crate::autonomy::Judge>) -> HoldParkAction {
    if !hold_parked(session) {
        return HoldParkAction::Wait;
    }
    let Some(judge) = judge else {
        return HoldParkAction::Wait;
    };
    let park = session.parked.as_ref().expect("hold_parked checked the record");
    // A risk above the ceiling is the one thing the backoff must never paper over; a park with no
    // question (risk `None`) counts as within it, since there is nothing to answer.
    if park
        .question_risk
        .is_some_and(|risk| !crate::autonomy::within_ceiling(risk, judge.risk_ceiling))
    {
        return HoldParkAction::NotifyOnly;
    }
    let Some(delay) = HOLD_RESUME_SCHEDULE.get(session.hold_resumes as usize) else {
        return HoldParkAction::GiveUp;
    };
    if now >= park.at + *delay {
        HoldParkAction::Resume
    } else {
        HoldParkAction::Wait
    }
}

/// How a queued colony's log line names the limits it is waiting on.
pub(crate) fn limits_message(max_parallel: usize, org_limit: Option<u64>, repo_limit: u64) -> String {
    let org = org_limit.map(|n| format!(", {n} for this org")).unwrap_or_default();
    format!("the parallel limit is {max_parallel}{org}, {repo_limit} per repository")
}

/// Check for a free slot and claim it without letting go of the lock in between: `claim` runs while the
/// write guard is still held, so nothing can slip between the check and the claim and two launches can
/// never both take the last free slot. The limits must be resolved before calling this (`org_settings`
/// does blocking file IO), and `claim` must not `.await` anything.
pub(crate) async fn with_slot<T>(
    sessions: &RwLock<Vec<Session>>,
    org: &str,
    repo: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    repo_limit: u64,
    claim: impl FnOnce(&mut Vec<Session>, bool) -> T,
) -> T {
    let mut guard = sessions.write().await;
    let room = has_room(&guard, org, repo, max_parallel, org_limit, repo_limit);
    claim(&mut guard, room)
}

/// When the queue loop last came round, as Unix seconds; 0 until its first tick. A wedged tick
/// (a `start_queued` that never returns) leaves it to age, which is what member health reads as
/// "colony runner not ticking" (issue #764).
static LAST_TICK: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Seconds since the queue loop last ticked, or `None` before its first tick.
pub fn last_tick_age_s() -> Option<i64> {
    match LAST_TICK.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        at => Some((chrono::Utc::now().timestamp() - at).max(0)),
    }
}

/// Starts queued colonies as slots free up, oldest first. A colony whose org or repository is at its own
/// limit doesn't hold up the ones behind it, and neither does one waiting for the branch of the colony it
/// is stacked on.
pub async fn run_queue(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        LAST_TICK.store(chrono::Utc::now().timestamp(), std::sync::atomic::Ordering::Relaxed);
        start_queued(&app).await;
    }
}

/// What the queue decided to do with the queued colony it examined under the admission lock. `None` from
/// [`claim_queued`] means this tick leaves it be: the room it counted on vanished, or the colony was
/// claimed or removed in between, and the next tick looks again.
enum Claim {
    /// The colony is admitted: it is `Starting`, and the caller boots it.
    Start(Session),
    /// The colony can never start, so the claim has taken it out of the queue; the message says why
    /// and the caller says so before moving on to the colonies behind it.
    Retire(Session, String),
    /// The colony keeps its place: a `claim_wait` waiter still held at the lock was re-pointed
    /// (issue #321), and the next candidate is looked at now rather than on a later tick.
    Wait,
    /// The colony keeps its place but moves onto a different parent (issue #982): the one it was
    /// stacked on failed, so it re-parents onto that parent's own parent — one generation per tick —
    /// rather than being retired. Nothing failed, so the colony stays `Queued`; the caller persists
    /// and broadcasts the moved record.
    Reparent(Session),
}

/// What holds a queued colony back before the slot rules even apply, decided on the snapshot: the
/// queueing rule (`restack::queue_decision`) is the decision, and this is the queue's reading of it.
enum Gate {
    /// Nothing holds it back; the slot rules decide, as for any colony.
    Admit,
    /// What it waits on is not ready yet: its stacked-on parent's branch, or the live colony it
    /// queued behind for overlap. Look past it this tick — a colony waiting on a slow holder must
    /// not stall the colonies behind it — and look again next tick.
    Hold,
    /// Its parent can never provide a branch; the message names the parent and says why.
    Retire(String),
    /// Its parent failed, but that parent's own parent — the colony one rung up the stack — might
    /// still provide a branch (issue #982). The child re-parents onto it rather than failing, one
    /// generation per 5 s tick, converging onto a live ancestor. A failed parent with no parent of
    /// its own has nothing to move onto and yields `Hold` instead — the child waits forever rather
    /// than failing, since the record is gone only when the parent itself is.
    Reparent(String),
}

fn gate(s: &Session, sessions: &[Session]) -> Gate {
    // Issue #881: a colony retrying a transient boot failure waits out its backoff — held, looked
    // past this tick like any other wait, so it never stalls the colonies behind it. Ahead of the
    // resume fast-path below, which would otherwise admit the kept worktree at once; the 5 s tick
    // picks it up as soon as `retry_at` passes.
    if s.retry_at.is_some_and(|at| at > Utc::now()) {
        return Gate::Hold;
    }
    // Issue #673: a colony a merge superseded stays put — held, not retired — until the operator
    // keeps it. Ahead of the resume fast-path below, which would otherwise admit a resumed-queued
    // colony the marker must hold; looked past this tick like any other wait.
    if crate::supersede::blocks_start(s) {
        return Gate::Hold;
    }
    // A colony with a kept worktree came from Resume, and resuming reuses the base it recorded at
    // its first boot: `boot_inner` never consults its parent again, because the branch already
    // exists on top of that base. The parent rule below is for fresh boots only — applied to a
    // resume it would fail a colony for a branch it does not need, typically one whose parent's
    // publish failed after the child had already built on it. `start_queued` reads the same flag to
    // decide that the boot it spawns is a resume.
    if s.git_admin_dir.is_some() {
        return Gate::Admit;
    }
    // Issue #321: a `claim_wait` waiter starts only when the effective holder of its issue is
    // itself — its holder finished and it is the oldest waiter, so no later launch can jump it.
    // Until then it holds, looked past this tick like a parent-wait, behind whoever holds now.
    if s.claim_wait
        && let Some(issue) = s.issue
        && issue_held_by(sessions, &s.repo, issue).is_some_and(|holder| holder.id != s.id)
    {
        return Gate::Hold;
    }
    // Issue #453: a colony queued behind a live same-repo colony for overlap waits until that
    // colony is no longer live — finished, or gone entirely, which releases it at once. Looked
    // past this tick like a parent-wait, so it never stalls the colonies behind it.
    if let Some(holder) = s.queued_behind.as_deref()
        && sessions.iter().any(|p| p.id == holder && p.status.is_live())
    {
        return Gate::Hold;
    }
    let Some(parent_id) = s.parent.as_deref() else {
        return Gate::Admit;
    };
    let parent = sessions.iter().find(|p| p.id == parent_id);
    // Issue #982: a failed parent can never provide a branch, but its own parent — one rung up the
    // stack — might still. Re-parent the child onto it rather than failing the child (which would
    // cascade the failure down every colony stacked on it), one generation per tick until it lands
    // on a live ancestor. A failed parent with no parent of its own has nothing to move onto, and
    // failing the child for it would be the same cascade with no way out: it waits instead. This
    // sits ahead of `restack::queue_decision`, which still refuses a `Failed` parent on its own.
    if let Some(parent) = parent
        && parent.status == SessionStatus::Failed
    {
        return match parent.parent.clone() {
            Some(grandparent) => Gate::Reparent(grandparent),
            None => Gate::Hold,
        };
    }
    match restack::queue_decision(parent_id, parent, s.stack) {
        Stacked::Ready(_) => Gate::Admit,
        Stacked::Wait => Gate::Hold,
        Stacked::Refuse(reason) => Gate::Retire(reason),
    }
}

/// The queue's decision for one colony, made while the admission lock is held. A colony that is still
/// queued and still has everything a boot needs is started; one that was cleaned up while it waited can
/// never start and is retired instead — retired, not merely skipped, or it would sit `Queued` at the head
/// of the queue and block every tick and every colony behind it.
fn claim_queued(s: &mut Session, room: bool) -> Option<Claim> {
    if !room {
        return None; // the slot vanished between the snapshot and the lock; wait for the next tick
    }
    if s.status != SessionStatus::Queued {
        return None; // another tick claimed it between the snapshot and the lock
    }
    if s.cleaned_up {
        // Defence in depth: `cleanup` now claims `cleaned_up` under the colony's lifecycle lock, and
        // its claim refuses a queued colony, so nothing should mark a queued colony cleaned up any
        // more. If one ever does anyway, the worktree and branch are gone — starting it would boot
        // onto a worktree that no longer exists, and `can_resume` would never take it back afterwards.
        s.status = SessionStatus::Failed;
        s.error = Some("cleaned up while it was waiting in the queue, so there is no worktree left to start on".into());
        // Written under the list lock, not through `update_session`, so the unseen mark it would
        // set at this crossing is set here (issue #744).
        s.unseen_failure = true;
        let cleared = s.clear_attention();
        s.updated_at = Utc::now();
        let mut message = String::from("was cleaned up while it waited in the queue, so it can never start");
        if let Some(note) = cleared_attention_message(&cleared) {
            message.push_str("; ");
            message.push_str(&note);
        }
        return Some(Claim::Retire(s.clone(), message));
    }
    s.status = SessionStatus::Starting;
    // A colony re-queued after a boot that died part way still carries that boot's phases, and one
    // released from overlap queueing still names its holder: both belong to the wait, not the run.
    s.boot_timing = None;
    s.queued_behind = None;
    // A promoted claim_wait waiter (issue #321) stops waiting here: the issue is its own, and the
    // caller publishes the GitHub claim it deliberately never made at admission.
    s.claim_wait = false;
    s.updated_at = Utc::now();
    Some(Claim::Start(s.clone()))
}

/// Issue #321: what still holds a `claim_wait` waiter back, so the decision can be re-checked under
/// the admission lock and the pointer kept true. `Some(holder)` while someone else effectively
/// holds its issue — the colony the waiter shows itself behind — and `None` once the issue is the
/// waiter's own and it may start.
fn waiter_still_held(s: &Session, sessions: &[Session]) -> Option<String> {
    let issue = s.issue?;
    if !s.claim_wait {
        return None;
    }
    issue_held_by(sessions, &s.repo, issue)
        .filter(|holder| holder.id != s.id)
        .map(|holder| holder.id)
}

/// Issue #321: the remote check a waiter's promotion must pass first — the same one a fresh launch
/// runs, with the same fallback: a failed lookup leaves the local guard holding, so an outage
/// retires nothing. A holder whose pull request merged is decided locally, with no call at all:
/// the work has landed, and promoting would redo it.
async fn waiter_remote_conflict(app: &Shared, waiter: &Session, sessions: &[Session]) -> Option<String> {
    let issue = waiter.issue?;
    if sessions
        .iter()
        .any(|s| s.repo == waiter.repo && s.issue == Some(issue) && s.status == SessionStatus::Merged)
    {
        return Some("the holder's pull request was merged; the issue is done".into());
    }
    let info = match crate::claims::check_remote_claim(app, &waiter.repo, issue).await {
        Ok(info) => info,
        Err(e) => {
            eprintln!(
                "claims: remote claim check for waiter {} on {}#{issue} failed ({e:#}); the local guard decides",
                waiter.id, waiter.repo
            );
            return None;
        }
    };
    let our_colonies: Vec<&str> = sessions
        .iter()
        .filter(|s| s.repo == waiter.repo && s.issue == Some(issue))
        .map(|s| s.id.as_str())
        .collect();
    crate::claims::claim_wait_conflict(info.as_ref(), issue, &our_colonies)
}

/// A queued colony stacked on a parent that can never provide a branch is failed with the reason, out
/// of the queue: retired, not merely skipped, or it would sit `Queued` at the head of the queue
/// forever. Retiring takes no slot, so it does not wait for one.
fn claim_refused(s: &mut Session, reason: &str) -> Option<Claim> {
    if s.status != SessionStatus::Queued {
        return None; // claimed by something else between the snapshot and the lock
    }
    s.status = SessionStatus::Failed;
    s.error = Some(reason.to_string());
    s.unseen_failure = true; // as above: this crossing bypasses `update_session` (issue #744)
    let cleared = s.clear_attention();
    s.updated_at = Utc::now();
    let mut message = format!("can never start: {reason}");
    if let Some(note) = cleared_attention_message(&cleared) {
        message.push_str("; ");
        message.push_str(&note);
    }
    Some(Claim::Retire(s.clone(), message))
}

/// Issue #982: a queued colony whose parent failed moves onto that parent's own parent instead of
/// being retired for a branch nobody could lend. It stays `Queued` — nothing failed — and the next
/// tick decides afresh against its new parent, walking up the stack one rung at a time. Guarded on
/// the status like [`claim_refused`]: a colony claimed between the snapshot and the lock is left be.
fn claim_reparent(s: &mut Session, new_parent_id: &str) -> Option<Claim> {
    if s.status != SessionStatus::Queued {
        return None; // claimed by something else between the snapshot and the lock
    }
    s.parent = Some(new_parent_id.to_string());
    s.updated_at = Utc::now();
    Some(Claim::Reparent(s.clone()))
}

/// The queued colony this tick acts on, oldest first: the first one nothing holds back and that fits.
/// A colony still waiting for its stacked-on parent's branch is looked past, so a slow parent cannot
/// stall the colonies behind it, and the first one whose parent can never provide a branch stops the
/// walk — it is retired where it stands, which needs no slot — as does the first one whose parent
/// failed and can be re-parented up the stack (issue #982), which likewise takes no slot. `Hold` is
/// filtered here and never returned; `None` when nothing in the queue can move this tick.
fn next_queued(sessions: &[Session], room: impl Fn(&Session) -> bool) -> Option<(&Session, Gate)> {
    let mut waiting: Vec<&Session> = sessions.iter().filter(|s| s.status == SessionStatus::Queued).collect();
    waiting.sort_by_key(|s| s.created_at);
    for candidate in waiting {
        match gate(candidate, sessions) {
            Gate::Hold => continue,
            Gate::Retire(reason) => return Some((candidate, Gate::Retire(reason))),
            Gate::Reparent(new_parent) => return Some((candidate, Gate::Reparent(new_parent))),
            Gate::Admit => {
                if room(candidate) {
                    return Some((candidate, Gate::Admit));
                }
            }
        }
    }
    None
}

pub(crate) async fn start_queued(app: &Shared) {
    // Quota-parked colonies whose provider recovered rejoin the queue on this same 5 s tick, ahead
    // of admission; a colony that only just parked keeps its terminal state until its reset passes.
    resume_quota_parked(app).await;
    // Read once per tick, so a settings save applies at once — to the hold timeout here and the
    // suspension settings just below (issue #562).
    let modules = app.modules.read().await.clone();
    // Holds past their timeout park on this same tick, ahead of admission, so the slots they release
    // are visible to the loop below.
    park_expired_holds(app, orgs::hold_timeout(&modules)).await;
    // Hold-parked colonies whose backoff step is due resume on this same tick (issue #876), after
    // the parks above so a colony just parked is not yet due; the resumes queue behind the slot
    // rules like any other.
    resume_hold_parked(app).await;
    // Colonies parked by the automatic provider-error retry (issue #980) whose backoff step is due
    // resume on this same tick, after the parks above.
    resume_provider_retry_parked(app).await;
    // Colonies parked waiting on an unusable Claude account (issue #984) resume on this same tick
    // once the account works again — first the re-sign-in sweep, then the resumes it unblocks.
    resume_waiting_for_account(app).await;
    // Colonies whose question has waited past the grace period suspend on this same tick, ahead of
    // admission, for the same reason: the slots they release are visible below (issue #562).
    suspend_waiting_colonies(app, &modules).await;
    // Every routable provider's plan out: the queue holds, and `/api/status` says why. Checked per
    // tick rather than per colony, so a recovered provider unpauses the whole queue at once.
    if providers::quota_status(app).await.paused {
        return;
    }
    // Below the free-space floor: queued colonies that would start hold until the reclaim tick
    // frees room. The hold rides in `room`, not an early return: retiring a colony that can never
    // start takes no slot, and leaving it Queued would stall the queue head (and all the disk the
    // tick is trying to free behind it) for as long as the floor holds.
    let paused = crate::reclaim::admission_paused(app).await;
    // Issue #880: while draining for an update or a restart nothing new boots — a boot admitted
    // now would be torn down mid-boot by the restart. The hold rides with the disk pause: a colony
    // that can never start still retires below, so the queue head never sticks on the hold. This
    // snapshot only skips work the tick can already see is pointless; the drain is read again under
    // the admission lock below, where the claim actually happens.
    // Issue #1074: while GitHub refuses the account (suspended, a revoked token, secondary limits
    // that keep coming), nothing boots either — each boot would only fail against the refusal and
    // add one more call. Queued colonies keep their place and move once the breaker closes.
    let held = paused || app.drain.draining() || crate::github_breaker::paused(app).is_some();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Issue #321: a waiter whose holder changed says so. `queued_behind` follows whoever
    // effectively holds its issue now, so the second waiter shows it is queued behind the first
    // once the first takes over. A no-op write persists and broadcasts nothing.
    {
        let sessions = app.sessions.read().await.clone();
        for waiter in sessions.iter().filter(|s| s.claim_wait) {
            let Some(holder) = waiter_still_held(waiter, &sessions) else {
                continue;
            };
            if waiter.queued_behind.as_deref() == Some(holder.as_str()) {
                continue;
            }
            // Conditional on purpose: a promotion between the snapshot and this write must not
            // hang a stale pointer on a colony that has already started.
            app.update_session(&waiter.id, |x| {
                if x.claim_wait && x.status == SessionStatus::Queued {
                    x.queued_behind = Some(holder.clone());
                }
            })
            .await;
        }
    }
    // Before the fresh launches: a suspended colony holding an undelivered answer comes back ahead
    // of them — answering it is what the user has been waiting for (issue #562). The floor above
    // holds this back with every other start.
    if !held {
        restore_suspended(app, &modules).await;
    }
    // A colony routed to an account in trouble (issue #984) does not boot only to fail its first
    // turn: it holds its place in the queue until the account works again, exactly as the account's
    // parked colonies wait. Read once for the whole pass; the resumes above have just swept.
    let troubled_accounts: Vec<String> = crate::account_health::snapshot(app)
        .await
        .into_iter()
        .map(|(account, _)| account)
        .collect();
    // Several slots can free at once, so keep going until nothing else fits.
    loop {
        let sessions = app.sessions.read().await.clone();
        let limits = |org: &str| {
            let settings = app.org_settings(org);
            (orgs::org_max_parallel(&settings), repo_limit(&modules, &settings))
        };
        let Some((next, gate_result)) = next_queued(&sessions, |s| {
            let (org_limit, repo_limit) = limits(&s.org);
            !held
                && !troubled_accounts.contains(&crate::account_health::account_of(s))
                && has_room(&sessions, &s.org, &s.repo, max_parallel, org_limit, repo_limit)
        }) else {
            break;
        };
        // A waiter the gate called ready is checked against the forge before its promotion: the holder's
        // PR may have merged, or the claim may have moved to someone else, and the check is a gh call, so
        // it runs here rather than under the admission lock (issue #321). A failure falls back to the
        // local guard, the same as admission does. A conflict retires the waiter, exactly as the gate's
        // own refusal would; every other decision is carried through untouched.
        let gate_result = match gate_result {
            Gate::Admit if next.claim_wait => match waiter_remote_conflict(app, next, &sessions).await {
                Some(reason) => Gate::Retire(reason),
                None => Gate::Admit,
            },
            other => other,
        };
        // Re-checked and claimed under one write lock, so neither another tick nor a concurrent create or
        // resume can take the slot in between.
        let (org_limit, repo_limit) = limits(&next.org);
        let claimed = with_slot(
            &app.sessions,
            &next.org,
            &next.repo,
            max_parallel,
            org_limit,
            repo_limit,
            |sessions, room| {
                // Issue #321: the promotion the snapshot promised is re-checked under the lock —
                // a holder appearing, or an older waiter, between the two is caught here. A waiter
                // still held keeps its place in the queue and shows who it is behind now; the tick
                // moves on and looks again next time.
                let still_held = sessions
                    .iter()
                    .find(|s| s.id == next.id)
                    .and_then(|s| waiter_still_held(s, sessions));
                let s = sessions.iter_mut().find(|s| s.id == next.id)?;
                if let Some(holder) = still_held {
                    s.queued_behind = Some(holder);
                    return Some(Claim::Wait);
                }
                match gate_result {
                    Gate::Retire(reason) => claim_refused(s, &reason),
                    // Issue #982: the parent failed and its own parent may still lend a branch; move
                    // the child up the stack rather than retiring it.
                    Gate::Reparent(new_parent) => claim_reparent(s, &new_parent),
                    // The drain is re-read here, under the lock, beside `room`: one that began
                    // between the tick's snapshot and this claim must not let the boot through
                    // (issue #880). The colony keeps its place for the next tick.
                    Gate::Admit => claim_queued(
                        s,
                        room && !app.drain.draining() && crate::github_breaker::paused(app).is_none(),
                    ),
                    // `next_queued` filters `Hold` out of its result, so a held colony never reaches here.
                    Gate::Hold => unreachable!("next_queued never returns a held colony"),
                }
            },
        )
        .await;
        match claimed {
            None => break,
            Some(Claim::Retire(retired, message)) => {
                // A queued colony that can never start crosses straight into its terminal state
                // outside `update_session` (the claim writes the record directly), so the journal
                // hears about the return here, not there.
                spend::record_returned(app, &retired).await;
                crate::activity::record_transition(app, crate::sessions::SessionStatus::Queued, &retired).await;
                app.persist_and_broadcast(&retired).await;
                app.session_log(&retired.id, "warn", message).await;
                // Retired without ever booting: it frees the issue for a retry, on GitHub as well
                // as locally, the same as a boot that fails after starting.
                crate::claims::spawn_release_if_needed(app.clone(), &retired);
                continue;
            }
            Some(Claim::Wait) => {
                // The re-point was written under the lock; journal and broadcast it, then keep looking
                // for a candidate that can go now instead of waiting for the next tick.
                if let Some(updated) = app.session(&next.id).await {
                    app.persist_and_broadcast(&updated).await;
                }
                continue;
            }
            Some(Claim::Reparent(updated)) => {
                // Issue #982: the failed parent's own parent is who the child waits on now. Nothing
                // failed, so it stays Queued — journal and broadcast the move, then keep looking for
                // a candidate that can go this tick. The next tick decides afresh on the new parent,
                // so a chain walks up one rung at a time and converges on a live ancestor.
                app.persist_and_broadcast(&updated).await;
                app.session_log(
                    &updated.id,
                    "info",
                    "its parent failed; moved onto that parent's own parent instead of failing".into(),
                )
                .await;
                continue;
            }
            Some(Claim::Start(starting)) => {
                app.persist_and_broadcast(&starting).await;
                app.session_log(&next.id, "info", "a slot came free; starting".into()).await;
                // A promoted claim_wait waiter (issue #321) takes the issue's mark over: the claim
                // it never made at admission is published now, best effort, off this path.
                if next.claim_wait
                    && let Some(issue) = next.issue
                {
                    crate::claims::spawn_publish(app.clone(), next.repo.clone(), issue, next.id.clone());
                }
                // A colony that already has a worktree came from Resume, not Create: `git_admin_dir` stays None
                // until a colony's first boot has created the worktree (boot_inner), and Resume refuses colonies
                // without one (can_resume). Booting a resumed colony as fresh would try to create the worktree it
                // already has, so the queue carries the resume through.
                let resume = next.git_admin_dir.is_some();
                // Issue #98: the queue's admission is the approval when it boots a colony that
                // already has a worktree — a fresh Resume grant, checked in `boot`.
                let grant = resume.then(|| crate::lifecycle::mint_resume_grant(next, "queue"));
                tokio::spawn(boot(app.clone(), next.id.clone(), resume, grant));
            }
        }
    }
    // Last call on the tick, after everything holding an answer — the restores and the fresh
    // launches above — has had its say: a pre-warm request (issue #701) boots a suspended colony's
    // question with whatever slots are left, never ahead of a colony that already holds an answer.
    if !held {
        prewarm_requested(app, &modules).await;
    }
}

// ---------------------------------------------------------------------------
// agentd transport
// ---------------------------------------------------------------------------

/// Parks every autopilot hold past its timeout (issue #217, parked properly by #213): the slot is
/// released, the worktree and branch kept, and the park record carries the `hold_timeout` reason —
/// what [`resume_quota_parked`] keys its own reason on, so these are left for the operator. The
/// expiry is re-checked under the admission lock inside [`park_colony`]'s claim, so a colony
/// answered in the meantime is not parked, and the attention stamp lands in the same claim.
pub(crate) async fn park_expired_holds(app: &Shared, timeout: chrono::Duration) {
    let now = Utc::now();
    let ids: Vec<String> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| hold_expired(s, now, timeout))
            .map(|s| s.id.clone())
            .collect()
    };
    let minutes = timeout.num_minutes();
    for id in ids {
        let Some(s) = app.session(&id).await else { continue };
        park_colony(
            app,
            &s,
            HOLD_TIMEOUT_REASON,
            None,
            format!(
                "autopilot hold exceeded {minutes} min with no answer; parked to release its slot — the worktree is kept, so press Resume to continue"
            ),
            format!("autopilot hold exceeded {minutes} min; parked to release its slot — worktree kept, resume to continue"),
        )
        .await;
    }
}

/// Acts on the hold-timeout backoff for every hold-parked colony (issue #876), on the same tick as
/// [`park_expired_holds`]: a due step stamps [`HOLD_RESUME_NOTE`], bumps `hold_resumes` and resumes
/// (queuing when no slot is free), an above-ceiling question raises [`HOLD_UNANSWERED_REASON`] once,
/// and a colony through every step is failed with [`ABANDONED_QUESTION_REASON`], the worktree kept.
/// Each write re-checks the park under the lock, and the resume handler re-checks again.
pub(crate) async fn resume_hold_parked(app: &Shared) {
    let judge = crate::autonomy::judge(&app.modules.read().await.clone(), &app.agents);
    let now = Utc::now();
    let actions: Vec<(String, HoldParkAction)> = {
        let sessions = app.sessions.read().await;
        sessions
            .iter()
            // Issue #673: a merge covered this colony's work — it stays parked, park record
            // intact, until it is kept; the tick after that resumes it like any other.
            .filter(|s| !crate::supersede::blocks_start(s))
            .map(|s| (s.id.clone(), hold_park_action(s, now, judge.as_ref())))
            .filter(|(_, action)| *action != HoldParkAction::Wait)
            .collect()
    };
    for (id, action) in actions {
        match action {
            HoldParkAction::Wait => {}
            HoldParkAction::Resume => {
                let stamped = app
                    .update_session(&id, |x| {
                        if !hold_parked(x) {
                            return false;
                        }
                        x.resume_note = Some(HOLD_RESUME_NOTE.to_string());
                        x.hold_resumes = x.hold_resumes.saturating_add(1);
                        true
                    })
                    .await
                    .is_some_and(|(_, stamped)| stamped);
                if stamped {
                    app.session_log(
                        &id,
                        "info",
                        "the question's hold timed out; resuming once more so the agent can choose without waiting".into(),
                    )
                    .await;
                    // The resume handler re-checks everything under its own locks (status, slot,
                    // supersession) and queues when none is free, so a colony an operator just
                    // touched is not doubled. A refusal (a race, a supersession, a failed rotation)
                    // leaves the colony parked: give the step back and take the note off, so a
                    // refusal neither burns the backoff nor rides a resume it was not written for.
                    if crate::lifecycle::resume(State(app.clone()), Path(id.clone()), None)
                        .await
                        .is_err()
                    {
                        app.update_session(&id, |x| {
                            if x.resume_note.as_deref() != Some(HOLD_RESUME_NOTE) {
                                return;
                            }
                            x.resume_note = None;
                            x.hold_resumes = x.hold_resumes.saturating_sub(1);
                        })
                        .await;
                    }
                }
            }
            HoldParkAction::NotifyOnly => {
                // No repeated write: identical attention is a no-op under `update_session`, and
                // the cockpit banner already names the park. `notify` fires once on the edge.
                app.update_session(&id, |x| {
                    if !hold_parked(x) {
                        return;
                    }
                    if let Some(attention) = x.attention.as_mut().and_then(Value::as_object_mut) {
                        attention.insert("reason".into(), Value::String(HOLD_UNANSWERED_REASON.into()));
                    }
                })
                .await;
            }
            HoldParkAction::GiveUp => {
                let mut attention = None;
                let failed = app
                    .update_session(&id, |x| {
                        if !hold_parked(x) {
                            return false;
                        }
                        x.status = SessionStatus::Failed;
                        x.error = Some(ABANDONED_QUESTION_REASON.to_string());
                        x.parked = None;
                        attention = x.clear_attention();
                        true
                    })
                    .await
                    .is_some_and(|(_, failed)| failed);
                if failed {
                    app.note_cleared_attention(&id, attention).await;
                    app.session_log(
                        &id,
                        "error",
                        "the question stayed unanswered through every retry; the colony failed and its worktree is kept for a person to resume".into(),
                    )
                    .await;
                    // A failed colony frees its issue for a retry, the same as any failure.
                    if let Some(s) = app.session(&id).await {
                        crate::claims::spawn_release_if_needed(app.clone(), &s);
                    }
                }
            }
        }
    }
}

/// Resumes every colony parked by the automatic provider-error retry (issue #980) whose backoff step
/// is due, on the same tick as [`resume_hold_parked`]: the colony is brought back through
/// `lifecycle::resume`, queuing behind the slot rules like any other resume. `provider_retries` is the
/// 1-based attempt the park was taken for and is indexed by [`provider_retry_due`]; nothing here bumps
/// it — events.rs owns the counter, and only a successful turn or the final give-up resets it.
pub(crate) async fn resume_provider_retry_parked(app: &Shared) {
    let now = Utc::now();
    let ids: Vec<String> = {
        let sessions = app.sessions.read().await;
        sessions
            .iter()
            // Issue #673: a merge covered this colony's work — it stays parked until it is kept.
            .filter(|s| !crate::supersede::blocks_start(s))
            .filter(|s| provider_retry_due(s, now))
            .map(|s| s.id.clone())
            .collect()
    };
    for id in ids {
        // The id came from an earlier read snapshot, so re-check it under the lock right before
        // resuming, exactly as `resume_hold_parked` does above: an operator may have stopped or
        // answered the colony in between, and `can_resume` still admits a `Stopped` colony
        // (lifecycle.rs), so without this the sweep would boot one the operator just stopped. A
        // colony no longer parked for the retry, or no longer due, is left alone — `resume` on its
        // own does not know this reason. A refusal (a race, a supersession, a failed rotation)
        // leaves it parked; the next tick looks again.
        let still_due = app
            .update_session(&id, |x| provider_retry_due(x, now))
            .await
            .is_some_and(|(_, due)| due);
        if !still_due {
            continue;
        }
        if crate::lifecycle::resume(State(app.clone()), Path(id.clone()), None)
            .await
            .is_ok()
        {
            app.session_log(&id, "info", "resuming automatically after a transient provider error".into())
                .await;
        }
    }
}

/// Resumes every colony parked waiting on a Claude account (issue #984) on the same tick as
/// [`resume_provider_retry_parked`]. A re-sign-in rewrites the credential file, so
/// [`crate::account_health::sweep_credentials`] runs first: an account whose stamp moved is cleared
/// and its colonies rejoin the queue; one still in trouble waits for the next tick.
pub(crate) async fn resume_waiting_for_account(app: &Shared) {
    crate::account_health::sweep_credentials(app).await;
    let ids: Vec<String> = {
        let sessions = app.sessions.read().await;
        sessions
            .iter()
            // Issue #673: a merge covered this colony's work — it stays parked until it is kept.
            .filter(|s| !crate::supersede::blocks_start(s))
            .filter(|s| waiting_for_account(s))
            .map(|s| s.id.clone())
            .collect()
    };
    for id in ids {
        // Re-checked under the lock right before resuming, as `resume_provider_retry_parked` does: a
        // colony no longer parked for this reason, or an operator who just stopped it, is left alone.
        let still_waiting = app
            .update_session(&id, |x| waiting_for_account(x))
            .await
            .is_some_and(|(_, waiting)| waiting);
        if !still_waiting {
            continue;
        }
        let Some(s) = app.session(&id).await else { continue };
        let account = crate::account_health::account_of(&s);
        if crate::account_health::troubled(app, &account).await.is_some() {
            continue;
        }
        if crate::lifecycle::resume(State(app.clone()), Path(id.clone()), None)
            .await
            .is_ok()
        {
            app.session_log(
                &id,
                "info",
                format!("resuming automatically: Claude account `{account}` works again"),
            )
            .await;
        }
    }
}

/// Whether this colony is parked waiting on a Claude account (issue #984), the park shape
/// [`resume_waiting_for_account`] owns. Pure, so the filter is testable apart from the tick.
pub(crate) fn waiting_for_account(s: &Session) -> bool {
    s.status == SessionStatus::Parked
        && s.parked
            .as_ref()
            .is_some_and(|p| p.reason == crate::account_health::WAITING_FOR_ACCOUNT_REASON)
}

// ---------------------------------------------------------------------------
// Suspend and restore (issue #562)
// ---------------------------------------------------------------------------

/// Whether this colony can be suspended: its agent's module declares a session transcript
/// directory (`session_resume` in module.json) and the runner has reported its session id. Both
/// are needed to bring the colony back with its conversation intact.
fn suspendable(s: &Session, agents: &[crate::modules::AgentModule]) -> bool {
    s.agent_session.is_some() && agents.iter().any(|a| a.id == s.agent && a.resume_dir.is_some())
}

/// Suspends colonies whose question has waited past the grace period (issue #562): the microVM is
/// torn down to free the slot, the worktree and the agent's session transcript are kept, and the
/// status stays `waiting_for_answer` — the question stays answerable, and answering re-boots the
/// colony, which [`restore_suspended`] brings back ahead of new launches. A colony whose agent
/// cannot resume its session is left running, said once per run. The claim is re-checked under the
/// write lock and the teardown holds the colony's lifecycle lock, the discipline of every stop: a
/// resume admitted by the claim waits for the teardown instead of booting a microVM this in-flight
/// removal then takes with it.
pub(crate) async fn suspend_waiting_colonies(app: &Shared, modules: &crate::config::ModulesConfig) {
    if !orgs::suspend_waiting(modules) {
        return;
    }
    let grace = orgs::suspend_after(modules);
    let now = Utc::now();
    let ids: Vec<String> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| s.status == SessionStatus::WaitingForAnswer && s.suspended.is_none())
            .map(|s| s.id.clone())
            .collect()
    };
    for id in ids {
        let Some(s) = app.session(&id).await else { continue };
        let rt = app.runtime(&id).await;
        if !suspendable(&s, &app.agents) {
            // Left running, said once per run rather than every tick.
            if !rt.suspend_skip_logged.swap(true, std::sync::atomic::Ordering::Relaxed) {
                app.session_log(
                    &id,
                    "info",
                    "this agent cannot resume its session, so the colony keeps its microVM while it waits for an answer".into(),
                )
                .await;
            }
            continue;
        }
        // The grace reads off the question's own timestamp, live or replayed; a colony whose wait
        // start is unknown never expires, the rule `hold_expired` also holds — parking on an
        // ambiguous timestamp could suspend a colony that was only just asked.
        let Some(since) = rt.activity.lock().await.question_since else {
            continue;
        };
        if now - since < grace {
            continue;
        }
        let cap = BLOCKING_QUESTION_CAP.max(grace);
        let lifecycle = app.session_lock(&id).await;
        let _lifecycle = lifecycle.lock().await;
        // The answer gate (issue #562): the open question's lock, held across the claim below. The
        // live answer path (`submit_answer`) holds this same lock across its not-suspended
        // re-check, its taking-down of the question and its send, so an answer and this claim
        // cannot interleave: an answer that went first reads here as no open question, and the
        // colony is skipped — its answer is in flight to a runner this tick must not tear down,
        // and the runner's own status events take it from here.
        let open_question = rt.open_question.lock().await;
        let Some(s) = app.session(&id).await else { continue };
        if s.status != SessionStatus::WaitingForAnswer || s.suspended.is_some() || open_question.is_none() {
            continue;
        }
        // A question that holds a tool call (issue #759) — an exec-policy `ask`, a subagent's
        // AskUserQuestion, an ACP permission request — is not the lead waiting between turns: its
        // tool call is blocked in flight on the answer, inside a live agent. Tearing the microVM
        // down kills that call and the agent that made it, and a resumed transcript cannot pick it
        // back up, so the lead only spawns another agent that asks again, or the answer reaches
        // nobody. Such a colony keeps its microVM (and its slot) until the question is answered, or
        // until [`BLOCKING_QUESTION_CAP`], past which it is suspended anyway and the log says what
        // that costs. Read under the gate: the flag is set before the question opens and cleared
        // with its answer.
        let blocking = rt.question_holds_tool_call.load(std::sync::atomic::Ordering::SeqCst);
        if blocking && now - since < cap {
            continue;
        }
        // How the colony comes back. The memory-snapshot store (issue #702) freezes the running
        // VM's memory while the colony waits; it sits behind `sandbox::supports_memory_snapshot`,
        // which is false because the pinned msb cannot restore a `--secret`-carrying sandbox (that
        // function has the measurement). So today the path is always the fallback — the agent
        // resumes its own session transcript in a fresh microVM — and `capture` seals the VM's
        // memory under a fresh per-colony key only when the gate turns on. Any capture failure is
        // the same fallback.
        let (path, snapshot) = if crate::sandbox::supports_memory_snapshot() {
            match crate::snapshot::capture(app, &id, &s.sandbox).await {
                Some(meta) => (crate::snapshot::MEMORY_SNAPSHOT, serde_json::to_value(&meta).ok()),
                None => (SESSION_RESUME, None),
            }
        } else {
            (SESSION_RESUME, None)
        };
        let claimed = app
            .update_session(&id, |x| {
                if x.status != SessionStatus::WaitingForAnswer || x.suspended.is_some() {
                    return false;
                }
                x.suspended = Some(Suspension {
                    at: Utc::now(),
                    snapshot: snapshot.clone(),
                    reason: WAITING_FOR_ANSWER.into(),
                    path: path.into(),
                });
                x.updated_at = Utc::now();
                true
            })
            .await
            .is_some_and(|(_, landed)| landed);
        if !claimed {
            continue;
        }
        // Claimed: the answer path can no longer forward into this runtime — it reads suspended
        // under the gate and holds instead — so the link may go.
        drop(open_question);
        let minutes = if blocking { cap.num_minutes() } else { grace.num_minutes() };
        let message = if blocking {
            format!(
                "no answer for {minutes} min to a question a tool call is blocked on; suspending anyway, \
                 so the colony stops holding a microVM and a slot — the agent that asked is lost with the \
                 microVM, the question stays answerable, and the answer reaches the lead agent when the \
                 colony resumes"
            )
        } else {
            format!(
                "no answer for {minutes} min; suspending — the microVM is removed, the worktree and the \
                 agent's session transcript are kept, and the question stays answerable"
            )
        };
        app.session_log(&id, if blocking { "warn" } else { "info" }, message).await;
        let mut entry = crate::activity::Entry::new("outcome.suspended", "colony").colony(&s);
        entry.detail = Some(format!(
            "waiting {minutes} min for an answer; the worktree and the agent's session are kept"
        ));
        crate::activity::record(app, entry).await;
        teardown_vm(app, &s).await;
    }
}

/// When this colony's turn in the restore line comes (issue #667): the moment its answer arrived,
/// falling back to the suspension's own for records saved before answers kept a time, then to the
/// last update — the order the restore pass ran by before then.
fn restore_key(s: &Session) -> DateTime<Utc> {
    s.pending_answer
        .as_ref()
        .and_then(|a| a.answered_at)
        .or_else(|| s.suspended.as_ref().map(|x| x.at))
        .unwrap_or(s.updated_at)
}

/// How many other answered colonies stand ahead of this one in the restore line: the suspended,
/// answered, still-waiting colonies ordered in front of it, by the same key and id tie-break the
/// restore pass itself sorts by.
fn answered_ahead(sessions: &[Session], s: &Session) -> usize {
    let key = (restore_key(s), s.id.as_str());
    sessions
        .iter()
        .filter(|other| {
            other.suspended.is_some()
                && other.pending_answer.is_some()
                && other.status == SessionStatus::WaitingForAnswer
                // Issue #673: a superseded colony that is not kept is out of the line altogether.
                && !crate::supersede::blocks_start(other)
                && (restore_key(other), other.id.as_str()) < key
        })
        .count()
}

/// The part of a held answer's log line that says where the colony stands in the restore line
/// (issue #667): whether the next tick can bring it back at once, or how many answered colonies
/// are ahead of it. `has_room` against the same snapshot and limits the restore pass itself
/// answers to decides whether a slot is free right now, and `paused` — the tick's own admission
/// hold, which stops the restore pass with everything else — keeps the note from promising a
/// next-tick resume the tick cannot make.
pub(crate) fn restore_line_note(
    sessions: &[Session],
    s: &Session,
    max_parallel: usize,
    org_limit: Option<u64>,
    repo_limit: u64,
    paused: bool,
) -> String {
    // Issue #673: the restore pass skips a colony a merge superseded until it is kept, so the note
    // must not promise a resume — it says what the colony is waiting on instead.
    if crate::supersede::blocks_start(s) {
        return "a merged pull request superseded this colony, so it waits until it is kept".into();
    }
    let ahead = answered_ahead(sessions, s);
    let room = has_room(sessions, &s.org, &s.repo, max_parallel, org_limit, repo_limit);
    match (ahead, room, paused) {
        (0, true, false) => "a slot is free, so it resumes on the next queue tick".into(),
        (0, _, true) => "launches are paused; it is next in line once the pause lifts".into(),
        (0, false, false) => "it is next in line when a slot frees".into(),
        (n, true, _) => format!("{n} answered colon{} ahead of it", if n == 1 { "y is" } else { "ies are" }),
        (n, false, _) => format!(
            "all slots are busy and {n} answered colon{} ahead of it",
            if n == 1 { "y is" } else { "ies are" }
        ),
    }
}

/// The clears every claim that boots a colony makes: the last boot's phases would read as this
/// one's under `starting`, and stale mesh and connection state would outlive the microVM they name.
fn claimed_for_boot(x: &mut Session) {
    x.error = None;
    x.attention = None;
    x.mesh = None;
    x.local_port = None;
    // A boot gets a fresh microVM, so the old one's preview port is gone (previews.rs).
    x.preview_port = None;
    x.boot_timing = None;
    x.updated_at = Utc::now();
}

/// The colony is claimed and about to boot: retire its old agent link and move the stale event log
/// aside, with the log's own file lock held across the rename (`rotate_events` has the why). A
/// rotation failure runs the given revert — putting the colony back under the claim's own
/// re-checks — logs, and answers `false`, ending the caller's tick: a retry every 5 s would only
/// churn on a storage problem.
async fn retire_and_rotate(app: &Shared, id: &str, revert: impl FnOnce(&mut Session), kept: &str) -> bool {
    let runtime = app.runtimes.lock().await.remove(id);
    if let Some(rt) = &runtime {
        rt.stop.send_replace(true);
        rt.retired.send_replace(true);
    }
    let rotated = {
        let _file_lock = match runtime.as_ref() {
            Some(rt) => Some(rt.file_lock.lock().await),
            None => None,
        };
        rotate_events(app.store(), id).await
    };
    if let Err(e) = rotated {
        let e = anyhow::Error::from(e);
        if let Some((x, ())) = app.update_session(id, revert).await {
            app.persist_and_broadcast(&x).await;
        }
        app.storage_failed("rotate the old event log", &e).await;
        app.session_log(
            id,
            "error",
            format!("could not move the old event log aside ({e}); the colony stays suspended and its {kept}"),
        )
        .await;
        return false;
    }
    true
}

/// Restores suspended colonies that hold an undelivered answer, ahead of the fresh launches in the
/// admission loop (issue #562): the answer is what the user has been waiting for. They come back in
/// answer order (issue #667), not suspension order. Each restore claims a slot through the same
/// admission every launch answers to; the claim clears the suspension — so `holds_slot` is true for
/// the boot — but keeps the answer, which the boot itself delivers once the runner is up, so a
/// failed boot leaves it on the record. The link drop and the event-log rotation are the resume
/// handler's, for the same reason: the fresh microVM's agentd numbers events from 1, and a stale
/// log would swallow them.
pub(crate) async fn restore_suspended(app: &Shared, modules: &crate::config::ModulesConfig) {
    let mut candidates: Vec<(DateTime<Utc>, String)> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| s.suspended.is_some() && s.pending_answer.is_some() && s.status == SessionStatus::WaitingForAnswer)
            // Issue #673: a merge covered this colony's work — the answer can wait until it is kept.
            .filter(|s| !crate::supersede::blocks_start(s))
            .map(|s| (restore_key(s), s.id.clone()))
            .collect()
    };
    candidates.sort();
    let max_parallel = orgs::global_max_parallel(modules) as usize;
    for (_, id) in candidates {
        let Some(s) = app.session(&id).await else { continue };
        let settings = app.org_settings(&s.org);
        let (org_limit, repo_limit) = (orgs::org_max_parallel(&settings), repo_limit(modules, &settings));
        // The snapshot decides whether to try; the claim re-checks under the lock. One that does
        // not fit right now does not stop the tick: a later candidate of another repository might.
        {
            let sessions = app.sessions.read().await;
            if !has_room(&sessions, &s.org, &s.repo, max_parallel, org_limit, repo_limit) {
                continue;
            }
        }
        let lifecycle = app.session_lock(&id).await;
        let _lifecycle = lifecycle.lock().await;
        let Some(s) = app.session(&id).await else { continue };
        // Issue #673 re-checked under the lifecycle lock: a merge seen since the snapshot holds it.
        if s.status != SessionStatus::WaitingForAnswer
            || s.suspended.is_none()
            || s.pending_answer.is_none()
            || crate::supersede::blocks_start(&s)
        {
            continue;
        }
        let suspension = s.suspended.clone();
        let claimed = with_slot(
            &app.sessions,
            &s.org,
            &s.repo,
            max_parallel,
            org_limit,
            repo_limit,
            |sessions, room| {
                let x = sessions.iter_mut().find(|x| x.id == id)?;
                if !room || x.status != SessionStatus::WaitingForAnswer || x.suspended.is_none() || x.pending_answer.is_none() {
                    return None;
                }
                x.status = SessionStatus::Starting;
                // Told to the boot for session.json's `restore` (issue #700) before the flag goes.
                x.was_suspended = x.suspended.is_some();
                x.suspended = None;
                // A pre-warm request on top of a held answer is stale: this boot delivers the
                // answer, so there is nothing left to warm.
                x.prewarm = None;
                claimed_for_boot(x);
                Some(x.clone())
            },
        )
        .await;
        let Some(s) = claimed else { continue };
        app.persist_and_broadcast(&s).await;
        let reverted = |x: &mut Session| {
            x.status = SessionStatus::WaitingForAnswer;
            x.suspended = suspension.clone();
        };
        if !retire_and_rotate(app, &id, reverted, "answer kept").await {
            break;
        }
        app.session_log(
            &id,
            "info",
            "answer in hand; booting a fresh microVM to resume the agent's session with it".into(),
        )
        .await;
        let resume = s.git_admin_dir.is_some();
        // Issue #98: delivering the answer boots a kept worktree — the same authorized effect as
        // any other resume, so it carries its own fresh grant into `boot`.
        let grant = resume.then(|| crate::lifecycle::mint_resume_grant(&s, "harness"));
        tokio::spawn(boot(app.clone(), id, resume, grant));
    }
}

/// Boots suspended colonies whose question someone has opened (issue #701), after the restores and
/// the fresh launches on the same tick: the answer then lands in an already-running VM instead of
/// costing a second boot. A request that has waited for a slot past the pre-warm timeout is dropped
/// rather than booted — whoever opened the question has stopped looking. Each boot goes through the
/// same admission the restores answer to; the claim stamps `started_at` but keeps the suspension,
/// because the colony is not back yet — it is warming, and the suspension is what answers to the
/// question until an answer lands ([`deliver_prewarmed`]) or the timeout gives up
/// ([`prewarm_expire`]). The link drop and the event-log rotation are this pass's, like the
/// restore's: the fresh microVM's agentd numbers events from 1, and a stale log would swallow them.
pub(crate) async fn prewarm_requested(app: &Shared, modules: &crate::config::ModulesConfig) {
    let timeout = orgs::prewarm_timeout(modules);
    let now = Utc::now();
    let mut candidates: Vec<(DateTime<Utc>, String)> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| {
                s.status == SessionStatus::WaitingForAnswer
                    && s.suspended.is_some()
                    && s.pending_answer.is_none()
                    && s.prewarm.as_ref().is_some_and(|p| p.started_at.is_none())
                    // Issue #673: a merge covered this colony's work — like the restore pass, no
                    // warm-up until it is kept.
                    && !crate::supersede::blocks_start(s)
            })
            .map(|s| (s.prewarm.as_ref().map(|p| p.requested_at).unwrap_or(now), s.id.clone()))
            .collect()
    };
    // Oldest request first: whoever has been looking longest warms first.
    candidates.sort();
    let max_parallel = orgs::global_max_parallel(modules) as usize;
    for (requested_at, id) in candidates {
        if now - requested_at >= timeout {
            // Stale: the question was opened and left. A cheap claim — no boot, no lifecycle lock.
            app.update_session(&id, |x| {
                if x.prewarm.as_ref().is_some_and(|p| p.started_at.is_none()) {
                    x.prewarm = None;
                }
            })
            .await;
            continue;
        }
        let Some(s) = app.session(&id).await else { continue };
        let settings = app.org_settings(&s.org);
        let (org_limit, repo_limit) = (orgs::org_max_parallel(&settings), repo_limit(modules, &settings));
        // The snapshot decides whether to try; the claim re-checks under the lock, as in the
        // restore pass above.
        {
            let sessions = app.sessions.read().await;
            if !has_room(&sessions, &s.org, &s.repo, max_parallel, org_limit, repo_limit) {
                continue;
            }
        }
        let lifecycle = app.session_lock(&id).await;
        let _lifecycle = lifecycle.lock().await;
        let Some(s) = app.session(&id).await else { continue };
        if s.status != SessionStatus::WaitingForAnswer
            || s.suspended.is_none()
            || s.pending_answer.is_some()
            || s.prewarm.as_ref().is_none_or(|p| p.started_at.is_some())
            || crate::supersede::blocks_start(&s)
        {
            continue;
        }
        // The question the fresh transcript has to carry, read off the old runtime before it
        // retires. `runtime` (get-or-create), not the map: after a restart nothing may have
        // materialized it yet, and the load replays the log the question was asked in.
        let old = app.runtime(&id).await;
        let question = old.open_question.lock().await.clone();
        let question_since = old.activity.lock().await.question_since;
        let claimed = with_slot(
            &app.sessions,
            &s.org,
            &s.repo,
            max_parallel,
            org_limit,
            repo_limit,
            |sessions, room| {
                let x = sessions.iter_mut().find(|x| x.id == id)?;
                if !room
                    || x.status != SessionStatus::WaitingForAnswer
                    || x.suspended.is_none()
                    || x.pending_answer.is_some()
                    || x.prewarm.as_ref().is_none_or(|p| p.started_at.is_some())
                    || crate::supersede::blocks_start(x)
                {
                    return None;
                }
                x.status = SessionStatus::Starting;
                if let Some(p) = x.prewarm.as_mut() {
                    p.started_at = Some(Utc::now());
                }
                // The warm-up boot restores a suspension (issue #700): session.json's `restore`
                // says so, though the suspension itself stays until an answer lands.
                x.was_suspended = x.suspended.is_some();
                claimed_for_boot(x);
                Some(x.clone())
            },
        )
        .await;
        let Some(s) = claimed else { continue };
        app.persist_and_broadcast(&s).await;
        let reverted = |x: &mut Session| {
            x.status = SessionStatus::WaitingForAnswer;
            x.prewarm = None;
        };
        if !retire_and_rotate(app, &id, reverted, "question open").await {
            break;
        }
        // The fresh runtime is loaded before the question is re-emitted, so its replay sees an
        // empty log: the question line is a host line and must not consume the rank of the
        // runner's first event in the reconnect cursor (`agent_seq` loads from the file, and only
        // agentd lines may move it). The runtime remembers the question before the line lands, so
        // an HTTP answer never meets the gap, and the graces that count from when it was asked
        // keep counting.
        let fresh = app.runtime(&id).await;
        if let Some((question_id, questions, risk)) = &question {
            *fresh.open_question.lock().await = Some((question_id.clone(), questions.clone(), *risk));
            crate::validation::emit_chain(
                app,
                &id,
                json!({
                    "type": "question",
                    "question_id": question_id,
                    "questions": questions,
                    "risk": risk.as_str(),
                }),
            )
            .await;
        }
        fresh.activity.lock().await.question_since = question_since;
        app.session_log(
            &id,
            "info",
            "question opened; booting a fresh microVM so the answer lands in an already-running colony".into(),
        )
        .await;
        let resume = s.git_admin_dir.is_some();
        // Issue #98: the warm-up boots a kept worktree — the same authorized effect as any other
        // resume, so it carries its own fresh grant into `boot`.
        let grant = resume.then(|| crate::lifecycle::mint_resume_grant(&s, "harness"));
        tokio::spawn(boot(app.clone(), id, resume, grant));
    }
}

/// Holds a warming colony open for its answer (issue #701), on the boot task: an answer that lands
/// is delivered at once ([`deliver_prewarmed`]); a request gone — delivered elsewhere, stopped,
/// expired — ends the wait quietly; and the timeout hands the colony back to its suspension
/// ([`prewarm_expire`]). The boot returns through here instead of finishing a normal launch.
pub(crate) async fn prewarm_wait(app: &Shared, id: &str) {
    let timeout = orgs::prewarm_timeout(&app.modules.read().await.clone());
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(s) = app.session(id).await else { return };
        if !s.prewarming() {
            return;
        }
        if s.pending_answer.is_some() {
            deliver_prewarmed(app, id).await;
            return;
        }
        // The timeout counts from when the colony was actually up (`ready_at`), falling back to the
        // claim. A lost race with an answer landing in the same second is retried on the next tick.
        let warmed_at = s.prewarm.as_ref().and_then(|p| p.ready_at.or(p.started_at));
        if warmed_at.is_some_and(|at| Utc::now() - at >= timeout) && prewarm_expire(app, id).await {
            return;
        }
    }
}

/// Delivers an answer that landed while the colony was warming (issue #701): the runner gets it as
/// its first user message and the colony comes out of suspension for good. The lifecycle lock is
/// held across the send and the claim, and [`prewarm_expire`] holds the same lock across its own
/// claim and teardown — so a timeout and an answer arriving together fully order, and exactly one
/// of the two acts.
pub(crate) async fn deliver_prewarmed(app: &Shared, id: &str) {
    let lifecycle = app.session_lock(id).await;
    let _lifecycle = lifecycle.lock().await;
    let Some(s) = app.session(id).await else { return };
    let Some(prompt) = s
        .pending_answer
        .as_ref()
        .filter(|_| s.prewarming())
        .map(|pa| pa.prompt.clone())
    else {
        return;
    };
    // Sent before the claim: the runner is already mid-turn by the time the record says delivered.
    // A send that fails — the link task is gone — must not spend the answer: the warm-up is given
    // up and the answer stays held for the restore pass, which boots a runner to take it.
    let sent = match app.runtimes.lock().await.get(id).cloned() {
        Some(rt) => rt
            .commands
            .send(json!({"type": "user_message", "id": "initial", "text": prompt}))
            .is_ok(),
        None => false,
    };
    if !sent {
        if let Some((x, true)) = app
            .update_session(id, |x| {
                if !x.prewarming() {
                    return false;
                }
                x.status = SessionStatus::WaitingForAnswer;
                x.prewarm = None;
                true
            })
            .await
        {
            app.persist_and_broadcast(&x).await;
        }
        return;
    }
    if let Some((s, true)) = app
        .update_session(id, |x| {
            if !x.prewarming() || x.pending_answer.is_none() {
                return false;
            }
            x.pending_answer = None;
            x.suspended = None;
            x.prewarm = None;
            x.error = None;
            true
        })
        .await
    {
        app.persist_and_broadcast(&s).await;
        app.session_log(
            id,
            "info",
            "held answer delivered: the agent resumes its session with it".into(),
        )
        .await;
        crate::activity::record_restored(app, &s).await;
    }
}

/// Gives a warming colony up once its timeout passes with no answer (issue #701): the colony goes
/// back to exactly what the suspension left — question open and answerable, the next answer
/// restoring it like any suspended colony's — and then the microVM is torn down, freeing the slot.
/// The claim comes first, the stop handler's order: `hold_answer` reads the record without this
/// lock, so an answer landing during the (slow) teardown finds a properly suspended colony to hold
/// on, not one still warming. Returns whether the expiry landed.
pub(crate) async fn prewarm_expire(app: &Shared, id: &str) -> bool {
    let lifecycle = app.session_lock(id).await;
    let _lifecycle = lifecycle.lock().await;
    let Some((s, true)) = app
        .update_session(id, |x| {
            if !x.prewarming() || x.pending_answer.is_some() {
                return false;
            }
            x.status = SessionStatus::WaitingForAnswer;
            x.prewarm = None;
            true
        })
        .await
    else {
        return false;
    };
    let minutes = orgs::prewarm_timeout(&app.modules.read().await.clone()).num_minutes();
    app.session_log(
        id,
        "info",
        format!(
            "no answer in {minutes} min; suspending again — the microVM is removed, the worktree and the \
             agent's session transcript are kept, and the question stays answerable"
        ),
    )
    .await;
    teardown_vm(app, &s).await;
    true
}

/// The resume scheduler's verdict for one colony at `now` (unix seconds): quota-parked, with a
/// worktree to resume into, and due — its provider recovered, or the reset a card's "wait" scheduled
/// it for has come ([`crate::quota_cards::park_due`]). The provider is the one the flag names (a
/// card's wait records it), else the one the park's error names; with neither, any exhaustion
/// anywhere holds it. Pure over `exhausted`, so the schedule is tested with a fake clock.
pub(crate) fn quota_resume_due(
    s: &Session,
    provider_ids: &[String],
    exhausted: &dyn Fn(&str) -> bool,
    any_exhausted: bool,
    now: i64,
) -> bool {
    let Some(attention) = s
        .attention
        .as_ref()
        .filter(|a| a["reason"].as_str() == Some(provider_quota::QUOTA_EXHAUSTED_REASON))
    else {
        return false;
    };
    if s.cleaned_up || s.git_admin_dir.is_none() {
        return false;
    }
    let provider = crate::quota_cards::flagged_provider(s, provider_ids);
    let out = match provider.as_deref() {
        Some(pid) => exhausted(pid),
        None => any_exhausted,
    };
    crate::quota_cards::park_due(attention, out, now)
}

/// Quota-parked colonies whose provider is no longer exhausted rejoin the queue as `Queued` — the
/// worktree never left, so the normal admission loop resumes them like any operator resume. Both
/// park shapes qualify: the pre-#213 `Stopped` stand-in and a real `Parked` whose park discarded
/// the microVM. A park that kept the microVM cannot be requeued — a fresh boot would claim the
/// sandbox name the kept machine still runs under — so its recovery is routed through the resume
/// handler instead ([`crate::lifecycle::resume`]): warm when the idle agent link and a slot are
/// there (the usual case while the mothership stayed up), and otherwise through that handler's own
/// cold path, which hands the kept machine in before booting, or queues with it handed in — the
/// same thing an operator's Resume press does. Without this a kept-VM park would sit stranded on a
/// provider that has long since recovered. A named provider recovers when its record lapses (reset
/// passed) or is gone (provider deleted); an unnamed one recovers when nothing is exhausted
/// anywhere, the account record included — an account-parked colony stays parked while the account
/// record holds and resumes when it lapses.
pub(crate) async fn resume_quota_parked(app: &Shared) {
    let (ids, kept_ids): (Vec<String>, Vec<String>) = {
        let sessions = app.sessions.read().await;
        let ids: Vec<String> = app.providers().iter().map(|p| p.id.clone()).collect();
        let any_exhausted = !app.gateway.quota_exhausted().is_empty();
        let now = Utc::now().timestamp();
        let recovered = |s: &Session| quota_resume_due(s, &ids, &|pid| app.gateway.is_quota_exhausted(pid), any_exhausted, now);
        let mut cold: Vec<String> = Vec::new();
        let mut kept: Vec<String> = Vec::new();
        for s in sessions
            .iter()
            .filter(|s| matches!(s.status, SessionStatus::Stopped | SessionStatus::Parked) && recovered(s))
            // Issue #673: a merge covered this colony's work — it stays parked, park record
            // intact, until it is kept; the tick after that resumes it like any other.
            .filter(|s| !crate::supersede::blocks_start(s))
        {
            // A kept-VM park takes the resume route; everything else is a plain requeue.
            if s.parked.as_ref().is_some_and(|p| p.vm_kept) {
                kept.push(s.id.clone());
            } else {
                cold.push(s.id.clone());
            }
        }
        (cold, kept)
    };
    for id in ids {
        // Queued holds no slot, so the flip needs no admission; the loop below boots it. The status
        // is re-checked under the lock, so a concurrent operator resume wins instead of doubling.
        let flipped = app
            .update_session(&id, |x| {
                let due = x.status == SessionStatus::Stopped
                    || (x.status == SessionStatus::Parked && x.parked.as_ref().is_some_and(|p| !p.vm_kept));
                if due {
                    x.status = SessionStatus::Queued;
                    x.error = None;
                    x.attention = None;
                    // The park record had its say; a requeue is a resume, so it goes (issue #213).
                    x.parked = None;
                    x.updated_at = Utc::now();
                }
                due
            })
            .await
            .is_some_and(|(_, flipped)| flipped);
        if flipped {
            let s = app.session(&id).await;
            if let Some(s) = s {
                app.persist_and_broadcast(&s).await;
            }
            app.session_log(&id, "info", "the provider's quota recovered; queued to resume".into())
                .await;
        }
    }
    for id in kept_ids {
        // The handler re-checks everything under its own locks — status, live link, running
        // microVM, the discard setting, admission — so a colony an operator just resumed or
        // stopped is not doubled, and one it cannot warm-resume lands on the cold path. A
        // refusal (409, 404) means the park is no longer this tick's to recover.
        let _ = crate::lifecycle::resume(State(app.clone()), Path(id), None).await;
    }
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    let queue = app.clone();
    tokio::spawn(async move { run_queue(queue).await });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::QuestionRisk;
    use crate::sessions::tests::{admit_create, admit_create_in, admit_resume, colony, stopped_colony_with_worktree};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn write_providers(root: &std::path::Path, ids: &[&str]) {
        let body: Vec<Value> = ids
            .iter()
            .map(|id| json!({"id": id, "name": id, "base_url": "http://127.0.0.1:1", "auth": "none"}))
            .collect();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(root.join("config/providers.json"), serde_json::to_vec(&body).unwrap()).unwrap();
    }

    /// An idle colony held on an autopilot wait, with a worktree like a live one has.
    fn held_colony(id: &str, org: &str, since: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = colony(org, SessionStatus::Idle);
        s.id = id.into();
        s.git_admin_dir = Some("git".into());
        s.attention = Some(json!({"reason": "autopilot_held", "since": since, "nudges": 0}));
        s
    }

    /// A hold-timeout park (issue #876): parked with the hold reason and `risk`, a kept worktree, and
    /// `resumes` steps already spent. `risk` `None` is a park with no open question.
    fn hold_parked_colony(id: &str, risk: Option<QuestionRisk>, resumes: u32, at: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = colony("acme", SessionStatus::Parked);
        s.id = id.into();
        s.git_admin_dir = Some("git".into());
        s.attention = Some(json!({"reason": HOLD_TIMEOUT_REASON, "nudges": 0}));
        s.parked = Some(crate::sessions::Park {
            at,
            reason: HOLD_TIMEOUT_REASON.into(),
            resets_at: None,
            vm_kept: false,
            question_risk: risk,
        });
        s.hold_resumes = resumes;
        s
    }

    /// A colony parked by the automatic provider-error retry (issue #980): parked with the retry
    /// reason, a kept worktree, and `attempts` retries already recorded.
    fn provider_retry_parked_colony(id: &str, attempts: u32, at: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = colony("acme", SessionStatus::Parked);
        s.id = id.into();
        s.git_admin_dir = Some("git".into());
        s.attention = Some(json!({"reason": PROVIDER_RETRY_REASON, "nudges": 0}));
        s.parked = Some(crate::sessions::Park {
            at,
            reason: PROVIDER_RETRY_REASON.into(),
            resets_at: None,
            vm_kept: false,
            question_risk: None,
        });
        s.provider_retries = attempts;
        s
    }

    /// An autonomy judge with `ceiling` as its risk ceiling.
    fn judge_at(ceiling: QuestionRisk) -> crate::autonomy::Judge {
        crate::autonomy::Judge {
            model: "judge-model".into(),
            fallback_models: Vec::new(),
            after_minutes: 0,
            max_answers: Some(3),
            free_text: false,
            risk_ceiling: ceiling,
        }
    }

    /// The modules config that arms the judge at `ceiling`, the way a configured install reads.
    fn judging_modules(ceiling: QuestionRisk) -> crate::config::ModulesConfig {
        use crate::config::ModuleChoice;
        use serde_json::Map;
        crate::config::ModulesConfig {
            autonomy: Some(ModuleChoice {
                provider: "judge".into(),
                enabled: true,
                settings: Map::from_iter([
                    ("model".into(), json!("judge-model")),
                    ("risk_ceiling".into(), serde_json::to_value(ceiling).unwrap()),
                ]),
            }),
            ..crate::config::ModulesConfig::default()
        }
    }

    #[test]
    fn a_hold_expires_at_the_timeout_and_not_before() {
        let timeout = chrono::Duration::minutes(30);
        let now = Utc::now();
        let held_since = |ago: chrono::Duration| {
            let mut s = colony("acme", SessionStatus::Idle);
            s.attention = Some(json!({"reason": "autopilot_held", "since": now - ago, "nudges": 0}));
            s
        };
        assert!(
            !hold_expired(&held_since(timeout - chrono::Duration::seconds(1)), now, timeout),
            "a hold a second inside the timeout still keeps its slot"
        );
        assert!(
            hold_expired(&held_since(timeout), now, timeout),
            "at the timeout the slot is released"
        );
        assert!(
            hold_expired(&held_since(timeout + chrono::Duration::hours(2)), now, timeout),
            "past the timeout it stays expired"
        );
    }

    #[test]
    fn the_hold_timeout_stamp_leaves_a_colony_a_resume_claimed_in_between_alone() {
        // The stamp and the park are one claim now (issue #213), so the race this test covered is
        // gone; what remains is that the record a park writes carries the reason its resume paths
        // key on, and that a live colony never wears it.
        let live = held_colony("acme", "acme", Utc::now());
        assert_eq!(
            live.attention.as_ref().and_then(|a| a["reason"].as_str()),
            Some(AUTOPILOT_HELD_REASON),
            "a held colony wears its hold, not a park reason"
        );
    }

    #[test]
    fn only_an_autopilot_held_idle_colony_can_expire() {
        let timeout = chrono::Duration::minutes(30);
        let now = Utc::now();
        let old = now - chrono::Duration::hours(3);
        assert!(
            !hold_expired(&colony("acme", SessionStatus::Idle), now, timeout),
            "an idle colony nobody is waiting on never expires"
        );
        let mut other = colony("acme", SessionStatus::Idle);
        other.attention = Some(json!({"reason": "stalled", "since": old, "nudges": 3}));
        assert!(!hold_expired(&other, now, timeout), "another reason never expires");
        for status in [
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Stopped,
        ] {
            let mut s = colony("acme", status);
            s.attention = Some(json!({"reason": "autopilot_held", "since": old, "nudges": 0}));
            assert!(!hold_expired(&s, now, timeout), "{status:?} never expires");
        }
        // A hold with no readable start keeps its slot rather than parking on a guess.
        for attention in [
            json!({"reason": "autopilot_held", "nudges": 0}),
            json!({"reason": "autopilot_held", "since": "not a timestamp", "nudges": 0}),
        ] {
            let mut s = colony("acme", SessionStatus::Idle);
            s.attention = Some(attention);
            assert!(!hold_expired(&s, now, timeout), "an ambiguous hold keeps its slot");
        }
    }

    /// Issue #980: a colony parked by the provider-error retry resumes only once the backoff step for
    /// the attempt it is on has elapsed — 2, 5, 10 then 20 minutes from the park — and no other park
    /// or live colony is swept up by the retry resume.
    #[test]
    fn a_provider_retry_park_is_due_only_after_its_backoff_step() {
        let now = Utc::now();
        for (attempt, minutes) in [(1u32, 2i64), (2, 5), (3, 10), (4, 20)] {
            let just_parked = provider_retry_parked_colony("abc", attempt, now);
            assert!(
                !provider_retry_due(&just_parked, now),
                "attempt {attempt} is not due the moment it parks"
            );
            assert!(
                !provider_retry_due(&just_parked, now + chrono::Duration::minutes(minutes - 1)),
                "attempt {attempt} is not due a minute early"
            );
            assert!(
                provider_retry_due(&just_parked, now + chrono::Duration::minutes(minutes)),
                "attempt {attempt} is due after {minutes} min"
            );
        }
        let other = hold_parked_colony("abc", None, 0, now);
        assert!(
            !provider_retry_due(&other, now + chrono::Duration::hours(1)),
            "a hold park is not a retry park"
        );
        let mut idle = colony("acme", SessionStatus::Idle);
        idle.provider_retries = 1;
        assert!(
            !provider_retry_due(&idle, now + chrono::Duration::hours(1)),
            "a live colony is never due"
        );
    }

    #[test]
    fn parked_colonies_hold_no_slots() {
        let mut live: Vec<Session> = (0..14).map(|_| colony("acme", SessionStatus::Running)).collect();
        assert!(
            !has_room(&live, "acme", "acme/repo", 14, None, 32),
            "14 live colonies fill 14 slots"
        );
        // Parking flips live colonies to Parked, which holds nothing (issue #213).
        for s in &mut live {
            s.status = SessionStatus::Parked;
        }
        assert_eq!(live.iter().filter(|s| s.status.busy()).count(), 0, "no parked colony is busy");
        assert!(
            has_room(&live, "acme", "acme/repo", 14, None, 32),
            "14 parked colonies hold no slots"
        );
    }

    #[test]
    fn the_queue_waits_for_a_slot_and_queued_colonies_hold_none() {
        let running = vec![
            colony("acme", SessionStatus::Running),
            colony("acme", SessionStatus::Idle),
            colony("acme", SessionStatus::Publishing),
        ];
        assert!(
            !has_room(&running, "acme", "acme/repo", 3, None, 32),
            "publishing still holds its slot"
        );
        assert!(has_room(&running, "acme", "acme/repo", 4, None, 32));

        // Queued and finished colonies are not occupying anything.
        let waiting = vec![
            colony("acme", SessionStatus::Queued),
            colony("acme", SessionStatus::Queued),
            colony("acme", SessionStatus::PrOpened),
            colony("acme", SessionStatus::Stopped),
            colony("acme", SessionStatus::Failed),
        ];
        assert!(
            has_room(&waiting, "acme", "acme/repo", 1, None, 32),
            "a queue of five holds no slots"
        );

        // An org limit applies on top of the global one, and only to that org.
        let mixed = vec![
            colony("acme", SessionStatus::Running),
            colony("other", SessionStatus::Running),
        ];
        assert!(
            !has_room(&mixed, "acme", "acme/repo", 5, Some(1), 32),
            "acme is at its own limit"
        );
        assert!(
            has_room(&mixed, "third", "third/repo", 5, Some(1), 32),
            "another org still has room"
        );

        // A repository limit applies on top of both, only to that repository, and whatever the org's.
        let mut same_org = colony("acme", SessionStatus::Running);
        same_org.repo = "acme/api".into();
        let repos = vec![colony("acme", SessionStatus::Running), same_org];
        assert!(
            !has_room(&repos, "acme", "acme/repo", 5, None, 1),
            "acme/repo is at its own limit"
        );
        assert!(
            !has_room(&repos, "acme", "acme/api", 5, Some(10), 1),
            "so is acme/api, under a roomy org"
        );
        assert!(
            has_room(&repos, "acme", "acme/web", 5, None, 1),
            "another repository still has room"
        );
        assert!(has_room(&repos, "acme", "acme/repo", 5, None, 2));
        assert!(
            !has_room(&repos, "acme", "acme/web", 5, Some(2), 3),
            "the org limit still binds first"
        );
        assert!(
            !has_room(&repos, "acme", "acme/web", 2, None, 3),
            "and so does the global one"
        );
    }

    #[test]
    fn a_publish_from_a_stopped_colony_holds_no_slot_but_a_live_origin_one_does() {
        // `colony()` builds a live-origin claim, which keeps its slot; a stopped-origin claim boots
        // nothing (host-side push only) and must not block anyone.
        let mut stopped_origin = colony("acme", SessionStatus::Publishing);
        stopped_origin.publishing_holds_slot = false;
        let live_origin = colony("acme", SessionStatus::Publishing);
        assert!(
            has_room(&[stopped_origin.clone()], "acme", "acme/repo", 1, None, 32),
            "a stopped colony's publish holds no slot"
        );
        assert!(
            !has_room(&[live_origin], "acme", "acme/repo", 1, None, 32),
            "a live colony's publish keeps its slot"
        );
        // The org limit counts the same way.
        assert!(
            has_room(&[stopped_origin], "acme", "acme/repo", 5, Some(1), 32),
            "a stopped colony's publish counts against neither limit"
        );
    }

    #[test]
    fn a_queued_colony_that_was_cleaned_up_is_retired_and_never_started() {
        let mut s = stopped_colony_with_worktree("acme", "queued-then-cleaned".into());
        s.status = SessionStatus::Queued;
        s.cleaned_up = true;
        let claim = claim_queued(&mut s, true);
        assert!(
            matches!(claim, Some(Claim::Retire(..))),
            "a cleaned-up colony is retired, not started"
        );
        assert_eq!(
            s.status,
            SessionStatus::Failed,
            "out of the queue, so no later tick can pick it up"
        );
        assert!(
            !can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()),
            "there is no worktree to resume onto"
        );
        assert!(s.error.is_some(), "the operator is told why it will never start");

        // An ordinary queued colony is still claimed for starting.
        let mut waiting = colony("acme", SessionStatus::Queued);
        assert!(
            matches!(claim_queued(&mut waiting, true), Some(Claim::Start(_))),
            "a queued colony with everything intact starts"
        );
        assert_eq!(waiting.status, SessionStatus::Starting);
    }

    /// A parent another colony could be stacked on, with the id and branch the tests need.
    fn parent_colony(id: &str, status: SessionStatus, branch: &str) -> Session {
        let mut p = colony("acme", status);
        p.id = id.into();
        p.branch = branch.into();
        p
    }

    /// A `claim_wait` waiter for issue 7 (issue #321), queued behind `holder`, created `ago_secs`
    /// ago so the oldest-first tiebreak is deterministic.
    fn claim_waiter(id: &str, holder: &str, ago_secs: i64) -> Session {
        let mut s = colony("acme", SessionStatus::Queued);
        s.id = id.into();
        s.issue = Some(7);
        s.claim_wait = true;
        s.queued_behind = Some(holder.into());
        s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
        s
    }

    /// The holder a claim waiter waits for, live on the same issue.
    fn issue_holder(id: &str, status: SessionStatus) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.issue = Some(7);
        s
    }

    #[test]
    fn a_claim_waiter_keeps_waiting_while_someone_else_holds_its_issue() {
        let holder = issue_holder("holder", SessionStatus::Running);
        let waiter = claim_waiter("waiter", "holder", 10);
        let sessions = vec![holder.clone(), waiter.clone()];
        assert!(
            next_queued(&sessions, |_| true).is_none(),
            "the holder still holds the issue, so the waiter is looked past"
        );
        // The pure decision the promotion re-checks under the lock: the waiter is held, and the
        // pointer names who holds the issue now. A colony that is no waiter is never held.
        assert_eq!(waiter_still_held(&waiter, &sessions).as_deref(), Some("holder"));
        assert_eq!(waiter_still_held(&holder, &sessions), None);
    }

    #[test]
    fn waiters_promote_oldest_first_and_the_second_re_points_to_the_first() {
        // Issue #321: the holder is gone and only the waiters remain. The oldest waiter's turn is
        // next — the gate admits it and the promotion clears the wait — and the second waiter
        // holds behind it, its pointer following whoever holds the issue now.
        let mut first = claim_waiter("first", "holder", 100);
        let second = claim_waiter("second", "holder", 50);
        let sessions = vec![first.clone(), second.clone()];
        let (picked, refuse) = next_queued(&sessions, |_| true).expect("the oldest waiter's turn has come");
        assert_eq!(picked.id, "first");
        assert!(matches!(refuse, Gate::Admit));
        // The promotion, re-checked under the lock: nothing holds the issue against it any more.
        assert!(matches!(claim_queued(&mut first, true), Some(Claim::Start(_))));
        assert!(!first.claim_wait, "promoted: the wait is over");
        assert_eq!(first.queued_behind, None);
        // The first waiter now holds the issue, so the second one cannot be jumped past it — and
        // it shows it is queued behind the first.
        let sessions = vec![first, second];
        assert_eq!(
            waiter_still_held(&sessions[1], &sessions).as_deref(),
            Some("first"),
            "the second waiter re-points to the first"
        );
        assert!(matches!(gate(&sessions[1], &sessions), Gate::Hold));
    }

    /// A colony retrying a transient boot failure waits out its backoff (issue #881) — checked ahead
    /// of the resume fast-path, so a kept worktree does not admit it early — and is looked past this
    /// tick like any other wait, so the colonies behind it are not stalled.
    #[test]
    fn a_colony_waiting_out_its_boot_retry_holds_and_does_not_block_others() {
        let mut retrying = colony("acme", SessionStatus::Queued);
        retrying.id = "retrying".into();
        // A kept worktree: without the retry wait, the resume fast-path would admit it at once.
        retrying.git_admin_dir = Some("git".into());
        retrying.retry_at = Some(Utc::now() + chrono::Duration::minutes(5));
        let mut other = colony("acme", SessionStatus::Queued);
        other.id = "other".into();
        let sessions = vec![retrying.clone(), other.clone()];
        assert!(matches!(gate(&retrying, &sessions), Gate::Hold), "the backoff has not passed");
        // The colony behind it is admitted this tick: a retrying colony is held, not a blocker.
        let (picked, _) = next_queued(&sessions, |_| true).expect("the colony behind starts");
        assert_eq!(picked.id, "other");
        // Once the wait passes, the colony itself is admitted again.
        retrying.retry_at = Some(Utc::now() - chrono::Duration::seconds(1));
        assert!(matches!(gate(&retrying, &sessions), Gate::Admit));
    }

    /// A queued colony stacked on `parent_id`, in the queue ahead of anything created later.
    /// Explicitly stacked: it starts from the parent's branch as soon as it is pushed.
    fn queued_child(id: &str, parent_id: &str, created_at: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = colony("acme", SessionStatus::Queued);
        s.id = id.into();
        s.parent = Some(parent_id.into());
        s.stack = true;
        s.created_at = created_at;
        s
    }

    /// A queued colony behind `parent_id` in the default mode: it waits for the parent's merge.
    fn queued_default_child(id: &str, parent_id: &str, created_at: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = colony("acme", SessionStatus::Queued);
        s.id = id.into();
        s.parent = Some(parent_id.into());
        s.created_at = created_at;
        s
    }

    #[test]
    fn a_child_waits_while_its_parent_is_running() {
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Running, "colonizer/issue-1-parent"),
        ];
        assert!(
            next_queued(&sessions, |_| true).is_none(),
            "the parent has not pushed a branch yet, so the child keeps waiting"
        );
    }

    #[test]
    fn a_child_waiting_on_its_parent_does_not_block_an_unrelated_colony_behind_it() {
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Running, "colonizer/issue-1-parent"),
            {
                let mut unrelated = colony("acme", SessionStatus::Queued);
                unrelated.id = "unrelated".into();
                unrelated.created_at = Utc::now() + chrono::Duration::minutes(1);
                unrelated
            },
        ];
        let (picked, refuse) = next_queued(&sessions, |_| true).expect("something in the queue can move");
        assert_eq!(picked.id, "unrelated", "the child waiting on its parent is looked past");
        assert!(
            matches!(refuse, Gate::Admit),
            "the unrelated colony starts, it is not retired"
        );
    }

    #[test]
    fn a_child_whose_parent_has_pushed_its_branch_starts_like_any_other_colony() {
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::PrOpened, "colonizer/issue-1-parent"),
        ];
        let (picked, refuse) =
            next_queued(&sessions, |_| true).expect("the parent's branch is on the remote, so the child starts");
        assert_eq!(picked.id, "child");
        assert!(matches!(refuse, Gate::Admit));
        // But it still waits for a slot like everyone else.
        assert!(next_queued(&sessions, |_| false).is_none(), "no room, nothing moves");
    }

    #[test]
    fn by_default_a_child_behind_an_open_pull_request_keeps_waiting() {
        // The same parent, but the child did not ask to stack: an open pull request is still work
        // unmerged, so the queue holds the child for the merge.
        let sessions = vec![
            queued_default_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::PrOpened, "colonizer/issue-1-parent"),
        ];
        assert!(
            next_queued(&sessions, |_| true).is_none(),
            "the parent's pull request has not merged yet, so the child keeps waiting"
        );
    }

    #[test]
    fn by_default_a_child_behind_a_merged_parent_starts_like_any_other_colony() {
        let sessions = vec![
            queued_default_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Merged, "colonizer/issue-1-parent"),
        ];
        let (picked, refuse) = next_queued(&sessions, |_| true).expect("the parent's work is merged, so the child starts");
        assert_eq!(picked.id, "child");
        assert!(matches!(refuse, Gate::Admit));
    }

    #[test]
    fn a_child_whose_parent_failed_moves_onto_the_parents_own_parent() {
        // Issue #982: the child's parent failed, but that parent's own parent — the grandparent —
        // may still lend a branch. The child re-parents onto it rather than being retired, one rung
        // up the stack per tick.
        let mut grandparent = parent_colony("grandparent", SessionStatus::PrOpened, "colonizer/issue-1-grandparent");
        grandparent.id = "grandparent".into();
        let mut failed = parent_colony("parent", SessionStatus::Failed, "");
        failed.parent = Some("grandparent".into());
        let sessions = vec![queued_child("child", "parent", Utc::now()), failed, grandparent];
        let Some((picked, gate)) = next_queued(&sessions, |_| true) else {
            panic!("the child is acted on, not left sitting at the head of the queue");
        };
        assert_eq!(picked.id, "child", "the child is what this tick acts on");
        let Gate::Reparent(new_parent) = gate else {
            panic!("the failed parent's own parent is what the child moves onto");
        };
        assert_eq!(new_parent, "grandparent", "one rung up the stack");

        // The claim moves the link and nothing else: it stays Queued, nothing failed.
        let mut queued = queued_child("child", "parent", Utc::now());
        let claim = claim_reparent(&mut queued, &new_parent);
        assert!(matches!(claim, Some(Claim::Reparent(_))), "re-parented, not retired");
        assert_eq!(queued.parent.as_deref(), Some("grandparent"));
        assert_eq!(queued.status, SessionStatus::Queued, "still in the queue, nothing failed");
        assert!(
            queued.error.is_none() && !queued.unseen_failure,
            "no failure was recorded against a colony that only moved"
        );
        // A colony claimed in the meantime is left alone.
        let mut starting = colony("acme", SessionStatus::Starting);
        assert!(claim_reparent(&mut starting, "grandparent").is_none());
        assert_eq!(starting.status, SessionStatus::Starting);
    }

    #[test]
    fn a_child_whose_failed_parent_has_no_parent_of_its_own_keeps_waiting() {
        // Nothing to move onto: the failed parent sits at the bottom of the stack, so there is no
        // grandparent and no branch to borrow. The child waits rather than failing (issue #982).
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Failed, ""),
        ];
        assert!(
            next_queued(&sessions, |_| true).is_none(),
            "the child waits; there is nobody further up the stack to build on"
        );
        // And the gate says so directly: held, never retired.
        assert!(matches!(gate(&sessions[0], &sessions), Gate::Hold));
    }

    #[test]
    fn a_parent_that_made_no_changes_still_retires_the_child_by_name_and_reason() {
        // Issue #982 stopped a failed parent from failing the child, but a parent whose work made no
        // changes still has no branch anywhere to build on: the child is retired as before.
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::NoChanges, "colonizer/issue-1-parent"),
        ];
        let Some((picked, gate)) = next_queued(&sessions, |_| true) else {
            panic!("the child is retired, not left sitting at the head of the queue");
        };
        assert_eq!(picked.id, "child", "the child is what this tick acts on");
        let Gate::Retire(reason) = gate else {
            panic!("a parent that made no changes can never lend a branch");
        };
        assert!(reason.contains("parent"), "the parent is named: {reason}");
        assert!(reason.contains("made no changes"), "and the reason is named: {reason}");

        // The claim takes the child out of the queue with that reason as its error.
        let mut queued = queued_child("child", "parent", Utc::now());
        let claim = claim_refused(&mut queued, &reason);
        assert!(matches!(claim, Some(Claim::Retire(..))), "retired, not started");
        assert_eq!(queued.status, SessionStatus::Failed, "out of the queue for good");
        assert_eq!(queued.error.as_deref(), Some(reason.as_str()));
        assert!(
            queued.unseen_failure,
            "the badge counts the retirement until someone looks (#744)"
        );
        // A colony claimed in the meantime is left alone.
        let mut starting = colony("acme", SessionStatus::Starting);
        assert!(claim_refused(&mut starting, &reason).is_none());
        assert_eq!(starting.status, SessionStatus::Starting);
    }

    /// A queued colony that is resuming: a kept worktree means `boot_inner` will reuse the base it
    /// recorded at its first boot and never ask its parent for a branch.
    fn queued_resume(id: &str, parent_id: &str, created_at: chrono::DateTime<chrono::Utc>) -> Session {
        let mut s = stopped_colony_with_worktree("acme", id.into());
        s.status = SessionStatus::Queued;
        s.parent = Some(parent_id.into());
        s.base = Some("colonizer/issue-1-parent".into());
        s.created_at = created_at;
        s
    }

    #[test]
    fn a_queued_resume_starts_even_when_its_parent_cannot_lend_a_branch() {
        // A child booted from its parent's branch, was stopped, and was queued for resume while the
        // slots were full; the parent's publish then failed. Retiring the child for a branch it does
        // not need would silently cancel the operator's resume — doubly wrong, since the parent did
        // push and the child is not asking for anything.
        let sessions = vec![
            queued_resume("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Failed, "colonizer/issue-1-parent"),
        ];
        let (picked, refuse) = next_queued(&sessions, |_| true).expect("a resume-capable colony moves whatever its parent did");
        assert_eq!(picked.id, "child");
        assert!(
            matches!(refuse, Gate::Admit),
            "the parent's failure is not this colony's: it starts, it is not retired"
        );
    }

    #[test]
    fn a_queued_resume_is_not_held_while_its_parent_is_still_running() {
        // Held, a resumed colony would sit in the queue forever behind a parent whose branch it
        // never looks at.
        let sessions = vec![
            queued_resume("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Running, "colonizer/issue-1-parent"),
        ];
        let (picked, refuse) = next_queued(&sessions, |_| true).expect("the resume does not wait on its parent");
        assert_eq!(picked.id, "child");
        assert!(matches!(refuse, Gate::Admit));
    }

    #[tokio::test]
    async fn quota_pause_holds_queued_colonies_while_every_provider_is_out() {
        let root = std::env::temp_dir().join(format!("colonizer-quota-pause-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        let mut waiting = colony("acme", SessionStatus::Queued);
        waiting.id = "waiting".into();
        *app.sessions.write().await = vec![waiting];
        app.gateway
            .mark_quota_exhausted("bailian", Some("09-23 07:54 UTC".into()), Some(Utc::now().timestamp() + 3600));
        start_queued(&app).await;
        let sessions = app.sessions.read().await;
        assert_eq!(
            sessions.iter().find(|s| s.id == "waiting").unwrap().status,
            SessionStatus::Queued
        );
        drop(sessions);
        // The pause lifts with the record, and the queue admits again.
        app.gateway.forget_quota("bailian");
        assert!(
            !crate::providers::quota_status(&app).await.paused,
            "no exhausted provider, no pause"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_provider_pause_names_the_provider_by_its_display_name_never_claude() {
        let root = std::env::temp_dir().join(format!("colonizer-quota-name-{}", crate::util::short_id()));
        std::fs::create_dir_all(root.join("config")).unwrap();
        let body = json!([{"id": "byteplus", "name": "BytePlus", "base_url": "http://127.0.0.1:1", "auth": "none"}]);
        std::fs::write(root.join("config/providers.json"), serde_json::to_vec(&body).unwrap()).unwrap();
        let app = crate::tests::test_app(&root);
        app.gateway
            .mark_quota_exhausted("byteplus", Some("10-05 19:51:58".into()), Some(Utc::now().timestamp() + 3600));
        let status = crate::providers::quota_status(&app).await;
        assert!(status.paused);
        assert_eq!(status.kind.as_deref(), Some("provider"));
        assert_eq!(status.providers, vec!["byteplus".to_string()]);
        let reason = status.reason.clone().unwrap_or_default();
        assert!(reason.contains("BytePlus plan exhausted"), "{reason}");
        assert!(
            !reason.contains("Claude"),
            "a non-Anthropic plan is never called Claude: {reason}"
        );
        assert_eq!(status.details.len(), 1);
        assert_eq!(status.details[0].id, "byteplus");
        assert_eq!(status.details[0].name, "BytePlus");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_draining_mothership_admits_nothing_until_the_drain_is_cleared() {
        let root = std::env::temp_dir().join(format!("colonizer-drain-hold-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        // The reclaim floor would otherwise pause admission on a host with less than the default 5G
        // free, keeping the colony queued for a reason this test is not about. Turn it off through
        // the module setting, not the process environment, so the test stays hermetic (reclaim.rs).
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("min_free_disk".into(), json!("0"));
        let mut waiting = colony("acme", SessionStatus::Queued);
        waiting.id = "waiting".into();
        *app.sessions.write().await = vec![waiting];
        // While the mothership is draining for an update or a restart, the tick boots nothing: the
        // colony keeps its place in the queue (issue #880).
        app.drain.enter();
        start_queued(&app).await;
        assert_eq!(
            app.session("waiting").await.unwrap().status,
            SessionStatus::Queued,
            "a draining mothership admits nothing"
        );
        // Clearing the drain lets the next tick take it, as before.
        app.drain.clear();
        start_queued(&app).await;
        assert_ne!(
            app.session("waiting").await.unwrap().status,
            SessionStatus::Queued,
            "the queue moves again once the drain is cleared"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn quota_parked(id: &str, error: &str) -> Session {
        let mut s = stopped_colony_with_worktree("acme", id.into());
        s.status = SessionStatus::Stopped;
        s.error = Some(error.into());
        s.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "since": Utc::now(), "nudges": 0}));
        s
    }

    /// The same colony in the post-#213 shape: `Parked` with a park record. `vm_kept` says whether
    /// the park left the microVM running — the difference between a cold auto-resume and one that
    /// must wait for the operator.
    fn quota_parked_record(id: &str, error: &str, vm_kept: bool) -> Session {
        let mut s = quota_parked(id, error);
        s.status = SessionStatus::Parked;
        s.parked = Some(crate::sessions::Park {
            at: Utc::now(),
            reason: provider_quota::QUOTA_EXHAUSTED_REASON.into(),
            resets_at: Some("09-23 07:54 UTC".into()),
            vm_kept,
            question_risk: None,
        });
        s
    }

    #[tokio::test]
    async fn quota_resume_requeues_only_colonies_whose_provider_recovered() {
        let root = std::env::temp_dir().join(format!("colonizer-quota-resume-{}", crate::util::short_id()));
        write_providers(&root, &["bailian", "zai"]);
        let app = crate::tests::test_app(&root);
        *app.sessions.write().await = vec![
            quota_parked("parked-hot", "provider quota exhausted (bailian, resets 09-23 07:54 UTC)"),
            quota_parked("parked-cool", "provider quota exhausted (zai)"),
        ];
        // Bailian's reset is ahead (still out); zai's passed (recovered).
        app.gateway
            .mark_quota_exhausted("bailian", Some("09-23 07:54 UTC".into()), Some(Utc::now().timestamp() + 3600));
        app.gateway
            .mark_quota_exhausted("zai", None, Some(Utc::now().timestamp() - 10));
        resume_quota_parked(&app).await;
        let sessions = app.sessions.read().await;
        let hot = sessions.iter().find(|s| s.id == "parked-hot").unwrap();
        assert_eq!(hot.status, SessionStatus::Stopped, "still exhausted, still parked");
        assert!(hot.attention.is_some(), "the attention stays until recovery");
        let cool = sessions.iter().find(|s| s.id == "parked-cool").unwrap();
        assert_eq!(cool.status, SessionStatus::Queued, "recovered, back in the queue");
        assert!(
            cool.attention.is_none() && cool.error.is_none(),
            "a requeue reads like a resume"
        );
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A recovered provider requeues a genuinely `Parked` colony whose park discarded the microVM,
    /// and routes a kept-VM park through the resume handler rather than stranding it: with no live
    /// agent link in this test the warm path is off the table, and with every parallel slot taken
    /// the handler's cold path queues the colony — handing the kept microVM in first, so the
    /// queue's later boot cannot collide with it.
    #[tokio::test]
    async fn quota_resume_requeues_a_cold_park_and_routes_a_kept_vm_park_through_resume() {
        let root = std::env::temp_dir().join(format!("colonizer-quota-parked-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        // Three live colonies fill the sandbox module's default parallel limit, so the kept-VM
        // park's resume is queued rather than admitted (and no boot is spawned under the test).
        let fillers: Vec<Session> = (0..3)
            .map(|i| {
                let mut s = colony("acme", SessionStatus::Running);
                s.id = format!("filler-{i}");
                s
            })
            .collect();
        let mut sessions = vec![
            quota_parked_record("parked-cold", "provider quota exhausted (bailian)", false),
            quota_parked_record("parked-warm", "provider quota exhausted (bailian)", true),
        ];
        sessions.extend(fillers);
        *app.sessions.write().await = sessions;
        for id in ["parked-cold", "parked-warm"] {
            tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        }
        // The record lapsed: both colonies' provider has recovered.
        app.gateway
            .mark_quota_exhausted("bailian", None, Some(Utc::now().timestamp() - 10));
        resume_quota_parked(&app).await;
        let sessions = app.sessions.read().await;
        let cold = sessions.iter().find(|s| s.id == "parked-cold").unwrap();
        assert_eq!(cold.status, SessionStatus::Queued, "a discarded-VM park rejoins the queue");
        assert!(cold.parked.is_none(), "the park record had its say and goes");
        let warm = sessions.iter().find(|s| s.id == "parked-warm").unwrap();
        assert_eq!(
            warm.status,
            SessionStatus::Queued,
            "a kept-VM park is recovered too, not stranded"
        );
        assert!(
            warm.parked.is_none() && warm.attention.is_none(),
            "its resume cleared the pause"
        );
        drop(sessions);
        let cold_log = std::fs::read_to_string(app.session_dir("parked-cold").join("harness.jsonl")).unwrap_or_default();
        assert!(
            cold_log.contains("the provider's quota recovered; queued to resume"),
            "{cold_log}"
        );
        let warm_log = std::fs::read_to_string(app.session_dir("parked-warm").join("harness.jsonl")).unwrap_or_default();
        assert!(
            warm_log.contains("cold resume queued: removing the microVM the park kept"),
            "the kept-VM park went through resume's cold path, which hands the machine in: {warm_log}"
        );
        assert!(
            !warm_log.contains("queued to resume"),
            "not the plain requeue route: {warm_log}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A restart must not resume a parked colony into a plan that is still out: the record the last
    /// run wrote is what the fresh gateway reads.
    #[tokio::test]
    async fn quota_resume_after_a_restart_keeps_a_colony_parked_while_the_saved_record_holds() {
        let root = std::env::temp_dir().join(format!("colonizer-quota-restart-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        crate::gateway::Gateway::new(&root.join("data"))
            .unwrap()
            .mark_quota_exhausted("bailian", Some("09-23 07:54 UTC".into()), Some(Utc::now().timestamp() + 3600));
        let app = crate::tests::test_app(&root);
        *app.sessions.write().await = vec![quota_parked(
            "parked",
            "provider quota exhausted (bailian, resets 09-23 07:54 UTC)",
        )];
        resume_quota_parked(&app).await;
        let sessions = app.sessions.read().await;
        let parked = sessions.iter().find(|s| s.id == "parked").unwrap();
        assert_eq!(parked.status, SessionStatus::Stopped, "the saved record still holds");
        assert!(parked.attention.is_some(), "the attention stays until recovery");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    /// An account pause rides out a mothership restart: the account record persists in
    /// provider-quota.json, the migration keeps the parked colony's resume ticket, the reloaded
    /// queue stays paused, and auto-resume still fires once the record lapses.
    #[tokio::test]
    async fn quota_resume_after_a_restart_keeps_an_account_parked_colony_until_the_account_lapses() {
        let root = std::env::temp_dir().join(format!("colonizer-account-restart-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        crate::gateway::Gateway::new(&root.join("data"))
            .unwrap()
            .mark_account_quota_exhausted(Some("7am (UTC)".into()), Some(Utc::now().timestamp() + 3600));
        let app = crate::tests::test_app(&root);
        *app.sessions.write().await = vec![quota_parked("parked", "provider quota exhausted (resets 7am (UTC))")];
        // The restart migration keeps the resume ticket, and the reload keeps the pause.
        let mut snapshot = app.sessions.read().await.clone();
        assert_eq!(crate::sessions::clear_stale_attention(&mut snapshot), 0);
        *app.sessions.write().await = snapshot;
        let status = crate::providers::quota_status(&app).await;
        assert!(status.paused, "the reloaded account record still pauses");
        assert!(status.providers.is_empty(), "no real provider is named exhausted");
        assert!(
            !app.gateway.is_quota_exhausted("bailian"),
            "the healthy provider stayed healthy across the restart"
        );
        resume_quota_parked(&app).await;
        let sessions = app.sessions.read().await;
        let parked = sessions.iter().find(|s| s.id == "parked").unwrap();
        assert_eq!(parked.status, SessionStatus::Stopped, "the saved account record still holds");
        assert!(parked.attention.is_some(), "the attention stays until recovery");
        drop(sessions);
        // The reset passes while the mothership is down: a fresh gateway drops the lapsed record
        // and the parked colony rejoins the queue on its own.
        crate::gateway::Gateway::new(&root.join("data"))
            .unwrap()
            .mark_account_quota_exhausted(Some("7am (UTC)".into()), Some(Utc::now().timestamp() - 10));
        let app2 = crate::tests::test_app(&root);
        *app2.sessions.write().await = app.sessions.read().await.clone();
        assert!(
            !crate::providers::quota_status(&app2).await.paused,
            "the lapsed account record reads as recovered"
        );
        resume_quota_parked(&app2).await;
        let sessions = app2.sessions.read().await;
        assert_eq!(
            sessions.iter().find(|s| s.id == "parked").unwrap().status,
            SessionStatus::Queued,
            "auto-resume still works after the restart"
        );
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn parking_expired_holds_releases_slots_for_another_org() {
        let root = std::env::temp_dir().join(format!("colonizer-hold-timeout-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let timeout = chrono::Duration::minutes(30);
        // One org fills all 3 slots with held colonies while another org waits in the queue.
        let mut sessions = vec![
            held_colony("a-1", "org-a", Utc::now()),
            held_colony("a-2", "org-a", Utc::now()),
            held_colony("a-3", "org-a", Utc::now()),
        ];
        let mut waiting = colony("org-b", SessionStatus::Queued);
        waiting.id = "b-1".into();
        sessions.push(waiting);
        *app.sessions.write().await = sessions;
        for id in ["a-1", "a-2", "a-3"] {
            tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        }
        // Fresh holds keep their slots: nothing parks and org-b still has no room.
        park_expired_holds(&app, timeout).await;
        {
            let sessions = app.sessions.read().await;
            assert!(
                sessions
                    .iter()
                    .filter(|s| s.org == "org-a")
                    .all(|s| s.status == SessionStatus::Idle),
                "holds within the timeout still count"
            );
            assert!(
                !has_room(&sessions, "org-b", "org-b/repo", 3, None, 32),
                "org-a's fresh holds fill every slot"
            );
        }
        // Three hours later the same holds are past the timeout.
        let old = Utc::now() - chrono::Duration::hours(3);
        for s in app.sessions.write().await.iter_mut().filter(|s| s.org == "org-a") {
            s.attention = Some(json!({"reason": "autopilot_held", "since": old, "nudges": 0}));
        }
        park_expired_holds(&app, timeout).await;
        let sessions = app.sessions.read().await;
        for s in sessions.iter().filter(|s| s.org == "org-a") {
            assert_eq!(s.status, SessionStatus::Parked, "an expired hold parks, for real");
            let park = s.parked.as_ref().expect("the park record names why and what was kept");
            assert_eq!(park.reason, HOLD_TIMEOUT_REASON);
            assert!(park.resets_at.is_none(), "a hold timeout has no upstream reset to wait for");
            assert_eq!(
                s.attention.as_ref().and_then(|a| a["reason"].as_str()),
                Some(HOLD_TIMEOUT_REASON),
                "the banner's reason is stamped in the same claim as the park"
            );
            assert_eq!(s.attention.as_ref().and_then(|a| a["nudges"].as_u64()), Some(0));
            assert!(s.attention.as_ref().and_then(|a| a["since"].as_str()).is_some());
            assert!(
                !s.cleaned_up && s.git_admin_dir.is_some(),
                "the worktree is kept, never cleaned up"
            );
            assert!(
                can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()),
                "a parked hold is resumable"
            );
            assert!(!s.holds_slot(), "a parked hold releases its slot");
        }
        assert!(
            has_room(&sessions, "org-b", "org-b/repo", 3, None, 32),
            "org-a's expired holds no longer block org-b"
        );
        drop(sessions);
        // Quota recovery must not requeue these: that path only matches its own reason.
        resume_quota_parked(&app).await;
        let sessions = app.sessions.read().await;
        assert!(
            sessions
                .iter()
                .filter(|s| s.org == "org-a")
                .all(|s| s.status == SessionStatus::Parked && s.parked.is_some()),
            "hold-timeout parks stay parked until resumed"
        );
        drop(sessions);
        let log = std::fs::read_to_string(app.session_dir("a-1").join("harness.jsonl")).unwrap();
        assert!(
            log.contains("autopilot hold exceeded 30 min; parked to release its slot"),
            "the colony's log says why it parked: {log}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The backoff policy, as `hold_park_action` decides it (issue #876): each step is its own delay
    /// from the park, indexed by the resumes already spent; an above-ceiling question never resumes;
    /// nothing happens without a judge or on a park the hold timeout did not make.
    #[test]
    fn the_backoff_resumes_on_its_schedule_and_never_above_the_ceiling() {
        use crate::protocol::QuestionRisk::*;
        let judge = judge_at(WorkspaceWrite);
        let at = Utc::now();
        let parked = hold_parked_colony("p", Some(WorkspaceWrite), 0, at);
        for (spent, delay) in HOLD_RESUME_SCHEDULE.iter().enumerate() {
            let mut s = parked.clone();
            s.hold_resumes = spent as u32;
            assert_eq!(
                hold_park_action(&s, at + *delay - chrono::Duration::minutes(1), Some(&judge)),
                HoldParkAction::Wait
            );
            assert_eq!(hold_park_action(&s, at + *delay, Some(&judge)), HoldParkAction::Resume);
        }
        // Every step spent: give up rather than resume a fourth time.
        let mut spent = parked.clone();
        spent.hold_resumes = HOLD_RESUME_SCHEDULE.len() as u32;
        assert_eq!(
            hold_park_action(&spent, at + chrono::Duration::days(2), Some(&judge)),
            HoldParkAction::GiveUp
        );
        // No judge: nothing is auto-resumed, whatever the timing.
        assert_eq!(
            hold_park_action(&parked, at + chrono::Duration::days(2), None),
            HoldParkAction::Wait
        );
        // Above the ceiling never resumes; a park with no open question counts as within it.
        assert_eq!(
            hold_park_action(
                &hold_parked_colony("c", Some(CredentialAdjacent), 0, at),
                at + chrono::Duration::days(2),
                Some(&judge)
            ),
            HoldParkAction::NotifyOnly
        );
        assert_eq!(
            hold_park_action(
                &hold_parked_colony("n", None, 0, at),
                at + HOLD_RESUME_SCHEDULE[0],
                Some(&judge)
            ),
            HoldParkAction::Resume
        );
        // A quota park is left to its own recovery, not the hold backoff.
        let mut quota = parked.clone();
        quota.parked.as_mut().unwrap().reason = "provider_quota_exhausted".into();
        assert_eq!(
            hold_park_action(&quota, at + chrono::Duration::days(2), Some(&judge)),
            HoldParkAction::Wait
        );
    }

    /// A within-ceiling hold-parked question resumes itself on the first due step (issue #876): the
    /// park and its pause go, the step is spent, and [`HOLD_RESUME_NOTE`] rides the resume. Every
    /// slot is taken so the resume queues rather than booting under the test.
    #[tokio::test]
    async fn a_hold_parked_question_resumes_on_the_first_step_with_the_timeout_note() {
        let root = std::env::temp_dir().join(format!("colonizer-hold-resume-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        *app.modules.write().await = judging_modules(QuestionRisk::WorkspaceWrite);
        let at = Utc::now() - HOLD_RESUME_SCHEDULE[0] - chrono::Duration::minutes(1);
        let mut sessions = vec![hold_parked_colony("p", Some(QuestionRisk::WorkspaceWrite), 0, at)];
        for i in 0..3 {
            let mut f = colony("acme", SessionStatus::Running);
            f.id = format!("filler-{i}");
            sessions.push(f);
        }
        *app.sessions.write().await = sessions;
        tokio::fs::create_dir_all(app.session_dir("p")).await.unwrap();
        resume_hold_parked(&app).await;
        let p = app.session("p").await.unwrap();
        assert_eq!(p.status, SessionStatus::Queued, "the resume queues behind the fillers");
        assert_eq!(p.resume_note.as_deref(), Some(HOLD_RESUME_NOTE), "the note rides the resume");
        assert_eq!(p.hold_resumes, 1, "the first step is spent");
        assert!(p.parked.is_none() && p.attention.is_none(), "the park and its pause go");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A question above the ceiling is never auto-resumed (issue #876): the colony stays parked, the
    /// queue raises the reason `notify` reads, and a second tick is quiet — the edge fires once.
    #[tokio::test]
    async fn a_question_above_the_ceiling_is_never_auto_resumed_and_flagged_once() {
        let root = std::env::temp_dir().join(format!("colonizer-hold-notify-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        *app.modules.write().await = judging_modules(QuestionRisk::WorkspaceWrite);
        let at = Utc::now() - chrono::Duration::days(2);
        *app.sessions.write().await = vec![hold_parked_colony("c", Some(QuestionRisk::CredentialAdjacent), 0, at)];
        let reason = |s: &Session| s.attention.as_ref().and_then(|a| a["reason"].as_str()).map(str::to_string);
        resume_hold_parked(&app).await;
        let c = app.session("c").await.unwrap();
        assert_eq!(
            c.status,
            SessionStatus::Parked,
            "an above-ceiling question is never auto-resumed"
        );
        assert!(
            c.parked.is_some() && c.resume_note.is_none() && c.hold_resumes == 0,
            "nothing was spent on it"
        );
        assert_eq!(reason(&c).as_deref(), Some(HOLD_UNANSWERED_REASON), "the reason notify reads");
        // A second tick neither resumes nor re-raises: the reason is stamped, and notify fires once.
        resume_hold_parked(&app).await;
        let c = app.session("c").await.unwrap();
        assert_eq!(c.status, SessionStatus::Parked);
        assert_eq!(reason(&c).as_deref(), Some(HOLD_UNANSWERED_REASON));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A question unanswered through every backoff step fails the colony with the machine reason
    /// (issue #876), clearing the park but keeping the worktree for a person to resume.
    #[tokio::test]
    async fn a_question_unanswered_through_every_step_fails_and_keeps_the_worktree() {
        let root = std::env::temp_dir().join(format!("colonizer-hold-giveup-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        *app.modules.write().await = judging_modules(QuestionRisk::WorkspaceWrite);
        let at = Utc::now() - chrono::Duration::days(2);
        *app.sessions.write().await = vec![hold_parked_colony(
            "p",
            Some(QuestionRisk::WorkspaceWrite),
            HOLD_RESUME_SCHEDULE.len() as u32,
            at,
        )];
        tokio::fs::create_dir_all(app.session_dir("p")).await.unwrap();
        resume_hold_parked(&app).await;
        let p = app.session("p").await.unwrap();
        assert_eq!(p.status, SessionStatus::Failed, "every step spent, so the colony gives up");
        assert_eq!(p.error.as_deref(), Some(ABANDONED_QUESTION_REASON));
        assert!(p.parked.is_none(), "a failed colony keeps no park record");
        assert!(
            p.git_admin_dir.is_some() && !p.cleaned_up,
            "the worktree is kept for a person to resume"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #980: the sweep itself (not just the `provider_retry_due` predicate) resumes a due
    /// provider-retry park — queuing behind the fillers — and leaves a not-yet-due one parked with
    /// its attempt count untouched.
    #[tokio::test]
    async fn a_due_provider_retry_park_resumes_and_a_pending_one_is_left() {
        let root = std::env::temp_dir().join(format!("colonizer-provider-retry-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let due_at = Utc::now() - chrono::Duration::minutes(3);
        let mut sessions = vec![
            provider_retry_parked_colony("due", 1, due_at),
            // Attempt 1's 2-minute step has not passed for this one.
            provider_retry_parked_colony("pending", 1, Utc::now()),
        ];
        for i in 0..3 {
            let mut f = colony("acme", SessionStatus::Running);
            f.id = format!("filler-{i}");
            sessions.push(f);
        }
        *app.sessions.write().await = sessions;
        for id in ["due", "pending"] {
            tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        }
        resume_provider_retry_parked(&app).await;
        let due = app.session("due").await.unwrap();
        assert_eq!(
            due.status,
            SessionStatus::Queued,
            "the due retry resumes (queues behind the fillers)"
        );
        assert!(due.parked.is_none(), "the park goes with the resume");
        assert_eq!(due.provider_retries, 1, "the sweep never changes the attempt count");
        let pending = app.session("pending").await.unwrap();
        assert_eq!(pending.status, SessionStatus::Parked, "a not-yet-due retry stays parked");
        assert!(pending.parked.is_some(), "its park record is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_queue_holds_a_waiting_child_and_retires_one_whose_parent_is_gone() {
        let root = std::env::temp_dir().join(format!("colonizer-queue-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut waiting = queued_child("waits", "live-parent", Utc::now());
        waiting.created_at = Utc::now() - chrono::Duration::minutes(2);
        // This child's parent had its record deleted outright, so there is nothing left to re-parent
        // onto and it is retired. A *failed* parent is treated quite differently (issue #982): that
        // child re-parents onto the grandparent, or waits when there is none — never this.
        let mut doomed = queued_child("doomed", "dead-parent", Utc::now());
        doomed.created_at = Utc::now() - chrono::Duration::minutes(1);
        let sessions = vec![
            parent_colony("live-parent", SessionStatus::Running, "colonizer/issue-1-live"),
            waiting,
            doomed,
        ];
        *app.sessions.write().await = sessions;
        // No colony here can start, so nothing is booted and nothing reaches for GitHub or a microVM.
        start_queued(&app).await;
        let sessions = app.sessions.read().await;
        let held = sessions.iter().find(|s| s.id == "waits").unwrap();
        assert_eq!(held.status, SessionStatus::Queued, "its parent is still running");
        let retired = sessions.iter().find(|s| s.id == "doomed").unwrap();
        assert_eq!(retired.status, SessionStatus::Failed, "its parent's record is gone");
        let error = retired.error.as_deref().unwrap_or_default();
        assert!(error.contains("dead-parent") && error.contains("no colony"), "{error}");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_creates_cannot_both_take_the_last_free_slot() {
        let max_parallel = 4;
        let attempts = 32;
        let sessions = Arc::new(RwLock::new(Vec::new()));
        // Everything is released onto the workers at once, so all `attempts` collide on the slots the way
        // concurrent HTTP handlers would. Snapshot-then-push (the old create) let more than the limit past
        // this barrier; one lock-held check-and-claim may not.
        let barrier = Arc::new(tokio::sync::Barrier::new(attempts));
        let mut tasks = Vec::new();
        for i in 0..attempts {
            let (sessions, barrier) = (sessions.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                admit_create(&sessions, "acme", max_parallel, None, format!("create-{i}")).await;
            }));
        }
        for task in tasks {
            task.await.expect("create task joined");
        }
        let done = sessions.read().await;
        assert_eq!(done.len(), attempts, "every create landed");
        assert_eq!(
            done.iter().filter(|s| s.status == SessionStatus::Starting).count(),
            max_parallel,
            "exactly the limit started, no matter how many collide"
        );
        assert_eq!(
            done.iter().filter(|s| s.status == SessionStatus::Queued).count(),
            attempts - max_parallel,
            "every colony past the limit queued instead"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn a_rush_of_creates_across_repositories_respects_the_per_repository_limit() {
        // The global limit is 8 and the org sets none, but each repository is held to 2: a batch of 13
        // on one repository (the rush that prompted the limit) starts only 2, and the others still start.
        let (max_parallel, repo_limit) = (8, 2);
        let batches = [("acme/api", 13), ("acme/web", 3), ("other/app", 1)];
        let attempts: usize = batches.iter().map(|(_, n)| n).sum();
        let sessions = Arc::new(RwLock::new(Vec::new()));
        let barrier = Arc::new(tokio::sync::Barrier::new(attempts));
        let mut tasks = Vec::new();
        for (repo, count) in batches {
            for i in 0..count {
                let (sessions, barrier) = (sessions.clone(), barrier.clone());
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    let id = format!("{repo}-{i}");
                    admit_create_in(&sessions, repo, max_parallel, None, repo_limit, id).await;
                }));
            }
        }
        for task in tasks {
            task.await.expect("create task joined");
        }
        let done = sessions.read().await;
        let starting = |repo: &str| {
            done.iter()
                .filter(|s| s.repo == repo && s.status == SessionStatus::Starting)
                .count() as u64
        };
        assert_eq!(
            starting("acme/api"),
            repo_limit,
            "the batch never passes its repository's limit"
        );
        assert_eq!(starting("acme/web"), repo_limit, "a sibling repository has its own");
        assert_eq!(starting("other/app"), 1, "and another org's repository is untouched");
        assert_eq!(
            done.iter().filter(|s| s.status == SessionStatus::Queued).count() as u64,
            attempts as u64 - 2 * repo_limit - 1,
            "every colony past a limit queued instead"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn a_mixed_rush_of_creates_and_resumes_respects_the_per_org_limit() {
        // The global limit is 8, but acme is held to 2 of them: acme floods the queue with both creates and
        // resumes, while another org keeps starting, since acme's limit holds back only acme.
        let max_parallel = 8;
        let acme_limit = 2;
        let acme_org_limit = Some(acme_limit as u64);
        let acme_creates = 6;
        let acme_resumes = 6;
        let other_creates = 4;
        let attempts = acme_creates + acme_resumes + other_creates;
        let sessions = Arc::new(RwLock::new(
            (0..acme_resumes)
                .map(|i| stopped_colony_with_worktree("acme", format!("acme-resume-{i}")))
                .collect::<Vec<_>>(),
        ));
        let barrier = Arc::new(tokio::sync::Barrier::new(attempts));
        let mut tasks = Vec::new();
        for i in 0..attempts {
            let (sessions, barrier) = (sessions.clone(), barrier.clone());
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let (org, id, resume) = if i < acme_creates {
                    ("acme", format!("acme-create-{i}"), false)
                } else if i < acme_creates + acme_resumes {
                    ("acme", format!("acme-resume-{}", i - acme_creates), true)
                } else {
                    ("other", format!("other-create-{i}"), false)
                };
                let org_limit = if org == "acme" { acme_org_limit } else { None };
                if resume {
                    admit_resume(&sessions, org, max_parallel, org_limit, &id).await;
                } else {
                    admit_create(&sessions, org, max_parallel, org_limit, id).await;
                }
            }));
        }
        for task in tasks {
            task.await.expect("admission task joined");
        }
        let done = sessions.read().await;
        let starting = |org: &str| {
            done.iter()
                .filter(|s| s.org == org && s.status == SessionStatus::Starting)
                .count()
        };
        assert_eq!(starting("acme"), acme_limit, "acme never passes its own limit");
        assert_eq!(starting("other"), other_creates, "acme's limit holds back only acme");
        assert!(
            starting("acme") + starting("other") <= max_parallel,
            "the global limit holds across the mix too"
        );
        assert_eq!(
            done.iter().filter(|s| s.status == SessionStatus::Queued).count(),
            attempts - starting("acme") - starting("other"),
            "every colony past a limit queued instead"
        );
    }

    // -- suspending colonies that wait on an answer (issue #562) ---------------------------------

    /// The smallest agent module, with or without the `session_resume` declaration that makes a
    /// colony's suspension possible at all.
    fn agent_module(id: &str, resume_dir: Option<&str>) -> crate::modules::AgentModule {
        crate::modules::AgentModule::test(id)
            .dir(std::path::PathBuf::from("/opt/colonizer/agent"))
            .entry(vec!["runner.mjs".into()])
            .resume_dir(resume_dir.map(String::from))
    }

    /// A colony waiting on its user, with whatever session id its runner has (or has not) reported.
    fn waiting_colony(id: &str, agent: &str, agent_session: Option<&str>) -> Session {
        let mut s = colony("acme", SessionStatus::WaitingForAnswer);
        s.id = id.into();
        s.agent = agent.into();
        s.agent_session = agent_session.map(String::from);
        s
    }

    #[test]
    fn suspension_needs_a_resumable_agent_and_a_reported_session_id() {
        let agents = vec![agent_module("claude-code", Some("/root/.claude/projects"))];
        assert!(
            suspendable(&waiting_colony("a", "claude-code", Some("s1")), &agents),
            "transcript dir declared, session id reported: suspendable"
        );
        assert!(
            !suspendable(&waiting_colony("b", "claude-code", None), &agents),
            "without the session id there is nothing to resume into"
        );
        assert!(
            !suspendable(&waiting_colony("c", "shell", Some("s1")), &agents),
            "an agent whose module declares no transcript dir cannot pick its session back up"
        );
    }

    #[test]
    fn a_suspended_colony_holds_no_slot_but_stays_answerable() {
        let mut s = waiting_colony("a", "claude-code", Some("s1"));
        assert!(s.holds_slot(), "a waiting colony holds its slot like any live one");
        s.suspended = Some(Suspension {
            at: Utc::now(),
            snapshot: None,
            reason: WAITING_FOR_ANSWER.into(),
            path: SESSION_RESUME.into(),
        });
        assert!(!s.holds_slot(), "the suspension is the slot given back");
        assert!(
            s.status.is_live(),
            "the status stays waiting_for_answer, so the question is still answerable"
        );
    }

    /// The suspension tick, end to end over a throwaway App: past the grace a resumable colony's
    /// microVM claim is dropped (`suspended` set, status untouched), inside it and with the setting
    /// off nothing happens, and a colony that cannot resume keeps running with one log line.
    #[tokio::test]
    async fn the_queue_suspends_a_waiting_colony_only_once_it_is_allowed_and_past_its_grace() {
        /// Puts a waiting colony in the app, its question asked `minutes_ago` ago and still open —
        /// the claim requires the open question (issue #562), the same lock an answer takes.
        async fn asked(app: &Shared, id: &str, minutes_ago: i64) {
            app.sessions.write().await.push(waiting_colony(id, "claude-code", Some("s1")));
            std::fs::create_dir_all(app.session_dir(id)).unwrap();
            let rt = app.runtime(id).await;
            rt.activity.lock().await.question_since = Some(Utc::now() - chrono::Duration::minutes(minutes_ago));
            *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), crate::protocol::QuestionRisk::ReadOnly));
        }

        let root = std::env::temp_dir().join(format!("colonizer-suspend-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app_with_agents(
            &root,
            vec![agent_module("claude-code", Some("/root/.claude/projects"))],
            |_| {},
        );
        let modules = app.modules.read().await.clone();
        asked(&app, "past-grace", 20).await;
        asked(&app, "within-grace", 1).await;
        asked(&app, "no-timestamp", 20).await;
        // An exec-policy ask (issue #759) waited just as long, but its tool call is in flight.
        asked(&app, "exec-policy", 20).await;
        app.runtime("exec-policy")
            .await
            .question_holds_tool_call
            .store(true, std::sync::atomic::Ordering::SeqCst);
        app.runtime("no-timestamp").await.activity.lock().await.question_since = None;
        app.sessions
            .write()
            .await
            .push(waiting_colony("no-session-id", "claude-code", None));
        std::fs::create_dir_all(app.session_dir("no-session-id")).unwrap();
        app.runtime("no-session-id").await.activity.lock().await.question_since =
            Some(Utc::now() - chrono::Duration::minutes(20));
        app.sessions
            .write()
            .await
            .push(waiting_colony("other-agent", "shell", Some("s1")));
        std::fs::create_dir_all(app.session_dir("other-agent")).unwrap();
        app.runtime("other-agent").await.activity.lock().await.question_since = Some(Utc::now() - chrono::Duration::minutes(20));

        suspend_waiting_colonies(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        let suspended = by_id("past-grace");
        let suspension = suspended.suspended.as_ref().expect("past the grace, suspended");
        assert_eq!(suspension.reason, WAITING_FOR_ANSWER);
        assert_eq!(
            suspension.path, SESSION_RESUME,
            "no memory snapshot today: the agent resumes its own transcript"
        );
        assert_eq!(
            suspended.status,
            SessionStatus::WaitingForAnswer,
            "the status is untouched, so the question stays answerable"
        );
        assert!(!suspended.holds_slot(), "the slot is back");
        for id in ["within-grace", "no-timestamp", "no-session-id", "other-agent", "exec-policy"] {
            assert!(by_id(id).suspended.is_none(), "{id} must keep its microVM and its slot");
        }
        drop(sessions);

        // The setting off suspends nobody, however long the wait.
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("suspend_waiting".into(), json!(false));
        let modules = app.modules.read().await.clone();
        suspend_waiting_colonies(&app, &modules).await;
        // Back on, the same tick finishes the job for the one that is now past its grace too.
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("suspend_waiting".into(), json!(true));
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("suspend_after_minutes".into(), json!(1));
        let modules = app.modules.read().await.clone();
        suspend_waiting_colonies(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        assert!(
            by_id("within-grace").suspended.is_some(),
            "the setting off only delayed it: back on and past the (now one minute) grace, it suspends too"
        );
        assert!(
            by_id("no-timestamp").suspended.is_none(),
            "an unknown wait start never expires"
        );
        assert!(
            by_id("no-session-id").suspended.is_none(),
            "nothing to resume, never suspended"
        );
        assert!(
            by_id("other-agent").suspended.is_none(),
            "the agent cannot resume, never suspended"
        );
        assert!(
            by_id("exec-policy").suspended.is_none(),
            "an exec-policy ask holds a tool call in flight: not suspended within the cap, however short the grace"
        );
        assert!(by_id("past-grace").suspended.is_some(), "already suspended, left as it is");
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #759, the follow-up: a subagent's AskUserQuestion blocks the subagent's tool call just
    /// as an exec-policy ask does, and the runner says so with `blocking: true`. Fed through the live
    /// event path, such a colony keeps its microVM past the grace, while the lead's own question —
    /// its turn ended, a resume delivers the answer — is still suspended. Past the cap every
    /// question is suspended, blocking or not, so no colony holds a slot for ever.
    #[tokio::test]
    async fn a_question_a_tool_call_is_blocked_on_keeps_its_colony_until_the_cap() {
        async fn asked(app: &Shared, id: &str, question: &str, minutes_ago: i64) {
            app.sessions.write().await.push(waiting_colony(id, "claude-code", Some("s1")));
            std::fs::create_dir_all(app.session_dir(id)).unwrap();
            let rt = app.runtime(id).await;
            crate::events::handle_agent_event(app, id, &rt, question).await;
            assert!(rt.open_question.lock().await.is_some(), "{id}: the question is open");
            rt.activity.lock().await.question_since = Some(Utc::now() - chrono::Duration::minutes(minutes_ago));
        }
        const SUBAGENT: &str =
            r#"{"seq":1,"type":"question","question_id":"toolu_sub","blocking":true,"risk":"read_only","questions":[]}"#;
        const LEAD: &str = r#"{"seq":1,"type":"question","question_id":"toolu_lead","risk":"read_only","questions":[]}"#;
        const EXEC: &str =
            r#"{"seq":1,"type":"question","question_id":"toolu_bash","kind":"exec_policy","blocking":true,"questions":[]}"#;

        let root = std::env::temp_dir().join(format!("colonizer-suspend-blocking-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app_with_agents(
            &root,
            vec![agent_module("claude-code", Some("/root/.claude/projects"))],
            |_| {},
        );
        let modules = app.modules.read().await.clone();
        let cap = BLOCKING_QUESTION_CAP.num_minutes();
        asked(&app, "subagent", SUBAGENT, 20).await;
        asked(&app, "subagent-near-cap", SUBAGENT, cap - 1).await;
        asked(&app, "exec-policy", EXEC, 20).await;
        asked(&app, "lead", LEAD, 20).await;
        asked(&app, "subagent-past-cap", SUBAGENT, cap + 1).await;
        asked(&app, "exec-policy-past-cap", EXEC, cap + 1).await;

        suspend_waiting_colonies(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        for id in ["subagent", "subagent-near-cap", "exec-policy"] {
            assert!(
                by_id(id).suspended.is_none(),
                "{id}: a tool call is blocked on the question, so the colony keeps its microVM within the cap"
            );
        }
        assert!(
            by_id("lead").suspended.is_some(),
            "the lead's own question past the grace is still suspended: that saving stays"
        );
        for id in ["subagent-past-cap", "exec-policy-past-cap"] {
            let s = by_id(id);
            assert!(s.suspended.is_some(), "{id}: past the cap it is suspended anyway");
            assert_eq!(
                s.status,
                SessionStatus::WaitingForAnswer,
                "{id}: the question stays answerable"
            );
        }
        drop(sessions);
        let log = std::fs::read_to_string(app.session_dir("subagent-past-cap").join("harness.jsonl")).unwrap();
        assert!(
            log.contains("suspending anyway") && log.contains("the agent that asked is lost"),
            "the capped suspension says what it costs: {log}"
        );
        let log = std::fs::read_to_string(app.session_dir("lead").join("harness.jsonl")).unwrap();
        assert!(
            !log.contains("suspending anyway"),
            "the ordinary suspension keeps its own line: {log}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The restore pass, ahead of the queue's own admissions: an answered suspension claims the
    /// last slot through the same admission a launch answers to — keeping its held answer for the
    /// boot to deliver — while an older queued launch waits for the next tick, and a second
    /// answered suspension for which no room is left stays suspended.
    #[tokio::test]
    async fn an_answered_suspension_is_restored_ahead_of_an_older_queued_launch() {
        let root = std::env::temp_dir().join(format!("colonizer-restore-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(1));
        let modules = app.modules.read().await.clone();

        let mut answered = waiting_colony("answered", "claude-code", Some("s1"));
        answered.suspended = Some(Suspension {
            at: Utc::now() - chrono::Duration::minutes(5),
            snapshot: None,
            reason: WAITING_FOR_ANSWER.into(),
            path: SESSION_RESUME.into(),
        });
        answered.pending_answer = Some(PendingAnswer {
            question_id: "q1".into(),
            prompt: "Q: Which file name?\nA: hello.txt".into(),
            answered_at: Some(Utc::now()),
        });
        let mut second = answered.clone();
        second.id = "second".into();
        second.suspended.as_mut().unwrap().at = Utc::now();
        let mut older_launch = colony("acme", SessionStatus::Queued);
        older_launch.id = "older-launch".into();
        older_launch.created_at = Utc::now() - chrono::Duration::minutes(30);
        *app.sessions.write().await = vec![older_launch, second, answered];
        std::fs::create_dir_all(app.session_dir("answered")).unwrap();
        std::fs::create_dir_all(app.session_dir("second")).unwrap();

        restore_suspended(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        let restored = by_id("answered");
        assert_eq!(restored.status, SessionStatus::Starting, "the slot went to the answer first");
        assert!(restored.suspended.is_none(), "restored, no longer suspended");
        let kept = restored.pending_answer.as_ref().expect("the answer survives the claim");
        assert_eq!(kept.question_id, "q1", "the boot, not the claim, delivers it");
        assert!(
            by_id("older-launch").status == SessionStatus::Queued,
            "the queued launch is older but waits: the answer was ahead of it"
        );
        assert!(
            !has_room(
                &sessions,
                "acme",
                "acme/repo",
                orgs::global_max_parallel(&modules) as usize,
                None,
                repo_limit(&modules, &app.org_settings("acme")),
            ),
            "so the queue itself could not admit anything else right now"
        );
        let waiting = by_id("second");
        assert!(
            waiting.suspended.is_some() && waiting.status == SessionStatus::WaitingForAnswer,
            "no room left: the second answered suspension stays suspended with its answer"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The suspension record as the suspend pass writes it for a waiting colony.
    fn suspended_now() -> Suspension {
        Suspension {
            at: Utc::now(),
            snapshot: None,
            reason: WAITING_FOR_ANSWER.into(),
            path: SESSION_RESUME.into(),
        }
    }

    /// A suspended waiting colony carrying a warm-up request (issue #701). `started_at: None` is
    /// the request still waiting for a slot; `Some` is the pass's claim — status `Starting`,
    /// `ready_at` alongside.
    fn warming_colony(id: &str, requested_at: DateTime<Utc>, started_at: Option<DateTime<Utc>>) -> Session {
        let mut s = waiting_colony(id, "claude-code", Some("s1"));
        s.suspended = Some(suspended_now());
        s.prewarm = Some(Prewarm {
            requested_at,
            started_at,
            ready_at: started_at,
        });
        if started_at.is_some() {
            s.status = SessionStatus::Starting;
        }
        s
    }

    /// Issue #701: opening a suspended colony's question pre-warms it. The pass claims the last
    /// slot through the same admission every start answers to — keeping the suspension on the
    /// record until the answer lands — and the question moves to the fresh runtime. A warm-up that
    /// already holds an answer is left to its own boot (no second one), and the delivery: the
    /// answer goes down the fresh link as the runner's first user message, and the colony comes
    /// out of suspension for good. (Colonies are warmed by hand where a real microVM would be.)
    #[tokio::test]
    async fn a_prewarm_request_boots_once_and_the_answer_lands_in_the_running_vm() {
        let root = std::env::temp_dir().join(format!("colonizer-prewarm-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(1));
        let modules = app.modules.read().await.clone();

        *app.sessions.write().await = vec![warming_colony("warming", Utc::now(), None)];
        std::fs::create_dir_all(app.session_dir("warming")).unwrap();
        let rt = app.runtime("warming").await;
        *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), crate::protocol::QuestionRisk::ReadOnly));
        rt.activity.lock().await.question_since = Some(Utc::now());
        drop(rt);

        prewarm_requested(&app, &modules).await;
        {
            let sessions = app.sessions.read().await;
            let s = sessions.iter().find(|s| s.id == "warming").unwrap();
            assert_eq!(s.status, SessionStatus::Starting, "claimed for the warm-up boot");
            assert!(s.prewarming(), "the claim stamped started_at");
            assert!(
                s.suspended.is_some(),
                "still the suspension's charge: the question is what answers"
            );
            assert!(s.holds_slot(), "the warm-up holds its slot again");
        }
        // The fresh runtime remembers the question, so the graces and the answer paths survive the
        // rotation, and the re-emitted line is what a restart replays it open from.
        let question = app.runtime("warming").await.open_question.lock().await.clone();
        assert_eq!(question.as_ref().map(|(id, ..)| id.as_str()), Some("q1"));

        // The delivery half runs on a colony warmed by hand — the pass's own boot cannot run a
        // real microVM here. The state below is the one a warm-up with a landed answer is in:
        // claimed, up, still suspended, answer held.
        let mut delivered = warming_colony("delivered", Utc::now(), Some(Utc::now()));
        delivered.pending_answer = Some(PendingAnswer {
            question_id: "q1".into(),
            prompt: "Q: Which file name?\nA: hello.txt".into(),
            answered_at: Some(Utc::now()),
        });
        app.sessions.write().await.push(delivered);
        std::fs::create_dir_all(app.session_dir("delivered")).unwrap();

        // No second boot: the restore pass leaves an answered warm-up alone — its own boot is the
        // one the answer rides.
        restore_suspended(&app, &modules).await;
        let s = app.session("delivered").await.unwrap();
        assert_eq!(
            s.status,
            SessionStatus::Starting,
            "no second boot: the warm-up is the boot this answer rides"
        );
        assert!(
            s.pending_answer.is_some() && s.suspended.is_some() && s.prewarming(),
            "the delivery is the warm-up's own wait, not the restore's"
        );

        // The runtime the delivery sends through — the pass materializes it for its own boots.
        drop(app.runtime("delivered").await);
        deliver_prewarmed(&app, "delivered").await;
        let mut rx = app
            .runtime("delivered")
            .await
            .commands_rx
            .lock()
            .await
            .take()
            .expect("the command channel");
        let sent = rx.try_recv().expect("the answer was sent down the link");
        assert_eq!(sent["type"], "user_message", "the runner gets the answer as a user message");
        assert_eq!(sent["id"], "initial", "the same shape the boot's initial prompt takes");
        assert!(sent["text"].as_str().unwrap().contains("hello.txt"));
        let s = app.session("delivered").await.unwrap();
        assert!(s.pending_answer.is_none() && s.suspended.is_none() && s.prewarm.is_none());
        assert!(s.holds_slot(), "live again, and holding its slot the ordinary way");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #701: a warm-up that gets no answer gives the slot back. Past the timeout the colony
    /// is suspended again — question still open and answerable — and the slot it freed is visible
    /// to the queue's admission at once, with an answer landing after the expiry still restored
    /// (the claim-first expiry leaves a properly suspended colony behind). A request that never
    /// got a slot expires too: opened and left alone, it is dropped instead of booting a colony
    /// nobody is looking at.
    #[tokio::test]
    async fn a_prewarm_with_no_answer_times_out_and_frees_the_slot() {
        let root = std::env::temp_dir().join(format!("colonizer-prewarm-timeout-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(1));
        let modules = app.modules.read().await.clone();

        // Warmed by hand (as the pass's claim leaves the record), so the pass's own boot task —
        // which cannot run a real microVM here — is not part of this test.
        let warming = warming_colony("warming", Utc::now(), Some(Utc::now() - chrono::Duration::minutes(10)));
        *app.sessions.write().await = vec![warming];
        std::fs::create_dir_all(app.session_dir("warming")).unwrap();
        assert!(app.session("warming").await.unwrap().holds_slot(), "warming, slot held");
        // A queued launch is waiting for exactly this slot.
        let mut queued = colony("acme", SessionStatus::Queued);
        queued.id = "queued".into();
        app.sessions.write().await.push(queued);

        assert!(prewarm_expire(&app, "warming").await, "the expiry landed");
        let s = app.session("warming").await.unwrap();
        assert_eq!(s.status, SessionStatus::WaitingForAnswer, "back to what the suspension left");
        assert!(s.suspended.is_some() && s.prewarm.is_none(), "suspended again, request spent");
        assert!(!s.holds_slot(), "the slot is back");
        {
            let sessions = app.sessions.read().await;
            assert!(
                has_room(
                    &sessions,
                    "acme",
                    "acme/repo",
                    orgs::global_max_parallel(&modules) as usize,
                    None,
                    repo_limit(&modules, &app.org_settings("acme")),
                ),
                "the queue can admit the launch it was waiting to"
            );
        }
        assert!(
            !prewarm_expire(&app, "warming").await,
            "an expired colony is not expired twice"
        );
        // An answer landing on the expired colony — held as `hold_answer` holds one on a suspended
        // colony — is not lost to the torn-down warm-up: the restore pass is the boot it rides.
        let held = PendingAnswer {
            question_id: "q1".into(),
            prompt: "Q: Which file name?\nA: hello.txt".into(),
            answered_at: Some(Utc::now()),
        };
        app.update_session("warming", |x| x.pending_answer = Some(held)).await;
        restore_suspended(&app, &modules).await;
        let s = app.session("warming").await.unwrap();
        assert_eq!(s.status, SessionStatus::Starting, "the held answer got its boot back");
        assert!(
            s.suspended.is_none() && s.pending_answer.is_some() && !s.prewarming(),
            "restored, not delivered into the dead warm-up link"
        );
        let _ = std::fs::remove_dir_all(root);

        // A request that waited for a slot past the timeout is dropped, not served: the pass
        // clears it and leaves the colony as the suspension left it.
        let root = std::env::temp_dir().join(format!("colonizer-prewarm-stale-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        let modules = app.modules.read().await.clone();
        *app.sessions.write().await = vec![warming_colony("stale", Utc::now() - chrono::Duration::minutes(10), None)];
        std::fs::create_dir_all(app.session_dir("stale")).unwrap();
        prewarm_requested(&app, &modules).await;
        let s = app.session("stale").await.unwrap();
        assert!(
            s.status == SessionStatus::WaitingForAnswer && s.suspended.is_some(),
            "never claimed: the request was stale"
        );
        assert!(
            s.prewarm.is_none(),
            "the spent request is off the record, so re-opening the question starts fresh"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #701: a pre-warm request never takes a slot ahead of a colony that already holds an
    /// answer — with the answers' restore and the warm-up racing for the same slots, the answers go
    /// first — and of two warm-up requests themselves, the older one is the one served.
    #[tokio::test]
    async fn a_prewarm_request_waits_behind_every_held_answer_and_serves_the_oldest_first() {
        let root = std::env::temp_dir().join(format!("colonizer-prewarm-order-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(2));
        let modules = app.modules.read().await.clone();

        let requested =
            |id: &str, minutes_ago: i64| warming_colony(id, Utc::now() - chrono::Duration::minutes(minutes_ago), None);
        let mut answered = requested("answered", 3);
        answered.prewarm = None;
        answered.pending_answer = Some(PendingAnswer {
            question_id: "q1".into(),
            prompt: "Q: Ship it?\nA: yes".into(),
            answered_at: Some(Utc::now()),
        });
        // (Four and one minute ago: five would read as past the default pre-warm timeout and be
        // dropped as stale, which is the pass's own rule.)
        *app.sessions.write().await = vec![answered, requested("older", 4), requested("newer", 1)];
        for id in ["answered", "older", "newer"] {
            std::fs::create_dir_all(app.session_dir(id)).unwrap();
        }

        restore_suspended(&app, &modules).await;
        prewarm_requested(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("answered").status, SessionStatus::Starting, "the answers went first");
        let served = by_id("older");
        assert_eq!(
            served.status,
            SessionStatus::Starting,
            "the older request is the one the last slot went to"
        );
        assert!(served.prewarming() && served.suspended.is_some(), "warming, still suspended");
        let waiting = by_id("newer");
        assert!(
            waiting.status == SessionStatus::WaitingForAnswer
                && waiting.suspended.is_some()
                && waiting.prewarm.as_ref().is_some_and(|p| p.started_at.is_none()),
            "no room left: the newer request keeps waiting for a later tick"
        );
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #667: the restore line runs in answer order, not suspension order. A was suspended
    /// first but answered last; with one slot free it is B, the earlier answer, that comes back,
    /// and A keeps its suspension and its answer for a later tick.
    #[tokio::test]
    async fn the_restore_line_runs_in_answer_order_not_suspension_order() {
        let root = std::env::temp_dir().join(format!("colonizer-restore-order-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(1));
        let modules = app.modules.read().await.clone();

        let answered = |id: &str, suspended_at: DateTime<Utc>, answered_at: DateTime<Utc>| {
            let mut s = waiting_colony(id, "claude-code", Some("s1"));
            s.suspended = Some(Suspension {
                at: suspended_at,
                snapshot: None,
                reason: WAITING_FOR_ANSWER.into(),
                path: SESSION_RESUME.into(),
            });
            s.pending_answer = Some(PendingAnswer {
                question_id: "q1".into(),
                prompt: "Q: Ship it?\nA: yes".into(),
                answered_at: Some(answered_at),
            });
            s
        };
        // Suspended ten minutes ago, answered ten seconds ago: suspension time would put it first.
        let mut a = answered(
            "suspended-first",
            Utc::now() - chrono::Duration::minutes(10),
            Utc::now() - chrono::Duration::seconds(10),
        );
        a.pending_answer.as_mut().unwrap().prompt = "A's answer".into();
        // Suspended later, answered a minute ago: the answer is what puts it in front.
        let b = answered(
            "answered-first",
            Utc::now() - chrono::Duration::minutes(5),
            Utc::now() - chrono::Duration::minutes(1),
        );
        *app.sessions.write().await = vec![a, b];
        std::fs::create_dir_all(app.session_dir("suspended-first")).unwrap();
        std::fs::create_dir_all(app.session_dir("answered-first")).unwrap();

        restore_suspended(&app, &modules).await;
        let sessions = app.sessions.read().await;
        let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
        assert_eq!(
            by_id("answered-first").status,
            SessionStatus::Starting,
            "the earlier answer is the one the slot went to"
        );
        let kept = by_id("suspended-first");
        assert!(
            kept.suspended.is_some() && kept.pending_answer.is_some() && kept.status == SessionStatus::WaitingForAnswer,
            "stays suspended with its answer, one step back in the line"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #673 meets the restore line (#667): an answered suspension a merge superseded is out
    /// of the line until it is kept — the slot goes to the later answer instead, the held colony
    /// keeps its suspension and its answer, nobody counts it as ahead of them, and its own note
    /// says it waits for a Keep rather than promising a resume.
    #[tokio::test]
    async fn a_superseded_answered_suspension_is_held_out_of_the_restore_line_until_kept() {
        let root = std::env::temp_dir().join(format!("colonizer-restore-supersede-{}", crate::util::short_id()));
        write_providers(&root, &["bailian"]);
        let app = crate::tests::test_app(&root);
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("max_parallel".into(), json!(1));
        let modules = app.modules.read().await.clone();

        let answered = |id: &str, answered_at: DateTime<Utc>| {
            let mut s = waiting_colony(id, "claude-code", Some("s1"));
            s.suspended = Some(Suspension {
                at: Utc::now() - chrono::Duration::minutes(10),
                snapshot: None,
                reason: WAITING_FOR_ANSWER.into(),
                path: SESSION_RESUME.into(),
            });
            s.pending_answer = Some(PendingAnswer {
                question_id: "q1".into(),
                prompt: "Q: Ship it?\nA: yes".into(),
                answered_at: Some(answered_at),
            });
            s
        };
        let mut held = answered("held", Utc::now() - chrono::Duration::minutes(5));
        held.superseded = Some(crate::supersede::Supersession {
            by: "merged".into(),
            pr_url: "https://github.com/acme/repo/pull/9".into(),
            pr: Some(9),
            title: "Fix the login".into(),
            reason: crate::supersede::OverlapReason::Issue,
            at: Utc::now(),
            kept: false,
        });
        let later = answered("later", Utc::now() - chrono::Duration::minutes(1));

        // The note, before anything moves: the held one waits for a Keep, and the later answer
        // does not count it as ahead.
        let snapshot = vec![held.clone(), later.clone()];
        assert!(
            restore_line_note(&snapshot, &held, 1, None, 1, false).contains("waits until it is kept"),
            "the held colony's note promises no resume"
        );
        assert_eq!(
            restore_line_note(&snapshot, &later, 1, None, 1, false),
            "a slot is free, so it resumes on the next queue tick",
            "a held colony stands in nobody's way"
        );

        *app.sessions.write().await = snapshot;
        std::fs::create_dir_all(app.session_dir("held")).unwrap();
        std::fs::create_dir_all(app.session_dir("later")).unwrap();
        restore_suspended(&app, &modules).await;
        {
            let sessions = app.sessions.read().await;
            let by_id = |id: &str| sessions.iter().find(|s| s.id == id).unwrap();
            assert_eq!(
                by_id("later").status,
                SessionStatus::Starting,
                "the slot went past the held colony"
            );
            let h = by_id("held");
            assert!(
                h.suspended.is_some() && h.pending_answer.is_some() && h.status == SessionStatus::WaitingForAnswer,
                "the held colony keeps its suspension and its answer"
            );
        }

        // Kept, it is back in the line: with the slot freed, the next pass restores it.
        app.update_session("later", |x| x.status = SessionStatus::Stopped).await;
        app.update_session("held", |x| {
            x.superseded.as_mut().is_some_and(crate::supersede::Supersession::mark_kept)
        })
        .await;
        restore_suspended(&app, &modules).await;
        assert_eq!(
            app.session("held").await.unwrap().status,
            SessionStatus::Starting,
            "kept: restored like any answered suspension"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The note a held answer's log line ends with (issue #667), pure over the snapshot: whether a
    /// slot is free, or who it waits behind — the tick's admission pause keeps the note from
    /// promising the next tick — and an old record that carries no answer time falls back to its
    /// suspension's own for the order.
    #[test]
    fn the_restore_line_note_says_what_the_next_ticks_will_do() {
        let now = Utc::now();
        let answered = |id: &str, answered_at: DateTime<Utc>| {
            let mut s = waiting_colony(id, "claude-code", Some("s1"));
            s.suspended = Some(Suspension {
                at: now - chrono::Duration::minutes(30),
                snapshot: None,
                reason: WAITING_FOR_ANSWER.into(),
                path: SESSION_RESUME.into(),
            });
            s.pending_answer = Some(PendingAnswer {
                question_id: "q1".into(),
                prompt: "Q: Ship it?\nA: yes".into(),
                answered_at: Some(answered_at),
            });
            s
        };
        let slot = colony("acme", SessionStatus::Running);
        let (max_parallel, org_limit, repo_limit) = (1, None, 1);
        let ours = answered("ours", now);

        assert_eq!(
            restore_line_note(&[], &ours, max_parallel, org_limit, repo_limit, false),
            "a slot is free, so it resumes on the next queue tick",
            "nothing busy, nobody ahead: the next tick brings it back"
        );
        let busy = vec![slot.clone(), ours.clone()];
        assert_eq!(
            restore_line_note(&busy, &ours, max_parallel, org_limit, repo_limit, false),
            "it is next in line when a slot frees",
            "the slot is taken, but nobody answered first"
        );
        // Paused, the tick restores nobody: even a free slot gets no next-tick promise.
        assert_eq!(
            restore_line_note(&[], &ours, max_parallel, org_limit, repo_limit, true),
            "launches are paused; it is next in line once the pause lifts",
        );
        assert_eq!(
            restore_line_note(&busy, &ours, max_parallel, org_limit, repo_limit, true),
            "launches are paused; it is next in line once the pause lifts",
        );
        let earlier = answered("earlier", now - chrono::Duration::minutes(1));
        let busy_line = vec![slot.clone(), earlier.clone(), ours.clone()];
        assert_eq!(
            restore_line_note(&busy_line, &ours, max_parallel, org_limit, repo_limit, false),
            "all slots are busy and 1 answered colony is ahead of it",
        );
        // The ahead wordings promise no tick, so they stand unchanged while paused.
        assert_eq!(
            restore_line_note(&busy_line, &ours, max_parallel, org_limit, repo_limit, true),
            "all slots are busy and 1 answered colony is ahead of it",
        );
        let second_earlier = answered("second-earlier", now - chrono::Duration::minutes(2));
        let busier_line = vec![slot, earlier.clone(), second_earlier, ours.clone()];
        assert_eq!(
            restore_line_note(&busier_line, &ours, max_parallel, org_limit, repo_limit, false),
            "all slots are busy and 2 answered colonies are ahead of it",
        );
        // A slot free despite the company: the ones ahead of it still go first, tick by tick.
        assert_eq!(
            restore_line_note(&[earlier, ours.clone()], &ours, 2, org_limit, repo_limit, false),
            "1 answered colony is ahead of it",
        );
        // Ties break by id, the order the restore pass itself sorts by: the same answer time, the
        // smaller id stands in front.
        let ahead_tie = answered("aaa-tie", now);
        let behind_tie = answered("zzz-tie", now);
        assert_eq!(
            answered_ahead(&[ahead_tie.clone(), behind_tie, ours.clone()], &ours),
            1,
            "same answer time, the smaller id stands ahead and the larger one behind"
        );
        assert_eq!(
            answered_ahead(std::slice::from_ref(&ours), &ahead_tie),
            0,
            "same answer time, ours is the larger id: nobody stands ahead of it"
        );
        // A record saved before answers kept a time restores by its suspension's own.
        let mut old = answered("old", now - chrono::Duration::minutes(40));
        old.pending_answer.as_mut().unwrap().answered_at = None;
        assert_eq!(
            answered_ahead(&[old, ours.clone()], &ours),
            1,
            "the old record stands ahead by its suspension time"
        );
    }

    /// A colony parked waiting on a Claude account (issue #984): parked with the account reason and
    /// a kept worktree, routed to `account`.
    fn account_waiting_colony(id: &str, account: &str) -> Session {
        let mut s = colony("acme", SessionStatus::Parked);
        s.id = id.into();
        s.claude_account = Some(account.into());
        s.git_admin_dir = Some("git".into());
        s.attention = Some(json!({"reason": crate::account_health::WAITING_FOR_ACCOUNT_REASON, "nudges": 0}));
        s.parked = Some(crate::sessions::Park {
            at: Utc::now(),
            reason: crate::account_health::WAITING_FOR_ACCOUNT_REASON.into(),
            resets_at: None,
            vm_kept: false,
            question_risk: None,
        });
        s
    }

    /// Issue #984: a colony parked waiting on an account stays parked while the account is broken and
    /// rejoins the queue once it works again — the record of trouble is what gates the resume.
    #[tokio::test]
    async fn a_colony_waiting_on_an_account_resumes_only_once_the_account_works() {
        let root = std::env::temp_dir().join(format!("colonizer-account-wait-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut sessions = vec![account_waiting_colony("waiting", "default")];
        for i in 0..3 {
            let mut f = colony("acme", SessionStatus::Running);
            f.id = format!("filler-{i}");
            sessions.push(f);
        }
        *app.sessions.write().await = sessions;
        tokio::fs::create_dir_all(app.session_dir("waiting")).await.unwrap();
        assert_eq!(crate::account_health::waiting_on(&app.sessions.read().await, "default"), 1);

        assert!(
            crate::account_health::record_failure(&app, "default", 401).await,
            "the account is marked"
        );
        resume_waiting_for_account(&app).await;
        assert_eq!(
            app.session("waiting").await.unwrap().status,
            SessionStatus::Parked,
            "still broken, still parked"
        );

        assert!(
            crate::account_health::record_ok(&app, "default").await,
            "the account works again"
        );
        resume_waiting_for_account(&app).await;
        let resumed = app.session("waiting").await.unwrap();
        assert_eq!(resumed.status, SessionStatus::Queued, "the colony rejoins the queue");
        assert!(resumed.parked.is_none(), "the park goes with the resume");
        let _ = std::fs::remove_dir_all(root);
    }
}
