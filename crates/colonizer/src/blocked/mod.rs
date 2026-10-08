//! Stacked dependents whose parent is paused or gone (issue #1140).
//!
//! A colony stacked on another builds on that colony's branch, so it waits for the parent. What the
//! wait looks like depends on the parent. A parent that is stopped or parked is *paused*: it may be
//! resumed and publish yet, so its dependents go to `blocked` — no slot, no microVM, not failed —
//! and go back to the queue the moment it runs again. A parent that is gone for good (cleaned up
//! before it published, a closed pull request, no changes, deleted) can never lend a branch, and
//! stacking only exists to avoid conflicts, so the dependent re-bases on the default branch and
//! queues like any other colony; a conflict is then the ordinary rebase path's to handle. A cascade
//! never produces `failed`.
//!
//! The decision is a pure function over the session records ([`parent_state`]); the queue applies it
//! to a queued colony (`queue::gate`) and [`release_blocked`] applies it to a blocked one each tick.

use crate::{
    Shared,
    sessions::{Session, SessionStatus},
};

/// What the colony a dependent is stacked on means for it right now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParentState {
    /// Nothing here holds the dependent back; the queue's ordinary rules decide.
    Fine,
    /// The parent is paused (or itself blocked): wait without a slot. The text says on what.
    Paused(String),
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
        // A parent that can never provide a branch: the re-base replaces what used to fail the child.
        SessionStatus::NoChanges => ParentState::Gone(format!(
            "{} (`{}`) made no changes, so it has no branch to build on",
            label(parent),
            short(parent_id)
        )),
        SessionStatus::Closed if !s.stack => ParentState::Gone(format!(
            "{} (`{}`) was closed without merging",
            label(parent),
            short(parent_id)
        )),
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
/// gone for good, and with its reason kept true while the parent stays paused. Runs once a queue tick.
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
