//! Packages: what a workspace's repositories publish (npm, crates, PyPI, Go modules, Dart, and
//! GitHub Packages) and what they depend on, read from the manifests and lockfiles at each
//! repository's default branch in the Mothership's bare clone (see `code.rs`).
//!
//! Everything that reads a file is a pure parser with a test; the registries (npm, crates.io, PyPI,
//! the Go module proxy, pub.dev) are asked only for the names a repository defines or depends on
//! directly, and OSV.dev for known advisories. Scanning a workspace can clone repositories and call
//! registries, so it runs in the background: the endpoints answer `{"status":"scanning"}` until the
//! first scan lands and serve it for an hour after that, refreshing it behind the answer.

use crate::{ApiResult, Shared, client_error};
use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use futures_util::{StreamExt, stream};

/// Runs futures eight at a time and returns their outputs.
async fn bounded<F: std::future::Future>(futs: Vec<F>) -> Vec<F::Output> {
    stream::iter(futs).buffer_unordered(8).collect().await
}
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};

/// A scan is served this long before it is redone behind the answer.
const SCAN_FRESH: Duration = Duration::from_secs(60 * 60);
/// A registry or OSV answer is kept this long.
const REGISTRY_FRESH: Duration = Duration::from_secs(6 * 60 * 60);
/// Most repositories scanned for one workspace, most recently pushed first.
const MAX_REPOS: usize = 25;
/// Largest manifest or lockfile read; bigger ones are skipped and reported.
const MAX_FILE: u64 = 12 * 1024 * 1024;
/// Most bytes read from one repository's manifests and lockfiles together.
const MAX_REPO_BYTES: u64 = 48 * 1024 * 1024;
/// Most direct dependencies whose latest version is asked for, per workspace.
const MAX_LATEST: usize = 250;
/// Most advisories whose details (summary, severity, fix) are fetched, per workspace.
const MAX_ADVISORY_DETAILS: usize = 80;
/// Most users listed per dependency version.
const MAX_USERS: usize = 20;

// --- ecosystems ---------------------------------------------------------------------------------

/// The ecosystems this reads, by id: `npm`, `cargo`, `pypi`, `go`, `dart`, `swift`.
pub fn osv_ecosystem(eco: &str) -> Option<&'static str> {
    Some(match eco {
        "npm" => "npm",
        "cargo" => "crates.io",
        "pypi" => "PyPI",
        "go" => "Go",
        "dart" => "Pub",
        _ => return None,
    })
}

/// A PyPI name in its normalized form (PEP 503): lowercase, runs of `-_.` as one `-`.
pub fn pypi_name(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            if !dash {
                out.push('-');
            }
            dash = true;
        } else {
            out.push(c.to_ascii_lowercase());
            dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

// --- what a repository defines ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Defined {
    pub ecosystem: &'static str,
    pub name: String,
    pub version: Option<String>,
    /// Marked not for publishing (`"private": true`, `publish = false`, `publish_to: none`, …).
    pub private: bool,
    /// A registry the manifest names (`publishConfig.registry`), when not the default.
    pub registry: Option<String>,
    /// The licence the manifest declares (SPDX expression or name), if any.
    pub license: Option<String>,
}

/// The package a manifest defines, if it names one. `file` is the file name, not the path.
pub fn defined_by(file: &str, text: &str) -> Option<Defined> {
    match file {
        "package.json" => {
            let v: Value = serde_json::from_str(text).ok()?;
            let name = v["name"].as_str()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(Defined {
                ecosystem: "npm",
                name,
                version: v["version"].as_str().map(str::to_string),
                private: v["private"].as_bool() == Some(true),
                registry: v["publishConfig"]["registry"].as_str().map(str::to_string),
                license: v["license"].as_str().map(str::to_string),
            })
        }
        "Cargo.toml" => {
            let v: toml::Value = toml::from_str(text).ok()?;
            let pkg = v.get("package")?;
            let name = pkg.get("name")?.as_str()?.to_string();
            let publish = pkg.get("publish");
            let private = matches!(publish, Some(toml::Value::Boolean(false)))
                || publish.and_then(|p| p.as_array()).is_some_and(|a| a.is_empty());
            Some(Defined {
                ecosystem: "cargo",
                name,
                version: pkg.get("version").and_then(|v| v.as_str()).map(str::to_string),
                private,
                registry: None,
                license: pkg.get("license").and_then(|v| v.as_str()).map(str::to_string),
            })
        }
        "pyproject.toml" => {
            let v: toml::Value = toml::from_str(text).ok()?;
            let project = v.get("project").or_else(|| v.get("tool").and_then(|t| t.get("poetry")))?;
            let name = project.get("name")?.as_str()?.to_string();
            let private = project
                .get("classifiers")
                .and_then(|c| c.as_array())
                .is_some_and(|c| c.iter().any(|x| x.as_str().is_some_and(|s| s.starts_with("Private ::"))));
            Some(Defined {
                ecosystem: "pypi",
                name,
                version: project.get("version").and_then(|v| v.as_str()).map(str::to_string),
                private,
                registry: None,
                license: project.get("license").and_then(|l| {
                    l.as_str()
                        .map(str::to_string)
                        .or_else(|| l.get("text").and_then(|t| t.as_str()).map(str::to_string))
                }),
            })
        }
        "go.mod" => {
            let module = text
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix("module "))?
                .trim()
                .trim_matches('"')
                .to_string();
            (!module.is_empty()).then_some(Defined {
                ecosystem: "go",
                name: module,
                version: None,
                private: false,
                registry: None,
                license: None,
            })
        }
        "pubspec.yaml" => {
            let top = |key: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(&format!("{key}:")))
                    .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
                    .filter(|v| !v.is_empty())
            };
            let name = top("name")?;
            Some(Defined {
                ecosystem: "dart",
                name,
                version: top("version"),
                private: top("publish_to").as_deref() == Some("none"),
                registry: None,
                license: None,
            })
        }
        _ => None,
    }
}

