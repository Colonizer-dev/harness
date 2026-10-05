use super::*;
use crate::providers::QuotaProbe;

#[test]
fn upstream_urls_keep_the_base_path_and_reject_traversal() {
    assert_eq!(
        upstream_url("https://api.deepseek.com/anthropic/", "/v1/messages", Some("beta=true")).as_deref(),
        Some("https://api.deepseek.com/anthropic/v1/messages?beta=true")
    );
    assert_eq!(
        upstream_url("http://100.80.225.14:8000", "/v1/models", None).as_deref(),
        Some("http://100.80.225.14:8000/v1/models")
    );
    // A base_url that already ends in /v1 (xai-grok's is https://api.x.ai/v1) must not grow a
    // second one: the gateway drops the guest path's repeat.
    assert_eq!(
        upstream_url("https://api.x.ai/v1", "/v1/chat/completions", None).as_deref(),
        Some("https://api.x.ai/v1/chat/completions")
    );
    assert_eq!(
        upstream_url("https://api.x.ai/v1/", "/v1/responses", None).as_deref(),
        Some("https://api.x.ai/v1/responses")
    );
    assert!(upstream_url("http://h", "/v1/../admin", None).is_none());
    assert!(upstream_url("http://h", "/v1/%2e%2e/admin", None).is_none());
    assert!(upstream_url("http://h", "v1/messages", None).is_none());
}

/// Issue #1018: a base whose path ends in any `/v<digits>` is the provider's documented API root,
/// so the guest path's `/v1` is dropped once; any other base keeps the full path appended.
#[test]
fn upstream_urls_respect_a_versioned_base_path() {
    let cases: &[(&str, &str, &str)] = &[
        // BytePlus ModelArk Coding Plan and Volcengine Ark: openai wire, version other than v1.
        (
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
            "/v1/chat/completions",
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
        ),
        (
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3/",
            "/v1/chat/completions",
            "https://ark.ap-southeast.bytepluses.com/api/coding/v3/chat/completions",
        ),
        (
            "https://ark.cn-beijing.volces.com/api/v3",
            "/v1/chat/completions",
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions",
        ),
        (
            "https://ark.cn-beijing.volces.com/api/v3",
            "/v1/models",
            "https://ark.cn-beijing.volces.com/api/v3/models",
        ),
        (
            "https://example.com/v4//",
            "/v1/responses",
            "https://example.com/v4/responses",
        ),
        // OpenAI and others documenting a /v1 root.
        (
            "https://api.openai.com/v1",
            "/v1/responses",
            "https://api.openai.com/v1/responses",
        ),
        (
            "https://api.openai.com/v1/",
            "/v1/chat/completions",
            "https://api.openai.com/v1/chat/completions",
        ),
        (
            "https://openrouter.ai/api/v1",
            "/v1/chat/completions",
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        (
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            "/v1/chat/completions",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
        ),
        // Z.AI documents /api/paas/v4 for its OpenAI-compatible API.
        (
            "https://api.z.ai/api/paas/v4",
            "/v1/chat/completions",
            "https://api.z.ai/api/paas/v4/chat/completions",
        ),
        // Unversioned bases keep today's behaviour: the full request path is appended.
        (
            "https://api.deepseek.com",
            "/v1/chat/completions",
            "https://api.deepseek.com/v1/chat/completions",
        ),
        (
            "https://api.deepseek.com/",
            "/v1/chat/completions",
            "https://api.deepseek.com/v1/chat/completions",
        ),
        (
            "https://api.deepseek.com/anthropic",
            "/v1/messages",
            "https://api.deepseek.com/anthropic/v1/messages",
        ),
        (
            "https://api.z.ai/api/anthropic",
            "/v1/messages",
            "https://api.z.ai/api/anthropic/v1/messages",
        ),
        (
            "https://ark.ap-southeast.bytepluses.com/api/coding",
            "/v1/messages",
            "https://ark.ap-southeast.bytepluses.com/api/coding/v1/messages",
        ),
        (
            "http://192.168.1.20:11434",
            "/v1/chat/completions",
            "http://192.168.1.20:11434/v1/chat/completions",
        ),
        ("http://localhost:8000/", "/v1/models", "http://localhost:8000/v1/models"),
        // Only the path counts: a host named v1, or a segment that merely starts with v, is no version.
        ("http://v1", "/v1/models", "http://v1/v1/models"),
        ("http://v1:8080", "/v1/models", "http://v1:8080/v1/models"),
        (
            "https://example.com/v1beta",
            "/v1/models",
            "https://example.com/v1beta/v1/models",
        ),
        ("https://example.com/dev", "/v1/models", "https://example.com/dev/v1/models"),
        ("https://example.com/v", "/v1/models", "https://example.com/v/v1/models"),
    ];
    for (base, rest, want) in cases {
        assert_eq!(upstream_url(base, rest, None).as_deref(), Some(*want), "base {base} + {rest}");
    }
    // A path that does not start with /v1/ is appended as is, even to a versioned base.
    assert_eq!(
        upstream_url("https://ark.cn-beijing.volces.com/api/v3", "/v1", None).as_deref(),
        Some("https://ark.cn-beijing.volces.com/api/v3/v1")
    );
}

#[test]
fn upstream_urls_reject_traversal_after_a_versioned_base() {
    assert!(upstream_url("https://ark.cn-beijing.volces.com/api/v3", "/v1/../admin", None).is_none());
    assert!(upstream_url("https://ark.cn-beijing.volces.com/api/v3", "/v1/%2e%2e/admin", None).is_none());
    assert!(
        upstream_url(
            "https://ark.cn-beijing.volces.com/api/v3",
            "/v1/chat/completions",
            Some("a b")
        )
        .is_none()
    );
}

#[test]
fn anthropic_bodies_are_normalized_only_for_providers_with_quirks() {
    let meta = ProviderQuirks {
        strip_cache_ttl: true,
        min_max_tokens: Some(16),
    };
    let body = Bytes::from(
        r#"{"model":"m","max_tokens":4,
                "system":[{"type":"text","text":"s","cache_control":{"type":"ephemeral","ttl":"1h"}}],
                "messages":[{"role":"user","content":[
                    {"type":"text","text":"hi","cache_control":{"type":"ephemeral","ttl":"5m"}},
                    {"type":"text","text":"plain"}]}],
                "tools":[{"name":"Bash","input_schema":{"type":"object"},
                    "cache_control":{"type":"ephemeral","ttl":"1h"}}]}"#,
    );
    let (out, note) = normalize_anthropic_body(&body, meta).expect("meta strips ttl and raises max_tokens");
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["max_tokens"], 16, "below the provider floor");
    assert!(
        !String::from_utf8_lossy(&out).contains("ttl"),
        "system blocks, message content blocks and tools all lose ttl"
    );
    assert_eq!(value["system"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(
        value["messages"][0]["content"][1],
        json!({"type": "text", "text": "plain"}),
        "blocks without cache_control are untouched"
    );
    assert!(note.contains("cache_control.ttl"), "the log names the field, got: {note}");
    assert!(note.contains("max_tokens"), "got: {note}");

    // No quirks: the body passes through untouched (None keeps the original bytes).
    assert_eq!(normalize_anthropic_body(&body, ProviderQuirks::default()), None);
    // Quirks but nothing to rewrite: also untouched.
    let clean = Bytes::from(r#"{"model":"m","max_tokens":100,"messages":[]}"#);
    assert_eq!(normalize_anthropic_body(&clean, meta), None);
    // Not JSON at all: never rewritten.
    assert_eq!(normalize_anthropic_body(&Bytes::from("not json"), meta), None);
    // A max_tokens already above the floor stays as the colony sent it.
    let ample = Bytes::from(r#"{"model":"m","max_tokens":1024,"messages":[]}"#);
    assert_eq!(normalize_anthropic_body(&ample, meta), None);
}

#[test]
fn forwarded_headers_drop_colony_credentials_and_oauth_betas() {
    let mut incoming = HeaderMap::new();
    incoming.insert("authorization", HeaderValue::from_static("Bearer colony-placeholder"));
    incoming.insert("x-api-key", HeaderValue::from_static("placeholder"));
    incoming.insert(COLONY_HEADER, HeaderValue::from_static("token"));
    incoming.insert("content-type", HeaderValue::from_static("application/json"));
    incoming.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    incoming.insert(
        "anthropic-beta",
        HeaderValue::from_static("oauth-2025-04-20, interleaved-thinking-2025-05-14"),
    );
    let out = forward_headers(
        &incoming,
        Some((HeaderName::from_static("x-api-key"), HeaderValue::from_static("real"))),
    );
    assert_eq!(out.get("x-api-key").unwrap(), "real");
    assert!(out.get("authorization").is_none());
    assert!(out.get(COLONY_HEADER).is_none());
    assert_eq!(out.get("anthropic-beta").unwrap(), "interleaved-thinking-2025-05-14");
    assert_eq!(out.get("content-type").unwrap(), "application/json");

    let mut only_oauth = HeaderMap::new();
    only_oauth.insert("anthropic-beta", HeaderValue::from_static("oauth-2025-04-20"));
    assert!(forward_headers(&only_oauth, None).get("anthropic-beta").is_none());
}

/// A keyed provider with no saved key is refused before any upstream call: the request would
/// otherwise go out with no credential and come back as an unexplained 401.
#[tokio::test]
async fn a_keyed_provider_without_a_saved_key_is_refused_before_any_upstream_call() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-nokey-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    // Routed to deepseek, so the allowlist lets it by and the missing key is what refuses.
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    // Port 9 (discard) is never reached: nothing may be sent upstream.
    std::fs::write(
        root.join("config/providers.json"),
        r#"[{"id":"deepseek","name":"DeepSeek","base_url":"http://127.0.0.1:9","auth":"x-api-key"}]"#,
    )
    .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(&token).unwrap());
    let response = proxy(
        State(app.clone()),
        Path(("deepseek".into(), "v1/messages".into())),
        Method::POST,
        "/providers/deepseek/v1/messages".parse().unwrap(),
        headers,
        Bytes::from_static(b"{}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"deepseek\""), "names the provider: {message}");
    assert!(
        message.contains("Settings → Model providers"),
        "says where to fix it: {message}"
    );
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        0,
        "refused locally, so nothing counts as provider usage"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "missing_key");
    assert_eq!(lines[0]["status"], 502);
    assert_eq!(lines[0]["fallback"], false, "a missing key earns no Claude retry");
    let _ = std::fs::remove_dir_all(root);
}

/// A configured provider the colony's model settings never routed to is refused before any
/// upstream call: providers.json is mothership-wide, and one colony's token must not open
/// another colony's provider (issue #409).
#[tokio::test]
async fn a_provider_outside_the_colonys_routing_is_refused_before_any_upstream_call() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-allowlist-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    // Routed to kimi only: deepseek is configured on the mothership but not this colony's.
    colony.allowed_providers = Some(vec!["kimi".into()]);
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    // Port 9 (discard) is never reached: nothing may be sent upstream. Both providers are
    // auth "none" so the missing-key refusal cannot mask the one under test.
    std::fs::write(
            root.join("config/providers.json"),
            r#"[{"id":"kimi","name":"Kimi","base_url":"http://127.0.0.1:9","auth":"none"},{"id":"deepseek","name":"DeepSeek","base_url":"http://127.0.0.1:9","auth":"none"}]"#,
        )
        .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(&token).unwrap());
    let response = proxy(
        State(app.clone()),
        Path(("deepseek".into(), "v1/messages".into())),
        Method::POST,
        "/providers/deepseek/v1/messages".parse().unwrap(),
        headers,
        Bytes::from_static(b"{}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["type"], "permission_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"deepseek\""), "names the provider: {message}");
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        0,
        "refused locally, so nothing counts as provider usage"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "not_routed");
    assert_eq!(lines[0]["status"], 403);
    let _ = std::fs::remove_dir_all(root);
}

