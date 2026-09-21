//! Publishing a colony's work: the Create PR button and the background job it claims a session
//! with, which stops the agent, removes the microVM, and hands the worktree to `github::publish`.
//!
//! The pull-request watcher lives here too: once a colony's work is published, what happens to
//! that pull request is the last thing the colony's badge still reflects.

use crate::{ApiResult, App, Shared, client_error, github, stack, util::truncate};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[allow(unused_imports)]
use crate::{events::*, lifecycle::*, queue::*, sessions::*};

/// How often a colony's pull request is checked, at first and at most: this spends the user's GitHub API
/// quota, so a freshly opened PR is noticed within a minute while one sitting for days costs an hour.
pub(crate) const PR_POLL_FIRST: Duration = Duration::from_secs(60);
pub(crate) const PR_POLL_MAX: Duration = Duration::from_secs(60 * 60);

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
    let Some((s, (claimed, was_live))) = app.update_session(&id, claim_publish).await else {
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
            // A fix colony's pull request is reviewed as it opens: a fresh independent session
            // judges the change (validation.rs) and merges it only when the review passes and the
            // operator asked for that. Off the publish path: the review reads the session fresh, so
            // nothing stale from this moment travels with it.
            if s.fix_for.is_some() {
                tokio::spawn(crate::validation::review_fix_pr(app.clone(), id.clone()));
            }
        }
        Err(e) => {
            let message = format!("{e:#}");
            log.error(format!("publishing failed: {message}")).await;
            let mut attention = None;
            app.update_session(&id, |x| {
                x.status = SessionStatus::Failed;
                x.error = Some(truncate(&message, 2000));
                attention = x.clear_attention();
            })
            .await;
            app.note_cleared_attention(&id, attention).await;
        }
    }
}

/// Colonies whose pull request still needs watching. `merged` is final; a closed PR can be reopened,
/// so `closed` keeps being watched.
pub(crate) fn pr_watched(status: SessionStatus, has_pr: bool) -> bool {
    has_pr && matches!(status, SessionStatus::PrOpened | SessionStatus::Closed)
}

/// Whether a pull request is due for its next check.
pub(crate) fn pr_due(last_checked: Instant, backoff: Duration, now: Instant) -> bool {
    now.duration_since(last_checked) >= backoff
}

/// The next check interval: doubled after a check with no news, reset when the state actually changed.
/// A failed `gh` call counts as no news, so a broken checkout backs off like an untouched PR.
pub(crate) fn pr_backoff(current: Duration, changed: bool) -> Duration {
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
        polling.retain(|id, _| {
            sessions
                .iter()
                .any(|s| &s.id == id && pr_watched(s.status, s.pr_url.is_some()))
        });
        for s in sessions.iter().filter(|s| pr_watched(s.status, s.pr_url.is_some())) {
            let Some(url) = s.pr_url.clone() else { continue };
            let poll = polling.entry(s.id.clone()).or_insert(PrPoll {
                last_checked: Instant::now(),
                backoff: PR_POLL_FIRST,
                failing: false,
            });
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
                        // A merge completes a stack, but GitHub only retargets a dependent pull
                        // request when its base branch is *deleted*, and nothing here ever deletes
                        // a branch — without this explicit call the children would point at a
                        // merged-but-undeleted branch indefinitely. Off the tick: the edits each
                        // take up to twenty seconds, and the loop must not sit on them while every
                        // other colony's merge goes unnoticed. A duplicate run is a no-op — a child
                        // that already moved no longer has the merged branch as its base — so two
                        // overlapping runs cost a redundant edit and nothing else.
                        if target == SessionStatus::Merged {
                            tokio::spawn({
                                let app = app.clone();
                                let parent = s.clone();
                                async move { retarget_stacked_children(&app, &parent).await }
                            });
                        }
                    }
                    poll.backoff = pr_backoff(poll.backoff, changed);
                }
                Err(e) => {
                    // No news is no change: leave the colony's status alone and try again later.
                    if !poll.failing {
                        poll.failing = true;
                        app.session_log(&s.id, "warn", format!("checking the pull request failed ({e:#}); will retry"))
                            .await;
                    }
                    poll.backoff = pr_backoff(poll.backoff, false);
                }
            }
        }
    }
}

