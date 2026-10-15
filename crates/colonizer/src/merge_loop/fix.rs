//! Issue #1054: a red pull request goes back to its own colony. The colony is resumed on its kept
//! worktree with a one-shot brief ([`brief`]): the failing checks' names, the tail of each failing
//! job's log, and the mandated instructions — fix the cause, never skip, delete or weaken a test
//! or check, run the failing checks locally, and publish to the same pull request. Unlike a
//! resolve ([`super::resolve`]) there is no merge step: the worktree is already the pull request's
//! own. A worktree that cannot take the fix (reclaimed, colony gone) is [`Started::Gone`], and the
//! loop decides between a redo colony and a person.

use crate::{Shared, sessions::SessionStatus, util::exec};
use axum::extract::{Path as AxumPath, State};
use std::path::Path;

/// What starting a fix answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Started {
    /// The colony was resumed on its worktree with the fix brief.
    Resumed,
    /// No worktree to fix on: why.
    Gone(String),
    Failed(String),
}

/// Where a fix is asked to land: the same pull request, from the colony's own worktree, or a fresh
/// redo of it when that worktree was reclaimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Landing {
    SamePullRequest,
    Redo,
}

/// The one-shot brief the colony gets (the session's `resume_note`, or a redo colony's
/// instructions): the failing checks, the tail of each failing job's log, and the mandated
/// instructions — fix the cause, never weaken a check, run the checks locally, publish.
pub(crate) fn brief(pr_url: &str, base: &str, failing: &[String], logs: &str, landing: Landing) -> String {
    let mut out = format!("Your pull request {pr_url} is red on `{base}`: these checks are failing:\n");
    for name in failing.iter().take(40) {
        out.push_str(&format!("- {name}\n"));
    }
    if failing.len() > 40 {
        out.push_str(&format!("- …and {} more\n", failing.len() - 40));
    }
    out.push_str(&format!(
        "\nThe failing jobs' logs (tails):\n\n{logs}\n\
         1. Fix the cause of every failure. Never skip, delete or weaken a test or check to make it pass, and do not \
         revert unrelated work.\n\
         2. Run the failing checks locally until they pass.\n"
    ));
    match landing {
        Landing::SamePullRequest => out.push_str(
            "3. Do not run `git commit`, `git push` or `git rebase`: when you finish, rewrite pr.md — keep its \
             description and add a line saying what you fixed — and the harness commits and pushes your fix to the \
             same pull request.\n",
        ),
        Landing::Redo => {
            let n = pr_url.rsplit('/').next().unwrap_or("N");
            out.push_str(&format!(
                "3. This colony's own worktree could not take the fix, so you are its redo: work on a fresh branch of \
                 your own, using the pull request as your reference (`git fetch origin pull/{n}/head:pr-{n}`) — do not \
                 widen the change — rewrite pr.md when you finish, and the harness opens the redo pull request.\n"
            ));
        }
    }
    out
}

/// Resumes the colony on its kept worktree with the fix brief as its one-shot note. Every way out
/// that is not [`Started::Resumed`] leaves the colony exactly as it was found.
pub(super) async fn start(app: &Shared, id: &str, note: String) -> Started {
    let Some(s) = app.session(id).await else {
        return Started::Gone("the colony is gone".into());
    };
    let (Some(admin), false) = (s.git_admin_dir.clone(), s.cleaned_up) else {
        return Started::Gone("the colony's worktree was cleaned up".into());
    };
    if !Path::new(&s.worktree).is_dir() {
        return Started::Gone("the colony's worktree is gone".into());
    }
    let lock = app.repo_lock(&s.repo).await;
    let _guard = lock.lock().await;
    let git = || super::resolve::worktree_git(app, &admin, &s.worktree);
    match exec(git().args(["status", "--porcelain"])).await {
        Ok(status) if status.lines().all(|l| l.starts_with("??")) => {}
        Ok(_) => return Started::Failed("the colony's worktree has uncommitted changes".into()),
        Err(e) => return Started::Failed(format!("{e:#}")),
    }
    // Resumable is `stopped`: the run publishes the fix to the same pull request and is `pr_opened`
    // again; the brief rides the resume as its one-shot note.
    let flipped = app
        .update_session(id, |x| {
            if x.status != SessionStatus::PrOpened {
                return false;
            }
            x.status = SessionStatus::Stopped;
            x.autopilot = true;
            x.resume_note = Some(note);
            true
        })
        .await
        .is_some_and(|(_, ok)| ok);
    if !flipped {
        return Started::Failed("the colony moved on before it could be resumed".into());
    }
    if let Err(e) = crate::lifecycle::resume(State(app.clone()), AxumPath(id.to_string()), None).await {
        super::resolve::restore(app, id).await;
        return Started::Failed(format!("the colony could not be resumed ({})", e.message()));
    }
    Started::Resumed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brief_fixes_the_cause_never_weakens_and_publishes_to_the_same_pull_request() {
        let text = brief(
            "https://github.com/acme/web/pull/7",
            "main",
            &["clippy".to_string(), "test".to_string()],
            "### clippy (run 5)\nerror: unused import\n",
            Landing::SamePullRequest,
        );
        for needle in [
            "acme/web/pull/7",
            "`main`",
            "- clippy",
            "- test",
            "error: unused import",
            "Fix the cause",
            "Never skip, delete or weaken a test or check",
            "Run the failing checks locally",
            "the same pull request",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
    }

    #[test]
    fn a_redo_lands_on_a_fresh_branch_and_carries_the_same_rules() {
        let text = brief(
            "https://github.com/acme/web/pull/7",
            "main",
            &["test".to_string()],
            "",
            Landing::Redo,
        );
        assert!(text.contains("pull/7/head:pr-7"), "{text}");
        assert!(text.contains("Never skip, delete or weaken a test or check"), "{text}");
        assert!(text.contains("Run the failing checks locally"), "{text}");
        assert!(!text.contains("the same pull request"), "{text}");
    }
}
