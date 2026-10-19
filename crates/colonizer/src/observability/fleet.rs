//! The observability half of fleet health (#860): the block a member's reduced `/api/status`
//! answer carries beside `runner` and `fleet_sync`, and the type the owner parses it back into.
//!
//! The block is four fields — `enabled`, `state`, `failing_since`, `destination_hash` — and the
//! rule it answers to is that nothing that names the backend crosses machines: no endpoint, no
//! header name or value, no error text. The destination is a truncated SHA-256 of the protocol and
//! the endpoint exactly as the status page shows them, so two members of a fleet that configure
//! the same backend hash the same, and a stranger polling the status learns nothing it can invert.
//!
//! `failing_since` is this process's own clock: the add-on's `status.json` carries no "since" for
//! the state it is in, so the first status answer that saw the failure starts one, and it is
//! cleared the moment the exporter is not failing. `last_success_unix` is never read for it — a
//! success can be days old after an exporter that was off, and would overstate every failure.

use super::{env::Resolved, supervisor};
use crate::Shared;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// A member's `observability` block of the reduced `/api/status` (issue #860), parsed on the owner:
/// whether export is on, the four-word state, when the current failure began, and a hash of the
/// destination — the only thing that identifies the backend, and only as a hash.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PeerObservability {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub failing_since: Option<u64>,
    #[serde(default)]
    pub destination_hash: Option<String>,
}

impl PeerObservability {
    /// How long the exporter has been failing, at `now_unix` (unix seconds). `Some` only when
    /// export is on, the state is `failing` and the member named a start: anything else — off, an
    /// older member's absent block, a member that is starting or fine — is not a fault to age.
    pub fn failing_for_s(&self, now_unix: i64) -> Option<u64> {
        if !self.enabled || self.state != "failing" {
            return None;
        }
        self.failing_since.map(|since| (now_unix.max(0) as u64).saturating_sub(since))
    }
}

/// The block's `destination_hash`: the first 12 bytes of SHA-256 over `protocol` and `endpoint`,
/// as lowercase hex — the same shape the supervisor's config `fingerprint` takes. Only the pair is
/// hashed, never a header or a credential, so the same backend configured on two members of a
/// fleet yields the same hash and nothing else does.
fn destination_hash(protocol: &str, endpoint: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{protocol}\n{endpoint}").as_bytes());
    crate::util::hex(&digest.as_ref()[..12])
}

/// The block's `state` from what this process knows: the supervisor's runtime state for the data
/// dir, and — while the add-on runs — its own `status.json` state. Four words: `starting` (on, not
/// yet known to be either), `ok`, `failing`. `restarting` counts as failing: a crash-looping add-on
/// is down, and a routine restart after a healthy run clears long before the ten-minute threshold
/// the owner reads the age against. A runtime that has not run yet, or has not caught up with a
/// config just switched on, says only `starting`.
fn merged_state(runtime: &str, addon: Option<&str>) -> &'static str {
    match runtime {
        "running" => match addon {
            Some("running") => "ok",
            Some("retrying") | Some("auth_failed") => "failing",
            _ => "starting",
        },
        "starting" | "off" | "" => "starting",
        _ => "failing",
    }
}

/// When this process first saw the current exporter failure, per data dir, in unix seconds: there
/// is no "since" anywhere on disk, so the clock starts at the first status answer that saw the
/// failure and is cleared the moment the exporter stops failing.
fn failures() -> &'static Mutex<BTreeMap<PathBuf, u64>> {
    static F: OnceLock<Mutex<BTreeMap<PathBuf, u64>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

/// Stamps that clock: `Some` is the start of the failure the member is in now, not of any failure
/// it ever had.
fn remember_failure(data_dir: &Path, failing: bool) -> Option<u64> {
    let mut clocks = failures().lock().ok()?;
    if !failing {
        clocks.remove(data_dir);
        return None;
    }
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    Some(*clocks.entry(data_dir.to_path_buf()).or_insert(now))
}

