//! The colony queue: when every parallel slot is taken, a new colony waits instead of being
//! refused, and a loop starts the oldest waiting colony that fits each time a slot frees up.
//!
//! Whether a colony fits is a pure function (`has_room`), so the admission rule can be tested
//! apart from the loop that applies it.

use crate::{Shared, orgs};
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

/// What the queue decided to do with the queued colony it examined under the admission lock. `None` from
/// [`claim_queued`] means this tick leaves it be: the room it counted on vanished, or the colony was
/// claimed or removed in between, and the next tick looks again.
enum Claim {
    /// The colony is admitted: it is `Starting`, and the caller boots it.
    Start(Session),
    /// The colony can never start, so the claim has taken it out of the queue; the caller says so and
    /// moves on to the colonies behind it.
    Retire(Session),
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
        return Some(Claim::Retire(s.clone()));
    }
    s.status = SessionStatus::Starting;
    s.updated_at = Utc::now();
    Some(Claim::Start(s.clone()))
}

pub(crate) async fn start_queued(app: &Shared) {
    let modules = app.modules.read().await.clone();
    let max_parallel = orgs::global_max_parallel(&modules) as usize;
    // Several slots can free at once, so keep going until nothing else fits.
    loop {
        let sessions = app.sessions.read().await.clone();
        let mut waiting: Vec<&Session> = sessions.iter().filter(|s| s.status == SessionStatus::Queued).collect();
        waiting.sort_by_key(|s| s.created_at);
        let Some(next) = waiting
            .into_iter()
            .find(|s| {
                has_room(
                    &sessions,
                    &s.org,
                    max_parallel,
                    orgs::org_max_parallel(&app.org_settings(&s.org)),
                )
            })
            .cloned()
        else {
            return;
        };
        // Re-checked and claimed under one write lock, so neither another tick nor a concurrent create or
        // resume can take the slot in between.
        let org_limit = orgs::org_max_parallel(&app.org_settings(&next.org));
        let claimed = with_slot(&app.sessions, &next.org, max_parallel, org_limit, |sessions, room| {
            let s = sessions.iter_mut().find(|s| s.id == next.id)?;
            claim_queued(s, room)
        })
        .await;
        match claimed {
            None => return,
            Some(Claim::Retire(retired)) => {
                app.persist_and_broadcast(&retired).await;
                app.session_log(
                    &retired.id,
                    "warn",
                    "was cleaned up while it waited in the queue, so it can never start".into(),
                )
                .await;
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
            matches!(claim, Some(Claim::Retire(_))),
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
