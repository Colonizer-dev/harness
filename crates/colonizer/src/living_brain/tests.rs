use super::queue::{EntryState, backoff, enqueue, since_ok};
use super::record::{self, Detail, Scrubber};
use super::*;
use crate::util::short_id;
use serde_json::{Value, json};
use std::sync::Arc;

/// A colony as `sessions.json` holds one: a finished, merged colony with a sentinel secret in its
/// instructions.
fn colony(id: &str) -> crate::sessions::Session {
    serde_json::from_value(json!({
        "id": id,
        "repo": "acme/webshop",
        "org": "acme",
        "issue": 42,
        "issue_title": "Discount the cart",
        "instructions": "use the key sk-ant-api03-SENTINELSENTINELSENTINELSENTINELSENTINEL-abcdef only in tests",
        "status": "merged",
        "branch": "colonizer/issue-42-abc",
        "worktree": "/colonizer/worktrees/wt",
        "sandbox": "colonizer-abc",
        "agent": "claude-code",
        "pr_url": "https://github.com/acme/webshop/pull/7",
        "cost_usd": 0.25,
        "model_usage": {
            "claude-sonnet-5": {"input_tokens": 10, "output_tokens": 20, "cache_read_tokens": 30, "cache_write_tokens": 40},
            "claude-haiku-4-5": {"input_tokens": 1, "output_tokens": 2, "cache_read_tokens": 0, "cache_write_tokens": 0}
        },
        "error": null,
        "created_at": "2026-10-01T10:00:00Z",
        "updated_at": "2026-10-01T10:05:00Z",
    }))
    .unwrap()
}

fn events() -> Vec<Value> {
    let lines = [
        json!({"type":"log","level":"info","message":"agentd up","seq":1,"ts":"2026-10-01T10:00:01.000Z"}),
        json!({"type":"user_message","id":"initial","text":"You are resolving GitHub issue #42 … HARNESS-BOILERPLATE","seq":2,"ts":"2026-10-01T10:00:02.000Z"}),
        json!({"type":"model_changed","model":"claude-sonnet-5","previous":null,"seq":3,"ts":"2026-10-01T10:00:03.000Z"}),
        json!({"type":"assistant_text","message_id":"m1","block_index":0,"text":"Reading /Users/alice/secret-notes first.","seq":4,"ts":"2026-10-01T10:00:04.000Z"}),
        json!({"type":"tool_call","message_id":"m1","tool_call_id":"t1","name":"Bash","input":{"command":"cat cart.js"},"seq":5,"ts":"2026-10-01T10:00:05.000Z"}),
        json!({"type":"tool_result","tool_call_id":"t1","output":"export function total() {}","is_error":false,"seq":6,"ts":"2026-10-01T10:00:06.000Z"}),
        // agentd's re-send after a reconnect: dropped by the seq rule.
        json!({"type":"tool_result","tool_call_id":"t1","output":"DUPLICATE","is_error":false,"seq":6,"ts":"2026-10-01T10:00:06.000Z"}),
        json!({"type":"assistant_text","message_id":"s1","text":"subagent chatter","agent":{"id":"x","name":"Explore"},"seq":7,"ts":"2026-10-01T10:00:07.000Z"}),
        json!({"type":"user_message","id":"watchdog-1","text":"are you stuck?","seq":8,"ts":"2026-10-01T10:00:08.000Z"}),
        json!({"type":"question","question_id":"q","risk":"workspace_write","questions":[{"question":"Round half up?","header":"R","multi_select":false,"options":[]}],"seq":9,"ts":"2026-10-01T10:00:09.000Z"}),
        json!({"type":"question_answered","question_id":"q","answers":{"Round half up?":"Yes"},"response":null,"seq":10,"ts":"2026-10-01T10:00:10.000Z"}),
        json!({"type":"assistant_text","message_id":"m2","block_index":0,"text":"Done.","seq":11,"ts":"2026-10-01T10:00:11.000Z"}),
    ];
    let bytes = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
    record::parse_events(bytes.as_bytes())
}

fn scrub() -> Scrubber {
    Scrubber::new(Some("/Users/alice".into()), vec!["hunter2-the-saved-secret".into()])
}

