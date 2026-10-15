use super::queue::{EntryState, backoff, enqueue, since_ok};
use super::record::{self, Detail, Scrubber};
use super::*;
use crate::util::short_id;
use serde_json::{Value, json};
use std::sync::Arc;

/// A colony as `sessions.json` holds one: merged, with a sentinel provider key in its instructions.
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
        "base": "main",
        "worktree": "/colonizer/worktrees/wt",
        "sandbox": "colonizer-abc",
        "agent": "claude-code",
        "pr_url": "https://github.com/acme/webshop/pull/7",
        "cost_usd": 0.25,
        "routed_cost_usd": 0.5,
        "model_usage": {
            "zz-routed/model": {"input_tokens": 1, "output_tokens": 2, "cache_read_tokens": 0, "cache_write_tokens": 0},
            "claude-sonnet-5": {"input_tokens": 10, "output_tokens": 20, "cache_read_tokens": 30, "cache_write_tokens": 40}
        },
        "error": null,
        "created_at": "2026-10-01T10:00:00Z",
        "updated_at": "2026-10-01T10:05:00Z",
    }))
    .unwrap()
}

fn lines() -> Vec<Value> {
    vec![
        json!({"seq":1,"ts":"t1","type":"log","origin":"agent","level":"info","message":"agentd up"}),
        json!({"seq":2,"ts":"t2","type":"user_message","origin":"user","id":"initial","text":"You are resolving GitHub issue #42 … HARNESS-BOILERPLATE"}),
        json!({"seq":3,"ts":"t3","type":"model_changed","origin":"agent","model":"claude-opus-5"}),
        json!({"seq":4,"ts":"t4","type":"assistant_text","origin":"agent","message_id":"m1","block_index":0,"text":"Reading /Users/alice/notes first."}),
        json!({"seq":5,"ts":"t4","type":"assistant_text_delta","origin":"agent","message_id":"m1","block_index":0,"delta":"Rea"}),
        json!({"seq":6,"ts":"t5","type":"tool_call","origin":"agent","message_id":"m1","tool_call_id":"call-1","name":"Bash","input":{"command":"cargo test"}}),
        json!({"seq":7,"ts":"t6","type":"tool_result","origin":"agent","tool_call_id":"call-1","is_error":true,"output":"Exit code 101\nfailed"}),
        // agentd's re-send after a reconnect: dropped by the seq rule.
        json!({"seq":7,"ts":"t6","type":"tool_result","origin":"agent","tool_call_id":"call-1","is_error":false,"output":"DUPLICATE"}),
        json!({"seq":8,"ts":"t7","type":"tool_call","origin":"agent","message_id":"m2","tool_call_id":"call-2","name":"Agent","input":{"prompt":"dig"}}),
        json!({"seq":9,"ts":"t8","type":"tool_result","origin":"agent","tool_call_id":"call-2","is_error":false,"background":true,"output":"Async agent launched"}),
        json!({"seq":10,"ts":"t9","type":"tool_call","origin":"subagent","agent":{"id":"call-2"},"message_id":"s1","tool_call_id":"sub-1","name":"Read","input":{"file_path":"/x"}}),
        json!({"seq":11,"ts":"t10","type":"assistant_text","origin":"subagent","agent":{"id":"call-2"},"message_id":"s2","text":"Found it."}),
        json!({"seq":12,"ts":"t11","type":"subagent_end","origin":"agent","tool_call_id":"call-2","status":"completed"}),
        json!({"seq":13,"ts":"t12","type":"thinking","origin":"agent","message_id":"m3","text":"Hmm."}),
        json!({"seq":14,"ts":"t13","type":"user_message","origin":"watchdog","id":"watchdog-1","text":"are you stuck?"}),
        json!({"seq":15,"ts":"t14","type":"question","origin":"agent","question_id":"q","questions":[{"question":"Round half up?","options":[{"label":"Yes","description":"away from zero"}]}]}),
        json!({"seq":16,"ts":"t15","type":"question_answered","origin":"user","question_id":"q","answers":{"Round half up?":"Yes"}}),
        json!({"seq":17,"ts":"t16","type":"verification","origin":"system","verdict":"verified","summary":"cargo test passed"}),
        json!({"seq":18,"ts":"t17","type":"status","origin":"agent","state":"idle"}),
    ]
}

fn events() -> Vec<Value> {
    let text: String = lines().iter().map(|l| format!("{l}\n")).collect();
    record::parse_events(text.as_bytes())
}

