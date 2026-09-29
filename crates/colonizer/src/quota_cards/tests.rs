use super::*;
use crate::sessions::tests::colony;

/// `bailian` (qwen3.8-max) and `zai` (glm-5), as providers.json holds them.
fn write_providers(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_vec(&json!([
            {"id": "bailian", "name": "Bailian", "base_url": "http://127.0.0.1:1", "auth": "none", "models": ["qwen3.8-max"]},
            {"id": "zai", "name": "Z.AI", "base_url": "http://127.0.0.1:1", "auth": "none", "models": ["glm-5"]},
        ]))
        .unwrap(),
    )
    .unwrap();
}

fn on_bailian(id: &str, org: &str, status: SessionStatus) -> Session {
    let mut s = colony(org, status);
    s.id = id.into();
    s.git_admin_dir = Some("git".into());
    s.allowed_providers = Some(vec!["bailian".into()]);
    s.allowed_models = Some(vec!["bailian/qwen3.8-max".into()]);
    s
}

fn test_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("colonizer-quota-cards-{name}-{}", uuid::Uuid::new_v4()))
}

/// Past the parallel limit, so a resume queues instead of spawning a boot that would flip the
/// status under the assertions (as lifecycle's tests do).
async fn fill_the_parallel_limit(app: &Shared) {
    let max = orgs::global_max_parallel(&app.modules.read().await.clone()) as usize;
    let mut sessions = app.sessions.write().await;
    for i in 0..max {
        let mut filler = colony("filler", SessionStatus::Idle);
        filler.id = format!("filler-{i}");
        sessions.push(filler);
    }
}

async fn act(app: &Shared, body: Value) -> Result<Value, crate::AppError> {
    let req: QuotaActionRequest = serde_json::from_value(body).unwrap();
    quota_action(State(app.clone()), Path("bailian".into()), Json(req))
        .await
        .map(|Json(v)| v)
}

#[test]
fn one_card_per_provider_lists_every_blocked_colony() {
    let providers: Vec<Provider> = serde_json::from_value(json!([
        {"id": "bailian", "name": "Bailian", "base_url": "http://x", "auth": "none", "models": ["qwen3.8-max"]},
        {"id": "zai", "name": "Z.AI", "base_url": "http://x", "auth": "none", "models": ["glm-5"]},
    ]))
    .unwrap();
    let mut sessions: Vec<Session> = (0..3)
        .map(|i| on_bailian(&format!("c{i}"), if i == 2 { "beta" } else { "acme" }, SessionStatus::Running))
        .collect();
    // Finished colonies never make the card, whatever the gateway remembers about them.
    sessions.push(on_bailian("done", "acme", SessionStatus::Merged));
    let hits: HashMap<String, ColonyQuotaHit> = ["c0", "c1", "c2", "done"]
        .iter()
        .map(|id| {
            (
                id.to_string(),
                ColonyQuotaHit {
                    provider: "bailian".into(),
                    hits: 4,
                    since: Utc::now(),
                },
            )
        })
        .collect();
    let exhausted = vec![(
        "bailian".to_string(),
        Some("Oct 1, 16:00 UTC".to_string()),
        Some(1_790_870_400),
    )];
    let picker = vec![
        json!({"id": "sonnet", "healthy": true}),
        json!({"id": "zai/glm-5", "healthy": true}),
        json!({"id": "bailian/qwen3.8-max", "healthy": false}),
    ];
    let cards = build_cards(&exhausted, &providers, &sessions, &hits, &picker);
    assert_eq!(cards.len(), 1, "one card for the provider, not one per colony");
    let card = &cards[0];
    assert_eq!(card["provider"], "bailian");
    assert_eq!(card["title"], "bailian · qwen3.8-max is out of quota");
    assert_eq!(card["reset_unix"], 1_790_870_400);
    let ids: Vec<&str> = card["colonies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["c0", "c1", "c2"]);
    assert_eq!(card["orgs"], json!(["acme", "beta"]));
    let offered: Vec<&str> = card["alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        vec!["sonnet", "zai/glm-5"],
        "the exhausted provider's own models are not offered"
    );

    // A provider with no blocked colony raises no card, and nothing exhausted raises nothing.
    let zai_only = vec![("zai".to_string(), None, None)];
    assert!(build_cards(&zai_only, &providers, &sessions, &hits, &picker).is_empty());
    assert!(build_cards(&[], &providers, &sessions, &hits, &picker).is_empty());
}

#[test]
fn a_quota_park_is_on_the_card_by_its_flag_or_its_error() {
    let ids = vec!["bailian".to_string(), "zai".to_string()];
    let mut named = on_bailian("named", "acme", SessionStatus::Parked);
    named.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "provider": "bailian"}));
    let mut legacy = on_bailian("legacy", "acme", SessionStatus::Stopped);
    legacy.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON}));
    legacy.error = Some("provider quota exhausted (bailian, resets 7am (UTC))".into());
    let mut other = on_bailian("other", "acme", SessionStatus::Parked);
    other.attention = Some(json!({"reason": "hold_timeout"}));
    let sessions = vec![named, legacy, other];
    let hit: Vec<&str> = affected("bailian", &sessions, &HashMap::new(), &ids)
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(hit, vec!["named", "legacy"]);
}

