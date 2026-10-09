use super::defeat::{Defeat, REACH_WINDOW_MINUTES, REPEATED_DENIALS, reaches};
use super::*;
use crate::boundary::Boundary;

const SETTINGS: WatchdogSettings = WatchdogSettings {
    enabled: true,
    stall_minutes: 15,
    max_nudges: 2,
    waiting_minutes: 30,
};

fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minutes)
}

#[test]
fn working_colonies_are_nudged_once_per_interval_then_flagged() {
    let mut activity = Activity::new(at(0));
    assert_eq!(
        decide(&SETTINGS, at(14), Observed::Working, &activity, None),
        Decision::Nothing
    );
    assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);

    activity.nudges = 1;
    activity.last_nudge = Some(at(15));
    assert_eq!(
        decide(&SETTINGS, at(20), Observed::Working, &activity, Some("stalled")),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(30), Observed::Working, &activity, Some("stalled")),
        Decision::Nudge
    );

    activity.nudges = 2;
    activity.last_nudge = Some(at(30));
    assert_eq!(
        decide(&SETTINGS, at(44), Observed::Working, &activity, Some("stalled")),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(45), Observed::Working, &activity, Some("stalled")),
        Decision::Flag("nudges_exhausted")
    );
    assert_eq!(
        decide(&SETTINGS, at(90), Observed::Working, &activity, Some("nudges_exhausted")),
        Decision::Nothing
    );
}

#[test]
fn progress_after_a_nudge_restarts_the_clock() {
    let activity = Activity {
        last: at(20),
        admitted_at: at(20),
        nudges: 1,
        last_nudge: Some(at(15)),
        question_since: None,
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 0,
        denied_since: None,
        last_denial: None,
        boundaries: BoundaryTrail::default(),
    };
    assert_eq!(
        decide(&SETTINGS, at(34), Observed::Working, &activity, None),
        Decision::Nothing
    );
    assert_eq!(decide(&SETTINGS, at(35), Observed::Working, &activity, None), Decision::Nudge);
}

#[test]
fn unanswered_questions_are_flagged_not_nudged() {
    let activity = Activity {
        last: at(0),
        admitted_at: at(0),
        nudges: 0,
        last_nudge: None,
        question_since: Some(at(0)),
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 0,
        denied_since: None,
        last_denial: None,
        boundaries: BoundaryTrail::default(),
    };
    assert_eq!(
        decide(&SETTINGS, at(29), Observed::WaitingForAnswer, &activity, None),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(30), Observed::WaitingForAnswer, &activity, None),
        Decision::Flag("waiting_for_answer")
    );
    assert_eq!(
        decide(
            &SETTINGS,
            at(60),
            Observed::WaitingForAnswer,
            &activity,
            Some("waiting_for_answer")
        ),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(61), Observed::Other, &activity, Some("waiting_for_answer")),
        Decision::Clear
    );
}

/// A colony whose agent never linked goes straight to the flag, never to a nudge (issue #760).
/// The boot's own length is *not* charged against it; a boot that never finished gets the far
/// longer floor. See [`starting_reference`].
#[test]
fn a_starting_colony_is_flagged_never_nudged() {
    // A boot that took 40 minutes (a cold image pull) and then finished: the link has had five
    // minutes, not forty — the false positive the deadline exists to prevent.
    let activity = Activity::new(at(0));
    let slow_boot = Observed::Starting {
        boot_total_ms: Some(40 * 60 * 1000),
    };
    assert_eq!(decide(&SETTINGS, at(45), slow_boot, &activity, None), Decision::Nothing);
    // The boot finished at minute 40, so the link's own 15-minute deadline falls at minute 55 —
    // not at minute 15, and not measured from the admission the cold pull was charging.
    assert_eq!(decide(&SETTINGS, at(54), slow_boot, &activity, None), Decision::Nothing);
    assert_eq!(
        decide(&SETTINGS, at(55), slow_boot, &activity, None),
        Decision::Flag("stalled"),
        "15 min after the boot finished the link has had its deadline"
    );
    // A boot still running — no `total_ms` yet — gets the whole floor: the pull in it is never
    // mistaken for a stalled agent, and a wedged boot still gets flagged.
    let booting = Observed::Starting { boot_total_ms: None };
    assert_eq!(decide(&SETTINGS, at(44), booting, &activity, None), Decision::Nothing);
    assert_eq!(decide(&SETTINGS, at(45), booting, &activity, None), Decision::Flag("stalled"));
    assert_eq!(
        decide(&SETTINGS, at(90), booting, &activity, Some("stalled")),
        Decision::Nothing,
        "the flag stands rather than repeating"
    );
    // No nudge is ever returned for this state, whatever the budget.
    for minutes in [15, 30, 600] {
        assert!(
            !matches!(decide(&SETTINGS, at(minutes), booting, &activity, None), Decision::Nudge),
            "a `starting` colony is never nudged, at {minutes} min"
        );
    }
    // Somebody else's flag is the more specific story and is left on the record.
    assert_eq!(
        decide(
            &SETTINGS,
            at(30),
            Observed::Starting { boot_total_ms: None },
            &activity,
            Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON)
        ),
        Decision::Nothing
    );
    // A colony admitted only a few minutes ago is inside its window whatever happened after.
    let recent = Activity::new(at(20));
    assert_eq!(
        decide(
            &SETTINGS,
            at(34),
            Observed::Starting { boot_total_ms: Some(0) },
            &recent,
            None
        ),
        Decision::Nothing
    );
    // The point of `admitted_at`: the retrying colony in issue #760 reset `since` to now on
    // every tick before this.
    let mut busy = Activity::new(at(0));
    busy.note_gateway_busy(at(60));
    assert_eq!(
        busy.admitted_at,
        at(0),
        "gateway traffic moves the progress clock, not this one"
    );
    assert_eq!(decide(&SETTINGS, at(60), booting, &busy, None), Decision::Flag("stalled"));
}

/// A colony with a stalled runtime and its nudges spent, under a watchdog of 15 min / 2 nudges.
async fn stalled_app(name: &str, status: SessionStatus) -> (Shared, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-watchdog-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/orgs.json"),
        r#"{"acme": {"watchdog": {"enabled": true, "stall_minutes": 15, "max_nudges": 2}}}"#,
    )
    .unwrap();
    let app = crate::tests::test_app(&root);
    let mut s = crate::sessions::tests::colony("acme", status);
    s.id = "w1".into();
    s.allowed_providers = Some(vec!["bailian".into()]);
    app.sessions.write().await.push(s);
    std::fs::create_dir_all(app.session_dir("w1")).unwrap();
    let rt = app.runtime("w1").await;
    {
        let mut activity = rt.activity.lock().await;
        activity.last = Utc::now() - Duration::hours(2);
        activity.nudges = 2;
        activity.last_nudge = Some(Utc::now() - Duration::hours(1));
    }
    (app, root)
}

