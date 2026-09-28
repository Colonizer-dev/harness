//! The pre-upload manifest for a local→hosted move (docs/hosted.md): what `GET
//! /api/upload/manifest` says WOULD be copied to a hosted Colonizer, and what stays home. No
//! upload happens here — the hosted service does not exist yet — but the contract is fixed now:
//! the copy set is an *allowlist*, only ever the three machine-written settings files (whose
//! schemas cannot carry a credential; `a_serialised_provider_carries_no_key` pins providers.json)
//! and the skill packs by name and version. Everything else in the config dir stays home with a
//! reason — credentials, telemetry and usage state, per-host state, the data dir as a whole — and
//! unknown files default to stays-home, never copied. The digest pins the copied set, so a future
//! confirm step can echo back exactly what the operator saw.
//!
//! `colonizer.toml` stays home on purpose: it parses today as publish attribution only, but it is
//! the one hand-edited, free-text file in the config dir, so the allowlist cannot vouch for its
//! bytes. `known-orgs.json` stays home too: it is this host's cache of discovered orgs, not a
//! setting.

use crate::{Settings, Shared, plugins};
use axum::{Json, extract::State};
use serde::Serialize;
use std::collections::BTreeSet;

/// One line of the manifest: a config-dir-relative name (`"providers.json"`,
/// `"provider-keys/deepseek"`), a pack (`"plugins/ecc"`), or the data dir as a whole (`"data/"`).
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Entry {
    name: String,
    /// `"config"` or `"plugin"` on the copied side; `"file"` or `"dir"` on the stays-home side.
    kind: &'static str,
    /// Copied: what it is (a pack's reason carries its version). Stays home: `credential` |
    /// `telemetry` | `usage` | `host` | `data` | `hand-edited` | `unknown` (anything not on the
    /// allowlist).
    reason: String,
}

/// What would be copied, what stays home, and the digest over the copied set.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Manifest {
    copied: Vec<Entry>,
    stays_home: Vec<Entry>,
    digest: String,
}

/// The allowlist: the only config-dir files ever listed as copied, each machine-written JSON with
/// a schema that cannot carry a credential.
const COPIED_FILES: &[(&str, &str)] = &[
    ("modules.json", "module selections"),
    ("providers.json", "provider list, without credentials"),
    ("orgs.json", "per-org overrides"),
];

/// Why a stays-home entry stays home. Credentials are matched by name — the known secret files
/// and dirs, plus any `.enc` sibling — so a renamed or future secret still lands here.
fn stays_home_reason(name: &str) -> &'static str {
    const CREDENTIALS: &[&str] = &[
        "provider-keys",
        "github-token",
        "claude-token",
        "claude-accounts",
        "claude-accounts.json",
        "api-token",
        "api-tokens.json",
        "notify-secret",
        "memory-keys",
        "voice-keys",
        "push-vapid-key",
        "colony-secrets.json",
        "secrets.json",
    ];
    if CREDENTIALS.contains(&name) || name.ends_with(".enc") {
        "credential"
    } else if name == "telemetry.json" {
        "telemetry"
    } else if name == "usage.json" || name == "usage-last.json" || name == "usage-sent.json" {
        "usage"
    } else if matches!(
        name,
        "host_id" | "known-orgs.json" | "updates.json" | "push-subscriptions.json" | "remote"
    ) {
        "host"
    } else if name == "colonizer.toml" {
        "hand-edited"
    } else {
        "unknown"
    }
}

/// sha256 over the copied entries, `name\tkind\treason\n` each: a pack added, a file appearing or
/// a reason changing all move it, so an echoed digest means the same copy set.
fn digest(copied: &[Entry]) -> String {
    let mut text = String::new();
    for entry in copied {
        text.push_str(&entry.name);
        text.push('\t');
        text.push_str(entry.kind);
        text.push('\t');
        text.push_str(&entry.reason);
        text.push('\n');
    }
    crate::util::hex(ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref())
}

