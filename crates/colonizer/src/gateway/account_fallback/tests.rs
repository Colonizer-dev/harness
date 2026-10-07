use super::*;
use crate::providers::Provider;

fn provider(id: &str, name: &str, trusted: bool) -> Provider {
    serde_json::from_value(json!({
        "id": id, "name": name, "base_url": "http://127.0.0.1:9", "auth": "none", "trusted": trusted,
        "models": ["MiniMax-M3.1"],
    }))
    .unwrap()
}

fn out_until(reset_at: &str, reset_unix: i64) -> QuotaState {
    QuotaState {
        reset_at: Some(reset_at.into()),
        reset_unix: Some(reset_unix),
        since: Utc::now(),
    }
}

const FALLBACK: &str = "minimax/MiniMax-M3.1";

#[test]
fn an_account_that_works_stays_on_claude_whatever_the_fallback() {
    let providers = [provider("minimax", "MiniMax", true)];
    assert_eq!(
        decide(None, FALLBACK, &providers, &|_| false, None, None),
        AccountRoute::Claude
    );
}

#[test]
fn no_fallback_set_keeps_todays_behaviour() {
    let providers = [provider("minimax", "MiniMax", true)];
    let account = out_until("19:51", 1);
    assert_eq!(
        decide(Some(&account), "", &providers, &|_| false, None, None),
        AccountRoute::Claude
    );
    // A bare Claude model, an unconfigured provider, or a provider that is itself out is no fallback.
    for unusable in ["sonnet", "nobody/model", "minimax/"] {
        assert_eq!(
            decide(Some(&account), unusable, &providers, &|_| false, None, None),
            AccountRoute::Claude,
            "{unusable}"
        );
    }
    assert_eq!(
        decide(Some(&account), FALLBACK, &providers, &|id| id == "minimax", None, None),
        AccountRoute::Claude,
        "a fallback provider that is out of quota too cannot take it"
    );
}

#[test]
fn an_exhausted_account_with_a_fallback_routes_to_it_until_the_reset() {
    let providers = [provider("minimax", "MiniMax", false)];
    let account = out_until("19:51", 1_791_229_918);
    assert_eq!(
        decide(Some(&account), FALLBACK, &providers, &|_| false, None, None),
        AccountRoute::Fallback {
            model: FALLBACK.into(),
            provider_name: "MiniMax".into(),
            reset_at: Some("19:51".into()),
            reset_unix: Some(1_791_229_918),
        }
    );
}

#[test]
fn a_restricted_task_falls_back_only_to_a_trusted_provider() {
    let account = out_until("19:51", 1);
    let restricted = Some(Sensitivity::Restricted);
    let untrusted = [provider("minimax", "MiniMax", false)];
    assert_eq!(
        decide(Some(&account), FALLBACK, &untrusted, &|_| false, restricted, None),
        AccountRoute::Parked {
            reason: "needs a trusted provider: Claude is out until 19:51; MiniMax is not marked trusted".into()
        }
    );
    let trusted = [provider("minimax", "MiniMax", true)];
    assert!(matches!(
        decide(Some(&account), FALLBACK, &trusted, &|_| false, restricted, None),
        AccountRoute::Fallback { .. }
    ));
    // The looser classes take an untrusted provider, as every request the gateway carries does.
    assert!(matches!(
        decide(
            Some(&account),
            FALLBACK,
            &untrusted,
            &|_| false,
            Some(Sensitivity::Standard),
            None
        ),
        AccountRoute::Fallback { .. }
    ));
}

#[test]
fn the_reset_time_falls_back_to_the_unix_stamp() {
    assert_eq!(until_words(Some("7am (UTC)"), Some(1)), " until 7am (UTC)");
    assert_eq!(until_words(None, Some(1_791_229_918)), " until 19:51 UTC");
    assert_eq!(until_words(None, None), "");
}

/// A live colony with a gateway token, in an app whose install names `fallback` as its account
/// fallback and whose providers file lists MiniMax (trusted or not).
async fn app_with_colony(
    name: &str,
    trusted: bool,
    fallback: &str,
    sensitivity: Option<&str>,
) -> (Shared, String, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-account-fallback-{name}-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(&[json!({
            "id": "minimax", "name": "MiniMax", "base_url": "http://127.0.0.1:9", "auth": "none",
            "trusted": trusted, "models": ["MiniMax-M3.1"],
        })])
        .unwrap(),
    )
    .unwrap();
    if !fallback.is_empty() {
        app.modules
            .write()
            .await
            .agent
            .settings
            .insert("account_fallback_model".into(), json!(fallback));
    }
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.sensitivity = sensitivity.map(str::to_string);
    app.sessions.write().await.push(colony);
    let token = "a".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    (app, token, root)
}