/// The resume scheduler on a fake clock: a waiting colony comes back exactly at its scheduled reset,
/// even while the gateway still reads the provider as out, and at once when the provider recovers.
#[test]
fn a_wait_resumes_at_reset_at_and_not_before() {
    let reset = 1_790_870_400;
    let ids = vec!["bailian".to_string()];
    let mut s = on_bailian("w", "acme", SessionStatus::Parked);
    s.attention = Some(json!({
        "reason": provider_quota::QUOTA_EXHAUSTED_REASON,
        "provider": "bailian",
        "action": WAIT_ACTION,
        "resume_unix": reset,
    }));
    let still_out = |_: &str| true;
    let due = |now| crate::queue::quota_resume_due(&s, &ids, &still_out, true, now);
    assert!(!due(reset - 3600), "an hour early stays parked");
    assert!(!due(reset - 1), "a second early stays parked");
    assert!(due(reset), "due at the reset");
    assert!(due(reset + 60), "and after it");
    let recovered = |_: &str| false;
    assert!(
        crate::queue::quota_resume_due(&s, &ids, &recovered, false, reset - 3600),
        "a provider that recovers early releases the wait early"
    );
    // A colony whose worktree is gone, or that is not quota-parked, is never scheduled.
    let mut gone = s.clone();
    gone.cleaned_up = true;
    assert!(!crate::queue::quota_resume_due(&gone, &ids, &still_out, true, reset));
    let mut held = s.clone();
    held.attention = Some(json!({"reason": "hold_timeout", "resume_unix": reset}));
    assert!(!crate::queue::quota_resume_due(&held, &ids, &still_out, true, reset));
}

