//! The colony queue: when every parallel slot is taken, a new colony waits instead of being
//! refused, and a loop starts the oldest waiting colony that fits each time a slot frees up.
//!
//! Whether a colony fits is a pure function (`has_room`), so the admission rule can be tested
//! apart from the loop that applies it.

use crate::{
    Shared, orgs, spend,
    stack::{self, Stacked},
};
use chrono::Utc;
use std::time::Duration;
use tokio::sync::RwLock;

#[allow(unused_imports)]
use crate::{events::*, lifecycle::*, publish::*, sessions::*};

/// Whether another colony can start right now. A queued colony holds no microVM, so it counts towards
/// neither the global limit nor the org's.
pub(crate) fn has_room(sessions: &[Session], org: &str, max_parallel: usize, org_limit: Option<u64>) -> bool {
    let busy = |s: &&Session| s.status.is_live() || s.status == SessionStatus::Publishing;
    if sessions.iter().filter(busy).count() >= max_parallel {
        return false;
    }
    match org_limit {
        Some(limit) => (sessions.iter().filter(busy).filter(|s| s.org == org).count() as u64) < limit,
        None => true,
    }
}

/// Check for a free slot and claim it without letting go of the lock in between: `claim` runs while the
/// write guard is still held, so nothing can slip between the check and the claim and two launches can
/// never both take the last free slot. `max_parallel` and `org_limit` must be resolved before calling this
/// (`org_settings` does blocking file IO), and `claim` must not `.await` anything.
pub(crate) async fn with_slot<T>(
    sessions: &RwLock<Vec<Session>>,
    org: &str,
    max_parallel: usize,
    org_limit: Option<u64>,
    claim: impl FnOnce(&mut Vec<Session>, bool) -> T,
) -> T {
    let mut guard = sessions.write().await;
    let room = has_room(&guard, org, max_parallel, org_limit);
    claim(&mut guard, room)
}

/// Starts queued colonies as slots free up, oldest first. A colony whose org is at its own limit
/// doesn't hold up the ones behind it, and neither does one waiting for the branch of the colony it
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
}

/// What holds a queued colony back before the slot rules even apply, decided on the snapshot: the
/// stacking rule (`stack::stacked_on`) is the decision, and this is the queue's reading of it.
enum Gate {
    /// Nothing holds it back; the slot rules decide, as for any colony.
    Admit,
    /// Its parent has not pushed a branch yet. Look past it this tick — a child waiting on a slow
    /// parent must not stall the colonies behind it — and look again next tick.
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
    let Some(parent_id) = s.parent.as_deref() else {
        return Gate::Admit;
    };
    let parent = sessions.iter().find(|p| p.id == parent_id);
    match stack::stacked_on(parent_id, parent) {
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
        s.updated_at = Utc::now();
        return Some(Claim::Retire(
            s.clone(),
            "was cleaned up while it waited in the queue, so it can never start".into(),
        ));
    }
    s.status = SessionStatus::Starting;
    s.updated_at = Utc::now();
    Some(Claim::Start(s.clone()))
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
    s.updated_at = Utc::now();
    Some(Claim::Retire(s.clone(), format!("can never start: {reason}")))
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
    // Below the free-space floor: queued colonies that would start hold until the reclaim tick
    // frees room. The hold rides in `room`, not an early return: retiring a colony that can never
    // start takes no slot, and leaving it Queued would stall the queue head (and all the disk the
    // tick is trying to free behind it) for as long as the floor holds.
    let paused = crate::reclaim::admission_paused(app).await;
    let modules = app.modules.read().await.clone();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Several slots can free at once, so keep going until nothing else fits.
    loop {
        let sessions = app.sessions.read().await.clone();
        let Some((next, refuse)) = next_queued(&sessions, |s| {
            !paused
                && has_room(
                    &sessions,
                    &s.org,
                    max_parallel,
                    orgs::org_max_parallel(&app.org_settings(&s.org)),
                )
        }) else {
            return;
        };
        // Re-checked and claimed under one write lock, so neither another tick nor a concurrent create or
        // resume can take the slot in between.
        let org_limit = orgs::org_max_parallel(&app.org_settings(&next.org));
        let claimed = with_slot(&app.sessions, &next.org, max_parallel, org_limit, |sessions, room| {
            let s = sessions.iter_mut().find(|s| s.id == next.id)?;
            match refuse.as_deref() {
                Some(reason) => claim_refused(s, reason),
                None => claim_queued(s, room),
            }
        })
        .await;
        match claimed {
            None => return,
            Some(Claim::Retire(retired, message)) => {
                // A queued colony that can never start crosses straight into its terminal state
                // outside `update_session` (the claim writes the record directly), so the journal
                // hears about the return here, not there.
                spend::record_returned(app, &retired.org).await;
                app.persist_and_broadcast(&retired).await;
                app.session_log(&retired.id, "warn", message).await;
                continue;
            }
            Some(Claim::Start(starting)) => {
                app.persist_and_broadcast(&starting).await;
                app.session_log(&next.id, "info", "a slot came free; starting".into()).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::{admit_create, admit_resume, colony, stopped_colony_with_worktree};
    use std::sync::Arc;

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
        let mixed = vec![
            colony("acme", SessionStatus::Running),
            colony("other", SessionStatus::Running),
        ];
        assert!(!has_room(&mixed, "acme", 5, Some(1)), "acme is at its own limit");
        assert!(has_room(&mixed, "third", 5, Some(1)), "another org still has room");
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

    /// A queued colony stacked on `parent_id`, in the queue ahead of anything created later.
    fn queued_child(id: &str, parent_id: &str, created_at: chrono::DateTime<chrono::Utc>) -> Session {
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
}
