//! Stacked colonies: a colony created with `after` builds on another colony's branch instead of the
//! repository's default branch, so dependent work can start without waiting for a review to finish.
//!
//! Everything here is a pure decision over the session records, so the rules can be tested apart
//! from the queue, the boot and the pull-request watcher that apply them.

use crate::sessions::{Session, SessionStatus};

/// What a queued child colony may do about the colony it was stacked on.
#[derive(Debug)]
pub(crate) enum Stacked {
    /// The parent has not pushed a branch yet; keep waiting.
    Wait,
    /// Branch from this base. `None` means the repository default branch.
    Ready(Option<String>),
    /// The parent can never provide a branch, and this says which.
    Refuse(String),
}

/// The decision for a colony stacked on `parent_id`. `parent` is the colony that id names, or `None`
/// when there is none — it was deleted, or the id never existed. The parent's status is matched
/// exhaustively, so a future status forces a decision here instead of silently picking one.
pub(crate) fn stacked_on(parent_id: &str, parent: Option<&Session>) -> Stacked {
    let Some(parent) = parent else {
        return Stacked::Refuse(format!("there is no colony `{parent_id}` to stack on"));
    };
    match parent.status {
        // The branch exists only once the parent's work is published, so until then the child waits.
        SessionStatus::Queued
        | SessionStatus::Starting
        | SessionStatus::Running
        | SessionStatus::WaitingForAnswer
        | SessionStatus::Idle
        | SessionStatus::Publishing => Stacked::Wait,
        // A closed pull request still has its branch pushed, so it is stackable; whether building on
        // work that was closed is wise is the reviewer's call, not the queue's.
        SessionStatus::PrOpened | SessionStatus::Closed => {
            // Opening a pull request is what pushes the branch, and cleanup removes the worktree and
            // the local branch but never anything on the remote — so a cleaned-up parent whose pull
            // request opened still has its branch there (`sync_repo` fetches every ref under
            // `refs/heads/*`). One cleaned up before it ever published has nothing anywhere for a
            // child to build on.
            if parent.cleaned_up && parent.pr_url.is_none() {
                return Stacked::Refuse(format!(
                    "colony `{parent_id}` was cleaned up before it opened a pull request, so it has no branch to build on"
                ));
            }
            Stacked::Ready(Some(parent.branch.clone()))
        }
        // The parent's work is already in the default branch, and its branch may yet be pruned: the
        // stack resolved itself, and the child starts from the default branch like any other colony.
        // Cleanup of the parent is beside the point — nothing about its branch is needed any more.
        SessionStatus::Merged => Stacked::Ready(None),
        SessionStatus::NoChanges => Stacked::Refuse(format!(
            "colony `{parent_id}` made no changes, so it has no branch to build on"
        )),
        // A stopped colony is paused rather than finished: one stopped before it published has
        // nothing anywhere to build on, and one stopped after still owns its branch but has a story
        // that is not over — it may be resumed and keep working. Refuse either way (the message says
        // only what is true of both), since a resumed or published parent can be stacked on again.
        SessionStatus::Stopped => Stacked::Refuse(format!(
            "colony `{parent_id}` was stopped, so it cannot be stacked on; resume it and try again"
        )),
        SessionStatus::Failed => Stacked::Refuse(format!("colony `{parent_id}` failed, so it has no branch to build on")),
    }
}

/// What a boot starts from: the decision [`boot_base`] makes, with the repository's default branch —
/// the one answer only the boot can look up — left for it to fetch.
#[derive(Debug)]
pub(crate) enum BootBase {
    /// The base the colony already has (a resume keeps its own): the branch exists on top of it.
    Kept(String),
    /// Branch from the parent's branch, which is on the remote. `colony` names the parent.
    Parent { colony: String, branch: String },
    /// The repository's default branch: no parent at all, or a parent whose work has merged.
    Default,
    /// The parent has not pushed a branch yet. The queue holds a fresh colony while it waits, so a
    /// boot meeting this means the parent moved under the queue's feet; `colony` names the parent.
    Wait { colony: String },
    /// The parent can never provide a branch, and this says which and why.
    Refuse(String),
}

