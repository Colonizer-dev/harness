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
    assert!(matches!(
        parent_state(&child(true), &[parent(SessionStatus::NoChanges)]),
        ParentState::Gone(_)
    ));
    assert!(matches!(
        parent_state(&child(false), &[parent(SessionStatus::Closed)]),
        ParentState::Gone(_)
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