/// What moving a merged colony's stacked children needs from the world, named so tests can stand in
/// for GitHub, the session record and the log and make any step fail — the pattern
/// `github::PublishOps` uses for the publish itself. Module-private on purpose: only the concrete
/// impl below is ever awaited here, so the spawned retarget's future stays `Send` with no
/// `async-trait` dependency.
trait RetargetOps {
    /// The repository's default branch, for a merged colony that recorded no base of its own.
    async fn default_branch(&self, repo: &str) -> anyhow::Result<String>;
    /// Points one open pull request at a different base branch, on GitHub.
    async fn edit_pr(&self, pr_url: &str, base: &str) -> anyhow::Result<()>;
    /// Records the new base on the colony; `false` when the colony no longer exists.
    async fn record_base(&self, id: &str, base: &str) -> bool;
    /// Says something in a colony's own log.
    async fn say(&self, id: &str, level: &str, message: String);
}

/// The real retarget operations: `gh` on GitHub, the session record, and the colony's own log.
struct GithubRetargetOps<'a> {
    app: &'a App,
}

impl RetargetOps for GithubRetargetOps<'_> {
    async fn default_branch(&self, repo: &str) -> anyhow::Result<String> {
        github::default_branch(self.app, repo).await
    }

    async fn edit_pr(&self, pr_url: &str, base: &str) -> anyhow::Result<()> {
        github::retarget_pr(self.app, pr_url, base).await
    }

    async fn record_base(&self, id: &str, base: &str) -> bool {
        self.app
            .update_session(id, |x| x.base = Some(base.to_string()))
            .await
            .is_some()
    }

    async fn say(&self, id: &str, level: &str, message: String) {
        self.app.session_log(id, level, message).await
    }
}

/// Moves a merged colony's stacked children onto the branch the merged colony was itself based on —
/// GitHub's own rule for a merged base, and not always the repository default: for a stack deeper
/// than two those differ, and the default would fold every lower colony's still-unmerged work into
/// the child's diff. GitHub's own retargeting cannot be relied on — it triggers on base-branch
/// deletion, and nothing here ever deletes a branch. A failure is logged and left for a person: a
/// pull request that could not be moved is not the child's work failing, so the colony is never
/// marked failed.
async fn retarget_stacked_children(app: &Shared, parent: &Session) {
    let ops = GithubRetargetOps { app: app.as_ref() };
    let Some(destination) = retarget_destination(&ops, parent).await else {
        return;
    };
    // Bound to a local first: a read guard held across the edits below would deadlock against
    // update_session's write lock.
    let sessions = app.sessions.read().await.clone();
    let children: Vec<Session> = stack::children_to_retarget(&sessions, parent).into_iter().cloned().collect();
    run_retargets(&ops, &children, &destination).await;
}

/// Where the children go: the branch the merged colony was itself based on, or — when it recorded no
/// base at all, a shape this code never creates — the repository's default branch. `None` when
/// there is nowhere to move them: the children already sit on the destination, or no destination
/// could be found, which is said on the merged colony's own log rather than left silent.
async fn retarget_destination(ops: &impl RetargetOps, parent: &Session) -> Option<String> {
    let destination = match stack::retarget_base(parent) {
        Some(base) => base,
        None => match ops.default_branch(&parent.repo).await {
            Ok(default) => default,
            Err(e) => {
                ops.say(
                    &parent.id,
                    "warn",
                    format!(
                        "no base was recorded for this colony and the default branch to fall back to could not be looked up, so the colonies stacked on it stay where they are: {e:#}"
                    ),
                )
                .await;
                return None;
            }
        },
    };
    (destination != parent.branch).then_some(destination)
}

