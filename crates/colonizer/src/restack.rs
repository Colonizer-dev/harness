//! Queue-by-default stacking and publish-time restack (issue #455).
//!
//! A child colony (`after`) opens its pull request against its parent's branch. When the parent
//! merges with delete-branch, that base is gone and `gh pr create --base <parent>` fails. Two
//! halves fix it:
//!
//! * Queueing decides when a child may start ([`queue_decision`], pure and tested): by default a
//!   child waits for its parent's pull request to merge and then starts from the fresh default
//!   branch. Only `stack: true` branches from the parent's branch while it is still open.
//! * The publish restacks a child whose parent merged under it: [`rebase_onto`] moves only the
//!   child's own commits onto the destination, since the parent's commit is already on the
//!   default branch via squash-merge.

use crate::{
    exec_bits::GitRun,
    sessions::{Session, SessionStatus},
    stack::{self, Stacked},
};
use anyhow::{Context, Result, bail};

/// When a colony created with `after` may start, decided on the parent's record. With `stack` the
/// old rule holds ([`stack::stacked_on`], unchanged): the child branches from the parent's branch
/// as soon as it is pushed. Without it the child queues for the parent's merge instead —
/// pre-publish and open pull requests wait, a merge sends the child to the default branch, and a
/// parent that can never merge the child onto anything refuses it.
pub(crate) fn queue_decision(parent_id: &str, parent: Option<&Session>, stack: bool) -> Stacked {
    if stack {
        return stack::stacked_on(parent_id, parent);
    }
    let Some(parent) = parent else {
        return stack::stacked_on(parent_id, None);
    };
    match parent.status {
        // Nothing to build on yet — and an open pull request is still work unmerged, so the child
        // waits for the merge rather than stacking onto a branch that may yet be deleted.
        SessionStatus::Queued
        | SessionStatus::Starting
        | SessionStatus::Running
        | SessionStatus::WaitingForAnswer
        | SessionStatus::Idle
        | SessionStatus::Publishing
        | SessionStatus::PrOpened => Stacked::Wait,
        // The parent's work is already in the default branch: the queue resolved itself, and the
        // child starts from the default branch like any other colony.
        SessionStatus::Merged => Stacked::Ready(None),
        // Closed without merging leaves nothing to queue for; stacking would still work, so the
        // refusal says how to ask for it.
        SessionStatus::Closed => Stacked::Refuse(format!(
            "colony `{parent_id}`'s pull request was closed without merging, so there is nothing to queue for; \
             pass `stack: true` to build on its branch anyway"
        )),
        // No branch to queue for and none to stack on either: the stacked rule's own refusal.
        SessionStatus::NoChanges | SessionStatus::Failed | SessionStatus::Stopped => {
            match stack::stacked_on(parent_id, Some(parent)) {
                Stacked::Refuse(reason) => Stacked::Refuse(reason),
                other => panic!(
                    "a {status} parent has no branch to lend: {other:?}",
                    status = parent.status.as_str()
                ),
            }
        }
    }
}

/// Whether a colony based on its parent's branch must be rebased now: the parent merged, and this
/// colony's base still names the parent's branch (a retry after a restack already moved on, so its
/// base no longer matches and skips). A base that never named the parent's branch needs nothing.
pub(crate) fn needs_restack(base: Option<&str>, parent: &Session) -> bool {
    parent.status == SessionStatus::Merged && base == Some(parent.branch.as_str())
}

/// Where a restacked child belongs: the branch the merged parent was itself based on — the same
/// rule the pull-request watcher applies to open children — or the repository default when the
/// parent recorded no base, a shape this code never creates.
pub(crate) fn restack_dest(parent: &Session, default_branch: &str) -> String {
    stack::retarget_base(parent).unwrap_or_else(|| default_branch.to_string())
}

/// Which commit the rebase keeps: the sha the stacked child's worktree branched from, recorded at
/// boot ([`crate::sessions::Session::stack_fork`]). The parent's remote ref disappears once its
/// branch is deleted — `sync_repo` fetches with `--prune` — which is why the boot records it.
pub async fn resolve_fork(git: &mut impl GitRun, recorded: Option<&str>, parent_branch: &str) -> Result<String> {
    if let Some(sha) = recorded.filter(|s| !s.trim().is_empty()) {
        return Ok(sha.trim().to_string());
    }
    // No recording (a child stacked before the field existed): the fork point is still computable
    // while the parent's remote ref survives. Past its deletion there is nothing left to rebase
    // from, and failing loudly beats rebasing the wrong range.
    let sha = git
        .run_git(vec![
            "merge-base".to_string(),
            "HEAD".to_string(),
            format!("origin/{parent_branch}"),
        ])
        .await
        .context(format!(
            "could not find where this branch forked from {parent_branch}: its remote ref is gone and no fork \
             point was recorded; publish again to retry"
        ))?;
    let sha = sha.trim().to_string();
    if sha.is_empty() {
        bail!(
            "could not find where this branch forked from {parent_branch}: its remote ref is gone and no fork \
             point was recorded; publish again to retry"
        );
    }
    Ok(sha)
}

