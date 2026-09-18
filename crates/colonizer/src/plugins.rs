//! Plugin directories — "skillsets" in Settings — that a colony can load read-only (docs/protocol.md,
//! "Plugin directories"). A name resolves in two places, in order: what the operator put in
//! `<data>/plugins/<name>`, then what shipped with the app in `plugins/<name>`, staged there by
//! scripts/fetch-vendor.sh. Boot and the Settings list both resolve through this module, so what the
//! toggles show is what a colony will mount.

use crate::{Shared, config::Settings, util::is_plain_name};
use anyhow::{Result, bail};
use axum::{Json, extract::State};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The comma-separated `plugins` setting as names, in order, without blanks or repeats.
pub fn parse_list(value: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn local_root(cfg: &Settings) -> PathBuf {
    cfg.data_dir.join("plugins")
}

fn vendored_root(cfg: &Settings) -> Option<PathBuf> {
    cfg.assets.as_ref().map(|assets| assets.join("plugins"))
}

/// The directory boot mounts for `name`. A local copy overrides a vendored one of the same name, and
/// neither can be named by a path.
pub fn resolve(cfg: &Settings, name: &str) -> Result<PathBuf> {
    let root = local_root(cfg);
    if !is_plain_name(name) {
        bail!(
            "plugin directory {name:?} must be a plain name under {}",
            root.display()
        );
    }
    let local = root.join(name);
    if local.is_dir() {
        return Ok(local);
    }
    match vendored_root(cfg).map(|vendored| vendored.join(name)) {
        Some(vendored) if vendored.is_dir() => Ok(vendored),
        _ => bail!(
            "plugin directory {name:?} is not in {} or among the app's vendored plugins",
            root.display()
        ),
    }
}

/// Entries directly under `dir` that `keep` accepts; 0 when the directory is absent.
fn count(dir: &Path, keep: impl Fn(&Path) -> bool) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| keep(&entry.path()))
                .count()
        })
        .unwrap_or(0)
}

/// What a switch in Settings needs to say about one plugin directory. The counts are the context cost
/// of switching it on: Claude Code discovers `skills/<name>/SKILL.md`, `agents/*.md` and `commands/*.md`.
fn describe(name: &str, dir: &Path, source: &str, shadows_vendored: bool) -> Value {
    let manifest: Value = std::fs::read(dir.join(".claude-plugin/plugin.json"))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
    let markdown = |path: &Path| path.is_file() && path.extension().is_some_and(|ext| ext == "md");
    json!({
        "name": name,
        "description": manifest["description"].as_str(),
        "version": manifest["version"].as_str(),
        "source": source,
        "shadows_vendored": shadows_vendored,
        "skills": count(&dir.join("skills"), |path| path.join("SKILL.md").is_file()),
        "agents": count(&dir.join("agents"), markdown),
        "commands": count(&dir.join("commands"), markdown),
    })
}

/// Plain-named subdirectories of `root`, by name.
fn directories(root: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, _)| is_plain_name(name))
        .collect()
}

/// Every plugin directory a colony could load, one entry per name, sorted.
pub fn available(cfg: &Settings) -> Value {
    let root = local_root(cfg);
    let vendored = vendored_root(cfg)
        .map(|dir| directories(&dir))
        .unwrap_or_default();
    let mut plugins: BTreeMap<String, Value> = vendored
        .iter()
        .map(|(name, dir)| (name.clone(), describe(name, dir, "vendored", false)))
        .collect();
    for (name, dir) in directories(&root) {
        let shadows = vendored.contains_key(&name);
        plugins.insert(name.clone(), describe(&name, &dir, "local", shadows));
    }
    json!({"local_root": root, "plugins": plugins.into_values().collect::<Vec<_>>()})
}