/// The manifest for `cfg`'s install: pure over the config and data dirs, so tests can pin it
/// against a fixture directory.
pub(crate) fn build(cfg: &Settings) -> Manifest {
    let mut copied = Vec::new();
    let mut allowlisted = BTreeSet::new();
    for (name, reason) in COPIED_FILES {
        if cfg.config_dir.join(name).is_file() {
            copied.push(Entry {
                name: (*name).into(),
                kind: "config",
                reason: (*reason).into(),
            });
            allowlisted.insert(*name);
        }
    }
    // Skill packs by name and version, never their contents; the hosted side re-fetches them.
    for (name, dir) in plugins::local_packs(cfg) {
        let reason = match plugins::manifest_version(&dir) {
            Some(version) => format!("skill pack {version}"),
            None => "skill pack, version unknown".into(),
        };
        copied.push(Entry {
            name: format!("plugins/{name}"),
            kind: "plugin",
            reason,
        });
    }

    let mut stays_home: Vec<Entry> = std::fs::read_dir(&cfg.config_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if allowlisted.contains(name.as_str()) {
                return None;
            }
            let kind = if entry.path().is_dir() { "dir" } else { "file" };
            Some(Entry {
                reason: stays_home_reason(&name).into(),
                kind,
                name,
            })
        })
        .collect();
    // The data directory as a whole: colonies, sessions, worktrees, clones, mesh state, caches and
    // generated files. The packs listed above are the one exception, by name and version only.
    stays_home.push(Entry {
        name: "data/".into(),
        kind: "dir",
        reason: "data".into(),
    });
    stays_home.sort_by(|a, b| a.name.cmp(&b.name));
    let digest = digest(&copied);
    Manifest {
        copied,
        stays_home,
        digest,
    }
}