#[tokio::test]
async fn switch_changes_the_colonies_model_and_restarts_them() {
    let root = test_root("switch");
    write_providers(&root);
    let app = crate::tests::test_app(&root);
    app.gateway.mark_quota_exhausted(
        "bailian",
        Some("Oct 1, 16:00 UTC".into()),
        Some(Utc::now().timestamp() + 3600),
    );
    {
        let mut sessions = app.sessions.write().await;
        let mut parked = on_bailian("p1", "acme", SessionStatus::Parked);
        parked.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "provider": "bailian"}));
        sessions.push(parked);
        let mut sub = on_bailian("p2", "acme", SessionStatus::Parked);
        sub.subagent_model_override = Some("bailian/qwen3.8-max".into());
        sub.model_override = Some("sonnet".into());
        sub.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "provider": "bailian"}));
        sessions.push(sub);
    }
    for id in ["p1", "p2"] {
        std::fs::create_dir_all(app.session_dir(id)).unwrap();
    }
    fill_the_parallel_limit(&app).await;
    assert_eq!(cards(&app).await.len(), 1);

    let refused = act(&app, json!({"action": "switch", "model": "bailian/qwen3.8-max"})).await;
    assert_eq!(
        refused.unwrap_err().status(),
        StatusCode::BAD_REQUEST,
        "not onto the exhausted provider"
    );
    let refused = act(&app, json!({"action": "switch", "model": "nope/nothing"})).await;
    assert_eq!(
        refused.unwrap_err().status(),
        StatusCode::BAD_REQUEST,
        "only a model on offer"
    );
    let refused = act(&app, json!({"action": "switch", "model": "zai/glm-5", "remember": true})).await;
    assert_eq!(
        refused.unwrap_err().status(),
        StatusCode::BAD_REQUEST,
        "fallback_model is Claude-only"
    );

    let reply = act(&app, json!({"action": "switch", "model": "zai/glm-5", "remember": false}))
        .await
        .unwrap();
    assert_eq!(reply["colonies"], json!(["p1", "p2"]), "{reply}");
    let p1 = app.session("p1").await.unwrap();
    assert_eq!(p1.model_override.as_deref(), Some("zai/glm-5"), "the orchestrator moves");
    assert_eq!(p1.status, SessionStatus::Queued, "restarted: queued to boot on the new model");
    assert!(p1.attention.is_none() && p1.parked.is_none());
    let p2 = app.session("p2").await.unwrap();
    assert_eq!(p2.subagent_model_override.as_deref(), Some("zai/glm-5"), "the subagent moves");
    assert_eq!(
        p2.model_override.as_deref(),
        Some("sonnet"),
        "a role not on the provider stays"
    );
    assert!(cards(&app).await.is_empty(), "nothing left on the card");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_org_switch_moves_the_orgs_settings_and_remembers_a_claude_fallback() {
    let root = test_root("org");
    write_providers(&root);
    std::fs::write(
        root.join("config/orgs.json"),
        serde_json::to_vec(&json!({
            "acme": {"agent": {"model": "bailian/qwen3.8-max", "background_model": "haiku"}},
            "beta": {"agent": {"model": "bailian/qwen3.8-max"}},
        }))
        .unwrap(),
    )
    .unwrap();
    let app = crate::tests::test_app(&root);
    app.gateway.mark_quota_exhausted("bailian", None, None);
    {
        let mut sessions = app.sessions.write().await;
        let mut s = on_bailian("a1", "acme", SessionStatus::Parked);
        s.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "provider": "bailian"}));
        sessions.push(s);
    }
    std::fs::create_dir_all(app.session_dir("a1")).unwrap();
    fill_the_parallel_limit(&app).await;
    let refused = act(&app, json!({"action": "switch", "model": "sonnet", "scope": "everything"})).await;
    assert_eq!(refused.unwrap_err().status(), StatusCode::BAD_REQUEST);
    act(
        &app,
        json!({"action": "switch", "model": "sonnet", "scope": "org", "remember": true}),
    )
    .await
    .unwrap();
    let acme = app.org_settings("acme").agent.unwrap();
    assert_eq!(acme.model.as_deref(), Some("sonnet"), "the org's own setting moves");
    assert_eq!(
        acme.background_model.as_deref(),
        Some("haiku"),
        "a role on another provider stays"
    );
    assert_eq!(
        app.org_settings("beta").agent.unwrap().model.as_deref(),
        Some("bailian/qwen3.8-max"),
        "an org with no colony on the card is left alone"
    );
    let bailian = app.providers().into_iter().find(|p| p.id == "bailian").unwrap();
    assert_eq!(
        bailian.fallback_model.as_deref(),
        Some("sonnet"),
        "remembered as the fallback"
    );
    assert_eq!(app.session("a1").await.unwrap().model_override.as_deref(), Some("sonnet"));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn wait_parks_the_colonies_until_the_reset() {
    let root = test_root("wait");
    write_providers(&root);
    let app = crate::tests::test_app(&root);
    let reset = Utc::now().timestamp() + 7200;
    app.gateway
        .mark_quota_exhausted("bailian", Some("Oct 1, 16:00 UTC".into()), Some(reset));
    app.sessions
        .write()
        .await
        .push(on_bailian("live", "acme", SessionStatus::Running));
    std::fs::create_dir_all(app.session_dir("live")).unwrap();
    app.gateway.note_colony_quota("live", "bailian");
    let reply = act(&app, json!({"action": "wait"})).await.unwrap();
    assert_eq!(reply["colonies"], json!(["live"]), "{reply}");
    let s = app.session("live").await.unwrap();
    assert_eq!(s.status, SessionStatus::Parked, "suspended, not failed");
    let attention = s.attention.clone().unwrap();
    assert_eq!(attention["action"], WAIT_ACTION);
    assert_eq!(attention["provider"], "bailian");
    assert_eq!(attention["resume_unix"], reset, "scheduled for the reset");
    assert!(app.gateway.colony_quota("live").is_none(), "no longer blocked, parked");
    let card = &cards(&app).await[0];
    assert_eq!(card["waiting"], 1);
    assert_eq!(card["resume_unix"], reset, "the card counts down to it");
    let ids = vec!["bailian".to_string(), "zai".to_string()];
    let out = |_: &str| true;
    assert!(!crate::queue::quota_resume_due(&s, &ids, &out, true, reset - 1));
    assert!(crate::queue::quota_resume_due(&s, &ids, &out, true, reset));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn stop_stops_the_colonies() {
    let root = test_root("stop");
    write_providers(&root);
    let app = crate::tests::test_app(&root);
    app.gateway.mark_quota_exhausted("bailian", None, None);
    {
        let mut sessions = app.sessions.write().await;
        sessions.push(on_bailian("r1", "acme", SessionStatus::Running));
        let mut legacy = on_bailian("old", "acme", SessionStatus::Stopped);
        legacy.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON}));
        legacy.error = Some("provider quota exhausted (bailian)".into());
        sessions.push(legacy);
    }
    for id in ["r1", "old"] {
        std::fs::create_dir_all(app.session_dir(id)).unwrap();
    }
    app.gateway.note_colony_quota("r1", "bailian");
    let reply = act(&app, json!({"action": "stop"})).await.unwrap();
    assert_eq!(reply["failed"], json!([]), "{reply}");
    assert_eq!(app.session("r1").await.unwrap().status, SessionStatus::Stopped);
    let old = app.session("old").await.unwrap();
    assert!(old.attention.is_none(), "the legacy park loses its resume ticket");
    assert!(cards(&app).await.is_empty());
    let refused = act(&app, json!({"action": "explode"})).await;
    assert_eq!(refused.unwrap_err().status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(root);
}