/// Where a boot's base comes from, decided on the record and the parent's current state — the same
/// rule `boot_inner` applies, pulled out pure so it can be tested without git or GitHub. `kept` is
/// the base a resume keeps (`s.base.clone().filter(|_| resume)` at the call site): the boot reuses
/// it whatever the parent is now doing, because the branch already exists on top of it. `parent_id`
/// and `parent` name the colony stacked on, when there is one; `stack` is the child's launch choice
/// — an explicit stack branches from the parent's open branch, while the default queues for its
/// merge (see `restack::queue_decision`).
pub(crate) fn boot_base(kept: Option<String>, parent_id: Option<&str>, parent: Option<&Session>, stack: bool) -> BootBase {
    if let Some(base) = kept {
        return BootBase::Kept(base);
    }
    let Some(parent_id) = parent_id else {
        return BootBase::Default;
    };
    match crate::restack::queue_decision(parent_id, parent, stack) {
        Stacked::Ready(Some(branch)) => BootBase::Parent {
            colony: parent_id.to_string(),
            branch,
        },
        Stacked::Ready(None) => BootBase::Default,
        Stacked::Wait => BootBase::Wait {
            colony: parent_id.to_string(),
        },
        Stacked::Refuse(reason) => BootBase::Refuse(reason),
    }
}

/// The colonies whose pull requests must move onto the branch `parent` was itself based on now that
/// `parent` has merged: the ones with a pull request still open and still based on the branch that
/// just merged. The base, not the `parent` link, is what selects — a colony deeper in the stack was
/// retargeted onto this branch when the colony it is stacked on merged, so its `parent` names some
/// other colony entirely while its base is what still points here. GitHub retargets a pull request
/// only when its base branch is *deleted*, and nothing here ever deletes a branch, so the watcher
/// does this explicitly.
pub(crate) fn children_to_retarget<'a>(sessions: &'a [Session], parent: &Session) -> Vec<&'a Session> {
    sessions
        .iter()
        .filter(|c| {
            c.status == SessionStatus::PrOpened && c.pr_url.is_some() && c.base.as_deref() == Some(parent.branch.as_str())
        })
        .collect()
}