fn scrub() -> Scrubber {
    Scrubber::new(Some("/Users/alice".into()), vec!["hunter2-the-saved-secret".into()])
}

fn prepared(detail: Detail) -> (record::SessionRecord, String) {
    let collected = record::collect(&record::row(&colony("c1")), &events()).unwrap();
    record::prepare(collected, detail, &scrub())
}

#[test]
fn an_empty_colony_has_the_schema_three_shape_byte_for_byte() {
    let mut s = colony("c0");
    s.model_usage = None;
    s.cost_usd = None;
    s.routed_cost_usd = None;
    s.branch.clear();
    s.base = None;
    s.pr_url = None;
    s.issue_title.clear();
    let collected = record::collect(&record::row(&s), &[]).unwrap();
    let (_, body) = record::prepare(collected, Detail::Full, &Scrubber::default());
    assert_eq!(
        body,
        concat!(
            r#"{"schema_version":3,"agent":"colonizer","collector_version":2,"detail":"full","#,
            r#""session_id":"c0","repo":"acme/webshop","branch":null,"models":[],"started_at":"2026-10-01T10:00:00Z","#,
            r#""ended_at":null,"turns":[],"totals":{"input_tokens":0,"output_tokens":0,"#,
            r#""cache_read_tokens":0,"cache_creation_tokens":0,"reasoning_tokens":0,"#,
            r#""cost_usd":null},"outcome":"merged","truncated":false,"omitted_turns":0,"#,
            r#""omitted_subagent_turns":0,"#,
            r#""colony":{"org":"acme","base":null,"issue":42,"issue_title":null,"pr_url":null,"#,
            r#""pr_opened_at":null,"merged_at":null,"ci_state":null,"origin":null}}"#
        )
    );
}

#[test]
fn full_maps_the_stream_like_the_collector() {
    let (r, body) = prepared(Detail::Full);
    let v: Value = serde_json::from_str(&body).unwrap();
    let turns = v["turns"].as_array().unwrap();
    let shape: Vec<(&str, &str)> = turns
        .iter()
        .map(|t| (t["role"].as_str().unwrap(), t["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("user", "use the key [REDACTED:anthropic_key] only in tests"),
            ("assistant", "Reading ~/notes first."),
            ("assistant", ""),
            ("reasoning", "Hmm."),
            ("event", "Watchdog recovery check"),
            ("assistant", "Round half up?\n- Yes: away from zero"),
            ("user", "Yes"),
            ("event", "Verification: verified — cargo test passed"),
        ]
    );
    assert_eq!(
        turns[1]["tool_calls"],
        json!([{"name":"Bash","format":"shell","input":"cargo test","output":"Exit code 101\nfailed","status":"error","exit_code":101}])
    );
    assert_eq!(
        turns[2]["tool_calls"],
        json!([{"name":"Agent","format":"text","input":"dig","output":"Found it.","status":"ok","subagent":{
            "id":"call-2","name":"subagent","description":null,"models":[],"started_at":"t7","ended_at":"t11",
            "status":"completed",
            "totals":{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0,"cache_creation_tokens":0,"reasoning_tokens":0,"cost_usd":null},
            "turns":[
                {"role":"assistant","at":"t9","text":"","tool_calls":[{"name":"Read","path":"/x"}]},
                {"role":"assistant","at":"t10","text":"Found it.","tool_calls":[]}
            ],
            "truncated":false,"omitted_turns":0
        }}]),
        "the subagent's steps nest under its call; its last words stay the call's output"
    );
    assert_eq!(turns[1]["model"], "claude-opus-5", "the first assistant turn names its model");
    assert!(turns[2].get("model").is_none(), "the same model is not repeated");
    assert_eq!(
        v["models"],
        json!(["claude-opus-5", "claude-sonnet-5", "zz-routed/model"]),
        "model_changed first, then sorted usage keys"
    );
    assert_eq!(v["totals"]["input_tokens"], 11);
    assert_eq!(v["totals"]["cache_creation_tokens"], 40);
    assert_eq!(v["totals"]["cost_usd"], 0.75);
    assert_eq!(v["started_at"], "2026-10-01T10:00:00Z");
    assert_eq!(v["ended_at"], "t16", "the last event the record used");
    assert_eq!(v["colony"]["base"], "main");
    assert_eq!(v["colony"]["issue_title"], "Discount the cart");
    assert_eq!(r.detail, Detail::Full);
    for absent in [
        "HARNESS-BOILERPLATE",
        "are you stuck",
        "DUPLICATE",
        "Async agent launched",
        "SENTINELSENTINEL",
    ] {
        assert!(!body.contains(absent), "{absent} in {body}");
    }
}

#[test]
fn messages_and_summary_drop_tool_io_and_reasoning() {
    let (_, body) = prepared(Detail::Messages);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["detail"], "messages");
    assert_eq!(v["turns"][1]["tool_calls"], json!([{"name": "Bash"}]));
    assert!(!body.contains("\"reasoning\""));
    assert!(!body.contains("cargo test\""));
    let mut long = colony("c1");
    long.instructions = "x".repeat(5000);
    let collected = record::collect(&record::row(&long), &events()).unwrap();
    let (_, body) = record::prepare(collected, Detail::Summary, &Scrubber::default());
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["turns"][0]["text"].as_str().unwrap().len(), 2000);
}

