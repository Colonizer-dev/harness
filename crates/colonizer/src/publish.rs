//! Publishing a colony's work: the Create PR button and the background job it claims a session
//! with, which stops the agent, removes the microVM, and hands the worktree to `github::publish`.
//!
//! The pull-request watcher lives here too: once a colony's work is published, what happens to
//! that pull request is the last thing the colony's badge still reflects.

use crate::{
    ApiResult, App, Shared, client_error, github, stack,
    util::{short_id, truncate},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde_json::json;
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
    // Issue #84: the operator's kill-switch refuses before the claim too, so nothing is torn down.
    if crate::authority::external_writes_blocked() {
        let message = BLOCKED.to_string();
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

/// Which merge time a colony flipping to `merged` keeps: the one it already has, else GitHub's
/// `mergedAt`, else the moment the flip was seen. A merge observed twice keeps the first time.
pub(crate) fn resolve_merged_at(
    existing: Option<DateTime<Utc>>,
    from_github: Option<DateTime<Utc>>,
    flip_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    existing.or(from_github).or(Some(flip_at))
}

/// Whether the startup backfill still wants this colony: merged, with a pull request to ask about,
/// and no merge time yet.
fn wants_merged_at(s: &Session) -> bool {
    s.status == SessionStatus::Merged && s.merged_at.is_none() && s.pr_url.is_some()
}

/// The colonies the startup backfill asks GitHub about, as `(id, pr_url)`.
pub(crate) fn merged_at_backfill_targets(sessions: &[Session]) -> Vec<(String, String)> {
    sessions
        .iter()
        .filter(|s| wants_merged_at(s))
        .filter_map(|s| s.pr_url.clone().map(|url| (s.id.clone(), url)))
        .collect()
}

/// Stamps a backfilled merge time; true when it did. Never touches `updated_at` — the backfill
/// persists through `record_measurement`, so old colonies keep their place in every list.
pub(crate) fn apply_merged_at(s: &mut Session, merged_at: DateTime<Utc>) -> bool {
    if !wants_merged_at(s) {
        return false;
    }
    s.merged_at = Some(merged_at);
    true
}

/// One-off backfill for `merged_at`: colonies already merged before the field existed gain GitHub's
/// `mergedAt` where it still reports one. Best effort — a `gh` failure or a missing timestamp skips
/// the colony, which keeps `None` and reads as before. Runs once at startup, off the serving path.
pub async fn backfill_merged_at(app: Shared) {
    let targets = merged_at_backfill_targets(&app.sessions.read().await);
    let mut filled = 0usize;
    for (id, pr_url) in targets {
        let merged_at = match github::pr_info(&app, &pr_url).await {
            Ok(info) => match info.merged_at {
                Some(at) => at,
                None => continue,
            },
            Err(_) => continue,
        };
        // Re-checked at apply time: the colony may have changed while `gh` was running.
        if !app.session(&id).await.is_some_and(|s| wants_merged_at(&s)) {
            continue;
        }
        // A measurement, not activity: `record_measurement` leaves `updated_at` alone.
        app.record_measurement(&id, |s| {
            apply_merged_at(s, merged_at);
        })
        .await;
        filled += 1;
    }
    if filled > 0 {
        println!("sessions: backfilled merged_at for {filled} merged colonies");
    }
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
pub(crate) struct PrPoll {
    last_checked: Instant,
    backoff: Duration,
    /// Set while `gh` keeps failing, so the reason is logged once per streak, not once per attempt.
    failing: bool,
    /// The last mergeability worth saying anything about: only non-`Unknown` readings are kept, so
    /// an `UNKNOWN` between two identical readings never re-logs.
    mergeability: Option<github::Mergeability>,
    /// The main sha the last auto-rebase acted on: the once-per-sha guard, so a main that has not
    /// moved never re-triggers.
    last_rebase_sha: Option<String>,
    /// When the watcher may try to auto-rebase again after a failure; `None` means now.
    rebase_backoff_until: Option<Instant>,
    /// The main sha the failure behind `rebase_backoff_until` was recorded against, so
    /// [`crate::rebase::rebase_due`] can tell a still-stuck main from one that has since moved on.
    rebase_failed_base: Option<String>,
}

/// How long a failed auto-rebase waits before the watcher tries again.
pub(crate) const REBASE_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// What one auto-rebase attempt did. The watch loop records the guard (`Rebased`) and the backoff
/// (`Failed`, `Conflicted`) on its in-memory poll; the session's `needs_rebase` flag is updated here.
pub(crate) enum RebaseOutcome {
    /// Nothing to rebase: the colony is gone, merged/closed, or has no worktree left.
    Gone,
    /// Already rebased onto this main: the once-per-sha guard held.
    Skipped,
    /// The rebase landed and pushed; carries the main sha for the guard.
    Rebased(String),
    /// Conflicts: the rebase was aborted and the colony was woken to resolve them.
    Conflicted,
    /// Anything else (fetch/rebase/push failed, gates dirty, writes blocked); back off.
    Failed,
}

/// Runs `git` in a worktree with a deadline: trimmed stdout, or the reason it failed.
async fn git_in(worktree: &std::path::Path, args: &[&str], secs: u64) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(worktree).args(args);
    Ok(crate::util::exec_within(Duration::from_secs(secs), &mut cmd)
        .await?
        .trim()
        .to_string())
}

/// Rebases a behind/conflicted pull request's branch onto its base (issue #453): fetch, guard
/// once per main sha, rebase, push with a lease on the pre-rebase head. On conflict the rebase is
/// aborted and the colony is woken with the conflicted files; when no colony is running to hear
/// it, the session flag says a person should look. Best effort with timeouts throughout — never
/// panics, and every outcome is said in the colony's own log.
pub(crate) async fn attempt_auto_rebase(
    app: &Shared,
    session_id: &str,
    mergeability: github::Mergeability,
    last_rebased: Option<String>,
) -> RebaseOutcome {
    let Some(s) = app.session(session_id).await else {
        return RebaseOutcome::Gone;
    };
    if matches!(s.status, SessionStatus::Merged | SessionStatus::Closed)
        || s.git_admin_dir.is_none()
        || !std::path::Path::new(&s.worktree).is_dir()
    {
        // Nothing to rebase onto what is gone or finished; the flags say a person should look, and
        // that nothing is left running that will ever clear them itself.
        app.update_session(session_id, |x| {
            x.needs_rebase = true;
            x.rebase_orphaned = true;
        })
        .await;
        return RebaseOutcome::Gone;
    }
    // Issue #84: a rebase plus a push writes, so the kill-switch refuses before git runs.
    if crate::authority::external_writes_blocked() {
        app.session_log(
            session_id,
            "warn",
            "its pull request fell behind its base, but external writes are blocked, so it was left alone; \
             rebase it by hand once writes are allowed"
                .to_string(),
        )
        .await;
        app.update_session(session_id, |x| x.needs_rebase = true).await;
        return RebaseOutcome::Failed;
    }
    let worktree = std::path::PathBuf::from(&s.worktree);
    let base = s.base.clone().unwrap_or_else(|| "main".to_string());
    let origin_base = format!("origin/{base}");
    if let Err(e) = git_in(&worktree, &["fetch", "origin"], 30).await {
        app.session_log(
            session_id,
            "warn",
            format!("auto-rebase: could not fetch origin ({e:#}); will retry"),
        )
        .await;
        return RebaseOutcome::Failed;
    }
    let main_sha = match git_in(&worktree, &["rev-parse", &origin_base], 10).await {
        Ok(sha) => sha,
        Err(e) => {
            app.session_log(
                session_id,
                "warn",
                format!("auto-rebase: could not read {origin_base} ({e:#}); will retry"),
            )
            .await;
            return RebaseOutcome::Failed;
        }
    };
    // The decision, with the sha the guard needs: behind/conflicted onto a main not tried before.
    if !crate::rebase::should_auto_rebase(mergeability, Some(&main_sha), last_rebased.as_deref()) {
        return RebaseOutcome::Skipped;
    }
    // Said before touching anything: files both sides changed are the ones a rebase trips on.
    let main_range = format!("HEAD..{origin_base}");
    let overlap = crate::rebase::files_overlap(
        &crate::rebase::touched_files(&worktree, &origin_base).await,
        &crate::rebase::diff_names(&worktree, &["diff", "--name-only", &main_range]).await,
    );
    if !overlap.is_empty() {
        app.session_log(
            session_id,
            "info",
            format!(
                "auto-rebase: its base also touched {} file(s) this branch changed: {}",
                overlap.len(),
                overlap.join(", ")
            ),
        )
        .await;
    }
    let old_head = git_in(&worktree, &["rev-parse", "HEAD"], 10).await.unwrap_or_default();
    if git_in(&worktree, &["rebase", &origin_base], 90).await.is_err() {
        // Read the conflicted files before the abort clears them, then wake the colony with them.
        let conflicted = crate::rebase::unmerged_files(&worktree).await;
        abort_rebase(app, session_id, &worktree).await;
        let text = crate::rebase::conflict_wake_text(&conflicted, &main_sha, &s.branch);
        // Issue #453: presence in the runtimes map outlives the colony (`teardown_vm` never removes
        // its entry), so it cannot tell a live colony from one already torn down. The session's own
        // status is the liveness truth; the sender being closed is the same fact seen from the other
        // side, checked too since a status flip and a channel close are not the same write.
        let is_live = app.session(session_id).await.is_some_and(|cur| cur.status.is_live());
        let live_runtime = if is_live {
            app.runtimes
                .lock()
                .await
                .get(session_id)
                .cloned()
                .filter(|rt| !rt.commands.is_closed())
        } else {
            None
        };
        let rebase_orphaned = if let Some(rt) = live_runtime {
            rt.send_command(json!({"type": "user_message", "id": format!("rebase-{}", short_id()), "text": text}));
            app.session_log(
                session_id,
                "warn",
                "auto-rebase hit conflicts and woke the colony to resolve them".to_string(),
            )
            .await;
            false
        } else {
            // Nothing is left running that will ever clear `needs_rebase` itself: a person has to,
            // so this is filed as a notification rather than only the session flag.
            app.session_log(
                session_id,
                "warn",
                format!("auto-rebase hit conflicts and no colony is running to resolve them: {text}"),
            )
            .await;
            true
        };
        app.update_session(session_id, |x| {
            x.needs_rebase = true;
            x.rebase_orphaned = rebase_orphaned;
        })
        .await;
        return RebaseOutcome::Conflicted;
    }
    // The tree must be clean after the rebase: anything still unmerged is a gate failure, not a push.
    if !crate::rebase::unmerged_files(&worktree).await.is_empty() {
        abort_rebase(app, session_id, &worktree).await;
        app.session_log(
            session_id,
            "warn",
            "auto-rebase left unmerged files behind, so it was aborted; will retry".to_string(),
        )
        .await;
        return RebaseOutcome::Failed;
    }
    // Issue #453: re-run the repo's own gates before force-pushing a rebase nothing reviewed — a
    // rebase that applies cleanly can still break the build (a conflicting semantic change neither
    // side's diff alone would show). The rebase already completed at this point (no rebase is "in
    // progress" to abort), so a gate failure resets the branch back to its pre-rebase head instead,
    // leaving the worktree exactly as it was rather than half-rebased and unpushed.
    if let Err(reason) = crate::rebase::run_gates(&worktree).await {
        if !old_head.is_empty()
            && let Err(e) = git_in(&worktree, &["reset", "--hard", &old_head], 30).await
        {
            app.session_log(
                session_id,
                "error",
                format!("auto-rebase: resetting back to {old_head} after a gate failure itself failed ({e:#}); the worktree may be left rebased but unpushed"),
            )
            .await;
        }
        app.session_log(
            session_id,
            "warn",
            format!("auto-rebase: gates failed after rebasing onto {main_sha}, so it was not pushed: {reason}"),
        )
        .await;
        return RebaseOutcome::Failed;
    }
    let lease = if old_head.is_empty() {
        "--force-with-lease".to_string()
    } else {
        format!("--force-with-lease={}:{}", s.branch, old_head)
    };
    match git_in(&worktree, &["push", &lease, "origin", &s.branch], 60).await {
        Ok(_) => {
            app.session_log(session_id, "info", format!("auto-rebased onto {main_sha} and pushed"))
                .await;
            app.update_session(session_id, |x| {
                x.needs_rebase = false;
                x.rebase_orphaned = false;
            })
            .await;
            RebaseOutcome::Rebased(main_sha)
        }
        Err(e) => {
            app.session_log(
                session_id,
                "warn",
                format!("auto-rebase: the push was refused ({e:#}); will retry"),
            )
            .await;
            RebaseOutcome::Failed
        }
    }
}

/// Aborts an in-progress rebase, best effort: a failed abort is a worktree stuck mid-rebase, which
/// would make every future attempt here fail confusingly (an unrelated "already rebasing" git error)
/// until someone notices — worth its own distinguishable log line rather than folding into the
/// conflict or gate-failure message that triggered it.
async fn abort_rebase(app: &Shared, session_id: &str, worktree: &std::path::Path) {
    if let Err(e) = git_in(worktree, &["rebase", "--abort"], 30).await {
        app.session_log(
            session_id,
            "error",
            format!("auto-rebase: `git rebase --abort` itself failed ({e:#}); the worktree may be stuck mid-rebase"),
        )
        .await;
    }
}

/// What the watcher says when a still-open pull request's mergeability moves, if anything: `Behind`
/// and `Conflicted` each log once on arrival with what to do about them, `Clean` logs once on the
/// way back, and `Unknown` is no news — it neither logs nor clears what was last seen, so it can
/// never flip-flop the log. The `Unknown` arm is why this takes the previous reading rather than
/// deciding from the current one alone.
pub(crate) fn mergeability_message(
    previous: Option<github::Mergeability>,
    current: github::Mergeability,
    pr_url: &str,
    base: Option<&str>,
) -> Option<(&'static str, String)> {
    use github::Mergeability::*;
    if current == Unknown || Some(current) == previous {
        return None;
    }
    // The git command names the branch, so it is only suggested when the base is known; the
    // cockpit action is always available.
    let branch = base.unwrap_or("its base branch");
    match current {
        Behind => {
            let how = match base {
                Some(known) => format!(
                    "catch it up from the cockpit (Catch up action) or run `git merge origin/{known}` in the colony's worktree"
                ),
                None => "catch it up from the cockpit (Catch up action)".to_string(),
            };
            Some(("warn", format!("{pr_url} is behind {branch}: {how}")))
        }
        Conflicted => {
            let how = match base {
                Some(known) => {
                    format!("resolve by merging `origin/{known}` into the branch and fixing the conflicts (no force pushes)")
                }
                None => {
                    "resolve by merging the base branch into the branch and fixing the conflicts (no force pushes)".to_string()
                }
            };
            Some((
                "warn",
                format!("{pr_url} conflicts with {branch}: {how} — GitHub does not list the conflicted files"),
            ))
        }
        Clean if matches!(previous, Some(Behind) | Some(Conflicted)) => {
            Some(("info", format!("{pr_url} can merge cleanly again")))
        }
        Clean | Unknown => None,
    }
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
                mergeability: None,
                last_rebase_sha: None,
                rebase_backoff_until: None,
                rebase_failed_base: None,
            });
            let now = Instant::now();
            if !pr_due(poll.last_checked, poll.backoff, now) {
                continue;
            }
            poll.last_checked = now;
            match github::pr_info(&app, &url).await {
                Ok(info) => {
                    let (state, mergeability, pr_merged_at, base_ref_oid) =
                        (info.state, info.mergeability, info.merged_at, info.base_ref_oid);
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
                        let flip_at = Utc::now();
                        changed = app
                            .update_session(&s.id, |x| {
                                // Re-checked under the write lock: the colony may have been deleted or
                                // moved on while `gh` was running.
                                let apply = pr_watched(x.status, x.pr_url.is_some()) && x.status != target;
                                if apply {
                                    x.status = target;
                                    if target == SessionStatus::Merged {
                                        x.merged_at = resolve_merged_at(x.merged_at, pr_merged_at, flip_at);
                                    }
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
                    // While the pull request is still open, a move into behind/conflicted — or back
                    // to clean — is said once in the colony's own log. A behind or conflicted PR
                    // stays `PrOpened`, so it keeps being watched and re-checked on backoff until it
                    // merges, closes, or moves again.
                    let mut merge_changed = false;
                    if state == github::PrState::Open
                        && let Some((level, message)) =
                            mergeability_message(poll.mergeability, mergeability, &url, s.base.as_deref())
                    {
                        app.session_log(&s.id, level, message).await;
                        merge_changed = true;
                    }
                    // Only an open PR's reading is remembered: a closed PR's last reading must not
                    // suppress the log when it reopens behind or conflicted.
                    if state == github::PrState::Open && mergeability != github::Mergeability::Unknown {
                        poll.mergeability = Some(mergeability);
                    }
                    // Issue #453: a still-open PR that fell behind or conflicts is rebased onto its
                    // base automatically — once per main sha, and with a backoff after failures that
                    // holds only while main sha stays the failure was recorded against (a moved main
                    // is news worth trying again for immediately). The git work runs inline like the
                    // `gh` call above, with short timeouts throughout.
                    if state == github::PrState::Open
                        && matches!(mergeability, github::Mergeability::Behind | github::Mergeability::Conflicted)
                        && crate::rebase::rebase_due(
                            poll.rebase_failed_base.as_deref(),
                            poll.rebase_backoff_until,
                            base_ref_oid.as_deref().unwrap_or(""),
                            now,
                        )
                    {
                        match attempt_auto_rebase(&app, &s.id, mergeability, poll.last_rebase_sha.clone()).await {
                            RebaseOutcome::Rebased(sha) => {
                                crate::rebase::record_rebase_attempt(&mut poll.last_rebase_sha, &sha);
                                poll.rebase_backoff_until = None;
                                poll.rebase_failed_base = None;
                                // Something actually changed: re-check soon to confirm the PR reads clean.
                                merge_changed = true;
                            }
                            RebaseOutcome::Failed | RebaseOutcome::Conflicted => {
                                poll.rebase_backoff_until = Some(now + REBASE_BACKOFF);
                                poll.rebase_failed_base = base_ref_oid.clone();
                            }
                            RebaseOutcome::Gone | RebaseOutcome::Skipped => {}
                        }
                    }
                    // A clean reading clears the flags a finished rebase — or a hand rebase — left
                    // behind, orphaned or not: a person catching it up by hand clears it just as well.
                    if state == github::PrState::Open && mergeability == github::Mergeability::Clean && s.needs_rebase {
                        app.update_session(&s.id, |x| {
                            x.needs_rebase = false;
                            x.rebase_orphaned = false;
                        })
                        .await;
                    }
                    poll.backoff = pr_backoff(poll.backoff, merge_changed || changed);
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
        // Issue #84: fail closed before GitHub is asked anything; `github::retarget_pr` checks again.
        let edited = if crate::authority::external_writes_blocked() {
            Err(anyhow::anyhow!("refusing to retarget {url}: {RETARGET_BLOCKED}"))
        } else {
            ops.edit_pr(&url, destination).await
        };
        match edited {
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
    if crate::authority::external_writes_blocked() {
        return Err(client_error(StatusCode::CONFLICT, BLOCKED));
    }
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
    // `pr_allowed` implies the commit and push gates, and fails closed on the kill-switch (#84).
    let allowed = pr_allowed(x.status, x.cleaned_up, x.git_admin_dir.is_some());
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
/// The three helpers delegate to this gate plus the #84 kill-switch (`claim_publish` goes through
/// `pr_allowed`); they exist so each external effect can grow its own grant check without
/// re-deriving the lifecycle preconditions.
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

/// Why a publish is refused while the operator's kill-switch is on (issue #84).
pub(crate) const BLOCKED: &str =
    "external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS / COLONIZER_NO_WRITE); unset it to publish";

/// Why a stacked child's pull request keeps its old base while the kill-switch is on (issue #84).
/// Nothing retries a retarget, so a person moves it once writes are allowed again.
pub(crate) const RETARGET_BLOCKED: &str = "external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS / \
     COLONIZER_NO_WRITE), so the pull request keeps its old base; retarget it by hand once they are allowed";

/// The per-effect split of `can_publish` (issue #98): local commit first, then push, then PR —
/// each ordered check assumes the earlier effects are granted and adds its own. Each fails closed
/// while external writes are blocked (issue #84); otherwise they delegate to the single lifecycle
/// gate, and a future per-effect grant check has one named place per effect to live.
pub(crate) fn commit_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::Commit));
    !crate::authority::external_writes_blocked() && can_publish(status, cleaned_up, has_worktree)
}

pub(crate) fn push_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::Push));
    !crate::authority::external_writes_blocked() && commit_allowed(status, cleaned_up, has_worktree)
}

pub(crate) fn pr_allowed(status: SessionStatus, cleaned_up: bool, has_worktree: bool) -> bool {
    debug_assert!(crate::authority::needs_independent_review(&crate::authority::Effect::OpenPr));
    !crate::authority::external_writes_blocked() && push_allowed(status, cleaned_up, has_worktree)
}

/// Binds the evidence a PR grant must name: the hex sha256 (see
/// `authority::bind_candidate`) over the PR body bytes the approval reviewed. A
/// grant authorizes exactly this hash — `authorize` denies any other candidate. Logged with every
/// pull request opened, so the audit trail names the exact body that went out.
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
    fn rebase_backoff_is_ten_minutes_and_the_poll_tracks_the_sha_it_failed_against() {
        // The decision itself (`crate::rebase::rebase_due`) is exhaustively unit-tested in
        // rebase.rs; this pins what the watch loop feeds it from a `PrPoll` (Fix 4: the once-a-sha
        // backoff, not a pure time one).
        fn poll(until: Option<Instant>, failed_base: Option<&str>) -> PrPoll {
            PrPoll {
                last_checked: Instant::now(),
                backoff: PR_POLL_FIRST,
                failing: false,
                mergeability: None,
                last_rebase_sha: None,
                rebase_backoff_until: until,
                rebase_failed_base: failed_base.map(String::from),
            }
        }
        assert_eq!(REBASE_BACKOFF, Duration::from_secs(600));
        let now = Instant::now();
        let until = now + Duration::from_secs(60);
        let due = |p: &PrPoll, current_base: &str| {
            crate::rebase::rebase_due(p.rebase_failed_base.as_deref(), p.rebase_backoff_until, current_base, now)
        };
        assert!(due(&poll(None, None), "abc"), "no failure yet: due");
        assert!(
            !due(&poll(Some(until), Some("abc")), "abc"),
            "a live backoff against the same sha holds"
        );
        assert!(
            due(&poll(Some(until), Some("abc")), "def"),
            "main moved on while backing off: due again"
        );
    }

    #[test]
    fn a_dirty_reading_wires_into_the_rebase_decision_and_its_guard() {
        // `gh` reports a conflicted PR as DIRTY; the watcher must read that as a rebase candidate.
        let conflicted = github::mergeability_from(Some("MERGEABLE"), Some("DIRTY"));
        assert_eq!(conflicted, github::Mergeability::Conflicted);
        assert!(crate::rebase::should_auto_rebase(conflicted, Some("abc"), None));
        // ... and the guard the loop records must hold the retry until main moves.
        let mut guard = None;
        crate::rebase::record_rebase_attempt(&mut guard, "abc");
        assert!(!crate::rebase::should_auto_rebase(conflicted, Some("abc"), guard.as_deref()));
        assert!(crate::rebase::should_auto_rebase(conflicted, Some("def"), guard.as_deref()));
        // Behind wires in the same way; clean and unknown never do.
        assert!(crate::rebase::should_auto_rebase(
            github::Mergeability::Behind,
            Some("abc"),
            None
        ));
        assert!(!crate::rebase::should_auto_rebase(
            github::Mergeability::Clean,
            Some("abc"),
            None
        ));
        assert!(!crate::rebase::should_auto_rebase(
            github::Mergeability::Unknown,
            Some("abc"),
            None
        ));
    }

    #[test]
    fn the_merge_time_is_githubs_first_the_flips_second_and_never_rewritten() {
        let github_time = Utc::now() - chrono::Duration::hours(2);
        let flip_at = Utc::now();
        // GitHub's time wins over the moment the flip was seen.
        assert_eq!(resolve_merged_at(None, Some(github_time), flip_at), Some(github_time));
        // Without one from GitHub, the flip time is the merge time.
        assert_eq!(resolve_merged_at(None, None, flip_at), Some(flip_at));
        // A merge observed twice keeps the first time, whatever GitHub says now.
        assert_eq!(
            resolve_merged_at(Some(github_time), Some(flip_at), flip_at),
            Some(github_time)
        );
        assert_eq!(resolve_merged_at(Some(github_time), None, flip_at), Some(github_time));
    }

    #[test]
    fn only_merged_colonies_missing_a_merge_time_are_backfill_candidates() {
        let mut merged = colony("acme", SessionStatus::Merged);
        merged.pr_url = Some("https://github.com/acme/repo/pull/7".into());
        let mut stamped = merged.clone();
        stamped.merged_at = Some(Utc::now());
        let mut no_pr = colony("acme", SessionStatus::Merged);
        no_pr.merged_at = None;
        let mut open = colony("acme", SessionStatus::PrOpened);
        open.pr_url = Some("https://github.com/acme/repo/pull/8".into());
        let sessions = vec![merged.clone(), stamped, no_pr, open];
        assert_eq!(
            merged_at_backfill_targets(&sessions),
            vec![(merged.id.clone(), merged.pr_url.clone().unwrap())],
            "only the merged colony with a PR and no merge time is asked about"
        );
    }

    #[test]
    fn the_backfill_fill_stamps_merged_at_and_leaves_updated_at_alone() {
        let mut s = colony("acme", SessionStatus::Merged);
        s.pr_url = Some("https://github.com/acme/repo/pull/7".into());
        let before = s.updated_at;
        let merged_at = Utc::now();
        assert!(apply_merged_at(&mut s, merged_at), "a candidate is stamped");
        assert_eq!(s.merged_at, Some(merged_at));
        assert_eq!(
            s.updated_at, before,
            "a backfill is not activity: old colonies keep their list order"
        );
        // Anything but a candidate is left alone.
        for status in [SessionStatus::PrOpened, SessionStatus::Closed, SessionStatus::Stopped] {
            let mut other = colony("acme", status);
            other.pr_url = Some("https://github.com/acme/repo/pull/9".into());
            assert!(!apply_merged_at(&mut other, merged_at), "{status:?} is not a candidate");
            assert_eq!(other.merged_at, None);
        }
        let mut stamped = s.clone();
        assert!(
            !apply_merged_at(&mut stamped, Utc::now()),
            "an existing merge time is never rewritten"
        );
        assert_eq!(stamped.merged_at, Some(merged_at));
    }

    #[test]
    fn mergeability_moves_are_said_once_per_transition_and_unknown_is_no_news() {
        use github::Mergeability::*;
        let url = "https://github.com/acme/repo/pull/7";
        // An open PR falling behind logs once, with where to catch up.
        let said = mergeability_message(None, Behind, url, Some("main"));
        let (level, text) = said.expect("arriving at behind is said");
        assert_eq!(level, "warn");
        assert!(text.contains(url), "{text}");
        assert!(text.contains("Catch up"), "{text}");
        assert!(text.contains("origin/main"), "{text}");
        // The same reading on the next poll says nothing.
        assert_eq!(mergeability_message(Some(Behind), Behind, url, Some("main")), None);
        // Back to clean says so once, then stays quiet.
        let (level, text) = mergeability_message(Some(Behind), Clean, url, Some("main")).expect("the recovery is said");
        assert_eq!(level, "info");
        assert!(text.contains(url), "{text}");
        assert_eq!(mergeability_message(Some(Clean), Clean, url, Some("main")), None);
        assert_eq!(mergeability_message(None, Clean, url, Some("main")), None);
        // Unknown never logs and never clears what was last seen: the caller keeps the old
        // reading, so the next real one still compares against it.
        assert_eq!(mergeability_message(Some(Behind), Unknown, url, Some("main")), None);
        assert_eq!(mergeability_message(None, Unknown, url, Some("main")), None);
        assert_eq!(mergeability_message(Some(Clean), Unknown, url, Some("main")), None);
    }

    #[test]
    fn mergeability_hints_without_a_recorded_base_name_no_branch() {
        use github::Mergeability::*;
        let url = "https://github.com/acme/repo/pull/7";
        // With no recorded base there is no branch to name, so the git command is dropped and the
        // cockpit action stands alone.
        let (level, text) = mergeability_message(None, Behind, url, None).expect("behind with no base is still said");
        assert_eq!(level, "warn");
        assert!(text.contains("Catch up"), "{text}");
        assert!(!text.contains("origin/"), "{text}");
        let (level, text) = mergeability_message(None, Conflicted, url, None).expect("conflicted with no base is still said");
        assert_eq!(level, "warn");
        assert!(text.contains("no force pushes"), "{text}");
        assert!(!text.contains("origin/"), "{text}");
    }

    #[test]
    fn a_conflicted_pull_request_stays_flagged_without_a_duplicate_line() {
        use github::Mergeability::*;
        let url = "https://github.com/acme/repo/pull/7";
        // The first conflicted reading logs, with how to resolve it and no force pushes.
        let (level, text) = mergeability_message(None, Conflicted, url, Some("main")).expect("arriving at conflicted is said");
        assert_eq!(level, "warn");
        assert!(text.contains(url), "{text}");
        assert!(text.contains("origin/main"), "{text}");
        assert!(text.contains("no force pushes"), "{text}");
        // Two more polls with nothing new: still flagged in the caller's stored reading, but silent.
        assert_eq!(mergeability_message(Some(Conflicted), Conflicted, url, Some("main")), None);
        assert_eq!(mergeability_message(Some(Conflicted), Conflicted, url, Some("main")), None);
        // Resolved: one info line, naming the PR.
        let (level, text) = mergeability_message(Some(Conflicted), Clean, url, Some("main")).expect("the recovery is said");
        assert_eq!(level, "info");
        assert!(text.contains(url), "{text}");
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

        // Issue #84: with external writes blocked, every effect fails closed — and so does the claim.
        let _blocked = crate::authority::test_block_external_writes();
        for status in [Running, WaitingForAnswer, Idle, Stopped, Failed, NoChanges] {
            assert!(!commit_allowed(status, false, true), "{status:?}");
            assert!(!push_allowed(status, false, true), "{status:?}");
            assert!(!pr_allowed(status, false, true), "{status:?}");
            let mut s = colony("acme", status);
            s.git_admin_dir = Some("git".into());
            assert_eq!(claim_publish(&mut s), (false, false), "{status:?}");
            assert_eq!(s.status, status, "a refused claim leaves the colony as it was");
        }
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

    /// Issue #84: with external writes blocked, no pull request is edited on GitHub, nothing is
    /// recorded, and the child's log says why.
    #[tokio::test]
    async fn blocked_external_writes_leave_the_child_on_its_old_base_and_say_why() {
        let _blocked = crate::authority::test_block_external_writes();
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
        assert!(ops.edits.borrow().is_empty(), "GitHub must not be asked to edit anything");
        assert!(ops.recorded.borrow().is_empty(), "the old base stays recorded");
        assert!(
            ops.said_contains("could not retarget") && ops.said_contains("COLONIZER_NO_EXTERNAL_EFFECTS"),
            "the refusal is said, for a person: {:?}",
            ops.said.borrow()
        );
        let (id, level, _) = ops.said.borrow()[0].clone();
        assert_eq!((id.as_str(), level.as_str()), ("child", "warn"));
    }
}