/// Rebases `branch` onto `origin/<dest>`, keeping only the commits after `fork` — the parent's own
/// commit is already on the destination via squash-merge, so it must not ride along. Returns the
/// number of the child's own commits moved. On conflict the rebase is aborted first, so the branch
/// is never left half-rebased, and the error names the conflicted files.
pub async fn rebase_onto(git: &mut impl GitRun, branch: &str, fork: &str, dest: &str) -> Result<usize> {
    let count = git
        .run_git(vec![
            "rev-list".to_string(),
            "--count".to_string(),
            format!("{fork}..{branch}"),
        ])
        .await?
        .trim()
        .parse::<usize>()
        .context("could not count the branch's own commits")?;
    if git
        .run_git(vec![
            "rebase".to_string(),
            "--onto".to_string(),
            format!("origin/{dest}"),
            fork.to_string(),
            branch.to_string(),
        ])
        .await
        .is_err()
    {
        let conflicts = git
            .run_git(vec![
                "diff".to_string(),
                "--name-only".to_string(),
                "--diff-filter=U".to_string(),
            ])
            .await
            .unwrap_or_default();
        let conflicts: Vec<String> = conflicts.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
        let _ = git.run_git(vec!["rebase".to_string(), "--abort".to_string()]).await;
        if conflicts.is_empty() {
            bail!("could not rebase {branch} onto origin/{dest}; the rebase was aborted, publish again to retry");
        }
        bail!(
            "could not rebase {branch} onto origin/{dest}: conflicts in {}; the rebase was aborted, \
             resolve them and publish again",
            conflicts.join(", ")
        );
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use std::{future::Future, pin::Pin};

    /// A colony another one could queue behind or stack on, with the id and branch the tests need.
    fn parent(status: SessionStatus) -> Session {
        let mut p = colony("acme", status);
        p.id = "parent".into();
        p.branch = "colonizer/issue-9-parent".into();
        p.base = Some("main".into());
        p
    }

    fn refuses(decision: Stacked) -> String {
        match decision {
            Stacked::Refuse(reason) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_stack_keeps_the_old_rule() {
        // An open pull request lends its branch, and a merge sends the child to the default.
        match queue_decision("parent", Some(&parent(SessionStatus::PrOpened)), true) {
            Stacked::Ready(Some(branch)) => assert_eq!(branch, "colonizer/issue-9-parent"),
            other => panic!("an explicit stack still branches from the open parent: {other:?}"),
        }
        assert!(matches!(
            queue_decision("parent", Some(&parent(SessionStatus::Merged)), true),
            Stacked::Ready(None)
        ));
        // ...while a running parent still makes it wait.
        assert!(matches!(
            queue_decision("parent", Some(&parent(SessionStatus::Running)), true),
            Stacked::Wait
        ));
    }

    #[test]
    fn by_default_a_child_waits_until_its_parents_pull_request_merges() {
        for status in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
            // An open pull request is still work unmerged: the child queues for the merge rather
            // than stacking onto a branch that may yet be deleted.
            SessionStatus::PrOpened,
        ] {
            assert!(
                matches!(queue_decision("parent", Some(&parent(status)), false), Stacked::Wait),
                "a parent that is {} has nothing merged yet",
                status.as_str()
            );
        }
        assert!(
            matches!(
                queue_decision("parent", Some(&parent(SessionStatus::Merged)), false),
                Stacked::Ready(None)
            ),
            "a merged parent sends the child to the default branch"
        );
    }

    #[test]
    fn a_closed_parent_refuses_and_says_how_to_stack_anyway() {
        let reason = refuses(queue_decision("parent", Some(&parent(SessionStatus::Closed)), false));
        assert!(reason.contains("parent"), "{reason}");
        assert!(reason.contains("closed without merging"), "{reason}");
        assert!(reason.contains("stack: true"), "the way back is named: {reason}");
    }

    #[test]
    fn a_parent_with_no_branch_to_lend_refuses_by_name_and_reason() {
        for (status, why) in [
            (SessionStatus::Failed, "failed"),
            (SessionStatus::Stopped, "was stopped"),
            (SessionStatus::NoChanges, "made no changes"),
        ] {
            let reason = refuses(queue_decision("parent", Some(&parent(status)), false));
            assert!(reason.contains("parent"), "the parent is named: {reason}");
            assert!(reason.contains(why), "{}: {reason}", status.as_str());
        }
        let reason = refuses(queue_decision("ghost", None, false));
        assert!(reason.contains("ghost"), "{reason}");
    }

    #[test]
    fn only_a_merged_parent_with_a_matching_base_needs_a_restack() {
        let merged = parent(SessionStatus::Merged);
        assert!(needs_restack(Some("colonizer/issue-9-parent"), &merged));
        assert!(!needs_restack(Some("main"), &merged), "a retry that already moved on skips");
        assert!(!needs_restack(None, &merged));
        assert!(
            !needs_restack(Some("colonizer/issue-9-parent"), &parent(SessionStatus::PrOpened)),
            "an unmerged parent restacks nothing"
        );
    }

    #[test]
    fn a_restack_follows_the_parents_own_base_not_the_default() {
        let mut middle = parent(SessionStatus::Merged);
        middle.base = Some("colonizer/issue-9-parent".into());
        assert_eq!(restack_dest(&middle, "main"), "colonizer/issue-9-parent");
        middle.base = None;
        assert_eq!(
            restack_dest(&middle, "main"),
            "main",
            "no recorded base falls back to the default"
        );
    }

    #[test]
    fn a_recorded_fork_is_used_verbatim() {
        /// A runner that must never run: the recorded fork returns before any git is consulted.
        struct NoGit;
        impl GitRun for NoGit {
            fn run_git<'a>(&'a mut self, _args: Vec<String>) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
                Box::pin(async { panic!("no git should run when the fork was recorded") })
            }
        }
        let fork = tokio_test_block_on(resolve_fork(&mut NoGit, Some("  abc123  "), "parent")).unwrap();
        assert_eq!(fork, "abc123", "no git is consulted when the boot recorded the fork");
    }

    /// Drives an async helper without a runtime, for the one case that touches no git at all.
    fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// `git` against a throwaway repo, without the harness: `GIT_CONFIG_NOSYSTEM` plus a local
    /// identity keep it hermetic (no global config needed). The `env_remove` calls drop the
    /// `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` a colony sandbox exports for its own worktree,
    /// so the test's git never touches anything outside the scratch repo.
    struct CliGit {
        dir: std::path::PathBuf,
    }

    impl GitRun for CliGit {
        fn run_git<'a>(&'a mut self, args: Vec<String>) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            Box::pin(async move {
                let out = tokio::process::Command::new("git")
                    .current_dir(&self.dir)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env_remove("GIT_DIR")
                    .env_remove("GIT_WORK_TREE")
                    .env_remove("GIT_INDEX_FILE")
                    .args(&args)
                    .output()
                    .await?;
                if !out.status.success() {
                    anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
                }
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            })
        }
    }

    fn scratch_repo(name: &str) -> (std::path::PathBuf, RemoveOnDrop) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "colonizer-restack-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), RemoveOnDrop(dir))
    }

    struct RemoveOnDrop(std::path::PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_sync(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The parent's branch squash-merged into main, then the child's own commits rebased onto main:
    /// only the child's commits land — the parent's commit must not ride along, since it is already
    /// on main through the squash.
    #[tokio::test]
    async fn only_the_childs_own_commits_land_on_main_after_a_squash_merge() {
        let (dir, _cleanup) = scratch_repo("squash");
        git_sync(&dir, &["init", "-q", "-b", "main"]);
        git_sync(&dir, &["config", "user.email", "restack@test"]);
        git_sync(&dir, &["config", "user.name", "restack test"]);
        git_sync(&dir, &["config", "commit.gpgsign", "false"]);
        // Fake an `origin` pointing at the same repo, so `origin/main` resolves like it does in a
        // colony worktree.
        git_sync(&dir, &["remote", "add", "origin", &dir.to_string_lossy()]);
        git_sync(&dir, &["fetch", "-q", "origin"]);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "base"]);
        git_sync(&dir, &["branch", "parent"]);
        git_sync(&dir, &["checkout", "-q", "parent"]);
        std::fs::write(dir.join("parent.txt"), "parent\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "parent work"]);
        git_sync(&dir, &["branch", "child"]);
        git_sync(&dir, &["checkout", "-q", "child"]);
        std::fs::write(dir.join("child.txt"), "child\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "child work"]);
        // The fork point the boot would have recorded: `origin/parent` at branch time.
        let fork = git_sync(&dir, &["rev-parse", "parent"]).trim().to_string();
        // GitHub's squash-merge, then the delete-branch cleanup that strands the child's base.
        git_sync(&dir, &["checkout", "-q", "main"]);
        git_sync(&dir, &["merge", "-q", "--squash", "parent"]);
        git_sync(&dir, &["commit", "-q", "-m", "parent work (#1)"]);
        git_sync(&dir, &["branch", "-D", "parent"]);
        git_sync(&dir, &["update-ref", "-d", "refs/remotes/origin/parent"]);
        git_sync(&dir, &["fetch", "-q", "origin"]);
        git_sync(&dir, &["checkout", "-q", "child"]);

        let mut git = CliGit { dir: dir.clone() };
        let moved = rebase_onto(&mut git, "child", &fork, "main").await.unwrap();
        assert_eq!(moved, 1, "exactly the child's own commit moves");
        let log = git_sync(&dir, &["log", "--oneline", "main..child"]);
        assert_eq!(log.lines().count(), 1, "one commit above main: {log}");
        assert!(log.contains("child work"), "{log}");
        assert!(dir.join("child.txt").exists());
        assert!(
            dir.join("parent.txt").exists(),
            "the parent's work arrives via the squash, not the rebase"
        );
        assert!(dir.join("base.txt").exists());
    }

    /// A conflicting rebase aborts and names the files, leaving the branch where it was.
    #[tokio::test]
    async fn a_conflicting_rebase_aborts_and_names_the_files() {
        let (dir, _cleanup) = scratch_repo("conflict");
        git_sync(&dir, &["init", "-q", "-b", "main"]);
        git_sync(&dir, &["config", "user.email", "restack@test"]);
        git_sync(&dir, &["config", "user.name", "restack test"]);
        git_sync(&dir, &["config", "commit.gpgsign", "false"]);
        git_sync(&dir, &["remote", "add", "origin", &dir.to_string_lossy()]);
        git_sync(&dir, &["fetch", "-q", "origin"]);
        std::fs::write(dir.join("shared.txt"), "base\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "base"]);
        git_sync(&dir, &["branch", "child"]);
        git_sync(&dir, &["checkout", "-q", "child"]);
        std::fs::write(dir.join("shared.txt"), "child\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "child work"]);
        let fork = git_sync(&dir, &["rev-parse", "child^"]).trim().to_string();
        git_sync(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("shared.txt"), "main\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "main moves"]);
        // The earlier fetch ran before `main` carried any commits, so `origin/main` must be refreshed
        // now — exactly what `sync_repo`'s own fetch does before a real restack.
        git_sync(&dir, &["fetch", "-q", "origin"]);
        git_sync(&dir, &["checkout", "-q", "child"]);
        let head_before = git_sync(&dir, &["rev-parse", "child"]).trim().to_string();

        let mut git = CliGit { dir: dir.clone() };
        let err = rebase_onto(&mut git, "child", &fork, "main").await.unwrap_err();
        assert!(
            format!("{err:#}").contains("shared.txt"),
            "the conflicted file is named: {err:#}"
        );
        assert_eq!(
            git_sync(&dir, &["rev-parse", "child"]).trim(),
            head_before,
            "the aborted rebase leaves the branch where it was"
        );
    }

    /// Without a recording, the fork still resolves while the parent's remote ref survives — and
    /// fails clearly once it is gone.
    #[tokio::test]
    async fn the_fork_falls_back_to_merge_base_while_the_parents_ref_survives() {
        let (dir, _cleanup) = scratch_repo("fork");
        git_sync(&dir, &["init", "-q", "-b", "main"]);
        git_sync(&dir, &["config", "user.email", "restack@test"]);
        git_sync(&dir, &["config", "user.name", "restack test"]);
        git_sync(&dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "base"]);
        git_sync(&dir, &["branch", "parent"]);
        git_sync(&dir, &["checkout", "-q", "parent"]);
        std::fs::write(dir.join("parent.txt"), "parent\n").unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "parent work"]);
        // A remote ref without a remote: exactly what `sync_repo` leaves behind.
        let sha = git_sync(&dir, &["rev-parse", "parent"]).trim().to_string();
        git_sync(&dir, &["update-ref", "refs/remotes/origin/parent", &sha]);

        let mut git = CliGit { dir: dir.clone() };
        let fork = resolve_fork(&mut git, None, "parent").await.unwrap();
        assert_eq!(fork, sha);
        git_sync(&dir, &["update-ref", "-d", "refs/remotes/origin/parent"]);
        let err = resolve_fork(&mut git, None, "parent").await.unwrap_err();
        assert!(
            format!("{err:#}").contains("no fork point was recorded"),
            "past the ref's deletion the failure says what is missing: {err:#}"
        );
    }
}