#[test]
fn an_empty_record_has_the_cli_v1_shape_byte_for_byte() {
    let mut s = colony("c0");
    s.model_usage = None;
    s.cost_usd = None;
    s.branch.clear();
    let r = record::build(&s, &[], Detail::Messages, &Scrubber::default());
    assert_eq!(
        record::serialise(&r),
        concat!(
            r#"{"schema_version":1,"agent":"colonizer","collector_version":1,"#,
            r#""session_id":"c0","repo":"acme/webshop","branch":null,"models":[],"started_at":"2026-10-01T10:00:00.000Z","#,
            r#""ended_at":"2026-10-01T10:05:00.000Z","turns":[],"totals":{"input_tokens":0,"output_tokens":0,"#,
            r#""cache_read_tokens":0,"cache_creation_tokens":0,"reasoning_tokens":0,"#,
            r#""cost_usd":null},"outcome":"merged","truncated":false}"#
        )
    );
}

#[test]
fn messages_carry_the_conversation_and_tool_names_only() {
    let r = record::build(&colony("c1"), &events(), Detail::Messages, &scrub());
    let v: Value = serde_json::from_str(&record::serialise(&r)).unwrap();
    let turns = v["turns"].as_array().unwrap();
    let roles: Vec<&str> = turns.iter().map(|t| t["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "assistant", "user", "assistant"]);
    // The person's task, not the harness prompt; the watchdog nudge and the subagent are not turns.
    assert_eq!(
        turns[0]["text"],
        "Issue #42: Discount the cart\n\nuse the key [REDACTED:anthropic_key] only in tests"
    );
    let body = record::serialise(&r);
    for absent in [
        "HARNESS-BOILERPLATE",
        "are you stuck",
        "subagent chatter",
        "cat cart.js",
        "export function",
        "DUPLICATE",
    ] {
        assert!(!body.contains(absent), "{absent} leaked into a messages record: {body}");
    }
    assert_eq!(turns[1]["tool_calls"], json!([{"name": "Bash"}]));
    assert_eq!(turns[2]["text"], "Question: Round half up?");
    assert_eq!(turns[3]["text"], "Answer: Yes");
    assert_eq!(v["models"], json!(["claude-sonnet-5", "claude-haiku-4-5"]));
    assert_eq!(v["totals"]["input_tokens"], 11);
    assert_eq!(v["totals"]["cache_creation_tokens"], 40);
    assert_eq!(v["totals"]["cost_usd"], 0.25);
    assert_eq!(v["ended_at"], "2026-10-01T10:00:11.000Z");
    assert_eq!(v["schema_version"], 1);
}

#[test]
fn full_adds_tool_input_and_output_and_says_schema_two() {
    let r = record::build(&colony("c1"), &events(), Detail::Full, &scrub());
    let v: Value = serde_json::from_str(&record::serialise(&r)).unwrap();
    assert_eq!(v["schema_version"], record::FULL_SCHEMA_VERSION);
    let call = &v["turns"][1]["tool_calls"][0];
    assert_eq!(call["name"], "Bash");
    assert_eq!(call["input"], r#"{"command":"cat cart.js"}"#);
    assert_eq!(call["output"], "export function total() {}");
    assert_eq!(call["is_error"], false);
}

#[test]
fn summary_sends_no_conversation_at_all() {
    let r = record::build(&colony("c1"), &events(), Detail::Summary, &scrub());
    let v: Value = serde_json::from_str(&record::serialise(&r)).unwrap();
    assert_eq!(v["turns"], json!([]));
    assert_eq!(v["totals"]["output_tokens"], 22);
}

#[test]
fn nothing_unredacted_leaves_the_record_or_the_note() {
    let s = colony("c1");
    let mut evs = events();
    evs.push(json!({"type":"assistant_text","message_id":"m9","text":"the saved one is hunter2-the-saved-secret","seq":12}));
    let r = record::build(&s, &evs, Detail::Full, &scrub());
    let body = record::serialise(&r);
    let note = record::note_body(&r, &s, Some("src1"), &scrub());
    for text in [&body, &note] {
        assert!(!text.contains("SENTINELSENTINEL"), "a provider key leaked: {text}");
        assert!(!text.contains("hunter2-the-saved-secret"), "a saved secret leaked: {text}");
        assert!(!text.contains("/Users/alice"), "the home path leaked: {text}");
    }
    assert!(body.contains("~/secret-notes"), "the home prefix becomes ~: {body}");
    assert!(note.contains("- Issue: #42 Discount the cart"));
    assert!(note.contains("- Pull request: https://github.com/acme/webshop/pull/7"));
    assert!(note.contains("- Source: src1 (agent_log)"));
    assert!(note.contains("Coding agent session: colonizer in webshop."));
}

#[test]
fn one_colony_is_one_byte_string() {
    let a = record::serialise(&record::build(&colony("c1"), &events(), Detail::Messages, &scrub()));
    let b = record::serialise(&record::build(&colony("c1"), &events(), Detail::Messages, &scrub()));
    assert_eq!(a, b);
}

#[test]
fn an_oversized_colony_loses_trailing_turns_and_says_so() {
    let mut evs = Vec::new();
    for i in 0..600 {
        evs.push(json!({"type":"assistant_text","message_id":format!("m{i}"),"text":"word ".repeat(400),"seq":i + 1}));
    }
    let r = record::build(&colony("c2"), &evs, Detail::Messages, &Scrubber::default());
    let body = record::serialise(&r);
    assert!(body.len() <= record::MAX_BODY_BYTES, "{}", body.len());
    assert!(body.ends_with(r#""truncated":true}"#));
}

#[test]
fn event_logs_are_read_archives_first() {
    let names = ["events.jsonl", "events-10.jsonl", "events-2.jsonl", "harness.jsonl"].map(String::from);
    assert_eq!(
        record::order_event_files(&names),
        ["events-2.jsonl", "events-10.jsonl", "events.jsonl"]
    );
}

#[test]
fn project_and_slug_follow_the_cli_rules() {
    assert_eq!(record::project_of("acme/webshop").as_deref(), Some("webshop"));
    assert_eq!(record::project_of("acme/.github").as_deref(), Some("github"));
    assert_eq!(record::project_of(""), None);
    assert_eq!(record::note_slug("AbC_12"), "agent-log-colonizer-abc-12");
}

#[test]
fn a_repositorys_rule_wins_over_its_orgs() {
    let saved = Saved {
        rules: vec![
            Rule {
                target: "Acme".into(),
                send: true,
                detail: Detail::Summary,
                mcp: false,
                since: None,
            },
            Rule {
                target: "acme/web".into(),
                send: false,
                detail: Detail::Full,
                mcp: true,
                since: None,
            },
        ],
        ..Saved::default()
    };
    assert_eq!(saved.rule_for("acme/web").unwrap().target, "acme/web");
    assert_eq!(saved.rule_for("ACME/api").unwrap().target, "Acme");
    assert!(saved.rule_for("other/x").is_none(), "no rule is off");
}

#[test]
fn urls_must_be_https_or_this_machine() {
    assert!(valid_url("https://app.livingbrain.wiki/").is_ok());
    assert!(valid_url("http://127.0.0.1:8787").is_ok());
    assert!(valid_url("http://brain.example").is_err());
    assert!(valid_url("https://user:pw@brain.example").is_err());
    assert!(valid_url("ftp://x").is_err());
    assert_eq!(url_host(DEFAULT_MCP).as_deref(), Some("mcp.livingbrain.wiki"));
}

#[test]
fn the_mcp_grant_is_a_placeholder_secret_for_the_mcp_host_only() {
    let saved = Saved::default();
    let (secret, url) = mcp_grant(&saved, Some("lbp_a_b".into()), "claude-code").unwrap();
    assert_eq!(secret.env, MCP_TOKEN_ENV);
    assert_eq!(secret.hosts, ["mcp.livingbrain.wiki"]);
    assert_eq!(url, DEFAULT_MCP);
    assert!(
        crate::colony_secrets::validate_env(MCP_TOKEN_ENV).is_err(),
        "no colony secret can shadow it"
    );
    assert!(
        mcp_grant(&saved, Some("t".into()), "pi")
            .err()
            .unwrap()
            .contains("pi agent module")
    );
    assert!(mcp_grant(&saved, None, "codex").is_err());
    let revoked = Saved {
        auth_error: Some("401".into()),
        ..Saved::default()
    };
    assert!(mcp_grant(&revoked, Some("t".into()), "codex").is_err());
}

#[test]
fn the_queue_takes_a_colony_once_per_change() {
    let mut q = super::queue::Queue::default();
    let now = chrono::Utc::now();
    assert!(enqueue(&mut q, "c1", "acme/web", "idle", "fp1", now));
    assert!(!enqueue(&mut q, "c1", "acme/web", "idle", "fp2", now), "already pending");
    let e = q.entries.get_mut("c1").unwrap();
    assert_eq!(e.pending_fp.as_deref(), Some("fp2"), "but it is sent as it is now");
    e.state = EntryState::Sent;
    e.fingerprint = Some("fp2".into());
    assert!(
        !enqueue(&mut q, "c1", "acme/web", "idle", "fp2", now),
        "sent at this fingerprint"
    );
    assert!(enqueue(&mut q, "c1", "acme/web", "merged", "fp3", now), "changed since");
    assert_eq!(backoff(1), 30);
    assert_eq!(backoff(3), 120);
    assert_eq!(backoff(30), 3600);
    assert!(!since_ok(None, now), "no since: nothing goes on its own");
}

// ---------------------------------------------------------------------------------------------
// Against a stand-in brain

type Calls = Arc<std::sync::Mutex<Vec<(String, Option<String>, Value)>>>;

/// A brain that records every call (path, Authorization, JSON body) and answers like the real one;
/// `unauthorized` makes every bearer call a 401.
async fn brain(calls: Calls, unauthorized: bool) -> String {
    use axum::{extract::Request, http::StatusCode, response::IntoResponse};
    let handler = move |request: Request| {
        let calls = calls.clone();
        async move {
            let path = request.uri().path().to_owned();
            let auth = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let bytes = axum::body::to_bytes(request.into_body(), 1 << 22).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
            calls.lock().unwrap().push((path.clone(), auth.clone(), body));
            if unauthorized && auth.is_some() {
                return (StatusCode::UNAUTHORIZED, axum::Json(json!({"title": "Sign in"}))).into_response();
            }
            let answer = match path.as_str() {
                "/v1/device-auth/code" => {
                    json!({"device_code": "dev1", "user_code": "ABCD-EFGH", "verification_uri": "https://brain/device", "interval": 5, "expires_in": 600})
                }
                "/v1/device-auth/token" => json!({"access_token": "lbp_tok1_secretpart"}),
                "/v1/tokens" => {
                    json!({"tokens": [{"prefix": "lbp_tok1", "name": "colonizer", "scopes": null, "created_at": "x"}]})
                }
                "/v1/pages/sources" => json!({"id": "src_1", "created": true}),
                "/v1/notes" => json!({"id": "n1", "url": "https://brain/wiki/agent-log-colonizer-c1", "unchanged": false}),
                _ => return StatusCode::NO_CONTENT.into_response(),
            };
            axum::Json(answer).into_response()
        }
    };
    let router = axum::Router::new().fallback(handler);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://127.0.0.1:{}", addr.port())
}

async fn connected_app(server: &str) -> (crate::Shared, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-living-brain-{}", short_id()));
    let app = crate::tests::test_app(&root);
    crate::util::write_secret(&token_file(&app.cfg.config_dir), "lbp_tok1_secretpart").unwrap();
    save(
        &app.cfg.config_dir,
        &Saved {
            server_url: server.into(),
            rules: vec![Rule {
                target: "acme".into(),
                send: true,
                detail: Detail::Messages,
                mcp: true,
                since: Some("2026-01-01T00:00:00Z".into()),
            }],
            connected_at: Some("2026-10-01T00:00:00Z".into()),
            token_prefix: Some("lbp_tok1".into()),
            ..Saved::default()
        },
    )
    .unwrap();
    let s = colony("c1");
    let lines: String = events().iter().map(|e| format!("{e}\n")).collect();
    app.store().write_file("c1", "events.jsonl", lines.as_bytes()).await.unwrap();
    app.sessions.write().await.push(s);
    (app, root)
}

#[tokio::test]
async fn a_finished_colony_is_sent_once_as_a_source_and_a_note() {
    let calls: Calls = Arc::default();
    let server = brain(calls.clone(), false).await;
    let (app, root) = connected_app(&server).await;
    assert_eq!(super::queue::pass(&app).await, 1);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "{calls:?}");
        let (path, auth, body) = &calls[0];
        assert_eq!(path, "/v1/pages/sources");
        assert_eq!(auth.as_deref(), Some("Bearer lbp_tok1_secretpart"));
        assert_eq!(body["kind"], "agent_log");
        assert_eq!(body["scope"], "personal");
        assert_eq!(body["path"], "colonizer session c1");
        assert_eq!(body["project"], "webshop");
        let record: Value = serde_json::from_str(body["body"].as_str().unwrap()).unwrap();
        assert_eq!(record["agent"], "colonizer");
        assert_eq!(record["outcome"], "merged");
        let (path, _, note) = &calls[1];
        assert_eq!(path, "/v1/notes");
        assert_eq!(note["slug"], "agent-log-colonizer-c1");
        assert!(note["body"].as_str().unwrap().contains("- Source: src_1 (agent_log)"));
    }
    let q = super::queue::load_queue(&app.cfg.config_dir);
    let e = &q.entries["c1"];
    assert_eq!(e.state, EntryState::Sent);
    assert_eq!(e.note_url.as_deref(), Some("https://brain/wiki/agent-log-colonizer-c1"));
    // Nothing changed: the next pass sends nothing.
    assert_eq!(super::queue::pass(&app).await, 0);
    assert_eq!(calls.lock().unwrap().len(), 2);
    // The view never carries the token.
    let view = view(&app).await.to_string();
    assert!(!view.contains("secretpart"), "{view}");
    assert!(view.contains(r#""sent":1"#), "{view}");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_revoked_token_stops_the_queue_and_asks_for_a_reconnect() {
    let calls: Calls = Arc::default();
    let server = brain(calls.clone(), true).await;
    let (app, root) = connected_app(&server).await;
    assert_eq!(super::queue::pass(&app).await, 0);
    assert!(load(&app.cfg.config_dir).auth_error.is_some());
    assert_eq!(
        super::queue::load_queue(&app.cfg.config_dir).entries["c1"].state,
        EntryState::Pending
    );
    assert_eq!(view(&app).await["state"], "unauthorized");
    let before = calls.lock().unwrap().len();
    assert_eq!(super::queue::pass(&app).await, 0);
    assert_eq!(calls.lock().unwrap().len(), before, "no request while the token is refused");
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_repository_switched_off_sends_nothing() {
    let calls: Calls = Arc::default();
    let server = brain(calls.clone(), false).await;
    let (app, root) = connected_app(&server).await;
    update(&app.cfg.config_dir, |s| s.rules[0].send = false).unwrap();
    assert_eq!(super::queue::pass(&app).await, 0);
    assert!(calls.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_device_flow_saves_the_token_and_names_it() {
    let calls: Calls = Arc::default();
    let server = brain(calls.clone(), false).await;
    let root = std::env::temp_dir().join(format!("colonizer-living-brain-{}", short_id()));
    let app = crate::tests::test_app(&root);
    update(&app.cfg.config_dir, |s| s.server_url = server.clone()).unwrap();
    let started = connect(axum::extract::State(app.clone())).await.unwrap().0;
    assert_eq!(started["device"]["user_code"], "ABCD-EFGH");
    assert_eq!(started["device"]["state"], "pending");
    assert!(
        !started.to_string().contains("dev1"),
        "the device code stays on the mothership"
    );
    // The poller waits the server's interval (5 s) before its first poll.
    for _ in 0..80 {
        if token(&app.cfg.config_dir).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(token(&app.cfg.config_dir).as_deref(), Some("lbp_tok1_secretpart"));
    let v = view(&app).await;
    assert_eq!(v["state"], "connected");
    assert_eq!(v["token_name"], "colonizer");
    assert_eq!(v["token_prefix"], "lbp_tok1");
    // Disconnect revokes at the brain and forgets it here.
    let _ = disconnect(axum::extract::State(app.clone())).await.unwrap();
    assert!(token(&app.cfg.config_dir).is_none());
    assert!(calls.lock().unwrap().iter().any(|(p, _, _)| p == "/v1/tokens/lbp_tok1"));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn the_server_address_does_not_move_under_a_token() {
    let root = std::env::temp_dir().join(format!("colonizer-living-brain-{}", short_id()));
    let app = crate::tests::test_app(&root);
    crate::util::write_secret(&token_file(&app.cfg.config_dir), "lbp_a_b").unwrap();
    let body: SettingsRequest = serde_json::from_value(json!({"server_url": "https://elsewhere.example"})).unwrap();
    let err = put_settings(axum::extract::State(app.clone()), axum::Json(body))
        .await
        .unwrap_err();
    assert_eq!(err.status(), axum::http::StatusCode::CONFLICT);
    // Rules: switching send on stamps since; a bad target is refused.
    let body: SettingsRequest =
        serde_json::from_value(json!({"rules": [{"target": "acme", "send": true, "detail": "summary", "mcp": false}]})).unwrap();
    let v = put_settings(axum::extract::State(app.clone()), axum::Json(body))
        .await
        .unwrap()
        .0;
    assert!(v["rules"][0]["since"].is_string());
    assert_eq!(v["rules"][0]["detail"], "summary");
    let body: SettingsRequest = serde_json::from_value(json!({"rules": [{"target": "a/b/c", "send": true}]})).unwrap();
    assert!(
        put_settings(axum::extract::State(app.clone()), axum::Json(body))
            .await
            .is_err()
    );
    let _ = std::fs::remove_dir_all(&root);
}
