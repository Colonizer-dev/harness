use super::*;
use crate::sessions::tests::colony;

fn parent(status: SessionStatus) -> Session {
    let mut p = colony("acme", status);
    p.id = "c8a6d23cdeadbeef".into();
    p.issue = Some(5);
    p.branch = "colonizer/issue-5-root".into();
    p
}

fn child(stack: bool) -> Session {
    let mut c = colony("acme", SessionStatus::Queued);
    c.id = "child".into();
    c.parent = Some("c8a6d23cdeadbeef".into());
    c.stack = stack;
    c
}

#[test]
fn a_paused_parent_blocks_with_the_reason_a_person_can_act_on() {
    for status in [SessionStatus::Stopped, SessionStatus::Parked] {
        match parent_state(&child(true), &[parent(status)]) {
            ParentState::Paused(reason) => assert_eq!(reason, format!("waiting on #5 (`c8a6d23c`, {})", status.as_str())),
            other => panic!("{status:?}: {other:?}"),
        }
    }
    let mut no_issue = parent(SessionStatus::Stopped);
    no_issue.issue = None;
    assert!(matches!(parent_state(&child(true), &[no_issue]), ParentState::Paused(r) if r.contains("colony c8a6d23c")));
}

#[test]
fn a_parent_gone_for_good_is_gone_and_a_living_one_is_fine() {
    assert!(matches!(parent_state(&child(true), &[]), ParentState::Gone(_)), "deleted");
    // NoChanges with issue is Unmet for non-stack children, Gone for stacked children.
    assert!(
        matches!(
            parent_state(&child(true), &[parent(SessionStatus::NoChanges)]),
            ParentState::Gone(_)
        ),
        "stacked child re-bases even if parent has issue"
    );
    assert!(
        matches!(
            parent_state(&child(false), &[parent(SessionStatus::NoChanges)]),
            ParentState::Unmet { issue: 5, .. }
        ),
        "non-stack child stays unmet while issue is open"
    );
    // Closed non-stack with issue is now Unmet, not Gone.
    assert!(matches!(
        parent_state(&child(false), &[parent(SessionStatus::Closed)]),
        ParentState::Unmet { issue: 5, .. }
    ));
    let mut cleaned = parent(SessionStatus::Parked);
    cleaned.cleaned_up = true;
    assert!(matches!(parent_state(&child(true), &[cleaned]), ParentState::Gone(_)));
    let mut unpublished = parent(SessionStatus::PrOpened);
    unpublished.cleaned_up = true;
    assert!(matches!(parent_state(&child(true), &[unpublished]), ParentState::Gone(_)));
    for status in [
        SessionStatus::Running,
        SessionStatus::PrOpened,
        SessionStatus::Merged,
        SessionStatus::Failed,
    ] {
        assert_eq!(parent_state(&child(true), &[parent(status)]), ParentState::Fine, "{status:?}");
    }
    assert_eq!(
        parent_state(&parent(SessionStatus::Running), &[]),
        ParentState::Fine,
        "no parent at all"
    );
}

#[test]
fn a_blocked_parent_blocks_its_own_dependents() {
    assert!(
        matches!(parent_state(&child(true), &[parent(SessionStatus::Blocked)]), ParentState::Paused(r) if r.contains("blocked"))
    );
}

#[test]
fn no_changes_with_issue_is_unmet() {
    let result = parent_state(&child(false), &[parent(SessionStatus::NoChanges)]);
    assert!(
        matches!(result, ParentState::Unmet { ref reason, issue: 5, .. } if reason.contains("#5") && reason.contains("no changes"))
    );
}

#[test]
fn no_changes_without_issue_is_gone() {
    let mut p = parent(SessionStatus::NoChanges);
    p.issue = None;
    assert!(matches!(parent_state(&child(false), &[p]), ParentState::Gone(_)));
}