/// A colony that never got a turn out because every request hit the exhausted plan does not sit in
/// `starting` unflagged: it is flagged with the quota reason and the provider (issue #760).
#[tokio::test]
async fn a_starting_colony_blocked_on_quota_is_flagged() {
    let root = test_root("starting");
    write_providers(&root);
    let app = crate::tests::test_app(&root);
    app.gateway
        .mark_quota_exhausted("bailian", Some("Oct 1, 16:00 UTC".into()), Some(Utc::now().timestamp() + 60));
    {
        let mut sessions = app.sessions.write().await;
        sessions.push(on_bailian("boot", "acme", SessionStatus::Starting));
        sessions.push(on_bailian("fine", "acme", SessionStatus::Running));
    }
    for id in ["boot", "fine"] {
        std::fs::create_dir_all(app.session_dir(id)).unwrap();
    }
    for _ in 0..3 {
        app.gateway.note_colony_quota("boot", "bailian");
    }
    let blocked = flag_blocked(&app).await;
    assert_eq!(blocked, HashSet::from(["boot".to_string()]));
    let attention = app.session("boot").await.unwrap().attention.unwrap();
    assert_eq!(attention["reason"], provider_quota::QUOTA_EXHAUSTED_REASON);
    assert_eq!(attention["provider"], "bailian");
    assert_eq!(attention["reset_at"], "Oct 1, 16:00 UTC");
    assert!(app.session("fine").await.unwrap().attention.is_none());
    // A success lifts the block: the next pass leaves it alone.
    app.gateway.clear_colony_quota("boot");
    assert!(flag_blocked(&app).await.is_empty());
    let _ = std::fs::remove_dir_all(root);
}
