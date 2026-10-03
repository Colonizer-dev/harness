//! Save-time tests for the `observability` settings API (#840).
//!
//! They live under `observability/` rather than in `modules.rs`'s test module so that sibling pull
//! requests appending module tests do not collide with this issue's.

use crate::modules::{KINDS, UpdateModule, is_required, list, providers, update};
use crate::{AppError, Shared};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde_json::{Map, Value, json};

fn obs_settings(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

async fn save_observability(
    app: &Shared,
    provider: &str,
    settings: Map<String, Value>,
    confirm_content: bool,
) -> Result<Value, AppError> {
    let req = UpdateModule {
        provider: provider.into(),
        enabled: true,
        settings,
        confirm_content,
    };
    update(State(app.clone()), Path("observability".into()), Json(req))
        .await
        .map(|Json(value)| value)
}

#[test]
fn observability_is_a_kind_and_it_stays_disablable() {
    assert!(KINDS.contains(&"observability"));
    assert!(!is_required("observability"), "exporting off the machine is opt-in by design");
    let ids: Vec<_> = providers("observability", &[]).into_iter().map(|p| p.id).collect();
    assert_eq!(ids, vec!["otlp".to_string(), "file".to_string()]);
}

#[tokio::test]
async fn observability_lists_both_providers_once_saved_and_persists_a_save() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    // Absent until saved, so it is not listed yet — that is what keeps an untouched install's
    // modules.json free of a kind nobody asked for.
    let before = list(State(app.clone())).await.0;
    assert!(before.iter().all(|k| k["kind"] != "observability"), "{before:?}");

    let saved = save_observability(
        &app,
        "otlp",
        obs_settings(&[("endpoint", json!("https://otlp.example.com"))]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(saved["enabled"], json!(true));
    assert_eq!(saved["settings"]["endpoint"], json!("https://otlp.example.com"));

    let listed = list(State(app.clone())).await.0;
    let obs = listed
        .iter()
        .find(|k| k["kind"] == "observability")
        .expect("listed once saved");
    let ids: Vec<_> = obs["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["otlp", "file"]);
    assert_eq!(obs["provider"], json!("otlp"));

    // The other provider saves too, on the same kind.
    let file = save_observability(&app, "file", obs_settings(&[("max_mb", json!(128))]), false)
        .await
        .unwrap();
    assert_eq!(file["provider"], json!("file"));
    assert_eq!(file["settings"]["max_mb"], json!(128));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn observability_refuses_endpoint_credentials_and_queries() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    for (endpoint, why) in [
        ("https://user:pass@otlp.example.com", "user:pass@"),
        ("https://user@otlp.example.com", "user@"),
        ("https://otlp.example.com?token=abc", "the query string"),
    ] {
        let err = save_observability(&app, "otlp", obs_settings(&[("endpoint", json!(endpoint))]), false)
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{endpoint}");
        assert!(
            err.message().contains("observability-headers"),
            "{why} must be refused pointing at the secret: {}",
            err.message()
        );
    }
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn observability_refuses_plain_http_to_a_public_host_naming_allow_insecure() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let err = save_observability(
        &app,
        "otlp",
        obs_settings(&[("endpoint", json!("http://otlp.example.com:4318"))]),
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("allow_insecure"), "{}", err.message());

    // The switch the message names opens it; a private address needs no switch at all.
    let opened = save_observability(
        &app,
        "otlp",
        obs_settings(&[
            ("endpoint", json!("http://otlp.example.com:4318")),
            ("allow_insecure", json!(true)),
        ]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(opened["settings"]["allow_insecure"], json!(true));
    let private = save_observability(
        &app,
        "otlp",
        obs_settings(&[("endpoint", json!("http://10.0.0.5:4318"))]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(private["settings"]["endpoint"], json!("http://10.0.0.5:4318"));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn observability_refuses_grpc_and_grafana_cloud_before_it() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let err = save_observability(&app, "otlp", obs_settings(&[("protocol", json!("grpc"))]), false)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("no gRPC"), "{}", err.message());

    // The preset's own message comes first, so the operator is told the real reason.
    let err = save_observability(
        &app,
        "otlp",
        obs_settings(&[("preset", json!("grafana_cloud")), ("protocol", json!("grpc"))]),
        false,
    )
    .await
    .unwrap_err();
    assert!(err.message().contains("Grafana Cloud"), "{}", err.message());
    assert!(!err.message().contains("this build has no gRPC"), "{}", err.message());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn observability_content_needs_a_confirmation_that_is_never_persisted() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let with_content = |extra: &[(&str, Value)]| {
        let mut pairs = vec![("endpoint", json!("https://otlp.example.com"))];
        pairs.extend_from_slice(extra);
        obs_settings(&pairs)
    };
    // off -> on without the word is refused.
    let err = save_observability(&app, "otlp", with_content(&[("conversation_content", json!(true))]), false)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("confirm_content"), "{}", err.message());

    // off -> on with it is accepted.
    let saved = save_observability(&app, "otlp", with_content(&[("conversation_content", json!(true))]), true)
        .await
        .unwrap();
    assert_eq!(saved["settings"]["conversation_content"], json!(true));
    assert!(
        saved["settings"].get("confirm_content").is_none(),
        "the confirmation is a request field, not a setting: {saved}"
    );

    // Already on, an unrelated edit without the word is fine: the cockpit saves back everything
    // it holds, so demanding the word again would refuse every later save.
    let edited = save_observability(
        &app,
        "otlp",
        with_content(&[("conversation_content", json!(true)), ("timeout_secs", json!(5))]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(edited["settings"]["timeout_secs"], json!(5));

    // The word is per switch: content is already on, but turning thinking on now is a new stream.
    let err = save_observability(
        &app,
        "otlp",
        with_content(&[("conversation_content", json!(true)), ("conversation_thinking", json!(true))]),
        false,
    )
    .await
    .unwrap_err();
    assert!(err.message().contains("confirm_content"), "{}", err.message());

    let stored = app.modules.read().await.get("observability").unwrap().settings.clone();
    assert!(!stored.contains_key("confirm_content"), "{stored:?}");
    let on_disk = std::fs::read_to_string(app.modules_file()).unwrap();
    assert!(!on_disk.contains("confirm_content"), "{on_disk}");
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn observability_ranges_are_checked_at_save_time() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let err = save_observability(&app, "otlp", obs_settings(&[("trace_sample_ratio", json!(2))]), false)
        .await
        .unwrap_err();
    assert!(err.message().contains("between 0 and 1"), "{}", err.message());
    let err = save_observability(&app, "otlp", obs_settings(&[("max_attribute_bytes", json!(64))]), false)
        .await
        .unwrap_err();
    assert!(err.message().contains("between 128 and 8192"), "{}", err.message());
    std::fs::remove_dir_all(root).ok();
}