/// The allowlist admits the provider the colony is routed to: the request dispatches and
/// counts as usage instead of coming back 403 (issue #409).
#[tokio::test]
async fn a_provider_within_the_colonys_routing_is_still_dispatched() {
    let router = Router::new().route("/v1/messages", axum::routing::post(|| async { axum::Json(json!({})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-routed-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.allowed_models = Some(vec!["deepseek/deepseek-chat".into()]);
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(&[json!({
            "id": "deepseek",
            "name": "DeepSeek",
            "base_url": format!("http://{addr}"),
            "auth": "none",
        })])
        .unwrap(),
    )
    .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(&token).unwrap());
    let response = proxy(
        State(app.clone()),
        Path(("deepseek".into(), "v1/messages".into())),
        Method::POST,
        "/providers/deepseek/v1/messages".parse().unwrap(),
        headers,
        Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "routed provider is not refused");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        1,
        "passed the allowlist, so the request dispatched"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1, "exactly one audit line per request");
    assert_eq!(lines[0]["failure"], Value::Null, "a forwarded 2xx did not fail");
    assert_eq!(
        lines[0]["model"], "deepseek-chat",
        "the line names the model the body asked for"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A provider answering 429 quota_exhausted to three colonies raises exactly one
/// "Provider out of quota" card listing all three (issue #767), with the reset the error named;
/// a later success for one colony takes it off the card.
#[tokio::test]
async fn a_quota_exhausted_provider_raises_one_card_for_every_blocked_colony() {
    let router = Router::new().route(
        "/v1/messages",
        axum::routing::post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({"type": "error", "error": {"type": "rate_limit_error",
                        "message": "quota_exhausted: the token plan quota has been exhausted, resets 2099-10-01T16:00:00Z"}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-quota-card-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(&[json!({
            "id": "bailian", "name": "Bailian", "base_url": format!("http://{addr}"), "auth": "none",
            "models": ["qwen3.8-max"],
        })])
        .unwrap(),
    )
    .unwrap();
    let mut tokens = Vec::new();
    for i in 0..3 {
        let id = format!("c{i}");
        let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
        colony.id = id.clone();
        colony.allowed_providers = Some(vec!["bailian".into()]);
        colony.allowed_models = Some(vec!["bailian/qwen3.8-max".into()]);
        app.sessions.write().await.push(colony);
        let token = format!("{i}").repeat(40);
        std::fs::create_dir_all(app.session_dir(&id)).unwrap();
        std::fs::write(app.gateway_token_file(&id), &token).unwrap();
        tokens.push(token);
    }
    for token in &tokens {
        for _ in 0..2 {
            let response = post_to_gateway(
                &app,
                token,
                "bailian",
                HeaderMap::new(),
                Bytes::from_static(br#"{"model":"qwen3.8-max","max_tokens":8,"messages":[]}"#),
            )
            .await;
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
            let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        }
    }
    let cards = crate::quota_cards::cards(&app).await;
    assert_eq!(cards.len(), 1, "one card for the provider: {cards:?}");
    let card = &cards[0];
    assert_eq!(card["provider"], "bailian");
    assert_eq!(card["title"], "bailian · qwen3.8-max is out of quota");
    assert_eq!(card["reset_at"], "2099-10-01T16:00:00Z");
    let listed: Vec<&str> = card["colonies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, vec!["c0", "c1", "c2"]);
    assert_eq!(card["colonies"][0]["hits"], 2);
    // A success for c1 (the success paths all clear the colony's block) takes it off the card.
    clear_model_error(&app, "c1").await;
    let cards = crate::quota_cards::cards(&app).await;
    assert_eq!(cards[0]["colonies"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// A provider whose remembered fallback is a model on another anthropic-wire provider (issue
/// #767): its 429 quota_exhausted is retried by the gateway on that provider with the model
/// swapped, and the colony gets the fallback's answer — not blocked, and no Claude licence for
/// the router. A cross-wire fallback (a hand-edited file) is not retried: the 429 stands and the
/// colony is blocked on the provider.
#[tokio::test]
async fn a_quota_exhausted_provider_is_retried_on_its_same_wire_fallback_provider() {
    let quota = Router::new().route(
        "/v1/messages",
        axum::routing::post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({"type": "error", "error": {"type": "rate_limit_error",
                        "message": "quota_exhausted: the token plan quota has been exhausted"}})),
            )
        }),
    );
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_by = seen.clone();
    let fallback = Router::new().route(
        "/v1/messages",
        axum::routing::post(move |body: Bytes| {
            let seen = seen_by.clone();
            async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                seen.lock()
                    .unwrap()
                    .push(request["model"].as_str().unwrap_or_default().to_string());
                axum::Json(
                    json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "glm-5",
                        "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
                        "usage": {"input_tokens": 3, "output_tokens": 1}}),
                )
            }
        }),
    );
    let serve = |router: Router| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    };
    let (quota_addr, fallback_addr) = (serve(quota).await, serve(fallback).await);
    let root = std::env::temp_dir().join(format!("colonizer-gateway-provider-fallback-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    let write = |fallback_model: &str| {
        std::fs::write(
            root.join("config/providers.json"),
            serde_json::to_string(&[
                json!({"id": "bailian", "name": "Bailian", "base_url": format!("http://{quota_addr}"), "auth": "none",
                        "models": ["qwen3.8-max"], "fallback_model": fallback_model}),
                json!({"id": "zai", "name": "Z.AI", "base_url": format!("http://{fallback_addr}"), "auth": "none",
                        "models": ["glm-5"]}),
                json!({"id": "grok", "name": "xAI", "base_url": format!("http://{fallback_addr}/v1"), "auth": "none",
                        "wire": "openai", "models": ["grok-5"]}),
            ])
            .unwrap(),
        )
        .unwrap();
    };
    write("zai/glm-5");
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    // Only bailian is the colony's: the fallback hop is the operator's, not the colony's scope.
    colony.allowed_providers = Some(vec!["bailian".into()]);
    colony.allowed_models = Some(vec!["bailian/qwen3.8-max".into()]);
    app.sessions.write().await.push(colony);
    let token = "f".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    let body = || Bytes::from_static(br#"{"model":"qwen3.8-max","max_tokens":8,"messages":[]}"#);

    let response = post_to_gateway(&app, &token, "bailian", HeaderMap::new(), body()).await;
    assert_eq!(response.status(), StatusCode::OK, "answered by the fallback provider");
    let answer: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(answer["content"][0]["text"], "ok");
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["glm-5".to_string()],
        "the model swapped to the fallback's"
    );
    assert!(app.gateway.is_quota_exhausted("bailian"), "the provider is still marked out");
    assert!(
        app.gateway.colony_quota("c1").is_none(),
        "a colony served by the fallback is not blocked"
    );

    // Cross-wire: not retried, the 429 stands without the router's Claude licence, and the
    // colony is blocked on the provider.
    write("grok/grok-5");
    let response = post_to_gateway(&app, &token, "bailian", HeaderMap::new(), body()).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get(FALLBACK_HEADER).is_none(),
        "no Claude retry for a provider fallback"
    );
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1, "nothing reached the other wire");
    assert_eq!(app.gateway.colony_quota("c1").map(|h| h.provider), Some("bailian".into()));
    let _ = std::fs::remove_dir_all(root);
}

/// The audit lines one colony's `gateway.jsonl` holds, parsed. Only meaningful once every
/// response body under test has been drained: a streamed line lands when the body ends.
fn audit_lines(app: &Shared, colony: &str) -> Vec<Value> {
    std::fs::read_to_string(app.session_dir(colony).join("gateway.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Serves any path and method, counting what arrives. The zero the tests assert against says
/// nothing reached the provider.
async fn counting_upstream(hits: Arc<AtomicU64>) -> String {
    let router = Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            axum::Json(json!({}))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A running colony `c1` — token file written, session dir created — routed to `allowed` for
/// providers and `models` (`"<provider>/<model>"` pairs) for models, with `providers` as the
/// mothership's providers.json. Returns the app and the colony token.
async fn colony_with_providers(root: &std::path::Path, allowed: &[&str], models: &[&str], providers: Value) -> (Shared, String) {
    let app = crate::tests::test_app(root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(allowed.iter().map(|id| id.to_string()).collect());
    colony.allowed_models = Some(models.iter().map(|m| m.to_string()).collect());
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/providers.json"), serde_json::to_string(&providers).unwrap()).unwrap();
    (app, token)
}

/// Serves one POST route that captures every request's headers and answers `body`. Returns the
/// base URL and the headers of the last request it saw.
async fn capturing_upstream(path: &str, body: Value) -> (String, Arc<Mutex<HeaderMap>>) {
    let seen = Arc::new(Mutex::new(HeaderMap::new()));
    let capture = seen.clone();
    let router = Router::new().route(
        path,
        axum::routing::post(move |headers: HeaderMap| {
            let capture = capture.clone();
            let body = body.clone();
            async move {
                *capture.lock().unwrap() = headers;
                axum::Json(body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), seen)
}

/// The placeholder credentials a colony's runner forwards, which must never reach an upstream.
fn placeholder_credentials() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("sk-ant-api03-FAKEFAKEFAKEFAKE"));
    headers.insert("authorization", HeaderValue::from_static("Bearer sk-FAKEFAKEFAKEFAKE"));
    headers
}

/// POSTs `body` to `provider` on the gateway as a colony carrying `token` and `credentials`.
async fn post_to_gateway(app: &Shared, token: &str, provider: &str, credentials: HeaderMap, body: Bytes) -> Response {
    gateway_request(app, token, provider, Method::POST, "v1/messages", credentials, body).await
}

/// Sends any method and provider-relative path to the gateway as colony `c1`'s token.
async fn gateway_request(
    app: &Shared,
    token: &str,
    provider: &str,
    method: Method,
    rest: &str,
    credentials: HeaderMap,
    body: Bytes,
) -> Response {
    let mut headers = credentials;
    headers.insert(COLONY_HEADER, HeaderValue::from_str(token).unwrap());
    proxy(
        State(app.clone()),
        Path((provider.to_string(), rest.into())),
        method,
        format!("/providers/{provider}/{rest}").parse().unwrap(),
        headers,
        body,
    )
    .await
}

/// The request shapes an anthropic-wire endpoint does not serve — a path outside the Messages
/// API, a GET on one it does — are refused before any upstream traffic: the provider's
/// credential is never attached to a request its endpoint did not ask for.
#[tokio::test]
async fn an_unserved_path_or_method_is_refused_before_any_upstream_call() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-shape-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none"}]),
    )
    .await;
    let body = Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#);

    let response = gateway_request(
        &app,
        &token,
        "deepseek",
        Method::POST,
        "v1/models",
        HeaderMap::new(),
        body.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response = gateway_request(
        &app,
        &token,
        "deepseek",
        Method::GET,
        "v1/messages",
        HeaderMap::new(),
        Bytes::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the provider");
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["failure"], "bad_request");
    assert_eq!(lines[1]["failure"], "bad_request");
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        0,
        "refused locally, so nothing counts as provider usage"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A request whose body names a model the colony's agent settings never routed to is refused
/// before any upstream call, like a provider outside the routing is (issue #681).
/// A colony booted before model scoping (#727) has providers on record but no models. It keeps
/// the provider-level scope it booted with, instead of every request being refused mid-run
/// (every subagent 403'd on the upgrade), and a provider outside its record is still refused.
#[tokio::test]
async fn a_colony_booted_before_model_scoping_keeps_its_provider_scope() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-legacy-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &[],
        json!([
            {"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none"},
            {"id": "other", "name": "Other", "base_url": base, "auth": "none"}
        ]),
    )
    .await;
    app.sessions.write().await.iter_mut().for_each(|s| s.allowed_models = None);

    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"any-model-on-deepseek","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_ne!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a legacy colony's recorded provider still serves it"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the request reached the provider");

    let response = post_to_gateway(
        &app,
        &token,
        "other",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"x","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a provider outside the record is still refused"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_model_outside_the_colonys_routing_is_refused_before_any_upstream_call() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-model-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none"}]),
    )
    .await;

    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"unrouted-model","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "permission_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("unrouted-model"), "names the model: {message}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the provider");
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "not_routed");

    // The routed model on the same provider still passes: the pair on record is what admits.
    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "the routed model is admitted");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["failure"], Value::Null);
    assert_eq!(lines[1]["model"], "deepseek-chat");
    let _ = std::fs::remove_dir_all(root);
}

/// A request that waited in the provider's queue is not sent once the colony it was
/// authenticated for has gone: the slot freeing re-checks the token and the budget before
/// anything leaves for the provider (issue #681).
#[tokio::test]
async fn a_queued_request_is_not_sent_after_the_colony_is_gone() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-queued-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none",
                    "max_concurrent": 1, "queue_timeout_secs": 30}]),
    )
    .await;
    let body = Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#);

    // The provider's only slot is held, so the request below queues behind it.
    let held = app.gateway.slots("deepseek", Some(1)).unwrap().acquire_owned().await.unwrap();
    let task = {
        let app = app.clone();
        let token = token.clone();
        tokio::spawn(async move { post_to_gateway(&app, &token, "deepseek", HeaderMap::new(), body).await })
    };
    for _ in 0..1000 {
        if app.gateway.load("deepseek").1 > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        app.gateway.load("deepseek").1,
        1,
        "the request is waiting on the provider's slot"
    );

    // The colony goes away while the request waits: stopped, and its token no longer any good.
    app.update_session("c1", |x| x.status = crate::sessions::SessionStatus::Stopped)
        .await;
    std::fs::write(app.gateway_token_file("c1"), b"rotated").unwrap();
    drop(held);

    let response = task.await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "permission_error");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the queued request never reached the provider"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose queue is already at its cap has further requests refused on arrival rather
/// than queued behind it (issue #681): agents work through parallel subagents, so bursts are
/// routine, and an unbounded wait would pile latency and held estimates on the colony.
#[tokio::test]
async fn a_colony_with_a_full_queue_is_refused_without_queueing() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-queuecap-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none",
                    "max_concurrent": 1, "queue_timeout_secs": 30}]),
    )
    .await;
    let body = Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#);

    // The provider's only slot is held, so the first COLONY_QUEUE_CAP requests queue and the
    // next one arrives to a full queue.
    let held = app.gateway.slots("deepseek", Some(1)).unwrap().acquire_owned().await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..COLONY_QUEUE_CAP {
        tasks.push({
            let app = app.clone();
            let token = token.clone();
            let body = body.clone();
            // The status only: a response held in a finished task keeps its slot until it is dropped,
            // and the tasks need not reach the queue in the order they were spawned.
            tokio::spawn(async move {
                post_to_gateway(&app, &token, "deepseek", HeaderMap::new(), body)
                    .await
                    .status()
            })
        });
    }
    for _ in 0..1000 {
        if app.gateway.load("deepseek").1 == COLONY_QUEUE_CAP {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        app.gateway.load("deepseek").1,
        COLONY_QUEUE_CAP,
        "the cap's worth are waiting"
    );

    let response = post_to_gateway(&app, &token, "deepseek", HeaderMap::new(), body).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let refused: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(refused["error"]["type"], "overloaded_error");

    drop(held);
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::OK);
    }
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.iter().filter(|l| l["failure"] == "queue_full").count(), 1);
    assert_eq!(
        lines.len(),
        COLONY_QUEUE_CAP as usize + 1,
        "each request leaves exactly one line"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A colony with no recorded allowlist — the field is filled in at boot, before the token is
/// written — reaches no provider: the record is what the token's access is derived from, so
/// without it there is nothing to admit (issue #681).
#[tokio::test]
async fn a_colony_without_a_recorded_allowlist_is_refused_every_provider() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-noallow-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = None;
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none"}]).to_string(),
    )
    .unwrap();

    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "permission_error");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the provider");
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "not_routed");
    let _ = std::fs::remove_dir_all(root);
}

/// POSTs `body` to `provider` + `rest` on the gateway as a colony carrying `token` and `credentials`.
async fn post_to_path(app: &Shared, token: &str, provider: &str, rest: &str, credentials: HeaderMap, body: Bytes) -> Response {
    gateway_request(
        app,
        token,
        provider,
        Method::POST,
        rest.trim_start_matches('/'),
        credentials,
        body,
    )
    .await
}

