//! The task-bearing UHP core (issue #650): request validation, the projection of a colony's §2
//! events into the §7.4 stream, cancellation and the scoped-token rules.

use super::*;
use crate::api_tokens::NewToken;
use crate::modules::AgentModule;
use crate::server::router;
use crate::sessions::tests::colony;
use crate::tests::{temp_root, test_app, test_app_with_agents};
use crate::uhp::VERSION_HEADER;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, Method, header};
use futures_util::StreamExt as _;
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn request(method: Method, uri: &str, headers: Vec<(HeaderName, String)>, body: &str) -> Request {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7878")
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn owner(app: &Shared) -> Vec<(HeaderName, String)> {
    vec![(header::AUTHORIZATION, format!("Bearer {}", app.api_token))]
}

fn with(token: &str) -> Vec<(HeaderName, String)> {
    vec![(header::AUTHORIZATION, format!("Bearer {token}"))]
}

async fn token(app: &Shared, scope: &str, orgs: &[&str]) -> String {
    app.api_tokens
        .create(NewToken {
            name: format!("ci-{scope}"),
            scope: scope.into(),
            orgs: orgs.iter().map(|o| o.to_string()).collect(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        })
        .await
        .unwrap()
        .token
}

async fn body_json(res: Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&bytes)))
}

async fn push(app: &Shared, id: &str, org: &str, status: SessionStatus) {
    let mut session = colony(org, status);
    session.id = id.into();
    session.agent = "claude-code".into();
    app.sessions.write().await.push(session);
}

/// Writes a colony's current event log, numbering the lines from 1.
fn write_log(app: &Shared, id: &str, events: &[Value]) {
    let dir = app.session_dir(id);
    std::fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut e = e.clone();
            e["seq"] = json!(i as u64 + 1);
            e.to_string()
        })
        .collect();
    std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
}

/// One whole turn in §2 terms, ending in `turn_end`.
fn a_turn(text: &str, cost: f64, input: u64, output: u64) -> Vec<Value> {
    vec![
        json!({"type": "user_message", "id": "initial", "text": text}),
        json!({"type": "status", "state": "working"}),
        json!({"type": "assistant_text_delta", "message_id": "m1", "block_index": 0, "delta": "Hel"}),
        json!({"type": "assistant_text_delta", "message_id": "m1", "block_index": 0, "delta": "lo"}),
        json!({"type": "assistant_text", "message_id": "m1", "block_index": 0, "text": "Hello"}),
        json!({"type": "thinking", "message_id": "m2", "block_index": 1, "text": "Write the file."}),
        json!({"type": "tool_call", "message_id": "m2", "tool_call_id": "t1", "name": "Bash", "input": {"command": "true"}}),
        json!({"type": "tool_result", "tool_call_id": "t1", "output": "ok"}),
        json!({"type": "log", "level": "info", "message": "quiet"}),
        json!({"type": "log", "level": "warn", "message": "loud"}),
        json!({"type": "turn_end", "is_error": false, "result": "done", "cost_usd": cost, "duration_ms": 1000,
               "model_usage": {"m": {"input_tokens": input, "output_tokens": output, "cache_read_tokens": 0, "cache_write_tokens": 0}}}),
        json!({"type": "status", "state": "idle"}),
    ]
}

fn types(events: &[Value]) -> Vec<String> {
    events.iter().map(|e| e["type"].as_str().unwrap().to_string()).collect()
}

