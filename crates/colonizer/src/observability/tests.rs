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
        save_anyway: false,
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
    assert_eq!(ids, vec!["otlp".to_string(), "file".to_string(), "loki".to_string()]);
}

#[tokio::test]
async fn observability_lists_every_provider_once_saved_and_persists_a_save() {
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
    assert_eq!(ids, vec!["otlp", "file", "loki"]);
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
    let err = save_observability(&app, "otlp", obs_settings(&[("metrics_interval_secs", json!(1))]), false)
        .await
        .unwrap_err();
    assert!(err.message().contains("between 5 and 86400"), "{}", err.message());
    std::fs::remove_dir_all(root).ok();
}

/// Plants a module choice straight into the in-memory store, past the save-time checks — the
/// compact field must still read a coherent word for a module the cockpit's validation would
/// never have let through, because the file on disk can say anything.
async fn plant(app: &Shared, provider: &str, enabled: bool, settings: &[(&str, Value)]) {
    app.modules.write().await.observability = Some(crate::config::ModuleChoice {
        provider: provider.into(),
        enabled,
        settings: obs_settings(settings),
    });
}

/// The compact `/api/status` field (#850): `off` when nothing is meant to run, `degraded` when
/// something is meant to run and is not healthy, `ok` when it is. Every rung is checked from the
/// same install so the ordering — verdict, then child, then the add-on's own word — is the one
/// the handler implements.
#[tokio::test]
async fn compact_status_maps_off_degraded_and_ok() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    assert_eq!(
        super::compact_status(&app).await,
        "off",
        "never configured is off, not a fault"
    );

    // Saved but switched off: still off — an operator who turned it off meant it off.
    plant(&app, "otlp", false, &[("endpoint", json!("https://otlp.example.com"))]).await;
    assert_eq!(super::compact_status(&app).await, "off");

    // Saved on with a config that cannot resolve (no endpoint): degraded, not off — the module
    // is meant to be running and is not.
    plant(&app, "otlp", true, &[]).await;
    assert_eq!(
        super::compact_status(&app).await,
        "degraded",
        "configured but invalid is degraded"
    );

    // A config that resolves: degraded while no supervision loop has a child up.
    plant(&app, "otlp", true, &[("endpoint", json!("https://otlp.example.com"))]).await;
    assert_eq!(
        super::compact_status(&app).await,
        "degraded",
        "configured but the child is not running"
    );

    // The loop says running and the add-on has written no complaint: ok.
    super::supervisor::test_set_runtime(&app.cfg.data_dir, "running");
    assert_eq!(super::compact_status(&app).await, "ok", "running with no status.json is ok");

    let status = |body: Value| {
        let path = super::supervisor::status_path(&app.cfg.data_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body.to_string()).unwrap()
    };
    // The add-on's own word, first overall then per signal.
    status(json!({"state": "retrying", "signals": {}}));
    assert_eq!(super::compact_status(&app).await, "degraded", "the add-on is retrying");
    status(json!({"state": "running", "signals": {"logs": {"state": "backing_off"}}}));
    assert_eq!(super::compact_status(&app).await, "degraded", "a signal is backing off");
    status(json!({"state": "running", "signals": {"logs": {"state": "ok"}}}));
    assert_eq!(super::compact_status(&app).await, "ok");

    // And the supervisor's verdict outranks a happy status.json.
    super::supervisor::test_set_runtime(&app.cfg.data_dir, "refused");
    assert_eq!(
        super::compact_status(&app).await,
        "degraded",
        "refused is degraded whatever the file says"
    );
    std::fs::remove_dir_all(root).ok();
}

/// The compact field reaches for the keychain (seconds) only once everything else says the install
/// is healthy: a malformed header list in the secret is the one fault no other read can see, so an
/// otherwise `ok` install that carries one must still read `degraded` — and the reads before that
/// never pay for the keychain at all.
#[tokio::test]
async fn compact_status_reads_the_header_secret_only_when_the_rest_is_healthy() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    plant(&app, "otlp", true, &[("endpoint", json!("https://otlp.example.com"))]).await;
    let headers = app.cfg.config_dir.join("observability-headers");
    std::fs::create_dir_all(headers.parent().unwrap()).unwrap();
    // A pair with no `=`: under the add-on's own rules a malformed list sends nothing, so the
    // exporter is not running what the operator configured.
    std::fs::write(&headers, "Authorization: Basic not-a-pair").unwrap();

    // Nothing is meant to run: off, and the secret is never consulted — the keychain read is the
    // slow part of this poll and an install that never turned export on never pays for it.
    plant(&app, "otlp", false, &[("endpoint", json!("https://otlp.example.com"))]).await;
    assert_eq!(super::compact_status(&app).await, "off");

    // Configured but no child up: degraded, again without the secret.
    plant(&app, "otlp", true, &[("endpoint", json!("https://otlp.example.com"))]).await;
    assert_eq!(super::compact_status(&app).await, "degraded");

    // Healthy on every other count: now the secret is read, and its verdict is the last word.
    super::supervisor::test_set_runtime(&app.cfg.data_dir, "running");
    assert_eq!(
        super::compact_status(&app).await,
        "degraded",
        "a malformed header list in the secret is an Invalid, not an ok install"
    );
    std::fs::write(&headers, "x-honeycomb-team=abc").unwrap();
    assert_eq!(super::compact_status(&app).await, "ok", "a good list leaves it ok");
    std::fs::remove_dir_all(root).ok();
}