/// Serves one POST route that captures every request's headers and body and answers `status`
/// with `content_type` and `body` exactly as given — the raw upstream a passthrough test needs,
/// where [`capturing_upstream`]'s JSON answer cannot carry an SSE stream or a byte-exact check.
/// Returns the base URL and the last request's headers and body.
async fn raw_upstream(
    path: &'static str,
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
) -> (String, Arc<Mutex<(HeaderMap, Bytes)>>) {
    let seen = Arc::new(Mutex::new((HeaderMap::new(), Bytes::new())));
    let capture = seen.clone();
    let answer = Bytes::from_static(body.as_bytes());
    let router = Router::new().route(
        path,
        axum::routing::post(move |headers: HeaderMap, request: Bytes| {
            let capture = capture.clone();
            let answer = answer.clone();
            async move {
                *capture.lock().unwrap() = (headers, request);
                (
                    status,
                    [(axum::http::header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
                    answer,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}"), seen)
}

/// A forwarded 2xx leaves one audit line per request carrying the outcome and the usage, and
/// none of what the request carried: keys, bearer tokens, the colony's own gateway token, prompt
/// text — the record's fixed struct is the whole allowlist (issue #302). The mock upstreams
/// double as the credential check: on neither wire may the colony's placeholder credentials
/// reach the upstream — the anthropic wire forwards an allowlist, the openai wire builds its
/// headers from scratch.
#[tokio::test]
async fn a_forwarded_request_leaves_one_redacted_audit_line_and_no_colony_credential_upstream() {
    let (anthropic_base, anthropic_seen) = capturing_upstream(
        "/v1/messages",
        json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-sonnet-5", "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }),
    )
    .await;
    let (openai_base, openai_seen) = capturing_upstream(
        "/v1/chat/completions",
        json!({
            "id": "cpl_1", "object": "chat.completion", "created": 1, "model": "gpt-5.5",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
        }),
    )
    .await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-audit-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek", "strix"],
        &["deepseek/claude-sonnet-5", "strix/gpt-5.5"],
        json!([
            {"id": "deepseek", "name": "DeepSeek", "base_url": anthropic_base, "auth": "none"},
            {"id": "strix", "name": "Strix", "base_url": openai_base, "auth": "none", "wire": "openai"},
        ]),
    )
    .await;

    let request_body = Bytes::from(
        r#"{"model":"claude-sonnet-5","max_tokens":8,"messages":[{"role":"user","content":[{"type":"text","text":"SECRET_PROMPT_TEXT"}]}]}"#,
    );
    let response = post_to_gateway(&app, &token, "deepseek", placeholder_credentials(), request_body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response_body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response = post_to_gateway(
        &app,
        &token,
        "strix",
        placeholder_credentials(),
        Bytes::from_static(br#"{"model":"gpt-5.5","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Only the mothership's own saved key may reach an upstream, and neither provider has one.
    for seen in [anthropic_seen, openai_seen] {
        let headers = seen.lock().unwrap().clone();
        assert!(headers.get("x-api-key").is_none(), "no x-api-key may reach the upstream");
        assert!(headers.get("authorization").is_none(), "no bearer may reach the upstream");
    }

    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 2, "exactly one audit line per request");
    let logged = serde_json::to_string(&lines).unwrap();
    for secret in [
        "sk-ant-api03-FAKEFAKEFAKEFAKE",
        "sk-FAKEFAKEFAKEFAKE",
        &token,
        "SECRET_PROMPT_TEXT",
    ] {
        assert!(
            !logged.contains(secret),
            "the audit lines carry no secret ({secret}): {logged}"
        );
    }
    assert!(!logged.contains("sk-"), "no key shape in the audit lines: {logged}");
    assert!(!logged.contains("Bearer "), "no bearer shape in the audit lines: {logged}");

    let anthropic_line = &lines[0];
    assert_eq!(anthropic_line["failure"], Value::Null);
    assert_eq!(anthropic_line["fallback"], false);
    assert_eq!(anthropic_line["wire"], "anthropic");
    assert_eq!(anthropic_line["method"], "POST");
    assert_eq!(anthropic_line["path"], "/v1/messages");
    assert_eq!(anthropic_line["model"], "claude-sonnet-5");
    assert_eq!(anthropic_line["wire_model"], "claude-sonnet-5");
    assert_eq!(anthropic_line["status"], 200);
    assert_eq!(
        anthropic_line["input_tokens"], 10,
        "the usage tap's counts land in the record"
    );
    assert_eq!(anthropic_line["output_tokens"], 5);
    assert_eq!(anthropic_line["request_bytes"], request_body.len());
    assert_eq!(
        anthropic_line["response_bytes"],
        response_body.len(),
        "exactly what was forwarded"
    );
    let openai_line = &lines[1];
    assert_eq!(openai_line["wire"], "openai");
    assert_eq!(
        openai_line["wire_model"], "gpt-5.5",
        "the translated request's model, validator applied"
    );
    assert_eq!(openai_line["status"], 200);
    assert_eq!(openai_line["input_tokens"], 3);
    assert_eq!(openai_line["output_tokens"], 2);
    let _ = std::fs::remove_dir_all(root);
}

/// A `model_map` entry renames the model on the anthropic wire, and the audit record says so:
/// `model` is what the colony asked for, `wire_model` what went upstream (#295 meets #302).
#[tokio::test]
async fn a_model_mapped_anthropic_request_audits_the_mapped_wire_model() {
    let (base, _seen) = capturing_upstream(
        "/v1/messages",
        json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
            "model": "deepseek-v4-pro", "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }),
    )
    .await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-mapped-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/claude-sonnet-5"],
        json!([
            {"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none",
             "model_map": {"claude-sonnet-5": "deepseek-v4-pro"}},
        ]),
    )
    .await;
    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        placeholder_credentials(),
        Bytes::from_static(br#"{"model":"claude-sonnet-5","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["model"], "claude-sonnet-5", "the requested model");
    assert_eq!(lines[0]["wire_model"], "deepseek-v4-pro", "the model_map's wire name");
    let _ = std::fs::remove_dir_all(root);
}

/// An OpenAI-wire route a colony speaks itself — `POST /v1/responses` (issue #629) — is
/// forwarded byte-for-byte, and accounting reads the Responses usage spelling: cached input
/// comes out of the input side, reasoning rides along as thinking. The spend lands on the
/// colony like any routed request, and the colony's own credentials stop at the gateway.
#[tokio::test]
async fn a_responses_passthrough_forwards_verbatim_and_records_the_usage() {
    const ANSWER: &str = r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.5",
"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}],
"usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,
"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}}}"#;
    let (base, seen) = raw_upstream("/v1/responses", StatusCode::OK, "application/json", ANSWER).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["strix"],
        &["strix/gpt-5.5"],
        json!([{"id": "strix", "name": "Strix", "base_url": base, "auth": "none", "wire": "openai",
                    "pricing": {"input_per_mtok": 1.0, "output_per_mtok": 1.0}}]),
    )
    .await;
    let request = Bytes::from_static(
        br#"{"model":"gpt-5.5","input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}],"max_output_tokens":16}"#,
    );
    let response = post_to_path(
        &app,
        &token,
        "strix",
        "/v1/responses",
        placeholder_credentials(),
        request.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        forwarded,
        Bytes::from_static(ANSWER.as_bytes()),
        "forwarded exactly as upstream sent it"
    );

    let (upstream_headers, upstream_body) = seen.lock().unwrap().clone();
    assert_eq!(upstream_body, request, "the body forwards as the colony sent it");
    assert!(
        upstream_headers.get("x-api-key").is_none(),
        "no x-api-key may reach the upstream"
    );
    assert!(
        upstream_headers.get(axum::http::header::AUTHORIZATION).is_none(),
        "no bearer may reach the upstream"
    );

    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["wire"], "openai");
    assert_eq!(lines[0]["path"], "/v1/responses");
    assert_eq!(lines[0]["model"], "gpt-5.5");
    assert_eq!(lines[0]["wire_model"], "gpt-5.5");
    assert_eq!(lines[0]["status"], 200);
    assert_eq!(lines[0]["input_tokens"], 6, "10 input minus the 4 that were cached");
    assert_eq!(lines[0]["output_tokens"], 4);

    let mut recorded = None;
    for _ in 0..1000 {
        if let Some(cost) = app.session("c1").await.and_then(|s| s.routed_cost_usd) {
            recorded = Some(cost);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        recorded.is_some_and(|cost| cost > 0.0),
        "the passthrough usage was priced and recorded"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// `POST /v1/chat/completions` is the same passthrough in Chat Completions' spelling, with one
/// rewrite each way: the connection policy's `model_map` renames the model on the way out (#295),
/// and a streaming request is asked for usage the colony did not ask for, because without
/// `stream_options.include_usage` the final chunk carries none and accounting counts nothing.
#[tokio::test]
async fn a_chat_passthrough_maps_the_model_and_counts_the_usage() {
    const ANSWER: &str = r#"{"id":"cpl_1","object":"chat.completion","created":1,"model":"gpt-5.5-route",
"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}],
"usage":{"prompt_tokens":7,"completion_tokens":5,"total_tokens":12,
"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":1}}}"#;
    let (base, seen) = raw_upstream("/v1/chat/completions", StatusCode::OK, "application/json", ANSWER).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-chat-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["kimi"],
        &["kimi/gpt-5.5"],
        json!([{"id": "kimi", "name": "Kimi", "base_url": base, "auth": "none", "wire": "openai",
                    "model_map": {"gpt-5.5": "gpt-5.5-route"}}]),
    )
    .await;
    let request =
        Bytes::from_static(br#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hi"}],"max_completion_tokens":16}"#);
    let response = post_to_path(
        &app,
        &token,
        "kimi",
        "/v1/chat/completions",
        placeholder_credentials(),
        request,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        forwarded,
        Bytes::from_static(ANSWER.as_bytes()),
        "forwarded exactly as upstream sent it"
    );

    let (_, upstream_body): (HeaderMap, Bytes) = seen.lock().unwrap().clone();
    let sent: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(sent["model"], "gpt-5.5-route", "the model_map's wire name");
    assert_eq!(
        sent["messages"],
        json!([{"role": "user", "content": "hi"}]),
        "the rest forwards untouched"
    );

    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["path"], "/v1/chat/completions");
    assert_eq!(lines[0]["model"], "gpt-5.5", "what the colony asked for");
    assert_eq!(lines[0]["wire_model"], "gpt-5.5-route", "what went upstream");
    assert_eq!(lines[0]["input_tokens"], 4, "7 prompt tokens minus the 3 that were cached");
    assert_eq!(lines[0]["output_tokens"], 5);
    let _ = std::fs::remove_dir_all(root);
}

/// A passthrough stream is counted from the usage its final event carries and forwarded untouched:
/// Responses names it on `response.completed`, a chat completion on the last chunk. The chat
/// request went out with `stream_options.include_usage` added, or that last chunk would carry
/// none. (The gateway's own keep-alive pings stay comments, which no event parser reads.)
#[tokio::test]
async fn a_passthrough_stream_counts_the_usage_its_final_event_carries() {
    const RESPONSES_SSE: &str = "\
event: response.created
data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}

event: response.output_text.delta
data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}

event: response.completed
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":4,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens_details\":{\"reasoning_tokens\":2}}}}

";
    const CHAT_SSE: &str = "\
data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-5.5\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}

data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-5.5\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}

data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-5.5\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":5,\"total_tokens\":12}}

data: [DONE]

";
    let (responses_base, responses_seen) =
        raw_upstream("/v1/responses", StatusCode::OK, "text/event-stream", RESPONSES_SSE).await;
    let (chat_base, chat_seen) = raw_upstream("/v1/chat/completions", StatusCode::OK, "text/event-stream", CHAT_SSE).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-sse-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["strix", "kimi"],
        &["strix/gpt-5.5", "kimi/gpt-5.5"],
        json!([
            {"id": "strix", "name": "Strix", "base_url": responses_base, "auth": "none", "wire": "openai"},
            {"id": "kimi", "name": "Kimi", "base_url": chat_base, "auth": "none", "wire": "openai"},
        ]),
    )
    .await;

    let response = post_to_path(
        &app,
        &token,
        "strix",
        "/v1/responses",
        placeholder_credentials(),
        Bytes::from_static(br#"{"model":"gpt-5.5","input":"hi","stream":true}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        forwarded,
        Bytes::from_static(RESPONSES_SSE.as_bytes()),
        "every event byte kept"
    );

    let response = post_to_path(
        &app,
        &token,
        "kimi",
        "/v1/chat/completions",
        placeholder_credentials(),
        Bytes::from_static(br#"{"model":"gpt-5.5","messages":[{"role":"user","content":"hi"}],"stream":true}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(forwarded, Bytes::from_static(CHAT_SSE.as_bytes()), "every chunk byte kept");
    let (_, chat_body): (HeaderMap, Bytes) = chat_seen.lock().unwrap().clone();
    let sent: Value = serde_json::from_slice(&chat_body).unwrap();
    assert_eq!(
        sent["stream_options"],
        json!({"include_usage": true}),
        "usage requested on the colony's behalf"
    );
    let (_, responses_body): (HeaderMap, Bytes) = responses_seen.lock().unwrap().clone();
    let responses_request: Value = serde_json::from_slice(&responses_body).unwrap();
    assert!(
        responses_request.get("stream_options").is_none(),
        "the Responses API has no stream_options; the injection is chat-only"
    );

    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["path"], "/v1/responses");
    assert_eq!(lines[0]["input_tokens"], 6, "10 input minus the 4 that were cached");
    assert_eq!(lines[0]["output_tokens"], 4);
    assert_eq!(lines[1]["path"], "/v1/chat/completions");
    assert_eq!(lines[1]["input_tokens"], 7);
    assert_eq!(lines[1]["output_tokens"], 5);
    let _ = std::fs::remove_dir_all(root);
}

/// The colony token also arrives as `Authorization: Bearer` — what a runner speaking the
/// OpenAI wire sends by default (issue #629). The same constant-time compare accepts it, a wrong
/// one is refused like an unknown gateway header, the gateway's own header keeps precedence when
/// a request carries both, and the colony's authorization never reaches the upstream.
#[tokio::test]
async fn the_colony_token_is_accepted_as_a_bearer_and_never_forwarded() {
    const ANSWER: &str = r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.5",
"output":[],"usage":{"input_tokens":1,"output_tokens":1}}"#;
    let (base, seen) = raw_upstream("/v1/responses", StatusCode::OK, "application/json", ANSWER).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-bearer-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["strix"],
        &["strix/gpt-5.5"],
        json!([{"id": "strix", "name": "Strix", "base_url": base, "auth": "none", "wire": "openai"}]),
    )
    .await;
    let request = Bytes::from_static(br#"{"model":"gpt-5.5","input":"hi"}"#);

    /// POSTs to strix `/v1/responses` carrying only `Authorization: Bearer <token>` — no gateway
    /// header, the way an OpenAI-wire runner authenticates.
    async fn post_as_bearer(app: &Shared, token: &str, body: Bytes) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        proxy(
            State(app.clone()),
            Path(("strix".into(), "v1/responses".into())),
            Method::POST,
            "/providers/strix/v1/responses".parse().unwrap(),
            headers,
            body,
        )
        .await
    }

    let response = post_as_bearer(&app, &token, request.clone()).await;
    assert_eq!(response.status(), StatusCode::OK, "a bearer alone authenticates");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();

    let response = post_as_bearer(&app, "not-a-colony", request.clone()).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a wrong bearer is an unknown token"
    );
    assert_eq!(audit_lines(&app, "c1").len(), 1, "an unknown token leaves no line");

    let mut bogus = HeaderMap::new();
    bogus.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_static("Bearer not-a-colony"),
    );
    let response = post_to_path(&app, &token, "strix", "/v1/responses", bogus, request).await;
    assert_eq!(response.status(), StatusCode::OK, "the gateway header outranks the bearer");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();

    let (upstream_headers, _) = seen.lock().unwrap().clone();
    assert!(
        upstream_headers.get(axum::http::header::AUTHORIZATION).is_none(),
        "the colony's bearer stops at the gateway"
    );
    assert_eq!(audit_lines(&app, "c1").len(), 2, "one line per authenticated request");
    let _ = std::fs::remove_dir_all(root);
}