/// Every SSE frame's `data:` JSON, in order.
fn sse_events(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

fn rid(session: &str, epoch: u64, turn: u64) -> ResponseId {
    ResponseId {
        session: session.into(),
        epoch,
        turn,
    }
}

// ---------------------------------------------------------------------------
// Ids and the projection
// ---------------------------------------------------------------------------

#[test]
fn response_ids_parse_from_the_right_and_refuse_the_malformed() {
    let id = ResponseId::parse("resp_abc.2.3").unwrap();
    assert_eq!(id, rid("abc", 2, 3));
    assert_eq!(id.render(), "resp_abc.2.3");
    assert_eq!(ResponseId::parse("resp_a.b.1.1").unwrap().session, "a.b");
    for bad in [
        "abc.1.1",
        "resp_.1.1",
        "resp_abc.0.1",
        "resp_abc.1.0",
        "resp_abc.1",
        "resp_abc.x.1",
        "resp_",
    ] {
        assert!(ResponseId::parse(bad).is_none(), "{bad}");
    }
    assert_eq!(session_of("resp_abc.1.1").as_deref(), Some("abc"));
}

#[test]
fn a_turn_projects_in_spec_order_with_one_terminal_event() {
    let mut projection = Projection::new(rid("s", 1, 1), "claude-code", 0);
    let mut out = Vec::new();
    for event in [json!({"type": "status", "state": "idle"})]
        .iter()
        .chain(a_turn("go", 0.5, 100, 20).iter())
    {
        out.extend(projection.feed(event));
    }
    assert_eq!(
        types(&out),
        [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_part.done",
            "response.output_item.added",
            "response.output_item.done",
            "response.output_item.done",
            "colonizer.log",
            "response.completed",
        ]
    );
    for (n, event) in out.iter().enumerate() {
        assert_eq!(event["sequence_number"], n as u64, "sequence numbers count the stream from 0");
    }
    assert!(projection.done());
    let done = &out.last().unwrap()["response"];
    assert_eq!(done["id"], "resp_s.1.1");
    assert_eq!(done["status"], "completed");
    assert_eq!(done["metadata"]["session_id"], "s");
    assert_eq!(done["metadata"]["harness_id"], "claude-code");
    assert_eq!(done["metadata"]["cost_usd"], "0.500000");
    assert_eq!(done["metadata"]["duration_ms"], "1000");
    assert_eq!(done["usage"]["input_tokens"], 100);
    assert_eq!(done["usage"]["output_tokens"], 20);
    let output = done["output"].as_array().unwrap();
    assert_eq!(output[0]["type"], "message");
    assert_eq!(output[0]["content"][0]["text"], "Hello");
    assert_eq!(output[1]["type"], "reasoning");
    assert_eq!(output[2]["type"], "function_call");
    assert_eq!(output[2]["call_id"], "t1");
    assert_eq!(output[2]["arguments"], "{\"command\":\"true\"}");
    assert_eq!(output[3]["type"], "function_call_output");
    assert_eq!(output[3]["output"], "ok");
    // Nothing after the terminal event belongs to this response.
    assert!(projection.feed(&json!({"type": "user_message", "text": "next"})).is_empty());
}

#[test]
fn a_later_turn_streams_only_its_own_events_and_measures_from_the_turn_before() {
    let mut projection = Projection::new(rid("s", 1, 2), "claude-code", 0);
    let mut out = Vec::new();
    for event in a_turn("one", 0.5, 100, 20).iter().chain(a_turn("two", 0.75, 160, 50).iter()) {
        out.extend(projection.feed(event));
    }
    assert_eq!(out[0]["type"], "response.created");
    assert_eq!(out[0]["sequence_number"], 0);
    let done = &out.last().unwrap()["response"];
    assert_eq!(done["id"], "resp_s.1.2");
    assert_eq!(
        done["metadata"]["cost_usd"], "0.250000",
        "this turn's cost, not the cumulative one"
    );
    assert_eq!(done["usage"]["input_tokens"], 60);
    assert_eq!(done["usage"]["output_tokens"], 30);
}

#[test]
fn how_a_turn_ends_picks_the_terminal_event() {
    // An error turn: failed, with the code the colony's attention names.
    let mut projection = Projection::new(rid("s", 1, 1), "claude-code", 0);
    projection.attention = Some(json!({"reason": "model_error"}));
    projection.feed(&json!({"type": "user_message", "text": "go"}));
    let out = projection.feed(&json!({"type": "turn_end", "is_error": true, "result": "upstream 529"}));
    let last = out.last().unwrap();
    assert_eq!(last["type"], "response.failed");
    assert_eq!(last["response"]["status"], "failed");
    assert_eq!(last["response"]["error"]["code"], "provider_error");

    // An interrupted turn: response.failed whose status is cancelled.
    let mut projection = Projection::new(rid("s", 1, 1), "claude-code", 0);
    projection.cancelled = true;
    projection.feed(&json!({"type": "user_message", "text": "go"}));
    let out = projection.feed(&json!({"type": "turn_end", "is_error": true, "result": "interrupted"}));
    assert_eq!(out.last().unwrap()["type"], "response.failed");
    assert_eq!(out.last().unwrap()["response"]["status"], "cancelled");

    // `exited` with no turn_end: the driver ends it from the colony's record.
    let mut projection = Projection::new(rid("s", 1, 1), "claude-code", 0);
    projection.feed(&json!({"type": "user_message", "text": "go"}));
    assert!(projection.feed(&json!({"type": "status", "state": "exited"})).is_empty());
    assert!(projection.exited);
    let out = projection.finish(Ending::Failed("harness_error", "gone".into()));
    assert_eq!(types(&out), ["error", "response.failed"]);
    assert_eq!(out[0]["code"], "harness_error");
    assert!(
        projection.finish(Ending::Cancelled).is_empty(),
        "a finished response stays finished"
    );

    let mut projection = Projection::new(rid("s", 1, 1), "claude-code", 0);
    let out = projection.finish(Ending::Incomplete("colonizer_spend_budget"));
    assert_eq!(
        types(&out),
        ["response.created", "response.in_progress", "response.incomplete"]
    );
    assert_eq!(out[2]["response"]["incomplete_details"]["reason"], "colonizer_spend_budget");
}

#[test]
fn the_colony_record_names_the_ending() {
    let mut s = colony("acme", SessionStatus::Stopped);
    assert_eq!(ending_for(&s), Ending::Cancelled);
    s.error = Some("passed its spend budget of $1.00 (org) at $2.00 of model spend".into());
    assert_eq!(ending_for(&s), Ending::Incomplete("colonizer_spend_budget"));
    s.error = Some("passed its host-disk quota of 1 GB".into());
    assert_eq!(ending_for(&s), Ending::Incomplete("colonizer_host_disk_quota"));
    s.error = Some(crate::sessions::VM_STOPPED_EARLY.into());
    assert_eq!(ending_for(&s), Ending::Incomplete("colonizer_max_session_length"));
    s.status = SessionStatus::Failed;
    s.error = Some(crate::sessions::AGENTD_NOT_READY.into());
    assert!(matches!(ending_for(&s), Ending::Failed("harness_error", _)));
}

// ---------------------------------------------------------------------------
// POST /uhp/v1/responses: validation and continuation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_create_validates_its_input_in_envelopes() {
    let root = temp_root();
    let app = test_app_with_agents(
        &root,
        vec![AgentModule::test("claude-code"), AgentModule::test("grok-build")],
        |_| {},
    );
    let router = router(&app);
    for (body, param) in [
        ("{not json", "body"),
        ("[1]", "body"),
        ("{}", "input"),
        (r#"{"input": "   "}"#, "input"),
        (r#"{"input": 5}"#, "input"),
        (r#"{"input": [{"type": "input_file", "file_id": "f"}]}"#, "input"),
        (r#"{"input": "go", "stream": "yes"}"#, "stream"),
        (r#"{"input": "go", "metadata": {"repo": 1}}"#, "metadata"),
        (r#"{"input": "go", "previous_response_id": 7}"#, "previous_response_id"),
        (r#"{"input": "go"}"#, "metadata.repo"),
        (r#"{"input": "go", "metadata": {"repo": "not a repo"}}"#, "metadata.repo"),
        (
            r#"{"input": "go", "metadata": {"repo": "acme/app", "issue": "x"}}"#,
            "metadata.issue",
        ),
    ] {
        let res = router
            .clone()
            .oneshot(request(Method::POST, "/uhp/v1/responses", owner(&app), body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{body}");
        assert!(res.headers().get(VERSION_HEADER).is_some(), "{body}");
        let err = body_json(res).await["error"].clone();
        assert_eq!(err["type"], "invalid_request_error", "{body}");
        assert_eq!(err["code"], "invalid_input", "{body}");
        assert_eq!(err["param"], param, "{body}");
    }
    // A harness the install lacks, and one the repository's colonies do not launch on.
    for (harness, status, code) in [
        ("chrn_nosuch", StatusCode::NOT_FOUND, "harness_not_found"),
        ("chrn_grok-build", StatusCode::CONFLICT, "harness_mismatch"),
    ] {
        let body = json!({"input": "go", "metadata": {"repo": "acme/app", "harness_id": harness}}).to_string();
        let res = router
            .clone()
            .oneshot(request(Method::POST, "/uhp/v1/responses", owner(&app), &body))
            .await
            .unwrap();
        assert_eq!(res.status(), status, "{harness}");
        assert_eq!(body_json(res).await["error"]["code"], code, "{harness}");
    }
    // An unknown response to continue.
    for previous in ["resp_nosuch.1.1", "garbage"] {
        let body = json!({"input": "go", "previous_response_id": previous}).to_string();
        let res = router
            .clone()
            .oneshot(request(Method::POST, "/uhp/v1/responses", owner(&app), &body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{previous}");
        assert_eq!(body_json(res).await["error"]["code"], "response_not_found");
    }
    // A method the route lacks is the envelope's 405.
    let res = router
        .oneshot(request(Method::GET, "/uhp/v1/responses", owner(&app), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body_json(res).await["error"]["code"], "method_not_allowed");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_continuation_resolves_by_the_colony_state() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "busy650", "acme", SessionStatus::Running).await;
    write_log(&app, "busy650", &[json!({"type": "user_message", "text": "go"})]);
    push(&app, "done650", "acme", SessionStatus::PrOpened).await;
    write_log(&app, "done650", &a_turn("go", 0.1, 1, 1));
    push(&app, "two650", "acme", SessionStatus::Idle).await;
    let mut both = a_turn("one", 0.1, 1, 1);
    both.extend(a_turn("two", 0.2, 2, 2));
    write_log(&app, "two650", &both);
    let router = router(&app);
    for (previous, metadata, status, code) in [
        ("resp_busy650.1.1", json!({}), StatusCode::CONFLICT, "session_busy"),
        ("resp_done650.1.1", json!({}), StatusCode::NOT_FOUND, "session_expired"),
        ("resp_two650.1.1", json!({}), StatusCode::CONFLICT, "colonizer_not_latest"),
        ("resp_two650.1.3", json!({}), StatusCode::NOT_FOUND, "response_not_found"),
        (
            "resp_two650.1.2",
            json!({"harness_id": "chrn_other"}),
            StatusCode::CONFLICT,
            "harness_mismatch",
        ),
    ] {
        let body = json!({"input": "more", "previous_response_id": previous, "metadata": metadata}).to_string();
        let res = router
            .clone()
            .oneshot(request(Method::POST, "/uhp/v1/responses", owner(&app), &body))
            .await
            .unwrap();
        assert_eq!(res.status(), status, "{previous}");
        let err = body_json(res).await["error"].clone();
        assert_eq!(err["code"], code, "{previous}");
        if code == "colonizer_not_latest" {
            assert_eq!(err["detail"]["latest_response_id"], "resp_two650.1.2");
        }
    }

    // An idle colony takes the input as a follow-up: the reply is the next turn, in progress, and
    // the message is on its way to the runner. Unknown fields are listed, not refused.
    let rt = app.runtime("two650").await;
    let mut commands = rt.commands_rx.lock().await.take().unwrap();
    let body = json!({"input": "more", "previous_response_id": "resp_two650.1.2", "model": "x", "store": false}).to_string();
    let res = router
        .oneshot(request(Method::POST, "/uhp/v1/responses", owner(&app), &body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let response = body_json(res).await;
    assert_eq!(response["id"], "resp_two650.1.3");
    assert_eq!(response["status"], "in_progress");
    assert_eq!(response["metadata"]["ignored_fields"], "model,store");
    let sent = commands.try_recv().unwrap();
    assert_eq!(sent["type"], "user_message");
    assert_eq!(sent["text"], "more");
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stored_turn_streams_as_sse_and_ends_on_its_terminal_event() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "sse650", "acme", SessionStatus::Idle).await;
    let mut log = a_turn("one", 0.1, 1, 1);
    log.extend(a_turn("two", 0.3, 3, 3));
    write_log(&app, "sse650", &log);
    let res = router(&app)
        .oneshot(request(
            Method::GET,
            "/uhp/v1/responses/resp_sse650.1.2?stream=true",
            owner(&app),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), "text/event-stream");
    let bytes = tokio::time::timeout(Duration::from_secs(5), axum::body::to_bytes(res.into_body(), 1 << 20))
        .await
        .expect("a finished turn's stream ends by itself")
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("event: response.created"), "{text}");
    assert!(text.contains("id: 1.13"), "the id line carries epoch.seq: {text}");
    let events = sse_events(&text);
    assert_eq!(events.first().unwrap()["type"], "response.created");
    assert_eq!(events.last().unwrap()["type"], "response.completed");
    assert_eq!(events.last().unwrap()["response"]["id"], "resp_sse650.1.2");
    for (n, event) in events.iter().enumerate() {
        assert_eq!(event["sequence_number"], n as u64);
    }

    // The same response without `stream` is the object, and an unknown one is the 404 envelope.
    let res = router(&app)
        .oneshot(request(Method::GET, "/uhp/v1/responses/resp_sse650.1.2", owner(&app), ""))
        .await
        .unwrap();
    assert_eq!(body_json(res).await["status"], "completed");
    let res = router(&app)
        .oneshot(request(Method::GET, "/uhp/v1/responses/resp_sse650.9.1", owner(&app), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(res).await["error"]["code"], "response_not_found");
    let _ = std::fs::remove_dir_all(root);
}

/// Reads SSE frames off a live body until one of type `until` arrives.
async fn read_until(
    stream: &mut (impl futures_util::Stream<Item = Result<Bytes, axum::Error>> + Unpin),
    seen: &mut String,
    until: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !sse_events(seen).iter().any(|e| e["type"] == until) {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .unwrap_or_else(|_| panic!("no {until} in: {seen}"));
        match chunk {
            Some(Ok(bytes)) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            _ => panic!("the stream ended before {until}: {seen}"),
        }
    }
}

#[tokio::test]
async fn a_live_stream_follows_the_colony_and_a_cancel_mid_stream_ends_it_cancelled() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "live650", "acme", SessionStatus::Running).await;
    write_log(
        &app,
        "live650",
        &[
            json!({"type": "status", "state": "idle"}),
            json!({"type": "user_message", "id": "initial", "text": "go"}),
        ],
    );
    let rt = app.runtime("live650").await;
    let mut commands = rt.commands_rx.lock().await.take().unwrap();
    let res = router(&app)
        .oneshot(request(
            Method::GET,
            "/uhp/v1/responses/resp_live650.1.1?stream=true",
            owner(&app),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let mut seen = String::new();
    read_until(&mut body, &mut seen, "response.in_progress").await;

    // The colony keeps working: a live event reaches the stream.
    rt.broadcast(
        Some(3),
        json!({"type": "assistant_text_delta", "message_id": "m", "block_index": 0, "delta": "Hi", "seq": 3}).to_string(),
    );
    read_until(&mut body, &mut seen, "response.output_text.delta").await;

    // Cancel mid-stream: the turn is interrupted, the colony is kept.
    let res = router(&app)
        .oneshot(request(
            Method::POST,
            "/uhp/v1/responses/resp_live650.1.1/cancel",
            owner(&app),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let reply = body_json(res).await;
    assert_eq!(reply["id"], "resp_live650.1.1");
    assert_eq!(reply["status"], "cancelled");
    assert_eq!(commands.try_recv().unwrap()["type"], "interrupt");
    assert_eq!(app.session("live650").await.unwrap().status, SessionStatus::Running);

    // The runner ends the turn; the stream's terminal event reads cancelled and the stream ends.
    rt.broadcast(
        Some(4),
        json!({"type": "turn_end", "is_error": true, "result": "interrupted", "seq": 4}).to_string(),
    );
    read_until(&mut body, &mut seen, "response.failed").await;
    let last = sse_events(&seen).last().unwrap().clone();
    assert_eq!(last["response"]["status"], "cancelled");
    let end = tokio::time::timeout(Duration::from_secs(5), body.next()).await.unwrap();
    assert!(end.is_none(), "the stream ends after its terminal event");

    // A repeat cancel of the finished response returns it unchanged, and changes nothing.
    std::fs::OpenOptions::new()
        .append(true)
        .open(app.session_dir("live650").join("events.jsonl"))
        .and_then(|mut f| {
            use std::io::Write as _;
            writeln!(
                f,
                "{}",
                json!({"type": "turn_end", "is_error": true, "result": "interrupted", "seq": 4})
            )
        })
        .unwrap();
    let res = router(&app)
        .oneshot(request(
            Method::POST,
            "/uhp/v1/responses/resp_live650.1.1/cancel",
            owner(&app),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["status"], "cancelled");
    assert!(commands.try_recv().is_err(), "no second interrupt");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_stream_ends_when_its_colony_stops_without_a_turn_end() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "gone650", "acme", SessionStatus::Running).await;
    write_log(&app, "gone650", &[json!({"type": "user_message", "text": "go"})]);
    let rt = app.runtime("gone650").await;
    let res = router(&app)
        .oneshot(request(
            Method::GET,
            "/uhp/v1/responses/resp_gone650.1.1?stream=true",
            owner(&app),
            "",
        ))
        .await
        .unwrap();
    let mut body = res.into_body().into_data_stream();
    let mut seen = String::new();
    read_until(&mut body, &mut seen, "response.in_progress").await;
    app.update_session("gone650", |x| x.status = SessionStatus::Stopped).await;
    rt.broadcast(Some(2), json!({"type": "status", "state": "exited", "seq": 2}).to_string());
    read_until(&mut body, &mut seen, "response.failed").await;
    assert_eq!(sse_events(&seen).last().unwrap()["response"]["status"], "cancelled");
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Session cancel and scopes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_cancel_stops_the_colony_and_a_repeat_is_not_an_error() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "q650", "acme", SessionStatus::Queued).await;
    push(&app, "pub650", "acme", SessionStatus::Publishing).await;
    let router = router(&app);
    for expected in ["stopped", "already_stopped"] {
        let res = router
            .clone()
            .oneshot(request(Method::POST, "/uhp/v1/sessions/q650/cancel", owner(&app), ""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["id"], "q650");
        assert_eq!(body["status"], "cancelled");
        assert_eq!(body["metadata"]["colonizer_result"], expected);
    }
    assert_eq!(app.session("q650").await.unwrap().status, SessionStatus::Stopped);
    let res = router
        .clone()
        .oneshot(request(Method::POST, "/uhp/v1/sessions/pub650/cancel", owner(&app), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(res).await["error"]["code"], "session_busy");
    let res = router
        .oneshot(request(Method::POST, "/uhp/v1/sessions/nosuch/cancel", owner(&app), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(res).await["error"]["code"], "session_not_found");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn only_write_scoped_tokens_create_or_cancel() {
    let root = temp_root();
    let app = test_app(&root);
    push(&app, "mine650", "acme", SessionStatus::Queued).await;
    push(&app, "theirs650", "other", SessionStatus::Queued).await;
    let read = token(&app, "read", &["acme"]).await;
    let operate = token(&app, "operate", &["acme"]).await;
    let launch = token(&app, "launch", &["acme"]).await;
    let router = router(&app);

    // A read token watches but may neither create nor cancel: the envelope's 403.
    for (method, uri) in [
        (Method::POST, "/uhp/v1/responses"),
        (Method::POST, "/uhp/v1/responses/resp_mine650.1.1/cancel"),
        (Method::POST, "/uhp/v1/sessions/mine650/cancel"),
    ] {
        let res = router
            .clone()
            .oneshot(request(method.clone(), uri, with(&read), r#"{"input": "go"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        let err = body_json(res).await["error"].clone();
        assert_eq!(err["type"], "permission_error", "{uri}");
        assert_eq!(err["code"], "insufficient_scope", "{uri}");
    }
    let res = router
        .clone()
        .oneshot(request(Method::GET, "/uhp/v1/responses/resp_mine650.1.1", with(&read), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["status"], "in_progress");
    // Outside the token's limits a response reads as unknown, never as forbidden.
    for uri in ["/uhp/v1/responses/resp_theirs650.1.1", "/uhp/v1/responses/garbage"] {
        let res = router
            .clone()
            .oneshot(request(Method::GET, uri, with(&read), ""))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(body_json(res).await["error"]["code"], "response_not_found", "{uri}");
    }

    // Operate cancels, but cannot create; launch gets past the gate to the request's own checks.
    let res = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/uhp/v1/responses",
            with(&operate),
            r#"{"input": "go"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = router
        .clone()
        .oneshot(request(Method::POST, "/uhp/v1/sessions/theirs650/cancel", with(&operate), ""))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(app.session("theirs650").await.unwrap().status, SessionStatus::Queued);
    let res = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/uhp/v1/responses/resp_mine650.1.1/cancel",
            with(&operate),
            "",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["status"], "cancelled");
    assert_eq!(app.session("mine650").await.unwrap().status, SessionStatus::Stopped);
    let res = router
        .oneshot(request(
            Method::POST,
            "/uhp/v1/responses",
            with(&launch),
            r#"{"input": "go"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(res).await["error"]["param"], "metadata.repo");
    let _ = std::fs::remove_dir_all(root);
}