/// Where those children belong: the branch the merged colony was itself based on — GitHub's own rule,
/// since a dependent pull request follows its merged base's base, not the repository's default. For a
/// stack deeper than two the two differ: in A ← B ← C with B merging, C must move onto A's branch, or
/// C's diff would grow all of A's still-unmerged work. `None` means the merged colony recorded no
/// base at all — a shape this code never creates — and the watcher falls back to the repository's
/// default branch for it.
pub(crate) fn retarget_base(parent: &Session) -> Option<String> {
    parent.base.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;

    /// A colony another one could be stacked on, with the id and branch the tests need.
    fn parent(status: SessionStatus) -> Session {
        let mut p = colony("acme", status);
        p.id = "parent".into();
        p.branch = "colonizer/issue-9-parent".into();
        p.base = Some("main".into());
        p
    }

    fn refuses(stacked: Stacked) -> String {
        match stacked {
            Stacked::Refuse(reason) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_parent_refuses_and_names_the_missing_colony() {
        let reason = refuses(stacked_on("ghost", None));
        assert!(reason.contains("ghost"), "{reason}");
        assert!(reason.contains("no colony"), "{reason}");
    }

    #[test]
    fn a_child_waits_while_its_parent_has_not_published_a_branch() {
        for status in [
            SessionStatus::Queued,
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Publishing,
        ] {
            assert!(
                matches!(stacked_on("parent", Some(&parent(status))), Stacked::Wait),
                "a parent that is {} has no branch on the remote yet",
                status.as_str()
            );
        }
    }

    #[test]
    fn a_merged_parent_sends_the_child_to_the_default_branch() {
        let mut merged = parent(SessionStatus::Merged);
        assert!(matches!(stacked_on("parent", Some(&merged)), Stacked::Ready(None)));
        // Even one cleaned up after the merge: the work is in the default branch, and the parent's
        // own branch may be gone.
        merged.cleaned_up = true;
        assert!(
            matches!(stacked_on("parent", Some(&merged)), Stacked::Ready(None)),
            "a merged parent's cleanup changes nothing"
        );
    }

    #[test]
    fn an_open_or_closed_pull_request_branch_is_ready_to_build_on() {
        for status in [SessionStatus::PrOpened, SessionStatus::Closed] {
            match stacked_on("parent", Some(&parent(status))) {
                Stacked::Ready(Some(branch)) => assert_eq!(branch, "colonizer/issue-9-parent", "{status:?}"),
                other => panic!("{} has its branch on the remote: {other:?}", status.as_str()),
            }
        }
    }

    #[test]
    fn a_failed_stopped_or_changeless_parent_is_refused_by_name_and_reason() {
        for (status, why) in [
            (SessionStatus::Failed, "failed"),
            (SessionStatus::Stopped, "was stopped"),
            (SessionStatus::NoChanges, "made no changes"),
        ] {
            let reason = refuses(stacked_on("parent", Some(&parent(status))));
            assert!(reason.contains("parent"), "the parent is named: {reason}");
            assert!(reason.contains(why), "{}: {reason}", status.as_str());
        }
    }

    #[test]
    fn a_stopped_parent_is_refused_whatever_its_pull_request_did() {
        // The old message claimed a stopped parent "was stopped before it opened a pull request" —
        // false for one stopped after. The refusal covers both, and the wording must too.
        let mut before = parent(SessionStatus::Stopped);
        before.pr_url = None;
        let reason = refuses(stacked_on("parent", Some(&before)));
        assert!(
            !reason.contains("before it opened"),
            "no claim about the pull request: {reason}"
        );
        assert!(reason.contains("resume it"), "the way back is named: {reason}");

        let mut after = parent(SessionStatus::Stopped);
        after.pr_url = Some("https://github.com/acme/repo/pull/9".into());
        let reason = refuses(stacked_on("parent", Some(&after)));
        assert!(
            !reason.contains("before it opened"),
            "a parent stopped after its pull request opened is not accused otherwise: {reason}"
        );
        assert!(reason.contains("resume it"), "{reason}");
    }

    #[test]
    fn a_parent_cleaned_up_before_its_pull_request_has_nothing_to_build_on() {
        let mut cleaned = parent(SessionStatus::PrOpened);
        cleaned.cleaned_up = true;
        cleaned.pr_url = None;
        let reason = refuses(stacked_on("parent", Some(&cleaned)));
        assert!(reason.contains("parent"), "{reason}");
        assert!(reason.contains("cleaned up before it opened a pull request"), "{reason}");
    }

    #[test]
    fn a_cleaned_up_parent_that_did_open_its_pull_request_still_lends_its_branch() {
        for status in [SessionStatus::PrOpened, SessionStatus::Closed] {
            let mut cleaned = parent(status);
            cleaned.cleaned_up = true;
            cleaned.pr_url = Some("https://github.com/acme/repo/pull/9".into());
            match stacked_on("parent", Some(&cleaned)) {
                Stacked::Ready(Some(branch)) => {
                    assert_eq!(branch, "colonizer/issue-9-parent", "{status:?}");
                }
                other => panic!("cleanup never removes a pushed branch: {other:?}"),
            }
        }
    }

    #[test]
    fn a_merge_retargets_the_open_pull_requests_still_based_on_the_merged_branch() {
        let merged = parent(SessionStatus::Merged);
        let mut child = colony("acme", SessionStatus::PrOpened);
        child.id = "child".into();
        child.parent = Some("parent".into());
        child.branch = "colonizer/issue-10-child".into();
        child.base = Some("colonizer/issue-9-parent".into());
        child.pr_url = Some("https://github.com/acme/repo/pull/10".into());

        let mut already_moved = child.clone();
        already_moved.id = "already-moved".into();
        already_moved.base = Some("main".into());
        let mut closed = child.clone();
        closed.id = "closed".into();
        closed.status = SessionStatus::Closed;
        let mut never_published = child.clone();
        never_published.id = "never-published".into();
        never_published.pr_url = None;
        // The `parent` link does not select: a colony that rides out the merge of the colony it is
        // stacked on still names that colony as its parent while sitting on the bottom colony's
        // branch — and the base is what says where its pull request points now.
        let mut someone_elses = child.clone();
        someone_elses.id = "someone-elses".into();
        someone_elses.parent = Some("someone-else".into());
        let mut no_parent = child.clone();
        no_parent.id = "no-parent".into();
        no_parent.parent = None;

        let sessions = vec![child, already_moved, closed, never_published, someone_elses, no_parent];
        let targets = children_to_retarget(&sessions, &merged);
        assert_eq!(
            targets.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["child", "someone-elses", "no-parent"],
            "every open pull request still based on the merged branch moves, whatever its parent link says"
        );
    }

    #[test]
    fn a_middle_colony_merging_sends_its_child_to_the_bottom_colonys_branch_not_the_default() {
        // A ← B ← C: the bottom colony's base is the default, the middle's base is the bottom's
        // branch, and the top's base is the middle's branch. When the middle merges, the top follows
        // the middle's own base — the bottom's branch — or the top's diff would grow all of the
        // bottom's still-unmerged work.
        let bottom = parent(SessionStatus::PrOpened);
        let mut middle = parent(SessionStatus::Merged);
        middle.id = "middle".into();
        middle.parent = Some("parent".into());
        middle.branch = "colonizer/issue-10-middle".into();
        middle.base = Some("colonizer/issue-9-parent".into());
        let mut top = colony("acme", SessionStatus::PrOpened);
        top.id = "top".into();
        top.parent = Some("middle".into());
        top.branch = "colonizer/issue-11-top".into();
        top.base = Some("colonizer/issue-10-middle".into());
        top.pr_url = Some("https://github.com/acme/repo/pull/11".into());

        let sessions = vec![bottom, middle.clone(), top];
        assert_eq!(
            children_to_retarget(&sessions, &middle)
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["top"],
            "the middle merging selects the colony stacked on it"
        );
        assert_eq!(
            retarget_base(&middle).as_deref(),
            Some("colonizer/issue-9-parent"),
            "the destination is the merged middle's own base, not the default branch"
        );
    }

    #[test]
    fn the_bottom_colony_merging_last_retargets_the_grandchild_it_never_parented() {
        // A ← B ← C, with B merged first: C's pull request was moved onto A's branch, but C still
        // records B as its parent. When A then merges, only C's base says whose branch it sits on —
        // selecting on the parent link would leave C pointed at A's merged-but-undeleted branch
        // forever.
        let mut a = parent(SessionStatus::Merged);
        a.id = "a".into();
        let mut b = parent(SessionStatus::Merged);
        b.id = "b".into();
        b.parent = Some("a".into());
        b.branch = "colonizer/issue-10-b".into();
        b.base = Some("colonizer/issue-9-parent".into());
        let mut c = colony("acme", SessionStatus::PrOpened);
        c.id = "c".into();
        c.parent = Some("b".into());
        c.branch = "colonizer/issue-11-c".into();
        c.base = Some("colonizer/issue-9-parent".into());
        c.pr_url = Some("https://github.com/acme/repo/pull/11".into());

        let sessions = vec![a, b, c];
        assert_eq!(
            children_to_retarget(&sessions, &sessions[0])
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["c"],
            "the grandchild follows the bottom colony's merge, though its parent is the merged middle"
        );
        assert_eq!(
            retarget_base(&sessions[0]).as_deref(),
            Some("main"),
            "the destination is the bottom colony's own base"
        );
    }

    // -- what a boot starts from -----------------------------------------------------------------

    #[test]
    fn a_resume_keeps_its_base_whatever_its_parent_is_now_doing() {
        // A colony with a kept worktree needs no branch from anyone: its branch already exists on
        // top of the base it kept, so even a parent that has since failed changes nothing.
        for status in [
            SessionStatus::Running,
            SessionStatus::Failed,
            SessionStatus::Stopped,
            SessionStatus::Merged,
        ] {
            match boot_base(
                Some("colonizer/issue-9-parent".into()),
                Some("parent"),
                Some(&parent(status)),
                true,
            ) {
                BootBase::Kept(base) => assert_eq!(base, "colonizer/issue-9-parent", "{status:?}"),
                other => panic!("a resume never asks its parent again: {other:?} for {status:?}"),
            }
        }
    }

    #[test]
    fn a_boot_with_no_parent_branches_from_the_default() {
        assert!(matches!(boot_base(None, None, None, true), BootBase::Default));
    }

    #[test]
    fn a_fresh_colony_branches_from_its_parents_branch_once_it_is_pushed() {
        let decision = boot_base(None, Some("parent"), Some(&parent(SessionStatus::PrOpened)), true);
        match decision {
            BootBase::Parent { colony, branch } => {
                assert_eq!(colony, "parent");
                assert_eq!(branch, "colonizer/issue-9-parent");
            }
            other => panic!("the parent's branch is on the remote: {other:?}"),
        }
        // ...and from the default once that parent's work has merged, like any unstacked colony.
        assert!(matches!(
            boot_base(None, Some("parent"), Some(&parent(SessionStatus::Merged)), true),
            BootBase::Default
        ));
    }

    #[test]
    fn a_parent_that_cannot_lend_a_branch_yet_makes_a_fresh_boot_wait_or_refuse() {
        match boot_base(None, Some("parent"), Some(&parent(SessionStatus::Running)), true) {
            BootBase::Wait { colony } => assert_eq!(colony, "parent"),
            other => panic!("a running parent has not pushed yet: {other:?}"),
        }
        let reason = match boot_base(None, Some("parent"), Some(&parent(SessionStatus::Failed)), true) {
            BootBase::Refuse(reason) => reason,
            other => panic!("a failed parent can never provide a branch: {other:?}"),
        };
        assert!(reason.contains("parent") && reason.contains("failed"), "{reason}");
    }

    #[test]
    fn without_the_stack_flag_a_boot_waits_for_the_merge_not_the_push() {
        // The default queues for the parent's merge: even a pushed branch is not a base.
        match boot_base(None, Some("parent"), Some(&parent(SessionStatus::PrOpened)), false) {
            BootBase::Wait { colony } => assert_eq!(colony, "parent"),
            other => panic!("an open pull request is still work unmerged: {other:?}"),
        }
        // ...and a merged parent sends the boot to the default branch.
        assert!(matches!(
            boot_base(None, Some("parent"), Some(&parent(SessionStatus::Merged)), false),
            BootBase::Default
        ));
    }
}