/// The fixture's colony admitted two hours ago with no nudges spent — overdue on the boot clock
/// whatever its `last` says. Returns the admission stamp so a test can say what it expects the
/// flag to be dated from.
async fn stale_starting(app: &Shared) -> DateTime<Utc> {
    let rt = app.runtime("w1").await;
    let mut a = rt.activity.lock().await;
    a.nudges = 0;
    a.last_nudge = None;
    a.admitted_at = Utc::now() - Duration::hours(2);
    a.admitted_at
}

/// The `since` an attention record carries, read back off the JSON the cockpit reads.
fn since_of(attention: &Value) -> DateTime<Utc> {
    let s = attention["since"].as_str().expect("since is stamped");
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// The final "this colony needs you" raises the attention flag the cockpit reads, not only a
/// log line (issue #760), and gateway traffic alone does not take it down again.
#[tokio::test]
async fn the_final_needs_you_sets_the_attention_flag() {
    let (app, root) = stalled_app("flag", SessionStatus::Running).await;
    check_all(&app).await;
    let attention = app.session("w1").await.unwrap().attention.expect("flagged");
    assert_eq!(attention["reason"], "nudges_exhausted");
    assert_eq!(attention["nudges"], 2);
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter()
            .any(|l| l["message"].as_str().is_some_and(|m| m.contains("this colony needs you"))),
        "and the log says so: {logs:?}"
    );
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.unwrap()["reason"],
        "nudges_exhausted",
        "the flag stays up"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Issue #981: a `waiting_for_answer` record with no question tracked is stuck — the answer was
/// taken and no runner status followed. The watchdog reconciles it to `idle`, on the record and
/// in the log, rather than leaving a colony the cockpit flags as needing you while `ask` can
/// answer nothing on it.
#[tokio::test]
async fn a_waiting_for_answer_with_no_question_is_reconciled_to_idle() {
    let (app, root) = stalled_app("reconcile", SessionStatus::WaitingForAnswer).await;
    assert_eq!(
        app.session("w1").await.unwrap().status,
        SessionStatus::WaitingForAnswer,
        "starts stuck"
    );
    check_all(&app).await;
    let s = app.session("w1").await.unwrap();
    assert_eq!(s.status, SessionStatus::Idle, "the stale wait is reconciled");
    assert!(s.attention.is_none(), "and its stale attention flag cleared");
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter()
            .any(|l| l["message"].as_str().is_some_and(|m| m.contains("no question is pending"))),
        "the log says why: {logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The reconciliation must leave a colony whose question is genuinely open: its wait is real,
/// and `ask` can answer it.
#[tokio::test]
async fn a_waiting_for_answer_with_an_open_question_is_left_alone() {
    let (app, root) = stalled_app("keep", SessionStatus::WaitingForAnswer).await;
    *app.runtime("w1").await.open_question.lock().await = Some((
        "q1".into(),
        vec![json!({"question": "Push now?"})],
        crate::protocol::QuestionRisk::WorkspaceWrite,
    ));
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().status,
        SessionStatus::WaitingForAnswer,
        "a real question keeps its status"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// With the recovery point off, a nudge is byte-for-byte the log line it always was (issue #586):
/// the decision point must not change what a colony sees when it is not switched on.
#[tokio::test]
async fn an_off_recovery_point_nudges_with_the_original_log_line() {
    let (app, root) = stalled_app("nudge-line", SessionStatus::Running).await;
    {
        let rt = app.runtime("w1").await;
        let mut activity = rt.activity.lock().await;
        activity.nudges = 0;
        activity.last_nudge = None;
    }
    check_all(&app).await;
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.contains("no progress for 15 min, nudged the agent (1/2)"))),
        "the off-mode nudge line is unchanged: {logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose recent calls were all denied is nudged (issue #609), and the nudge names the
/// denied boundary with a log line that says it was a hint loop.
#[tokio::test]
async fn a_hint_loop_is_nudged_naming_the_denial() {
    let (app, root) = stalled_app("hint-loop", SessionStatus::Running).await;
    {
        let rt = app.runtime("w1").await;
        let mut activity = rt.activity.lock().await;
        activity.nudges = 0;
        activity.last_nudge = None;
        activity.denials = 3;
        activity.denied_since = Some(Utc::now() - Duration::hours(2));
        activity.last_denial = Some(("egress".to_string(), "denied host example.com".to_string()));
    }
    check_all(&app).await;
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.contains("3 tool calls denied in a row (egress), nudged the agent (1/2)"))),
        "the log says it was a hint loop: {logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony blocked on an exhausted provider gets the quota flag and is not nudged: another
/// request cannot be answered until the plan resets (issue #760).
#[tokio::test]
async fn a_quota_blocked_colony_is_flagged_with_the_quota_reason_not_nudged() {
    let (app, root) = stalled_app("quota", SessionStatus::Running).await;
    app.gateway
        .mark_quota_exhausted("bailian", None, Some(Utc::now().timestamp() + 600));
    app.gateway.note_colony_quota("w1", "bailian");
    check_all(&app).await;
    let attention = app.session("w1").await.unwrap().attention.expect("flagged");
    assert_eq!(attention["reason"], crate::provider_quota::QUOTA_EXHAUSTED_REASON);
    assert_eq!(attention["provider"], "bailian");
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose boot finished but whose agent never linked is flagged, not nudged, and the
/// log line is the one that says the agent never linked, not the one that blames an unfinished
/// boot (issue #760).
#[tokio::test]
async fn a_starting_colony_with_no_agent_turn_is_flagged_not_nudged() {
    let (app, root) = stalled_app("boot", SessionStatus::Starting).await;
    stale_starting(&app).await;
    // The boot ran for ten minutes and finished: the link has then had the fixture's two hours
    // to come up.
    app.update_session("w1", |s| {
        s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
        true
    })
    .await;
    check_all(&app).await;
    let s = app.session("w1").await.unwrap();
    let attention = s.attention.expect("flagged");
    assert_eq!(s.status, SessionStatus::Starting, "the watchdog does not move the status");
    assert_eq!(attention["reason"], "stalled");
    assert_eq!(attention["nudges"], 0, "and it did not nudge");
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.contains("its agent still has not linked"))),
        "the log names the link that never came, not a boot still running: {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|l| l["message"].as_str().is_some_and(|m| m.contains("this colony needs you"))),
        "and says the colony needs you: {logs:?}"
    );
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.unwrap()["reason"],
        "stalled",
        "the flag stands rather than repeating"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A boot still inside its deadline is left alone: the long boot must not be charged against the
