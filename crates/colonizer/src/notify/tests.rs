use super::*;
use serde_json::{Map, json};

fn settings() -> NotifySettings {
    NotifySettings {
        enabled: true,
        on_question: true,
        on_attention: true,
        on_failed: true,
        on_pull_request: true,
        on_provider: true,
        on_quota: true,
        desktop: false,
        webhook_url: String::new(),
    }
}

/// A provider as `providers.json` holds one: an id and name the operator chose, plus defaults.
fn provider(id: &str, name: &str) -> Provider {
    Provider {
        id: id.into(),
        name: name.into(),
        base_url: "https://provider.test/v1".into(),
        auth: "x-api-key".into(),
        wire: Default::default(),
        models: Vec::new(),
        preset: String::new(),
        timeout_secs: None,
        max_concurrent: None,
        queue_timeout_secs: None,
        context_tokens: None,
        fallback_model: None,
        pricing: None,
        model_map: Default::default(),
        disabled_tools: Vec::new(),
        quota: None,
        normalize_cache_ttl: false,
        trusted: false,
        vetted: false,
        vendor: None,
    }
}

/// Enough requests for the rule to speak: `failures/requests` at the named rate.
fn usage(requests: u64, failures: u64) -> ProviderUsage {
    ProviderUsage {
        requests,
        failures,
        duration_ms: requests * 100,
        ..Default::default()
    }
}

fn seen(status: SessionStatus, attention: Option<&str>) -> Seen {
    Seen {
        status,
        attention: attention.map(String::from),
        rebase_orphaned: false,
    }
}

/// A colony with nothing worth announcing, as the loop would see it between ticks.
fn colony(id: &str, status: SessionStatus) -> Session {
    serde_json::from_value(json!({
        "id": id,
        "repo": "acme/webshop",
        "org": "acme",
        "issue": 42,
        "issue_title": "SENTINEL-issue-title",
        "status": status,
        "branch": "colonizer/SENTINEL-branch",
        "worktree": "/colonizer/worktrees/wt",
        "sandbox": "colonizer-abc123",
        "agent": "claude",
        "pr_url": "https://github.com/acme/webshop/pull/7",
        "error": "SENTINEL-error",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
    }))
    .unwrap()
}

fn fired(settings: &NotifySettings, was: SessionStatus, is: SessionStatus) -> Vec<Event> {
    decide(settings, Some(&seen(was, None)), &seen(is, None))
}

#[test]
fn each_event_fires_once_on_its_edge_and_not_while_it_holds() {
    for (event, status) in [
        (Event::Question, SessionStatus::WaitingForAnswer),
        (Event::Failed, SessionStatus::Failed),
        (Event::PullRequest, SessionStatus::PrOpened),
    ] {
        let s = settings();
        assert_eq!(fired(&s, SessionStatus::Running, status), vec![event]);
        assert!(fired(&s, status, status).is_empty(), "held {:?} is not an edge", status);
        assert!(
            fired(&s, status, SessionStatus::Running).is_empty(),
            "leaving {:?} announces nothing",
            status
        );
        // Leaving and coming back is a new edge, and it fires once again.
        assert!(fired(&s, status, SessionStatus::Running).is_empty());
        assert_eq!(fired(&s, SessionStatus::Running, status), vec![event]);
    }
}

#[test]
fn attention_fires_for_the_watchdogs_reasons_only() {
    let s = settings();
    let edge = |was: Option<&str>, now: Option<&str>| {
        decide(
            &s,
            Some(&seen(SessionStatus::Running, was)),
            &seen(SessionStatus::Running, now),
        )
    };
    assert_eq!(edge(None, Some("stalled")), vec![Event::Attention("stalled")]);
    assert_eq!(
        edge(Some("stalled"), Some("nudges_exhausted")),
        vec![Event::Attention("nudges_exhausted")],
        "out of nudges is a second edge after the stall"
    );
    // Held states are not edges.
    assert!(edge(Some("stalled"), Some("stalled")).is_empty());
    assert!(
        edge(None, Some("waiting_for_answer")).is_empty(),
        "the watchdog's waiting flag is this module's question event, not an attention one"
    );
    assert!(
        edge(None, Some("autopilot_held")).is_empty(),
        "autopilot's flag is not the watchdog's"
    );
    assert!(edge(Some("autopilot_held"), None).is_empty(), "clearing announces nothing");
    // The control-defeat flag (issue #609) is an edge a person hears about, in its own words.
    assert_eq!(edge(None, Some("control_defeat")), vec![Event::Attention("control_defeat")]);
    assert!(
        Event::Attention("control_defeat")
            .text("acme/webshop", Some(42))
            .contains("controls")
    );
}