#[test]
fn a_resume_and_a_bare_issue_read_as_events() {
    let mut s = colony("c2");
    s.instructions.clear();
    let evs = record::parse_events(
        b"{\"seq\":1,\"ts\":\"a\",\"type\":\"user_message\",\"origin\":\"user\",\"id\":\"initial\",\"text\":\"p\"}\n{\"seq\":1,\"ts\":\"b\",\"type\":\"user_message\",\"origin\":\"user\",\"id\":\"initial\",\"text\":\"p\"}\n",
    );
    // Two files' worth of events in one buffer would dedupe; feed them as two files do.
    let mut all = evs.clone();
    all.extend(record::parse_events(
        b"{\"seq\":1,\"ts\":\"c\",\"type\":\"user_message\",\"origin\":\"user\",\"id\":\"initial\",\"text\":\"p\"}\n",
    ));
    let r = record::collect(&record::row(&s), &all).unwrap();
    let texts: Vec<&str> = r.turns.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["Colony started on issue #42: Discount the cart", "Colony resumed"]);
}

#[test]
fn nothing_unredacted_leaves_the_record_or_the_note() {
    let s = colony("c1");
    let mut evs = events();
    evs.push(json!({"type":"assistant_text","origin":"agent","message_id":"m9","text":"the saved one is hunter2-the-saved-secret","ts":"t99"}));
    let collected = record::collect(&record::row(&s), &evs).unwrap();
    let (r, body) = record::prepare(collected, Detail::Full, &scrub());
    let note = super::note::body(&r, Some("src1"), &scrub());
    for text in [&body, &note] {
        assert!(!text.contains("SENTINELSENTINEL"), "a provider key leaked: {text}");
        assert!(!text.contains("hunter2-the-saved-secret"), "a saved secret leaked: {text}");
        assert!(!text.contains("/Users/alice"), "the home path leaked: {text}");
    }
    assert!(note.contains("- Issue: #42 Discount the cart"));
    assert!(note.contains("- Pull request: https://github.com/acme/webshop/pull/7"));
    assert!(note.contains("- Outcome: merged"));
    assert!(note.contains("- Detail: full"));
    assert!(note.contains("- Source: src1 (agent_log)"));
    assert!(note.contains("Coding agent session: colonizer in webshop."));
}

#[test]
fn one_colony_is_one_byte_string() {
    assert_eq!(prepared(Detail::Full).1, prepared(Detail::Full).1);
}