/// Everything else on an OpenAI-wire provider is still refused: the gateway serves the
/// translated `/v1/messages` and the two passthrough routes, any other path 404s and a GET on a
/// served one is the 405 every wire answers (#681), and neither reaches upstream.
#[tokio::test]
async fn an_openai_wire_route_the_provider_does_not_serve_is_a_404() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-404-{}", uuid::Uuid::new_v4()));
    // Port 9 (discard) is never reached: nothing may be sent upstream.
    let (app, token) = colony_with_providers(
        &root,
        &["strix"],
        &["strix/gpt-5.5"],
        json!([{"id": "strix", "name": "Strix", "base_url": "http://127.0.0.1:9", "auth": "none", "wire": "openai"}]),
    )
    .await;
    let request = Bytes::from_static(br#"{"model":"gpt-5.5","input":"hi"}"#);

    let response = post_to_path(
        &app,
        &token,
        "strix",
        "/v1/embeddings",
        placeholder_credentials(),
        request.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "not_found_error");

    let response = gateway_request(
        &app,
        &token,
        "strix",
        Method::GET,
        "v1/responses",
        HeaderMap::new(),
        Bytes::new(),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "a GET is not the passthrough"
    );
    assert_eq!(audit_lines(&app, "c1").len(), 2, "each refusal leaves its line");
    let _ = std::fs::remove_dir_all(root);
}

