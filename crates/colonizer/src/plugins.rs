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
    for name in value.split(',').map(str::trim).filter(|name| !name.is_empty()) {
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
        bail!("plugin directory {name:?} must be a plain name under {}", root.display());
    }
    let local = root.join(name);
    if local.is_dir() {
        validate(&local)?;
        return Ok(local);
    }
    match vendored_root(cfg).map(|vendored| vendored.join(name)) {
        Some(vendored) if vendored.is_dir() => {
            validate(&vendored)?;
            Ok(vendored)
        }
        _ => bail!(
            "plugin directory {name:?} is not in {} or among the app's vendored plugins",
            root.display()
        ),
    }
}

/// The plugin manifest: the root `plugin.json` when present, otherwise the
/// legacy `.claude-plugin/plugin.json` the currently-staged packs use.
fn manifest_file(dir: &Path) -> PathBuf {
    let root = dir.join("plugin.json");
    if root.is_file() {
        root
    } else {
        dir.join(".claude-plugin/plugin.json")
    }
}

/// A legacy directory-style manifest entry (`"./skills/"`, `"skills/"`, `"."`):
/// "every skill under that directory". True when `rel` — already stripped of a
/// leading `./` and slashes — resolves under `dir` to a directory holding a
/// `SKILL.md` at most two levels beneath it (`skills/<name>/SKILL.md`, or one
/// level for a single-skill directory).
fn is_skill_tree(dir: &Path, rel: &str) -> bool {
    if rel.contains("..") {
        return false;
    }
    let base = if rel == "." { dir.to_path_buf() } else { dir.join(rel) };
    if !base.is_dir() {
        return false;
    }
    let mut stack = vec![(base, 0u8)];
    while let Some((sub, depth)) = stack.pop() {
        if sub.join("SKILL.md").is_file() {
            return true;
        }
        if depth >= 2 {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&sub) {
            stack.extend(entries.flatten().filter(|e| e.path().is_dir()).map(|e| (e.path(), depth + 1)));
        }
    }
    false
}

/// Boot-time structural validation for a plugin directory, run from
/// [`resolve`]: the manifest must exist and parse (root or legacy path),
/// every `skills/*/` directory carrying a `SKILL.md` must have a safe plain
/// name, and every skill the manifest's `skills` array lists must exist on
/// disk. A pack that fails any of these blocks its colony's launch with the
/// named error rather than mounting a degraded colony. The deeper rule set —
/// notably `mcp.json` host-gating — lives in `scripts/validate-plugins.mjs`
/// and is deliberately not duplicated here.
pub fn validate(dir: &Path) -> Result<()> {
    let manifest_path = manifest_file(dir);
    let data = match std::fs::read(&manifest_path) {
        Ok(data) => data,
        Err(_) => bail!(
            "{}: missing plugin manifest (expected plugin.json or .claude-plugin/plugin.json)",
            dir.display()
        ),
    };
    let manifest: Value = match serde_json::from_slice(&data) {
        Ok(manifest) => manifest,
        Err(err) => bail!("{}: invalid plugin manifest: {err}", manifest_path.display()),
    };
    if !manifest.is_object() {
        bail!("{}: invalid plugin manifest: expected a JSON object", manifest_path.display());
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("skills")) {
        for entry in entries.flatten() {
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_plain_name(&name) {
                bail!("{}: skill directory {name:?} must be a plain name", dir.display());
            }
        }
    }
    if let Some(listed) = manifest.get("skills").and_then(Value::as_array) {
        for skill in listed.iter().filter_map(Value::as_str) {
            let rel = skill.trim().trim_start_matches("./").trim_matches('/');
            if rel.is_empty() || rel.contains("..") {
                bail!("{}: manifest lists invalid skill {skill:?}", manifest_path.display());
            }
            // Legacy directory-style entries (`"./skills/"`, `"skills/"`, `"."`) mean
            // "every skill under that directory" — the shape upstream ecc ships
            // (`skills: ["./skills/"]`) — so they pass when the entry resolves to a
            // directory with skills beneath it.
            if is_skill_tree(dir, rel) {
                continue;
            }
            let candidate = if rel.contains('/') {
                dir.join(rel)
            } else {
                dir.join("skills").join(rel)
            };
            if !candidate.join("SKILL.md").is_file() && !candidate.is_file() {
                bail!(
                    "{}: manifest lists skill {skill:?} but it is missing on disk",
                    manifest_path.display()
                );
            }
        }
    }
    Ok(())
}

/// Skill names across the enabled packs must be unique: the model addresses a
/// skill as `<pack>:<name>`, so two packs answering to the same name are
/// ambiguous by construction. A collision blocks boot with both packs named.
/// What counts is what Claude Code loads — `skills/<name>/SKILL.md` on disk —
/// not what each manifest lists.
pub fn check_skill_uniqueness(packs: &[(&str, PathBuf)]) -> Result<()> {
    let mut owner: BTreeMap<String, &str> = BTreeMap::new();
    for (pack, dir) in packs {
        let Ok(entries) = std::fs::read_dir(dir.join("skills")) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            let Ok(skill) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(first) = owner.insert(skill.clone(), *pack) {
                bail!(
                    "skill {skill:?} is in both {first:?} and {pack:?}: skill names must be unique across enabled packs"
                );
            }
        }
    }
    Ok(())
}

/// Entries directly under `dir` that `keep` accepts; 0 when the directory is absent.
fn count(dir: &Path, keep: impl Fn(&Path) -> bool) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().filter(|entry| keep(&entry.path())).count())
        .unwrap_or(0)
}

