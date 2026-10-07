use super::*;
use crate::sessions::tests::colony;
use serde_json::json;

fn idle(id: &str, quiet_min: i64) -> Session {
    let mut s = colony("acme", SessionStatus::Idle);
    s.id = id.into();
    s.git_admin_dir = Some("git".into());
    s.updated_at = Utc::now() - chrono::Duration::minutes(quiet_min);
    s
}

const FIFTEEN: chrono::Duration = chrono::Duration::minutes(15);

#[test]
fn idle_held_and_flagged_colonies_are_due_after_the_timeout() {
    let now = Utc::now();
    assert!(idle_park_due(&idle("quiet", 16), now, FIFTEEN));
    assert!(!idle_park_due(&idle("fresh", 14), now, FIFTEEN));
    for reason in ["autopilot_held", "nudges_exhausted", "stalled", "control_defeat"] {
        let mut s = idle(reason, 20);
        s.attention = Some(json!({"reason": reason, "since": now, "nudges": 1}));
        assert!(idle_park_due(&s, now, FIFTEEN), "{reason}");
    }
}

#[test]
fn a_question_or_another_status_is_never_parked_here() {
    let now = Utc::now();
    let mut asking = idle("asking", 60);
    asking.attention = Some(json!({"reason": "waiting_for_answer", "since": now, "nudges": 0}));
    assert!(!idle_park_due(&asking, now, FIFTEEN));
    for status in [
        SessionStatus::WaitingForAnswer,
        SessionStatus::Running,
        SessionStatus::Publishing,
        SessionStatus::Parked,
    ] {
        let mut s = idle("other", 60);
        s.status = status;
        assert!(!idle_park_due(&s, now, FIFTEEN), "{status:?}");
    }
}

#[test]
fn the_pr_description_message_is_sent_once_and_only_for_that_wait() {
    let mut s = idle("a", 1);
    s.autopilot = true;
    assert!(wants_pr_rewrite(PR_NOT_WRITTEN, &s));
    assert!(!wants_pr_rewrite("a question is open", &s));
    s.pr_rewrite_nudged = true;
    assert!(!wants_pr_rewrite(PR_NOT_WRITTEN, &s), "asked once already");
    assert!(PR_REWRITE_MESSAGE.contains("/harness/out/pr.md"));
}

#[tokio::test]
async fn an_idle_colony_parks_after_the_timeout_and_its_slot_is_reused() {
    let root = std::env::temp_dir().join(format!("colonizer-idle-park-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let mut sessions = vec![idle("quiet", 30), idle("fresh", 2)];
    let mut waiting = colony("acme", SessionStatus::Queued);
    waiting.id = "waiting".into();
    sessions.push(waiting);
    for id in ["quiet", "fresh", "waiting"] {
        tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
    }
    *app.sessions.write().await = sessions;
    assert!(
        !crate::queue::has_room(&app.sessions.read().await, "acme", "acme/repo", 2, None, 32),
        "two idle colonies fill two slots"
    );
    park_idle(&app, FIFTEEN).await;
    let after = app.sessions.read().await.clone();
    let quiet = after.iter().find(|s| s.id == "quiet").unwrap();
    assert_eq!(quiet.status, SessionStatus::Parked);
    assert_eq!(quiet.parked.as_ref().unwrap().reason, IDLE_PARK_REASON);
    assert!(
        quiet.git_admin_dir.is_some() && !quiet.cleaned_up,
        "the worktree is kept for Resume"
    );
    assert_eq!(after.iter().find(|s| s.id == "fresh").unwrap().status, SessionStatus::Idle);
    assert!(
        crate::queue::has_room(&after, "acme", "acme/repo", 2, None, 32),
        "the freed slot is there for the queue"
    );
    assert!(!crate::push::needs_you(quiet), "parked for idleness asks nothing of a person");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_open_question_keeps_its_colony_unparked() {
    let root = std::env::temp_dir().join(format!("colonizer-idle-park-q-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    *app.sessions.write().await = vec![idle("asking", 60)];
    tokio::fs::create_dir_all(app.session_dir("asking")).await.unwrap();
    let rt = app.runtime("asking").await;
    *rt.open_question.lock().await = Some(("q1".into(), vec![], crate::protocol::QuestionRisk::ReadOnly));
    park_idle(&app, FIFTEEN).await;
    assert_eq!(app.session("asking").await.unwrap().status, SessionStatus::Idle);
    let _ = std::fs::remove_dir_all(root);
}