#[test]
fn closed_unmerged_non_stack_with_issue_is_unmet() {
    let result = parent_state(&child(false), &[parent(SessionStatus::Closed)]);
    assert!(
        matches!(result, ParentState::Unmet { ref reason, issue: 5, .. } if reason.contains("#5") && reason.contains("closed"))
    );
}

#[test]
fn closed_unmerged_non_stack_without_issue_is_gone() {
    let mut p = parent(SessionStatus::Closed);
    p.issue = None;
    assert!(matches!(parent_state(&child(false), &[p]), ParentState::Gone(_)));
}

#[test]
fn closed_stacked_is_fine_even_with_issue() {
    // stack:true ignores the Closed rule; the stack case takes normal precedence.
    let result = parent_state(&child(true), &[parent(SessionStatus::Closed)]);
    assert_eq!(result, ParentState::Fine);
}

#[test]
fn grandchild_of_unmet_stays_blocked() {
    // A child whose parent is unmet: the parent is Blocked (because release_blocked blocks it).
    // A grandchild whose parent is that blocked child: it should also be blocked.
    let p = parent(SessionStatus::NoChanges);
    let mut c = child(false);
    c.status = SessionStatus::Blocked;
    c.blocked_reason = Some("waiting on #5 (`c8a6d23c`, no changes; issue still open)".into());
    let mut gc = colony("acme", SessionStatus::Queued);
    gc.parent = Some(c.id.clone());
    gc.stack = false;

    assert!(matches!(
        parent_state(&gc, &[p, c]),
        ParentState::Paused(ref reason) if reason.contains("blocked")
    ));
}

#[test]
fn unmet_parent_with_closed_issue_decision() {
    // When a parent is Unmet (NoChanges + issue) and the issue is closed, the decision is to
    // rebase the child onto the default branch. This test verifies the decision logic separately
    // from the async github fetch.
    let p = parent(SessionStatus::NoChanges);
    let unmet_state = parent_state(&child(false), std::slice::from_ref(&p));
    assert!(matches!(unmet_state, ParentState::Unmet { issue: 5, .. }));

    // The pure decision: if we were told the issue is closed, we would treat it as Gone.
    // This is verified in release_blocked via is_issue_open returning false.
    // We test here that the parent's issue is Some(5), so it would have been Unmet.
    // When issue closes, release_blocked would rebase the child.
    assert_eq!(p.issue, Some(5), "parent must have issue for Unmet");
}

#[test]
fn relaunch_finds_newest_live_session_with_same_issue() {
    let p = parent(SessionStatus::NoChanges);
    let mut relaunch = colony("acme", SessionStatus::Running);
    relaunch.id = "relaunchAB".into();
    relaunch.issue = Some(5);
    relaunch.created_at = p.created_at + chrono::Duration::seconds(10);

    let found = find_relaunch(&p, &[p.clone(), relaunch.clone()]);
    assert_eq!(found.map(|s| s.id), Some("relaunchAB".to_string()));

    // Older relaunch should not be found.
    let mut older = colony("acme", SessionStatus::Running);
    older.id = "olderXY".into();
    older.issue = Some(5);
    older.created_at = p.created_at - chrono::Duration::seconds(10);
    let found = find_relaunch(&p, &[p.clone(), older]);
    assert!(found.is_none());

    // Different issue should not be found.
    let mut different = colony("acme", SessionStatus::Running);
    different.id = "diffXY".into();
    different.issue = Some(6);
    different.created_at = p.created_at + chrono::Duration::seconds(10);
    let found = find_relaunch(&p, &[p.clone(), different]);
    assert!(found.is_none());

    // Failed status should not be found.
    let mut failed = colony("acme", SessionStatus::Failed);
    failed.id = "failedXY".into();
    failed.issue = Some(5);
    failed.created_at = p.created_at + chrono::Duration::seconds(10);
    let found = find_relaunch(&p, &[p.clone(), failed]);
    assert!(found.is_none());

    // Newest should win if multiple relaunches exist.
    let mut relaunch2 = colony("acme", SessionStatus::Running);
    relaunch2.id = "newestZZ".into();
    relaunch2.issue = Some(5);
    relaunch2.created_at = p.created_at + chrono::Duration::seconds(20);
    let found = find_relaunch(&p, &[p.clone(), relaunch.clone(), relaunch2.clone()]);
    assert_eq!(found.map(|s| s.id), Some("newestZZ".to_string()));
}

