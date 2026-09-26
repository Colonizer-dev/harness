//! Monorepo detection: whether a repository holds several packages, and where each one lives, so
//! the cockpit can show a monorepo's packages under its row and attribute colonies to them by the
//! paths they changed.
//!
//! One `gh api` call lists the default branch's whole tree; the workspace manifests at the root
//! (package.json, pnpm-workspace.yaml, lerna.json, Cargo.toml, go.work) are read raw, one call each,
//! only when the tree has them. Everything after that — the parsers, glob expansion over the tree,
//! the fallback for `apps/*` and `packages/*` — is pure and tested without `gh`.

use crate::{ApiResult, Shared, client_error};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

/// How long a detection is served before it is refreshed behind the answer.
const PACKAGES_FRESH: Duration = Duration::from_secs(10 * 60);
/// Files whose presence in a directory makes it a package.
const MANIFESTS: &[&str] = &["package.json", "Cargo.toml", "go.mod", "pyproject.toml"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detection {
    pub monorepo: bool,
    /// What declared the packages: `npm-workspaces`, `pnpm`, `yarn`, `bun`, `turbo`, `nx`,
    /// `cargo`, `go-work`, `lerna` or `dirs`; `null` for a single-package repository.
    pub tool: Option<&'static str>,
    pub packages: Vec<Package>,
}

/// The root files a detection may need to read, given the paths in the tree.
pub fn wanted_root_files(paths: &BTreeSet<String>) -> Vec<&'static str> {
    ["package.json", "pnpm-workspace.yaml", "lerna.json", "Cargo.toml", "go.work"]
        .into_iter()
        .filter(|f| paths.contains(*f))
        .collect()
}

/// `workspaces` from a package.json: an array of globs, or `{ "packages": [...] }` (yarn).
pub fn npm_workspaces(package_json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(package_json) else {
        return Vec::new();
    };
    let list = match &v["workspaces"] {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("packages").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    list.iter().filter_map(Value::as_str).map(str::to_string).collect()
}

/// `packages:` from a pnpm-workspace.yaml: the `- 'glob'` items under that key. A small line
/// reader, not a YAML parser — the file is a flat list in practice.
pub fn pnpm_workspace(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('-') {
            inside = trimmed.starts_with("packages:");
            continue;
        }
        if inside && let Some(item) = trimmed.strip_prefix('-') {
            let item = item.trim().trim_matches(|c| c == '\'' || c == '"');
            if !item.is_empty() {
                out.push(item.to_string());
            }
        }
    }
    out
}

/// `packages` from a lerna.json.
pub fn lerna_packages(json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v["packages"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// `[workspace] members` from a Cargo.toml.
pub fn cargo_members(toml_text: &str) -> Vec<String> {
    toml_text
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("workspace")?.get("members")?.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect()
}

/// The `use` directories of a go.work, in both the single-line and the block form.
pub fn go_work_uses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        if block {
            if line == ")" {
                block = false;
            } else if !line.is_empty() {
                out.push(line.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("use") {
            let rest = rest.trim();
            if rest == "(" {
                block = true;
            } else if !rest.is_empty() {
                out.push(rest.to_string());
            }
        }
    }
    out.into_iter()
        .map(|p| p.trim_start_matches("./").trim_end_matches('/').to_string())
        .filter(|p| !p.is_empty() && p != ".")
        .collect()
}

/// The directories in a tree listing (every parent of every path).
pub fn directories(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    for path in paths {
        let mut rest = path.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            if !dirs.insert(parent.to_string()) {
                break;
            }
            rest = parent;
        }
    }
    dirs
}

/// Whether a directory path matches a workspace glob: `*` is one path segment, `**` any number.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[&str], s: &[&str]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(&"**"), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(pp), Some(ss)) => segment(pp, ss) && go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn segment(p: &str, s: &str) -> bool {
        match p.split_once('*') {
            None => p == s,
            Some((pre, post)) => s.len() >= pre.len() + post.len() && s.starts_with(pre) && s.ends_with(post),
        }
    }
    let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
    let p: Vec<&str> = pattern.split('/').filter(|x| !x.is_empty()).collect();
    let s: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
    go(&p, &s)
}