/// The passthrough routes are the openai wire's own: an anthropic-wire provider serves Messages
/// only, so a `/v1/responses` or `/v1/chat/completions` POST to one is refused with the same 404
/// as any unserved path (#681) — the runners refuse such a route before they start, and the
/// gateway holds the line should one arrive anyway.
#[tokio::test]
async fn an_anthropic_wire_provider_does_not_serve_the_openai_passthrough() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-anthropic-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": base, "auth": "none"}]),
    )
    .await;
    for rest in ["/v1/responses", "/v1/chat/completions"] {
        let response = post_to_path(
            &app,
            &token,
            "deepseek",
            rest,
            HeaderMap::new(),
            Bytes::from_static(br#"{"model":"deepseek-chat","input":"hi"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{rest}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the provider");
    let _ = std::fs::remove_dir_all(root);
}

/// Model scoping (#681/#727) applies to the passthrough exactly as to `/v1/messages`: a
/// Responses or Chat Completions body naming a model the colony's settings never routed to is
/// refused before any upstream call, and the routed pair (`strix/gpt-5.5`, which a codex colony
/// set to `strix/gpt-5.5` records at boot) is admitted.
#[tokio::test]
async fn a_passthrough_model_outside_the_colonys_routing_is_refused() {
    let hits = Arc::new(AtomicU64::new(0));
    let base = counting_upstream(hits.clone()).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-model-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["strix"],
        &["strix/gpt-5.5"],
        json!([{"id": "strix", "name": "Strix", "base_url": base, "auth": "none", "wire": "openai"}]),
    )
    .await;
    for (rest, body) in [
        ("/v1/responses", &br#"{"model":"gpt-other","input":"hi"}"#[..]),
        (
            "/v1/chat/completions",
            &br#"{"model":"gpt-other","messages":[{"role":"user","content":"hi"}]}"#[..],
        ),
    ] {
        let response = post_to_path(&app, &token, "strix", rest, HeaderMap::new(), Bytes::from_static(body)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{rest}");
        let refused: Value =
            serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
        let message = refused["error"]["message"].as_str().unwrap();
        assert!(message.contains("gpt-other"), "names the model: {message}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the provider");
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line["failure"] == "not_routed"), "{lines:?}");

    let response = post_to_path(
        &app,
        &token,
        "strix",
        "/v1/responses",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"gpt-5.5","input":"hi"}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "the routed pair is admitted");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(root);
}

/// The openai-wire presets store their `/v1` in the base_url (xai-grok's is `https://api.x.ai/v1`,
/// issue #629). The join must not grow a second one: the translated `/v1/messages` and both
/// passthrough routes land just below the base's `/v1`, where the upstream actually listens —
/// against a repeated `/v1/v1` both mocks would never have been reached.
#[tokio::test]
async fn a_base_url_that_already_ends_in_v1_does_not_grow_a_second_one() {
    const ANSWER: &str = r#"{"id":"cpl_1","object":"chat.completion","created":1,"model":"m",
"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}],
"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
    let (responses_base, responses_seen) = raw_upstream("/v1/responses", StatusCode::OK, "application/json", ANSWER).await;
    let (chat_base, chat_seen) = raw_upstream("/v1/chat/completions", StatusCode::OK, "application/json", ANSWER).await;
    let root = std::env::temp_dir().join(format!("colonizer-gateway-v1-base-{}", uuid::Uuid::new_v4()));
    let (app, token) = colony_with_providers(
        &root,
        &["strix", "kimi"],
        &["strix/m", "kimi/m"],
        json!([
            {"id": "strix", "name": "Strix", "base_url": format!("{responses_base}/v1"), "auth": "none", "wire": "openai"},
            {"id": "kimi", "name": "Kimi", "base_url": format!("{chat_base}/v1"), "auth": "none", "wire": "openai"}
        ]),
    )
    .await;

    let request = Bytes::from_static(br#"{"model":"m","input":"hi"}"#);
    let response = post_to_path(&app, &token, "strix", "/v1/responses", placeholder_credentials(), request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the passthrough landed below the base's /v1"
    );
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let (_, upstream_body): (HeaderMap, Bytes) = responses_seen.lock().unwrap().clone();
    let sent: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(sent["model"], "m");

    let request = Bytes::from_static(br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":16}"#);
    let response = post_to_gateway(&app, &token, "kimi", placeholder_credentials(), request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the translated join skipped the repeated /v1"
    );
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let (_, upstream_body): (HeaderMap, Bytes) = chat_seen.lock().unwrap().clone();
    let sent: Value = serde_json::from_slice(&upstream_body).unwrap();
    assert_eq!(sent["model"], "m", "the model rides along");
    assert_eq!(sent["max_completion_tokens"], 16, "anthropic's max_tokens on the openai wire");
    let _ = std::fs::remove_dir_all(root);
}

/// The budget and its in-flight reservation gate a passthrough like any other request: a
/// request whose estimate would tip the colony past its cap is refused before dispatch, so a
/// burst of `/v1/responses` POSTs cannot spend past the cap either.
#[tokio::test]
async fn a_passthrough_request_still_honors_the_budget() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-passthrough-budget-{}", uuid::Uuid::new_v4()));
    // Port 9 (discard) is never reached: the request is refused before dispatch.
    let (app, token) = colony_with_providers(
        &root,
        &["strix"],
        &["strix/gpt-5.5"],
        json!([{
            "id": "strix", "name": "Strix", "base_url": "http://127.0.0.1:9", "auth": "none", "wire": "openai",
            "pricing": {"input_per_mtok": 1.0, "output_per_mtok": 1.0}
        }]),
    )
    .await;
    let provider = Provider {
        base_url: "http://127.0.0.1:9".into(),
        wire: crate::providers::Wire::Openai,
        pricing: Some(crate::providers::Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 1.0,
            ..Default::default()
        }),
        ..provider("strix", None)
    };
    let body = Bytes::from_static(br#"{"model":"gpt-5.5","input":"hi","max_output_tokens":3000}"#);
    let budget = estimate_request_cost_usd(&provider, &body).0 * 0.5;
    app.modules
        .write()
        .await
        .sandbox
        .settings
        .insert("budget_usd".into(), json!(budget));

    let response = post_to_path(&app, &token, "strix", "/v1/responses", HeaderMap::new(), body).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let refused: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("budget"), "names the budget: {message}");
    assert_eq!(
        app.gateway.usage_counters("strix").snapshot().requests,
        0,
        "refused locally, so nothing counts as provider usage"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "budget");
    let _ = std::fs::remove_dir_all(root);
}

/// A provider that cannot be reached answers 502 with the fallback header, and the audit line
/// names the branch. An unknown colony token leaves no line at all: it cannot be attributed.
#[tokio::test]
async fn an_unreachable_provider_logs_unreachable_and_an_unknown_token_logs_nothing() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-unreachable-{}", uuid::Uuid::new_v4()));
    // Port 9 (discard) refuses: the dispatch fails without touching the network.
    let (app, token) = colony_with_providers(
        &root,
        &["deepseek"],
        &["deepseek/deepseek-chat"],
        json!([{"id": "deepseek", "name": "DeepSeek", "base_url": "http://127.0.0.1:9", "auth": "none"}]),
    )
    .await;

    let response = post_to_gateway(
        &app,
        &token,
        "deepseek",
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers().get(FALLBACK_HEADER).and_then(|v| v.to_str().ok()),
        Some("unreachable"),
        "the router gets its fallback licence"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "unreachable");
    assert_eq!(lines[0]["fallback"], true);
    assert_eq!(lines[0]["status"], 502);

    // An unknown token is refused before anything is attributed: no second line.
    let response = post_to_gateway(&app, "not-a-colony", "deepseek", HeaderMap::new(), Bytes::from_static(b"{}")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(audit_lines(&app, "c1").len(), 1, "the unattributable request leaves no line");
    let _ = std::fs::remove_dir_all(root);
}

/// A colony whose task touches restricted paths is refused a provider nobody has marked
/// trusted (issue #472): the allowlist above admits the provider, but the sensitivity gate
/// still refuses it before any upstream call.
#[tokio::test]
async fn a_restricted_task_is_refused_a_provider_not_marked_trusted() {
    let root = std::env::temp_dir().join(format!("colonizer-gateway-sensitivity-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    // The allowlist lets deepseek by, so the sensitivity gate is what refuses.
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.sensitivity = Some("restricted".into());
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    // Port 9 (discard) is never reached: nothing may be sent upstream. No "trusted" key: the
    // default is false, exactly as a provider nobody has vetted reads.
    std::fs::write(
        root.join("config/providers.json"),
        r#"[{"id":"deepseek","name":"DeepSeek","base_url":"http://127.0.0.1:9","auth":"none"}]"#,
    )
    .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(&token).unwrap());
    let response = proxy(
        State(app.clone()),
        Path(("deepseek".into(), "v1/messages".into())),
        Method::POST,
        "/providers/deepseek/v1/messages".parse().unwrap(),
        headers,
        Bytes::from_static(b"{}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["type"], "sensitivity_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("\"deepseek\""), "names the provider: {message}");
    assert!(message.contains("trusted"), "says what would fix it: {message}");
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        0,
        "refused locally, so nothing counts as provider usage"
    );
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "restricted");
    assert_eq!(lines[0]["status"], 403);
    let _ = std::fs::remove_dir_all(root);
}

/// The same restricted colony passes straight through to a provider an operator has marked
/// `trusted` in providers.json: vetting it is the fix the refusal names (issue #472).
#[tokio::test]
async fn a_restricted_task_dispatches_to_a_provider_marked_trusted() {
    let router = Router::new().route("/v1/messages", axum::routing::post(|| async { axum::Json(json!({})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-sensitivity-ok-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.allowed_models = Some(vec!["deepseek/deepseek-chat".into()]);
    colony.sensitivity = Some("restricted".into());
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(&[json!({
            "id": "deepseek",
            "name": "DeepSeek",
            "base_url": format!("http://{addr}"),
            "auth": "none",
            "trusted": true,
        })])
        .unwrap(),
    )
    .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(COLONY_HEADER, HeaderValue::from_str(&token).unwrap());
    let response = proxy(
        State(app.clone()),
        Path(("deepseek".into(), "v1/messages".into())),
        Method::POST,
        "/providers/deepseek/v1/messages".parse().unwrap(),
        headers,
        Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[]}"#),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "a trusted provider is not refused");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        1,
        "marked trusted, so the request dispatched"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// An org vendor pin is wired through end to end (issue #626): the gate reads the overrides of
/// the colony's org, so a trusted provider is refused while its recorded vendor is off the org's
/// list, and recording that vendor on the provider lets the same request through.
#[tokio::test]
async fn an_org_vendor_pin_gates_on_the_providers_recorded_vendor() {
    let router = Router::new().route("/v1/messages", axum::routing::post(|| async { axum::Json(json!({})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-vendor-pin-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.allowed_models = Some(vec!["deepseek/deepseek-chat".into()]);
    colony.sensitivity = Some("restricted".into());
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    // The org pins restricted work to one vendor; the trusted provider below records none, then
    // the pinned one. `app.providers()` reads the file per request, so the rewrite lands.
    std::fs::write(
        root.join("config/orgs.json"),
        r#"{"acme":{"sensitivity":{"restricted_vendors":["anthropic"]}}}"#,
    )
    .unwrap();
    let write_provider = |vendor: Option<&str>| {
        std::fs::write(
            root.join("config/providers.json"),
            serde_json::to_string(&[json!({
                "id": "deepseek",
                "name": "DeepSeek",
                "base_url": format!("http://{addr}"),
                "auth": "none",
                "trusted": true,
                "vendor": vendor,
            })])
            .unwrap(),
        )
        .unwrap();
    };
    async fn call(app: crate::Shared, token: &str) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(COLONY_HEADER, HeaderValue::from_str(token).unwrap());
        proxy(
            State(app),
            Path(("deepseek".into(), "v1/messages".into())),
            Method::POST,
            "/providers/deepseek/v1/messages".parse().unwrap(),
            headers,
            Bytes::from_static(br#"{"model":"deepseek-chat","max_tokens":8,"messages":[]}"#),
        )
        .await
    }

    write_provider(None);
    let response = call(app.clone(), &token).await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "trusted alone does not pass a vendor pin"
    );
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "sensitivity_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("vendor"), "names the pin that refused it: {message}");
    assert_eq!(app.gateway.usage_counters("deepseek").snapshot().requests, 0);
    let lines = audit_lines(&app, "c1");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["failure"], "restricted");

    write_provider(Some("anthropic"));
    let response = call(app.clone(), &token).await;
    assert_eq!(response.status(), StatusCode::OK, "the recorded vendor matches the org's pin");
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        1,
        "past the gate, so the request dispatched"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The estimate reads the request's own `max_tokens` (`max_completion_tokens` and the Responses
/// passthrough's `max_output_tokens` too) and falls back to the documented constant when the body
/// names none of them, is not JSON, or the provider carries no pricing at all — which estimates,
/// like it records, at nothing (issue #409).
#[test]
fn cost_estimates_read_the_requests_own_cap_and_fall_back_without_one() {
    let priced = Provider {
        pricing: Some(crate::providers::Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            ..Default::default()
        }),
        ..provider("strix", None)
    };
    let expected =
        |body: &Bytes, output: u64| (body.len() as u64 / 4) as f64 * 1.0 / 1_000_000.0 + output as f64 * 2.0 / 1_000_000.0;

    let anthropic = Bytes::from(r#"{"model":"m","max_tokens":3000,"messages":[]}"#);
    assert!(
        (estimate_request_cost_usd(&priced, &anthropic).0 - expected(&anthropic, 3000)).abs() < 1e-12,
        "the request's own max_tokens is the estimated output"
    );
    let openai = Bytes::from(r#"{"model":"gpt-5.5","max_completion_tokens":500,"messages":[]}"#);
    assert!(
        (estimate_request_cost_usd(&priced, &openai).0 - expected(&openai, 500)).abs() < 1e-12,
        "the OpenAI spelling of the same cap is read too"
    );
    let responses = Bytes::from(r#"{"model":"gpt-5.5","max_output_tokens":800,"input":"hi"}"#);
    assert!(
        (estimate_request_cost_usd(&priced, &responses).0 - expected(&responses, 800)).abs() < 1e-12,
        "the Responses passthrough's spelling is read too"
    );

    // No cap named, or not JSON at all: the documented fallback bounds the output side.
    let bare = Bytes::from(r#"{"model":"m","messages":[]}"#);
    assert!((estimate_request_cost_usd(&priced, &bare).0 - expected(&bare, ESTIMATED_MAX_TOKENS)).abs() < 1e-12);
    let junk = Bytes::from("not json");
    assert!((estimate_request_cost_usd(&priced, &junk).0 - expected(&junk, ESTIMATED_MAX_TOKENS)).abs() < 1e-12);

    // No pricing configured: nothing to reserve, exactly as recording it costs nothing.
    assert_eq!(estimate_request_cost_usd(&provider("strix", None), &anthropic).0, 0.0);
}

/// A reservation holds until its guard drops: while held it counts against the cap, and once
/// dropped the same reservation fits again — the mechanism `proxy` leans on (issue #409).
#[test]
fn a_reservation_holds_until_its_guard_drops() {
    let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-reserve-{}", uuid::Uuid::new_v4())));
    let reserved = gateway.colony_reserved("c1");
    assert_eq!(reserved.load(Ordering::SeqCst), 0, "nothing in flight, nothing reserved");

    let held = Reserved::new(&reserved, micro_usd(0.60));
    assert_eq!(
        reserved.load(Ordering::SeqCst),
        micro_usd(0.60),
        "the guard's creation reserves"
    );
    // The decision `proxy` makes: recorded spend plus outstanding reservations plus this estimate
    // against the cap.
    let (spent, budget) = (0.30, 1.00);
    assert!(
        spent + reserved.load(Ordering::SeqCst) as f64 / 1_000_000.0 + 0.20 > budget,
        "a request that fits alone is tipped over by the held reservation"
    );

    drop(held);
    assert_eq!(
        reserved.load(Ordering::SeqCst),
        0,
        "the guard's drop gives the reservation back"
    );
    assert!(spent + 0.20 <= budget, "the same request fits again once the guard is gone");
    assert_eq!(micro_usd(-1.0), 0, "a negative estimate reserves nothing");
}

/// Two requests that each fit the budget cannot burst past it together (issue #409): the first
/// reserves its estimate until its response has streamed, so a second arriving while that
/// reservation is held is refused 403 instead of dispatched — and the colony is not stopped, which
/// stays `enforce_budget`'s consequence for overspend that has actually been recorded. The first
/// response going away ends its reservation, and the same request fits again.
#[tokio::test]
async fn parallel_requests_cannot_burst_past_the_budget() {
    let router = Router::new().route("/v1/messages", axum::routing::post(|| async { axum::Json(json!({})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-reserve-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.allowed_models = Some(vec!["deepseek/m".into()]);
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    let provider = Provider {
        base_url: format!("http://{addr}"),
        pricing: Some(crate::providers::Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 1.0,
            ..Default::default()
        }),
        ..provider("deepseek", None)
    };
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(std::slice::from_ref(&provider)).unwrap(),
    )
    .unwrap();
    // A budget one request fits and two never do: 1.5x the estimate leaves room for the first
    // alone, and the first plus the second's estimate crosses it.
    let body = Bytes::from(r#"{"model":"m","max_tokens":3000,"messages":[]}"#);
    let budget = estimate_request_cost_usd(&provider, &body).0 * 1.5;
    app.modules
        .write()
        .await
        .sandbox
        .settings
        .insert("budget_usd".into(), json!(budget));

    async fn call(app: &Shared, token: &str, body: Bytes) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(COLONY_HEADER, HeaderValue::from_str(token).unwrap());
        proxy(
            State(app.clone()),
            Path(("deepseek".into(), "v1/messages".into())),
            Method::POST,
            "/providers/deepseek/v1/messages".parse().unwrap(),
            headers,
            body,
        )
        .await
    }

    // The first dispatches; its reservation lives in the response body it returns.
    let first = call(&app, &token, body.clone()).await;
    assert_eq!(first.status(), StatusCode::OK);

    // The same request again, while the first's reservation is still held: refused, not dispatched.
    let second = call(&app, &token, body.clone()).await;
    assert_eq!(second.status(), StatusCode::FORBIDDEN);
    let refused: Value = serde_json::from_slice(&axum::body::to_bytes(second.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(refused["error"]["type"], "permission_error");
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(message.contains("budget"), "names the budget: {message}");
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        1,
        "the refused request never reached the provider or counted as usage"
    );
    assert!(
        app.session("c1").await.is_some_and(|s| s.status.is_live()),
        "a refused request stops nothing; that is enforce_budget's consequence"
    );

    // The first response going away ends its reservation: the same request fits again.
    drop(first);
    let third = call(&app, &token, body.clone()).await;
    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(
        app.gateway.usage_counters("deepseek").snapshot().requests,
        2,
        "only dispatched requests count"
    );
    let _ = axum::body::to_bytes(third.into_body(), usize::MAX).await;
    let _ = std::fs::remove_dir_all(root);
}

/// The reservation outlives the body (issue #409): the cost is recorded by a task spawned when
/// the body ends, and until that lands the reservation still counts against the budget — a
/// response that has fully streamed is not yet a response whose cost is visible. Holding the
/// sessions write lock parks the recorder's task inside `record_routed_usage`, so the ordering
/// is observed deterministically instead of raced; releasing the lock lets the cost land, and
/// only then the reservation go.
#[test]
fn a_reservation_is_claimed_only_while_it_fits_and_parallel_claims_cannot_share_a_total() {
    let reserved = Arc::new(AtomicU64::new(0));
    // Budget room for one 600-unit request, not two.
    let fits = |outstanding: u64| outstanding + 600 <= 1_000;
    let first = Reserved::try_new(&reserved, 600, fits).expect("the first fits");
    assert_eq!(reserved.load(Ordering::SeqCst), 600);
    assert_eq!(
        Reserved::try_new(&reserved, 600, fits).err(),
        Some(600),
        "the second sees the first"
    );
    drop(first);
    assert_eq!(reserved.load(Ordering::SeqCst), 0, "a dropped claim is handed back");

    // Many threads racing for room for exactly three claims: exactly three get one.
    let room = |outstanding: u64| outstanding + 100 <= 300;
    let won = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| Reserved::try_new(&reserved, 100, room).ok()))
            .collect();
        handles.into_iter().filter_map(|h| h.join().unwrap()).collect::<Vec<_>>()
    });
    assert_eq!(won.len(), 3);
    assert_eq!(reserved.load(Ordering::SeqCst), 300);
}

#[tokio::test]
async fn the_reservation_outlives_the_streamed_body_until_its_cost_is_recorded() {
    let router = Router::new().route(
        "/v1/messages",
        axum::routing::post(|| async { axum::Json(json!({"usage": {"input_tokens": 100, "output_tokens": 50}})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-outlive-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let mut colony = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
    colony.id = "c1".into();
    colony.allowed_providers = Some(vec!["deepseek".into()]);
    colony.allowed_models = Some(vec!["deepseek/m".into()]);
    app.sessions.write().await.push(colony);
    let token = "t".repeat(40);
    std::fs::create_dir_all(app.session_dir("c1")).unwrap();
    std::fs::write(app.gateway_token_file("c1"), &token).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    let provider = Provider {
        base_url: format!("http://{addr}"),
        pricing: Some(crate::providers::Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 1.0,
            ..Default::default()
        }),
        ..provider("deepseek", None)
    };
    std::fs::write(
        root.join("config/providers.json"),
        serde_json::to_string(std::slice::from_ref(&provider)).unwrap(),
    )
    .unwrap();
    // No budget is needed: the reservation exists whenever the provider is priced.
    let body = Bytes::from(r#"{"model":"m","max_tokens":3000,"messages":[]}"#);

    async fn call(app: &Shared, token: &str, body: Bytes) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(COLONY_HEADER, HeaderValue::from_str(token).unwrap());
        proxy(
            State(app.clone()),
            Path(("deepseek".into(), "v1/messages".into())),
            Method::POST,
            "/providers/deepseek/v1/messages".parse().unwrap(),
            headers,
            body,
        )
        .await
    }

    let response = call(&app, &token, body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);

    // Park the recorder's task before the body ends: record_routed_usage cannot take the
    // sessions write lock this test holds, so the cost cannot land while it is held.
    let sessions = app.sessions.write().await;
    let streamed = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        streamed,
        Bytes::from(r#"{"usage":{"input_tokens":100,"output_tokens":50}}"#),
        "the body forwarded verbatim"
    );
    assert_eq!(
        app.gateway.colony_reserved("c1").load(Ordering::SeqCst),
        micro_usd(estimate_request_cost_usd(&provider, &body).0),
        "the body has fully streamed, yet the reservation is held until the cost lands"
    );
    drop(sessions);

    // The cost lands, and only then does the reservation go.
    let mut recorded = None;
    for _ in 0..1000 {
        if let Some(cost) = app.session("c1").await.and_then(|s| s.routed_cost_usd) {
            recorded = Some(cost);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(recorded.is_some_and(|cost| cost > 0.0), "the streamed usage was recorded");
    assert_eq!(
        app.gateway.colony_reserved("c1").load(Ordering::SeqCst),
        0,
        "the reservation is released once the recorded cost replaces the estimate"
    );
    assert!(
        app.session("c1").await.is_some_and(|s| s.status.is_live()),
        "recording a cost well under any budget stops nothing"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Probes a provider of the given wire whose upstream answers `/v1/models` with `status`.
async fn probe_answering(wire: crate::providers::Wire, status: StatusCode) -> Value {
    let router = Router::new().route("/v1/models", axum::routing::get(move || async move { status }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-probe-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let provider = Provider {
        base_url: format!("http://{addr}"),
        wire,
        ..provider("x", None)
    };
    let health = probe(&app, &provider).await;
    let _ = std::fs::remove_dir_all(root);
    health
}

use std::sync::atomic::AtomicUsize;

/// A fake provider answering `/v1/models` after `delay`, counting every hit. Shared by the
/// probe-cache tests below.
async fn counting_server(delay: Duration) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let router = Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let counter = counter.clone();
            async move {
                tokio::time::sleep(delay).await;
                counter.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (addr, hits)
}

fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!("colonizer-probe-cache-{}", uuid::Uuid::new_v4()))
}

/// The second lookup within the TTL serves the cache: one network hit for two lookups, and the
/// cached one does no I/O, so it comes back faster than the ~300 ms upstream delay the first
/// paid. Both durations print for the report (`--nocapture` reproduces the measurement).
#[tokio::test]
async fn second_probe_within_ttl_does_not_hit_the_network() {
    let (addr, hits) = counting_server(Duration::from_millis(300)).await;
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    let provider = Provider {
        base_url: format!("http://{addr}"),
        ..provider("x", None)
    };
    let start = Instant::now();
    let first = probe_cached(&app, &provider).await;
    let uncached = start.elapsed();
    let start = Instant::now();
    let second = probe_cached(&app, &provider).await;
    let cached = start.elapsed();
    eprintln!("provider probe: uncached {uncached:?}, cached {cached:?}");
    assert_eq!(first["reachable"], true);
    assert_eq!(first, second);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the second lookup must come from the cache");
    assert!(
        uncached >= Duration::from_millis(200),
        "the fake delay must be real: {uncached:?}"
    );
    assert!(
        cached < Duration::from_millis(300),
        "a cached lookup does no I/O, so it must beat the server delay: {cached:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A stale entry re-probes instead of serving: expiry is by timestamp, not by waiting out the TTL.
#[tokio::test]
async fn stale_probe_entry_reprobes() {
    let (addr, hits) = counting_server(Duration::ZERO).await;
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    let provider = Provider {
        base_url: format!("http://{addr}"),
        ..provider("x", None)
    };
    // `Instant::now() - TTL` underflows when the host's monotonic clock is younger than the
    // TTL (fresh CI containers); there is then no way to be stale, so say so and skip.
    let Some(stale_at) = Instant::now().checked_sub(PROVIDER_PROBE_TTL + Duration::from_secs(1)) else {
        eprintln!(
            "skipping stale_probe_entry_reprobes: host uptime is under {:?}, so a stale timestamp cannot be constructed",
            PROVIDER_PROBE_TTL + Duration::from_secs(1)
        );
        let _ = std::fs::remove_dir_all(root);
        return;
    };
    app.provider_probe_cache
        .lock()
        .await
        .insert(probe_cache_key(&provider), (stale_at, json!({"reachable": "stale"})));
    let health = probe_cached(&app, &provider).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "a stale entry must re-probe");
    assert_eq!(health["reachable"], true, "the fresh answer replaces the stale marker");
    let _ = std::fs::remove_dir_all(root);
}

/// Same id, new endpoint: the key carries the base URL, so a repointed provider re-probes.
#[tokio::test]
async fn repointed_provider_misses_the_cache() {
    let (addr_a, hits_a) = counting_server(Duration::ZERO).await;
    let (addr_b, hits_b) = counting_server(Duration::ZERO).await;
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    let old = Provider {
        base_url: format!("http://{addr_a}"),
        ..provider("up", None)
    };
    let new = Provider {
        base_url: format!("http://{addr_b}"),
        ..old.clone()
    };
    probe_cached(&app, &old).await;
    probe_cached(&app, &new).await;
    assert_eq!(hits_a.load(Ordering::SeqCst), 1);
    assert_eq!(
        hits_b.load(Ordering::SeqCst),
        1,
        "the new endpoint must be probed, not served the old answer"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The on-demand health check always probes, and its answer warms the next boot lookup.
#[tokio::test]
async fn health_handler_probes_fresh_and_warms_the_cache() {
    let (addr, hits) = counting_server(Duration::ZERO).await;
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(
        root.join("config/providers.json"),
        format!(r#"[{{"id":"up","name":"Up","base_url":"http://{addr}","auth":"none"}}]"#),
    )
    .unwrap();
    let provider = Provider {
        base_url: format!("http://{addr}"),
        ..provider("up", None)
    };
    // A warm entry must not satisfy the handler: it probes regardless.
    app.provider_probe_cache
        .lock()
        .await
        .insert(probe_cache_key(&provider), (Instant::now(), json!({"reachable": "stale"})));
    let fresh = provider_health(State(app.clone()), Path("up".into())).await.unwrap().0;
    assert_eq!(fresh["reachable"], true);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the handler always probes");
    assert_eq!(probe_cached(&app, &provider).await, fresh);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the handler's answer warms the cache");
    let _ = std::fs::remove_dir_all(root);
}

/// Forgetting an id drops its entries at every endpoint and leaves other ids alone: rotating a
/// key or changing auth/base_url never serves the old answer again.
#[tokio::test]
async fn forget_probe_drops_every_entry_for_the_id() {
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    let here = Provider {
        base_url: "http://127.0.0.1:9".into(),
        ..provider("x", None)
    };
    let moved = Provider {
        base_url: "http://127.0.0.1:10".into(),
        ..here.clone()
    };
    let other = Provider {
        base_url: "http://127.0.0.1:9".into(),
        ..provider("y", None)
    };
    {
        let mut cache = app.provider_probe_cache.lock().await;
        let now = Instant::now();
        cache.insert(probe_cache_key(&here), (now, json!({"reachable": true})));
        cache.insert(probe_cache_key(&moved), (now, json!({"reachable": true})));
        cache.insert(probe_cache_key(&other), (now, json!({"reachable": true})));
    }
    forget_probe(&app, "x").await;
    let cache = app.provider_probe_cache.lock().await;
    assert!(cache.get(&probe_cache_key(&here)).is_none());
    assert!(cache.get(&probe_cache_key(&moved)).is_none());
    assert!(cache.get(&probe_cache_key(&other)).is_some(), "other ids must survive");
    drop(cache);
    let _ = std::fs::remove_dir_all(root);
}

/// An Anthropic-compatible endpoint that does not serve `/v1/models` still routes, so its 404
/// reads as reachable with no published list rather than as a failed probe.
#[tokio::test]
async fn an_anthropic_provider_without_a_model_list_probes_as_reachable() {
    let health = probe_answering(crate::providers::Wire::Anthropic, StatusCode::NOT_FOUND).await;
    assert_eq!(health["reachable"], true);
    assert_eq!(health["status"], 404, "the real status is kept");
    assert_eq!(health["error"], Value::Null);
    assert_eq!(health["models"], json!([]));
    assert_eq!(health["note"], "no model list");
    assert!(health.get("quota").is_none(), "no probe configured, no quota field");
}

/// Only the anthropic-wire 404 is softened: a refused key, or a 404 from an OpenAI-wire
/// endpoint (which must serve `/v1/models`), come back as they are.
#[tokio::test]
async fn other_probe_failures_carry_no_note() {
    let refused = probe_answering(crate::providers::Wire::Anthropic, StatusCode::UNAUTHORIZED).await;
    assert_eq!(refused["status"], 401);
    assert_eq!(refused.get("note"), Some(&Value::Null), "the key is always present");
    let openai = probe_answering(crate::providers::Wire::Openai, StatusCode::NOT_FOUND).await;
    assert_eq!(openai["status"], 404);
    assert_eq!(openai.get("note"), Some(&Value::Null));
}

/// The number a quota pointer reads: a JSON number or a numeric string — some plans quote the
/// count — and nothing else.
#[test]
fn quota_remaining_reads_numbers_and_numeric_strings() {
    let body = json!({"data": {"remaining": 12_345_678}, "quoted": "42000", "note": {"x": 1}});
    assert_eq!(quota_remaining(&body, "/data/remaining"), Some(json!(12_345_678)));
    assert_eq!(quota_remaining(&body, "/quoted"), Some(json!(42_000)), "a numeric string");
    assert_eq!(quota_remaining(&json!({"left": 1.5}), "/left"), Some(json!(1.5)));
    assert_eq!(quota_remaining(&body, "/missing"), None, "a pointer that misses");
    assert_eq!(quota_remaining(&body, "/note"), None, "an object is not a number");
    assert_eq!(
        quota_remaining(&body, "/data"),
        None,
        "a nested object is not a number either"
    );
}

/// A provider with a quota probe gets the plan balance in its health answer, on the same probe
/// that says reachable. The quota half never changes the verdict, whatever goes wrong with it.
#[tokio::test]
async fn a_quota_probe_rides_along_on_the_health_answer_without_changing_it() {
    let router = Router::new()
        .route("/v1/models", axum::routing::get(|| async { StatusCode::OK }))
        .route(
            "/plan",
            axum::routing::get(|| async { axum::Json(json!({"data": {"remaining": 12_345_678, "total": "20000000"}})) }),
        )
        .route("/broken", axum::routing::get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base_url = format!("http://{addr}");
    let root = cache_root();
    let app = crate::tests::test_app(&root);
    let quota_at = |path: &str, pointer: &str| {
        Some(QuotaProbe {
            url: format!("{base_url}{path}"),
            pointer: pointer.into(),
            limit_pointer: None,
        })
    };
    // A prepaid plan usually renames Claude's models to its own (#295's model_map); the probe
    // reads the plan balance all the same, since neither half of it sends a model.
    let healthy = probe(
        &app,
        &Provider {
            base_url: base_url.clone(),
            quota: quota_at("/plan", "/data/remaining"),
            model_map: BTreeMap::from([("claude-sonnet-5".to_string(), "deepseek-v4-pro".to_string())]),
            ..provider("x", None)
        },
    )
    .await;
    assert_eq!(healthy["reachable"], true);
    assert_eq!(healthy["quota"], json!({"remaining": 12_345_678, "error": null}));

    // A limit pointer reads the plan's total from the same answer, a quoted count included.
    let with_limit = probe(
        &app,
        &Provider {
            base_url: base_url.clone(),
            quota: Some(QuotaProbe {
                url: format!("{base_url}/plan"),
                pointer: "/data/remaining".into(),
                limit_pointer: Some("/data/total".into()),
            }),
            ..provider("x", None)
        },
    )
    .await;
    assert_eq!(
        with_limit["quota"],
        json!({"remaining": 12_345_678, "limit": 20_000_000, "error": null})
    );

    let refused = probe(
        &app,
        &Provider {
            quota: quota_at("/broken", "/nope"),
            base_url,
            ..provider("x", None)
        },
    )
    .await;
    assert_eq!(refused["reachable"], true, "a failed quota read is not a failed probe");
    assert_eq!(
        refused["quota"],
        json!({"remaining": null, "error": "quota endpoint answered HTTP 404"})
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A gateway whose usage file lives in a fresh temp directory.
fn usage_gateway(dir: &std::path::Path) -> Gateway {
    Gateway::new(dir).unwrap()
}

fn provider(id: &str, fallback_model: Option<&str>) -> Provider {
    Provider {
        id: id.into(),
        name: id.into(),
        base_url: "http://127.0.0.1:9".into(),
        auth: "none".into(),
        wire: crate::providers::Wire::Anthropic,
        models: vec![],
        preset: "custom".into(),
        timeout_secs: None,
        max_concurrent: None,
        queue_timeout_secs: None,
        context_tokens: None,
        fallback_model: fallback_model.map(str::to_string),
        pricing: None,
        model_map: BTreeMap::new(),
        disabled_tools: Vec::new(),
        quota: None,
        normalize_cache_ttl: false,
        trusted: false,
        vetted: false,
        vendor: None,
    }
}

#[test]
fn usage_counts_every_outcome_a_request_can_have() {
    let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4())));
    let usage = gateway.usage_counters("strix");

    // A dispatched request that streamed back fine.
    usage.add_request();
    let timed = Timed::new(usage.clone());
    std::thread::sleep(Duration::from_millis(2));
    drop(timed);
    let after_success = usage.snapshot();
    assert_eq!(
        (after_success.requests, after_success.failures, after_success.fallbacks),
        (1, 0, 0)
    );
    assert!(after_success.last_request_at.is_some());

    // Each of the three gateway-level fallback errors, with a fallback model configured.
    let fallback = provider("strix", Some("sonnet"));
    for _ in 0..3 {
        usage.add_request();
        usage.add_failure_with_fallback(&fallback, GatewayFailure::Unreachable);
    }
    let after_errors = usage.snapshot();
    assert_eq!(
        (after_errors.requests, after_errors.failures, after_errors.fallbacks),
        (4, 3, 3)
    );

    // An upstream status >= 400 is a failure without a fallback answer.
    usage.add_request();
    usage.add_failure(GatewayFailure::UpstreamError);
    let after_status = usage.snapshot();
    assert_eq!(
        (after_status.requests, after_status.failures, after_status.fallbacks),
        (5, 4, 3)
    );
    assert!(
        after_status.duration_ms > 0,
        "the dropped timer recorded the request's wall-clock time"
    );

    // Without a fallback model the failure is counted, the predicted fallback is not.
    usage.add_failure_with_fallback(&provider("strix", None), GatewayFailure::UpstreamError);
    assert_eq!(usage.snapshot().fallbacks, 3);
    assert_eq!(usage.snapshot().failures, 5);
}

/// A tally with no requests has no start instant, the first counted request sets it once, and the
/// instant survives a flush and reload — an older `provider-usage.json` without one loads as `None`.
#[test]
fn the_tally_starts_at_its_first_request_and_a_restart_keeps_that_instant() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    let usage = gateway.usage_counters("strix");
    assert_eq!(usage.snapshot().since, None, "nothing counted yet, so no start instant");

    usage.add_request();
    let since = usage.snapshot().since.expect("the first counted request starts the tally");
    usage.add_request();
    assert_eq!(usage.snapshot().since, Some(since), "a later request does not move the start");

    gateway.flush_usage();
    let reopened = usage_gateway(&dir).usage("strix");
    assert_eq!(reopened.since, Some(since), "the reload keeps the tally's start instant");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn usage_with(requests: u64, failures: u64, duration_ms: u64) -> ProviderUsage {
    ProviderUsage {
        requests,
        failures,
        fallbacks: 0,
        duration_ms,
        last_request_at: None,
        since: None,
        last_failure: None,
    }
}

#[test]
fn a_provider_with_no_requests_is_not_rated_and_not_degraded() {
    let health = health(&ProviderUsage::default());
    assert_eq!(
        health,
        UsageHealth {
            failure_pct: 0.0,
            avg_latency_ms: 0,
            rated: false,
            degraded: false,
            last_failure: None
        }
    );
}

/// A terrible rate on a handful of requests is noise — the fan-out this rule exists to catch starts
/// at tens of requests — so it never reads as degraded.
#[test]
fn a_terrible_rate_on_a_handful_of_requests_is_not_rated_and_therefore_not_degraded() {
    let health = health(&usage_with(10, 9, 90_000));
    assert_eq!(health.failure_pct, 90.0);
    assert_eq!(health.avg_latency_ms, 9_000);
    assert!(!health.rated, "10 requests is under HEALTH_MIN_SAMPLE");
    assert!(
        !health.degraded,
        "an unrated provider is never degraded, however terrible its rate"
    );
}

/// The numbers behind this rule: one provider taking 29.4% of 32 689 requests, at 12.8 s each.
#[test]
fn the_fanout_that_prompted_this_rule_reads_as_degraded_at_29_4_pct_and_12_800_ms() {
    let health = health(&usage_with(32_689, 9_599, 418_419_200));
    assert_eq!(health.failure_pct, 29.4);
    assert_eq!(health.avg_latency_ms, 12_800);
    assert!(health.rated);
    assert!(health.degraded, "29.4% is far past DEGRADED_PCT on a large sample");
}

#[test]
fn a_provider_is_degraded_from_ten_percent_exactly_and_not_below_it() {
    // 99 failures in 1 000 requests rounds to 9.9% — rated, but under the line.
    let under = health(&usage_with(1_000, 99, 0));
    assert_eq!(under.failure_pct, 9.9);
    assert!(under.rated);
    assert!(!under.degraded, "9.9% is just under DEGRADED_PCT");

    // 5 failures in the minimum 50 requests is exactly 10% — and exactly the sample size.
    let at = health(&usage_with(HEALTH_MIN_SAMPLE, 5, 0));
    assert_eq!(at.failure_pct, 10.0);
    assert!(at.rated);
    assert!(at.degraded, "10.0% is at DEGRADED_PCT, on the smallest sample that rates");
}

#[test]
fn failure_pct_is_rounded_to_one_decimal_and_latency_is_whole_milliseconds() {
    // 3/7 is 42.857…%, rounded to one decimal place.
    let health = health(&usage_with(7, 3, 10_001));
    assert_eq!(health.failure_pct, 42.9);
    assert_eq!(health.avg_latency_ms, 1_428, "latency is duration_ms/requests, truncated");
}

/// A request that queues past `queue_timeout_secs` never reaches the provider: it still counts as a
/// request and a fallback failure, but `proxy` only creates the `Timed` guard once the slot is in hand,
/// so the queue wait adds nothing to `duration_ms`.
#[tokio::test]
async fn a_queue_timeout_counts_a_request_and_a_failure_but_no_duration() {
    let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4())));
    let usage = gateway.usage_counters("strix");
    // The provider's only slot is held by a request already in flight, and the one below is given no
    // queue time to speak of, so its wait gives up immediately.
    let held = gateway.slots("strix", Some(1)).unwrap().acquire_owned().await.unwrap();
    let queued_out = Provider {
        queue_timeout_secs: Some(0),
        ..provider("strix", Some("sonnet"))
    };

    // The queue-timeout path in `proxy`, in its order: the attempt counts before it waits, the wait
    // times out, and the failure carries the fallback prediction — with no timer covering any of it.
    usage.add_request();
    let acquired = tokio::time::timeout(
        Duration::from_secs(queued_out.queue_timeout_secs()),
        gateway.slots("strix", Some(1)).unwrap().acquire_owned(),
    )
    .await;
    assert!(acquired.is_err(), "the wait gives up while the first request holds the slot");
    usage.add_failure_with_fallback(&queued_out, GatewayFailure::QueueFull);

    let snapshot = usage.snapshot();
    assert_eq!((snapshot.requests, snapshot.failures, snapshot.fallbacks), (1, 1, 1));
    assert_eq!(
        snapshot.duration_ms, 0,
        "queued time is not dispatched time: nothing reached the provider"
    );
    drop(held);
}

/// A response whose headers arrived but whose body then failed still counts as a failure: the colony
/// got no usable response. Counted once per request, never also at the header phase when the status
/// was >= 400.
#[tokio::test]
async fn an_openai_body_that_fails_after_the_headers_still_counts_as_a_failure() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    let strix = provider("strix", Some("sonnet"));
    let usage = Arc::new(UsageCounters::default());
    let info = openai::RequestInfo {
        model: "gpt-5.5".into(),
        stream: false,
    };
    let broken_body = |status: u16| {
        let reset: futures_util::stream::Once<futures_util::future::Ready<Result<Bytes, std::io::Error>>> =
            futures_util::stream::once(futures_util::future::ready(Err(std::io::Error::other(
                "connection reset mid-body",
            ))));
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(status)
                .body(reqwest::Body::wrap_stream(reset))
                .unwrap(),
        )
    };
    let response = openai_response(
        broken_body(200),
        guards(),
        usage.clone(),
        Box::new(|_| {}),
        Duration::from_secs(30),
        &info,
        &gateway,
        &strix,
        false,
        "strix",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(usage.snapshot().failures, 1, "the body-phase failure is counted");

    // A >= 400 status whose body then fails must not count twice: the body-phase count is the only one.
    let response = openai_response(
        broken_body(500),
        guards(),
        usage.clone(),
        Box::new(|_| {}),
        Duration::from_secs(30),
        &info,
        &gateway,
        &strix,
        false,
        "strix",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        usage.snapshot().failures,
        2,
        "one per request, never a header-phase count on top of the body-phase one"
    );
    assert_eq!(
        usage.snapshot().last_failure.as_deref(),
        Some("body_read_failed"),
        "the body-phase failure names its branch"
    );

    // A plain upstream error (not quota) counts one failure and names its own branch, with no
    // Claude retry on offer.
    let response = openai_response(
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(500)
                .body(reqwest::Body::from(r#"{"error":{"message":"kaboom"}}"#))
                .unwrap(),
        ),
        guards(),
        usage.clone(),
        Box::new(|_| {}),
        Duration::from_secs(30),
        &info,
        &gateway,
        &strix,
        false,
        "strix",
        None,
        None,
    )
    .await;
    assert!(
        !response.headers().contains_key(FALLBACK_HEADER),
        "an upstream error earns no retry"
    );
    assert_eq!(
        usage.snapshot().last_failure.as_deref(),
        Some("upstream_error"),
        "the non-quota branch names itself"
    );
}

/// A quota error records the provider as exhausted and names it in headers, offering the
/// Claude fallback exactly when the provider has a `fallback_model`.
#[tokio::test]
async fn an_openai_quota_error_marks_the_provider_and_offers_fallback() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    let info = openai::RequestInfo {
        model: "gpt-5.5".into(),
        stream: false,
    };
    let quota_body = || {
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(429)
                .body(reqwest::Body::from(
                    r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota."}}"#,
                ))
                .unwrap(),
        )
    };
    let answer = |fallback: bool| {
        let gateway = &gateway;
        let info = &info;
        async move {
            let model = fallback.then_some("sonnet");
            let strix = provider("strix", model);
            openai_response(
                quota_body(),
                guards(),
                gateway.usage_counters("strix"),
                Box::new(|_| {}),
                Duration::from_secs(30),
                info,
                gateway,
                &strix,
                model.is_some(),
                "strix",
                None,
                None,
            )
            .await
        }
    };
    let response = answer(true).await;
    assert!(gateway.is_quota_exhausted("strix"), "the plan is recorded as out");
    assert!(
        response.headers().contains_key(QUOTA_HEADER),
        "the answer names the exhaustion"
    );
    assert_eq!(
        response.headers().get(FALLBACK_HEADER).and_then(|v| v.to_str().ok()),
        Some(provider_quota::QUOTA_FALLBACK),
        "a fallback_model earns the Claude retry"
    );
    gateway.forget_quota("strix");
    let response = answer(false).await;
    assert!(gateway.is_quota_exhausted("strix"), "marked whatever the fallback");
    assert!(response.headers().contains_key(QUOTA_HEADER));
    assert!(
        !response.headers().contains_key(FALLBACK_HEADER),
        "no fallback_model, no retry"
    );
    assert_eq!(
        gateway.usage("strix").last_failure.as_deref(),
        Some("quota_exhausted"),
        "the last failure code is the quota, whatever the fallback"
    );
}

/// An upstream 2xx clears the provider's quota record, so the queue that reads
/// `quota_exhausted()` unpauses without waiting for a reset or a re-probe.
#[test]
fn upstream_success_clears_a_quota_record() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    gateway.mark_quota_exhausted("strix", None, None);
    assert!(gateway.is_quota_exhausted("strix"), "marked, so exhausted");
    assert_eq!(gateway.quota_exhausted().len(), 1, "the queue would pause on this");
    gateway.clear_quota_on_success("strix");
    assert!(!gateway.is_quota_exhausted("strix"), "success proves the plan is back");
    assert!(gateway.quota_exhausted().is_empty(), "nothing exhausted, no pause");
    assert!(!usage_gateway(&dir).is_quota_exhausted("strix"), "the clear reached disk too");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The account record pauses alone, marks no real provider, and survives both a routed
/// success and a restart: only its lapse or an explicit account resume clears it.
#[test]
fn an_account_mark_holds_without_marking_any_provider() {
    let dir = std::env::temp_dir().join(format!("colonizer-account-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    gateway.mark_account_quota_exhausted(Some("7am (UTC)".into()), Some(Utc::now().timestamp() + 3600));
    assert!(gateway.is_account_quota_exhausted(), "the account cap holds");
    assert!(!gateway.is_quota_exhausted("strix"), "a healthy provider stays healthy");
    assert!(
        gateway.quota_exhausted().iter().any(|(id, _, _)| id == ACCOUNT_QUOTA_ID),
        "the queue's unnamed rule sees the account record"
    );
    // One routed success proves nothing about the account's own cap.
    gateway.mark_quota_exhausted("strix", None, None);
    gateway.clear_quota_on_success("strix");
    assert!(!gateway.is_quota_exhausted("strix"), "the provider record cleared");
    assert!(gateway.is_account_quota_exhausted(), "the account pause holds through it");
    // ... nor does clearing for the account id itself: only lapse or explicit resume.
    gateway.clear_quota_on_success(ACCOUNT_QUOTA_ID);
    assert!(
        gateway.is_account_quota_exhausted(),
        "a success never lifts the account pause"
    );
    gateway.forget_account_quota();
    assert!(!gateway.is_account_quota_exhausted(), "the explicit resume lifts it");
    assert!(gateway.quota_exhausted().is_empty());

    // And the record rides out a restart in provider-quota.json.
    gateway.mark_account_quota_exhausted(Some("7am (UTC)".into()), Some(Utc::now().timestamp() + 3600));
    drop(gateway);
    let reopened = usage_gateway(&dir);
    assert!(reopened.is_account_quota_exhausted(), "the reload keeps the account pause");
    assert_eq!(reopened.account_quota_state().unwrap().reset_at.as_deref(), Some("7am (UTC)"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A quota record survives a restart until its reset, so a restarted mothership does not resume
/// every parked colony into the same exhausted plan; a record that lapsed meanwhile is dropped.
#[test]
fn a_quota_record_survives_a_restart_until_its_reset() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    gateway.mark_quota_exhausted("strix", Some("09-23 07:54 UTC".into()), Some(Utc::now().timestamp() + 3600));
    gateway.mark_quota_exhausted("zai", None, None);
    drop(gateway);

    let reopened = usage_gateway(&dir);
    assert!(reopened.is_quota_exhausted("strix"), "the reset is ahead, so still out");
    assert_eq!(
        reopened.quota_state("strix").unwrap().reset_at.as_deref(),
        Some("09-23 07:54 UTC")
    );
    assert!(
        reopened.is_quota_exhausted("zai"),
        "a reset-less mark keeps its TTL across the restart"
    );

    // Reset passed (and a reset-less mark past its TTL) while the mothership was down.
    reopened.mark_quota_exhausted("strix", None, Some(Utc::now().timestamp() - 10));
    reopened.quota.lock().unwrap().get_mut("zai").unwrap().since =
        Utc::now() - chrono::Duration::seconds(provider_quota::QUOTA_DEFAULT_TTL_SECS + 1);
    reopened.write_quota(&reopened.quota.lock().unwrap());
    let after_reset = usage_gateway(&dir);
    assert!(!after_reset.is_quota_exhausted("strix"), "past its reset, recovered");
    assert!(after_reset.quota_state("strix").is_none(), "and dropped on load");
    assert!(after_reset.quota_state("zai").is_none(), "past its TTL, dropped on load");

    std::fs::write(dir.join("provider-quota.json"), b"{not json").unwrap();
    assert!(
        usage_gateway(&dir).quota_exhausted().is_empty(),
        "a corrupt file means nothing is out"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A reset-less mark counts while fresh and lapses past its TTL, bounding the retry loop.
#[test]
fn a_reset_less_mark_lapses_past_its_ttl() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    gateway.mark_quota_exhausted("strix", None, None);
    assert!(gateway.is_quota_exhausted("strix"), "a fresh reset-less mark counts");
    gateway.quota.lock().unwrap().get_mut("strix").unwrap().since =
        chrono::Utc::now() - chrono::Duration::seconds(provider_quota::QUOTA_DEFAULT_TTL_SECS + 1);
    assert!(!gateway.is_quota_exhausted("strix"), "past TTL reads as recovered");
    assert!(
        gateway.quota_exhausted().is_empty(),
        "the queue and resume see the recovery too"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn usage_survives_a_restart_and_a_broken_file_degrades_to_defaults() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    let usage = gateway.usage_counters("strix");
    usage.add_request();
    gateway.flush_usage();
    assert!(dir.join("provider-usage.json").exists(), "a dirty flush writes the file");

    let reopened = usage_gateway(&dir);
    let reopened_usage = reopened.usage("strix");
    assert_eq!(
        (reopened_usage.requests, reopened_usage.failures, reopened_usage.fallbacks),
        (1, 0, 0)
    );
    assert_eq!(reopened_usage.last_request_at, usage.snapshot().last_request_at);

    // Two providers dirty at once flush together, and the flush clears every flag: after it, removing
    // the file and flushing again must not write it back, since both providers were persisted.
    usage.add_request();
    gateway.usage_counters("loki").add_request();
    gateway.flush_usage();
    std::fs::remove_file(dir.join("provider-usage.json")).unwrap();
    gateway.flush_usage();
    assert!(
        !dir.join("provider-usage.json").exists(),
        "a flush with nothing left dirty does not write the file"
    );

    std::fs::write(dir.join("provider-usage.json"), "{not json").unwrap();
    assert_eq!(
        usage_gateway(&dir).usage("strix"),
        ProviderUsage::default(),
        "a corrupt file means empty counters"
    );
    assert_eq!(
        usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()))).usage("strix"),
        ProviderUsage::default(),
        "a missing file means empty counters"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn deleting_a_provider_forgets_its_usage_and_flushes_the_removal() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    gateway.usage_counters("strix").add_request();
    gateway.flush_usage();
    gateway.forget_usage("strix");
    assert_eq!(
        usage_gateway(&dir).usage("strix"),
        ProviderUsage::default(),
        "the removal is written at once"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The flush loop, the shutdown flush and `forget_usage` can write at the same time, so every write
/// gets its own tmp path: whatever the interleaving and whichever rename lands last, the file on disk
/// is a complete snapshot that parses — never a half-overwritten one — and no tmp file is left behind.
#[test]
fn concurrent_writers_always_leave_a_parseable_usage_file() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = Arc::new(usage_gateway(&dir));
    // Writers whose snapshots differ in size — ids padded to different lengths, the map growing as they
    // go — the interleaving that used to let a shorter write cut a longer one off mid-JSON.
    let threads: Vec<_> = (0..3)
        .map(|t| {
            let gateway = gateway.clone();
            std::thread::spawn(move || {
                let pad = "x".repeat((t + 1) * 64);
                for i in 0..40 {
                    gateway.usage_counters(&format!("w{t}-{i}-{pad}")).add_request();
                    gateway.write_usage();
                }
            })
        })
        .chain(std::iter::once({
            let gateway = gateway.clone();
            std::thread::spawn(move || {
                for i in 0..40 {
                    gateway.usage_counters(&format!("gone-{i}")).add_request();
                    gateway.forget_usage(&format!("gone-{i}"));
                }
            })
        }))
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    let saved: BTreeMap<String, ProviderUsage> =
        serde_json::from_slice(&std::fs::read(dir.join("provider-usage.json")).expect("the last write left the file in place"))
            .expect("the file always parses, whatever the interleaving");
    assert!(!saved.is_empty(), "the surviving providers are on disk");
    for (id, usage) in &saved {
        let kept = id.starts_with("w0-") || id.starts_with("w1-") || id.starts_with("w2-");
        assert!(kept || id.starts_with("gone-"), "unexpected provider {id}");
        assert_eq!(usage.requests, 1, "provider {id} kept its tally");
    }
    assert!(
        std::fs::read_dir(&dir)
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "provider-usage.json"),
        "no tmp file is left behind"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn slots_follow_the_configured_limit() {
    let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-gateway-{}", uuid::Uuid::new_v4())));
    assert!(gateway.slots("local", None).is_none());
    let one = gateway.slots("local", Some(1)).unwrap();
    let held = one.clone().acquire_owned().await.unwrap();
    assert!(gateway.slots("local", Some(1)).unwrap().try_acquire().is_err());
    // Raising the limit takes effect immediately.
    assert!(gateway.slots("local", Some(2)).unwrap().try_acquire().is_ok());
    drop(held);

    let busy = Counted::new(&gateway.colony_counter("abc"));
    assert!(gateway.colony_busy("abc"));
    drop(busy);
    assert!(!gateway.colony_busy("abc"));
    assert!(!gateway.colony_busy("unknown"));
}

#[test]
fn tokens_compare_in_constant_time() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
}

fn guards() -> Guards {
    (
        Counted::new(&Default::default()),
        Counted::new(&Default::default()),
        None,
        Timed::new(Default::default()),
    )
}

/// A mock provider stream that yields `items` spaced out by their delays.
fn delayed_chunks(items: Vec<(Duration, Bytes)>) -> impl futures_util::Stream<Item = reqwest::Result<Bytes>> {
    futures_util::stream::unfold(items.into_iter(), |mut it| async move {
        let (delay, chunk) = it.next()?;
        tokio::time::sleep(delay).await;
        Some((Ok(chunk), it))
    })
}

fn openai_chunk(delta: &str, finish_reason: &str) -> Bytes {
    Bytes::from(format!(
        "data: {{\"id\":\"c\",\"choices\":[{{\"delta\":{delta},\"finish_reason\":{finish_reason}}}]}}\n\n"
    ))
}

#[tokio::test(start_paused = true)]
async fn translated_streams_still_get_pinged_through_a_silent_prefill() {
    let upstream = delayed_chunks(vec![
        (Duration::ZERO, openai_chunk(r#"{"role":"assistant"}"#, "null")),
        (
            Duration::from_secs(40),
            [
                openai_chunk(r#"{"content":"hi"}"#, "\"stop\""),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ]
            .concat()
            .into(),
        ),
    ]);
    let body = stream_body(
        openai::translate_stream(upstream, "gpt-5.5".into(), |_| {}),
        guards(),
        Duration::from_secs(120),
        true,
    );
    tokio::pin!(body);

    let (mut pings, mut out) = (0, Vec::new());
    while let Some(chunk) = body.next().await {
        let chunk = chunk.unwrap();
        if chunk.as_ref() == SSE_PING {
            pings += 1;
            assert!(out.is_empty() || out.ends_with(b"\n\n"), "a ping landed inside an event");
        } else {
            out.extend_from_slice(&chunk);
        }
    }
    assert_eq!(pings, 2);
    assert!(out.ends_with(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
}

/// Upstream chunks that translate to nothing still reset the silence deadline. Here the provider talks
/// every 10 s for 40 s against a 25 s timeout; counting only translated bytes would kill it at 25 s.
#[tokio::test(start_paused = true)]
async fn untranslatable_chunks_still_count_as_activity() {
    let empty = || (Duration::from_secs(10), openai_chunk("{}", "null"));
    let upstream = delayed_chunks(vec![
        (Duration::ZERO, openai_chunk(r#"{"role":"assistant"}"#, "null")),
        empty(),
        empty(),
        (Duration::from_secs(10), Bytes::from_static(b": OPENROUTER PROCESSING\n\n")),
        (
            Duration::from_secs(10),
            [
                openai_chunk(r#"{"content":"done"}"#, "\"stop\""),
                Bytes::from_static(b"data: [DONE]\n\n"),
            ]
            .concat()
            .into(),
        ),
    ]);
    let body = stream_body(
        openai::translate_stream(upstream, "gpt-5.5".into(), |_| {}),
        guards(),
        Duration::from_secs(25),
        true,
    );
    tokio::pin!(body);
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.expect("the stream must not time out"));
    }
    assert!(std::str::from_utf8(&out).unwrap().contains("event: message_stop"));
}

#[tokio::test(start_paused = true)]
async fn sse_streams_get_pinged_through_a_silent_prefill() {
    let chunks = delayed_chunks(vec![
        (Duration::ZERO, Bytes::from_static(b"event: message_start\n\n")),
        // Long enough silence to cross two SSE_PING_INTERVAL (15s) ticks before the real byte.
        (Duration::from_secs(40), Bytes::from_static(b"event: message_stop\n\n")),
    ]);
    let body = stream_body(chunks, guards(), Duration::from_secs(120), true);
    tokio::pin!(body);

    let mut pings = 0;
    let mut reals = 0;
    while let Some(chunk) = body.next().await {
        if chunk.unwrap().as_ref() == SSE_PING {
            pings += 1;
        } else {
            reals += 1;
        }
    }
    assert_eq!(reals, 2, "both real chunks should still arrive");
    assert_eq!(
        pings, 2,
        "one ping per 15s tick of the 40s silent gap, not reset by pings themselves"
    );
}

#[tokio::test(start_paused = true)]
async fn non_sse_responses_never_get_pinged() {
    let chunks = delayed_chunks(vec![(Duration::from_secs(40), Bytes::from_static(b"{\"ok\":true}"))]);
    let body = stream_body(chunks, guards(), Duration::from_secs(120), false);
    tokio::pin!(body);
    let chunk = body.next().await.unwrap().unwrap();
    assert_eq!(chunk.as_ref(), b"{\"ok\":true}");
    assert!(body.next().await.is_none());
}

/// Drains `chunks` through a counting body over an Anthropic SSE tap, returning the bytes the colony
/// would receive and the usage the tap read.
async fn counted(chunks: Vec<std::io::Result<Bytes>>, is_sse: bool) -> (Vec<u8>, Option<Usage>) {
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    let body = counted_body(
        futures_util::stream::iter(chunks),
        UsageTap::new(Shape::Anthropic, is_sse),
        Some(Box::new(move |usage| *sink.lock().unwrap() = Some(usage))),
        None,
    );
    tokio::pin!(body);
    let mut forwarded = Vec::new();
    while let Some(chunk) = body.next().await {
        forwarded.extend_from_slice(&chunk.expect("the body forwards"));
    }
    let counted = seen.lock().unwrap().take();
    (forwarded, counted)
}

fn message_start(input: u64, cache_read: u64, cache_write: u64) -> String {
    format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{input},\
             \"cache_read_input_tokens\":{cache_read},\"cache_creation_input_tokens\":{cache_write},\"output_tokens\":1}}}}}}\n\n"
    )
}

fn message_delta(output: u64) -> String {
    format!(
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output}}}}}\n\n"
    )
}

#[tokio::test]
async fn an_anthropic_stream_is_counted_without_its_bytes_being_touched() {
    let body = message_start(25, 40, 5)
        + "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n"
        + &message_delta(15);
    let (forwarded, counted) = counted(vec![Ok(Bytes::from(body.clone()))], true).await;
    assert_eq!(
        String::from_utf8_lossy(&forwarded),
        body,
        "the colony receives exactly the bytes upstream sent"
    );
    assert_eq!(
        counted,
        Some(Usage {
            input_tokens: 25,
            output_tokens: 15,
            cache_read_tokens: 40,
            cache_write_tokens: 5,
            thinking_tokens: 0
        })
    );
}

#[tokio::test]
async fn counting_survives_chunks_split_mid_event() {
    let body = message_start(25, 40, 5) + &message_delta(15);
    let pieces: Vec<std::io::Result<Bytes>> = body
        .as_bytes()
        .chunks(7)
        .map(|piece| Ok(Bytes::copy_from_slice(piece)))
        .collect();
    let (forwarded, counted) = counted(pieces, true).await;
    assert_eq!(String::from_utf8_lossy(&forwarded), body);
    assert_eq!(
        counted,
        Some(Usage {
            input_tokens: 25,
            output_tokens: 15,
            cache_read_tokens: 40,
            cache_write_tokens: 5,
            thinking_tokens: 0
        }),
        "a data line split across chunks is waited for, not half-read"
    );
}

#[tokio::test]
async fn an_anthropic_json_body_is_counted_and_passes_through_unchanged() {
    let body = br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":12,"cache_read_input_tokens":80,"cache_creation_input_tokens":2,"output_tokens":9}}"#;
    let (forwarded, counted) = counted(vec![Ok(Bytes::from_static(body))], false).await;
    assert_eq!(
        forwarded,
        body.to_vec(),
        "the colony receives exactly the bytes upstream sent"
    );
    assert_eq!(
        counted,
        Some(Usage {
            input_tokens: 12,
            output_tokens: 9,
            cache_read_tokens: 80,
            cache_write_tokens: 2,
            thinking_tokens: 0
        })
    );
}

/// Meta's Anthropic-wire responses (and any other reasoning model on that wire) report thinking
/// tokens spent from the output budget in `usage.output_tokens_details.thinking_tokens`; a response
/// without the field (every provider before Meta) must still count as zero, not error.
#[test]
fn anthropic_usage_reads_thinking_tokens_when_present() {
    let with_thinking = json!({"input_tokens": 12, "output_tokens": 9, "output_tokens_details": {"thinking_tokens": 40}});
    assert_eq!(anthropic_usage(&with_thinking).thinking_tokens, 40);

    let without = json!({"input_tokens": 12, "output_tokens": 9});
    assert_eq!(anthropic_usage(&without).thinking_tokens, 0);
}

#[tokio::test]
async fn an_anthropic_json_body_with_thinking_tokens_prices_them() {
    let body = br#"{"id":"msg_1","type":"message","usage":{"input_tokens":12,"output_tokens":9,"output_tokens_details":{"thinking_tokens":40}}}"#;
    let (_, counted) = counted(vec![Ok(Bytes::from_static(body))], false).await;
    assert_eq!(counted.map(|u| u.thinking_tokens), Some(40));
}

#[tokio::test]
async fn message_delta_carries_the_running_thinking_total() {
    let body = message_start(25, 40, 5)
        + "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{},\"usage\":{\"output_tokens\":15,\"output_tokens_details\":{\"thinking_tokens\":6}}}\n\n";
    let (_, counted) = counted(vec![Ok(Bytes::from(body))], true).await;
    assert_eq!(
        counted.map(|u| (u.output_tokens, u.thinking_tokens)),
        Some((15, 6)),
        "message_delta's usage carries thinking tokens the same way it carries output tokens"
    );
}

#[tokio::test]
async fn a_malformed_or_truncated_body_counts_nothing_and_still_forwards() {
    // A data line that never becomes valid JSON, and a stream cut off mid-event, both account zero.
    for (body, is_sse) in [
        (
            b"event: message_start\ndata: not json\n\nevent: message_delta\ndata: {}\n\n".as_slice(),
            true,
        ),
        (b"event: message_start\ndata: {\"type\":\"message_star".as_slice(), true),
        (b"<html>gateway error</html>".as_slice(), false),
        (b" &".as_slice(), false),
    ] {
        let (forwarded, counted) = counted(vec![Ok(Bytes::from_static(body))], is_sse).await;
        assert_eq!(forwarded, body.to_vec(), "bytes must pass unchanged no matter what they say");
        assert_eq!(counted, None, "no spend is recorded for {body:?}");
    }
}

/// `pings` are injected by `stream_body` upstream of the tap, so the tap must read past them.
#[tokio::test]
async fn keep_alive_pings_do_not_confuse_the_counting() {
    let body = message_start(10, 0, 0) + ": keep-alive\n\n" + &message_delta(4);
    let (forwarded, counted) = counted(vec![Ok(Bytes::from(body.clone()))], true).await;
    assert_eq!(String::from_utf8_lossy(&forwarded), body);
    assert_eq!(
        counted,
        Some(Usage {
            input_tokens: 10,
            output_tokens: 4,
            ..Default::default()
        })
    );
}

#[tokio::test]
async fn an_empty_body_records_nothing_at_all() {
    let (forwarded, counted) = counted(vec![], true).await;
    assert!(forwarded.is_empty());
    assert_eq!(counted, None, "no body end callback fires for a body that never arrived");
}

#[tokio::test(start_paused = true)]
async fn pings_never_land_inside_a_partly_forwarded_event() {
    let chunks = delayed_chunks(vec![
        (
            Duration::ZERO,
            Bytes::from_static(b"event: content_block_delta\ndata: {\"delta\":"),
        ),
        (Duration::from_secs(40), Bytes::from_static(b"\"hi\"}\n\n")),
        (Duration::from_secs(40), Bytes::from_static(b"event: message_stop\n\n")),
    ]);
    let body = stream_body(chunks, guards(), Duration::from_secs(120), true);
    tokio::pin!(body);
    let mut forwarded = Vec::new();
    while let Some(chunk) = body.next().await {
        forwarded.extend_from_slice(&chunk.unwrap());
    }
    let expected =
        b"event: content_block_delta\ndata: {\"delta\":\"hi\"}\n\n: keep-alive\n\n: keep-alive\n\nevent: message_stop\n\n";
    assert_eq!(String::from_utf8_lossy(&forwarded), String::from_utf8_lossy(expected));
}

#[tokio::test(start_paused = true)]
async fn a_provider_silent_past_the_overall_timeout_still_errors_despite_pings() {
    let chunks = delayed_chunks(vec![(Duration::from_secs(600), Bytes::from_static(b"too late"))]);
    let body = stream_body(chunks, guards(), Duration::from_secs(50), true);
    tokio::pin!(body);
    let mut pings = 0;
    loop {
        match body.next().await.unwrap() {
            Ok(chunk) => {
                assert_eq!(chunk.as_ref(), SSE_PING);
                pings += 1;
            }
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
                break;
            }
        }
    }
    assert!(pings >= 3, "expected pings while waiting, got {pings}");
}

/// Issue #1018: the URL a 404 names is the one the request hit, minus anything secret.
#[test]
fn redacted_urls_keep_the_path_and_drop_credentials_and_query() {
    let url = reqwest::Url::parse("https://user:secret@ark.example.com:8443/api/coding/v3/chat/completions?key=abc#f").unwrap();
    assert_eq!(
        redacted_url(&url),
        "https://ark.example.com:8443/api/coding/v3/chat/completions"
    );
    assert_eq!(
        wrong_route_hint(404, "https://h/v3/v1/chat/completions").as_deref(),
        Some("upstream answered 404 at https://h/v3/v1/chat/completions; check the provider's base URL")
    );
    assert!(wrong_route_hint(405, "u").is_some());
    assert!(wrong_route_hint(500, "u").is_none());
    assert!(wrong_route_hint(401, "u").is_none());
}

/// Issue #1018: a translated route's upstream 404 reaches the colony as a 404 that names the URL
/// and the base URL as the likely cause, not as a generic error.
#[tokio::test]
async fn an_openai_404_names_the_url_and_the_base_url() {
    let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
    let gateway = usage_gateway(&dir);
    let ark = provider("ark", None);
    let info = openai::RequestInfo {
        model: "m".into(),
        stream: false,
    };
    let response = openai_response(
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(404)
                .body(reqwest::Body::from("404 page not found"))
                .unwrap(),
        ),
        guards(),
        Arc::new(UsageCounters::default()),
        Box::new(|_| {}),
        Duration::from_secs(30),
        &info,
        &gateway,
        &ark,
        false,
        "ark",
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["error"]["type"], "not_found_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("upstream answered 404 at "), "got: {message}");
    assert!(message.contains("check the provider's base URL"), "got: {message}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Issue #1018: the save-time test sends one token through the colony's own route and reports the
/// URL it hit and the status, so a wrong base path is caught at setup.
#[tokio::test]
async fn the_save_test_reports_the_url_and_status_it_hit() {
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let record = seen.clone();
    let router = Router::new()
        .route(
            "/api/coding/v3/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let record = record.clone();
                async move {
                    record.lock().unwrap().push(body);
                    axum::Json(json!({
                        "id": "x", "object": "chat.completion", "model": "ark-code",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "p"}, "finish_reason": "length"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        )
        .route(
            "/anthropic/v1/messages",
            axum::routing::post(|| async { axum::Json(json!({"type": "message", "content": []})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = std::env::temp_dir().join(format!("colonizer-gateway-test-{}", uuid::Uuid::new_v4()));
    let app = crate::tests::test_app(&root);
    let openai_at = |base: String| Provider {
        base_url: base,
        wire: crate::providers::Wire::Openai,
        models: vec!["ark-code".into()],
        ..provider("ark", None)
    };

    let good = test_request(&app, &openai_at(format!("http://{addr}/api/coding/v3/"))).await;
    assert_eq!(good["ok"], true, "got: {good}");
    assert_eq!(good["status"], 200);
    assert_eq!(good["url"], format!("http://{addr}/api/coding/v3/chat/completions"));
    assert_eq!(good["model"], "ark-code");
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["model"], "ark-code");

    // A base missing the provider's path: the test names the URL it hit and the likely cause.
    let wrong = test_request(&app, &openai_at(format!("http://{addr}/api/coding"))).await;
    assert_eq!(wrong["ok"], false);
    assert_eq!(wrong["status"], 404);
    assert_eq!(wrong["url"], format!("http://{addr}/api/coding/v1/chat/completions"));
    assert!(
        wrong["error"].as_str().unwrap().contains("check the provider's base URL"),
        "got: {wrong}"
    );

    let anthropic = Provider {
        base_url: format!("http://{addr}/anthropic"),
        models: vec!["m".into()],
        ..provider("a", None)
    };
    let answered = test_request(&app, &anthropic).await;
    assert_eq!(answered["ok"], true, "got: {answered}");
    assert_eq!(answered["url"], format!("http://{addr}/anthropic/v1/messages"));

    let unlisted = test_request(&app, &provider("none", None)).await;
    assert_eq!(unlisted["ok"], false);
    assert_eq!(unlisted["error"], "list at least one model to test with");
    let _ = std::fs::remove_dir_all(root);
}
