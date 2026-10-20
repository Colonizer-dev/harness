//! Stacked dependents whose parent is paused or gone (issue #1140).
//!
//! A colony stacked on another builds on that colony's branch, so it waits for the parent. What the
//! wait looks like depends on the parent. A parent that is stopped or parked is *paused*: it may be
//! resumed and publish yet, so its dependents go to `blocked` — no slot, no microVM, not failed —
//! and go back to the queue the moment it runs again. A parent that is gone for good (cleaned up
//! before it published, a closed pull request with no issue, no changes without an issue, deleted)
//! can never lend a branch, and stacking only exists to avoid conflicts, so the dependent re-bases
//! on the default branch and queues like any other colony. However, a parent that ended with no
//! changes or was closed unmerged *but has an open GitHub issue* is not gone for good — the work
//! may be tried again — so its dependent stays blocked until the issue closes. A cascade never
//! produces `failed`.
//!
//! The decision is a pure function over the session records ([`parent_state`]); the queue applies it
//! to a queued colony (`queue::gate`) and [`release_blocked`] applies it to a blocked one each tick.
//! For unmet parents, [`release_blocked`] checks the issue state each tick and releases the child
//! when the issue closes, or finds a newer relaunch session and re-points the child to it.

use crate::{
    Shared,
    sessions::{Session, SessionStatus},
};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tracing::warn;

/// What the colony a dependent is stacked on means for it right now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParentState {
    /// Nothing here holds the dependent back; the queue's ordinary rules decide.
    Fine,
    /// The parent is paused (or itself blocked): wait without a slot. The text says on what.
    Paused(String),
    /// The parent has no changes or was closed unmerged, but has an open GitHub issue: the work
    /// may be retried, so wait and check the issue state.
    Unmet { reason: String, repo: String, issue: u64 },
    /// The parent is gone for good: re-base on the default branch. The text says why.
    Gone(String),
}

