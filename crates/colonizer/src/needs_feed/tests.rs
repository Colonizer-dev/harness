use super::*;
use crate::sessions::tests::colony;
use serde_json::json;

fn failed(id: &str, issue: u64, age_h: i64, error: &str) -> Session {
    let mut s = colony("acme", SessionStatus::Failed);
    s.id = id.into();
    s.issue = Some(issue);
    s.error = Some(error.into());
    s.unseen_failure = true;
    s.created_at = Utc::now() - chrono::Duration::hours(age_h);
    s.updated_at = s.created_at;
    s
}

fn newer(id: &str, issue: u64, status: SessionStatus) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.issue = Some(issue);
    s
}

#[test]
fn a_failure_a_newer_colony_for_the_same_issue_has_overtaken_leaves_the_list() {
    for status in [SessionStatus::Queued, SessionStatus::Running, SessionStatus::Merged] {
        let sessions = vec![failed("old", 7, 80, "boom"), newer("new", 7, status)];
        assert_eq!(rows(&sessions, Utc::now()), 0, "{status:?}");
    }
    // A newer colony that itself failed does not hide it: one entry per issue remains (the newest).
    let sessions = vec![failed("old", 7, 80, "boom"), failed("newer", 7, 1, "boom again")];
    assert_eq!(rows(&sessions, Utc::now()), 1);
    // Another issue is unrelated.
    let sessions = vec![failed("old", 7, 80, "boom"), newer("new", 8, SessionStatus::Running)];
    assert_eq!(rows(&sessions, Utc::now()), 1);
}

#[test]
fn cascade_failures_never_count() {
    let sessions = vec![
        failed(
            "a",
            1,
            1,
            "colony `c8a6d23c` was stopped or parked, so it cannot be stacked on",
        ),
        failed("b", 2, 1, "colony `a` failed, so it has no branch to build on"),
    ];
    assert_eq!(rows(&sessions, Utc::now()), 0);
}

#[test]
fn old_abandoned_questions_fold_into_one_row() {
    let abandoned = |id: &str, issue: u64, age_h: i64| failed(id, issue, age_h, crate::queue::ABANDONED_QUESTION_REASON);
    let sessions = vec![
        abandoned("a", 1, 100),
        abandoned("b", 2, 90),
        abandoned("c", 3, 73),
        abandoned("fresh", 4, 5),
    ];
    assert_eq!(rows(&sessions, Utc::now()), 2, "one folded row plus the fresh one");
}

#[test]
fn one_row_per_issue_and_an_idle_parked_colony_is_not_a_row() {
    let mut a = newer("a", 3, SessionStatus::Running);
    a.attention = Some(json!({"reason": "stalled", "since": Utc::now(), "nudges": 1}));
    a.created_at = Utc::now() - chrono::Duration::hours(2);
    let mut b = newer("b", 3, SessionStatus::Idle);
    b.attention = Some(json!({"reason": "nudges_exhausted", "since": Utc::now(), "nudges": 3}));
    assert_eq!(rows(&[a, b], Utc::now()), 1, "deduped by issue");
    let mut parked = newer("p", 9, SessionStatus::Parked);
    parked.attention = Some(json!({"reason": crate::idle_park::IDLE_PARK_REASON, "since": Utc::now(), "nudges": 0}));
    assert_eq!(rows(&[parked], Utc::now()), 0);
}
