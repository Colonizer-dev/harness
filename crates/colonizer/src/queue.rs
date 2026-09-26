//! The colony queue: when every parallel slot is taken, a new colony waits instead of being
//! refused, and a loop starts the oldest waiting colony that fits each time a slot frees up.
//!
//! Whether a colony fits is a pure function (`has_room`), so the admission rule can be tested
//! apart from the loop that applies it.

use crate::{Shared, orgs, provider_quota, providers, restack, spend, stack::Stacked};
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

/// The attention reason a colony parked for outlasting its hold carries. `Stopped` stands in until
/// #213 adds `Parked`, the same stand-in quota parking uses — and the reason string is what keeps
/// [`resume_quota_parked`] from requeueing these: it only matches its own reason.
pub(crate) const HOLD_TIMEOUT_REASON: &str = "hold_timeout";

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

/// Starts queued colonies as slots free up, oldest first. A colony whose org or repository is at its own
/// limit doesn't hold up the ones behind it, and neither does one waiting for the branch of the colony it
/// is stacked on.
pub async fn run_queue(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
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
}

fn gate(s: &Session, sessions: &[Session]) -> Gate {
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
    let cleared = s.clear_attention();
    s.updated_at = Utc::now();
    let mut message = format!("can never start: {reason}");
    if let Some(note) = cleared_attention_message(&cleared) {
        message.push_str("; ");
        message.push_str(&note);
    }
    Some(Claim::Retire(s.clone(), message))
}