/// How a person names a colony: its issue number when it has one, else a short id.
fn label(parent: &Session) -> String {
    match parent.issue {
        Some(issue) => format!("#{issue}"),
        None => format!("colony {}", short(&parent.id)),
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Cached issue state: (repo, issue) -> (is_open, checked_at). TTL is 5 minutes.
const ISSUE_CHECK_TTL: Duration = Duration::from_secs(5 * 60);

type IssueStateCache = Mutex<HashMap<(String, u64), (bool, Instant)>>;

fn issue_state_cache() -> &'static IssueStateCache {
    static CACHE: std::sync::OnceLock<IssueStateCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parses the issue state from a GitHub issue JSON response.
/// Returns Some(true) if the issue is open, Some(false) if closed.
/// Returns None if the state field is missing or unrecognized, with a warning logged.
fn issue_open(issue_json: &serde_json::Value) -> Option<bool> {
    let state = issue_json.get("state").and_then(|s| s.as_str());
    match state {
        Some(s) => {
            let state_upper = s.trim().to_ascii_uppercase();
            match state_upper.as_str() {
                "OPEN" => Some(true),
                "CLOSED" => Some(false),
                _ => {
                    warn!(state = %s, "unrecognized issue state");
                    None
                }
            }
        }
        None => {
            warn!("issue state field missing or not a string");
            None
        }
    }
}

/// Checks if an issue is currently open, with cached results. Returns `None` on fetch error or
/// unrecognized state. A missing result is fetched via `gh issue view`; errors are logged but stay
/// cached for the TTL. An unrecognized state is treated as unknown (returns None) to prevent
/// releasing a dependent by mistake.
async fn is_issue_open(app: &Shared, repo: &str, issue: u64) -> Option<bool> {
    let cache_key = (repo.to_string(), issue);
    let now = Instant::now();

    // Check cache first.
    if let Ok(cache) = issue_state_cache().lock()
        && let Some((is_open, checked_at)) = cache.get(&cache_key)
        && now.duration_since(*checked_at) < ISSUE_CHECK_TTL
    {
        return Some(*is_open);
    }

    // Fetch fresh state from GitHub.
    match crate::github::fetch_issue(app, repo, issue).await {
        Ok(issue_json) => {
            let is_open = issue_open(&issue_json);

            // Cache the result only if we successfully parsed the state.
            if let Some(open) = is_open
                && let Ok(mut cache) = issue_state_cache().lock()
            {
                cache.insert(cache_key.clone(), (open, now));
            }

            is_open
        }
        Err(e) => {
            warn!(repo = %repo, issue = %issue, error = %e, "failed to check issue state");
            None
        }
    }
}

/// Finds the newest session with the same repo and issue as the parent, created after the parent,
/// in a live or merged state. Used to detect if the work was relaunched.
/// Excludes sessions whose parent chain reaches back to the parent (to prevent cycles).
fn find_relaunch(parent: &Session, sessions: &[Session]) -> Option<Session> {
    let live_or_merged_states = [
        SessionStatus::Queued,
        SessionStatus::Blocked,
        SessionStatus::Starting,
        SessionStatus::Running,
        SessionStatus::WaitingForAnswer,
        SessionStatus::Idle,
        SessionStatus::Publishing,
        SessionStatus::PrOpened,
        SessionStatus::Merged,
    ];

    // Helper: check if a session is a descendant of target (walks parent chain).
    // Guards against infinite loops with a visit count.
    let is_descendant_of = |candidate: &Session, target_id: &str, sessions: &[Session]| {
        let mut current_parent = candidate.parent.clone();
        let mut visits = 0;
        const MAX_CHAIN_DEPTH: usize = 100;
        while let Some(p_id) = current_parent {
            if p_id == target_id {
                return true;
            }
            visits += 1;
            if visits > MAX_CHAIN_DEPTH {
                // Cycle detected elsewhere; don't follow it.
                break;
            }
            current_parent = sessions.iter().find(|s| s.id == p_id).and_then(|s| s.parent.clone());
        }
        false
    };

    sessions
        .iter()
        .filter(|s| {
            s.id != parent.id
                && s.repo == parent.repo
                && s.issue == parent.issue
                && s.created_at > parent.created_at
                && live_or_merged_states.contains(&s.status)
                // Exclude sessions whose parent chain reaches the parent
                // (would create a cycle if we pointed to them).
                && !is_descendant_of(s, &parent.id, sessions)
        })
        .max_by_key(|s| s.created_at)
        .cloned()
}

/// The state of the colony `s` is stacked on, from the session records. A colony with no parent is
/// `Fine`. A failed parent is also `Fine` here: the queue walks the child up to the parent's own
/// parent (issue #982) and falls back to the default branch when there is none.
pub(crate) fn parent_state(s: &Session, sessions: &[Session]) -> ParentState {
    let Some(parent_id) = s.parent.as_deref() else {
        return ParentState::Fine;
    };
    let Some(parent) = sessions.iter().find(|p| p.id == parent_id) else {
        return ParentState::Gone(format!("there is no colony `{parent_id}` to stack on"));
    };
    match parent.status {
        // Paused, not over — unless its worktree is gone, which is what resuming needs.
        SessionStatus::Stopped | SessionStatus::Parked => {
            if parent.cleaned_up {
                ParentState::Gone(format!(
                    "{} (`{}`) was cleaned up and cannot be resumed",
                    label(parent),
                    short(parent_id)
                ))
            } else {
                ParentState::Paused(format!(
                    "waiting on {} (`{}`, {})",
                    label(parent),
                    short(parent_id),
                    parent.status.as_str()
                ))
            }
        }
        // A blocked parent waits itself; its dependents wait with it, so the whole chain reads as
        // waiting rather than as a queue that never moves.
        SessionStatus::Blocked => ParentState::Paused(format!("waiting on {} (`{}`, blocked)", label(parent), short(parent_id))),
        // A parent with no changes: for merge-gated children, if it has an issue, the work may be
        // retried, so stay unmet; for stacked children or no issue, it's gone for good.
        SessionStatus::NoChanges if !s.stack => {
            if let Some(issue) = parent.issue {
                ParentState::Unmet {
                    reason: format!(
                        "waiting on {} (`{}`, no changes; issue still open)",
                        label(parent),
                        short(parent_id)
                    ),
                    repo: parent.repo.clone(),
                    issue,
                }
            } else {
                ParentState::Gone(format!(
                    "{} (`{}`) made no changes, so it has no branch to build on",
                    label(parent),
                    short(parent_id)
                ))
            }
        }
        SessionStatus::NoChanges => ParentState::Gone(format!(
            "{} (`{}`) made no changes, so it has no branch to build on",
            label(parent),
            short(parent_id)
        )),
        SessionStatus::Closed if !s.stack => {
            if let Some(issue) = parent.issue {
                ParentState::Unmet {
                    reason: format!(
                        "waiting on {} (`{}`, pull request closed unmerged; issue still open)",
                        label(parent),
                        short(parent_id)
                    ),
                    repo: parent.repo.clone(),
                    issue,
                }
            } else {
                ParentState::Gone(format!(
                    "{} (`{}`) was closed without merging",
                    label(parent),
                    short(parent_id)
                ))
            }
        }
        _ if parent.cleaned_up && parent.pr_url.is_none() && parent.status.is_terminal() => ParentState::Gone(format!(
            "{} (`{}`) was cleaned up before it opened a pull request, so it has no branch to build on",
            label(parent),
            short(parent_id)
        )),
        _ => ParentState::Fine,
    }
}

/// The text the log gets when a dependent is re-based on the default branch.
pub(crate) fn rebased_message(why: &str) -> String {
    format!("{why}; re-based on the default branch and queued instead of failing")
}

/// Moves every blocked colony whose parent changed: back to the queue when the parent runs again (or
/// failed — the queue then walks it up the stack), re-based on the default branch when the parent is
/// gone for good, and with its reason kept true while the parent stays paused. For unmet parents (those
/// with no changes or closed unmerged but with an open issue), checks if the issue is still open. If the
/// issue closes, rebase the child. If another session was created for the same issue, re-point the child
/// to it. Runs once a queue tick.
pub(crate) async fn release_blocked(app: &Shared) {
    let sessions = app.sessions.read().await.clone();
    for s in sessions.iter().filter(|s| s.status == SessionStatus::Blocked) {
        match parent_state(s, &sessions) {
            ParentState::Paused(reason) => {
                if s.blocked_reason.as_deref() != Some(reason.as_str()) {
                    app.update_session(&s.id, |x| {
                        if x.status == SessionStatus::Blocked {
                            x.blocked_reason = Some(reason.clone());
                        }
                    })
                    .await;
                }
            }
            ParentState::Unmet { reason, repo, issue } => {
                // Check if the issue is still open.
                match is_issue_open(app, &repo, issue).await {
                    Some(true) => {
                        // Issue is still open. Check for a relaunch.
                        let parent = sessions.iter().find(|p| p.id == s.parent.as_deref().unwrap_or(""));
                        if let Some(parent_session) = parent {
                            if let Some(relaunch) = find_relaunch(parent_session, &sessions) {
                                // Re-point child to the relaunch.
                                let moved = app
                                    .update_session(&s.id, |x| {
                                        if x.status != SessionStatus::Blocked {
                                            return false;
                                        }
                                        x.parent = Some(relaunch.id.clone());
                                        true
                                    })
                                    .await
                                    .is_some_and(|(_, moved)| moved);
                                if moved {
                                    app.session_log(
                                        &s.id,
                                        "info",
                                        format!("#{issue} was relaunched as `{}`; now waiting on it", short(&relaunch.id)),
                                    )
                                    .await;
                                }
                            } else {
                                // Keep blocked and update reason if needed.
                                if s.blocked_reason.as_deref() != Some(reason.as_str()) {
                                    app.update_session(&s.id, |x| {
                                        if x.status == SessionStatus::Blocked {
                                            x.blocked_reason = Some(reason.clone());
                                        }
                                    })
                                    .await;
                                }
                            }
                        }
                    }
                    Some(false) => {
                        // Issue is closed. Move to Gone behavior: rebase and queue.
                        let why = format!("#{issue} (which was blocking it) has now closed");
                        let moved = app
                            .update_session(&s.id, |x| {
                                if x.status != SessionStatus::Blocked {
                                    return false;
                                }
                                rebase_on_default(x);
                                x.status = SessionStatus::Queued;
                                true
                            })
                            .await
                            .is_some_and(|(_, moved)| moved);
                        if moved {
                            app.session_log(&s.id, "info", rebased_message(&why)).await;
                        }
                    }
                    None => {
                        // Fetch error: stay blocked with current reason.
                        if s.blocked_reason.as_deref() != Some(reason.as_str()) {
                            app.update_session(&s.id, |x| {
                                if x.status == SessionStatus::Blocked {
                                    x.blocked_reason = Some(reason.clone());
                                }
                            })
                            .await;
                        }
                    }
                }
            }
            ParentState::Gone(why) => {
                let moved = app
                    .update_session(&s.id, |x| {
                        if x.status != SessionStatus::Blocked {
                            return false;
                        }
                        rebase_on_default(x);
                        x.status = SessionStatus::Queued;
                        true
                    })
                    .await
                    .is_some_and(|(_, moved)| moved);
                if moved {
                    app.session_log(&s.id, "info", rebased_message(&why)).await;
                }
            }
            ParentState::Fine => {
                let moved = app
                    .update_session(&s.id, |x| {
                        if x.status != SessionStatus::Blocked {
                            return false;
                        }
                        x.blocked_reason = None;
                        x.status = SessionStatus::Queued;
                        true
                    })
                    .await
                    .is_some_and(|(_, moved)| moved);
                if moved {
                    app.session_log(
                        &s.id,
                        "info",
                        "the colony it waited on is running again; back in the queue".into(),
                    )
                    .await;
                }
            }
        }
    }
}

/// Cuts the stack: the colony no longer has a parent and branches from the default branch.
pub(crate) fn rebase_on_default(s: &mut Session) {
    s.parent = None;
    s.stack = false;
    s.stack_fork = None;
    s.blocked_reason = None;
}

#[cfg(test)]
mod tests;