/// The dependencies a manifest declares directly, as `(ecosystem, name, dev)`.
pub fn declared_by(file: &str, text: &str) -> Vec<(&'static str, String, bool)> {
    let mut out = Vec::new();
    match file {
        "package.json" => {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                for (key, dev) in [
                    ("dependencies", false),
                    ("optionalDependencies", false),
                    ("devDependencies", true),
                ] {
                    if let Some(map) = v[key].as_object() {
                        for (name, spec) in map {
                            // Workspace links are the repository's own packages, not dependencies.
                            if spec
                                .as_str()
                                .is_some_and(|s| s.starts_with("workspace:") || s.starts_with("file:") || s.starts_with("link:"))
                            {
                                continue;
                            }
                            out.push(("npm", name.clone(), dev));
                        }
                    }
                }
            }
        }
        "Cargo.toml" => {
            if let Ok(v) = toml::from_str::<toml::Value>(text) {
                let mut tables: Vec<(&toml::Value, bool)> = Vec::new();
                for (key, dev) in [
                    ("dependencies", false),
                    ("build-dependencies", false),
                    ("dev-dependencies", true),
                ] {
                    if let Some(t) = v.get(key) {
                        tables.push((t, dev));
                    }
                    if let Some(targets) = v.get("target").and_then(|t| t.as_table()) {
                        for target in targets.values() {
                            if let Some(t) = target.get(key) {
                                tables.push((t, dev));
                            }
                        }
                    }
                }
                if let Some(t) = v.get("workspace").and_then(|w| w.get("dependencies")) {
                    tables.push((t, false));
                }
                for (table, dev) in tables {
                    let Some(table) = table.as_table() else { continue };
                    for (key, spec) in table {
                        // A path dependency is a crate in this repository.
                        if spec.get("path").is_some() {
                            continue;
                        }
                        let name = spec.get("package").and_then(|p| p.as_str()).unwrap_or(key).to_string();
                        out.push(("cargo", name, dev));
                    }
                }
            }
        }
        "pyproject.toml" => {
            if let Ok(v) = toml::from_str::<toml::Value>(text) {
                let project = v.get("project");
                for spec in project
                    .and_then(|p| p.get("dependencies"))
                    .and_then(|d| d.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = spec.as_str().and_then(pep508_name) {
                        out.push(("pypi", name, false));
                    }
                }
                if let Some(groups) = project
                    .and_then(|p| p.get("optional-dependencies"))
                    .and_then(|d| d.as_table())
                {
                    for list in groups.values() {
                        for spec in list.as_array().into_iter().flatten() {
                            if let Some(name) = spec.as_str().and_then(pep508_name) {
                                out.push(("pypi", name, true));
                            }
                        }
                    }
                }
                let poetry = v.get("tool").and_then(|t| t.get("poetry"));
                if let Some(deps) = poetry.and_then(|p| p.get("dependencies")).and_then(|d| d.as_table()) {
                    for name in deps.keys().filter(|k| *k != "python") {
                        out.push(("pypi", pypi_name(name), false));
                    }
                }
                if let Some(groups) = poetry.and_then(|p| p.get("group")).and_then(|g| g.as_table()) {
                    for group in groups.values() {
                        if let Some(deps) = group.get("dependencies").and_then(|d| d.as_table()) {
                            for name in deps.keys() {
                                out.push(("pypi", pypi_name(name), true));
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// The distribution name at the start of a PEP 508 requirement (`requests[socks]>=2; …` →
/// `requests`), normalized.
pub fn pep508_name(spec: &str) -> Option<String> {
    let spec = spec.trim();
    let end = spec
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(spec.len());
    let name = &spec[..end];
    (!name.is_empty()).then(|| pypi_name(name))
}

// --- what a lockfile resolves -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Locked {
    pub ecosystem: &'static str,
    pub name: String,
    pub version: String,
    /// What the lockfile itself says, when it says it (go.mod `// indirect`, pubspec.lock
    /// `direct main`); `None` leaves it to the manifests.
    pub direct: Option<bool>,
    pub dev: bool,
    /// The package that pulls this one in, where the lockfile's layout says (npm nested installs).
    pub via: Option<String>,
}

fn locked(ecosystem: &'static str, name: &str, version: &str) -> Locked {
    Locked {
        ecosystem,
        name: name.to_string(),
        version: version.to_string(),
        direct: None,
        dev: false,
        via: None,
    }
}

/// `name@version` split at the last `@` that is not a scope's leading one.
fn split_at_version(spec: &str) -> Option<(&str, &str)> {
    let at = spec[1..].rfind('@')? + 1;
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

/// npm's package-lock.json (v1 nested `dependencies`, v2/v3 flat `packages`).
pub fn parse_package_lock(text: &str) -> Vec<Locked> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(packages) = v["packages"].as_object() {
        for (key, entry) in packages {
            let Some(at) = key.rfind("node_modules/") else { continue };
            if entry["link"].as_bool() == Some(true) {
                continue;
            }
            let name = &key[at + "node_modules/".len()..];
            if let Some(version) = entry["version"].as_str() {
                let mut l = locked("npm", name, version);
                l.dev = entry["dev"].as_bool() == Some(true);
                // `node_modules/a/node_modules/b`: b is installed for a.
                let parent = key[..at].trim_end_matches('/');
                l.via = parent
                    .rfind("node_modules/")
                    .map(|p| parent[p + "node_modules/".len()..].to_string());
                out.push(l);
            }
        }
        return out;
    }
    fn walk(deps: &serde_json::Map<String, Value>, parent: Option<&str>, out: &mut Vec<Locked>) {
        for (name, entry) in deps {
            if let Some(version) = entry["version"].as_str() {
                let mut l = locked("npm", name, version);
                l.dev = entry["dev"].as_bool() == Some(true);
                l.via = parent.map(str::to_string);
                out.push(l);
            }
            if let Some(nested) = entry["dependencies"].as_object() {
                walk(nested, Some(name), out);
            }
        }
    }
    if let Some(deps) = v["dependencies"].as_object() {
        walk(deps, None, &mut out);
    }
    out
}

/// JSON with comments and trailing commas (bun.lock) made strict.
pub fn strict_json(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            ',' => {
                let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Bun's text lockfile: `"packages": { key: ["name@version", …] }`.
pub fn parse_bun_lock(text: &str) -> Vec<Locked> {
    let Ok(v) = serde_json::from_str::<Value>(&strict_json(text)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in v["packages"].as_object().into_iter().flat_map(|m| m.values()) {
        let Some(spec) = entry.get(0).and_then(Value::as_str) else {
            continue;
        };
        let Some((name, version)) = split_at_version(spec) else {
            continue;
        };
        if version.contains(':') {
            // workspace:, github:, file:, link: — not a registry release.
            continue;
        }
        out.push(locked("npm", name, version));
    }
    out
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(['"', '\''])
}

/// pnpm-lock.yaml (v5 `/name/1.0.0`, v6 `/name@1.0.0`, v9 `name@1.0.0`), keys of `packages:`,
/// with the peer suffix `(…)` dropped.
pub fn parse_pnpm_lock(text: &str) -> Vec<Locked> {
    let mut out = Vec::new();
    let mut in_packages = false;
    for line in text.lines() {
        if indent(line) == 0 && !line.trim().is_empty() {
            in_packages = line.trim_end() == "packages:";
            continue;
        }
        if !in_packages || indent(line) != 2 || !line.trim_end().ends_with(':') {
            continue;
        }
        let key = unquote(line.trim().trim_end_matches(':'));
        let key = key.strip_prefix('/').unwrap_or(key);
        let key = key.split('(').next().unwrap_or(key);
        if let Some((name, version)) = split_at_version(key) {
            out.push(locked("npm", name, version));
        } else if let Some(slash) = key.rfind('/') {
            let (name, version) = (&key[..slash], &key[slash + 1..]);
            if !name.is_empty() && version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.push(locked("npm", name, version));
            }
        }
    }
    out
}

/// yarn.lock, classic (`version "1.2.3"`) and berry (`version: 1.2.3`).
pub fn parse_yarn_lock(text: &str) -> Vec<Locked> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if indent(line) == 0 {
            current = None;
            let first = line.trim_end_matches(':').split(", ").next().map(unquote).unwrap_or("");
            if first.starts_with("__metadata") {
                continue;
            }
            if let Some((name, spec)) = split_at_version(first) {
                let local = ["workspace:", "link:", "file:", "portal:", "patch:", "exec:"];
                if !local.iter().any(|p| spec.starts_with(p)) {
                    current = Some(name.to_string());
                }
            }
            continue;
        }
        if let Some(name) = &current {
            let t = line.trim();
            let version = t
                .strip_prefix("version ")
                .or_else(|| t.strip_prefix("version: "))
                .map(unquote);
            if let Some(version) = version {
                out.push(locked("npm", name, version));
                current = None;
            }
        }
    }
    out
}

/// Cargo.lock, Poetry's poetry.lock and uv's uv.lock: `[[package]]` tables with name and version.
/// Packages without a registry source (the workspace's own crates, path or editable installs) are
/// left out.
pub fn parse_toml_lock(ecosystem: &'static str, text: &str) -> Vec<Locked> {
    let Ok(v) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pkg in v.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
        let (Some(name), Some(version)) = (
            pkg.get("name").and_then(|n| n.as_str()),
            pkg.get("version").and_then(|n| n.as_str()),
        ) else {
            continue;
        };
        let source = pkg.get("source");
        let local = match ecosystem {
            "cargo" => source.is_none(),
            _ => source.and_then(|s| s.as_table()).is_some_and(|s| {
                ["editable", "virtual", "directory", "workspace", "path"]
                    .iter()
                    .any(|k| s.contains_key(*k))
                    || s.get("type")
                        .and_then(|t| t.as_str())
                        .is_some_and(|t| matches!(t, "directory" | "file"))
            }),
        };
        if local {
            continue;
        }
        let name = if ecosystem == "pypi" {
            pypi_name(name)
        } else {
            name.to_string()
        };
        out.push(locked(ecosystem, &name, version));
    }
    out
}

/// requirements.txt pins (`name==1.2.3`); anything not pinned is listed with version `*`.
/// A requirements line without its comment: `#` starts one only at the line's start or after
/// whitespace, so a URL's `#egg=name` fragment survives.
pub fn strip_comment(line: &str) -> &str {
    let t = line.trim();
    if t.starts_with('#') {
        return "";
    }
    match t.find(" #").or_else(|| t.find("\t#")) {
        Some(i) => t[..i].trim(),
        None => t,
    }
}

pub fn parse_requirements(text: &str) -> Vec<Locked> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = strip_comment(line);
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let spec = line.split(';').next().unwrap_or(line);
        let Some(name) = pep508_name(spec) else { continue };
        let version = spec.split_once("==").map(|(_, v)| v.trim()).unwrap_or("*");
        let mut l = locked("pypi", &name, version);
        l.direct = Some(true);
        out.push(l);
    }
    out
}

/// go.mod `require` lines; `// indirect` marks a transitive requirement.
pub fn parse_go_mod(text: &str) -> Vec<Locked> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        let entry = if in_block {
            if t == ")" {
                in_block = false;
                continue;
            }
            t
        } else if t == "require (" {
            in_block = true;
            continue;
        } else if let Some(rest) = t.strip_prefix("require ") {
            rest
        } else {
            continue;
        };
        let (spec, comment) = entry.split_once("//").map_or((entry, ""), |(a, b)| (a, b));
        let mut parts = spec.split_whitespace();
        let (Some(module), Some(version)) = (parts.next(), parts.next()) else {
            continue;
        };
        let mut l = locked("go", module, version);
        l.direct = Some(!comment.contains("indirect"));
        out.push(l);
    }
    out
}

/// SwiftPM's Package.resolved (v1 `object.pins`, v2/v3 `pins`).
pub fn parse_package_resolved(text: &str) -> Vec<Locked> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let pins = v["pins"].as_array().or_else(|| v["object"]["pins"].as_array());
    let mut out = Vec::new();
    for pin in pins.into_iter().flatten() {
        let name = pin["identity"].as_str().or_else(|| pin["package"].as_str());
        let version = pin["state"]["version"]
            .as_str()
            .map(str::to_string)
            .or_else(|| pin["state"]["revision"].as_str().map(|r| r.chars().take(7).collect()));
        if let (Some(name), Some(version)) = (name, version) {
            out.push(locked("swift", name, &version));
        }
    }
    out
}

/// Dart's pubspec.lock: `packages:` entries with `dependency:`, `source:` and `version:`.
pub fn parse_pubspec_lock(text: &str) -> Vec<Locked> {
    let mut out = Vec::new();
    let mut in_packages = false;
    let mut name: Option<String> = None;
    let (mut dependency, mut source, mut version) = (String::new(), String::new(), String::new());
    let mut flush = |name: &mut Option<String>, dependency: &mut String, source: &mut String, version: &mut String| {
        if let Some(n) = name.take()
            && source == "hosted"
            && !version.is_empty()
        {
            let mut l = locked("dart", &n, version);
            l.direct = Some(dependency.starts_with("direct"));
            l.dev = dependency.contains("dev");
            out.push(l);
        }
        dependency.clear();
        source.clear();
        version.clear();
    };
    for line in text.lines() {
        if indent(line) == 0 && !line.trim().is_empty() {
            flush(&mut name, &mut dependency, &mut source, &mut version);
            in_packages = line.trim_end() == "packages:";
            continue;
        }
        if !in_packages {
            continue;
        }
        let t = line.trim();
        if indent(line) == 2 && t.ends_with(':') {
            flush(&mut name, &mut dependency, &mut source, &mut version);
            name = Some(unquote(t.trim_end_matches(':')).to_string());
        } else if indent(line) == 4 {
            if let Some(v) = t.strip_prefix("dependency:") {
                dependency = unquote(v).to_string();
            } else if let Some(v) = t.strip_prefix("source:") {
                source = unquote(v).to_string();
            } else if let Some(v) = t.strip_prefix("version:") {
                version = unquote(v).to_string();
            }
        }
    }
    flush(&mut name, &mut dependency, &mut source, &mut version);
    out
}

/// The parser for a lockfile name, if this reads it.
pub fn parse_lockfile(file: &str, text: &str) -> Option<Vec<Locked>> {
    Some(match file {
        "package-lock.json" | "npm-shrinkwrap.json" => parse_package_lock(text),
        "bun.lock" => parse_bun_lock(text),
        "pnpm-lock.yaml" => parse_pnpm_lock(text),
        "yarn.lock" => parse_yarn_lock(text),
        "Cargo.lock" => parse_toml_lock("cargo", text),
        "poetry.lock" | "uv.lock" => parse_toml_lock("pypi", text),
        "requirements.txt" => parse_requirements(text),
        "go.mod" => parse_go_mod(text),
        "Package.resolved" => parse_package_resolved(text),
        "pubspec.lock" => parse_pubspec_lock(text),
        _ => return None,
    })
}

// --- supply-chain signals from the files themselves ----------------------------------------------

/// A dependency declared in a way that cannot be verified or pinned: a git or URL source, a
/// wildcard range, an unpinned requirement, a lockfile entry without an integrity hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecRisk {
    pub ecosystem: &'static str,
    pub name: String,
    /// `unpinned-source`, `wildcard-range`, `unpinned-version` or `missing-integrity`.
    pub kind: &'static str,
    pub detail: String,
}

fn spec_risk(ecosystem: &'static str, name: &str, kind: &'static str, detail: String) -> SpecRisk {
    SpecRisk {
        ecosystem,
        name: name.to_string(),
        kind,
        detail,
    }
}