/// Moves each child still based on the merged branch onto `destination`, saying every outcome in the
/// child's own log. One that could not be moved is said and left for a person — there is no retry,
/// and the colony is never marked failed for its pull request's base.
async fn run_retargets(ops: &impl RetargetOps, children: &[Session], destination: &str) {
    for child in children {
        let Some(url) = child.pr_url.clone() else { continue };
        let id = child.id.clone();
        match ops.edit_pr(&url, destination).await {
            Ok(()) => {
                // `gh pr edit` has already moved the pull request on GitHub, so the recorded base
                // must follow or the record quietly disagrees with the real pull request — and a
                // merged colony is never polled again, so this is the last look anything takes.
                if ops.record_base(&id, destination).await {
                    ops.say(
                        &id,
                        "info",
                        format!("the colony this one is stacked on was merged, so its pull request now targets {destination}"),
                    )
                    .await;
                } else {
                    // The colony was deleted while `gh pr edit` ran: GitHub has the new base and
                    // nothing here does. Said rather than left silent — there is no retry.
                    ops.say(
                        &id,
                        "warn",
                        format!("its pull request was retargeted onto {destination}, but the colony was deleted before the new base could be recorded"),
                    )
                    .await;
                }
            }
            Err(e) => {
                ops.say(
                    &id,
                    "warn",
                    format!("could not retarget its pull request onto {destination}: {e:#}"),
                )
                .await;
            }
        }
    }
}

pub async fn publish(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if !can_publish(s.status, s.cleaned_up, s.git_admin_dir.is_some()) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this session can't be published right now",
        ));
    }
    tokio::spawn(publish_session(app.clone(), id.clone()));
    Ok(Json(s))
}

/// Claims a colony for the background publish: the status flip, the slot decision and the error
/// clear, as one step. A live-origin claim keeps its parallel slot (`Session::holds_slot`) — the
/// teardown inside the publish frees the microVM, but nothing may boot into the half-published
/// worktree. A stopped, failed or no-changes claim boots nothing (host-side push only) and holds
/// nothing, so publishing a stopped colony never takes a slot another colony is waiting for.
/// Returns whether the claim landed, and whether a microVM was live under it.
pub(crate) fn claim_publish(x: &mut Session) -> (bool, bool) {
    let allowed = can_publish(x.status, x.cleaned_up, x.git_admin_dir.is_some());
    // Before the mutation: `publishing` itself is not a live status.
    let was_live = allowed && x.status.is_live();
    if allowed {
        x.status = SessionStatus::Publishing;
        x.publishing_holds_slot = was_live;
        x.error = None;
    }
    (allowed, was_live)
}

/// Whether a colony can publish: it needs its worktree on disk, no publish already in flight, and a
/// state a publish makes sense from. A failed or no-changes publish can be retried directly — the
/// worktree, the committed branch and the remote are all still there, so no new microVM is booted.
///
/// Issue #98: this single gate covers commit, push, and PR creation together today. The split is
/// `commit_allowed` → `push_allowed` → `pr_allowed` below: each later effect needs the earlier one
/// plus its own `authority::Effect` grant (`needs_independent_review` is true for all three), and
/// PR creation additionally needs the candidate bound via `publish_candidate_hash` before it runs.
/// The three helpers delegate to this gate for now, so behavior is unchanged; they exist so each
/// external effect can grow its own grant check without re-deriving the lifecycle preconditions.
pub(crate) fn can_publish(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
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

/// The per-effect split of `can_publish` (issue #98): local commit first, then push, then PR —
/// each ordered check assumes the earlier effects are granted and adds its own. They delegate to
/// the single lifecycle gate for now, so existing callers keep their behavior; the point is that
/// a future per-effect grant check has one named place per effect to live.
pub(crate) fn commit_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::Commit));
    can_publish(status, cleaned_up, has_worktree)
}

