use super::*;
use crate::sessions::tests::*;

/// Two colonies in one store, inserted oldest first so the newest-first order is visible.
async fn two_colonies() -> (Shared, PathBuf) {
    let (app, root) = app_with_colony("older", SessionStatus::Running).await;
    let mut newer = colony("acme", SessionStatus::Idle);
    newer.id = "newer".into();
    app.sessions.write().await.push(newer);
    (app, root)
}

async fn list_response(app: &Shared, query: ListQuery) -> Value {
    let response = list(State(app.clone()), Query(query), HeaderMap::new(), None).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn page(limit: &str, cursor: Option<&str>) -> ListQuery {
    ListQuery {
        limit: Some(limit.into()),
        cursor: cursor.map(String::from),
        ..ListQuery::default()
    }
}

/// The `external_ref` / `origin` filters (issue #901), each optionally present, over the bare array
/// — neither changes the reply shape, only what is in it.
fn filter(external_ref: Option<&str>, origin: Option<&str>) -> ListQuery {
    ListQuery {
        external_ref: external_ref.map(String::from),
        origin: origin.map(String::from),
        ..ListQuery::default()
    }
}

/// The ids a list answer carries, newest first.
fn ids(parsed: &Value) -> Vec<&str> {
    parsed.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect()
}

/// Four colonies for the filters to choose between, inserted oldest first: one carries both fields,
/// one shares its reference and differs in origin, one carries a reference of its own and no
/// origin, and the newest carries neither. A filter that keeps the wrong pair, or that ORs instead
/// of ANDs, shows up here.
async fn four_colonies() -> (Shared, PathBuf) {
    let (app, root) = app_with_colony("oldest", SessionStatus::Running).await;
    {
        let mut sessions = app.sessions.write().await;
        let found = |id: &str, external_ref: Option<&str>, origin: Option<&str>| {
            let mut s = colony("acme", SessionStatus::Idle);
            s.id = id.into();
            s.external_ref = external_ref.map(String::from);
            s.origin = origin.map(String::from);
            s
        };
        let oldest = sessions.iter_mut().find(|s| s.id == "oldest").unwrap();
        oldest.external_ref = Some("ticket-1".into());
        oldest.origin = Some("burn_down".into());
        sessions.push(found("same-ref", Some("ticket-1"), Some("burn_down")));
        sessions.push(found("other-origin", Some("ticket-1"), Some("redteam")));
        sessions.push(found("unnamed", None, None));
    }
    (app, root)
}

/// `?external_ref=` and `?origin=` each select exactly the colonies carrying the value, newest
/// first, in the bare-array shape the route has always answered — a filter narrows the list, it
/// does not page it.
#[tokio::test]
async fn external_ref_and_origin_each_select_their_own_colonies() {
    let (app, root) = four_colonies().await;

    let by_ref = list_response(&app, filter(Some("ticket-1"), None)).await;
    assert_eq!(
        ids(&by_ref),
        ["other-origin", "same-ref", "oldest"],
        "the reference names three colonies, newest first: {by_ref}"
    );
    assert!(by_ref.is_array(), "a filter alone is still the bare array: {by_ref}");

    let by_origin = list_response(&app, filter(None, Some("redteam"))).await;
    assert_eq!(ids(&by_origin), ["other-origin"], "the origin names one colony: {by_origin}");
    assert!(
        !ids(&list_response(&app, filter(None, Some("burn_down"))).await).contains(&"other-origin"),
        "and does not name the colony it does not match"
    );

    // A value no colony carries is an empty list, not a refusal: the route cannot tell an unknown
    // reference from a colony that was never created.
    assert!(
        list_response(&app, filter(Some("ticket-9"), None))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        list_response(&app, filter(None, Some("map")))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A colony with no reference of its own is not in the answer for any value.
    assert!(
        !ids(&by_ref).contains(&"unnamed"),
        "a colony with no reference matches nothing"
    );

    // Both together narrow to the colonies carrying both — the pair that shares only one field is
    // not in it, so the filters compose rather than widen.
    let both = list_response(&app, filter(Some("ticket-1"), Some("burn_down"))).await;
    assert_eq!(ids(&both), ["same-ref", "oldest"], "both filters must match: {both}");
    let crossed = list_response(&app, filter(Some("ticket-1"), Some("redteam"))).await;
    assert_eq!(ids(&crossed), ["other-origin"], "and they pair per colony: {crossed}");

    // A value is trimmed the way a body field is, so a padded query filters the same colony.
    assert_eq!(
        ids(&list_response(&app, filter(None, Some(" redteam "))).await),
        ["other-origin"]
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A filter sent blank is a **400** `invalid_input`, the same refusal a bad `cursor` gets — never a
/// filter that quietly matches every colony.
#[tokio::test]
async fn a_blank_filter_is_invalid_input() {
    let (app, root) = four_colonies().await;
    for query in [
        filter(Some("   "), None),
        filter(None, Some("")),
        filter(Some("t-1"), Some(" ")),
    ] {
        let response = list(State(app.clone()), Query(query), HeaderMap::new(), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "a blank filter is refused");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["code"], "invalid_input", "{parsed}");
    }
    // The same query still pages when asked to: a filter and a page compose.
    let paged = list_response(
        &app,
        ListQuery {
            limit: Some("10".into()),
            ..filter(None, Some("redteam"))
        },
    )
    .await;
    assert_eq!(
        ids(&paged["sessions"]),
        ["other-origin"],
        "filtered inside the page too: {paged}"
    );
    assert_eq!(paged["next_cursor"], Value::Null);
    let _ = std::fs::remove_dir_all(root);
}

/// The filters narrow what the scoped-token rule already let through: a matching colony outside the
/// token's org/repo limits stays out of the answer, so a reference is not a way to read one.
#[tokio::test]
async fn a_scoped_token_never_finds_a_colony_through_a_filter() {
    let (app, root) = four_colonies().await;
    let mut elsewhere = colony("other", SessionStatus::Running);
    elsewhere.id = "elsewhere".into();
    elsewhere.repo = "other/repo".into();
    elsewhere.org = "other".into();
    elsewhere.external_ref = Some("ticket-1".into());
    elsewhere.origin = Some("burn_down".into());
    app.sessions.write().await.push(elsewhere);

    let token = axum::Extension(crate::api_tokens::ScopedToken {
        id: "tok_test".into(),
        name: "watcher".into(),
        scope: crate::api_tokens::Scope::Read,
        orgs: Vec::new(),
        repos: vec!["acme/repo".into()],
        max_concurrent: None,
        budget_usd_per_day: None,
    });

    let seen = list_response_with_token(&app, filter(Some("ticket-1"), None), Some(token.clone())).await;
    assert_eq!(
        ids(&seen),
        ["other-origin", "same-ref", "oldest"],
        "the hidden colony is not in the answer: {seen}"
    );
    // The owner sees all four, hidden colony included, so what the token missed is the filter's
    // doing rather than the fixture's.
    assert!(
        ids(&list_response(&app, filter(Some("ticket-1"), None)).await).contains(&"elsewhere"),
        "the owner sees every match"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Without `limit` or `cursor` the route answers the cockpit's bare array, newest first —
/// the shape the web UI and the sockets hub have always unwrapped.
#[tokio::test]
async fn the_list_stays_a_bare_array_without_pagination_params() {
    let (app, root) = two_colonies().await;
    let parsed = list_response(&app, ListQuery::default()).await;
    let ids: Vec<&str> = parsed.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["newer", "older"], "newest first, no wrapper: {parsed}");
    let _ = std::fs::remove_dir_all(root);
}

/// With a pagination param the answer is the §7 page shape, and walking `next_cursor`
/// reaches every colony exactly once and ends at `null`.
#[tokio::test]
async fn the_list_pages_newest_first_until_the_cursor_is_null() {
    let (app, root) = two_colonies().await;

    let first = list_response(&app, page("1", None)).await;
    assert_eq!(first["sessions"][0]["id"], "newer", "the page keeps the newest-first order");
    assert_eq!(first["next_cursor"], "newer", "the page's last colony is the next cursor");

    let second = list_response(&app, page("1", Some("newer"))).await;
    assert_eq!(
        second["sessions"][0]["id"], "older",
        "the cursor starts right after its colony"
    );
    assert_eq!(second["next_cursor"], Value::Null, "nothing follows the last colony");

    let last = list_response(&app, page("5", Some("older"))).await;
    assert_eq!(
        last["sessions"].as_array().unwrap().len(),
        0,
        "nothing follows the last colony"
    );
    assert_eq!(last["next_cursor"], Value::Null, "the walk ends at null");

    let both = list_response(&app, page("100", None)).await;
    let ids: Vec<&str> = both["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["newer", "older"], "one page holds them in order");
    assert_eq!(both["next_cursor"], Value::Null, "nothing follows a whole list");
    let _ = std::fs::remove_dir_all(root);
}

/// A cursor that names no visible colony is a **400** `invalid_input`, not a page.
#[tokio::test]
async fn a_bad_cursor_is_invalid_input() {
    let (app, root) = two_colonies().await;
    for cursor in ["missing", "OLDER"] {
        let response = list(State(app.clone()), Query(page("1", Some(cursor))), HeaderMap::new(), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["code"], "invalid_input", "{cursor}: {parsed}");
    }
    // A malformed limit is the same refusal, naming its parameter.
    let response = list(State(app.clone()), Query(page("soon", None)), HeaderMap::new(), None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(root);
}

/// The page bounds are clamped, not refused: a zero limit reads one colony, an enormous one
/// reads the whole list.
#[tokio::test]
async fn the_page_limit_is_clamped() {
    let (app, root) = two_colonies().await;
    let one = list_response(&app, page("0", None)).await;
    assert_eq!(one["sessions"].as_array().unwrap().len(), 1, "limit 0 clamps to 1");
    let all = list_response(&app, page("100000", None)).await;
    assert_eq!(
        all["sessions"].as_array().unwrap().len(),
        2,
        "limit past the list clamps to it"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A scoped token's org/repo limits filter the bare array and every page: outside them a
/// colony is not in the answer at all, and its id is no cursor either.
#[tokio::test]
async fn the_scoped_filter_applies_to_bare_and_paged_alike() {
    let (app, root) = two_colonies().await;
    let mut elsewhere = colony("other", SessionStatus::Running);
    elsewhere.id = "elsewhere".into();
    elsewhere.repo = "other/repo".into();
    elsewhere.org = "other".into();
    app.sessions.write().await.push(elsewhere);

    let token = axum::Extension(crate::api_tokens::ScopedToken {
        id: "tok_test".into(),
        name: "watcher".into(),
        scope: crate::api_tokens::Scope::Read,
        orgs: Vec::new(),
        repos: vec!["acme/repo".into()],
        max_concurrent: None,
        budget_usd_per_day: None,
    });

    let bare = list_bare(State(app.clone()), Some(token.clone())).await;
    let ids: Vec<&str> = bare.0.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["newer", "older"], "the token sees only acme/repo: {ids:?}");

    let paged = list_response_with_token(&app, page("10", None), Some(token.clone())).await;
    let ids: Vec<&str> = paged["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["newer", "older"], "the page is filtered too: {paged}");
    assert_eq!(paged["next_cursor"], Value::Null);

    // A hidden colony's id is not a valid cursor either: it names no visible colony.
    let response = list(
        State(app.clone()),
        Query(page("1", Some("elsewhere"))),
        HeaderMap::new(),
        Some(token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "a hidden id is no cursor");
    let _ = std::fs::remove_dir_all(root);
}

async fn list_response_with_token(
    app: &Shared,
    query: ListQuery,
    token: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Value {
    let response = list(State(app.clone()), Query(query), HeaderMap::new(), token).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn commands_only_reach_a_colony_whose_microvm_is_up() {
    use SessionStatus::*;
    for status in [Starting, Running, WaitingForAnswer, Idle] {
        assert!(accepts_commands(status), "{status:?} should accept an answer");
    }
    // Publishing included: the microVM is already gone, and an answer sent then
    // used to vanish while the card kept spinning.
    for status in [Publishing, PrOpened, Merged, Closed, NoChanges, Stopped, Failed, Queued] {
        assert!(!accepts_commands(status), "{status:?} must not accept an answer");
    }
}

/// Issue #508: a `read`-scoped token may watch a colony's events socket but not drive it —
/// its commands are refused with a warn on the transcript, never forwarded. An `operate`
/// token's answer goes down to the agent with its free-text note marked as external input,
/// its structured answers untouched, and the activity line names the token, never the secret.
#[tokio::test]
async fn a_read_scoped_token_watches_the_socket_but_cannot_drive_it() {
    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), QuestionRisk::ReadOnly));
    let mut rx = rt.commands_rx.lock().await.take().unwrap();
    let scoped = |scope| crate::api_tokens::ScopedToken {
        id: "tok_test".into(),
        name: "watcher".into(),
        scope,
        orgs: Vec::new(),
        repos: Vec::new(),
        max_concurrent: None,
        budget_usd_per_day: None,
    };
    let answer = r#"{"type":"answer","question_id":"q1","answers":{"a":"b"},"response":"go"}"#;
    client_command(
        &app,
        "abc",
        &rt,
        Some(crate::auth::Via::Token("watcher".into())),
        Some(scoped(crate::api_tokens::Scope::Read)),
        answer,
    )
    .await;
    assert!(rx.try_recv().is_err(), "nothing was forwarded to the agent");
    {
        let logs = rt.logs.lock().await;
        let last = logs.back().unwrap();
        assert_eq!(last["level"], "warn", "{last}");
        assert!(
            last["message"]
                .as_str()
                .unwrap()
                .contains("can watch this colony but not drive it"),
            "{last}"
        );
    }
    client_command(
        &app,
        "abc",
        &rt,
        Some(crate::auth::Via::Token("watcher".into())),
        Some(scoped(crate::api_tokens::Scope::Operate)),
        answer,
    )
    .await;
    let forwarded = rx.try_recv().unwrap();
    assert_eq!(forwarded["type"], "answer");
    assert_eq!(forwarded["question_id"], "q1");
    assert_eq!(forwarded["answers"], json!({"a": "b"}), "the answer labels are untouched");
    assert_eq!(
        forwarded["response"].as_str().unwrap(),
        "[external input from API token \"watcher\"] go",
        "the free-text note is marked as external input"
    );
    // The token's free-text message is external input too, marked like its answers; the
    // owner's message on the same socket is not.
    client_command(
        &app,
        "abc",
        &rt,
        Some(crate::auth::Via::Token("watcher".into())),
        Some(scoped(crate::api_tokens::Scope::Operate)),
        r#"{"type":"user_message","text":"rerun the failing suite"}"#,
    )
    .await;
    let forwarded = rx.try_recv().unwrap();
    assert_eq!(forwarded["type"], "user_message");
    assert_eq!(
        forwarded["text"].as_str().unwrap(),
        "[external input from API token \"watcher\"] rerun the failing suite",
        "the message is marked as external input"
    );
    client_command(&app, "abc", &rt, None, None, r#"{"type":"user_message","text":"carry on"}"#).await;
    let forwarded = rx.try_recv().unwrap();
    assert_eq!(forwarded["text"], "carry on", "the owner's message carries no marking");
    // Switching the model is not in the issue's operate list (issue #508): any scoped token is
    // refused with a warn on the transcript, never forwarded.
    client_command(
        &app,
        "abc",
        &rt,
        Some(crate::auth::Via::Token("watcher".into())),
        Some(scoped(crate::api_tokens::Scope::Launch)),
        r#"{"type":"set_model","model":"opus"}"#,
    )
    .await;
    assert!(rx.try_recv().is_err(), "set_model from a scoped token is not forwarded");
    {
        let logs = rt.logs.lock().await;
        let last = logs.back().unwrap();
        assert_eq!(last["level"], "warn", "{last}");
        assert!(
            last["message"].as_str().unwrap().contains("model"),
            "the warn says what was refused: {last}"
        );
    }
    let log = std::fs::read_to_string(app.cfg.data_dir.join(crate::activity::FILE)).unwrap();
    assert!(log.contains("token:watcher"), "the actor is the token: {log}");
    assert!(!log.contains("col_"), "no secret value reaches the log: {log}");
    let _ = std::fs::remove_dir_all(root);
}

/// Issue #746: the messages twin shares the socket's path — the client id becomes the wire id
/// (`u-<client>`), a repeat of the same id is answered without a second delivery, and the
/// socket path (no client id) delivers every time.
#[tokio::test]
async fn the_message_twin_delivers_once_per_client_id() {
    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    let mut rx = rt.commands_rx.lock().await.take().unwrap();

    let first = submit_message(&app, "abc", &rt, "  carry on  ", Some("deliv-1"), None)
        .await
        .unwrap();
    assert!(first.delivered);
    assert_eq!(first.id, "u-deliv-1", "the client id becomes the wire id");
    let forwarded = rx.try_recv().unwrap();
    assert_eq!(forwarded["type"], "user_message");
    assert_eq!(forwarded["id"], "u-deliv-1");
    assert_eq!(forwarded["text"], "carry on", "trimmed, like the socket path always trimmed");

    let again = submit_message(&app, "abc", &rt, "carry on", Some("deliv-1"), None)
        .await
        .unwrap();
    assert!(!again.delivered, "a repeated client id is a duplicate");
    assert_eq!(again.id, "u-deliv-1", "the reply still names the id");
    assert!(rx.try_recv().is_err(), "nothing was delivered twice");

    // A different client id delivers; so does a socket message with no client id at all.
    assert!(
        submit_message(&app, "abc", &rt, "go", Some("deliv-2"), None)
            .await
            .unwrap()
            .delivered
    );
    rx.try_recv().unwrap();
    let socket = submit_message(&app, "abc", &rt, "go", None, None).await.unwrap();
    assert!(socket.delivered);
    assert!(socket.id.starts_with("u-"), "the socket's id is minted: {}", socket.id);
    rx.try_recv().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// A send into a dead runner channel (the receiver dropped while the colony stops) delivers
/// nothing, so it must not be remembered: the twin answers the 409-shaped `Undelivered` and a
/// retry of the same client id is not absorbed as a duplicate — the message is not silently
/// lost.
#[tokio::test]
async fn a_failed_send_is_undelivered_and_leaves_the_client_id_free() {
    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    drop(rt.commands_rx.lock().await.take().unwrap());

    assert!(
        matches!(
            submit_message(&app, "abc", &rt, "carry on", Some("retry-1"), None).await,
            Err(MessageError::Undelivered)
        ),
        "a send into a dead runner is an error, not a fake success"
    );
    // Not remembered: the same client id goes down the send path again (and fails again)
    // instead of being answered as a duplicate.
    assert!(matches!(
        submit_message(&app, "abc", &rt, "carry on", Some("retry-1"), None).await,
        Err(MessageError::Undelivered)
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// The answer twin's send into a dead runner answers the not-accepting 409 too, instead of a
/// 204 for an answer that never reached the agent; the socket path discards the same error
/// silently, as it always has.
#[tokio::test]
async fn a_failed_answer_send_is_not_a_204() {
    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), QuestionRisk::ReadOnly));
    drop(rt.commands_rx.lock().await.take().unwrap());
    let parsed = AnswerCommand::parse(&json!({
        "type": "answer",
        "question_id": "q1",
        "answers": {"a": "b"},
        "response": "go",
    }))
    .unwrap();
    assert!(matches!(
        submit_answer(&app, "abc", &rt, parsed, None, None, true).await,
        Err(AnswerError::NotAccepting(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// The issue #1247 loop end to end: an operator's answer is put on the record — question, choice,
/// free-text note — and the brief a resume hands the agent after the restart carries the decision,
/// so the question is never put to the person twice. A lost answer records nothing.
#[tokio::test]
async fn an_answer_is_recorded_and_handed_back_on_a_resume() {
    let (app, root) = app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
    let rt = app.runtime("abc").await;
    let mut rx = rt.commands_rx.lock().await.take().unwrap();
    let questions = vec![json!({
        "question": "Which file name?",
        "header": "File",
        "options": [{"label": "hello.txt"}, {"label": "hi.txt"}],
    })];
    *rt.open_question.lock().await = Some(("q1".into(), questions.clone(), QuestionRisk::ReadOnly));
    let answer = |saw: &Vec<Value>| {
        AnswerCommand::parse(&json!({
            "question_id": "q1",
            "answers": {"Which file name?": "hello.txt"},
            "response": "the short one",
            "questions": saw,
        }))
        .unwrap()
    };

    // An answer that is refused answers nothing, so it records nothing either.
    let stale = AnswerCommand::parse(&json!({
        "question_id": "qx",
        "answers": {"Which file name?": "hello.txt"},
        "questions": questions,
    }))
    .unwrap();
    assert!(matches!(
        submit_answer(&app, "abc", &rt, stale, None, None, true).await,
        Err(AnswerError::Stale)
    ));
    assert!(
        app.session("abc").await.unwrap().answered_questions.is_empty(),
        "a refused answer is not a decision on the record"
    );

    // Live: the answer goes down and the decision lands on the record.
    *rt.open_question.lock().await = Some(("q1".into(), questions.clone(), QuestionRisk::ReadOnly));
    assert!(
        submit_answer(&app, "abc", &rt, answer(&questions), None, None, true)
            .await
            .is_ok()
    );
    assert_eq!(rx.try_recv().unwrap()["answers"], json!({"Which file name?": "hello.txt"}));

    let s = app.session("abc").await.unwrap();
    let kept = s.answered_questions.as_slice();
    assert_eq!(kept.len(), 1, "one question, one decision: {kept:?}");
    assert_eq!(kept[0].question_id, "q1");
    assert_eq!(kept[0].header, "File");
    assert_eq!(kept[0].question, "Which file name?");
    assert_eq!(kept[0].answer, json!("hello.txt"));
    assert_eq!(kept[0].response.as_deref(), Some("the short one"));
    assert!(
        kept[0].answered_at > chrono::Utc::now() - chrono::Duration::minutes(1),
        "stamped now"
    );

    // The restart, then the resume: the brief the resumed runner is launched with closes with the
    // decision, in the same terms the answer prompts use.
    let brief = format!(
        "{}{}",
        crate::github::build_prompt(&s, None, "main", true, &[], None, None),
        crate::boot::resume_extras(&s, true),
    );
    assert!(brief.contains("## Decisions already made"), "{brief}");
    assert!(brief.contains("- File: hello.txt, note: the short one"), "{brief}");
    assert!(
        !brief.contains("(no choice given)"),
        "every recorded entry carries the choice that was made: {brief}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// An answer that arrives while a colony is parked by the hold timeout (issue #876) is kept as the
/// resume note — the person's answer in place of the backoff's wording — and the colony resumes.
#[tokio::test]
async fn an_answer_while_parked_resumes_the_colony_with_it() {
    let (app, root) = app_with_colony("abc", SessionStatus::Parked).await;
    let rt = app.runtime("abc").await;
    app.update_session("abc", |x| {
        x.git_admin_dir = Some("git".into());
        x.parked = Some(crate::sessions::Park {
            at: chrono::Utc::now(),
            reason: crate::queue::HOLD_TIMEOUT_REASON.into(),
            resets_at: None,
            vm_kept: false,
            question_risk: Some(QuestionRisk::WorkspaceWrite),
        });
    })
    .await;
    // Every slot taken, so the resume queues rather than booting under the test.
    for i in 0..3 {
        let mut f = colony("acme", SessionStatus::Running);
        f.id = format!("filler-{i}");
        app.sessions.write().await.push(f);
    }
    let asked = vec![json!({"question": "Push now?", "options": [{"label": "yes"}, {"label": "no"}]})];
    *rt.open_question.lock().await = Some(("q-1".into(), asked.clone(), QuestionRisk::WorkspaceWrite));
    let parsed = AnswerCommand::parse(&json!({
        "question_id": "q-1",
        "answers": {"Push now?": "yes"},
        "questions": asked,
    }))
    .unwrap();
    assert!(
        submit_answer(&app, "abc", &rt, parsed, None, None, true).await.is_ok(),
        "a parked colony accepts the answer and resumes"
    );
    let s = app.session("abc").await.unwrap();
    let note = s.resume_note.as_deref().unwrap_or_default();
    assert!(
        note.contains("While you were parked, your question was answered"),
        "the parked wording: {note}"
    );
    assert!(note.contains("Push now?"), "the note replays the question: {note}");
    // And the decision is on the record (issue #1247), so later resumes carry it too.
    let kept = &s.answered_questions;
    assert_eq!(kept.len(), 1, "the parked decision is on the record: {kept:?}");
    assert_eq!(kept[0].header, "", "the question carried no header");
    assert_eq!(kept[0].answer, json!("yes"));
    assert_eq!(
        s.status,
        SessionStatus::Queued,
        "the colony is on its way back, not left parked"
    );
    assert!(s.parked.is_none(), "the park had its say and goes");
    let _ = std::fs::remove_dir_all(root);
}

/// The twin marks a scoped token's text as external input like the socket does, and refuses
/// with a named error what the socket drops silently — without burning the refused message's
/// client id, so a corrected retry still delivers.
#[tokio::test]
async fn the_message_twin_marks_a_scoped_tokens_text_and_refuses_the_rest() {
    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
    let rt = app.runtime("abc").await;
    let mut rx = rt.commands_rx.lock().await.take().unwrap();
    let scoped = crate::api_tokens::ScopedToken {
        id: "tok_test".into(),
        name: "watcher".into(),
        scope: crate::api_tokens::Scope::Operate,
        orgs: Vec::new(),
        repos: Vec::new(),
        max_concurrent: None,
        budget_usd_per_day: None,
    };
    let sent = submit_message(&app, "abc", &rt, "rerun the suite", Some("watch-1"), Some(&scoped))
        .await
        .unwrap();
    assert_eq!(sent.id, "u-watch-1");
    assert_eq!(
        rx.try_recv().unwrap()["text"].as_str().unwrap(),
        "[external input from API token \"watcher\"] rerun the suite",
        "the message is marked as external input"
    );

    assert!(matches!(
        submit_message(&app, "abc", &rt, "   ", Some("watch-2"), None).await,
        Err(MessageError::Invalid)
    ));
    assert!(matches!(
        submit_message(&app, "abc", &rt, &"x".repeat(100_001), Some("watch-3"), None).await,
        Err(MessageError::Invalid)
    ));
    assert!(matches!(
        submit_message(&app, "zzz", &rt, "hello", Some("watch-4"), None).await,
        Err(MessageError::NoSession)
    ));
    assert!(
        submit_message(&app, "abc", &rt, "now really", Some("watch-2"), None)
            .await
            .unwrap()
            .delivered,
        "an invalid text did not burn its client id"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose microVM is down refuses a message naming its state — the twin's 409, where
/// the socket answers a stale client with a fresh session frame instead.
#[tokio::test]
async fn the_message_twin_refuses_a_colony_that_cannot_take_one() {
    let (app, root) = app_with_colony("abc", SessionStatus::Merged).await;
    let rt = app.runtime("abc").await;
    assert!(matches!(
        submit_message(&app, "abc", &rt, "hello", Some("late-1"), None).await,
        Err(MessageError::NotAccepting(SessionStatus::Merged))
    ));
    let _ = std::fs::remove_dir_all(root);
}

/// Issue #746: the offline outbox replays a queued answer with the question content it saw.
/// The replay delivers once — a second replay finds the question closed — and an answer to a
/// question that changed under the same id (a rebooted runner counts `q-1` afresh) is refused
/// as stale instead of answering something the operator never read.
#[tokio::test]
async fn a_replayed_answer_delivers_once_and_never_to_a_changed_question() {
    let (app, root) = app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
    let rt = app.runtime("abc").await;
    let mut rx = rt.commands_rx.lock().await.take().unwrap();
    let asked = vec![json!({"question": "Push now?", "options": [{"label": "yes"}, {"label": "no"}]})];
    let replay = |saw: &Vec<Value>| {
        AnswerCommand::parse(&json!({
            "question_id": "q-1",
            "answers": {"Push now?": "yes"},
            "questions": saw,
        }))
        .unwrap()
    };

    // The question changed under the same id while the answer sat in the queue: refused.
    let changed = vec![json!({"question": "Delete the branch?", "options": [{"label": "yes"}]})];
    *rt.open_question.lock().await = Some(("q-1".into(), changed, QuestionRisk::WorkspaceWrite));
    assert!(matches!(
        submit_answer(&app, "abc", &rt, replay(&asked), None, None, true).await,
        Err(AnswerError::Stale)
    ));
    assert!(rx.try_recv().is_err(), "nothing reached the agent");

    // The question it saw: delivered, once.
    *rt.open_question.lock().await = Some(("q-1".into(), asked.clone(), QuestionRisk::WorkspaceWrite));
    assert!(
        submit_answer(&app, "abc", &rt, replay(&asked), None, None, true)
            .await
            .is_ok()
    );
    let forwarded = rx.try_recv().unwrap();
    assert_eq!(forwarded["question_id"], "q-1");
    assert!(
        forwarded.get("questions").is_none(),
        "the runner gets the answer, not the check"
    );
    assert!(matches!(
        submit_answer(&app, "abc", &rt, replay(&asked), None, None, true).await,
        Err(AnswerError::NoQuestion)
    ));
    assert!(rx.try_recv().is_err(), "a second replay delivers nothing");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn client_ids_are_bounded_and_shaped() {
    assert!(valid_client_id("m-1_2"));
    assert!(valid_client_id(&"a".repeat(64)));
    assert!(!valid_client_id(&"a".repeat(65)), "64 is the cap");
    assert!(!valid_client_id(""));
    assert!(!valid_client_id("has space"));
    assert!(!valid_client_id("unicode-é"));
    assert!(!valid_client_id("../escape"));
}

/// Issue #508: the question route separates an empty inbox from an unknown colony — a colony
/// that is not asking reads 204 with no body (nothing to answer, not an error), an unknown id
/// stays a 404, and an open question reads as the body `colonizer ask` and the cockpit parse.
#[tokio::test]
async fn the_question_route_answers_nothing_pending_with_a_204() {
    use axum::body::to_bytes;

    let (app, root) = app_with_colony("abc", SessionStatus::Running).await;

    // Nothing pending: 204, no body.
    let response = question(State(app.clone()), Path("abc".into())).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // An open question reads as the JSON body, carrying the id an answer names.
    let rt = app.runtime("abc").await;
    *rt.open_question.lock().await = Some((
        "q1".into(),
        vec![json!({"question": "Push now?", "options": []})],
        QuestionRisk::WorkspaceWrite,
    ));
    let response = question(State(app.clone()), Path("abc".into())).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["question_id"], "q1");

    // An unknown colony stays a 404.
    let error = question(State(app), Path("zzz".into())).await.unwrap_err();
    assert_eq!(error.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(root);
}

/// `POST /api/sessions/{id}/seen` (issue #744): a look at a colony with nothing unseen answers
/// 204 and stays silent — its question, if any, is still open, and no other device may close a
/// notification for it — while a look at an unseen failure clears the flag and resolves it on
/// the one subscribed device. An unknown colony is a 404 either way.
#[tokio::test]
async fn seen_clears_the_unseen_failure_and_404s_an_unknown_id() {
    let captures: crate::push::tests::Captures = std::sync::Arc::default();
    let addr = crate::push::tests::capture_server(captures.clone()).await;
    let (app, root) = app_with_colony("abc", SessionStatus::Failed).await;
    let phone = crate::push::tests::device(&format!("http://{addr}/phone"), [7u8; 16]);
    crate::push::tests::notify_on(&app).await;
    std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
    crate::push::save(&app.cfg.config_dir, &[phone.subscription]).unwrap();

    let response = seen(State(app.clone()), Path("abc".into())).await.unwrap();
    assert_eq!(response, StatusCode::NO_CONTENT);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(captures.lock().unwrap().is_empty(), "a look with nothing unseen stays silent");

    app.update_session("abc", |s| s.unseen_failure = true).await.unwrap();
    assert!(
        crate::push::needs_you(&app.session("abc").await.unwrap()),
        "the failure is unseen"
    );
    let response = seen(State(app.clone()), Path("abc".into())).await.unwrap();
    assert_eq!(response, StatusCode::NO_CONTENT);
    crate::push::tests::await_captures(&captures, 1).await;
    assert!(!app.session("abc").await.unwrap().unseen_failure, "the failure has been seen");

    let error = seen(State(app), Path("zzz".into())).await.unwrap_err();
    assert_eq!(error.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn set_model_forwards_only_what_a_model_setting_could_hold() {
    for id in [
        "opus",
        "claude-opus-5-5",
        "claude-opus-5-5[1m]",
        "deepseek/deepseek-flash",
        "together/deepseek-ai/DeepSeek-V4.1-Flash",
        "local/qwen3:32b",
    ] {
        assert_eq!(set_model_id(id), Some(id), "{id}");
    }
    assert_eq!(set_model_id("  sonnet\n"), Some("sonnet"), "trimmed");
    assert!(set_model_id(&"m".repeat(128)).is_some());
    // No model id has whitespace, quotes or non-ASCII in it.
    for bad in ["", "   ", "has space", "opus\"}", "opus\n{\"type\":\"shutdown\"}", "claudé"] {
        assert_eq!(set_model_id(bad), None, "{bad:?}");
    }
    // The longest id `/api/models` can list: a 32-byte provider id, `/`, a 120-byte model.
    let longest = format!("{}/{}", "p".repeat(32), "m".repeat(120));
    assert_eq!(set_model_id(&longest), Some(longest.as_str()), "the longest listed id");
    assert_eq!(set_model_id(&format!("{longest}m")), None, "one byte over");
}

/// The user message a resumed runner receives (issue #562): every question as it was asked,
/// the choices made (a multi-select joined), a question with no choice said so, and the
/// free-text note trimmed in. Wording the agent reads — pinned.
#[test]
fn the_answer_prompt_replays_the_questions_the_choices_and_the_note() {
    let questions = vec![
        json!({"question": "Which file name?", "options": [{"label": "hello.txt"}]}),
        json!({"question": "Which extras?", "options": []}),
        json!({"question": "Ship it?"}),
    ];
    let answers = json!({"Which file name?": "hello.txt", "Which extras?": ["lint", "format"]});
    let prompt = answer_prompt(&questions, &answers, "  make it quick  ");
    assert!(prompt.starts_with("Earlier you asked the user something"), "{prompt}");
    assert!(prompt.contains("Q: Which file name?\nA: hello.txt"), "{prompt}");
    assert!(prompt.contains("Q: Which extras?\nA: lint, format"), "{prompt}");
    assert!(prompt.contains("Q: Ship it?\nA: (no choice given)"), "{prompt}");
    assert!(prompt.ends_with("Their note: make it quick"), "{prompt}");
    let quiet = answer_prompt(&questions, &answers, "   ");
    assert!(!quiet.contains("Their note"), "{quiet}");
}

/// An answer to a suspended colony (issue #562) goes nowhere — there is no runner — but must
/// never be lost: it is held on the record and persisted before the answer returns, the open
/// question is closed in the event log so a restart does not re-open it, the colony keeps its
/// status and its suspension, and a second answer finds no question to answer.
#[tokio::test]
async fn an_answer_to_a_suspended_colony_is_kept_and_the_question_closed() {
    let (app, root) = app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
    let rt = app.runtime("abc").await;
    let mut rx = rt.commands_rx.lock().await.take().unwrap();
    let questions = vec![json!({
        "question": "Which file name?",
        "header": "File",
        "options": [{"label": "hello.txt"}, {"label": "hi.txt"}],
    })];
    *rt.open_question.lock().await = Some(("q1".into(), questions, QuestionRisk::ReadOnly));
    rt.activity.lock().await.question_since = Some(Utc::now());
    app.update_session("abc", |x| {
        x.suspended = Some(Suspension {
            at: Utc::now(),
            snapshot: None,
            reason: WAITING_FOR_ANSWER.into(),
            path: SESSION_RESUME.into(),
        });
    })
    .await
    .unwrap();

    let answer = AnswerCommand {
        question_id: "q1".into(),
        answers: json!({"Which file name?": "hello.txt"}),
        response: Value::String("go ahead".into()),
        questions: None,
    };
    assert!(
        submit_answer(&app, "abc", &rt, answer, None, None, true).await.is_ok(),
        "a suspended colony takes answers"
    );

    let s = app.session("abc").await.unwrap();
    let held = s.pending_answer.as_ref().expect("the answer is held on the record");
    assert_eq!(held.question_id, "q1");
    assert!(held.prompt.contains("Q: Which file name?\nA: hello.txt"), "{}", held.prompt);
    assert!(held.prompt.ends_with("Their note: go ahead"), "{}", held.prompt);
    // A held answer is a decision all the same (issue #1247): it goes on the record now, so the
    // resume delivers it even if the held delivery itself were lost.
    let kept = &s.answered_questions;
    assert_eq!(kept.len(), 1, "the held decision is on the record: {kept:?}");
    assert_eq!(kept[0].header, "File");
    assert_eq!(kept[0].answer, json!("hello.txt"));
    assert_eq!(kept[0].response.as_deref(), Some("go ahead"));
    assert_eq!(
        s.status,
        SessionStatus::WaitingForAnswer,
        "the status is untouched — the colony is still waiting, now with the answer"
    );
    assert!(
        s.suspended.is_some(),
        "and still suspended: nothing answered the question yet"
    );
    assert!(
        std::fs::read_to_string(app.sessions_file())
            .unwrap()
            .contains("pending_answer"),
        "the held answer reaches sessions.json before the answer returns"
    );
    let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
    assert!(
        events.contains("\"question_answered\"") && events.contains("\"q1\""),
        "the question is closed in the event log so a restart's replay does not re-open it: {events}"
    );
    assert!(rt.open_question.try_lock().unwrap().is_none(), "no question is open any more");
    assert!(rt.activity.lock().await.question_since.is_none());
    assert!(
        rx.try_recv().is_err(),
        "nothing is sent down the link — there is no runner to receive it"
    );
    {
        let logs = rt.logs.lock().await;
        let last = logs.back().unwrap();
        assert!(last["message"].as_str().unwrap().contains("kept on the colony"), "{last}");
    }
    // A second answer finds no open question: the first was taken.
    let again = AnswerCommand {
        question_id: "q1".into(),
        answers: json!({}),
        response: Value::Null,
        questions: None,
    };
    assert!(
        matches!(
            submit_answer(&app, "abc", &rt, again, None, None, true).await,
            Err(AnswerError::NoQuestion)
        ),
        "the held answer took the question with it"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The race the suspension tick and a live answer could make (issue #562), closed by the open
/// question's lock both take: an answer that forwarded to the live runner takes the question
/// down as it goes, so the tick reads no question and leaves the colony running with its
/// answer in flight — and a tick that claimed first has the answer held instead, with nothing
/// sent down a link that teardown is stopping.
#[tokio::test]
async fn an_answer_and_the_suspension_claim_cannot_interleave() {
    /// A waiting, resumable colony with the question `q1` open and past the grace, and the
    /// receiver end of its agent link's command channel.
    async fn asked(app: &Shared, id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Value> {
        let mut s = colony("acme", SessionStatus::WaitingForAnswer);
        s.id = id.into();
        s.agent = "claude-code".into();
        s.agent_session = Some("s1".into());
        app.sessions.write().await.push(s);
        std::fs::create_dir_all(app.session_dir(id)).unwrap();
        let rt = app.runtime(id).await;
        rt.activity.lock().await.question_since = Some(Utc::now() - chrono::Duration::minutes(20));
        *rt.open_question.lock().await = Some((
            "q1".into(),
            vec![json!({"question": "Push now?", "options": []})],
            QuestionRisk::WorkspaceWrite,
        ));
        rt.commands_rx.lock().await.take().unwrap()
    }
    let answer = || AnswerCommand {
        question_id: "q1".into(),
        answers: json!({"Push now?": "yes"}),
        response: Value::Null,
        questions: None,
    };

    let root = std::env::temp_dir().join(format!("colonizer-answer-race-{}", crate::util::short_id()));
    let app = crate::tests::test_app_with_agents(
        &root,
        vec![
            crate::modules::AgentModule::test("claude-code")
                .dir(std::path::PathBuf::from("/opt/colonizer/agent"))
                .entry(vec!["runner.mjs".into()])
                .resume_dir(Some("/root/.claude/projects".into())),
        ],
        |_| {},
    );
    app.modules
        .write()
        .await
        .sandbox
        .settings
        .insert("suspend_waiting".into(), json!(true));
    app.modules
        .write()
        .await
        .sandbox
        .settings
        .insert("suspend_after_minutes".into(), json!(1));
    let modules = app.modules.read().await.clone();
    let mut live = asked(&app, "answered-first").await;
    let mut claimed = asked(&app, "claimed-first").await;

    // The answer wins the race: it is forwarded to the runner that is still up, and takes the
    // open question down with it.
    assert!(
        submit_answer(
            &app,
            "answered-first",
            &app.runtime("answered-first").await,
            answer(),
            None,
            None,
            true
        )
        .await
        .is_ok()
    );
    let forwarded = live.try_recv().unwrap();
    assert_eq!(forwarded["type"], "answer", "the answer went down the live link");
    assert_eq!(forwarded["question_id"], "q1");
    assert!(
        app.runtime("answered-first")
            .await
            .open_question
            .try_lock()
            .unwrap()
            .is_none(),
        "the question went with the answer, as the runner's own echo would take it"
    );

    // So the tick skips this colony: an answer is in flight to a runner it must not tear down.
    crate::queue::suspend_waiting_colonies(&app, &modules).await;
    let s = app.session("answered-first").await.unwrap();
    assert!(
        s.suspended.is_none() && s.holds_slot(),
        "the colony keeps its microVM and its slot — its answer is on its way"
    );
    assert_eq!(
        s.status,
        SessionStatus::WaitingForAnswer,
        "the runner has not echoed yet; its status events take it from here"
    );
    assert!(s.pending_answer.is_none(), "nothing was held — it was delivered");

    // The claim wins the other race: the same tick suspended the colony still waiting, and an
    // answer arriving after that is held, not sent into the link the teardown is stopping.
    let s = app.session("claimed-first").await.unwrap();
    assert!(
        s.suspended.is_some() && !s.holds_slot(),
        "the unanswered colony was suspended"
    );
    assert!(
        submit_answer(
            &app,
            "claimed-first",
            &app.runtime("claimed-first").await,
            answer(),
            None,
            None,
            true
        )
        .await
        .is_ok()
    );
    assert!(
        claimed.try_recv().is_err(),
        "nothing goes down a link the teardown is stopping"
    );
    let s = app.session("claimed-first").await.unwrap();
    let held = s.pending_answer.as_ref().expect("the answer is held for the restore");
    assert_eq!(held.question_id, "q1");
    assert!(s.suspended.is_some(), "still suspended, the restore to deliver it");
    let _ = std::fs::remove_dir_all(root);
}