/// link (issue #760).
#[tokio::test]
async fn a_starting_colony_inside_the_boot_window_is_left_alone() {
    let (app, root) = stalled_app("boot-grace", SessionStatus::Starting).await;
    {
        let rt = app.runtime("w1").await;
        let mut activity = rt.activity.lock().await;
        activity.nudges = 0;
        // A cold image pull: forty minutes into a boot that has not finished yet, and well past
        // the fixture's `stall_minutes` of 15 — under the old clock that was the false positive,
        // an agent reported dead while it was still downloading gigabytes. `last` is set
        // alongside so nothing else sees a stale clock either.
        activity.admitted_at = Utc::now() - Duration::minutes(40);
        activity.last = activity.admitted_at;
    }
    check_all(&app).await;
    assert!(
        app.session("w1").await.unwrap().attention.is_none(),
        "forty minutes into a cold boot, that boot is still a boot"
    );
    // The same colony once the boot has finished and the link is overdue: now it is a flag.
    app.update_session("w1", |s| {
        s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
        true
    })
    .await;
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.expect("flagged")["reason"],
        "stalled"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Gateway traffic must not take down the boot flag (issue #760): a `starting` colony whose
/// agent is evidently making requests but never linked still needs a person, and
/// `note_gateway_busy` has already restarted its clock.
#[tokio::test]
async fn gateway_traffic_does_not_clear_a_starting_colonys_stall_flag() {
    let (app, root) = stalled_app("busy-boot", SessionStatus::Starting).await;
    stale_starting(&app).await;
    app.update_session("w1", |s| {
        s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
        true
    })
    .await;
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.expect("flagged")["reason"],
        "stalled"
    );
    // An in-flight request, the same shape `gateway/tests.rs` uses via `Counted`.
    let counter = app.gateway.colony_counter("w1");
    counter.fetch_add(1, Ordering::SeqCst);
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.expect("the flag stands")["reason"],
        "stalled",
        "gateway traffic must not clear a stuck boot's flag"
    );
    counter.fetch_sub(1, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(root);
}

/// The narrowness of that exemption: a *different* reason's flag on a `starting` colony is
/// still cleared under gateway traffic, as it was before the boot arm existed.
#[tokio::test]
async fn gateway_traffic_still_clears_another_reason_on_a_starting_colony() {
    let (app, root) = stalled_app("busy-other", SessionStatus::Starting).await;
    app.update_session("w1", |s| {
        s.attention = Some(json!({ "reason": "agent_failed" }));
        true
    })
    .await;
    let counter = app.gateway.colony_counter("w1");
    counter.fetch_add(1, Ordering::SeqCst);
    check_all(&app).await;
    assert!(
        app.session("w1").await.unwrap().attention.is_none(),
        "only this watchdog's own boot flag is exempt"
    );
    counter.fetch_sub(1, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(root);
}

/// The regression for the clock the exemption could not create (issue #760): this is the colony
/// the issue describes — an agent that keeps issuing requests and keeps getting refused, whose
/// status never leaves `starting` and whose event stream never links. `note_gateway_busy` runs
/// before `decide` on every tick and moves `Activity::last` to now, so a `starting` window read
/// off `last` restarts every tick and the flag can never be raised. It reads
/// `Activity::admitted_at` instead.
#[tokio::test]
async fn sustained_gateway_traffic_does_not_hide_a_stuck_boot_forever() {
    // No `boot_timing` at all: the boot never finished, so the wider `BOOT_DEADLINE_MINUTES`
    // floor applies, and two hours is well past it.
    let (app, root) = stalled_app("busy-forever", SessionStatus::Starting).await;
    stale_starting(&app).await;
    // The request in flight, held across every tick below — the shape `gateway/tests.rs` uses
    // via `Counted`, which is private to the gateway module.
    let counter = app.gateway.colony_counter("w1");
    counter.fetch_add(1, Ordering::SeqCst);
    check_all(&app).await;
    let attention = app
        .session("w1")
        .await
        .unwrap()
        .attention
        .expect("flagged despite the traffic");
    assert_eq!(attention["reason"], "stalled");
    assert!(
        app.runtime("w1").await.activity.lock().await.last > Utc::now() - Duration::minutes(1),
        "the gateway clock really did move, so the flag cannot have come off it"
    );
    // The next tick, with the request *still* in flight, must not take it down.
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.expect("the flag stands")["reason"],
        "stalled",
        "sustained traffic must not postpone the flag a second time"
    );
    counter.fetch_sub(1, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(root);
}

/// The `since` on the record is the clock the decision was made on (issue #760), so the cockpit
/// says the colony has needed someone since the flag became due rather than since the last thing
/// the gateway happened to see.
#[tokio::test]
async fn a_starting_colonys_since_is_the_window_the_flag_was_decided_on() {
    let (app, root) = stalled_app("since", SessionStatus::Starting).await;
    let admitted = stale_starting(&app).await;
    // A progress clock at a *different*, much later instant, so the assertions can tell the two
    // apart: `progress_reference` would report this one.
    app.runtime("w1").await.activity.lock().await.last = Utc::now() - Duration::minutes(30);
    // A ten-minute boot: the link's window opened at `admitted_at + 600_000`.
    app.update_session("w1", |s| {
        s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
        true
    })
    .await;
    check_all(&app).await;
    let attention = app.session("w1").await.unwrap().attention.expect("flagged");
    assert_eq!(attention["reason"], "stalled");
    assert_eq!(
        since_of(&attention),
        admitted + Duration::milliseconds(600_000),
        "the boot-end reference, not the admission instant and not the progress clock"
    );
    // And the same for the boot-unfinished case, where the window opens at admission itself.
    let (app2, root2) = stalled_app("since-boot", SessionStatus::Starting).await;
    let admitted2 = stale_starting(&app2).await;
    check_all(&app2).await;
    let attention = app2.session("w1").await.unwrap().attention.expect("flagged");
    assert_eq!(since_of(&attention), admitted2);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[test]
fn disabled_watchdog_clears_only_its_own_attention() {
    let settings = WatchdogSettings {
        enabled: false,
        ..SETTINGS
    };
    let activity = Activity::new(at(0));
    assert_eq!(
        decide(&settings, at(500), Observed::Working, &activity, None),
        Decision::Nothing
    );
    assert_eq!(
        decide(&settings, at(500), Observed::Working, &activity, Some("stalled")),
        Decision::Clear
    );
    assert_eq!(
        decide(&settings, at(500), Observed::Other, &activity, Some("autopilot_held")),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(500), Observed::Other, &activity, Some("autopilot_held")),
        Decision::Nothing
    );
}

#[test]
fn a_turn_is_finished_only_when_the_runner_is_quiet() {
    let now = at(0);
    let stale = now - Duration::seconds(120);
    assert!(turn_end_due(now, Some(stale), 0, false, false), "the grace is met");
    assert!(
        !turn_end_due(now, Some(now - Duration::seconds(119)), 0, false, false),
        "inside the grace it is left alone"
    );
    assert!(!turn_end_due(now, None, 0, false, false), "no final answer to finish");
    assert!(!turn_end_due(now, Some(stale), 1, false, false), "a tool call is in flight");
    assert!(!turn_end_due(now, Some(stale), 0, true, false), "a question is open");
    assert!(
        !turn_end_due(now, Some(stale), 0, false, true),
        "a gateway request is in flight"
    );
}

#[test]
fn only_a_running_runner_is_read_as_alive() {
    assert!(runner_running(r#"{"agent":{"running":true,"state":"working"}}"#));
    assert!(!runner_running(r#"{"agent":{"running":false,"state":"idle"}}"#));
    assert!(!runner_running("{}"));
    assert!(!runner_running("not json"));
}

/// A colony with agentd answering `/v1/health` with `body` on a local port, its token on disk,
/// and its stall clock quiet, so only the turn-end recovery is under test. The probe walks the
/// real path (`sessions::agentd::agentd_http`), not a stub.
async fn app_with_agentd(name: &str, body: &str) -> (Shared, std::path::PathBuf) {
    let (app, root) = stalled_app(name, SessionStatus::Running).await;
    {
        let rt = app.runtime("w1").await;
        let mut activity = rt.activity.lock().await;
        activity.last = Utc::now();
        activity.nudges = 0;
        activity.last_nudge = None;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let response = response.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let vm = app.session_dir("w1").join("vm");
    std::fs::create_dir_all(&vm).unwrap();
    std::fs::write(vm.join("token"), "test-token\n").unwrap();
    app.update_session("w1", |x| x.local_port = Some(port)).await;
    (app, root)
}

/// Acceptance (issue #878): a final answer the runner never ended, with agentd still answering,
/// is finished — the `watchdog_turn_end` event is recorded and the turn-end path runs.
#[tokio::test]
async fn a_final_answer_the_runner_never_ended_is_finished() {
    let (app, root) = app_with_agentd("finish", r#"{"agent":{"running":true,"state":"working"}}"#).await;
    app.update_session("w1", |x| x.autopilot = true).await;
    let rt = app.runtime("w1").await;
    crate::events::handle_agent_event(
        &app,
        "w1",
        &rt,
        r#"{"seq":1,"type":"assistant_text","message_id":"m","block_index":0,"text":"all done"}"#,
    )
    .await;
    // Two minutes pass with the runner silent: the final answer's clock is what the watchdog reads.
    rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
    check_all(&app).await;

    assert!(rt.final_text_at.lock().await.is_none(), "the claim is spent");
    let events = std::fs::read_to_string(app.session_dir("w1").join("events.jsonl")).unwrap();
    assert!(
        events.contains(r#""type":"watchdog_turn_end""#),
        "the end is on the record: {events}"
    );
    // The turn-end path ran: its autopilot decision is in the colony's log.
    let logs = rt.logs.lock().await.clone();
    assert!(
        logs.iter()
            .any(|l| l["message"].as_str().is_some_and(|m| m.contains("finishing the turn"))),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.contains("autopilot: not publishing yet"))),
        "the ordinary turn-end path ran: {logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Acceptance: a tool call in flight is not an ended turn, so it is left alone.
#[tokio::test]
async fn a_tool_call_in_flight_holds_the_turn_open() {
    let (app, root) = app_with_agentd("tool", r#"{"agent":{"running":true,"state":"working"}}"#).await;
    let rt = app.runtime("w1").await;
    rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
    rt.open_tool_calls.lock().await.insert("toolu_1".into());
    check_all(&app).await;
    assert!(
        rt.final_text_at.lock().await.is_some(),
        "not finished while a call is in flight"
    );
    assert!(
        !std::fs::read_to_string(app.session_dir("w1").join("events.jsonl"))
            .unwrap_or_default()
            .contains("watchdog_turn_end")
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Acceptance: a request in flight through the gateway is progress, not a wedge.
#[tokio::test]
async fn a_gateway_request_in_flight_holds_the_turn_open() {
    let (app, root) = app_with_agentd("busy", r#"{"agent":{"running":true,"state":"working"}}"#).await;
    let rt = app.runtime("w1").await;
    rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
    app.gateway.colony_counter("w1").fetch_add(1, Ordering::SeqCst);
    check_all(&app).await;
    assert!(
        rt.final_text_at.lock().await.is_some(),
        "not finished while the gateway is busy"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Acceptance: the probe can take seconds, and a busy agent is not interrupted. If the runner
/// starts a tool call while the probe is in flight, the end is not synthesised even though the
/// probe answers that the runner is alive — the state is re-read after the probe.
#[tokio::test]
async fn a_tool_call_started_during_the_probe_holds_the_turn_open() {
    let (app, root) = stalled_app("race", SessionStatus::Running).await;
    let rt = app.runtime("w1").await;
    {
        let mut activity = rt.activity.lock().await;
        activity.last = Utc::now();
        activity.nudges = 0;
        activity.last_nudge = None;
        rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
    }
    // A health stub that opens a tool call on the real Runtime *before* it answers, standing in
    // for the runner beginning work during the probe's window.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = r#"{"agent":{"running":true,"state":"working"}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let stub_rt = rt.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let response = response.clone();
            let rt = stub_rt.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                rt.open_tool_calls.lock().await.insert("toolu_race".into());
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let vm = app.session_dir("w1").join("vm");
    std::fs::create_dir_all(&vm).unwrap();
    std::fs::write(vm.join("token"), "test-token\n").unwrap();
    app.update_session("w1", |x| x.local_port = Some(port)).await;

    check_all(&app).await;

    assert!(
        rt.final_text_at.lock().await.is_some(),
        "the claim is kept: the runner started working during the probe"
    );
    assert!(
        !std::fs::read_to_string(app.session_dir("w1").join("events.jsonl"))
            .unwrap_or_default()
            .contains("watchdog_turn_end"),
        "no end is synthesised for a busy runner"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose agentd is gone is wedged, not finished: the recovery keeps the final answer so
/// the next tick can retry, logs that it could not run once (not once per tick), and leaves the
/// colony to the stall handling rather than restarting it.
#[tokio::test]
async fn an_unreachable_agentd_leaves_the_turn_to_the_stall_handling() {
    let (app, root) = stalled_app("wedged", SessionStatus::Running).await;
    let rt = app.runtime("w1").await;
    {
        let mut activity = rt.activity.lock().await;
        activity.last = Utc::now();
        activity.nudges = 0;
        activity.last_nudge = None;
        rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
    }
    // No token file and no local port: dial_agentd fails at once, as an unreachable agentd would.
    check_all(&app).await;
    check_all(&app).await;
    assert!(
        rt.final_text_at.lock().await.is_some(),
        "a probe that failed leaves the claim for the next tick to retry"
    );
    let logs = rt.logs.lock().await.clone();
    assert_eq!(
        logs.iter()
            .filter(|l| l["message"].as_str().is_some_and(|m| m.contains("agentd is not answering")))
            .count(),
        1,
        "logged once per final answer, not once per tick: {logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A hint loop is nudged on the progress that preceded it: a retried call that moved `last`
/// forward does not hold the nudge off (issue #609).
#[test]
fn a_hint_loop_is_nudged_on_the_progress_before_the_loop() {
    let activity = Activity {
        last: at(20),
        admitted_at: at(20),
        nudges: 0,
        last_nudge: None,
        question_since: None,
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 2,
        denied_since: Some(at(0)),
        last_denial: Some(("egress".to_string(), "denied host example.com".to_string())),
        boundaries: BoundaryTrail::default(),
    };
    assert_eq!(
        decide(&SETTINGS, at(14), Observed::Working, &activity, None),
        Decision::Nothing
    );
    assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);
}

/// One denial — a hiccup — leaves the clock on `last`, and a cleared streak does too: the loop
/// threshold is what switches the reference, not any denial at all (issue #609).
#[test]
fn one_denial_or_a_cleared_streak_keeps_the_normal_clock() {
    let one = Activity {
        last: at(10),
        admitted_at: at(10),
        nudges: 0,
        last_nudge: None,
        question_since: None,
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 1,
        denied_since: Some(at(0)),
        last_denial: Some(("read_only".to_string(), "path is read-only".to_string())),
        boundaries: BoundaryTrail::default(),
    };
    assert_eq!(decide(&SETTINGS, at(24), Observed::Working, &one, None), Decision::Nothing);
    assert_eq!(decide(&SETTINGS, at(25), Observed::Working, &one, None), Decision::Nudge);

    let cleared = Activity::new(at(10));
    assert_eq!(
        decide(&SETTINGS, at(24), Observed::Working, &cleared, None),
        Decision::Nothing
    );
    assert_eq!(decide(&SETTINGS, at(25), Observed::Working, &cleared, None), Decision::Nudge);
}

/// The hint-loop nudge names the denial's class and hint, and `Activity::hint_loop` names the
/// loop only at the threshold (issue #609).
#[test]
fn the_hint_loop_nudge_names_the_denial() {
    let text = hint_loop_text(3, "egress", "denied host example.com");
    assert!(text.contains("your last 3 tool calls were denied"), "{text}");
    assert!(text.contains("egress: denied host example.com"), "{text}");

    let mut activity = Activity::new(at(0));
    activity.denials = 1;
    activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
    assert!(activity.hint_loop().is_none(), "one denial is not a loop");
    activity.denials = 2;
    assert_eq!(
        activity.hint_loop(),
        Some((2, "egress", "denied")),
        "two in a row, with the class and hint to name"
    );
}

/// After the nudge cap a hint loop flags `nudges_exhausted` like any stall (issue #609).
#[test]
fn a_hint_loop_flags_nudges_exhausted_after_the_cap() {
    let activity = Activity {
        last: at(60),
        admitted_at: at(60),
        nudges: 2,
        last_nudge: Some(at(30)),
        question_since: None,
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 3,
        denied_since: Some(at(0)),
        last_denial: Some(("tool_disabled".to_string(), "tool is not allowed".to_string())),
        boundaries: BoundaryTrail::default(),
    };
    assert_eq!(
        decide(&SETTINGS, at(44), Observed::Working, &activity, Some("stalled")),
        Decision::Nothing
    );
    assert_eq!(
        decide(&SETTINGS, at(45), Observed::Working, &activity, Some("stalled")),
        Decision::Flag("nudges_exhausted")
    );
}

/// `note_denials` keeps the streak: a denial extends it (its `denied_since` the progress before
/// the loop began), an unclassified error or an unrelated line leaves it, and a success or a
/// real break in the loop — a person's message, a question, a turn end — clears it (issue #609).
#[test]
fn a_denial_streak_is_noted_and_a_break_clears_it() {
    let mut activity = Activity::new(at(0));
    let denial = json!({
        "type": "tool_result",
        "tool_call_id": "t",
        "output": "blocked",
        "is_error": true,
        "denial": {"class": "egress", "hint": "denied host example.com"},
    });
    assert!(
        note_denials(&mut activity, "tool_result", &denial),
        "a denial is not progress"
    );
    assert_eq!(activity.denials, 1);
    assert_eq!(activity.denied_since, Some(at(0)));
    assert_eq!(
        activity.last_denial,
        Some(("egress".to_string(), "denied host example.com".to_string()))
    );

    activity.last = at(5);
    assert!(note_denials(&mut activity, "tool_result", &denial));
    assert_eq!(activity.denials, 2);
    assert_eq!(activity.denied_since, Some(at(0)), "the streak's clock is where it began");

    let plain = json!({"type": "tool_result", "tool_call_id": "t", "output": "boom", "is_error": true});
    assert!(!note_denials(&mut activity, "tool_result", &plain));
    assert_eq!(
        activity.denials, 2,
        "an unclassified error neither extends nor clears the streak"
    );

    assert!(!note_denials(&mut activity, "log", &json!({"type": "log", "message": "hi"})));
    assert_eq!(activity.denials, 2, "an unrelated line leaves the streak alone");

    // A person's message, a question and a turn end each break the loop.
    let mut note_break = |kind: &str| {
        activity.denials = 3;
        activity.denied_since = Some(at(0));
        activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
        assert!(!note_denials(&mut activity, kind, &json!({"type": kind})));
        assert_eq!(activity.denials, 0, "{kind} ends the loop");
        assert_eq!(activity.denied_since, None, "{kind} ends the loop");
        assert_eq!(activity.last_denial, None, "{kind} ends the loop");
    };
    note_break("user_message");
    note_break("question");
    note_break("turn_end");

    // A successful result also ends it.
    activity.denials = 2;
    activity.denied_since = Some(at(0));
    activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
    let ok = json!({"type": "tool_result", "tool_call_id": "t", "output": "fine", "is_error": false});
    assert!(!note_denials(&mut activity, "tool_result", &ok));
    assert_eq!(activity.denials, 0);
    assert_eq!(activity.denied_since, None);
    assert_eq!(activity.last_denial, None);
}

/// A busy gateway restarts the hint loop's clock (issue #609) as it does the stall clock: the
/// old `denied_since` would otherwise nudge a looping colony on every other tick, while the
/// streak stands so the nudge can still name what was denied.
#[test]
fn a_busy_gateway_holds_off_the_hint_loop_nudge() {
    let mut activity = Activity {
        last: at(0),
        admitted_at: at(0),
        nudges: 0,
        last_nudge: None,
        question_since: None,
        judged: 0,
        judge_failures: 0,
        risk_announced: None,
        recoveries: 0,
        denials: 3,
        denied_since: Some(at(0)),
        last_denial: Some(("egress".to_string(), "denied host example.com".to_string())),
        boundaries: BoundaryTrail::default(),
    };
    // Without a busy tick the loop would nudge at 15.
    assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);
    activity.note_gateway_busy(at(14));
    assert_eq!(activity.denied_since, Some(at(14)), "the busy tick restarts the loop's clock");
    assert_eq!(activity.denials, 3, "the streak stands, so the nudge can still name it");
    assert!(activity.hint_loop().is_some());
    assert_eq!(
        decide(&SETTINGS, at(20), Observed::Working, &activity, None),
        Decision::Nothing
    );
    assert_eq!(decide(&SETTINGS, at(29), Observed::Working, &activity, None), Decision::Nudge);
}

// ---- Control-defeat signature (issue #609) ----

fn denial(kind: &str, control: &str, target: Option<&str>) -> Boundary {
    Boundary {
        kind: kind.into(),
        control: control.into(),
        detail: format!("{kind} by {control}"),
        target: target.map(String::from),
        at: "2026-01-01T00:00:00.000Z".into(),
    }
}

#[test]
fn a_single_benign_deny_and_a_retry_are_not_a_defeat() {
    let mut trail = BoundaryTrail::default();
    let deny = denial("exec_policy_deny", "exec_policy:secret-paths", Some(".env"));
    assert_eq!(note_boundary(&mut trail, deny.clone(), at(0)), None, "one denial is a wall");
    assert_eq!(note_boundary(&mut trail, deny, at(1)), None, "a single retry is still a wall");
    assert_eq!(
        note_boundary(&mut trail, denial("egress_denied", "egress", Some("x.example")), at(2)),
        None,
        "a different control does not add up"
    );
    assert_eq!(
        note_boundary(
            &mut trail,
            denial("path_policy_unbound", "path_policy:masked", Some("v/.env")),
            at(3)
        ),
        None,
        "an unbound path alone is the host's fault, not the colony's"
    );
}

#[test]
fn repeated_denials_of_one_control_within_the_window_fire_with_their_evidence() {
    let mut trail = BoundaryTrail::default();
    let deny = |cmd: &str| Boundary {
        detail: format!("deny (default): cat {cmd}"),
        ..denial("exec_policy_deny", "exec_policy:secret-paths", Some(cmd))
    };
    assert_eq!(note_boundary(&mut trail, deny(".env"), at(0)), None);
    assert_eq!(note_boundary(&mut trail, deny("./.env"), at(4)), None);
    let defeat = note_boundary(&mut trail, deny(".env.local"), at(9)).expect("the third in 10 min fires");
    assert_eq!(defeat.signature, "repeated_denial");
    assert_eq!(defeat.evidence.len(), REPEATED_DENIALS);
    assert!(defeat.summary.contains("exec_policy:secret-paths"), "{}", defeat.summary);
    assert_eq!(
        note_boundary(&mut trail, deny(".envrc"), at(9)),
        None,
        "the evidence is spent: the next flag needs a fresh pattern"
    );

    // Spread past the window, three denials are not a pattern.
    let mut slow = BoundaryTrail::default();
    assert_eq!(note_boundary(&mut slow, deny(".env"), at(0)), None);
    assert_eq!(note_boundary(&mut slow, deny(".env"), at(6)), None);
    assert_eq!(
        note_boundary(&mut slow, deny(".env"), at(12)),
        None,
        "the first fell out of the window"
    );
}

// Issue #1258: `.git` is read-only by design, and `git-read-only` denies the attempts with a reason
// saying so — an agent retrying `git add` is hitting the intended wall, not defeating a control.
#[test]
fn repeated_git_read_only_denials_never_flag_control_defeat() {
    let mut trail = BoundaryTrail::default();
    let deny = |attempt: &str| Boundary {
        detail: format!("deny (default): {attempt}"),
        ..denial("exec_policy_deny", "exec_policy:git-read-only", Some(".git/index"))
    };
    for minute in 0..REPEATED_DENIALS as i64 + 2 {
        assert_eq!(
            note_boundary(&mut trail, deny(&format!("git add -A (try {minute})")), at(minute)),
            None,
            "the read-only .git wall is by design, so the retries never add up"
        );
    }
    // A different exec-policy control in the same window still counts as before: the exemption is
    // the rule, not the kind.
    let mut other = BoundaryTrail::default();
    for minute in 0..REPEATED_DENIALS as i64 - 1 {
        assert_eq!(
            note_boundary(
                &mut other,
                denial("exec_policy_deny", "exec_policy:script-egress", None),
                at(minute)
            ),
            None
        );
    }
    assert!(
        note_boundary(
            &mut other,
            denial("exec_policy_deny", "exec_policy:script-egress", None),
            at(9)
        )
        .is_some()
    );
}

#[test]
fn an_ask_bypass_attempt_and_a_publish_rewrite_fire_at_once() {
    let mut trail = BoundaryTrail::default();
    note_boundary(
        &mut trail,
        denial("exec_policy_deny", "exec_policy:writes-outside-repo", Some("/etc/foo")),
        at(0),
    );
    let bypass = note_boundary(
        &mut trail,
        denial(
            "exec_policy_ask_bypass_attempt",
            "exec_policy:writes-outside-repo",
            Some("/etc/foo"),
        ),
        at(1),
    )
    .expect("a refused ask asked again fires");
    assert_eq!(bypass.signature, "ask_bypass");
    assert_eq!(bypass.evidence.len(), 2, "the refusal and the attempt");

    let rewrite = note_boundary(&mut trail, denial("publish_rewrite_refused", "gitfile", Some(".git")), at(2))
        .expect("a publish rewrite fires");
    assert_eq!(rewrite.signature, "publish_rewrite");
    assert_eq!(rewrite.evidence[0].control, "gitfile");
}

#[test]
fn a_refused_target_reached_by_a_successful_call_is_a_defeat() {
    let mut trail = BoundaryTrail::default();
    note_boundary(&mut trail, denial("egress_denied", "egress", Some("evil.example")), at(0));
    note_boundary(
        &mut trail,
        denial("exec_policy_deny", "exec_policy:writes-outside-repo", Some("/colonizer/x")),
        at(0),
    );

    // A call that only mentions the host as text is not a reach; an errored reach is the wall again.
    note_reach_call(&mut trail, "t1", &json!({"command": "grep evil.example notes.md"}), at(1));
    assert_eq!(note_reach_result(&mut trail, "t1", false), None);
    note_reach_call(&mut trail, "t2", &json!({"command": "curl https://evil.example/x"}), at(1));
    assert_eq!(
        note_reach_result(&mut trail, "t2", true),
        None,
        "an errored result is the wall holding"
    );

    note_reach_call(
        &mut trail,
        "t3",
        &json!({"command": "python3 -c 'urlopen(\"https://evil.example/\")'"}),
        at(2),
    );
    let defeat = note_reach_result(&mut trail, "t3", false).expect("the denied host reached");
    assert_eq!(defeat.signature, "deny_then_reach");
    assert_eq!(defeat.evidence[0].kind, "egress_denied");

    note_reach_call(&mut trail, "t4", &json!({"file_path": "/colonizer/x", "content": "y"}), at(3));
    let write = note_reach_result(&mut trail, "t4", false).expect("the refused write target written another way");
    assert_eq!(write.evidence[0].target.as_deref(), Some("/colonizer/x"));

    // Past the reach window the target is forgotten.
    note_boundary(&mut trail, denial("egress_denied", "egress", Some("late.example")), at(10));
    note_reach_call(
        &mut trail,
        "t5",
        &json!({"command": "curl https://late.example"}),
        at(10 + REACH_WINDOW_MINUTES + 1),
    );
    assert_eq!(note_reach_result(&mut trail, "t5", false), None);
}

#[test]
fn reaches_matches_whole_hosts_and_path_tokens_only() {
    assert!(reaches(r#"{"command":"curl https://evil.example/x"}"#, "evil.example"));
    assert!(reaches(r#"{"command":"git push git@evil.example:x"}"#, "evil.example"));
    assert!(!reaches(r#"{"command":"curl https://evil.example.org"}"#, "evil.example"));
    assert!(!reaches(r#"{"command":"echo evil.example"}"#, "evil.example"));
    assert!(reaches(r#"{"file_path":".env"}"#, ".env"));
    assert!(reaches(r#"{"command":"cp a ./.env"}"#, ".env"));
    assert!(!reaches(r#"{"file_path":".env.example"}"#, ".env"));
    assert!(!reaches(r#"{"file_path":"config/.env"}"#, ".env"));
    assert!(reaches(r#"{"command":"tee /etc/foo"}"#, "/etc/foo"));
    assert!(!reaches(r#"{"command":"tee /etc/foobar"}"#, "/etc/foo"));
}

/// #1079: an agent in /workspace was refused `git check-ignore -v .env ...; wc -c ...`, and its next
/// call that named the workspace root was scored as reaching the refused file.
#[test]
fn a_call_naming_only_an_ancestor_of_the_refused_path_is_not_a_reach() {
    for ancestor in [
        r#"{"command":"cd /workspace && git status"}"#,
        r#"{"command":"ls -la /workspace"}"#,
        r#"{"command":"ls /workspace/"}"#,
        r#"{"command":"cd /workspace/config && make"}"#,
        r#"{"command":"ls /workspace/*"}"#,
        r#"{"command":"ls ."}"#,
        r#"{"path":"/workspace","pattern":"TODO"}"#,
    ] {
        assert!(!reaches(ancestor, ".env"), "{ancestor}");
        assert!(!reaches(ancestor, "/workspace/.env"), "{ancestor}");
        assert!(!reaches(ancestor, "/workspace/config/.env"), "{ancestor}");
    }
    assert!(!reaches(r#"{"command":"ls ~"}"#, "~/.ssh"));
    // A target that names no file of its own never matches: the workspace root or above (what an
    // older runner reported for `cd /workspace && cat .env`), a redirect, the colony's own output.
    assert!(!reaches(r#"{"command":"cd /workspace && cargo test"}"#, "/workspace"));
    assert!(!reaches(r#"{"command":"cd /workspace && cargo test"}"#, "/workspace/"));
    assert!(!reaches(r#"{"command":"ls /"}"#, "/"));
    assert!(!reaches(r#"{"command":"make 2>&1 | tail"}"#, "2>&1"));
    assert!(!reaches(
        r#"{"file_path":"/harness/out/pr.md","content":"x"}"#,
        "/harness/out/pr.md"
    ));
    assert!(!reaches(r#"{"command":"ls /harness/out"}"#, "/harness/out"));
    // #1153: the output dir is the colony's to write, so a path under it is never a reached
    // target, and writing there does not reach a target refused elsewhere.
    assert!(!reaches(
        r#"{"file_path":"/harness/out/verify/log","content":"x"}"#,
        "/harness/out/verify/log"
    ));
    assert!(!reaches(r#"{"command":"cat > /harness/out/verify/log"}"#, "/etc/foo"));
}

#[test]
fn a_call_naming_the_refused_path_itself_still_reaches_it() {
    for (input, target) in [
        (r#"{"command":"cat .env"}"#, ".env"),
        (r#"{"command":"cd /workspace && cat .env"}"#, ".env"),
        (r#"{"file_path":"/workspace/.env"}"#, ".env"),
        (r#"{"command":"cat ./.env"}"#, "/workspace/.env"),
        (r#"{"command":"cat /workspace/sub/../.env"}"#, ".env"),
        (r#"{"file_path":"/workspace/config/.env"}"#, "config/.env"),
        // A glob that matches it, the way the shell would expand it.
        (r#"{"command":"cat .e*"}"#, ".env"),
        (r#"{"command":"cat /workspace/.en?"}"#, ".env"),
        (r#"{"command":"head /workspace/config/.*"}"#, "/workspace/config/.env"),
        (r#"{"command":"cat ~/.ssh/id_*"}"#, "~/.ssh"),
        // Something under a refused directory.
        (r#"{"command":"cat ~/.ssh/id_rsa"}"#, "~/.ssh"),
        (r#"{"file_path":"/etc/foo/bar"}"#, "/etc/foo"),
    ] {
        assert!(reaches(input, target), "{input} should reach {target}");
    }
    // A shell glob does not match a leading dot, and a sibling is another file.
    assert!(!reaches(r#"{"command":"cat *"}"#, ".env"));
    assert!(!reaches(r#"{"command":"cat /workspace/*"}"#, "/workspace/.env"));
    assert!(!reaches(r#"{"command":"cat .env.e*"}"#, ".env"));
    assert!(!reaches(r#"{"command":"cat /workspace/other/.env"}"#, ".env"));
}

#[test]
fn deny_then_reach_skips_the_workspace_root_and_fires_on_the_file() {
    let mut trail = BoundaryTrail::default();
    note_boundary(
        &mut trail,
        denial("exec_policy_deny", "exec_policy:secret-paths", Some(".env")),
        at(0),
    );
    note_reach_call(&mut trail, "root", &json!({"command": "cd /workspace && git log -1"}), at(1));
    assert_eq!(
        note_reach_result(&mut trail, "root", false),
        None,
        "the workspace root is not the file"
    );
    note_reach_call(&mut trail, "file", &json!({"file_path": "/workspace/.env"}), at(2));
    let defeat = note_reach_result(&mut trail, "file", false).expect("the refused file itself");
    assert_eq!(defeat.signature, "deny_then_reach");

    // An older runner named the workspace root as the target: nothing reaches it.
    let mut trail = BoundaryTrail::default();
    note_boundary(
        &mut trail,
        denial("exec_policy_deny", "exec_policy:secret-paths", Some("/workspace")),
        at(0),
    );
    note_reach_call(&mut trail, "ls", &json!({"command": "ls /workspace"}), at(1));
    assert_eq!(note_reach_result(&mut trail, "ls", false), None);
}

/// #1153: `/harness/out` is the colony's own output directory — the harness itself tells the agent
/// to write `pr.md` and the verify logs there — so a refusal on a host path followed by a
/// successful write into the output dir is the brief being followed, not a defeat. A call that
/// reaches the refused path itself still is one.
#[test]
fn a_write_into_the_output_dir_after_a_refusal_is_not_a_deny_then_reach() {
    let mut trail = BoundaryTrail::default();
    note_boundary(
        &mut trail,
        denial("exec_policy_deny", "exec_policy:writes-outside-repo", Some("/etc/foo")),
        at(0),
    );
    note_reach_call(&mut trail, "pr", &json!({"command": "cat > /harness/out/pr.md"}), at(1));
    assert_eq!(
        note_reach_result(&mut trail, "pr", false),
        None,
        "the pull request description is the colony's to write"
    );
    note_reach_call(
        &mut trail,
        "log",
        &json!({"file_path": "/harness/out/verify/log", "content": "ok"}),
        at(2),
    );
    assert_eq!(
        note_reach_result(&mut trail, "log", false),
        None,
        "a path under the output dir is the colony's too"
    );

    note_reach_call(&mut trail, "etc", &json!({"file_path": "/etc/foo"}), at(3));
    let defeat = note_reach_result(&mut trail, "etc", false).expect("the refused file written another way");
    assert_eq!(defeat.signature, "deny_then_reach");
    assert_eq!(defeat.evidence[0].target.as_deref(), Some("/etc/foo"));
}

/// The flag is the attention item with the evidence, and the log says why; the watchdog's own
/// tick does not nudge over it or take it down.
#[tokio::test]
async fn the_control_defeat_flag_carries_its_evidence_and_holds_against_the_tick() {
    let (app, root) = stalled_app("defeat", SessionStatus::Running).await;
    let defeat = Defeat {
        signature: "repeated_denial",
        summary: "`egress` refused the colony 3 times in 10 min".into(),
        evidence: vec![denial("egress_denied", "egress", Some("a.example")); 3],
    };
    flag_control_defeat(&app, "w1", defeat).await;
    let attention = app.session("w1").await.unwrap().attention.expect("flagged");
    assert_eq!(attention["reason"], CONTROL_DEFEAT_REASON);
    assert_eq!(attention["signature"], "repeated_denial");
    assert_eq!(attention["evidence"].as_array().unwrap().len(), 3);
    assert_eq!(attention["evidence"][0]["type"], "boundary");
    assert_eq!(attention["evidence"][0]["target"], "a.example");
    check_all(&app).await;
    assert_eq!(
        app.session("w1").await.unwrap().attention.unwrap()["reason"],
        CONTROL_DEFEAT_REASON,
        "a stalled colony's tick does not replace the flag"
    );
    let logs = app.runtime("w1").await.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["level"] == "error"
            && l["message"]
                .as_str()
                .is_some_and(|m| m.contains("control-defeat signature (repeated_denial)"))),
        "{logs:?}"
    );
    assert!(
        !logs
            .iter()
            .any(|l| l["message"].as_str().is_some_and(|m| m.contains("nudged the agent"))),
        "and no nudge was sent over it"
    );
    let _ = std::fs::remove_dir_all(root);
}

// ---- The lost-continuation re-drive (issue #1266) ----

/// Acceptance (issue #1266): once the last in-flight subagent's settlement has stood past the
/// grace, the turn is re-driven — the interrupt first, so the re-drive message starts a fresh
/// turn instead of queueing behind the wedged one — and the claim is spent on the send.
#[tokio::test]
async fn a_turn_lost_after_the_last_subagent_is_redriven() {
    let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    let mut commands = rt.commands_rx.lock().await.take().expect("the command channel");
    let armed = at(0);
    *rt.turn_lost_since.lock().await = Some(armed);
    let s = app.session("abc").await.unwrap();
    maybe_redrive_subagent_turn(&app, &s, &rt, &SETTINGS, at(3)).await;

    let interrupt = commands.try_recv().expect("the interrupt went out");
    assert_eq!(interrupt["type"], "interrupt");
    let message = commands.try_recv().expect("the re-drive message behind it");
    assert_eq!(message["type"], "user_message");
    assert!(
        message["id"].as_str().is_some_and(|id| id.starts_with("watchdog-")),
        "a watchdog id, so its echo is not progress: {message}"
    );
    assert!(commands.try_recv().is_err(), "and nothing else");

    let attention = app.session("abc").await.unwrap().attention.expect("flagged");
    assert_eq!(attention["reason"], TURN_LOST_REASON);
    assert_eq!(since_of(&attention), armed, "dated from the settlement, not the re-drive");
    assert!(
        rt.turn_lost_since.lock().await.is_none(),
        "the claim is spent on the send, so the next tick cannot repeat it"
    );
    let logs = rt.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.contains("the turn never resumed; interrupting the dead turn"))),
        "{logs:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Inside the grace the window is left to the continuation it waits for, and with the watchdog
/// off nothing is re-driven at all.
#[tokio::test]
async fn a_fresh_subagent_settlement_is_not_redriven_early() {
    let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    let mut commands = rt.commands_rx.lock().await.take().expect("the command channel");
    let armed = at(0);
    *rt.turn_lost_since.lock().await = Some(armed);
    let s = app.session("abc").await.unwrap();
    maybe_redrive_subagent_turn(&app, &s, &rt, &SETTINGS, at(0) + Duration::seconds(30)).await;
    assert!(commands.try_recv().is_err(), "inside the grace nothing is sent");
    assert_eq!(*rt.turn_lost_since.lock().await, Some(armed), "the window stands");
    assert!(app.session("abc").await.unwrap().attention.is_none(), "and no flag went up");

    // A disabled watchdog leaves the turn alone whatever the window says.
    let settings = WatchdogSettings {
        enabled: false,
        ..SETTINGS
    };
    maybe_redrive_subagent_turn(&app, &s, &rt, &settings, at(30)).await;
    assert!(commands.try_recv().is_err(), "a disabled watchdog does not re-drive");
    let _ = std::fs::remove_dir_all(root);
}