/// #861: `relay_fleet_history` defaults off, saves like any switch, reaches the add-on's contract
/// settings, and the contract carries the relay list (empty until members report their own
/// exporters, #860).
#[tokio::test]
async fn relay_fleet_history_defaults_off_saves_and_reaches_the_contract() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let saved = save_observability(
        &app,
        "otlp",
        obs_settings(&[("endpoint", json!("https://otlp.example.com"))]),
        false,
    )
    .await
    .unwrap();
    // The save echoes what was sent: an unasked switch is simply absent, never on.
    assert_ne!(
        saved["settings"]["relay_fleet_history"],
        json!(true),
        "off unless asked: {:?}",
        saved["settings"]
    );
    let saved = save_observability(
        &app,
        "otlp",
        obs_settings(&[
            ("endpoint", json!("https://otlp.example.com")),
            ("relay_fleet_history", json!(true)),
        ]),
        false,
    )
    .await
    .unwrap();
    assert_eq!(saved["settings"]["relay_fleet_history"], json!(true));

    match crate::observability::supervisor::resolve(&app).await {
        crate::observability::env::Resolved::On(e) => {
            let contract = crate::observability::supervisor::contract(&app, &e.settings).await;
            assert_eq!(contract["settings"]["relay_fleet_history"], json!(true), "{contract}");
            assert_eq!(contract["relay_members"], json!([]), "{contract}");
        }
        _ => panic!("an enabled module with an endpoint resolves on"),
    }
    std::fs::remove_dir_all(root).ok();
}

/// Regenerates the committed archive-bundle fixture the observability add-on's tests read
/// (#859): a real tar+zstd bundle and its sidecar, written by the mothership's own archiver over
/// the v0.1.9 fixture's richer colony (`a1b2c3d4`: events, a finding, an `out/` file and the
/// credential `vm/session.json` the bundle must leave out). Run it on purpose with
/// `cargo test -p colonizer-harness --locked regenerate_archive_bundle_fixture -- --ignored`; the output
/// lands in `tests/fixtures/observability/archive/` and is committed, so it needs re-running only
/// when the bundle layout or the fixture colony moves.
#[tokio::test]
#[ignore = "writes the committed fixture; run it on purpose"]
async fn regenerate_archive_bundle_fixture() {
    let root = crate::tests::temp_root();
    // Pinned before the app can read one: the bundle's `mothership` host id keys every record and
    // trace id the add-on builds from this fixture, so it must never move.
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/host_id"), "host-1").unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/data-v0.1.9");
    copy_dir(&fixture.join("sessions/a1b2c3d4"), &root.join("data/sessions/a1b2c3d4"));
    let sessions: Vec<crate::sessions::Session> =
        serde_json::from_slice(&std::fs::read(fixture.join("sessions.json")).unwrap()).unwrap();
    let colony = sessions
        .iter()
        .find(|s| s.id == "a1b2c3d4")
        .expect("the fixture holds the colony");
    let app = crate::tests::test_app(&root);
    let rel = crate::archive::archive_session(&app, colony)
        .await
        .unwrap()
        .expect("the colony has files to archive");
    let bundle = root.join("data/archive").join(&rel);
    let sidecar = crate::archive::sidecar_of(&bundle);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/observability/archive");
    // Replaced wholesale, so a renamed revision of an older run never lingers beside the new one.
    std::fs::remove_dir_all(&out).ok();
    for (from, rel) in [
        (bundle.clone(), rel.clone()),
        (
            sidecar,
            crate::archive::sidecar_of(std::path::Path::new(&rel))
                .to_string_lossy()
                .into_owned(),
        ),
    ] {
        let to = out.join(&rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
    std::fs::remove_dir_all(root).ok();
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