/// The package directories a set of workspace globs names: matching directories that hold a
/// manifest. `!` globs exclude.
pub fn expand(globs: &[String], paths: &BTreeSet<String>) -> Vec<String> {
    let dirs = directories(paths);
    let has_manifest = |dir: &str| MANIFESTS.iter().any(|m| paths.contains(&format!("{dir}/{m}")));
    let (exclude, include): (Vec<&String>, Vec<&String>) = globs.iter().partition(|g| g.starts_with('!'));
    dirs.iter()
        .filter(|d| include.iter().any(|g| glob_matches(g, d)))
        .filter(|d| !exclude.iter().any(|g| glob_matches(g.trim_start_matches('!'), d)))
        .filter(|d| has_manifest(d))
        .cloned()
        .collect()
}

/// The whole detection from a tree listing and the root manifests that were read.
pub fn detect(paths: &BTreeSet<String>, read: impl Fn(&str) -> Option<String>) -> Detection {
    let mut found: Option<(&'static str, Vec<String>)> = None;
    if let Some(yaml) = read("pnpm-workspace.yaml") {
        found = Some(("pnpm", expand(&pnpm_workspace(&yaml), paths)));
    }
    if found.as_ref().is_none_or(|(_, p)| p.is_empty())
        && let Some(json) = read("package.json")
    {
        let globs = npm_workspaces(&json);
        if !globs.is_empty() {
            let tool = if paths.contains("bun.lockb") || paths.contains("bun.lock") {
                "bun"
            } else if paths.contains("yarn.lock") {
                "yarn"
            } else {
                "npm-workspaces"
            };
            found = Some((tool, expand(&globs, paths)));
        }
    }
    if found.as_ref().is_none_or(|(_, p)| p.is_empty())
        && let Some(json) = read("lerna.json")
    {
        found = Some(("lerna", expand(&lerna_packages(&json), paths)));
    }
    if found.as_ref().is_none_or(|(_, p)| p.is_empty())
        && let Some(text) = read("Cargo.toml")
    {
        let members = cargo_members(&text);
        if !members.is_empty() {
            found = Some(("cargo", expand(&members, paths)));
        }
    }
    if found.as_ref().is_none_or(|(_, p)| p.is_empty())
        && let Some(text) = read("go.work")
    {
        let uses: Vec<String> = go_work_uses(&text)
            .into_iter()
            .filter(|d| paths.contains(&format!("{d}/go.mod")))
            .collect();
        found = Some(("go-work", uses));
    }
    if found.as_ref().is_none_or(|(_, p)| p.is_empty()) {
        let dirs = expand(&["apps/*".into(), "packages/*".into()], paths);
        found = Some(("dirs", dirs));
    }
    let (mut tool, dirs) = found.unwrap_or(("dirs", Vec::new()));
    // A task runner on top names the repository better than the package manager under it.
    if paths.contains("turbo.json") {
        tool = "turbo";
    } else if paths.contains("nx.json") {
        tool = "nx";
    }
    let packages: Vec<Package> = dirs
        .into_iter()
        .map(|path| Package {
            name: path.rsplit('/').next().unwrap_or(&path).to_string(),
            path,
        })
        .collect();
    let monorepo = packages.len() >= 2;
    Detection {
        monorepo,
        tool: monorepo.then_some(tool),
        packages: if monorepo { packages } else { Vec::new() },
    }
}

fn valid_part(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && s != "." && s != ".."
}

async fn fetch(app: &Shared, repo: &str) -> anyhow::Result<Detection> {
    // Both calls are conditional (see `github::gh_get`): an unchanged tree or manifest is a 304.
    let tree = crate::github::gh_get_json(app, &format!("repos/{repo}/git/trees/HEAD?recursive=1")).await?;
    let paths: BTreeSet<String> = tree_blobs(&tree);
    let mut files = std::collections::HashMap::new();
    for name in wanted_root_files(&paths) {
        let raw = crate::github::gh_get(
            app,
            &format!("repos/{repo}/contents/{name}"),
            Some("application/vnd.github.raw"),
        )
        .await;
        if let Ok((_, text)) = raw {
            files.insert(name, text);
        }
    }
    Ok(detect(&paths, |name| files.get(name).cloned()))
}

/// The blob paths of a `git/trees?recursive=1` answer.
fn tree_blobs(tree: &Value) -> BTreeSet<String> {
    tree["tree"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["type"].as_str() == Some("blob"))
        .filter_map(|e| e["path"].as_str().map(str::to_string))
        .collect()
}

/// GET /api/repos/{owner}/{name}/packages: whether the repository is a monorepo, and its packages.
pub async fn list_packages(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    if !valid_part(&owner) || !valid_part(&name) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let repo = format!("{owner}/{name}");
    let key = format!("packages:{repo}");
    let value = crate::cached_answer(&app, key, PACKAGES_FRESH, move |app| {
        let repo = repo.clone();
        async move { Ok(json!(fetch(&app, &repo).await?)) }
    })
    .await?;
    Ok(Json(value))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/repos/{owner}/{name}/packages", routing::get(list_packages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn parsers_read_each_workspace_format() {
        assert_eq!(
            npm_workspaces(r#"{"workspaces":["apps/*","packages/*"]}"#),
            vec!["apps/*", "packages/*"]
        );
        assert_eq!(npm_workspaces(r#"{"workspaces":{"packages":["libs/*"]}}"#), vec!["libs/*"]);
        assert!(npm_workspaces(r#"{"name":"x"}"#).is_empty());
        assert_eq!(
            pnpm_workspace("packages:\n  - 'apps/*'\n  - \"packages/**\"\n  # c\ncatalog:\n  - x\n"),
            vec!["apps/*", "packages/**"]
        );
        assert_eq!(lerna_packages(r#"{"packages":["modules/*"]}"#), vec!["modules/*"]);
        assert_eq!(
            cargo_members("[workspace]\nmembers = [\n  \"crates/*\",\n  \"tools/cli\",\n]\n"),
            vec!["crates/*", "tools/cli"]
        );
        assert!(cargo_members("[package]\nname = \"x\"\n").is_empty());
        assert_eq!(
            go_work_uses("go 1.22\n\nuse (\n\t./api\n\t./web // ui\n)\nuse ./tools\n"),
            vec!["api", "web", "tools"]
        );
    }

    #[test]
    fn globs_match_one_segment_or_any_depth() {
        assert!(glob_matches("apps/*", "apps/pwa"));
        assert!(!glob_matches("apps/*", "apps/pwa/src"));
        assert!(glob_matches("packages/**", "packages/sdk/core"));
        assert!(glob_matches("./crates/*/", "crates/core"));
        assert!(glob_matches("packages/sdk-*", "packages/sdk-web"));
        assert!(!glob_matches("packages/sdk-*", "packages/ui"));
    }

    #[test]
    fn an_npm_monorepo_lists_its_manifest_directories_and_excludes_negations() {
        let paths = tree(&[
            "package.json",
            "turbo.json",
            "apps/pwa/package.json",
            "apps/pwa/src/main.ts",
            "apps/website/package.json",
            "apps/notes/README.md",
            "packages/sdk/package.json",
            "packages/legacy/package.json",
        ]);
        let d = detect(&paths, |f| {
            (f == "package.json").then(|| r#"{"workspaces":["apps/*","packages/*","!packages/legacy"]}"#.to_string())
        });
        assert!(d.monorepo);
        assert_eq!(d.tool, Some("turbo"), "turbo names the repository over npm workspaces");
        let found: Vec<&str> = d.packages.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            found,
            vec!["apps/pwa", "apps/website", "packages/sdk"],
            "no manifest, no package; ! excludes"
        );
    }

    #[test]
    fn cargo_go_and_the_dirs_fallback_are_detected_and_a_single_package_is_not_a_monorepo() {
        let cargo = tree(&["Cargo.toml", "crates/a/Cargo.toml", "crates/b/Cargo.toml"]);
        let d = detect(&cargo, |_| Some("[workspace]\nmembers = [\"crates/*\"]\n".into()));
        assert_eq!((d.monorepo, d.tool, d.packages.len()), (true, Some("cargo"), 2));

        let dirs = tree(&["apps/a/package.json", "packages/b/pyproject.toml"]);
        let d = detect(&dirs, |_| None);
        assert_eq!((d.monorepo, d.tool), (true, Some("dirs")));

        let single = tree(&["package.json", "src/index.ts"]);
        let d = detect(&single, |_| Some(r#"{"name":"x"}"#.into()));
        assert_eq!(
            d,
            Detection {
                monorepo: false,
                tool: None,
                packages: Vec::new()
            }
        );
    }
}