/// The queued colony this tick acts on, oldest first: the first one nothing holds back and that fits.
/// A colony still waiting for its stacked-on parent's branch is looked past, so a slow parent cannot
/// stall the colonies behind it, and the first one whose parent can never provide a branch stops the
/// walk — it is retired where it stands, which needs no slot. `None` when nothing in the queue can
/// move this tick.
fn next_queued(sessions: &[Session], room: impl Fn(&Session) -> bool) -> Option<(&Session, Option<String>)> {
    let mut waiting: Vec<&Session> = sessions.iter().filter(|s| s.status == SessionStatus::Queued).collect();
    waiting.sort_by_key(|s| s.created_at);
    for candidate in waiting {
        match gate(candidate, sessions) {
            Gate::Hold => continue,
            Gate::Retire(reason) => return Some((candidate, Some(reason))),
            Gate::Admit => {
                if room(candidate) {
                    return Some((candidate, None));
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
    if !paused {
        restore_suspended(app, &modules).await;
    }
    // Several slots can free at once, so keep going until nothing else fits.
    loop {
        let sessions = app.sessions.read().await.clone();
        let limits = |org: &str| {
            let settings = app.org_settings(org);
            (orgs::org_max_parallel(&settings), repo_limit(&modules, &settings))
        };
        let Some((next, refuse)) = next_queued(&sessions, |s| {
            let (org_limit, repo_limit) = limits(&s.org);
            !paused && has_room(&sessions, &s.org, &s.repo, max_parallel, org_limit, repo_limit)
        }) else {
            return;
        };
        // A waiter the gate called ready is checked against the forge before its promotion: the holder's
        // PR may have merged, or the claim may have moved to someone else, and the check is a gh call, so
        // it runs here rather than under the admission lock (issue #321). A failure falls back to the
        // local guard, the same as admission does.
        let refuse = match refuse {
            Some(reason) => Some(reason),
            None if next.claim_wait => waiter_remote_conflict(app, next, &sessions).await,
            None => None,
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
                match refuse.as_deref() {
                    Some(reason) => claim_refused(s, reason),
                    None => claim_queued(s, room),
                }
            },
        )
        .await;
        match claimed {
            None => return,
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
                tokio::spawn(boot(app.clone(), next.id.clone(), resume));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// agentd transport
// ---------------------------------------------------------------------------

/// Parks every autopilot hold past its timeout: the same stop quota parking takes — microVM removed,
/// worktree kept, slot released — with the hold-timeout attention reason instead of a hold. The stop
/// is claimed under the colony's lifecycle lock with the expiry re-checked under the admission lock,
/// so a colony answered in the meantime is not parked. Parked-as-`Stopped` is resumable
/// (`can_resume`), and [`resume_quota_parked`] leaves these alone — it only matches its own reason.
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
        let parked = stop_colony(
            app,
            &s,
            |x| hold_expired(x, Utc::now(), timeout),
            format!(
                "autopilot hold exceeded {minutes} min with no answer; parked to release its slot — the worktree is kept, so press Resume to continue"
            ),
            format!("autopilot hold exceeded {minutes} min; parked to release its slot — worktree kept, resume to continue"),
        )
        .await;
        if parked {
            app.update_session(&id, stamp_hold_timeout).await;
        }
    }
}

/// Stamps the hold-timeout attention onto a parked colony. Conditional on purpose: a resume may have
/// claimed the colony (Stopped→Starting) between the stop's claim and this write, now that the
/// lifecycle lock is released — stamping unconditionally would mislabel a live colony as parked.
fn stamp_hold_timeout(x: &mut Session) {
    if x.status == SessionStatus::Stopped && !x.cleaned_up {
        x.attention = Some(json!({"reason": HOLD_TIMEOUT_REASON, "since": Utc::now(), "nudges": 0}));
    }
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
        // How the colony comes back. Today's sandbox has no memory snapshot (the seam in
        // `sandbox::supports_memory_snapshot`), so the path is always the fallback: the agent
        // resumes its own session transcript in a fresh microVM.
        let path = if crate::sandbox::supports_memory_snapshot() {
            "memory_snapshot"
        } else {
            SESSION_RESUME
        };
        let claimed = app
            .update_session(&id, |x| {
                if x.status != SessionStatus::WaitingForAnswer || x.suspended.is_some() {
                    return false;
                }
                x.suspended = Some(Suspension {
                    at: Utc::now(),
                    snapshot: None,
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
        let minutes = grace.num_minutes();
        app.session_log(
            &id,
            "info",
            format!(
                "no answer for {minutes} min; suspending — the microVM is removed, the worktree and the \
                 agent's session transcript are kept, and the question stays answerable"
            ),
        )
        .await;
        let mut entry = crate::activity::Entry::new("outcome.suspended", "colony").colony(&s);
        entry.detail = Some(format!(
            "waiting {minutes} min for an answer; the worktree and the agent's session are kept"
        ));
        crate::activity::record(app, entry).await;
        teardown_vm(app, &s).await;
    }
}

/// Restores suspended colonies that hold an undelivered answer, ahead of the fresh launches in the
/// admission loop (issue #562): the answer is what the user has been waiting for. Each restore
/// claims a slot through the same admission every launch answers to; the claim clears the
/// suspension — so `holds_slot` is true for the boot — but keeps the answer, which the boot itself
/// delivers once the runner is up, so a failed boot leaves it on the record. The link drop and the
/// event-log rotation are the resume handler's, for the same reason: the fresh microVM's agentd
/// numbers events from 1, and a stale log would swallow them.
pub(crate) async fn restore_suspended(app: &Shared, modules: &crate::config::ModulesConfig) {
    let mut candidates: Vec<(DateTime<Utc>, String)> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| s.suspended.is_some() && s.pending_answer.is_some() && s.status == SessionStatus::WaitingForAnswer)
            .map(|s| (s.suspended.as_ref().map(|x| x.at).unwrap_or(s.updated_at), s.id.clone()))
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
        if s.status != SessionStatus::WaitingForAnswer || s.suspended.is_none() || s.pending_answer.is_none() {
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
                x.suspended = None;
                x.error = None;
                x.attention = None;
                x.mesh = None;
                x.local_port = None;
                // The last boot's phases would read as this one's under `starting`.
                x.boot_timing = None;
                x.updated_at = Utc::now();
                Some(x.clone())
            },
        )
        .await;
        let Some(s) = claimed else { continue };
        app.persist_and_broadcast(&s).await;
        // The colony is ours: drop the old agent link and rotate the event log, exactly as the
        // resume handler does, with the log's own file lock held across the rename.
        let runtime = app.runtimes.lock().await.remove(&id);
        if let Some(rt) = &runtime {
            rt.stop.send_replace(true);
            rt.retired.send_replace(true);
        }
        let dir = app.session_dir(&id);
        let rotated = {
            let _file_lock = match runtime.as_ref() {
                Some(rt) => Some(rt.file_lock.lock().await),
                None => None,
            };
            rotate_events(&dir)
        };
        if let Err(e) = rotated {
            // Put the colony back as the suspension left it, answer included, and stop this tick:
            // a rotation failure is a storage problem, and a retry every 5 s would only churn.
            let e = anyhow::Error::from(e);
            if let Some((x, ())) = app
                .update_session(&id, |x| {
                    x.status = SessionStatus::WaitingForAnswer;
                    x.suspended = suspension.clone();
                })
                .await
            {
                app.persist_and_broadcast(&x).await;
            }
            app.storage_failed("rotate the old event log", &e).await;
            app.session_log(
                &id,
                "error",
                format!("could not move the old event log aside ({e}); the colony stays suspended and its answer kept"),
            )
            .await;
            break;
        }
        app.session_log(
            &id,
            "info",
            "answer in hand; booting a fresh microVM to resume the agent's session with it".into(),
        )
        .await;
        tokio::spawn(boot(app.clone(), id, s.git_admin_dir.is_some()));
    }
}

/// Quota-parked colonies whose provider is no longer exhausted rejoin the queue as `Queued` — the
/// worktree never left, so the normal admission loop resumes them like any operator resume. A
/// named provider recovers when its record lapses (reset passed) or is gone (provider deleted); an
/// unnamed one recovers when nothing is exhausted anywhere, the account record included — an
/// account-parked colony stays parked while the account record holds and resumes when it lapses.
pub(crate) async fn resume_quota_parked(app: &Shared) {
    let ids: Vec<String> = {
        let sessions = app.sessions.read().await;
        let ids: Vec<String> = app.providers().iter().map(|p| p.id.clone()).collect();
        let any_exhausted = !app.gateway.quota_exhausted().is_empty();
        sessions
            .iter()
            .filter(|s| {
                s.status == SessionStatus::Stopped
                    && s.attention
                        .as_ref()
                        .is_some_and(|a| a["reason"].as_str() == Some(provider_quota::QUOTA_EXHAUSTED_REASON))
                    && !s.cleaned_up
                    && s.git_admin_dir.is_some()
                    && match provider_quota::mentioned_provider(s.error.as_deref().unwrap_or_default(), &ids, &[]) {
                        Some(pid) => !app.gateway.is_quota_exhausted(&pid),
                        None => !any_exhausted,
                    }
            })
            .map(|s| s.id.clone())
            .collect()
    };
    for id in ids {
        // Queued holds no slot, so the flip needs no admission; the loop below boots it. The status
        // is re-checked under the lock, so a concurrent operator resume wins instead of doubling.
        let flipped = app
            .update_session(&id, |x| {
                if x.status != SessionStatus::Stopped {
                    return false;
                }
                x.status = SessionStatus::Queued;
                x.error = None;
                x.attention = None;
                x.updated_at = Utc::now();
                true
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // The ordinary case: still parked, so the stamp lands.
        let mut parked = stopped_colony_with_worktree("acme", "parked".into());
        stamp_hold_timeout(&mut parked);
        assert_eq!(
            parked.attention.as_ref().and_then(|a| a["reason"].as_str()),
            Some(HOLD_TIMEOUT_REASON)
        );
        // A resume that flipped the colony back to Starting between the stop and the stamp wins:
        // stamping hold_timeout onto a live colony would mislabel it as parked.
        let mut resumed = stopped_colony_with_worktree("acme", "resumed".into());
        resumed.status = SessionStatus::Starting;
        stamp_hold_timeout(&mut resumed);
        assert!(resumed.attention.is_none(), "a resumed colony keeps no park reason");
        // Cleaned up in between: the worktree the reason promises is gone, so no stamp either.
        let mut cleaned = stopped_colony_with_worktree("acme", "cleaned".into());
        cleaned.cleaned_up = true;
        stamp_hold_timeout(&mut cleaned);
        assert!(cleaned.attention.is_none());
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

    #[test]
    fn parked_colonies_hold_no_slots() {
        let mut live: Vec<Session> = (0..14).map(|_| colony("acme", SessionStatus::Running)).collect();
        assert!(
            !has_room(&live, "acme", "acme/repo", 14, None, 32),
            "14 live colonies fill 14 slots"
        );
        // Parking flips live colonies to Stopped, which holds nothing.
        for s in &mut live {
            s.status = SessionStatus::Stopped;
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
        assert!(refuse.is_none());
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
        assert!(refuse.is_none(), "the unrelated colony starts, it is not retired");
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
        assert!(refuse.is_none());
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
        assert!(refuse.is_none());
    }

    #[test]
    fn a_child_whose_parent_failed_is_retired_with_a_message_naming_the_parent() {
        let sessions = vec![
            queued_child("child", "parent", Utc::now()),
            parent_colony("parent", SessionStatus::Failed, "colonizer/issue-1-parent"),
        ];
        let Some((picked, refuse)) = next_queued(&sessions, |_| true) else {
            panic!("the child is retired, not left sitting at the head of the queue");
        };
        assert_eq!(picked.id, "child", "the child is what this tick acts on");
        let reason = refuse.expect("the retirement says why");
        assert!(reason.contains("parent"), "the parent is named: {reason}");
        assert!(reason.contains("failed"), "and the reason is named: {reason}");

        // The claim takes the child out of the queue with that reason as its error.
        let mut queued = queued_child("child", "parent", Utc::now());
        let claim = claim_refused(&mut queued, &reason);
        assert!(matches!(claim, Some(Claim::Retire(..))), "retired, not started");
        assert_eq!(queued.status, SessionStatus::Failed, "out of the queue for good");
        assert_eq!(queued.error.as_deref(), Some(reason.as_str()));
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
            refuse.is_none(),
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
        assert!(refuse.is_none());
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

    fn quota_parked(id: &str, error: &str) -> Session {
        let mut s = stopped_colony_with_worktree("acme", id.into());
        s.status = SessionStatus::Stopped;
        s.error = Some(error.into());
        s.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "since": Utc::now(), "nudges": 0}));
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
            assert_eq!(s.status, SessionStatus::Stopped, "an expired hold parks");
            assert_eq!(
                s.attention.as_ref().and_then(|a| a["reason"].as_str()),
                Some(HOLD_TIMEOUT_REASON),
                "parked-as-stopped under the hold-timeout reason"
            );
            assert_eq!(s.attention.as_ref().and_then(|a| a["nudges"].as_u64()), Some(0));
            assert!(s.attention.as_ref().and_then(|a| a["since"].as_str()).is_some());
            assert!(
                !s.cleaned_up && s.git_admin_dir.is_some(),
                "the worktree is kept, never cleaned up"
            );
            assert!(
                can_resume(s.status, s.cleaned_up, s.git_admin_dir.is_some()),
                "a parked hold resumes like any stopped colony"
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
                .all(|s| s.status == SessionStatus::Stopped),
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

    #[tokio::test]
    async fn the_queue_holds_a_waiting_child_and_retires_one_whose_parent_is_gone() {
        let root = std::env::temp_dir().join(format!("colonizer-queue-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut waiting = queued_child("waits", "live-parent", Utc::now());
        waiting.created_at = Utc::now() - chrono::Duration::minutes(2);
        let mut doomed = queued_child("doomed", "dead-parent", Utc::now());
        doomed.created_at = Utc::now() - chrono::Duration::minutes(1);
        let sessions = vec![
            parent_colony("live-parent", SessionStatus::Running, "colonizer/issue-1-live"),
            parent_colony("dead-parent", SessionStatus::Failed, ""),
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
        assert_eq!(retired.status, SessionStatus::Failed, "its parent can never provide a branch");
        let error = retired.error.as_deref().unwrap_or_default();
        assert!(error.contains("dead-parent") && error.contains("failed"), "{error}");
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
        crate::modules::AgentModule {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            dir: std::path::PathBuf::from("/opt/colonizer/agent"),
            entry: vec!["runner.mjs".into()],
            needs_claude: false,
            schema: json!({}),
            egress: None,
            resume_dir: resume_dir.map(String::from),
        }
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
        for id in ["within-grace", "no-timestamp", "no-session-id", "other-agent"] {
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
        assert!(by_id("past-grace").suspended.is_some(), "already suspended, left as it is");
        drop(sessions);
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
}