/// The `observability` block of the reduced `/api/status` answer (issue #860), always present on
/// this version — so an owner can tell a member with export off (`enabled: false`) from an older
/// member that reports no block at all. A state, an age and one hash; never an endpoint, a header
/// or an error string.
pub(crate) async fn health_summary(app: &Shared) -> Value {
    let data_dir = app.cfg.data_dir.clone();
    // The no-identity resolve: this answer is polled by strangers, who must not force the hostname
    // probe or the host-id read `supervisor::resolve`'s identity runs.
    let (enabled, state, hash) = match supervisor::resolve_for_fleet(app).await {
        // Off is a choice, not a fault: nothing to age, no destination to name.
        Resolved::Off(_) => (false, "off", None),
        // Switched on but unusable: a fault to age, and no effective settings to hash.
        Resolved::Invalid(_) => (true, "failing", None),
        Resolved::On(e) => {
            // The effective destination as the status page shows it: a Loki module's URL completed
            // with its push path and the protocol named "loki", else the OTLP endpoint as saved.
            let loki = e.str("provider") == super::settings::LOKI;
            let endpoint = if loki {
                supervisor::loki_push_url(e.str("url"))
            } else {
                e.str("endpoint").to_owned()
            };
            let protocol = if loki { "loki" } else { e.str("protocol") };
            let addon = std::fs::read_to_string(supervisor::status_path(&data_dir))
                .ok()
                .and_then(|b| serde_json::from_str::<Value>(&b).ok())
                .and_then(|status| status["state"].as_str().map(str::to_string));
            (
                true,
                merged_state(supervisor::runtime_state(&data_dir), addon.as_deref()),
                Some(destination_hash(protocol, &endpoint)),
            )
        }
    };
    json!({
        "enabled": enabled,
        "state": state,
        "failing_since": remember_failure(&data_dir, state == "failing"),
        "destination_hash": hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{temp_root, test_app};
    use crate::util::short_id;

    fn otlp_module(endpoint: &str) -> crate::config::ModuleChoice {
        crate::config::ModuleChoice {
            provider: "otlp".into(),
            enabled: true,
            settings: serde_json::from_value(json!({"endpoint": endpoint})).unwrap(),
        }
    }

    #[test]
    fn the_state_vocabulary_is_four_words_over_the_known_states() {
        assert_eq!(merged_state("running", Some("running")), "ok");
        assert_eq!(merged_state("running", Some("retrying")), "failing");
        assert_eq!(merged_state("running", Some("auth_failed")), "failing");
        assert_eq!(merged_state("running", Some("starting")), "starting");
        assert_eq!(merged_state("running", None), "starting", "no status written yet");
        // The supervisor's own failure words: the process is down, whatever the last status said.
        for runtime in ["failed", "refused", "invalid", "no_addon", "restarting"] {
            assert_eq!(merged_state(runtime, Some("running")), "failing", "{runtime}");
        }
        // Not yet known: a supervisor that has not run, or has not caught up with a config just
        // switched on.
        for runtime in ["", "off", "starting"] {
            assert_eq!(merged_state(runtime, Some("retrying")), "starting", "{runtime:?}");
        }
    }

    #[test]
    fn the_destination_hash_tracks_the_endpoint_and_the_protocol() {
        let a = destination_hash("http/protobuf", "https://otlp.example.com");
        assert_eq!(a.len(), 24, "12 bytes, as hex");
        assert!(a.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)), "{a}");
        assert_eq!(a, destination_hash("http/protobuf", "https://otlp.example.com"));
        assert_ne!(
            a,
            destination_hash("http/json", "https://otlp.example.com"),
            "the protocol is hashed"
        );
        assert_ne!(
            a,
            destination_hash("http/protobuf", "https://other.example.com"),
            "the endpoint is hashed"
        );
        // The separator keeps a protocol and an endpoint from spilling into each other.
        assert_ne!(destination_hash("a", "b"), destination_hash("ab", ""));
    }

    #[test]
    fn failing_for_s_reads_only_a_present_failure() {
        let failing = PeerObservability {
            enabled: true,
            state: "failing".into(),
            failing_since: Some(1000),
            destination_hash: None,
        };
        assert_eq!(failing.failing_for_s(1600), Some(600));
        assert_eq!(
            failing.failing_for_s(500),
            Some(0),
            "a clock behind the start ages zero, not negative"
        );
        assert_eq!(
            PeerObservability {
                enabled: false,
                ..failing.clone()
            }
            .failing_for_s(999_999),
            None,
            "off"
        );
        assert_eq!(
            PeerObservability {
                state: "ok".into(),
                ..failing.clone()
            }
            .failing_for_s(999_999),
            None,
            "fine"
        );
        assert_eq!(
            PeerObservability {
                failing_since: None,
                ..failing
            }
            .failing_for_s(999_999),
            None,
            "no start named: nothing to age"
        );
    }

    #[tokio::test]
    async fn export_off_answers_enabled_false_and_ages_nothing() {
        let root = temp_root();
        let app = test_app(&root);
        let block = health_summary(&app).await;
        assert_eq!(
            block,
            json!({"enabled": false, "state": "off", "failing_since": null, "destination_hash": null}),
            "{block}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// #860's rule: nothing that names the backend crosses machines. An endpoint with a canary
    /// host and path, and a header secret whose name and value are canaries — all built at
    /// runtime, so no literal ever sits in the repository — and none of them reaches the block, or
    /// the reduced status answer that carries it.
    #[tokio::test]
    async fn the_block_names_no_endpoint_and_no_header() {
        let root = temp_root();
        let app = test_app(&root);
        let host = format!("canary-host-{}", short_id());
        let path = format!("canary-path-{}", short_id());
        let endpoint = format!("https://{host}.example.com/{path}");
        let header = format!("x-canary-{}", short_id());
        let secret = format!("canary-secret-{}", short_id());
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        crate::util::write_private(
            &app.cfg.config_dir.join("observability-headers"),
            format!("{header}={secret}").as_bytes(),
        )
        .unwrap();
        app.modules.write().await.observability = Some(otlp_module(&endpoint));

        let block = health_summary(&app).await;
        assert_eq!(block["enabled"], json!(true), "{block}");
        let shown = block.to_string();
        for canary in [&endpoint, &host, &path, &header, &secret] {
            assert!(!shown.contains(canary), "the block names the backend: {shown}");
        }
        let hash = block["destination_hash"].as_str().expect("a destination hash while on");
        assert_eq!(hash.len(), 24, "{block}");
        assert!(
            hash.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{block}"
        );

        // The whole reduced answer — what a peer actually polls — keeps the same rule.
        let body = crate::status::reduced_status(&app).await;
        let shown = body.to_string();
        for canary in [&endpoint, &host, &path, &header, &secret] {
            assert!(!shown.contains(canary), "the reduced status names the backend: {shown}");
        }
        assert_eq!(body["observability"], block, "the block rides the answer as-is");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A config that cannot work is a failure the owner can see, and its age is the start of the
    /// failure the member is in now: it survives repeated answers, clears when the config is
    /// fixed, and starts over when a new failure comes.
    #[tokio::test]
    async fn an_unusable_config_fails_and_the_clock_restarts_with_each_failure() {
        let root = temp_root();
        let app = test_app(&root);
        // The unusable config: an endpoint that carries credentials, which the endpoint rules
        // refuse. Built at runtime from fragments, so no credential-shaped URI is ever literal in
        // the source — and neither half of the userinfo reaches the answer.
        let (user, pass) = (format!("user-{}", short_id()), format!("pass-{}", short_id()));
        let credentialed = format!("https://{user}:{pass}@otlp.example.com");
        app.modules.write().await.observability = Some(otlp_module(&credentialed));
        let failing = health_summary(&app).await;
        assert_eq!(
            failing["enabled"],
            json!(true),
            "the switch is on, the config is broken: {failing}"
        );
        assert_eq!(failing["state"], json!("failing"), "{failing}");
        for canary in [&user, &pass] {
            assert!(!failing.to_string().contains(canary), "the answer names the credential: {failing}");
        }
        assert!(failing["failing_since"].is_u64(), "{failing}");
        assert!(
            failing["destination_hash"].is_null(),
            "no effective settings to hash: {failing}"
        );
        let since = failing["failing_since"].as_u64().unwrap();

        let again = health_summary(&app).await;
        assert_eq!(again["failing_since"], json!(since), "the same failure keeps its start");

        app.modules.write().await.observability = Some(otlp_module("https://otlp.example.com"));
        let starting = health_summary(&app).await;
        assert_eq!(
            starting["state"],
            json!("starting"),
            "the supervisor has not ticked yet: {starting}"
        );
        assert!(
            starting["failing_since"].is_null(),
            "not failing: the clock is cleared: {starting}"
        );
        assert!(starting["destination_hash"].is_string(), "{starting}");

        app.modules.write().await.observability = Some(otlp_module(&credentialed));
        let anew = health_summary(&app).await;
        assert!(
            anew["failing_since"].as_u64().is_some_and(|at| at >= since),
            "a new failure starts its own clock"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