#[test]
fn an_oversized_colony_keeps_its_opening_and_says_what_it_cut() {
    let mut evs = Vec::new();
    for i in 0..400 {
        evs.push(json!({"type":"assistant_text","origin":"agent","message_id":format!("m{i}"),"text":"word ".repeat(1000),"ts":format!("t{i}")}));
    }
    let collected = record::collect(&record::row(&colony("c3")), &evs).unwrap();
    let (r, body) = record::prepare(collected, Detail::Full, &Scrubber::default());
    assert!(body.len() <= record::MAX_BODY_BYTES, "{}", body.len());
    assert!(r.truncated && r.omitted_turns > 0);
    assert_eq!(r.turns[0].at.as_deref(), Some("t0"), "the opening stays");
    assert!(
        r.turns[1]
            .text
            .ends_with("earlier turn(s) omitted to fit the upload size cap")
    );
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
fn a_colony_settles_after_half_an_hour_of_quiet() {
    use crate::sessions::SessionStatus;
    let now = std::time::SystemTime::now();
    let old = now - std::time::Duration::from_secs(31 * 60);
    let recent = now - std::time::Duration::from_secs(60);
    assert!(super::queue::settled(SessionStatus::Merged, Some(old), now));
    assert!(super::queue::settled(SessionStatus::WaitingForAnswer, None, now));
    assert!(!super::queue::settled(SessionStatus::Idle, Some(recent), now));
    assert!(!super::queue::settled(SessionStatus::Running, Some(old), now));
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

/// A Claude Code colony as the runner writes it: the orchestrator starts an `Explore` subagent in
/// the foreground, which starts one of its own; a second subagent runs in the background on another
/// model and ends with `subagent_end`.
fn subagent_lines() -> Vec<Value> {
    let explore = json!({"id":"toolu_task1","name":"Explore","description":"Survey the storage layer","model":"claude-haiku-5"});
    let nested = json!({"id":"toolu_inner","name":"general-purpose","description":"Read the tests","model":"claude-haiku-5"});
    let tests = json!({"id":"toolu_task2","name":"general-purpose","description":"Write tests","model":"claude-sonnet-5"});
    vec![
        json!({"seq":1,"ts":"2026-09-24T10:00:00.000Z","type":"user_message","id":"initial","origin":"user","text":"x"}),
        json!({"seq":2,"ts":"2026-09-24T10:00:01.000Z","type":"model_changed","model":"claude-opus-5","origin":"agent"}),
        json!({"seq":3,"ts":"2026-09-24T10:00:02.000Z","type":"tool_call","message_id":"m1","tool_call_id":"toolu_task1","name":"Task","input":{"subagent_type":"Explore","description":"Survey the storage layer","prompt":"Find every write under /Users/alice/repo"},"origin":"agent"}),
        json!({"seq":4,"ts":"2026-09-24T10:00:04.000Z","type":"thinking","message_id":"s1","text":"Start with the store.","agent":explore,"origin":"subagent"}),
        json!({"seq":5,"ts":"2026-09-24T10:00:05.000Z","type":"tool_call","message_id":"s1","tool_call_id":"toolu_r1","name":"Bash","input":{"command":"cat /Users/alice/.env"},"agent":explore,"origin":"subagent"}),
        json!({"seq":6,"ts":"2026-09-24T10:00:06.000Z","type":"tool_result","tool_call_id":"toolu_r1","output":"TOKEN=hunter2-the-saved-secret","is_error":false,"agent":explore,"origin":"subagent"}),
        json!({"seq":7,"ts":"2026-09-24T10:00:07.000Z","type":"tool_call","message_id":"s2","tool_call_id":"toolu_inner","name":"Agent","input":{"subagent_type":"general-purpose","description":"Read the tests","prompt":"look"},"agent":explore,"origin":"subagent"}),
        json!({"seq":8,"ts":"2026-09-24T10:00:08.000Z","type":"assistant_text","message_id":"n1","text":"Two tests.","agent":nested,"origin":"subagent"}),
        json!({"seq":9,"ts":"2026-09-24T10:00:09.000Z","type":"tool_result","tool_call_id":"toolu_inner","output":"Two tests.","is_error":false,"agent":explore,"origin":"subagent"}),
        json!({"seq":10,"ts":"2026-09-24T10:00:10.000Z","type":"assistant_text","message_id":"s3","text":"Three writers.","agent":explore,"origin":"subagent"}),
        json!({"seq":11,"ts":"2026-09-24T10:00:11.000Z","type":"tool_result","tool_call_id":"toolu_task1","output":"Three writers.","is_error":false,"origin":"agent"}),
        json!({"seq":12,"ts":"2026-09-24T10:00:12.000Z","type":"tool_call","message_id":"m2","tool_call_id":"toolu_task2","name":"Task","input":{"subagent_type":"general-purpose","description":"Write tests","run_in_background":true},"origin":"agent"}),
        json!({"seq":13,"ts":"2026-09-24T10:00:13.000Z","type":"tool_result","tool_call_id":"toolu_task2","output":"Started in the background.","is_error":false,"background":true,"origin":"agent"}),
        json!({"seq":14,"ts":"2026-09-24T10:00:15.000Z","type":"assistant_text","message_id":"b1","text":"Writing tests.","agent":tests,"origin":"subagent"}),
        json!({"seq":15,"ts":"2026-09-24T10:02:20.000Z","type":"assistant_text","message_id":"b2","text":"Tests written.","agent":tests,"origin":"subagent"}),
        json!({"seq":16,"ts":"2026-09-24T10:02:21.000Z","type":"subagent_end","tool_call_id":"toolu_task2","status":"stopped","origin":"agent"}),
    ]
}

fn subagent_record(evs: &[Value], detail: Detail) -> (record::SessionRecord, String) {
    let collected = record::collect(&record::row(&colony("c5")), evs).unwrap();
    record::prepare(collected, detail, &scrub())
}

#[test]
fn a_subagents_steps_nest_under_its_call_with_its_name_and_model() {
    let (_, body) = subagent_record(&subagent_lines(), Detail::Full);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["schema_version"], 3);
    let turns = v["turns"].as_array().unwrap();
    let explore = &turns[1]["tool_calls"][0];
    assert_eq!(explore["name"], "Task");
    assert_eq!(explore["output"], "Three writers.");
    let sub = &explore["subagent"];
    assert_eq!(sub["id"], "toolu_task1");
    assert_eq!(sub["name"], "Explore");
    assert_eq!(sub["description"], "Survey the storage layer");
    assert_eq!(sub["models"], json!(["claude-haiku-5"]));
    assert_eq!(sub["status"], "completed", "answered in the foreground");
    assert_eq!(sub["started_at"], "2026-09-24T10:00:02.000Z", "from its launch");
    assert_eq!(sub["ended_at"], "2026-09-24T10:00:11.000Z", "to its result");
    let inner: Vec<&str> = sub["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["role"].as_str().unwrap())
        .collect();
    assert_eq!(inner, ["reasoning", "assistant", "assistant", "assistant"]);
    assert_eq!(sub["turns"][1]["model"], "claude-haiku-5", "each list names its own model");
    assert_eq!(sub["turns"][1]["tool_calls"][0]["input"], "cat ~/.env");
    // The subagent it started nests under its own call.
    let nested = &sub["turns"][2]["tool_calls"][0]["subagent"];
    assert_eq!(nested["name"], "general-purpose");
    assert_eq!(nested["description"], "Read the tests");
    assert_eq!(nested["turns"][0]["text"], "Two tests.");
    // The background one: its own model, ended by subagent_end, its last words the output.
    let background = &turns[2]["tool_calls"][0];
    assert_eq!(background["status"], "interrupted");
    assert_eq!(background["output"], "Tests written.");
    assert_eq!(background["subagent"]["models"], json!(["claude-sonnet-5"]));
    assert_eq!(background["subagent"]["status"], "stopped");
    assert_eq!(background["subagent"]["ended_at"], "2026-09-24T10:02:21.000Z");
    assert!(background["subagent"].get("description").is_some());
    // Messages: names only, but the subagent and its messages stay; its reasoning goes.
    let (_, body) = subagent_record(&subagent_lines(), Detail::Messages);
    let v: Value = serde_json::from_str(&body).unwrap();
    let sub = &v["turns"][1]["tool_calls"][0]["subagent"];
    assert_eq!(v["turns"][1]["tool_calls"][0]["name"], "Task");
    assert!(v["turns"][1]["tool_calls"][0].get("input").is_none());
    assert_eq!(sub["turns"][0]["tool_calls"][0], json!({"name": "Bash"}));
    assert!(!body.contains("\"reasoning\""), "{body}");
    assert_eq!(sub["turns"].as_array().unwrap().last().unwrap()["text"], "Three writers.");
}

#[test]
fn nothing_unredacted_leaves_a_subagents_turns() {
    let mut evs = subagent_lines();
    evs.push(json!({"seq":17,"ts":"2026-09-24T10:03:00.000Z","type":"tool_call","message_id":"m3","tool_call_id":"toolu_task3","name":"Task","input":{"subagent_type":"Explore","description":"check hunter2-the-saved-secret"},"origin":"agent"}));
    evs.push(json!({"seq":18,"ts":"2026-09-24T10:03:01.000Z","type":"assistant_text","message_id":"x1","text":"key sk-ant-api03-SENTINELSENTINELSENTINELSENTINELSENTINEL-abcdef in /Users/alice/x","agent":{"id":"toolu_task3","name":"Explore"},"origin":"subagent"}));
    let (r, body) = subagent_record(&evs, Detail::Full);
    let note = super::note::body(&r, Some("src5"), &scrub());
    for text in [&body, &note] {
        assert!(!text.contains("hunter2-the-saved-secret"), "a saved secret leaked: {text}");
        assert!(!text.contains("SENTINELSENTINEL"), "a provider key leaked: {text}");
        assert!(!text.contains("/Users/alice"), "the home path leaked: {text}");
    }
    assert!(body.contains("[REDACTED:secret]"), "{body}");
    assert!(
        note.contains("> 🤖 **Explore** · claude-haiku-5 · 9s · completed · 4 step(s) — Survey the storage layer"),
        "{note}"
    );
    assert!(note.contains("> Three writers."), "{note}");
    assert!(note.contains("- Subagents: 4"), "{note}");
}

#[test]
fn the_size_cap_trims_subagents_first_and_keeps_their_answers() {
    let mut evs = vec![json!({"seq":1,"ts":"t0","type":"user_message","id":"initial","origin":"user","text":"x"})];
    for task in 0..3 {
        let id = format!("toolu_task{task}");
        let agent = json!({"id": id, "name": "Explore"});
        evs.push(json!({"type":"tool_call","message_id":format!("m{task}"),"tool_call_id":id,"name":"Task","input":{"subagent_type":"Explore"},"origin":"agent","ts":format!("t{task}")}));
        for step in 0..150 {
            evs.push(json!({"type":"assistant_text","message_id":format!("s{task}-{step}"),"text":"word ".repeat(600),"agent":agent,"origin":"subagent","ts":format!("t{task}-{step}")}));
        }
        evs.push(json!({"type":"assistant_text","message_id":format!("s{task}-end"),"text":format!("Answer {task}."),"agent":agent,"origin":"subagent","ts":format!("t{task}-end")}));
        evs.push(json!({"type":"tool_result","tool_call_id":id,"output":format!("Answer {task}."),"is_error":false,"origin":"agent","ts":format!("t{task}-done")}));
    }
    evs.push(json!({"type":"assistant_text","origin":"agent","message_id":"last","text":"All done.","ts":"t9"}));
    let collected = record::collect(&record::row(&colony("c6")), &evs).unwrap();
    let (r, body) = record::prepare(collected, Detail::Full, &Scrubber::default());
    assert!(body.len() <= record::MAX_BODY_BYTES, "{}", body.len());
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(r.truncated);
    assert_eq!(r.omitted_turns, 0, "the orchestrator's own turns all stay");
    assert!(r.omitted_subagent_turns > 0);
    assert_eq!(v["omitted_subagent_turns"], r.omitted_subagent_turns);
    let first = &v["turns"][1]["tool_calls"][0]["subagent"];
    assert_eq!(first["truncated"], true, "the oldest subagent goes first");
    let turns = first["turns"].as_array().unwrap();
    assert!(
        turns[0]["text"]
            .as_str()
            .unwrap()
            .ends_with("earlier turn(s) of this subagent omitted to fit the upload size cap")
    );
    assert_eq!(turns.last().unwrap()["text"], "Answer 0.", "its final answer stays");
    assert_eq!(first["omitted_turns"].as_u64().unwrap() as usize + turns.len() - 1, 151);
    let last = &v["turns"][3]["tool_calls"][0]["subagent"];
    assert_eq!(
        last["truncated"], false,
        "the newest subagent is untouched when the older ones paid"
    );
    assert_eq!(v["turns"].as_array().unwrap().last().unwrap()["text"], "All done.");
    assert_eq!(
        record::prepare(
            record::collect(&record::row(&colony("c6")), &evs).unwrap(),
            Detail::Full,
            &Scrubber::default()
        )
        .1,
        body
    );
}

#[test]
fn a_record_upgrade_requeues_every_colony_already_sent() {
    let fp = super::queue::fingerprint_text("merged", "", 0.5, 100);
    assert!(
        fp.starts_with(&format!("v{}.{}|", record::SCHEMA_VERSION, record::COLLECTOR_VERSION)),
        "{fp}"
    );
    let mut q = super::queue::Queue::default();
    let now = chrono::Utc::now();
    assert!(enqueue(&mut q, "c1", "acme/web", "backfill", "merged||0.500000|100", now));
    let e = q.entries.get_mut("c1").unwrap();
    e.state = EntryState::Sent;
    e.fingerprint = e.pending_fp.clone();
    assert!(
        enqueue(&mut q, "c1", "acme/web", "merged", &fp, now),
        "sent at schema 2: queued again"
    );
    // The backfilled colony predates the rule, and is still looked at because it was sent.
    let old = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let since = Some("2026-06-01T00:00:00Z");
    assert!(!super::queue::sweep_candidate(since, old, false));
    assert!(super::queue::sweep_candidate(since, old, true));
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
    // Settled: nothing written for over half an hour.
    let file = app.cfg.data_dir.join("sessions/c1/events.jsonl");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
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
        assert_eq!(record["schema_version"], 3);
        assert_eq!(record["detail"], "messages");
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