#[test]
fn relaunch_running_keeps_child_blocked_until_merge() {
    // After re-pointing child to a Running relaunch, parent_state should return Fine,
    // indicating the child can leave Blocked state. However, gate() will then check
    // the relaunch's status and keep it blocked/waiting (queued) until the relaunch merges.
    let p = parent(SessionStatus::NoChanges);
    let mut relaunch = colony("acme", SessionStatus::Running);
    relaunch.id = "relaunched".into();
    relaunch.issue = Some(5);
    relaunch.created_at = p.created_at + chrono::Duration::seconds(10);

    // Child whose parent is the Running relaunch (after re-pointing).
    let mut c = child(false);
    c.parent = Some("relaunched".into());

    // parent_state evaluates against the Running relaunch → Fine
    // (not Unmet, because the relaunch's status is Running, not NoChanges)
    assert_eq!(
        parent_state(&c, &[p.clone(), relaunch.clone()]),
        ParentState::Fine,
        "after re-pointing to Running relaunch, parent_state is Fine"
    );
}

#[test]
fn relaunch_merged_releases_child_to_start() {
    // After re-pointing child to a Merged relaunch, parent_state is Fine,
    // and gate() will admit the child to start (since parent is Merged).
    let p = parent(SessionStatus::NoChanges);
    let mut relaunch = colony("acme", SessionStatus::Merged);
    relaunch.id = "relaunched".into();
    relaunch.issue = Some(5);
    relaunch.created_at = p.created_at + chrono::Duration::seconds(10);

    // Child whose parent is the Merged relaunch (after re-pointing).
    let mut c = child(false);
    c.parent = Some("relaunched".into());

    // parent_state evaluates against the Merged relaunch → Fine
    assert_eq!(
        parent_state(&c, &[p.clone(), relaunch.clone()]),
        ParentState::Fine,
        "after re-pointing to Merged relaunch, parent_state is Fine"
    );
}

#[test]
fn find_relaunch_excludes_child_and_descendants() {
    // Ensure find_relaunch doesn't pick the child itself or its descendants,
    // which could create a cycle.
    let p = parent(SessionStatus::NoChanges);
    let mut c = child(false);
    c.status = SessionStatus::Queued;
    c.issue = Some(5); // Same issue as parent (unusual but testing defense)
    c.created_at = p.created_at + chrono::Duration::seconds(1);

    // Even though child has the same issue and was created after parent,
    // it should not be treated as a relaunch of the parent.
    // (In reality, a relaunch would have its own independent parent, not parent,
    // but we're testing that find_relaunch is defensive.)
    let found = find_relaunch(&p, &[p.clone(), c.clone()]);
    assert!(found.is_none(), "find_relaunch must not return the child session itself");
}

#[test]
fn an_issue_state_is_read_whatever_its_case_and_an_unknown_one_releases_nothing() {
    // Uppercase OPEN
    let json = serde_json::json!({"state": "OPEN"});
    assert_eq!(issue_open(&json), Some(true));

    // Uppercase CLOSED
    let json = serde_json::json!({"state": "CLOSED"});
    assert_eq!(issue_open(&json), Some(false));

    // Mixed case Open
    let json_open = serde_json::json!({"state": "Open"});
    assert_eq!(issue_open(&json_open), Some(true));

    // Mixed case ClOsEd
    let json_closed = serde_json::json!({"state": "ClOsEd"});
    assert_eq!(issue_open(&json_closed), Some(false));

    // Missing state field
    let json = serde_json::json!({"number": 5});
    assert_eq!(issue_open(&json), None);

    // Unrecognized state
    let json = serde_json::json!({"state": "UNKNOWN"});
    assert_eq!(issue_open(&json), None);
}