/// An above-ceiling hold-parked colony announces exactly once (issue #876): the reason change is
/// the edge a person hears about — held, it stays quiet, and clearing it announces nothing.
#[test]
fn a_park_too_risky_to_answer_announces_once() {
    let s = settings();
    let parked = seen(SessionStatus::Parked, Some(crate::queue::HOLD_TIMEOUT_REASON));
    let raised = seen(SessionStatus::Parked, Some(crate::queue::HOLD_UNANSWERED_REASON));
    assert_eq!(
        decide(&s, Some(&parked), &raised),
        vec![Event::Attention(crate::queue::HOLD_UNANSWERED_REASON)],
        "the raised reason is the edge"
    );
    assert!(decide(&s, Some(&raised), &raised).is_empty(), "held, it does not repeat");
    assert!(decide(&s, Some(&raised), &parked).is_empty(), "clearing announces nothing");
}

#[test]
fn rebase_orphaned_becoming_true_fires_once_and_only_with_on_attention() {
    let s = settings();
    let orphaned = |rebase_orphaned: bool| Seen {
        status: SessionStatus::PrOpened,
        attention: None,
        rebase_orphaned,
    };
    assert_eq!(
        decide(&s, Some(&orphaned(false)), &orphaned(true)),
        vec![Event::NeedsRebase],
        "becoming orphaned is the edge"
    );
    assert!(
        decide(&s, Some(&orphaned(true)), &orphaned(true)).is_empty(),
        "held orphaned is not an edge"
    );
    assert!(
        decide(&s, Some(&orphaned(true)), &orphaned(false)).is_empty(),
        "clearing announces nothing"
    );
    let mut off = s.clone();
    off.on_attention = false;
    assert!(
        decide(&off, Some(&orphaned(false)), &orphaned(true)).is_empty(),
        "gated by the same switch as attention"
    );
}

#[test]
fn the_event_switches_decide_what_fires() {
    let mut s = settings();
    s.on_question = false;
    assert!(fired(&s, SessionStatus::Running, SessionStatus::WaitingForAnswer).is_empty());
    assert_eq!(fired(&s, SessionStatus::Running, SessionStatus::Failed), vec![Event::Failed]);
    s = settings();
    s.on_pull_request = false;
    assert!(fired(&s, SessionStatus::Running, SessionStatus::PrOpened).is_empty());
}

#[test]
fn disabled_notify_decides_nothing_and_first_sight_only_seeds() {
    let mut s = settings();
    s.enabled = false;
    assert!(decide(&s, None, &seen(SessionStatus::Failed, Some("stalled"))).is_empty());
    assert!(
        decide(
            &s,
            Some(&seen(SessionStatus::Running, None)),
            &seen(SessionStatus::Failed, None)
        )
        .is_empty()
    );
    assert!(
        decide(&settings(), None, &seen(SessionStatus::Failed, Some("stalled"))).is_empty(),
        "a colony seen for the first time seeds the state instead of announcing a backlog"
    );
}

#[test]
fn the_text_names_the_repo_and_issue_without_repository_content() {
    assert_eq!(
        Event::Question.text("acme/webshop", Some(42)),
        "acme/webshop #42 needs an answer"
    );
    assert_eq!(
        Event::Attention("stalled").text("acme/webshop", Some(42)),
        "acme/webshop #42 has stalled"
    );
    assert_eq!(
        Event::Attention("nudges_exhausted").text("acme/webshop", Some(42)),
        "acme/webshop #42 is out of nudges"
    );
    assert_eq!(Event::Failed.text("acme/webshop", None), "acme/webshop failed");
    assert_eq!(
        Event::PullRequest.text("acme/webshop", None),
        "acme/webshop opened a pull request",
        "a colony with no issue is just the repository"
    );
    let long: String = "r".repeat(MAX_TEXT + 50);
    assert_eq!(
        Event::Failed.text(&long, None).chars().count(),
        MAX_TEXT + 1,
        "capped, ellipsis included"
    );
}