/// Whether an npm version spec points somewhere other than the registry: git, a URL, a GitHub
/// `user/repo` shorthand or a tarball.
pub fn npm_spec_is_source(spec: &str) -> bool {
    let s = spec.trim();
    ["git+", "git:", "git@", "github:", "gitlab:", "bitbucket:", "http:", "https:"]
        .iter()
        .any(|p| s.starts_with(p))
        || (!s.starts_with('@')
            && !s.starts_with("npm:")
            && !s.contains(' ')
            && s.split('/').count() == 2
            && !s.starts_with("workspace:")
            && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
}

/// Risky declarations in a manifest or requirements file.
pub fn manifest_risks(file: &str, text: &str) -> Vec<SpecRisk> {
    let mut out = Vec::new();
    match file {
        "package.json" => {
            let Ok(v) = serde_json::from_str::<Value>(text) else {
                return out;
            };
            for key in ["dependencies", "optionalDependencies", "devDependencies"] {
                for (name, spec) in v[key].as_object().into_iter().flatten() {
                    let Some(spec) = spec.as_str() else { continue };
                    if spec.starts_with("workspace:") || spec.starts_with("file:") || spec.starts_with("link:") {
                        continue;
                    }
                    if npm_spec_is_source(spec) {
                        out.push(spec_risk(
                            "npm",
                            name,
                            "unpinned-source",
                            format!("installed from {spec}, not the registry"),
                        ));
                    } else if matches!(spec.trim(), "*" | "" | "latest" | "x" | "next") {
                        out.push(spec_risk("npm", name, "wildcard-range", format!("any version (\"{spec}\")")));
                    }
                }
            }
        }
        "Cargo.toml" => {
            let Ok(v) = toml::from_str::<toml::Value>(text) else {
                return out;
            };
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                for (name, spec) in v.get(key).and_then(|t| t.as_table()).into_iter().flatten() {
                    if let Some(git) = spec.get("git").and_then(|g| g.as_str()) {
                        let pinned = spec.get("rev").is_some() || spec.get("tag").is_some();
                        let detail = if pinned {
                            format!("from git {git}")
                        } else {
                            format!("from git {git}, following a branch rather than a pinned revision")
                        };
                        out.push(spec_risk("cargo", name, "unpinned-source", detail));
                    } else if spec.as_str() == Some("*") || spec.get("version").and_then(|x| x.as_str()) == Some("*") {
                        out.push(spec_risk("cargo", name, "wildcard-range", "any version (\"*\")".into()));
                    }
                }
            }
        }
        "requirements.txt" => {
            for line in text.lines() {
                let line = strip_comment(line);
                if line.is_empty() || line.starts_with("-r") || line.starts_with("-c") {
                    continue;
                }
                let body = line.trim_start_matches("-e").trim();
                if body.starts_with("git+") || body.contains(" @ ") || body.starts_with("http") {
                    let name = body
                        .split(" @ ")
                        .next()
                        .and_then(pep508_name)
                        .filter(|n| !n.starts_with("git") && !n.starts_with("http"))
                        .or_else(|| body.split("#egg=").nth(1).map(pypi_name))
                        .unwrap_or_else(|| body.chars().take(60).collect());
                    out.push(spec_risk(
                        "pypi",
                        &name,
                        "unpinned-source",
                        format!("installed from {}", body.chars().take(120).collect::<String>()),
                    ));
                } else if !body.starts_with('-')
                    && !body.contains("==")
                    && let Some(name) = pep508_name(body)
                {
                    out.push(spec_risk(
                        "pypi",
                        &name,
                        "unpinned-version",
                        format!("not pinned to one version (\"{body}\")"),
                    ));
                }
            }
        }
        "pyproject.toml" => {
            let Ok(v) = toml::from_str::<toml::Value>(text) else {
                return out;
            };
            for spec in v
                .get("project")
                .and_then(|p| p.get("dependencies"))
                .and_then(|d| d.as_array())
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
            {
                if (spec.contains(" @ ") || spec.contains("git+"))
                    && let Some(name) = pep508_name(spec)
                {
                    out.push(spec_risk(
                        "pypi",
                        &name,
                        "unpinned-source",
                        format!("installed from a URL ({spec})"),
                    ));
                }
            }
        }
        _ => {}
    }
    out
}

/// npm package-lock.json entries installed from the registry without an `integrity` hash, or
/// resolved from somewhere other than a registry.
pub fn package_lock_integrity(text: &str) -> Vec<SpecRisk> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, entry) in v["packages"].as_object().into_iter().flatten() {
        let Some(at) = key.rfind("node_modules/") else { continue };
        if entry["link"].as_bool() == Some(true) || entry["version"].as_str().is_none() {
            continue;
        }
        let name = &key[at + "node_modules/".len()..];
        let resolved = entry["resolved"].as_str().unwrap_or("");
        if resolved.starts_with("git")
            || resolved.starts_with("file:")
            || (resolved.starts_with("http") && !resolved.contains("registry"))
        {
            out.push(spec_risk("npm", name, "unpinned-source", format!("resolved from {resolved}")));
        } else if entry["integrity"].as_str().is_none() && entry["inBundle"].as_bool() != Some(true) {
            out.push(spec_risk(
                "npm",
                name,
                "missing-integrity",
                "no integrity hash in package-lock.json".into(),
            ));
        }
    }
    out
}

/// Levenshtein distance, stopping early past `max`.
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        if cur.iter().min().copied().unwrap_or(0) > max {
            return max + 1;
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Widely used names per ecosystem that typosquats imitate.
const POPULAR: &[(&str, &[&str])] = &[
    (
        "npm",
        &[
            "react",
            "react-dom",
            "lodash",
            "express",
            "axios",
            "typescript",
            "webpack",
            "vite",
            "next",
            "vue",
            "chalk",
            "commander",
            "debug",
            "moment",
            "dayjs",
            "uuid",
            "dotenv",
            "eslint",
            "prettier",
            "jest",
            "vitest",
            "zod",
            "yargs",
            "request",
            "colors",
            "cross-env",
            "nodemon",
            "mongoose",
            "mysql",
            "pg",
            "redis",
            "socket.io",
            "jsonwebtoken",
            "bcrypt",
            "body-parser",
            "cors",
            "helmet",
            "rimraf",
            "glob",
            "minimist",
            "semver",
            "tslib",
            "rxjs",
            "classnames",
            "prop-types",
            "styled-components",
            "tailwindcss",
            "postcss",
            "autoprefixer",
            "babel-core",
            "core-js",
            "electron",
            "puppeteer",
            "discord.js",
            "ethers",
            "web3",
        ],
    ),
    (
        "pypi",
        &[
            "requests",
            "numpy",
            "pandas",
            "django",
            "flask",
            "fastapi",
            "pydantic",
            "urllib3",
            "setuptools",
            "boto3",
            "botocore",
            "pyyaml",
            "cryptography",
            "pillow",
            "scipy",
            "matplotlib",
            "sqlalchemy",
            "pytest",
            "click",
            "jinja2",
            "beautifulsoup4",
            "selenium",
            "tensorflow",
            "torch",
            "openai",
            "anthropic",
            "langchain",
            "httpx",
            "uvicorn",
            "celery",
            "redis",
            "psycopg2",
            "colorama",
            "python-dateutil",
        ],
    ),
    (
        "cargo",
        &[
            "serde",
            "serde_json",
            "tokio",
            "rand",
            "clap",
            "reqwest",
            "anyhow",
            "thiserror",
            "log",
            "regex",
            "chrono",
            "futures",
            "hyper",
            "axum",
            "tracing",
            "syn",
            "quote",
            "proc-macro2",
            "libc",
            "bytes",
            "itertools",
            "once_cell",
            "lazy_static",
            "base64",
            "uuid",
            "sha2",
            "ring",
            "rustls",
            "openssl",
            "toml",
        ],
    ),
];

/// The popular package a name imitates, if it is one or two edits away from it (and is not it).
pub fn typosquat_of(ecosystem: &str, name: &str) -> Option<&'static str> {
    let list = POPULAR.iter().find(|(e, _)| *e == ecosystem)?.1;
    let bare = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    if bare.len() < 4 || list.contains(&bare.as_str()) || name.starts_with('@') {
        return None;
    }
    list.iter()
        .copied()
        .find(|p| p.len() >= 4 && edit_distance(&bare, p, 2) <= if p.len() >= 7 { 2 } else { 1 })
}

/// How a dependency's licence reads for a project that is not itself copyleft: `strong-copyleft`
/// (AGPL, SSPL), `copyleft` (GPL), `unknown` (none, UNLICENSED, unreadable), else `None`.
pub fn license_concern(license: Option<&str>) -> Option<&'static str> {
    let Some(l) = license.map(str::trim).filter(|l| !l.is_empty()) else {
        return Some("unknown");
    };
    let upper = l.to_ascii_uppercase();
    if upper.contains("AGPL") || upper.contains("SSPL") || upper.contains("SERVER SIDE PUBLIC") {
        return Some("strong-copyleft");
    }
    // GPL but not LGPL; "GPL-2.0 OR MIT" offers a permissive choice.
    let permissive_choice = upper.contains(" OR ") && ["MIT", "APACHE", "BSD", "ISC"].iter().any(|p| upper.contains(p));
    if (upper.contains("GPL") && !upper.contains("LGPL")) && !permissive_choice {
        return Some("copyleft");
    }
    if matches!(upper.as_str(), "UNLICENSED" | "UNKNOWN" | "SEE LICENSE IN LICENSE" | "NONE") {
        return Some("unknown");
    }
    None
}

const MANIFESTS: &[&str] = &["package.json", "Cargo.toml", "pyproject.toml", "go.mod", "pubspec.yaml"];
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "bun.lock",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.lock",
    "poetry.lock",
    "uv.lock",
    "requirements.txt",
    "go.mod",
    "Package.resolved",
    "pubspec.lock",
];