pub async fn list(State(app): State<Shared>) -> Json<Value> {
    Json(available(&app.cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(dir: &Path, version: &str, skills: &[&str], agents: &[&str]) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/plugin.json"),
            json!({"name": "x", "version": version, "description": "d"}).to_string(),
        )
        .unwrap();
        for skill in skills {
            std::fs::create_dir_all(dir.join("skills").join(skill)).unwrap();
            std::fs::write(dir.join("skills").join(skill).join("SKILL.md"), "---\n").unwrap();
        }
        // A skill directory without SKILL.md is not a skill Claude Code would load.
        std::fs::create_dir_all(dir.join("skills/not-a-skill")).unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        for agent in agents {
            std::fs::write(dir.join("agents").join(format!("{agent}.md")), "").unwrap();
        }
        std::fs::write(dir.join("agents/README.txt"), "").unwrap();
    }

    fn settings(root: &Path, assets: Option<PathBuf>) -> Settings {
        Settings {
            bind: "127.0.0.1:0".into(),
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            runtime_dir: root.join("run"),
            assets,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".into(),
            allowed_hosts: vec![],
        }
    }

    /// A data directory and an app directory, as a real install lays them out. Returns the temp root to
    /// remove afterwards.
    fn install() -> (PathBuf, Settings) {
        let root = std::env::temp_dir().join(format!(
            "colonizer-plugins-test-{}",
            crate::util::short_id()
        ));
        let app = root.join("app");
        std::fs::create_dir_all(app.join("plugins")).unwrap();
        let cfg = settings(&root, Some(app));
        std::fs::create_dir_all(cfg.data_dir.join("plugins")).unwrap();
        (root, cfg)
    }

    #[test]
    fn plugin_lists_parse_in_order_without_blanks_or_repeats() {
        assert_eq!(
            parse_list(" ecc, ,superpowers,ecc,google-skills "),
            ["ecc", "superpowers", "google-skills"]
        );
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn a_local_copy_overrides_a_vendored_one_and_paths_are_refused() {
        let (root, cfg) = install();
        let vendored = cfg.assets.clone().unwrap().join("plugins/ecc");
        plugin(&vendored, "2.2.1", &["tdd"], &[]);
        assert_eq!(resolve(&cfg, "ecc").unwrap(), vendored);

        let local = cfg.data_dir.join("plugins/ecc");
        plugin(&local, "9.9.9", &[], &[]);
        assert_eq!(resolve(&cfg, "ecc").unwrap(), local);

        assert!(resolve(&cfg, "missing").is_err());
        for bad in ["../ecc", "a/b", ".hidden", ""] {
            assert!(resolve(&cfg, bad).is_err(), "{bad:?} must be refused");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_listing_merges_both_places_and_counts_what_claude_code_loads() {
        let (root, cfg) = install();
        let app_plugins = cfg.assets.clone().unwrap().join("plugins");
        plugin(
            &app_plugins.join("ecc"),
            "2.2.1",
            &["tdd", "debugging"],
            &["planner"],
        );
        plugin(
            &app_plugins.join("superpowers"),
            "6.3.0",
            &["brainstorming"],
            &[],
        );
        plugin(
            &cfg.data_dir.join("plugins/superpowers"),
            "6.4.0-local",
            &[],
            &[],
        );
        plugin(
            &cfg.data_dir.join("plugins/team-skills"),
            "1.0.0",
            &["house-style"],
            &[],
        );
        std::fs::write(cfg.data_dir.join("plugins/stray-file"), "").unwrap();

        let listing = available(&cfg);
        let plugins = listing["plugins"].as_array().unwrap();
        let summary: Vec<(String, String, bool, u64, u64)> = plugins
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap().into(),
                    p["source"].as_str().unwrap().into(),
                    p["shadows_vendored"].as_bool().unwrap(),
                    p["skills"].as_u64().unwrap(),
                    p["agents"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("ecc".into(), "vendored".into(), false, 2, 1),
                ("superpowers".into(), "local".into(), true, 0, 0),
                ("team-skills".into(), "local".into(), false, 1, 0),
            ]
        );
        assert_eq!(plugins[1]["version"], "6.4.0-local");
        assert_eq!(listing["local_root"], json!(cfg.data_dir.join("plugins")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_app_or_plugins_folder_lists_nothing() {
        let root = std::env::temp_dir().join(format!(
            "colonizer-plugins-test-{}",
            crate::util::short_id()
        ));
        assert_eq!(available(&settings(&root, None))["plugins"], json!([]));
        assert_eq!(
            available(&settings(&root, Some(root.join("no-such-app"))))["plugins"],
            json!([])
        );
    }
}