#[test]
fn the_desktop_decision_answers_from_its_inputs_not_the_machine() {
    let linux = DesktopEnv {
        os: "linux",
        notify_send_on_path: true,
        display: Some(":0".into()),
        ..Default::default()
    };
    assert_eq!(desktop_tool(&linux), Ok(Tool::NotifySend));
    let wayland = DesktopEnv {
        os: "linux",
        notify_send_on_path: true,
        wayland_display: Some("wayland-0".into()),
        ..Default::default()
    };
    assert_eq!(desktop_tool(&wayland), Ok(Tool::NotifySend));
    let headless = DesktopEnv {
        os: "linux",
        notify_send_on_path: true,
        ..Default::default()
    };
    assert!(
        desktop_tool(&headless).is_err(),
        "no DISPLAY and no WAYLAND_DISPLAY is no desktop"
    );
    let macos = DesktopEnv {
        os: "macos",
        osascript_on_path: true,
        ..Default::default()
    };
    assert_eq!(desktop_tool(&macos), Ok(Tool::Osascript));
    for mut over_ssh in [linux.clone(), macos.clone()] {
        over_ssh.ssh_connection = Some("203.0.113.7 5222 192.168.0.2 22".into());
        assert!(desktop_tool(&over_ssh).is_err(), "over SSH there is no desktop to notify");
    }
    let no_binary = DesktopEnv {
        os: "linux",
        display: Some(":0".into()),
        ..Default::default()
    };
    assert!(desktop_tool(&no_binary).is_err(), "the tool has to be on the PATH");
    let no_mac_binary = DesktopEnv {
        os: "macos",
        ..Default::default()
    };
    assert!(desktop_tool(&no_mac_binary).is_err());
    assert!(
        desktop_tool(&DesktopEnv {
            os: "windows",
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn applescript_escaping_survives_quotes_and_drops_control_characters() {
    assert_eq!(applescript_string("plain"), "plain");
    assert_eq!(applescript_string(r#"he said "hi""#), r#"he said \"hi\""#);
    assert_eq!(applescript_string("back\\slash"), "back\\\\slash");
    assert_eq!(applescript_string("two\nlines\r\there"), "twolineshere");
    assert_eq!(applescript_string("nul\u{0}byte"), "nulbyte");
}

#[test]
fn the_webhook_payload_carries_no_repository_content() {
    let at = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
    for (event, status) in [
        (Event::Question, SessionStatus::WaitingForAnswer),
        (Event::Attention("stalled"), SessionStatus::Running),
        (Event::Failed, SessionStatus::Failed),
        (Event::PullRequest, SessionStatus::PrOpened),
        (Event::NeedsRebase, SessionStatus::PrOpened),
    ] {
        let session = colony("abc123", status);
        let body = serde_json::to_string(&payload(event, at, &session)).unwrap();
        for sentinel in ["SENTINEL-issue-title", "SENTINEL-branch", "SENTINEL-error"] {
            assert!(!body.contains(sentinel), "{event:?} leaked {sentinel}: {body}");
        }
        let value: Value = serde_json::from_str(&body).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["at", "colony", "event", "id", "pr_url", "provider", "text"],
            "the payload is exactly seven keys, one shape for receivers"
        );
        let mut colony_keys: Vec<&str> = value["colony"].as_object().unwrap().keys().map(String::as_str).collect();
        colony_keys.sort_unstable();
        assert_eq!(colony_keys, ["id", "issue", "org", "repo", "status"]);
        assert_eq!(value["event"], json!(event.name()));
        assert_eq!(value["colony"]["status"], json!(status));
        assert!(
            value["provider"].is_null(),
            "a session event is never about a provider: {body}"
        );
        if matches!(event, Event::PullRequest | Event::NeedsRebase) {
            assert_eq!(
                value["pr_url"],
                json!("https://github.com/acme/webshop/pull/7"),
                "events about a pull request carry its address"
            );
        } else {
            assert!(value["pr_url"].is_null(), "{event:?} carries no pr_url: {body}");
        }
    }
}

#[test]
fn the_webhook_signature_is_pinned() {
    // Computed once with an independent implementation. Pins the whole scheme: HMAC-SHA256 over
    // `"{timestamp}.{body}"`, lowercase hex, wrapped as `sha256=<hex>` by the sender.
    assert_eq!(
        signature("a-signing-secret-for-tests", "1789000000", r#"{"event":"failed"}"#),
        "4f3fc4526050244f4333184258c3b34374cfa8f9ede75b3e94c321fce6d76712"
    );
    // A different timestamp or body signs differently, so both really are covered by the MAC.
    assert_ne!(
        signature("a-signing-secret-for-tests", "1789000001", r#"{"event":"failed"}"#),
        "4f3fc4526050244f4333184258c3b34374cfa8f9ede75b3e94c321fce6d76712"
    );
}

#[test]
fn a_webhook_url_must_be_http_or_https_and_empty_means_off() {
    assert!(webhook_valid("https://example.com/hook"));
    assert!(webhook_valid("http://127.0.0.1:9000/hook"));
    assert!(!webhook_valid(""), "empty is off, which is not the same as invalid");
    assert!(!webhook_valid("file:///etc/passwd"));
    assert!(!webhook_valid("ftp://example.com"));
    assert!(!webhook_valid("example.com/hook"));
}

#[test]
fn diff_seeds_new_colonies_announces_edges_and_prunes_gone_ones() {
    let s = settings();
    let known = colony("abc123", SessionStatus::Failed);
    let fresh = colony("def456", SessionStatus::Queued);
    let mut seen = HashMap::new();
    seen.insert(
        known.id.clone(),
        Seen {
            status: SessionStatus::Running,
            attention: None,
            rebase_orphaned: false,
        },
    );
    // The known colony failed: an edge. The fresh one is seen for the first time: not.
    let list = [known.clone(), fresh.clone()];
    let (events, next) = diff(&list, &seen, |_| s.clone());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0.id, known.id);
    assert_eq!(events[0].1, Event::Failed);
    assert_eq!(next.len(), 2, "the fresh colony is seeded, not announced");
    assert!(next.contains_key(&fresh.id));

    // A colony no longer in the list leaves the map, so it cannot grow forever.
    let remainder = [fresh];
    let (events, next) = diff(&remainder, &next, |_| s.clone());
    assert!(events.is_empty(), "a held state is not an edge");
    assert_eq!(next.len(), 1);
    assert!(!next.contains_key(&known.id));
}

#[test]
fn a_degraded_provider_announces_once_and_rearms_only_below_the_clear_line() {
    let s = settings();
    let degraded = UsageHealth {
        failure_pct: 29.4,
        avg_latency_ms: 1_200,
        rated: true,
        degraded: true,
        last_failure: None,
    };
    let ok = UsageHealth {
        failure_pct: 3.2,
        avg_latency_ms: 900,
        rated: true,
        degraded: false,
        last_failure: None,
    };
    // A provider seen for the first time only seeds, whatever its rate: a restart must not
    // announce every provider that was already failing before it.
    assert_eq!(decide_provider(&s, None, &degraded), (false, true));
    // Holding is not an edge; the crossing announces once.
    assert_eq!(decide_provider(&s, Some(true), &degraded), (false, true));
    assert_eq!(decide_provider(&s, Some(false), &degraded), (true, true));
    assert_eq!(decide_provider(&s, Some(false), &ok), (false, false));
    // Between the lines is not recovered: 9% is under the degraded line but the announcement
    // stays spent, so a rate hovering at the line does not flap.
    let hovering = UsageHealth {
        failure_pct: 9.0,
        avg_latency_ms: 900,
        rated: true,
        degraded: false,
        last_failure: None,
    };
    assert_eq!(decide_provider(&s, Some(true), &hovering), (false, true));
    assert_eq!(decide_provider(&s, Some(false), &hovering), (false, false));
    // Clearly under 8% re-arms, so the next crossing announces again.
    let recovered = UsageHealth {
        failure_pct: 7.9,
        avg_latency_ms: 900,
        rated: true,
        degraded: false,
        last_failure: None,
    };
    assert_eq!(decide_provider(&s, Some(true), &recovered), (false, false));
    assert_eq!(decide_provider(&s, Some(false), &degraded), (true, true));
}

#[test]
fn provider_events_respect_the_switches_and_an_unrated_provider_is_never_degraded() {
    let degraded = UsageHealth {
        failure_pct: 29.4,
        avg_latency_ms: 1_200,
        rated: true,
        degraded: true,
        last_failure: None,
    };
    let mut s = settings();
    s.enabled = false;
    assert!(!decide_provider(&s, Some(false), &degraded).0);
    assert_eq!(
        decide_provider(&s, Some(true), &degraded),
        (false, true),
        "the state is carried through untouched, so re-enabling announces no backlog"
    );
    s = settings();
    s.on_provider = false;
    assert!(!decide_provider(&s, Some(false), &degraded).0);
    // A provider with too few requests to judge is noise, never a degraded one.
    let unrated = UsageHealth {
        failure_pct: 40.0,
        avg_latency_ms: 800,
        rated: false,
        degraded: false,
        last_failure: None,
    };
    for was in [None, Some(false), Some(true)] {
        assert_eq!(decide_provider(&settings(), was, &unrated), (false, false));
    }
}

#[test]
fn diff_providers_seeds_new_ones_announces_crossings_and_prunes_gone_ones() {
    let s = settings();
    let zai = provider("zai", "zai");
    let local = provider("local", "Local model");
    let failing = usage(100, 30);
    let healthy = usage(100, 1);
    let table =
        |zai_usage: ProviderUsage| HashMap::from([("zai".to_string(), zai_usage), ("local".to_string(), healthy.clone())]);
    let read = |table: HashMap<String, ProviderUsage>| move |p: &Provider| table[&p.id].clone();

    // First sight of both: nothing announces, both are seeded with their current state.
    let (events, next) = diff_providers(
        &[zai.clone(), local.clone()],
        &HashMap::new(),
        &s,
        read(table(failing.clone())),
    );
    assert!(events.is_empty(), "first sight only seeds");
    assert_eq!(next, HashMap::from([("zai".to_string(), true), ("local".to_string(), false)]));

    // Holding is not an edge.
    let (events, _) = diff_providers(&[zai.clone(), local.clone()], &next, &s, read(table(failing.clone())));
    assert!(events.is_empty());

    // Clearly recovered re-arms; the next crossing announces once, about the right provider.
    let (_, armed) = diff_providers(&[zai.clone(), local.clone()], &next, &s, read(table(usage(100, 5))));
    assert!(!armed["zai"]);
    let (events, _) = diff_providers(&[zai.clone(), local.clone()], &armed, &s, read(table(failing.clone())));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].provider.id, "zai");
    assert_eq!(events[0].provider.name, "zai");
    assert_eq!(events[0].health.failure_pct, 30.0);
    assert_eq!(events[0].usage.requests, 100);

    // A provider no longer in providers.json leaves the map, so it cannot grow forever.
    let (_, next) = diff_providers(std::slice::from_ref(&local), &armed, &s, read(table(failing.clone())));
    assert!(!next.contains_key("zai"));
}

#[test]
fn the_provider_text_and_payload_carry_no_repository_content() {
    assert_eq!(
        Event::provider_text("zai", 29.4, None),
        "zai is failing 29.4% of its requests"
    );
    assert_eq!(
        Event::provider_text("zai", 29.4, Some("quota_exhausted")),
        "zai is failing 29.4% of its requests; last failure quota_exhausted"
    );
    let long_name: String = "z".repeat(MAX_TEXT + 50);
    assert_eq!(
        Event::provider_text(&long_name, 29.4, None).chars().count(),
        MAX_TEXT + 1,
        "capped, ellipsis included, like the session text"
    );

    let at = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
    let tallies = ProviderUsage {
        requests: 1_000,
        failures: 294,
        duration_ms: 48_000_000,
        ..Default::default()
    };
    let verdict = health(&tallies);
    let body = serde_json::to_string(&provider_payload("zai", "zai", tallies.requests, &verdict, at)).unwrap();
    assert_eq!(
        Event::provider_text("zai", verdict.failure_pct, verdict.last_failure.as_deref()),
        "zai is failing 29.4% of its requests"
    );
    assert!(!body.contains("acme"), "a provider event carries no repository: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["at", "colony", "event", "id", "pr_url", "provider", "text"],
        "one shape with the session payload"
    );
    assert!(value["colony"].is_null(), "no colony behind a provider event: {body}");
    assert!(value["pr_url"].is_null());
    assert_eq!(value["event"], "provider_degraded");
    let mut provider_keys: Vec<&str> = value["provider"].as_object().unwrap().keys().map(String::as_str).collect();
    provider_keys.sort_unstable();
    assert_eq!(
        provider_keys,
        ["avg_latency_ms", "failure", "failure_pct", "id", "name", "requests"]
    );
    assert_eq!(
        value["provider"],
        json!({
            "id": "zai", "name": "zai", "failure_pct": 29.4, "avg_latency_ms": 48_000, "requests": 1_000,
            "failure": null
        })
    );

    // A provider the gateway has seen fail names the code it last failed with.
    let mut last = verdict.clone();
    last.last_failure = Some("unreachable".into());
    let payload = provider_payload("zai", "zai", tallies.requests, &last, at);
    assert_eq!(payload["provider"]["failure"], "unreachable");
    assert_eq!(
        payload["text"],
        "zai is failing 29.4% of its requests; last failure unreachable"
    );
}

#[test]
fn the_notify_module_schema_defaults_to_every_event_and_no_channel() {
    let schema = crate::modules::providers("notify", &[]).remove(0).schema;
    for key in [
        "on_question",
        "on_attention",
        "on_failed",
        "on_pull_request",
        "on_provider",
        "on_quota",
    ] {
        assert_eq!(schema["properties"][key]["default"], json!(true), "{key} is on by default");
    }
    assert_eq!(schema["properties"]["desktop"]["default"], json!(false));
    assert_eq!(schema["properties"]["webhook_url"]["default"], json!(""));
    assert!(
        schema["properties"]["webhook_url"]["description"]
            .as_str()
            .is_some_and(|d| d.contains("no repository content")),
        "the description says what the webhook carries"
    );
}

#[test]
fn notify_settings_survive_a_round_trip_through_the_module_choice() {
    let mut choice = crate::config::ModuleChoice {
        provider: "default".into(),
        enabled: true,
        settings: Map::new(),
    };
    choice.settings.insert("desktop".into(), json!(true));
    let schema = crate::modules::schema_for("notify", "default", &[]);
    let flag = |key: &str| {
        crate::config::setting(&choice, &schema, key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    assert!(flag("desktop"), "an explicit setting wins");
    assert!(flag("on_failed"), "a missing setting falls back to the schema default");
}

/// A notify candidate as `announce` builds one, for the fact-key rules.
fn notify_candidate(event: Event, session: &str, open_question: Option<&str>) -> ledger::Candidate {
    ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("{}:{session}", event.name()),
        class: event.name().to_string(),
        fact: fact_key(event, session, open_question),
        colony: Some(session.to_string()),
        priority: matches!(event, Event::Question),
    }
}

#[test]
fn a_stall_and_an_out_of_nudges_are_two_facts_so_both_announce_within_the_hour() {
    let limits = ledger::Limits::for_kind(ledger::Kind::Notify);
    let t0 = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
    let stalled = notify_candidate(Event::Attention("stalled"), "abc123", None);
    let nudged = notify_candidate(Event::Attention("nudges_exhausted"), "abc123", None);
    assert_ne!(stalled.fact, nudged.fact, "two reasons are two facts, even for one colony");
    let mut st = ledger::LedgerState::default();
    assert_eq!(ledger::check(&st, &limits, &stalled, t0), ledger::Verdict::Deliver);
    ledger::record(&mut st, &stalled, &ledger::Verdict::Deliver, t0);
    // The class-only key would have dropped this as a duplicate of the stall; past the topic
    // cooldown, the second reason announces too — nothing was consumed by the first edge.
    assert_eq!(
        ledger::check(&st, &limits, &nudged, t0 + chrono::Duration::minutes(15)),
        ledger::Verdict::Deliver
    );
}

#[test]
fn the_fact_key_names_the_question_or_nothing_and_never_the_class_alone() {
    assert_eq!(
        fact_key(Event::Question, "abc123", Some("q7")).as_deref(),
        Some("question:abc123:q7")
    );
    assert_eq!(
        fact_key(Event::Question, "abc123", None),
        None,
        "no question to name claims nothing"
    );
    assert_eq!(
        fact_key(Event::Attention("stalled"), "abc123", None).as_deref(),
        Some("attention:stalled:abc123")
    );
    for event in [Event::Failed, Event::PullRequest, Event::NeedsRebase, Event::ProviderDegraded] {
        assert_eq!(
            fact_key(event, "abc123", Some("q7")),
            None,
            "{event:?} claims nothing: the edge fires once"
        );
    }
}

/// Issue #984: an account entering trouble is one notification however many colonies are on it,
/// a repeat is quiet, and clearing announces the resolution once.
#[test]
fn an_account_entering_trouble_announces_once_per_state_change() {
    let trouble = |state| crate::account_health::Trouble {
        state,
        class: "auth".into(),
        status: 401,
        since: Utc::now(),
        cred_stamp: None,
    };
    let now = vec![("default".to_string(), trouble(crate::account_health::State::NeedsSignIn))];
    let (edges, states) = account_edges(&BTreeMap::new(), &now, |_| 10);
    assert_eq!(
        edges,
        vec![AccountEdge::Entered {
            account: "default".into(),
            state: crate::account_health::State::NeedsSignIn,
            waiting: 10,
        }],
        "ten colonies on one account is one line"
    );
    let (again, states) = account_edges(&states, &now, |_| 10);
    assert!(again.is_empty(), "the same state is not an edge");
    let (cleared, _) = account_edges(&states, &[], |_| 0);
    assert_eq!(
        cleared,
        vec![AccountEdge::Cleared {
            account: "default".into()
        }]
    );
}

/// Issue #984: the per-colony path stays quiet for a colony parked waiting on an account — the
/// host-level account edge is the one line, so ten waiting colonies are not ten notifications.
#[test]
fn a_colony_waiting_on_an_account_does_not_notify_per_colony() {
    let s = settings();
    assert!(
        decide(
            &s,
            Some(&seen(SessionStatus::Running, None)),
            &seen(SessionStatus::Parked, Some(crate::account_health::WAITING_FOR_ACCOUNT_REASON)),
        )
        .is_empty(),
        "the account wait is announced once, host-level, not per colony"
    );
}

// -- issue #767: the out-of-quota card's push ------------------------------------------------

/// A card holding `n` colonies on provider `zai`, resetting at a human time.
fn quota_card(n: usize) -> QuotaCard {
    QuotaCard {
        provider: "zai".into(),
        name: "Z.AI".into(),
        reset_at: Some("Oct 6, 04:00 UTC".into()),
        colonies: (0..n).map(|i| colony(&format!("c{i}"), SessionStatus::Running)).collect(),
    }
}

#[test]
fn a_quota_card_announces_once_per_provider_however_many_colonies_it_holds() {
    let s = settings();
    let cards = vec![quota_card(5)];
    let (edges, announced) = quota_edges(&s, &BTreeSet::new(), &cards);
    assert_eq!(edges.len(), 1, "five blocked colonies are one announcement");
    assert_eq!(edges[0].provider, "zai");
    let (again, announced) = quota_edges(&s, &announced, &cards);
    assert!(again.is_empty(), "a card still open is not a new edge");
    // The plan resets and the card closes: the next exhaustion announces again.
    let (closed, announced) = quota_edges(&s, &announced, &[]);
    assert!(closed.is_empty() && announced.is_empty());
    let (reopened, _) = quota_edges(&s, &announced, &cards);
    assert_eq!(reopened.len(), 1);
}

#[test]
fn the_quota_switch_and_the_module_switch_hold_the_card_back() {
    let cards = vec![quota_card(2)];
    for off in [
        NotifySettings {
            on_quota: false,
            ..settings()
        },
        NotifySettings {
            enabled: false,
            ..settings()
        },
    ] {
        let (edges, announced) = quota_edges(&off, &BTreeSet::new(), &cards);
        assert!(edges.is_empty(), "switched off: nothing announces");
        assert!(announced.is_empty(), "and nothing is spent, so switching on announces it");
        let (edges, _) = quota_edges(&settings(), &announced, &cards);
        assert_eq!(edges.len(), 1);
    }
    // Per colony, the quota flag is never an attention event of its own: the card is the line.
    let s = settings();
    assert!(
        decide(
            &s,
            Some(&seen(SessionStatus::Running, None)),
            &seen(SessionStatus::Running, Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON)),
        )
        .is_empty()
    );
}

#[test]
fn the_quota_line_and_webhook_carry_labels_only() {
    let card = quota_card(3);
    assert_eq!(
        quota_text(&card.name, card.reset_at.as_deref(), 3),
        "Z.AI is out of quota: 3 colonies are waiting; resets Oct 6, 04:00 UTC"
    );
    assert_eq!(quota_text("Z.AI", None, 1), "Z.AI is out of quota: 1 colony is waiting");
    let body = quota_payload(&card, Utc::now());
    assert_eq!(body["event"], "provider_quota_exhausted");
    assert_eq!(body["colony"], Value::Null);
    assert_eq!(body["provider"]["id"], "zai");
    assert_eq!(body["provider"]["colonies"], 3);
    assert!(!body.to_string().contains("SENTINEL"), "no colony content: {body}");
}

/// End to end (issue #767): a card holding three colonies is one push to each device that takes
/// it — decrypted, it names the provider and reset, opens the Inbox where the card is, and
/// carries nothing of the colonies — while a device with the event off, one in quiet hours and
/// one scoped to another repo get nothing.
#[tokio::test]
async fn one_push_for_a_card_of_many_colonies_respecting_each_devices_prefs() {
    use crate::push::tests::{Captures, agree, await_captures, capture_server, device, unseal};
    use crate::push_prefs::{QUOTA, QuietHours};
    let captures: Captures = Default::default();
    let addr = capture_server(captures.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-notify-quota-{}", crate::util::short_id()));
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::tests::test_app(&root);
    crate::push::tests::notify_on(&app).await;
    let mut phone = device(&format!("http://{addr}/phone"), [3u8; 16]);
    let mut muted = device(&format!("http://{addr}/muted"), [4u8; 16]);
    muted.subscription.prefs.events.insert(QUOTA.into(), false);
    let mut asleep = device(&format!("http://{addr}/asleep"), [5u8; 16]);
    // Quiet all day but the last minute: whatever the clock says, the push is held.
    asleep.subscription.prefs.quiet = Some(QuietHours { start: 0, end: 1439 });
    asleep.subscription.prefs.questions_break_quiet = true;
    let mut elsewhere = device(&format!("http://{addr}/elsewhere"), [6u8; 16]);
    elsewhere.subscription.prefs.scope = vec!["globex".into()];
    std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
    crate::push::save(
        &app.cfg.config_dir,
        &[
            phone.subscription.clone(),
            muted.subscription.clone(),
            asleep.subscription.clone(),
            elsewhere.subscription.clone(),
        ],
    )
    .unwrap();
    let client = reqwest::Client::new();
    let card = quota_card(3);
    announce_quota(&app, Some(&client), &card, &settings(), &mut Reasons::default()).await;
    await_captures(&captures, 1).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let got: Vec<(String, axum::http::HeaderMap, Vec<u8>)> = captures.lock().unwrap().clone();
    let paths: Vec<&str> = got.iter().map(|(p, ..)| p.as_str()).collect();
    // `asleep` is quiet unless this test runs in the device's last minute of the UTC day.
    let quiet_now = asleep.subscription.prefs.in_quiet(Utc::now().timestamp());
    let want: &[&str] = if quiet_now { &["phone"] } else { &["phone", "asleep"] };
    assert_eq!(paths.len(), want.len(), "one push per device that takes it: {paths:?}");
    for name in want {
        assert!(paths.contains(name), "{name} in {paths:?}");
    }
    let (_, headers, body) = got.iter().find(|(p, ..)| p == "phone").unwrap().clone();
    assert_eq!(headers.get("Urgency").unwrap(), "normal");
    let ua_public = crate::push::tests::ua_public(&phone);
    let sender = &body[21..21 + body[20] as usize];
    let plaintext = unseal(&agree(&mut phone, sender), &ua_public, &[3u8; 16], &body);
    let payload: Value = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(payload["title"], "Provider out of quota");
    assert_eq!(
        payload["body"],
        "Z.AI is out of quota: 3 colonies are waiting; resets Oct 6, 04:00 UTC"
    );
    assert_eq!(payload["url"], "/?view=inbox", "tapping it opens the Inbox card");
    assert_eq!(payload["tag"], "quota-zai", "one notification per provider");
    assert_eq!(payload["silent"], true);
    assert!(payload.get("colony").is_none());
    assert!(!String::from_utf8_lossy(&plaintext).contains("SENTINEL"));
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------
// Stable event ids (issue #896)
// ---------------------------------------------------------------------------

/// What a fake webhook receiver records per POST: the headers and the body.
pub(crate) type Received = std::sync::Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, String)>>>;

/// A webhook receiver on 127.0.0.1 that records every POST and answers with the next status in
/// `answers`, or 200 once they run out.
pub(crate) async fn receiver(received: Received, answers: Vec<u16>) -> String {
    let answers = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(answers)));
    let router = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
            let received = received.clone();
            let answers = answers.clone();
            async move {
                received.lock().unwrap().push((headers, body));
                let status = answers.lock().unwrap().pop_front().unwrap_or(200);
                StatusCode::from_u16(status).unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}/hook")
}

fn is_event_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("evt_") && id[4..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[test]
fn an_event_keeps_its_id_however_often_its_payload_is_built() {
    let session = colony("c1", SessionStatus::Failed);
    let first = payload(Event::Failed, Utc::now(), &session);
    let later = payload(Event::Failed, Utc::now() + chrono::Duration::minutes(5), &session);
    let id = first["id"].as_str().unwrap();
    assert!(is_event_id(id), "{id}");
    assert_eq!(first["id"], later["id"], "the same edge is the same id, whenever it is sent");

    // Anything that makes it a different event makes it a different id.
    let other_event = payload(Event::PullRequest, Utc::now(), &session);
    let stalled = payload(Event::Attention("stalled"), Utc::now(), &session);
    let out_of_nudges = payload(Event::Attention("nudges_exhausted"), Utc::now(), &session);
    let other_colony = payload(Event::Failed, Utc::now(), &colony("c2", SessionStatus::Failed));
    let mut again = session.clone();
    again.updated_at += chrono::Duration::seconds(1);
    let failed_again = payload(Event::Failed, Utc::now(), &again);
    let ids: BTreeSet<String> = [&first, &other_event, &stalled, &out_of_nudges, &other_colony, &failed_again]
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 6, "{ids:?}");
    // The id is a hash: it names no colony content.
    assert!(!first["id"].as_str().unwrap().contains("SENTINEL"));
    assert_eq!(event_id("a", "bc", "d"), event_id("a", "bc", "d"));
    assert_ne!(event_id("a", "bc", "d"), event_id("ab", "c", "d"));
}

#[test]
fn every_host_level_payload_carries_an_id() {
    let at = Utc::now();
    let health = UsageHealth {
        rated: true,
        degraded: true,
        failure_pct: 20.0,
        avg_latency_ms: 10,
        last_failure: None,
    };
    let bodies = [
        provider_payload("zai", "Z.AI", 100, &health, at),
        judge_payload("zai", "http", Some(500), "boom", at),
        quota_payload(&quota_card(2), at),
    ];
    for body in &bodies {
        assert!(is_event_id(body["id"].as_str().unwrap()), "{body}");
    }
    assert_eq!(
        provider_payload("zai", "Z.AI", 100, &health, at)["id"],
        bodies[0]["id"],
        "the same detection is the same id"
    );
    assert_ne!(bodies[0]["id"], bodies[1]["id"]);
}

/// End to end: the webhook carries the event's id in `X-Colonizer-Event-Id` and in the body, the
/// signature still covers the body, and the same event sent again keeps its id.
#[tokio::test]
async fn the_webhook_carries_the_event_id_in_a_header_and_the_body() {
    let received: Received = Default::default();
    let url = receiver(received.clone(), Vec::new()).await;
    let root = std::env::temp_dir().join(format!("colonizer-notify-id-{}", crate::util::short_id()));
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::tests::test_app(&root);
    std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
    write_secret(&secret_file(&app), "whsec-test").unwrap();
    let client = reqwest::Client::new();
    let settings = NotifySettings {
        webhook_url: url,
        ..settings()
    };
    let session = colony("c1", SessionStatus::WaitingForAnswer);
    for _ in 0..2 {
        // Rebuilt each time, as a resend would be: a later `at`, the same id.
        let body = payload(Event::Question, Utc::now(), &session);
        assert!(post_webhook(&app, &client, &body, None, &settings, &mut Reasons::default()).await);
    }
    let got = received.lock().unwrap().clone();
    assert_eq!(got.len(), 2);
    let mut ids = BTreeSet::new();
    for (headers, body) in &got {
        let header = headers.get(EVENT_ID_HEADER).unwrap().to_str().unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["id"], header, "the header and the body carry the same id");
        let timestamp = headers.get("X-Colonizer-Timestamp").unwrap().to_str().unwrap();
        assert_eq!(
            headers.get("X-Colonizer-Signature").unwrap().to_str().unwrap(),
            format!("sha256={}", signature("whsec-test", timestamp, body)),
            "the signature covers the body, id included"
        );
        assert!(!body.contains("whsec-test"), "the secret never travels in the payload");
        ids.insert(header.to_string());
    }
    assert_eq!(ids.len(), 1, "a resent event keeps its id: {ids:?}");
    let _ = std::fs::remove_dir_all(root);
}