/// Whether a path is one this reads: a manifest or lockfile outside vendored and build directories
/// (node_modules, vendor, target, …) and test fixtures.
pub fn wanted(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    if !(MANIFESTS.contains(&file) || LOCKFILES.contains(&file)) {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    let skip = [
        "node_modules/",
        "vendor/",
        "target/",
        "dist/",
        "build/",
        ".venv/",
        "third_party/",
        "fixtures/",
        "testdata/",
        "__fixtures__/",
        "examples/",
    ];
    !skip.iter().any(|d| lower.starts_with(d) || lower.contains(&format!("/{d}")))
}

// --- scanning a repository ----------------------------------------------------------------------

/// One repository's manifests and lockfiles, parsed.
#[derive(Debug, Default)]
pub struct RepoScan {
    pub repo: String,
    pub sha: String,
    pub defined: Vec<(String, Defined)>,
    pub declared: Vec<(&'static str, String, bool)>,
    pub locked: Vec<(String, Locked)>,
    pub files: Vec<String>,
    pub skipped: Vec<String>,
    /// Risky declarations: `(path, risk)`.
    pub risks: Vec<(String, SpecRisk)>,
}

async fn scan_repo(app: &Shared, repo: &str) -> Result<RepoScan> {
    let bare = crate::code::ensure_bare(app, repo).await?;
    let (_, sha) = crate::code::resolve(app, &bare, None).await?;
    let entries = crate::code::ls_tree(app, &bare, &sha).await?;
    let mut scan = RepoScan {
        repo: repo.to_string(),
        sha: sha.clone(),
        ..Default::default()
    };
    let mut picked: Vec<(String, String)> = Vec::new();
    let mut total = 0u64;
    for (blob, size, path) in entries {
        if !wanted(&path) {
            continue;
        }
        if size > MAX_FILE || total + size > MAX_REPO_BYTES {
            scan.skipped.push(path);
            continue;
        }
        total += size;
        picked.push((blob, path));
    }
    let blobs: Vec<String> = picked.iter().map(|(b, _)| b.clone()).collect();
    let mut texts: Vec<Option<String>> = vec![None; picked.len()];
    crate::code::cat_batch(app, &bare, &blobs, |i, bytes| {
        texts[i] = Some(String::from_utf8_lossy(bytes).into_owned());
    })
    .await?;
    for ((_, path), text) in picked.into_iter().zip(texts) {
        let Some(text) = text else { continue };
        let file = path.rsplit('/').next().unwrap_or(&path).to_string();
        let dir = path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        if let Some(d) = defined_by(&file, &text) {
            scan.defined.push((dir.clone(), d));
        }
        scan.declared.extend(declared_by(&file, &text));
        scan.risks
            .extend(manifest_risks(&file, &text).into_iter().map(|r| (path.clone(), r)));
        if file == "package-lock.json" || file == "npm-shrinkwrap.json" {
            scan.risks
                .extend(package_lock_integrity(&text).into_iter().map(|r| (path.clone(), r)));
        }
        if let Some(locked) = parse_lockfile(&file, &text) {
            scan.files.push(path.clone());
            scan.locked.extend(locked.into_iter().map(|l| (path.clone(), l)));
        }
    }
    Ok(scan)
}

/// The workspace's repositories, most recently pushed first: not archived, capped at [`MAX_REPOS`].
async fn org_repos(app: &Shared, org: &str) -> Result<Vec<String>> {
    let Json(list) = crate::github::list_repos(State(app.clone()))
        .await
        .map_err(|e| anyhow::anyhow!("could not list repositories: {}", e.message()))?;
    let mut repos: Vec<(String, String)> = list
        .iter()
        .filter(|r| r["archived"].as_bool() != Some(true))
        .filter_map(|r| {
            let full = r["full_name"].as_str()?;
            let owner = full.split('/').next()?;
            owner
                .eq_ignore_ascii_case(org)
                .then(|| (r["pushed_at"].as_str().unwrap_or("").to_string(), full.to_string()))
        })
        .collect();
    repos.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(repos.into_iter().take(MAX_REPOS).map(|(_, r)| r).collect())
}

async fn scan_all(app: &Shared, repos: &[String]) -> Vec<Result<RepoScan, (String, String)>> {
    let mut out = Vec::new();
    for repo in repos {
        out.push(scan_repo(app, repo).await.map_err(|e| (repo.clone(), format!("{e:#}"))));
    }
    out
}

// --- registries ---------------------------------------------------------------------------------

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION"), " (+https://colonizer.dev)"))
        .build()?)
}

async fn get_json(client: &reqwest::Client, url: &str, accept: Option<&str>) -> Result<Option<Value>> {
    let mut req = client.get(url);
    if let Some(a) = accept {
        req = req.header("accept", a);
    }
    let res = req.send().await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        anyhow::bail!("{url} answered {}", res.status());
    }
    Ok(Some(res.json().await?))
}