pub(crate) fn push_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::Push));
    commit_allowed(status, cleaned_up, has_worktree)
}

pub(crate) fn pr_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::OpenPr));
    push_allowed(status, cleaned_up, has_worktree)
}

/// Binds the evidence a PR grant must name: the hex sha256 (see
/// `authority::bind_candidate`) over the PR body bytes the approval reviewed. A
/// grant authorizes exactly this hash — `authorize` denies any other candidate.
pub(crate) fn publish_candidate_hash(pr_body: &[u8]) -> String {
    crate::authority::bind_candidate(&[pr_body])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;

    #[test]
    fn the_publish_claim_holds_a_slot_only_when_the_colony_was_live() {
        let mut running = colony("acme", SessionStatus::Running);
        running.git_admin_dir = Some("git".into());
        let (claimed, was_live) = claim_publish(&mut running);
        assert!(claimed && was_live, "a live colony is claimed with its microVM under it");
        assert_eq!(running.status, SessionStatus::Publishing);
        assert!(
            running.publishing_holds_slot && running.holds_slot(),
            "a live colony's publish keeps its slot"
        );
        // A second claim finds the publish already in flight.
        assert_eq!(
            claim_publish(&mut running),
            (false, false),
            "publishing itself is not publishable"
        );

        // A stopped, failed or no-changes colony publishes its kept worktree with no new microVM,
        // so its claim holds nothing.
        for status in [SessionStatus::Stopped, SessionStatus::Failed, SessionStatus::NoChanges] {
            let mut s = colony("acme", status);
            s.git_admin_dir = Some("git".into());
            let (claimed, was_live) = claim_publish(&mut s);
            assert!(claimed && !was_live, "{status:?} is claimed with no microVM under it");
            assert_eq!(s.status, SessionStatus::Publishing);
            assert!(
                !s.publishing_holds_slot && !s.holds_slot(),
                "{status:?} never takes a slot another colony is waiting for"
            );
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
        assert_eq!(
            pr_backoff(Duration::from_secs(3600), false),
            Duration::from_secs(3600),
            "capped at an hour"
        );
        assert_eq!(pr_backoff(Duration::from_secs(4000), false), Duration::from_secs(3600));
        // Real news buys a fast next check again (e.g. a closed PR reopened).
        assert_eq!(pr_backoff(Duration::from_secs(3600), true), Duration::from_secs(60));
    }

    #[test]
    fn a_pull_request_is_due_once_its_backoff_has_elapsed() {
        let checked = Instant::now();
        assert!(
            pr_due(checked - Duration::from_secs(60), Duration::from_secs(60), checked),
            "a backoff ago is due"
        );
        assert!(!pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(59)));
        assert!(pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(60)));
        assert!(pr_due(checked, Duration::from_secs(60), checked + Duration::from_secs(3600)));
    }

    #[test]
    fn failed_and_no_changes_colonies_can_publish_again_without_a_new_microvm() {
        assert!(can_publish(SessionStatus::Failed, false, true));
        assert!(can_publish(SessionStatus::NoChanges, false, true));
    }

    #[test]
    fn the_per_effect_split_matches_the_single_gate_and_binds_the_pr_body() {
        // Issue #98: the ordered commit → push → PR checks delegate to `can_publish`
        // today, so they agree everywhere; the split is where per-effect grants attach.
        use SessionStatus::*;
        for status in [Running, WaitingForAnswer, Idle, Stopped, Failed, NoChanges] {
            assert!(commit_allowed(status, false, true), "{status:?}");
            assert!(push_allowed(status, false, true), "{status:?}");
            assert!(pr_allowed(status, false, true), "{status:?}");
        }
        for status in [Queued, Starting, Publishing, PrOpened] {
            assert!(!commit_allowed(status, false, true), "{status:?}");
            assert!(!push_allowed(status, false, true), "{status:?}");
            assert!(!pr_allowed(status, false, true), "{status:?}");
        }
        // The PR grant names exactly the body it reviewed: any other bytes bind elsewhere.
        let bound = publish_candidate_hash(b"the reviewed pr body");
        assert_eq!(bound, crate::authority::bind_candidate(&[b"the reviewed pr body"]));
        assert_ne!(bound, publish_candidate_hash(b"edited after approval"));
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

    // ----- the retarget glue, against a stand-in for GitHub, the record and the log -----

    use std::cell::RefCell;

    /// A stand-in for [`GithubRetargetOps`]: everything asked of it is recorded, and each step can
    /// be made to fail.
    struct FakeRetarget {
        /// What the default-branch lookup answers for a merged colony with no recorded base.
        default_branch: Result<String, String>,
        /// Whether the edit on GitHub succeeds.
        edit_ok: bool,
        /// Whether the colony the new base is recorded on still exists.
        colony_exists: bool,
        /// How many times the default branch was looked up.
        default_asked: std::cell::Cell<usize>,
        /// `(pr_url, base)` of every edit asked of GitHub.
        edits: RefCell<Vec<(String, String)>>,
        /// `(id, base)` of every base recorded.
        recorded: RefCell<Vec<(String, String)>>,
        /// `(id, level, message)` of everything said.
        said: RefCell<Vec<(String, String, String)>>,
    }

    impl FakeRetarget {
        fn merged_parent(&self) -> Session {
            let mut p = crate::sessions::tests::colony("acme", SessionStatus::Merged);
            p.id = "parent".into();
            p.branch = "colonizer/issue-9-parent".into();
            p.base = Some("main".into());
            p
        }

        fn said_contains(&self, fragment: &str) -> bool {
            self.said.borrow().iter().any(|(_, _, m)| m.contains(fragment))
        }
    }

    impl RetargetOps for FakeRetarget {
        async fn default_branch(&self, _repo: &str) -> anyhow::Result<String> {
            self.default_asked.set(self.default_asked.get() + 1);
            self.default_branch.clone().map_err(anyhow::Error::msg)
        }

        async fn edit_pr(&self, pr_url: &str, base: &str) -> anyhow::Result<()> {
            self.edits.borrow_mut().push((pr_url.into(), base.into()));
            if self.edit_ok {
                Ok(())
            } else {
                Err(anyhow::anyhow!("gh pr edit failed"))
            }
        }

        async fn record_base(&self, id: &str, base: &str) -> bool {
            self.recorded.borrow_mut().push((id.into(), base.into()));
            self.colony_exists
        }

        async fn say(&self, id: &str, level: &str, message: String) {
            self.said.borrow_mut().push((id.into(), level.into(), message));
        }
    }

    #[tokio::test]
    async fn the_recorded_base_is_where_the_children_go_and_the_default_is_never_asked() {
        let ops = FakeRetarget {
            default_branch: Ok("develop".into()),
            edit_ok: true,
            colony_exists: true,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let parent = ops.merged_parent();
        assert_eq!(
            retarget_destination(&ops, &parent).await.as_deref(),
            Some("main"),
            "the destination is the branch the merged colony was itself based on"
        );
        assert_eq!(ops.default_asked.get(), 0, "the default branch need not be looked up");

        // A colony already sitting on its own base is nowhere to move to, and asks nothing.
        let mut settled = parent.clone();
        settled.base = Some(settled.branch.clone());
        assert_eq!(retarget_destination(&ops, &settled).await, None);
        assert_eq!(ops.default_asked.get(), 0);
    }

    #[tokio::test]
    async fn with_no_recorded_base_the_children_fall_back_to_the_default_branch() {
        let ops = FakeRetarget {
            default_branch: Ok("develop".into()),
            edit_ok: true,
            colony_exists: true,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let mut parent = ops.merged_parent();
        parent.base = None;
        assert_eq!(
            retarget_destination(&ops, &parent).await.as_deref(),
            Some("develop"),
            "the fallback is the repository's default branch"
        );
        assert_eq!(ops.default_asked.get(), 1, "looked up, since nothing was recorded");
    }

    #[tokio::test]
    async fn no_base_and_no_default_leaves_the_children_saying_why_on_the_merged_colonys_log() {
        let ops = FakeRetarget {
            default_branch: Err("gh api failed".into()),
            edit_ok: true,
            colony_exists: true,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let mut parent = ops.merged_parent();
        parent.base = None;
        assert_eq!(retarget_destination(&ops, &parent).await, None, "nowhere to send them");
        assert_eq!(ops.default_asked.get(), 1);
        assert!(
            ops.said_contains("stay where they are") && ops.said_contains("gh api failed"),
            "the gap is said, not silent: {:?}",
            ops.said.borrow()
        );
        let (_, level, _) = ops.said.borrow()[0].clone();
        assert_eq!(level, "warn", "said on the merged colony's log");
    }

    #[tokio::test]
    async fn a_moved_child_records_its_new_base_and_says_so() {
        let ops = FakeRetarget {
            default_branch: Ok("main".into()),
            edit_ok: true,
            colony_exists: true,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let mut child = crate::sessions::tests::colony("acme", SessionStatus::PrOpened);
        child.id = "child".into();
        child.pr_url = Some("https://github.com/acme/repo/pull/10".into());

        run_retargets(&ops, &[child], "main").await;
        assert_eq!(
            ops.edits.borrow().as_slice(),
            [("https://github.com/acme/repo/pull/10".to_string(), "main".to_string())],
            "GitHub is asked to point the pull request at the destination"
        );
        assert_eq!(
            ops.recorded.borrow().as_slice(),
            [("child".to_string(), "main".to_string())],
            "the recorded base follows the real pull request"
        );
        assert!(ops.said_contains("now targets main"), "the child's log says what happened");
        let (_, level, _) = ops.said.borrow()[0].clone();
        assert_eq!(level, "info");
    }

    #[tokio::test]
    async fn an_edit_that_fails_is_said_and_the_base_is_left_alone() {
        let ops = FakeRetarget {
            default_branch: Ok("main".into()),
            edit_ok: false,
            colony_exists: true,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let mut child = crate::sessions::tests::colony("acme", SessionStatus::PrOpened);
        child.id = "child".into();
        child.pr_url = Some("https://github.com/acme/repo/pull/10".into());

        run_retargets(&ops, &[child], "main").await;
        assert!(
            ops.recorded.borrow().is_empty(),
            "nothing is recorded: GitHub still has the old base"
        );
        assert!(
            ops.said_contains("could not retarget") && ops.said_contains("gh pr edit failed"),
            "the failure is said, for a person: {:?}",
            ops.said.borrow()
        );
        let (_, level, _) = ops.said.borrow()[0].clone();
        assert_eq!(level, "warn");
    }

    #[tokio::test]
    async fn a_child_deleted_before_its_base_was_recorded_says_so() {
        let ops = FakeRetarget {
            default_branch: Ok("main".into()),
            edit_ok: true,
            colony_exists: false,
            default_asked: std::cell::Cell::new(0),
            edits: RefCell::new(Vec::new()),
            recorded: RefCell::new(Vec::new()),
            said: RefCell::new(Vec::new()),
        };
        let mut child = crate::sessions::tests::colony("acme", SessionStatus::PrOpened);
        child.id = "child".into();
        child.pr_url = Some("https://github.com/acme/repo/pull/10".into());

        run_retargets(&ops, &[child], "main").await;
        assert!(
            ops.said_contains("deleted before the new base could be recorded"),
            "GitHub has the new base and nothing here does; that is said: {:?}",
            ops.said.borrow()
        );
        let (_, level, _) = ops.said.borrow()[0].clone();
        assert_eq!(level, "warn");
    }
}