/// `GET /api/upload/manifest`: what a move to hosted Colonizer would copy, and what stays home.
pub async fn manifest(State(app): State<Shared>) -> Json<Manifest> {
    Json(build(&app.cfg))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/upload/manifest", routing::get(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// One fake credential value reused for every secret in the fixture: if any copied entry ever
    /// carried it, the serialized manifest would name it.
    const SECRET: &str = "sk-test-SECRET-do-not-upload";

    /// A config dir holding every kind of file the harness keeps there, plus two skill packs
    /// (root and legacy manifests). Returns nothing; build() re-reads the tree.
    fn fixture(root: &Path) {
        let config = root.join("config");
        let data = root.join("data");
        std::fs::create_dir_all(config.join("provider-keys")).unwrap();
        std::fs::create_dir_all(data.join("plugins")).unwrap();
        // Allowlisted settings files (machine-written, no credentials).
        std::fs::write(
            config.join("modules.json"),
            serde_json::to_string(&crate::config::ModulesConfig::default()).unwrap(),
        )
        .unwrap();
        std::fs::write(config.join("providers.json"), "[]").unwrap();
        std::fs::write(config.join("orgs.json"), "{}").unwrap();
        // Credentials.
        for (name, body) in [
            ("provider-keys/deepseek", SECRET),
            ("github-token", SECRET),
            ("github-token.enc", "v1:c2Vjb3JldA==:c2Vjb3JldA=="),
            ("claude-token", SECRET),
            ("notify-secret", SECRET),
            ("push-vapid-key", SECRET),
            ("secrets.json", SECRET),
            ("colony-secrets.json", SECRET),
            ("claude-accounts.json", "{}"),
            ("api-token", SECRET),
            // api-tokens.json must parse: the test App loads it at startup.
            ("api-tokens.json", "[]"),
        ] {
            std::fs::write(config.join(name), body).unwrap();
        }
        for dir in ["memory-keys", "voice-keys", "claude-accounts", "remote"] {
            std::fs::create_dir_all(config.join(dir)).unwrap();
            std::fs::write(config.join(dir).join("x"), SECRET).unwrap();
        }
        // Telemetry / usage / host state.
        for name in [
            "telemetry.json",
            "usage.json",
            "usage-last.json",
            "usage-sent.json",
            "host_id",
            "known-orgs.json",
            "push-subscriptions.json",
            // updates.json is loaded by the test App at startup; keep it parseable.
            "updates.json",
        ] {
            std::fs::write(config.join(name), "{}").unwrap();
        }
        // Hand-edited settings: stays home (free-text, so the allowlist cannot vouch for it).
        std::fs::write(config.join("colonizer.toml"), "[publish]\nco_author = false\n").unwrap();
        // An unknown file defaults to stays-home.
        std::fs::write(config.join("unlisted-thing.bin"), SECRET).unwrap();
        // Skill packs, one with the legacy manifest location ("" = the pack root).
        let pack = |name: &str, version: &str, manifest_dir: &str| {
            let dir = data.join("plugins").join(name);
            let target = if manifest_dir.is_empty() {
                dir.clone()
            } else {
                dir.join(manifest_dir)
            };
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(
                target.join("plugin.json"),
                format!(r#"{{"name": "{name}", "version": "{version}", "description": "d"}}"#),
            )
            .unwrap();
        };
        pack("ecc", "6.4.0", ".claude-plugin");
        pack("superpowers", "6.4.2", "");
    }

    fn app(root: &Path) -> Shared {
        crate::tests::test_app(root)
    }

    #[test]
    fn the_manifest_copies_only_the_allowlist_and_keeps_everything_else_home() {
        let root = std::env::temp_dir().join(format!("colonizer-upload-{}", crate::util::short_id()));
        fixture(&root);
        let cfg = app(&root).cfg.clone();
        let m = build(&cfg);

        // Copied: exactly the allowlist items present, packs last, sorted by name.
        let copied: Vec<&str> = m.copied.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            copied,
            [
                "modules.json",
                "providers.json",
                "orgs.json",
                "plugins/ecc",
                "plugins/superpowers"
            ]
        );
        assert_eq!(m.copied[3].kind, "plugin");
        assert_eq!(m.copied[3].reason, "skill pack 6.4.0", "packs appear with their version");
        assert_eq!(m.copied[4].reason, "skill pack 6.4.2");

        // Every credential, telemetry, usage and host item is on the stays-home side, with its
        // reason category; unknown files default home; the data dir is one entry.
        let by = |name: &str| {
            m.stays_home
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("no {name} in {:?}", m.stays_home))
        };
        for name in [
            "provider-keys",
            "github-token",
            "github-token.enc",
            "claude-token",
            "claude-accounts",
            "claude-accounts.json",
            "api-token",
            "api-tokens.json",
            "notify-secret",
            "memory-keys",
            "voice-keys",
            "push-vapid-key",
            "colony-secrets.json",
            "secrets.json",
        ] {
            assert_eq!(by(name).reason, "credential", "{name}");
        }
        assert_eq!(by("telemetry.json").reason, "telemetry");
        assert_eq!(by("usage.json").reason, "usage");
        assert_eq!(by("usage-last.json").reason, "usage");
        assert_eq!(by("usage-sent.json").reason, "usage");
        for name in [
            "host_id",
            "known-orgs.json",
            "updates.json",
            "push-subscriptions.json",
            "remote",
        ] {
            assert_eq!(by(name).reason, "host", "{name}");
        }
        assert_eq!(by("colonizer.toml").reason, "hand-edited");
        assert_eq!(by("unlisted-thing.bin").reason, "unknown");
        assert_eq!(by("data/").reason, "data");

        // The secret must not appear anywhere in the manifest, copied or not.
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains(SECRET), "{json}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_digest_moves_when_the_copy_set_moves() {
        let root = std::env::temp_dir().join(format!("colonizer-upload-digest-{}", crate::util::short_id()));
        fixture(&root);
        let cfg = app(&root).cfg.clone();
        let first = build(&cfg);
        assert_eq!(first.digest, build(&cfg).digest, "the same tree digests the same");
        std::fs::remove_file(root.join("config/orgs.json")).unwrap();
        let second = build(&cfg);
        assert_ne!(first.digest, second.digest, "a changed copy set must change the digest");
        assert_eq!(second.copied.len(), first.copied.len() - 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_route_answers_with_the_built_manifest() {
        let root = std::env::temp_dir().join(format!("colonizer-upload-route-{}", crate::util::short_id()));
        fixture(&root);
        let app = app(&root);
        let Json(m) = manifest(State(app.clone())).await;
        assert_eq!(m.copied[0].name, "modules.json");
        assert_eq!(m.digest.len(), 64, "sha256 hex");
        assert!(m.stays_home.iter().any(|e| e.reason == "credential"));
        let _ = std::fs::remove_dir_all(root);
    }
}