/// The Go module proxy's path escaping: each capital letter becomes `!` and its lowercase.
pub fn go_escape(module: &str) -> String {
    module
        .chars()
        .flat_map(|c| {
            if c.is_ascii_uppercase() {
                vec!['!', c.to_ascii_lowercase()]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// What a registry says about a name: `{latest, published_at, downloads, downloads_period, url}`,
/// `null` when the registry does not know it. Cached per name for [`REGISTRY_FRESH`].
async fn registry_info(app: &Shared, eco: &'static str, name: String, with_downloads: bool) -> Value {
    let key = format!("registry:{eco}:{name}:{with_downloads}");
    crate::cached_answer(app, key, REGISTRY_FRESH, move |_app| {
        let name = name.clone();
        async move {
            let client = client()?;
            let enc = name.replace('/', "%2F");
            Ok(match eco {
                "npm" => {
                    let doc = get_json(
                        &client,
                        &format!("https://registry.npmjs.org/{enc}"),
                        Some("application/vnd.npm.install-v1+json"),
                    )
                    .await?;
                    match doc {
                        None => Value::Null,
                        Some(doc) => {
                            let downloads = if with_downloads {
                                get_json(
                                    &client,
                                    &format!("https://api.npmjs.org/downloads/point/last-week/{name}"),
                                    None,
                                )
                                .await
                                .ok()
                                .flatten()
                                .and_then(|d| d["downloads"].as_u64())
                            } else {
                                None
                            };
                            json!({
                                "latest": doc["dist-tags"]["latest"],
                                "published_at": doc["modified"],
                                "downloads": downloads,
                                "downloads_period": "last week",
                                "url": format!("https://www.npmjs.com/package/{name}"),
                            })
                        }
                    }
                }
                "cargo" => match get_json(&client, &format!("https://crates.io/api/v1/crates/{name}"), None).await? {
                    None => Value::Null,
                    Some(doc) => json!({
                        "latest": doc["crate"]["max_stable_version"].as_str().or(doc["crate"]["max_version"].as_str()),
                        "published_at": doc["crate"]["updated_at"],
                        "created_at": doc["crate"]["created_at"],
                        "downloads": doc["crate"]["recent_downloads"],
                        "downloads_period": "90 days",
                        "url": format!("https://crates.io/crates/{name}"),
                    }),
                },
                "pypi" => match get_json(&client, &format!("https://pypi.org/pypi/{name}/json"), None).await? {
                    None => Value::Null,
                    Some(doc) => {
                        let latest = doc["info"]["version"].as_str().unwrap_or_default().to_string();
                        json!({
                            "latest": latest,
                            "published_at": doc["releases"][&latest][0]["upload_time_iso_8601"],
                            "downloads": Value::Null,
                            "url": format!("https://pypi.org/project/{name}/"),
                        })
                    }
                },
                "go" => match get_json(
                    &client,
                    &format!("https://proxy.golang.org/{}/@latest", go_escape(&name)),
                    None,
                )
                .await
                {
                    Ok(Some(doc)) => json!({
                        "latest": doc["Version"],
                        "published_at": doc["Time"],
                        "downloads": Value::Null,
                        "url": format!("https://pkg.go.dev/{name}"),
                    }),
                    // The proxy answers 410/404 for modules it cannot fetch (private, missing).
                    _ => Value::Null,
                },
                "dart" => match get_json(&client, &format!("https://pub.dev/api/packages/{name}"), None).await? {
                    None => Value::Null,
                    Some(doc) => json!({
                        "latest": doc["latest"]["version"],
                        "published_at": doc["latest"]["published"],
                        "downloads": Value::Null,
                        "url": format!("https://pub.dev/packages/{name}"),
                    }),
                },
                _ => Value::Null,
            })
        }
    })
    .await
    .unwrap_or(Value::Null)
}

/// Dotted numeric comparison, good enough for "is this behind latest": `v`-prefix and build
/// metadata are ignored, a pre-release sorts before its release.
pub fn version_lt(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches(['v', '=', '^', '~']);
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = v.split_once('-').map_or((v, false), |(c, _)| (c, true));
        (
            core.split('.')
                .map(|p| {
                    p.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse()
                        .unwrap_or(0)
                })
                .collect(),
            pre,
        )
    }
    let (pa, prea) = parts(a);
    let (pb, preb) = parts(b);
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let (x, y) = (pa.get(i).copied().unwrap_or(0), pb.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    prea && !preb
}

/// What a registry says about one version: `{deprecated, yanked, install_scripts, license,
/// published_at}`; `null` when the registry does not know it. npm, crates.io and PyPI only.
async fn version_info(app: &Shared, eco: &'static str, name: String, version: String) -> Value {
    let key = format!("registry-version:{eco}:{name}@{version}");
    crate::cached_answer(app, key, REGISTRY_FRESH * 4, move |_app| {
        let (name, version) = (name.clone(), version.clone());
        async move {
            let client = client()?;
            Ok(match eco {
                "npm" => match get_json(
                    &client,
                    &format!("https://registry.npmjs.org/{}/{version}", name.replace('/', "%2F")),
                    None,
                )
                .await?
                {
                    None => Value::Null,
                    Some(doc) => npm_version_facts(&doc),
                },
                "cargo" => match get_json(&client, &format!("https://crates.io/api/v1/crates/{name}/{version}"), None).await? {
                    None => Value::Null,
                    Some(doc) => json!({
                        "deprecated": Value::Null,
                        "yanked": doc["version"]["yanked"].as_bool().unwrap_or(false),
                        "install_scripts": [],
                        "license": doc["version"]["license"],
                        "published_at": doc["version"]["created_at"],
                    }),
                },
                "pypi" => match get_json(&client, &format!("https://pypi.org/pypi/{name}/{version}/json"), None).await? {
                    None => Value::Null,
                    Some(doc) => {
                        let classifier = doc["info"]["classifiers"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c.as_str())
                            .find_map(|c| {
                                c.strip_prefix("License :: ")
                                    .map(|l| l.rsplit(" :: ").next().unwrap_or(l).to_string())
                            });
                        let license = doc["info"]["license_expression"]
                            .as_str()
                            .map(str::to_string)
                            .or(classifier)
                            .or_else(|| {
                                doc["info"]["license"]
                                    .as_str()
                                    .map(|l| l.lines().next().unwrap_or("").chars().take(60).collect())
                            });
                        json!({
                            "deprecated": Value::Null,
                            "yanked": doc["info"]["yanked"].as_bool().unwrap_or(false),
                            "install_scripts": [],
                            "license": license,
                            "published_at": doc["urls"][0]["upload_time_iso_8601"],
                        })
                    }
                },
                _ => Value::Null,
            })
        }
    })
    .await
    .unwrap_or(Value::Null)
}

/// The supply-chain facts in one npm version manifest: deprecation, install-time scripts, licence.
pub fn npm_version_facts(doc: &Value) -> Value {
    let scripts: Vec<String> = ["preinstall", "install", "postinstall"]
        .iter()
        .filter(|k| doc["scripts"][**k].as_str().is_some_and(|s| !s.trim().is_empty()))
        .map(|k| {
            format!(
                "{k}: {}",
                doc["scripts"][*k]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect::<String>()
            )
        })
        .collect();
    let license = doc["license"]
        .as_str()
        .map(str::to_string)
        .or_else(|| doc["license"]["type"].as_str().map(str::to_string));
    json!({
        "deprecated": doc["deprecated"].as_str(),
        "yanked": false,
        "install_scripts": scripts,
        "license": license,
        "published_at": Value::Null,
    })
}

// --- OSV ----------------------------------------------------------------------------------------

/// Advisory ids per `(ecosystem, name, version)`, from OSV.dev's batch API. Cached per query set.
async fn osv_ids(app: &Shared, queries: Vec<(&'static str, String, String)>) -> HashMap<(String, String, String), Vec<String>> {
    let mut out = HashMap::new();
    for chunk in queries.chunks(1000) {
        let body: Vec<Value> = chunk
            .iter()
            .filter_map(|(eco, name, version)| {
                let v = if *eco == "go" {
                    version.trim_start_matches('v')
                } else {
                    version.as_str()
                };
                Some(json!({"package": {"name": name, "ecosystem": osv_ecosystem(eco)?}, "version": v}))
            })
            .collect();
        let key = format!("osv-batch:{:x}", fnv(&serde_json::to_string(&body).unwrap_or_default()));
        let answer = crate::cached_answer(app, key, REGISTRY_FRESH, move |_app| {
            let body = body.clone();
            async move {
                let res = client()?
                    .post("https://api.osv.dev/v1/querybatch")
                    .timeout(Duration::from_secs(30))
                    .json(&json!({"queries": body}))
                    .send()
                    .await?;
                if !res.status().is_success() {
                    anyhow::bail!("OSV answered {}", res.status());
                }
                Ok(res.json::<Value>().await?)
            }
        })
        .await;
        let Ok(answer) = answer else { continue };
        for (q, result) in chunk.iter().zip(answer["results"].as_array().into_iter().flatten()) {
            let ids: Vec<String> = result["vulns"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect();
            if !ids.is_empty() {
                out.insert((q.0.to_string(), q.1.clone(), q.2.clone()), ids);
            }
        }
    }
    out
}

/// Summary, severity and first fixed version of one advisory; `null` when OSV cannot say.
async fn osv_detail(app: &Shared, id: String) -> Value {
    let key = format!("osv-vuln:{id}");
    crate::cached_answer(app, key, REGISTRY_FRESH * 4, move |_app| {
        let id = id.clone();
        async move {
            let Some(doc) = get_json(&client()?, &format!("https://api.osv.dev/v1/vulns/{id}"), None).await? else {
                return Ok(Value::Null);
            };
            Ok(advisory_summary(&doc))
        }
    })
    .await
    .unwrap_or(Value::Null)
}

/// The parts of an OSV record the cockpit shows.
pub fn advisory_summary(doc: &Value) -> Value {
    let severity = doc["database_specific"]["severity"]
        .as_str()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| {
            if doc["severity"].as_array().is_some_and(|a| !a.is_empty()) {
                "scored".into()
            } else {
                "unknown".into()
            }
        });
    let fixed = doc["affected"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|a| a["ranges"].as_array().into_iter().flatten())
        .flat_map(|r| r["events"].as_array().into_iter().flatten())
        .find_map(|e| e["fixed"].as_str().map(str::to_string));
    json!({
        "id": doc["id"],
        "summary": doc["summary"].as_str().or(doc["details"].as_str().map(|d| d.lines().next().unwrap_or(""))),
        "severity": severity,
        "fixed": fixed,
        "url": format!("https://osv.dev/vulnerability/{}", doc["id"].as_str().unwrap_or("")),
    })
}

fn fnv(s: &str) -> u64 {
    s.bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

// --- the views ------------------------------------------------------------------------------

async fn published(app: &Shared, org: &str, repos: &[String]) -> Result<Value> {
    let scans = scan_all(app, repos).await;
    let mut rows = Vec::new();
    let mut repo_rows = Vec::new();
    for scan in &scans {
        match scan {
            Ok(s) => {
                repo_rows.push(json!({"repo": s.repo, "sha": s.sha, "defined": s.defined.len()}));
                for (dir, d) in &s.defined {
                    rows.push((s.repo.clone(), dir.clone(), d.clone()));
                }
            }
            Err((repo, error)) => repo_rows.push(json!({"repo": repo, "error": error})),
        }
    }
    let lookups = bounded(
        rows.into_iter()
            .map(|(repo, dir, d)| {
                let app = app.clone();
                async move {
                    let info = if d.private || d.registry.as_deref().is_some_and(|r| !r.contains("npmjs.org")) {
                        Value::Null
                    } else {
                        registry_info(&app, d.ecosystem, d.name.clone(), true).await
                    };
                    let status = if d.private {
                        "private"
                    } else if info.is_null() {
                        "unpublished"
                    } else {
                        "published"
                    };
                    let behind = match (d.version.as_deref(), info["latest"].as_str()) {
                        (Some(local), Some(latest)) => version_lt(latest, local),
                        _ => false,
                    };
                    json!({
                        "ecosystem": d.ecosystem,
                        "name": d.name,
                        "version": d.version,
                        "repo": repo,
                        "path": dir,
                        "private": d.private,
                        "registry": d.registry,
                        "status": status,
                        "unreleased_changes": behind,
                        "published": info,
                    })
                }
            })
            .collect::<Vec<_>>(),
    )
    .await;
    let mut packages = lookups;
    packages.sort_by(|a, b| (a["ecosystem"].as_str(), a["name"].as_str()).cmp(&(b["ecosystem"].as_str(), b["name"].as_str())));
    Ok(json!({
        "org": org,
        "scanned_at": chrono::Utc::now(),
        "repos": repo_rows,
        "packages": packages,
        "github_packages": github_packages(app, org).await,
    }))
}

/// The organisation's GitHub Packages (npm, container, maven, rubygems, nuget), or an explanation
/// when the token cannot list them (it needs `read:packages`).
async fn github_packages(app: &Shared, org: &str) -> Value {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for kind in ["npm", "container", "maven", "rubygems", "nuget"] {
        let res =
            crate::util::exec(&mut app.gh(["api", &format!("/orgs/{org}/packages?package_type={kind}&per_page=100")])).await;
        match res {
            Ok(text) => {
                for p in serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default()
                {
                    out.push(json!({
                        "name": p["name"],
                        "type": p["package_type"],
                        "visibility": p["visibility"],
                        "versions": p["version_count"],
                        "updated_at": p["updated_at"],
                        "url": p["html_url"],
                        "repo": p["repository"]["full_name"],
                    }));
                }
            }
            Err(e) => errors.push(format!("{kind}: {e:#}")),
        }
    }
    let note = (!errors.is_empty() && out.is_empty())
        .then(|| "GitHub did not list the organisation's packages (the token may lack read:packages)".to_string());
    json!({"packages": out, "note": note})
}

async fn dependencies(app: &Shared, org: &str, repos: &[String]) -> Result<Value> {
    let scans = scan_all(app, repos).await;
    // (ecosystem, name) → aggregate.
    #[derive(Default)]
    struct Agg {
        direct: Option<bool>,
        dev: bool,
        versions: BTreeMap<String, Vec<(String, String)>>,
    }
    let mut agg: BTreeMap<(&'static str, String), Agg> = BTreeMap::new();
    let mut repo_rows = Vec::new();
    for scan in &scans {
        let s = match scan {
            Ok(s) => s,
            Err((repo, error)) => {
                repo_rows.push(json!({"repo": repo, "error": error}));
                continue;
            }
        };
        repo_rows.push(json!({"repo": s.repo, "sha": s.sha, "lockfiles": s.files, "skipped": s.skipped}));
        let declared: HashMap<(&'static str, String), bool> =
            s.declared.iter().map(|(e, n, dev)| ((*e, n.clone()), *dev)).collect();
        let own: BTreeSet<(&'static str, String)> = s.defined.iter().map(|(_, d)| (d.ecosystem, d.name.clone())).collect();
        for (path, l) in &s.locked {
            let key = (l.ecosystem, l.name.clone());
            if own.contains(&key) {
                continue;
            }
            let entry = agg.entry(key.clone()).or_default();
            let direct = l.direct.or_else(|| declared.get(&key).map(|_| true));
            entry.direct = match (entry.direct, direct) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), _) | (_, Some(false)) => Some(false),
                _ => None,
            };
            entry.dev = entry.dev || l.dev || declared.get(&key).copied().unwrap_or(false);
            let users = entry.versions.entry(l.version.clone()).or_default();
            if users.len() < MAX_USERS {
                let at = (s.repo.clone(), path.clone());
                if !users.contains(&at) {
                    users.push(at);
                }
            }
        }
    }
    // Latest version, for direct dependencies only (bounded).
    let direct: Vec<(&'static str, String)> = agg
        .iter()
        .filter(|(_, a)| a.direct == Some(true))
        .map(|(k, _)| k.clone())
        .filter(|(eco, _)| *eco != "swift")
        .take(MAX_LATEST)
        .collect();
    let latest: HashMap<(&'static str, String), String> = bounded(
        direct
            .into_iter()
            .map(|(eco, name)| {
                let app = app.clone();
                async move {
                    let info = registry_info(&app, eco, name.clone(), false).await;
                    info["latest"].as_str().map(|l| ((eco, name), l.to_string()))
                }
            })
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .flatten()
    .collect();
    // Advisories for every concrete version in use.
    let queries: Vec<(&'static str, String, String)> = agg
        .iter()
        .flat_map(|((eco, name), a)| a.versions.keys().map(move |v| (*eco, name.clone(), v.clone())))
        .filter(|(eco, _, v)| osv_ecosystem(eco).is_some() && v != "*")
        .collect();
    let ids = osv_ids(app, queries).await;
    let unique: BTreeSet<String> = ids.values().flatten().cloned().collect();
    let details: HashMap<String, Value> = bounded(
        unique
            .into_iter()
            .take(MAX_ADVISORY_DETAILS)
            .map(|id| {
                let app = app.clone();
                async move { (id.clone(), osv_detail(&app, id).await) }
            })
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .collect();

    let mut packages = Vec::new();
    let mut per_eco: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    let (mut outdated_n, mut vulnerable_n) = (0u64, 0u64);
    for ((eco, name), a) in &agg {
        let latest_v = latest.get(&(*eco, name.clone()));
        let mut outdated = false;
        let mut vulnerable = false;
        let versions: Vec<Value> = a
            .versions
            .iter()
            .map(|(version, users)| {
                let behind = latest_v.is_some_and(|l| version != "*" && version_lt(version, l));
                outdated |= behind;
                let vulns: Vec<Value> = ids
                    .get(&(eco.to_string(), name.clone(), version.clone()))
                    .into_iter()
                    .flatten()
                    .map(|id| {
                        details
                            .get(id)
                            .cloned()
                            .filter(|d| !d.is_null())
                            .unwrap_or_else(|| json!({"id": id, "severity": "unknown"}))
                    })
                    .collect();
                vulnerable |= !vulns.is_empty();
                json!({
                    "version": version,
                    "behind": behind,
                    "users": users.iter().map(|(repo, path)| json!({"repo": repo, "path": path})).collect::<Vec<_>>(),
                    "vulns": vulns,
                })
            })
            .collect();
        let slot = per_eco.entry(eco).or_default();
        if a.direct == Some(true) {
            slot.0 += 1;
        } else {
            slot.1 += 1;
        }
        outdated_n += outdated as u64;
        vulnerable_n += vulnerable as u64;
        packages.push(json!({
            "ecosystem": eco,
            "name": name,
            "direct": a.direct,
            "dev": a.dev,
            "latest": latest_v,
            "outdated": outdated,
            "vulnerable": vulnerable,
            "drift": a.versions.len() > 1,
            "versions": versions,
        }));
    }
    Ok(json!({
        "org": org,
        "scanned_at": chrono::Utc::now(),
        "repos": repo_rows,
        "ecosystems": per_eco.iter().map(|(e, (d, t))| json!({"ecosystem": e, "direct": d, "transitive": t})).collect::<Vec<_>>(),
        "totals": {
            "direct": per_eco.values().map(|x| x.0).sum::<u64>(),
            "transitive": per_eco.values().map(|x| x.1).sum::<u64>(),
            "outdated": outdated_n,
            "vulnerable": vulnerable_n,
        },
        "packages": packages,
    }))
}

/// Most (package, version) pairs whose registry facts are asked for, per workspace.
const MAX_VERSION_FACTS: usize = 300;

fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 4,
        "high" => 3,
        "moderate" | "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// Days since an RFC 3339 time, if it parses.
fn age_days(at: &Value) -> Option<i64> {
    let t = chrono::DateTime::parse_from_rfc3339(at.as_str()?).ok()?;
    Some((chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_days())
}

async fn supply_chain(app: &Shared, org: &str, repos: &[String]) -> Result<Value> {
    let scans = scan_all(app, repos).await;
    // (eco, name, version) → users, direct, via.
    #[derive(Default)]
    struct Use {
        direct: bool,
        via: BTreeSet<String>,
        users: Vec<(String, String)>,
    }
    let mut uses: BTreeMap<(&'static str, String, String), Use> = BTreeMap::new();
    let mut repo_rows = Vec::new();
    let mut spec_risks: Vec<(String, String, SpecRisk)> = Vec::new();
    let mut own_copyleft = false;
    for scan in &scans {
        let s = match scan {
            Ok(s) => s,
            Err((repo, error)) => {
                repo_rows.push(json!({"repo": repo, "error": error}));
                continue;
            }
        };
        repo_rows.push(json!({"repo": s.repo, "sha": s.sha, "lockfiles": s.files}));
        own_copyleft |= s
            .defined
            .iter()
            .any(|(_, d)| d.license.as_deref().is_some_and(|l| l.to_ascii_uppercase().contains("GPL")));
        let declared: BTreeSet<(&'static str, String)> = s.declared.iter().map(|(e, n, _)| (*e, n.clone())).collect();
        let own: BTreeSet<(&'static str, String)> = s.defined.iter().map(|(_, d)| (d.ecosystem, d.name.clone())).collect();
        for (path, l) in &s.locked {
            if own.contains(&(l.ecosystem, l.name.clone())) {
                continue;
            }
            let u = uses.entry((l.ecosystem, l.name.clone(), l.version.clone())).or_default();
            u.direct |= l.direct == Some(true) || declared.contains(&(l.ecosystem, l.name.clone()));
            if let Some(via) = &l.via {
                u.via.insert(via.clone());
            }
            if u.users.len() < MAX_USERS && !u.users.contains(&(s.repo.clone(), path.clone())) {
                u.users.push((s.repo.clone(), path.clone()));
            }
        }
        for (path, r) in &s.risks {
            spec_risks.push((s.repo.clone(), path.clone(), r.clone()));
        }
    }

    // Advisories for every concrete version.
    let queries: Vec<(&'static str, String, String)> = uses
        .keys()
        .filter(|(eco, _, v)| osv_ecosystem(eco).is_some() && v != "*")
        .map(|(e, n, v)| (*e, n.clone(), v.clone()))
        .collect();
    let ids = osv_ids(app, queries).await;
    let unique: BTreeSet<String> = ids.values().flatten().cloned().collect();
    let details: HashMap<String, Value> = bounded(
        unique
            .into_iter()
            .take(MAX_ADVISORY_DETAILS)
            .map(|id| {
                let app = app.clone();
                async move { (id.clone(), osv_detail(&app, id).await) }
            })
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .collect();

    // Registry facts for direct dependencies and anything with an advisory (bounded).
    let mut wanted_facts: Vec<(&'static str, String, String)> = uses
        .iter()
        .filter(|((eco, _, v), u)| matches!(*eco, "npm" | "cargo" | "pypi") && v != "*" && u.direct)
        .map(|((e, n, v), _)| (*e, n.clone(), v.clone()))
        .collect();
    for (e, n, v) in ids.keys() {
        if let Some(eco) = ["npm", "cargo", "pypi"].into_iter().find(|x| x == e) {
            wanted_facts.push((eco, n.clone(), v.clone()));
        }
    }
    wanted_facts.sort();
    wanted_facts.dedup();
    wanted_facts.truncate(MAX_VERSION_FACTS);
    let facts: HashMap<(&'static str, String, String), Value> = bounded(
        wanted_facts
            .into_iter()
            .map(|(e, n, v)| {
                let app = app.clone();
                async move {
                    let info = version_info(&app, e, n.clone(), v.clone()).await;
                    ((e, n, v), info)
                }
            })
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .collect();
    // Package-level facts (downloads, age) for direct dependencies.
    let direct_names: BTreeSet<(&'static str, String)> = uses
        .iter()
        .filter(|((eco, _, _), u)| u.direct && matches!(*eco, "npm" | "cargo"))
        .map(|((e, n, _), _)| (*e, n.clone()))
        .take(MAX_LATEST)
        .collect();
    let pkg_facts: HashMap<(&'static str, String), Value> = bounded(
        direct_names
            .into_iter()
            .map(|(e, n)| {
                let app = app.clone();
                async move {
                    let info = registry_info(&app, e, n.clone(), true).await;
                    ((e, n), info)
                }
            })
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .collect();

    let mut risks: Vec<Value> = Vec::new();
    let mut push = |severity: &str,
                    kind: &str,
                    eco: &str,
                    name: &str,
                    version: Option<&str>,
                    reason: String,
                    fix: Value,
                    url: String,
                    users: &[(String, String)],
                    via: &BTreeSet<String>,
                    direct: bool| {
        risks.push(json!({
            "severity": severity,
            "kind": kind,
            "ecosystem": eco,
            "name": name,
            "version": version,
            "reason": reason,
            "fix": fix,
            "url": url,
            "direct": direct,
            "via": via.iter().collect::<Vec<_>>(),
            "users": users.iter().map(|(r, p)| json!({"repo": r, "path": p})).collect::<Vec<_>>(),
        }));
    };
    for ((eco, name, version), u) in &uses {
        let key = (eco.to_string(), name.clone(), version.clone());
        // (1) Known vulnerabilities.
        for id in ids.get(&key).into_iter().flatten() {
            let d = details.get(id).cloned().unwrap_or(Value::Null);
            let sev = d["severity"].as_str().unwrap_or("unknown");
            let sev = if sev == "scored" || sev == "unknown" {
                "moderate"
            } else {
                sev
            };
            let fixed = d["fixed"].as_str();
            push(
                if sev == "medium" { "moderate" } else { sev },
                "vulnerability",
                eco,
                name,
                Some(version),
                format!("{id}: {}", d["summary"].as_str().unwrap_or("known vulnerability")),
                json!({"available": fixed.is_some(), "version": fixed}),
                d["url"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("https://osv.dev/vulnerability/{id}")),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        let f = facts
            .get(&(*eco, name.clone(), version.clone()))
            .cloned()
            .unwrap_or(Value::Null);
        let reg_url = match *eco {
            "npm" => format!("https://www.npmjs.com/package/{name}/v/{version}"),
            "cargo" => format!("https://crates.io/crates/{name}/{version}"),
            "pypi" => format!("https://pypi.org/project/{name}/{version}/"),
            _ => String::new(),
        };
        // (2) Yanked / deprecated.
        if f["yanked"].as_bool() == Some(true) {
            push(
                "high",
                "yanked",
                eco,
                name,
                Some(version),
                "this version was yanked from the registry".into(),
                json!({"available": true, "version": Value::Null}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        if let Some(msg) = f["deprecated"].as_str() {
            push(
                "moderate",
                "deprecated",
                eco,
                name,
                Some(version),
                format!("deprecated: {}", msg.chars().take(160).collect::<String>()),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        // (3) Install-time code.
        let scripts: Vec<&str> = f["install_scripts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
            .collect();
        if !scripts.is_empty() {
            push(
                "moderate",
                "install-script",
                eco,
                name,
                Some(version),
                format!("runs code on install — review: {}", scripts.join("; ")),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        // (4) Freshness and typosquats.
        if age_days(&f["published_at"]).is_some_and(|d| d < 7) {
            push(
                "low",
                "fresh-release",
                eco,
                name,
                Some(version),
                "this version was published less than 7 days ago".into(),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        let pkg = pkg_facts.get(&(*eco, name.clone())).cloned().unwrap_or(Value::Null);
        if age_days(&pkg["created_at"]).is_some_and(|d| d < 30) {
            push(
                "moderate",
                "young-package",
                eco,
                name,
                Some(version),
                "the package itself is less than 30 days old".into(),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        if u.direct && pkg["downloads"].as_u64().is_some_and(|d| d < 100) {
            push(
                "low",
                "low-downloads",
                eco,
                name,
                Some(version),
                format!(
                    "only {} downloads ({})",
                    pkg["downloads"],
                    pkg["downloads_period"].as_str().unwrap_or("recently")
                ),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        if let Some(popular) = typosquat_of(eco, name) {
            push(
                "high",
                "typosquat",
                eco,
                name,
                Some(version),
                format!("name is one or two letters from the popular \"{popular}\" — check it is the package you meant"),
                json!({"available": false}),
                reg_url.clone(),
                &u.users,
                &u.via,
                u.direct,
            );
        }
        // (6) Licences, for projects that are not copyleft themselves.
        if !own_copyleft && !f.is_null() {
            match license_concern(f["license"].as_str()) {
                Some("strong-copyleft") => push(
                    "high",
                    "license",
                    eco,
                    name,
                    Some(version),
                    format!(
                        "licence {} (strong copyleft) in a non-copyleft project",
                        f["license"].as_str().unwrap_or("?")
                    ),
                    json!({"available": false}),
                    reg_url.clone(),
                    &u.users,
                    &u.via,
                    u.direct,
                ),
                Some("copyleft") => push(
                    "moderate",
                    "license",
                    eco,
                    name,
                    Some(version),
                    format!(
                        "licence {} (copyleft) in a non-copyleft project",
                        f["license"].as_str().unwrap_or("?")
                    ),
                    json!({"available": false}),
                    reg_url.clone(),
                    &u.users,
                    &u.via,
                    u.direct,
                ),
                Some("unknown") if u.direct => push(
                    "low",
                    "license",
                    eco,
                    name,
                    Some(version),
                    "no recognisable licence".into(),
                    json!({"available": false}),
                    reg_url.clone(),
                    &u.users,
                    &u.via,
                    u.direct,
                ),
                _ => {}
            }
        }
    }
    // (5) Unpinned or unverifiable sources, from the files themselves.
    let empty = BTreeSet::new();
    for (repo, path, r) in &spec_risks {
        let sev = match r.kind {
            "unpinned-source" => "moderate",
            _ => "low",
        };
        push(
            sev,
            r.kind,
            r.ecosystem,
            &r.name,
            None,
            r.detail.clone(),
            json!({"available": false}),
            String::new(),
            &[(repo.clone(), path.clone())],
            &empty,
            true,
        );
    }
    risks.sort_by(|a, b| {
        severity_rank(b["severity"].as_str().unwrap_or(""))
            .cmp(&severity_rank(a["severity"].as_str().unwrap_or("")))
            .then(a["name"].as_str().cmp(&b["name"].as_str()))
    });
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for r in &risks {
        *counts
            .entry(match r["severity"].as_str().unwrap_or("low") {
                "critical" => "critical",
                "high" => "high",
                "moderate" | "medium" => "moderate",
                _ => "low",
            })
            .or_default() += 1;
    }
    Ok(json!({
        "org": org,
        "scanned_at": chrono::Utc::now(),
        "repos": repo_rows,
        "counts": counts,
        "fixable": risks.iter().filter(|r| r["fix"]["available"].as_bool() == Some(true)).count(),
        "risks": risks,
        "note": format!("registry facts for up to {MAX_VERSION_FACTS} direct or vulnerable versions; advisories from OSV.dev"),
    }))
}

// --- routes -------------------------------------------------------------------------------------

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && s != "."
        && s != ".."
}

fn scanning(what: &str, scope: &str) -> Value {
    json!({"status": "scanning", "message": format!("reading {scope}'s {what}; this can take a minute the first time")})
}

/// GET /api/orgs/{org}/packages/published
pub async fn org_published(State(app): State<Shared>, Path(org): Path<String>) -> ApiResult<Value> {
    if !valid_part(&org) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid organization"));
    }
    let key = format!("deps-published:{org}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let org = org.clone();
        move |app| {
            let org = org.clone();
            async move {
                let repos = org_repos(&app, &org).await?;
                published(&app, &org, &repos).await
            }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("published packages", &org))))
}

/// GET /api/orgs/{org}/packages/dependencies
pub async fn org_dependencies(State(app): State<Shared>, Path(org): Path<String>) -> ApiResult<Value> {
    if !valid_part(&org) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid organization"));
    }
    let key = format!("deps-dependencies:{org}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let org = org.clone();
        move |app| {
            let org = org.clone();
            async move {
                let repos = org_repos(&app, &org).await?;
                dependencies(&app, &org, &repos).await
            }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("dependencies", &org))))
}

/// GET /api/orgs/{org}/packages/supply-chain
pub async fn org_supply_chain(State(app): State<Shared>, Path(org): Path<String>) -> ApiResult<Value> {
    if !valid_part(&org) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid organization"));
    }
    let key = format!("deps-supply:{org}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let org = org.clone();
        move |app| {
            let org = org.clone();
            async move {
                let repos = org_repos(&app, &org).await?;
                supply_chain(&app, &org, &repos).await
            }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("supply chain", &org))))
}

/// GET /api/repos/{owner}/{name}/supply-chain
pub async fn repo_supply_chain(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    if !valid_part(&owner) || !valid_part(&name) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let repo = format!("{owner}/{name}");
    let key = format!("deps-supply-repo:{repo}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let (owner, repo) = (owner.clone(), repo.clone());
        move |app| {
            let (owner, repo) = (owner.clone(), repo.clone());
            async move { supply_chain(&app, &owner, std::slice::from_ref(&repo)).await }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("supply chain", &repo))))
}

/// GET /api/repos/{owner}/{name}/published
pub async fn repo_published(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    if !valid_part(&owner) || !valid_part(&name) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let repo = format!("{owner}/{name}");
    let key = format!("deps-published-repo:{repo}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let (owner, repo) = (owner.clone(), repo.clone());
        move |app| {
            let (owner, repo) = (owner.clone(), repo.clone());
            async move { published(&app, &owner, std::slice::from_ref(&repo)).await }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("published packages", &repo))))
}

/// GET /api/repos/{owner}/{name}/dependencies
pub async fn repo_dependencies(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    if !valid_part(&owner) || !valid_part(&name) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let repo = format!("{owner}/{name}");
    let key = format!("deps-dependencies-repo:{repo}");
    let value = crate::cached_answer_nowait(&app, key, SCAN_FRESH, {
        let (owner, repo) = (owner.clone(), repo.clone());
        move |app| {
            let (owner, repo) = (owner.clone(), repo.clone());
            async move { dependencies(&app, &owner, std::slice::from_ref(&repo)).await }
        }
    });
    Ok(Json(value.unwrap_or_else(|| scanning("dependencies", &repo))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Locked]) -> Vec<String> {
        v.iter().map(|l| format!("{}@{}", l.name, l.version)).collect()
    }

    #[test]
    fn manifests_say_what_they_define_and_whether_it_is_private() {
        let npm = defined_by(
            "package.json",
            r#"{"name":"@acme/sdk","version":"1.2.0","publishConfig":{"registry":"https://npm.pkg.github.com"}}"#,
        )
        .unwrap();
        assert_eq!(
            (npm.ecosystem, npm.name.as_str(), npm.version.as_deref(), npm.private),
            ("npm", "@acme/sdk", Some("1.2.0"), false)
        );
        assert_eq!(npm.registry.as_deref(), Some("https://npm.pkg.github.com"));
        assert!(
            defined_by("package.json", r#"{"name":"web","private":true}"#)
                .unwrap()
                .private
        );
        assert!(
            defined_by("package.json", r#"{"private":true}"#).is_none(),
            "no name, nothing defined"
        );
        let krate = defined_by(
            "Cargo.toml",
            "[package]\nname = \"colonizer\"\nversion = \"0.1.9\"\npublish = false\n",
        )
        .unwrap();
        assert!(krate.private && krate.name == "colonizer");
        assert!(defined_by("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n").is_none());
        let py = defined_by("pyproject.toml", "[project]\nname = \"legal-agent\"\nversion = \"0.3.0\"\n").unwrap();
        assert_eq!((py.ecosystem, py.name.as_str()), ("pypi", "legal-agent"));
        let go = defined_by("go.mod", "module github.com/acme/tool\n\ngo 1.22\n").unwrap();
        assert_eq!(go.name, "github.com/acme/tool");
        let dart = defined_by("pubspec.yaml", "name: app\nversion: 1.0.0+3\npublish_to: none\n").unwrap();
        assert!(dart.private && dart.version.as_deref() == Some("1.0.0+3"));
    }

    #[test]
    fn manifests_list_their_direct_dependencies() {
        let npm = declared_by(
            "package.json",
            r#"{"dependencies":{"react":"^19","@acme/ui":"workspace:*"},"devDependencies":{"vitest":"^3"}}"#,
        );
        assert_eq!(
            npm,
            vec![("npm", "react".to_string(), false), ("npm", "vitest".to_string(), true)]
        );
        let cargo = declared_by(
            "Cargo.toml",
            "[dependencies]\nserde = \"1\"\nlocal = { path = \"../local\" }\nrenamed = { package = \"tokio\", version = \"1\" }\n[dev-dependencies]\ntempfile = \"3\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
        );
        assert!(cargo.contains(&("cargo", "serde".into(), false)));
        assert!(
            cargo.contains(&("cargo", "tokio".into(), false)),
            "a renamed crate counts by its package name"
        );
        assert!(cargo.contains(&("cargo", "libc".into(), false)));
        assert!(cargo.contains(&("cargo", "tempfile".into(), true)));
        assert!(
            !cargo.iter().any(|(_, n, _)| n == "local"),
            "path crates are the repository's own"
        );
        let py = declared_by(
            "pyproject.toml",
            "[project]\ndependencies = [\"FastAPI>=0.110\", \"pydantic[email]~=2.6; python_version>'3.9'\"]\n[tool.poetry.dependencies]\npython = \"^3.11\"\nLangGraph = \"*\"\n",
        );
        assert!(py.contains(&("pypi", "fastapi".into(), false)));
        assert!(py.contains(&("pypi", "pydantic".into(), false)));
        assert!(py.contains(&("pypi", "langgraph".into(), false)));
        assert!(!py.iter().any(|(_, n, _)| n == "python"));
    }

    #[test]
    fn npm_lockfiles_in_every_format() {
        let v3 = r#"{"lockfileVersion":3,"packages":{"":{"name":"root"},"node_modules/react":{"version":"19.1.0"},"node_modules/@types/node":{"version":"22.1.0","dev":true},"node_modules/a/node_modules/b":{"version":"2.0.0"},"apps/web":{"name":"web"},"node_modules/web":{"link":true,"resolved":"apps/web"}}}"#;
        let l = parse_package_lock(v3);
        let mut got = names(&l);
        got.sort();
        assert_eq!(got, vec!["@types/node@22.1.0", "b@2.0.0", "react@19.1.0"]);
        assert!(
            names(&l).contains(&"b@2.0.0".to_string()),
            "a nested install is listed by its own name"
        );
        assert!(l.iter().any(|x| x.name == "@types/node" && x.dev));
        assert!(!l.iter().any(|x| x.name == "web"), "workspace links are skipped");
        let v1 = r#"{"lockfileVersion":1,"dependencies":{"lodash":{"version":"4.17.21","dependencies":{"inner":{"version":"1.0.0"}}}}}"#;
        assert_eq!(names(&parse_package_lock(v1)), vec!["lodash@4.17.21", "inner@1.0.0"]);

        let bun = "{\n  \"lockfileVersion\": 1,\n  // comment\n  \"packages\": {\n    \"react\": [\"react@19.1.0\", \"\", {}, \"sha512-x\"],\n    \"@acme/ui\": [\"@acme/ui@workspace:packages/ui\"],\n    \"zod\": [\"zod@3.23.8\", \"\", {},],\n  },\n}\n";
        assert_eq!(names(&parse_bun_lock(bun)), vec!["react@19.1.0", "zod@3.23.8"]);

        let pnpm = "lockfileVersion: '9.0'\n\nimporters:\n  .:\n    dependencies:\n      react:\n        specifier: ^19\n        version: 19.1.0\n\npackages:\n\n  react@19.1.0:\n    resolution: {integrity: x}\n\n  '@babel/core@7.25.2':\n    resolution: {integrity: y}\n\n  styled@6.1.0(react@19.1.0):\n    resolution: {integrity: z}\n\nsnapshots:\n\n  react@19.1.0: {}\n";
        assert_eq!(
            names(&parse_pnpm_lock(pnpm)),
            vec!["react@19.1.0", "@babel/core@7.25.2", "styled@6.1.0"]
        );
        let pnpm5 = "lockfileVersion: 5.4\npackages:\n  /react/18.2.0:\n    resolution: {integrity: x}\n";
        assert_eq!(names(&parse_pnpm_lock(pnpm5)), vec!["react@18.2.0"]);

        let yarn1 = "# yarn lockfile v1\n\n\"@babel/core@^7.0.0\", \"@babel/core@^7.1.0\":\n  version \"7.25.2\"\n  resolved \"x\"\n\nlodash@^4.17.0:\n  version \"4.17.21\"\n";
        assert_eq!(names(&parse_yarn_lock(yarn1)), vec!["@babel/core@7.25.2", "lodash@4.17.21"]);
        let berry = "__metadata:\n  version: 8\n\n\"react@npm:^19.0.0\":\n  version: 19.1.0\n  resolution: \"react@npm:19.1.0\"\n\n\"web@workspace:apps/web\":\n  version: 0.0.0-use.local\n";
        assert_eq!(names(&parse_yarn_lock(berry)), vec!["react@19.1.0"]);
    }

    #[test]
    fn rust_python_go_swift_and_dart_lockfiles() {
        let cargo = "version = 4\n\n[[package]]\nname = \"colonizer\"\nversion = \"0.1.9\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        assert_eq!(
            names(&parse_toml_lock("cargo", cargo)),
            vec!["serde@1.0.210"],
            "the workspace's own crate has no source"
        );
        let uv = "version = 1\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n\n[[package]]\nname = \"FastAPI\"\nversion = \"0.115.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n";
        assert_eq!(names(&parse_toml_lock("pypi", uv)), vec!["fastapi@0.115.0"]);
        let poetry = "[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\n";
        assert_eq!(names(&parse_toml_lock("pypi", poetry)), vec!["requests@2.32.3"]);
        let req = "# pins\nrequests==2.32.3\nuvicorn[standard]>=0.30  # server\n-r base.txt\n-e .\n";
        let r = parse_requirements(req);
        assert_eq!(names(&r), vec!["requests@2.32.3", "uvicorn@*"]);
        assert!(r.iter().all(|x| x.direct == Some(true)));
        let gomod = "module x\n\nrequire (\n\tgithub.com/spf13/cobra v1.8.1\n\tgolang.org/x/sys v0.24.0 // indirect\n)\nrequire github.com/stretchr/testify v1.9.0\n";
        let g = parse_go_mod(gomod);
        assert_eq!(
            names(&g),
            vec![
                "github.com/spf13/cobra@v1.8.1",
                "golang.org/x/sys@v0.24.0",
                "github.com/stretchr/testify@v1.9.0"
            ]
        );
        assert_eq!(
            g.iter().map(|x| x.direct).collect::<Vec<_>>(),
            vec![Some(true), Some(false), Some(true)]
        );
        let swift2 = r#"{"pins":[{"identity":"swift-collections","location":"https://github.com/apple/swift-collections","state":{"version":"1.1.2","revision":"abc"}}],"version":2}"#;
        assert_eq!(names(&parse_package_resolved(swift2)), vec!["swift-collections@1.1.2"]);
        let swift1 =
            r#"{"object":{"pins":[{"package":"Alamofire","state":{"revision":"1234567890","version":null}}]},"version":1}"#;
        assert_eq!(names(&parse_package_resolved(swift1)), vec!["Alamofire@1234567"]);
        let pub_lock = "packages:\n  http:\n    dependency: \"direct main\"\n    description:\n      name: http\n    source: hosted\n    version: \"1.2.2\"\n  meta:\n    dependency: transitive\n    source: hosted\n    version: \"1.15.0\"\n  flutter:\n    dependency: \"direct main\"\n    source: sdk\n    version: \"0.0.0\"\nsdks:\n  dart: \">=3.4.0\"\n";
        let p = parse_pubspec_lock(pub_lock);
        assert_eq!(names(&p), vec!["http@1.2.2", "meta@1.15.0"]);
        assert_eq!(p.iter().map(|x| x.direct).collect::<Vec<_>>(), vec![Some(true), Some(false)]);
    }

    #[test]
    fn supply_chain_signals_from_the_files() {
        let pkg = r#"{"dependencies":{"left-pad":"github:user/left-pad","lodash":"*","react":"^19","ui":"workspace:*","tar":"https://example.com/tar.tgz"}}"#;
        let r = manifest_risks("package.json", pkg);
        let kinds: Vec<(&str, &str)> = r.iter().map(|x| (x.name.as_str(), x.kind)).collect();
        assert!(kinds.contains(&("left-pad", "unpinned-source")) && kinds.contains(&("tar", "unpinned-source")));
        assert!(kinds.contains(&("lodash", "wildcard-range")));
        assert!(!kinds.iter().any(|(n, _)| *n == "react" || *n == "ui"));
        let cargo = manifest_risks(
            "Cargo.toml",
            "[dependencies]
foo = { git = \"https://github.com/a/foo\" }
bar = { git = \"https://github.com/a/bar\", rev = \"abc\" }
baz = \"*\"
",
        );
        let by = |n: &str| cargo.iter().find(|x| x.name == n).expect(n);
        assert!(by("foo").detail.contains("following a branch") && by("foo").kind == "unpinned-source");
        assert!(
            !by("bar").detail.contains("following a branch"),
            "a pinned rev is still a git source, but pinned"
        );
        assert_eq!(by("baz").kind, "wildcard-range");
        let req = manifest_risks(
            "requirements.txt",
            "requests==2.32.3
flask>=3
-e git+https://github.com/a/b.git#egg=mylib
",
        );
        assert_eq!(
            req.iter().map(|x| (x.name.as_str(), x.kind)).collect::<Vec<_>>(),
            vec![("flask", "unpinned-version"), ("mylib", "unpinned-source")]
        );
        let lock = r#"{"packages":{"":{},"node_modules/a":{"version":"1.0.0","resolved":"https://registry.npmjs.org/a/-/a-1.0.0.tgz","integrity":"sha512-x"},"node_modules/b":{"version":"1.0.0","resolved":"https://registry.npmjs.org/b/-/b-1.0.0.tgz"},"node_modules/c":{"version":"1.0.0","resolved":"git+ssh://git@github.com/x/c.git#abc"}}}"#;
        let i = package_lock_integrity(lock);
        assert_eq!(
            i.iter().map(|x| (x.name.as_str(), x.kind)).collect::<Vec<_>>(),
            vec![("b", "missing-integrity"), ("c", "unpinned-source")]
        );
        let nested = parse_package_lock(r#"{"packages":{"node_modules/a/node_modules/b":{"version":"2.0.0"}}}"#);
        assert_eq!(
            nested[0].via.as_deref(),
            Some("a"),
            "a nested install names the package that pulls it in"
        );
    }

    #[test]
    fn supply_chain_signals_from_names_and_licences() {
        assert_eq!(
            typosquat_of("npm", "lodahs"),
            None,
            "two swaps is past the short-name threshold"
        );
        assert_eq!(typosquat_of("npm", "expres"), Some("express"));
        assert_eq!(typosquat_of("npm", "reqeusts"), None, "requests lives on PyPI, not npm");
        assert_eq!(typosquat_of("pypi", "reqeusts"), Some("requests"));
        assert_eq!(typosquat_of("npm", "react"), None, "a popular name is not its own typosquat");
        assert_eq!(typosquat_of("npm", "@acme/reakt"), None, "scoped packages are the org's own");
        assert_eq!(typosquat_of("cargo", "serde_jsom"), Some("serde_json"));
        assert_eq!(edit_distance("kitten", "sitting", 5), 3);
        assert_eq!(license_concern(Some("AGPL-3.0-only")), Some("strong-copyleft"));
        assert_eq!(license_concern(Some("GPL-3.0")), Some("copyleft"));
        assert_eq!(license_concern(Some("LGPL-2.1")), None);
        assert_eq!(
            license_concern(Some("GPL-2.0 OR MIT")),
            None,
            "a permissive choice is offered"
        );
        assert_eq!(license_concern(Some("MIT")), None);
        assert_eq!(license_concern(None), Some("unknown"));
        assert_eq!(license_concern(Some("UNLICENSED")), Some("unknown"));
        let facts = npm_version_facts(
            &json!({"deprecated":"use x instead","scripts":{"postinstall":"node setup.js","test":"jest"},"license":{"type":"MIT"}}),
        );
        assert_eq!(facts["deprecated"], "use x instead");
        assert_eq!(facts["install_scripts"], json!(["postinstall: node setup.js"]));
        assert_eq!(facts["license"], "MIT");
    }

    #[test]
    fn helpers_are_exact() {
        assert!(wanted("package.json") && wanted("apps/pwa/package.json") && wanted("crates/x/Cargo.toml"));
        assert!(!wanted("node_modules/react/package.json") && !wanted("web/node_modules/x/package.json"));
        assert!(!wanted("tests/fixtures/package-lock.json") && !wanted("src/main.rs"));
        assert_eq!(pypi_name("Foo__Bar.baz"), "foo-bar-baz");
        assert_eq!(pep508_name("  Django>=5 ; extra == 'x'").as_deref(), Some("django"));
        assert_eq!(go_escape("github.com/Azure/go-sdk"), "github.com/!azure/go-sdk");
        assert!(version_lt("1.2.3", "1.10.0") && !version_lt("1.10.0", "1.2.3"));
        assert!(version_lt("2.0.0-rc.1", "2.0.0") && !version_lt("v1.8.1", "1.8.1"));
        assert!(version_lt("0.24.0", "v0.25.0"));
        assert_eq!(split_at_version("@scope/pkg@1.0.0"), Some(("@scope/pkg", "1.0.0")));
        assert_eq!(split_at_version("@scope/pkg"), None);
        let strict = strict_json("{\"a\": [1, 2,], // x\n \"s\": \"a,//b\",}");
        let v: Value = serde_json::from_str(&strict).expect("strict JSON parses");
        assert_eq!(v["a"], json!([1, 2]));
        assert_eq!(v["s"], "a,//b", "commas and slashes inside strings are kept");
        let doc = json!({"id":"GHSA-1","summary":"prototype pollution","database_specific":{"severity":"HIGH"},"affected":[{"ranges":[{"events":[{"introduced":"0"},{"fixed":"4.17.21"}]}]}]});
        let s = advisory_summary(&doc);
        assert_eq!((s["severity"].as_str(), s["fixed"].as_str()), (Some("high"), Some("4.17.21")));
    }
}