async fn ask(app: &Shared, token: &str) -> Value {
    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(token).unwrap());
    let response = account_route(State(app.clone()), headers).await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

/// The acceptance path: the account runs out, the colony's Claude requests go to the fallback; the
/// reset lapses the record and routing returns to Claude with no setting touched.
#[tokio::test]
async fn the_account_running_out_routes_to_the_fallback_and_the_reset_restores_claude() {
    let (app, token, root) = app_with_colony("route", false, FALLBACK, None).await;
    assert_eq!(ask(&app, &token).await["action"], "claude", "the account works");

    app.gateway
        .mark_account_quota_exhausted(Some("19:51".into()), Some(Utc::now().timestamp() + 3600));
    let answer = ask(&app, &token).await;
    assert_eq!(answer["action"], "fallback");
    assert_eq!(answer["model"], FALLBACK);
    assert_eq!(answer["reset_at"], "19:51");
    assert!(
        fallback_usable(&app).await.is_some(),
        "the queue reads the same fallback and stops pausing on the account"
    );

    // The reset passes: the record lapses by itself, and the saved setting is exactly as it was.
    app.gateway
        .mark_account_quota_exhausted(Some("19:51".into()), Some(Utc::now().timestamp() - 10));
    assert_eq!(ask(&app, &token).await["action"], "claude", "back on Claude at the reset");
    assert!(fallback_usable(&app).await.is_none());
    assert_eq!(
        configured_model(&app).await,
        FALLBACK,
        "nothing in the saved settings changed, so nothing is switched back by hand"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_restricted_task_with_an_untrusted_fallback_parks_with_the_reason() {
    let (app, token, root) = app_with_colony("restricted", false, FALLBACK, Some("restricted")).await;
    app.gateway
        .mark_account_quota_exhausted(Some("19:51".into()), Some(Utc::now().timestamp() + 3600));
    let answer = ask(&app, &token).await;
    assert_eq!(answer["action"], "parked");
    assert_eq!(
        answer["reason"],
        "needs a trusted provider: Claude is out until 19:51; MiniMax is not marked trusted"
    );
    assert_eq!(
        app.gateway.account_park_reason("c1").as_deref(),
        answer["reason"].as_str(),
        "the park card reads the same reason"
    );
    let (app, token, root2) = app_with_colony("restricted-trusted", true, FALLBACK, Some("restricted")).await;
    app.gateway
        .mark_account_quota_exhausted(Some("19:51".into()), Some(Utc::now().timestamp() + 3600));
    assert_eq!(ask(&app, &token).await["action"], "fallback", "a trusted fallback carries it");
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[tokio::test]
async fn with_no_fallback_set_an_exhausted_account_changes_nothing() {
    let (app, token, root) = app_with_colony("none", true, "", None).await;
    app.gateway
        .mark_account_quota_exhausted(Some("19:51".into()), Some(Utc::now().timestamp() + 3600));
    assert_eq!(ask(&app, &token).await["action"], "claude");
    assert!(fallback_usable(&app).await.is_none(), "the queue still pauses on the account");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_unknown_token_is_refused() {
    let (app, _, root) = app_with_colony("auth", false, FALLBACK, None).await;
    let mut headers = HeaderMap::new();
    headers.insert(
        COLONY_HEADER,
        HeaderValue::from_static("not-a-colony-token-not-a-colony-token"),
    );
    let response = account_route(State(app.clone()), headers).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_colony_log_hears_of_a_switch_once() {
    let dir = std::env::temp_dir().join(format!("colonizer-account-notes-{}", uuid::Uuid::new_v4()));
    let gateway = Gateway::new(&dir).unwrap();
    let fallback = AccountRoute::Fallback {
        model: FALLBACK.into(),
        provider_name: "MiniMax".into(),
        reset_at: None,
        reset_unix: None,
    };
    assert!(
        !gateway.account_note("c1", &AccountRoute::Claude),
        "Claude to Claude is not news"
    );
    assert!(gateway.account_note("c1", &fallback));
    assert!(!gateway.account_note("c1", &fallback), "the second request says nothing");
    assert!(
        gateway.account_note("c1", &AccountRoute::Claude),
        "the return to Claude is said once"
    );
    assert!(!gateway.account_note("c1", &AccountRoute::Claude));
    let _ = std::fs::remove_dir_all(dir);
}