/// What a switch in Settings needs to say about one plugin directory. The counts are the context cost
/// of switching it on: Claude Code discovers `skills/<name>/SKILL.md`, `agents/*.md` and `commands/*.md`.
fn describe(name: &str, dir: &Path, source: &str, shadows_vendored: bool) -> Value {
    let manifest: Value = std::fs::read(manifest_file(dir))
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
    let vendored = vendored_root(cfg).map(|dir| directories(&dir)).unwrap_or_default();
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
            fleet_peers: vec![],
        }
    }

    /// A data directory and an app directory, as a real install lays them out. Returns the temp root to
    /// remove afterwards.
    fn install() -> (PathBuf, Settings) {
        let root = std::env::temp_dir().join(format!("colonizer-plugins-test-{}", crate::util::short_id()));
        let app = root.join("app");
        std::fs::create_dir_all(app.join("plugins")).unwrap();
        let cfg = settings(&root, Some(app));
        std::fs::create_dir_all(cfg.data_dir.join("plugins")).unwrap();
        (root, cfg)
    }

    fn write_manifest(dir: &Path, at_root: bool, manifest: Value) {
        let path = if at_root {
            dir.join("plugin.json")
        } else {
            dir.join(".claude-plugin/plugin.json")
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, manifest.to_string()).unwrap();
    }

    #[test]
    fn a_legacy_directory_style_skills_entry_means_every_skill_beneath_it() {
        let (root, cfg) = install();
        // The exact shape upstream ecc ships: directory entries, not skill names.
        let dir = cfg.data_dir.join("plugins/ecc-shape");
        plugin(&dir, "2.2.1", &["tdd"], &[]);
        write_manifest(
            &dir,
            false,
            json!({"name": "ecc", "version": "2.2.1", "description": "d",
                   "skills": ["./skills/"], "commands": ["./commands/"]}),
        );
        assert!(resolve(&cfg, "ecc-shape").is_ok(), "ecc must keep booting unchanged");
        // ...but a directory entry with no skills beneath it still fails.
        let dir = cfg.data_dir.join("plugins/empty-shape");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::remove_dir_all(dir.join("skills/not-a-skill")).unwrap();
        write_manifest(
            &dir,
            false,
            json!({"name": "x", "version": "1.0.0", "description": "d", "skills": ["./skills/"]}),
        );
        assert!(resolve(&cfg, "empty-shape").is_err(), "an empty skills tree is not a skill");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_skill_in_two_enabled_packs_fails_naming_both_packs() {
        let (root, cfg) = install();
        let a = cfg.data_dir.join("plugins/pack-a");
        let b = cfg.data_dir.join("plugins/pack-b");
        plugin(&a, "1.0.0", &["shared", "only-a"], &[]);
        plugin(&b, "1.0.0", &["shared", "only-b"], &[]);
        let err = check_skill_uniqueness(&[("pack-a", a), ("pack-b", b)]).unwrap_err().to_string();
        assert!(err.contains("pack-a") && err.contains("pack-b"), "names both packs: {err}");
        assert!(err.contains("shared"), "names the skill: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_manifest_listed_skill_missing_on_disk_fails_with_a_named_error() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/ghost-pack");
        plugin(&dir, "1.0.0", &["real"], &[]);
        write_manifest(
            &dir,
            false,
            json!({"name": "x", "version": "1.0.0", "description": "d", "skills": ["skills/real", "ghost"]}),
        );
        let err = resolve(&cfg, "ghost-pack").unwrap_err().to_string();
        assert!(err.contains("ghost"), "names the skill: {err}");
        assert!(err.contains("plugin.json"), "names the file: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unsafe_skill_directory_name_fails_validation() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/unsafe-pack");
        plugin(&dir, "1.0.0", &[], &[]);
        std::fs::create_dir_all(dir.join("skills/.hidden")).unwrap();
        std::fs::write(dir.join("skills/.hidden/SKILL.md"), "---\n").unwrap();
        let err = resolve(&cfg, "unsafe-pack").unwrap_err().to_string();
        assert!(err.contains(".hidden"), "names the directory: {err}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_root_manifest_is_preferred_over_the_legacy_one() {
        let (root, cfg) = install();
        let dir = cfg.data_dir.join("plugins/rooted");
        plugin(&dir, "1.0.0-legacy", &[], &[]);
        write_manifest(
            &dir,
            true,
            json!({"name": "x", "version": "2.0.0-root", "description": "d"}),
        );
        // The legacy manifest is broken, but the root one carries the pack.
        std::fs::write(dir.join(".claude-plugin/plugin.json"), "{broken").unwrap();
        assert!(resolve(&cfg, "rooted").is_ok());
        let plugins = available(&cfg)["plugins"].as_array().unwrap().clone();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["version"], "2.0.0-root");
        std::fs::remove_dir_all(root).unwrap();
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
        plugin(&app_plugins.join("ecc"), "2.2.1", &["tdd", "debugging"], &["planner"]);
        plugin(&app_plugins.join("superpowers"), "6.3.0", &["brainstorming"], &[]);
        plugin(&cfg.data_dir.join("plugins/superpowers"), "6.4.0-local", &[], &[]);
        plugin(&cfg.data_dir.join("plugins/team-skills"), "1.0.0", &["house-style"], &[]);
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
        let root = std::env::temp_dir().join(format!("colonizer-plugins-test-{}", crate::util::short_id()));
        assert_eq!(available(&settings(&root, None))["plugins"], json!([]));
        assert_eq!(
            available(&settings(&root, Some(root.join("no-such-app"))))["plugins"],
            json!([])
        );
    }
}
