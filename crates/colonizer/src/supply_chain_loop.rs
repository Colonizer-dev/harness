//! The built-in "Dependencies & supply chain" loop: on a schedule, check the dependencies of every
//! repository the operator opted in, and hand the fixable findings to one colony per repository
//! and ecosystem, which bumps them and opens a pull request.
//!
//! **Off by default, and opt-in per repository or org.** Nothing runs until the loop is switched on
//! *and* its allowlist names an org (`acme`) or a repository (`acme/app`); both start empty.
//!
//! **The check never runs inside a colony.** It reads the manifests and lockfiles at each
//! repository's default branch in the mothership's bare mirror (`code.rs`, the same read the
//! Packages view does) and hands them to the scanners already installed on the host: `cargo-audit`
//! for `Cargo.lock`, `npm audit` for `package-lock.json`, `osv-scanner` for the rest, and
//! `cargo-deny` for a repository whose `deny.toml` sets a licence policy. A lockfile no installed
//! scanner reads goes to the mothership's own OSV lookup (`deps.rs`), unless the operator switched
//! that off; either way the report says which scanner is missing and how to install it. Nothing
//! here downloads or installs a tool.
//!
//! **The fix is careful and rate-gentle.** Findings are grouped per repository and ecosystem into
//! one "supply-chain target" colony, never one per package; its brief lists the exact findings and
//! asks for minimal bumps to the fixed versions and nothing else. A target is refused when an open
//! pull request or a live colony already works on the same findings, while the repository's
//! cooldown lasts, past the per-repository and per-run caps, and always while
//! `COLONIZER_NO_EXTERNAL_EFFECTS` blocks external writes — then the run only reports.
//!
//! **Every run is reported:** findings by severity, what was dispatched, and what was skipped and
//! why — kept in the loop's history, written to the activity log, and served to the cockpit. A
//! critical or high vulnerability with no fixed version raises an attention item, because no
//! colony can bump its way out of it. A dry run does all of the reading and none of the writing.

use crate::{
    ApiResult, App, Shared,
    activity::Entry,
    authority, client_error,
    schedule::{Cadence, next_run_after},
    sessions::{self, NewSession, Session},
    util::{short_id, valid_repo, write_atomic},
};
use anyhow::{Context, Result, bail};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};

/// The loop's name, as the cockpit and the activity log show it.
pub const NAME: &str = "Dependencies & supply chain";
/// The origin tag a supply-chain target colony carries: `supply-chain:<ecosystem>`.
pub const ORIGIN_PREFIX: &str = "supply-chain:";
/// The title prefix the Packages view's hand-off gives a colony it starts on one risk; a live one
/// counts as already working on that package (duplicates.rs).
use crate::duplicates::HAND_OFF_PREFIX;
const FILE: &str = "supply-chain-loop.json";
/// Runs kept in the loop's history.
const HISTORY: usize = 30;
/// Findings kept per repository in a stored report.
const MAX_STORED_FINDINGS: usize = 200;
/// Findings listed in one colony's brief.
const MAX_BRIEF_FINDINGS: usize = 60;
/// The most frequent the loop may run: hourly.
pub const MIN_INTERVAL_MINUTES: u32 = 60;
/// How long one scanner may run on one lockfile.
const TOOL_LIMIT: Duration = Duration::from_secs(10 * 60);
/// A dispatch record is forgotten this long after its colony has finished.
const RECORD_DAYS: i64 = 30;

// --- findings -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Moderate,
    High,
    Critical,
}

impl Severity {
    /// A scanner's word for a severity; anything unrecognised is `None`.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word.trim().to_ascii_lowercase().as_str() {
            "critical" => Self::Critical,
            "high" => Self::High,
            "moderate" | "medium" => Self::Moderate,
            "low" | "info" | "informational" | "none" => Self::Low,
            _ => return None,
        })
    }

    /// A CVSS base score's band.
    pub fn from_score(score: f64) -> Self {
        if score >= 9.0 {
            Self::Critical
        } else if score >= 7.0 {
            Self::High
        } else if score >= 4.0 {
            Self::Moderate
        } else {
            Self::Low
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Moderate => "moderate",
            Self::Low => "low",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Vulnerability,
    Yanked,
    Unmaintained,
    Deprecated,
    License,
    Outdated,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Vulnerability => "vulnerability",
            Kind::Yanked => "yanked",
            Kind::Unmaintained => "unmaintained",
            Kind::Deprecated => "deprecated",
            Kind::License => "license",
            Kind::Outdated => "outdated",
        }
    }
}

/// One thing a scanner found in one lockfile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// `cargo`, `npm`, `pypi` or `go`.
    pub ecosystem: String,
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
    pub kind: Kind,
    pub severity: Severity,
    /// The advisory's id (RUSTSEC-…, GHSA-…), when there is one.
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    /// The lowest version that fixes it, when a scanner names one.
    #[serde(default)]
    pub fixed: Option<String>,
    /// Whether a fix exists at all: npm can say yes without naming the version, and a yanked
    /// release is fixed by any release that is not yanked.
    #[serde(default)]
    pub fix_available: bool,
    /// When the fix is reached by bumping another package (npm's `fixAvailable.name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_via: Option<String>,
    /// Whether the fix is a major version bump.
    #[serde(default)]
    pub major_bump: bool,
    #[serde(default)]
    pub url: Option<String>,
    /// The lockfile (or `deny.toml`) it was found through, repository-relative.
    #[serde(default)]
    pub lockfile: String,
    /// The scanner that found it.
    pub scanner: String,
}

impl Finding {
    fn new(ecosystem: &str, package: &str, kind: Kind, severity: Severity, title: String, lockfile: &str, scanner: &str) -> Self {
        Finding {
            ecosystem: ecosystem.to_string(),
            package: package.to_string(),
            version: None,
            kind,
            severity,
            id: None,
            title,
            fixed: None,
            fix_available: false,
            fix_via: None,
            major_bump: false,
            url: None,
            lockfile: lockfile.to_string(),
            scanner: scanner.to_string(),
        }
    }

    /// What identifies the finding across runs and scanners: the package, its version, what is
    /// wrong, and the advisory.
    pub fn key(&self) -> String {
        format!(
            "{}:{}@{}:{}:{}",
            self.ecosystem,
            self.package,
            self.version.as_deref().unwrap_or("*"),
            self.kind.word(),
            self.id.as_deref().unwrap_or("")
        )
    }

    /// Whether a colony can fix it with a version bump: a vulnerability with a fix, or a yanked
    /// release. Unmaintained, deprecated, licence and outdated findings are reported, not dispatched.
    pub fn dispatchable(&self) -> bool {
        match self.kind {
            Kind::Vulnerability => self.fix_available,
            Kind::Yanked => true,
            _ => false,
        }
    }
}

/// The ecosystem a lockfile or manifest belongs to, by file name.
pub fn lockfile_ecosystem(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    Some(match file {
        "Cargo.lock" => "cargo",
        "package-lock.json" | "npm-shrinkwrap.json" | "bun.lock" | "pnpm-lock.yaml" | "yarn.lock" => "npm",
        "poetry.lock" | "uv.lock" | "requirements.txt" => "pypi",
        "go.mod" => "go",
        _ => return None,
    })
}

/// OSV's ecosystem names, in this module's words.
fn from_osv_ecosystem(eco: &str) -> Option<&'static str> {
    Some(match eco {
        "crates.io" => "cargo",
        "npm" => "npm",
        "PyPI" => "pypi",
        "Go" => "go",
        _ => return None,
    })
}

// --- versions -----------------------------------------------------------------------------------

/// The part of a version a semver-compatible bump keeps: the major, or for `0.x` the minor.
fn compat_key(version: &str) -> (u64, u64) {
    let v = version.trim().trim_start_matches(['v', '=', '^', '~', '>', '<', ' ']);
    let mut parts = v.split(['.', '-', '+']).map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .unwrap_or(0)
    });
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    if major == 0 { (0, minor) } else { (major, 0) }
}

/// Whether going from `from` to `to` is a major bump. Cargo and npm read `0.x` minors as breaking
/// (their `^` does); Go and PyPI versions are compared by the major alone.
pub fn is_major_bump(ecosystem: &str, from: &str, to: &str) -> bool {
    let (a, b) = (compat_key(from), compat_key(to));
    if matches!(ecosystem, "cargo" | "npm") {
        a != b
    } else {
        a.0 != b.0
    }
}

/// How many major versions `current` is behind `latest` (for `0.x`, minors count).
pub fn majors_behind(current: &str, latest: &str) -> u64 {
    let (a, b) = (compat_key(current), compat_key(latest));
    if a.0 == 0 && b.0 == 0 {
        b.1.saturating_sub(a.1)
    } else if a.0 == 0 {
        b.0
    } else {
        b.0.saturating_sub(a.0)
    }
}

/// The versions a requirement list names as lower bounds (`^0.14.32`, `>=1.4.2`, `~2.1`), in order.
fn bounds_in(reqs: &[String]) -> Vec<String> {
    reqs.iter()
        .flat_map(|r| r.split([',', '|']))
        .filter_map(|c| {
            let c = c.trim();
            let v = c.trim_start_matches(['>', '=', '^', '~', ' ']);
            (!c.starts_with('<') && v.chars().next().is_some_and(|ch| ch.is_ascii_digit())).then(|| v.trim().to_string())
        })
        .collect()
}

/// The smallest candidate above the current version (every candidate when the current one is not
/// known): the minimal bump that fixes the finding.
pub fn minimal_fix(current: Option<&str>, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .filter(|c| current.is_none_or(|cur| crate::deps::version_lt(cur, c)))
        .min_by(|a, b| {
            if crate::deps::version_lt(a, b) {
                std::cmp::Ordering::Less
            } else if crate::deps::version_lt(b, a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .cloned()
}

/// A CVSS v3 vector's base score (`CVSS:3.1/AV:N/AC:L/…`); `None` when it is not one.
pub fn cvss3_score(vector: &str) -> Option<f64> {
    let mut m: BTreeMap<&str, &str> = BTreeMap::new();
    let mut parts = vector.trim().split('/');
    if !parts.next()?.starts_with("CVSS:3") {
        return None;
    }
    for p in parts {
        let (k, v) = p.split_once(':')?;
        m.insert(k, v);
    }
    let changed = *m.get("S")? == "C";
    let av = match *m.get("AV")? {
        "N" => 0.85,
        "A" => 0.62,
        "L" => 0.55,
        "P" => 0.2,
        _ => return None,
    };
    let ac = match *m.get("AC")? {
        "L" => 0.77,
        "H" => 0.44,
        _ => return None,
    };
    let pr = match (*m.get("PR")?, changed) {
        ("N", _) => 0.85,
        ("L", false) => 0.62,
        ("L", true) => 0.68,
        ("H", false) => 0.27,
        ("H", true) => 0.5,
        _ => return None,
    };
    let ui = match *m.get("UI")? {
        "N" => 0.85,
        "R" => 0.62,
        _ => return None,
    };
    let cia = |k: &str| -> Option<f64> {
        Some(match *m.get(k)? {
            "H" => 0.56,
            "L" => 0.22,
            "N" => 0.0,
            _ => return None,
        })
    };
    let iss = 1.0 - (1.0 - cia("C")?) * (1.0 - cia("I")?) * (1.0 - cia("A")?);
    let impact = if changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powi(15)
    } else {
        6.42 * iss
    };
    if impact <= 0.0 {
        return Some(0.0);
    }
    let exploitability = 8.22 * av * ac * pr * ui;
    let raw = if changed {
        (1.08 * (impact + exploitability)).min(10.0)
    } else {
        (impact + exploitability).min(10.0)
    };
    // CVSS rounds up to one decimal; the epsilon keeps 9.8 from reading as 9.9 on float noise.
    Some(((raw * 10.0) - 1e-9).ceil() / 10.0)
}

// --- scanner output parsers (pure, each tested against a fixture) -------------------------------

fn json_of(text: &str, tool: &str) -> Result<Value> {
    let text = text.trim();
    let start = text.find('{').with_context(|| format!("{tool} printed no JSON"))?;
    serde_json::from_str(&text[start..]).with_context(|| format!("{tool} printed JSON this cannot read"))
}

fn package_of(v: &Value) -> (String, Option<String>) {
    (
        v["name"].as_str().unwrap_or_default().to_string(),
        v["version"].as_str().map(str::to_string),
    )
}

/// `cargo audit --json`: vulnerabilities, plus the unmaintained and yanked warnings.
pub fn parse_cargo_audit(text: &str, lockfile: &str) -> Result<Vec<Finding>> {
    let v = json_of(text, "cargo-audit")?;
    let mut out = Vec::new();
    for item in v["vulnerabilities"]["list"].as_array().into_iter().flatten() {
        let adv = &item["advisory"];
        let (name, version) = package_of(&item["package"]);
        if name.is_empty() {
            continue;
        }
        let severity = adv["cvss"]
            .as_str()
            .and_then(cvss3_score)
            .map(Severity::from_score)
            .or_else(|| adv["severity"].as_str().and_then(Severity::parse))
            .unwrap_or(Severity::Moderate);
        let patched: Vec<String> = item["versions"]["patched"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect();
        let fixed = minimal_fix(version.as_deref(), &bounds_in(&patched));
        let id = adv["id"].as_str().map(str::to_string);
        let mut f = Finding::new(
            "cargo",
            &name,
            Kind::Vulnerability,
            severity,
            adv["title"].as_str().unwrap_or("known vulnerability").to_string(),
            lockfile,
            "cargo-audit",
        );
        f.major_bump = matches!((&version, &fixed), (Some(a), Some(b)) if is_major_bump("cargo", a, b));
        f.fix_available = fixed.is_some();
        f.fixed = fixed;
        f.url = adv["url"]
            .as_str()
            .map(str::to_string)
            .or_else(|| id.as_ref().map(|id| format!("https://rustsec.org/advisories/{id}")));
        f.id = id;
        f.version = version;
        out.push(f);
    }
    for (key, kind, severity) in [
        ("unmaintained", Kind::Unmaintained, Severity::Low),
        ("yanked", Kind::Yanked, Severity::High),
        ("unsound", Kind::Vulnerability, Severity::Moderate),
    ] {
        for item in v["warnings"][key].as_array().into_iter().flatten() {
            let (name, version) = package_of(&item["package"]);
            if name.is_empty() {
                continue;
            }
            let adv = &item["advisory"];
            let title = match kind {
                Kind::Yanked => "this version was yanked from crates.io".to_string(),
                _ => adv["title"].as_str().unwrap_or(key).to_string(),
            };
            let mut f = Finding::new("cargo", &name, kind, severity, title, lockfile, "cargo-audit");
            f.id = adv["id"].as_str().map(str::to_string);
            f.url = f.id.as_ref().map(|id| format!("https://rustsec.org/advisories/{id}"));
            f.version = version;
            // A yanked release is fixed by the nearest release that is not yanked.
            f.fix_available = kind == Kind::Yanked;
            out.push(f);
        }
    }
    Ok(out)
}

/// `npm audit --json` (report version 2): one finding per advisory, on the package it names.
pub fn parse_npm_audit(text: &str, lockfile: &str) -> Result<Vec<Finding>> {
    let v = json_of(text, "npm audit")?;
    if let Some(err) = v["error"]["summary"].as_str() {
        bail!("npm audit failed: {err}");
    }
    let mut out = Vec::new();
    for (name, entry) in v["vulnerabilities"].as_object().into_iter().flatten() {
        let (fix_available, fixed, fix_via, major) = match &entry["fixAvailable"] {
            Value::Bool(b) => (*b, None, None, false),
            Value::Object(o) => (
                true,
                o.get("version").and_then(Value::as_str).map(str::to_string),
                o.get("name")
                    .and_then(Value::as_str)
                    .filter(|n| n != name)
                    .map(str::to_string),
                o.get("isSemVerMajor").and_then(Value::as_bool).unwrap_or(false),
            ),
            _ => (false, None, None, false),
        };
        for via in entry["via"].as_array().into_iter().flatten() {
            // A string `via` names another package this one is vulnerable through; that package's
            // own entry carries the advisory.
            if !via.is_object() {
                continue;
            }
            let severity = via["severity"]
                .as_str()
                .and_then(Severity::parse)
                .or_else(|| entry["severity"].as_str().and_then(Severity::parse))
                .unwrap_or(Severity::Moderate);
            let url = via["url"].as_str().map(str::to_string);
            let id = url
                .as_deref()
                .and_then(|u| u.rsplit('/').next())
                .filter(|s| s.starts_with("GHSA-"))
                .map(str::to_string)
                .or_else(|| via["source"].as_u64().map(|n| format!("npm-{n}")));
            let range = via["range"].as_str().unwrap_or_default();
            let mut title = via["title"].as_str().unwrap_or("known vulnerability").to_string();
            if !range.is_empty() {
                title.push_str(&format!(" (affects {range})"));
            }
            let mut f = Finding::new("npm", name, Kind::Vulnerability, severity, title, lockfile, "npm audit");
            f.id = id;
            f.url = url;
            f.fix_available = fix_available;
            f.fixed = fixed.clone();
            f.fix_via = fix_via.clone();
            f.major_bump = major;
            out.push(f);
        }
    }
    Ok(out)
}

/// The severity OSV records for one vulnerability: the group's max score, the database's word, or
/// a CVSS v3 vector.
fn osv_severity(vuln: &Value, group_score: Option<f64>) -> Severity {
    if let Some(score) = group_score {
        return Severity::from_score(score);
    }
    if let Some(s) = vuln["database_specific"]["severity"].as_str().and_then(Severity::parse) {
        return s;
    }
    vuln["severity"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["score"].as_str().and_then(cvss3_score))
        .map(Severity::from_score)
        .max()
        .unwrap_or(Severity::Moderate)
}

/// Every `fixed` event an OSV record lists.
fn osv_fixed(vuln: &Value) -> Vec<String> {
    vuln["affected"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|a| a["ranges"].as_array().into_iter().flatten())
        .flat_map(|r| r["events"].as_array().into_iter().flatten())
        .filter_map(|e| e["fixed"].as_str().map(str::to_string))
        .collect()
}

/// `osv-scanner --format json`: one finding per alias group (GHSA, CVE and PYSEC ids for one flaw
/// are one finding), on the lockfile the caller scanned.
pub fn parse_osv_scanner(text: &str, lockfile: &str) -> Result<Vec<Finding>> {
    let v = json_of(text, "osv-scanner")?;
    let mut out = Vec::new();
    for result in v["results"].as_array().into_iter().flatten() {
        for pkg in result["packages"].as_array().into_iter().flatten() {
            let p = &pkg["package"];
            let Some(eco) = p["ecosystem"].as_str().and_then(from_osv_ecosystem) else {
                continue;
            };
            let name = p["name"].as_str().unwrap_or_default();
            let version = p["version"].as_str().map(str::to_string);
            let vulns: Vec<&Value> = pkg["vulnerabilities"].as_array().into_iter().flatten().collect();
            let mut groups: Vec<(Vec<String>, Option<f64>)> = pkg["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| {
                    (
                        g["ids"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|i| i.as_str().map(str::to_string))
                            .collect(),
                        g["max_severity"].as_str().and_then(|s| s.trim().parse::<f64>().ok()),
                    )
                })
                .collect();
            // No groups (an older osv-scanner): every vulnerability is its own group.
            if groups.is_empty() {
                groups = vulns
                    .iter()
                    .filter_map(|v| v["id"].as_str().map(|id| (vec![id.to_string()], None)))
                    .collect();
            }
            for (ids, score) in groups {
                let members: Vec<&Value> = vulns
                    .iter()
                    .copied()
                    .filter(|v| v["id"].as_str().is_some_and(|id| ids.iter().any(|x| x == id)))
                    .collect();
                let Some(lead) = members
                    .iter()
                    .find(|v| v["summary"].as_str().is_some_and(|s| !s.is_empty()))
                    .or(members.first())
                else {
                    continue;
                };
                let fixes: Vec<String> = members.iter().flat_map(|v| osv_fixed(v)).collect();
                let fixed = minimal_fix(version.as_deref(), &fixes);
                let id = lead["id"].as_str().map(str::to_string);
                let mut f = Finding::new(
                    eco,
                    name,
                    Kind::Vulnerability,
                    osv_severity(lead, score),
                    lead["summary"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("known vulnerability")
                        .to_string(),
                    lockfile,
                    "osv-scanner",
                );
                f.url = id.as_ref().map(|id| format!("https://osv.dev/vulnerability/{id}"));
                f.id = id;
                f.major_bump = matches!((&version, &fixed), (Some(a), Some(b)) if is_major_bump(eco, a, b));
                f.fix_available = fixed.is_some();
                f.fixed = fixed;
                f.version = version.clone();
                out.push(f);
            }
        }
    }
    Ok(out)
}

/// `cargo deny --format json check licenses bans`: one JSON diagnostic per line (cargo-deny prints
/// them on stderr). Licence rejections are what a repository's `deny.toml` policy exists for; bans,
/// and any advisory lines, come along.
pub fn parse_cargo_deny(text: &str, policy: &str) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    let mut read_any = false;
    for line in text.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        read_any = true;
        if v["type"].as_str() != Some("diagnostic") {
            continue;
        }
        let d = &v["fields"];
        let level = d["severity"].as_str().unwrap_or_default();
        let code = d["code"].as_str().unwrap_or_default();
        let (kind, severity) = match code {
            "rejected" | "unlicensed" | "no-license-field" | "parse-error" => (Kind::License, Severity::High),
            "banned" | "not-allowed" | "workspace-duplicate" => (Kind::License, Severity::Moderate),
            "vulnerability" => (Kind::Vulnerability, Severity::High),
            "unmaintained" => (Kind::Unmaintained, Severity::Low),
            "yanked" => (Kind::Yanked, Severity::High),
            _ => continue,
        };
        if level != "error" && kind == Kind::License {
            continue;
        }
        let krate = &d["graphs"][0]["Krate"];
        let (name, version) = package_of(krate);
        if name.is_empty() {
            continue;
        }
        let mut title = d["message"].as_str().unwrap_or(code).to_string();
        if kind == Kind::License
            && let Some(span) = d["labels"][0]["span"].as_str()
        {
            title.push_str(&format!(" ({span})"));
        }
        let mut f = Finding::new("cargo", &name, kind, severity, title, policy, "cargo-deny");
        f.id = d["advisory"]["id"].as_str().map(str::to_string);
        f.url = d["advisory"]["url"].as_str().map(str::to_string);
        f.version = version;
        f.fix_available = kind == Kind::Yanked;
        out.push(f);
    }
    if !read_any && !text.trim().is_empty() {
        bail!("cargo-deny printed no JSON diagnostics");
    }
    Ok(out)
}

/// The mothership's own reading (`deps.rs`' supply-chain view for one repository): its known
/// vulnerabilities, yanked and deprecated versions, as findings on the lockfile that locks them.
pub fn parse_builtin(risks: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    for r in risks["risks"].as_array().into_iter().flatten() {
        let kind = match r["kind"].as_str().unwrap_or_default() {
            "vulnerability" => Kind::Vulnerability,
            "yanked" => Kind::Yanked,
            "deprecated" => Kind::Deprecated,
            _ => continue,
        };
        let (Some(eco), Some(name)) = (r["ecosystem"].as_str(), r["name"].as_str()) else {
            continue;
        };
        let lockfile = r["users"][0]["path"].as_str().unwrap_or_default();
        let reason = r["reason"].as_str().unwrap_or_default();
        let (id, title) = match (kind, reason.split_once(": ")) {
            (Kind::Vulnerability, Some((id, rest))) => (Some(id.to_string()), rest.to_string()),
            _ => (None, reason.to_string()),
        };
        let mut f = Finding::new(
            eco,
            name,
            kind,
            r["severity"].as_str().and_then(Severity::parse).unwrap_or(Severity::Moderate),
            title,
            lockfile,
            "built-in OSV lookup",
        );
        f.version = r["version"].as_str().map(str::to_string);
        f.id = id;
        f.fixed = r["fix"]["version"].as_str().map(str::to_string);
        f.fix_available = kind == Kind::Yanked || f.fixed.is_some();
        f.major_bump = matches!((&f.version, &f.fixed), (Some(a), Some(b)) if is_major_bump(eco, a, b));
        f.url = r["url"].as_str().filter(|u| !u.is_empty()).map(str::to_string);
        out.push(f);
    }
    out
}

/// Direct dependencies at least one major version behind, from `deps.rs`' dependencies view.
pub fn parse_outdated(deps: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    for p in deps["packages"].as_array().into_iter().flatten() {
        if p["direct"].as_bool() != Some(true) {
            continue;
        }
        let (Some(eco), Some(name), Some(latest)) = (p["ecosystem"].as_str(), p["name"].as_str(), p["latest"].as_str()) else {
            continue;
        };
        for v in p["versions"].as_array().into_iter().flatten() {
            let Some(version) = v["version"].as_str().filter(|v| *v != "*") else {
                continue;
            };
            let behind = majors_behind(version, latest);
            if behind == 0 {
                continue;
            }
            let lockfile = v["users"][0]["path"].as_str().unwrap_or_default();
            let mut f = Finding::new(
                eco,
                name,
                Kind::Outdated,
                Severity::Low,
                format!(
                    "{behind} major version{} behind (latest {latest})",
                    if behind == 1 { "" } else { "s" }
                ),
                lockfile,
                "registry",
            );
            f.version = Some(version.to_string());
            f.fixed = Some(latest.to_string());
            f.major_bump = true;
            out.push(f);
        }
    }
    out
}

// --- which scanner reads which lockfile ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    CargoAudit,
    CargoDeny,
    NpmAudit,
    OsvScanner,
}

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::CargoAudit, Tool::CargoDeny, Tool::NpmAudit, Tool::OsvScanner];

    /// The executable looked for on the host.
    pub fn binary(self) -> &'static str {
        match self {
            Tool::CargoAudit => "cargo-audit",
            Tool::CargoDeny => "cargo-deny",
            Tool::NpmAudit => "npm",
            Tool::OsvScanner => "osv-scanner",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::CargoAudit => "cargo-audit",
            Tool::CargoDeny => "cargo-deny",
            Tool::NpmAudit => "npm audit",
            Tool::OsvScanner => "osv-scanner",
        }
    }

    /// How to install it: said to the operator, never run.
    pub fn install_hint(self) -> &'static str {
        match self {
            Tool::CargoAudit => "cargo install --locked cargo-audit",
            Tool::CargoDeny => "cargo install --locked cargo-deny",
            Tool::NpmAudit => "install Node.js, which ships npm",
            Tool::OsvScanner => "brew install osv-scanner, or go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest",
        }
    }
}

/// The host scanners that read a lockfile, best first. Empty: only the built-in lookup reads it.
fn preferred(lockfile: &str) -> &'static [Tool] {
    match lockfile.rsplit('/').next().unwrap_or(lockfile) {
        "Cargo.lock" => &[Tool::CargoAudit, Tool::OsvScanner],
        "package-lock.json" | "npm-shrinkwrap.json" => &[Tool::NpmAudit, Tool::OsvScanner],
        "pnpm-lock.yaml" | "yarn.lock" | "poetry.lock" | "uv.lock" | "requirements.txt" | "go.mod" => &[Tool::OsvScanner],
        _ => &[],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub tool: Tool,
    /// The lockfile, or the `deny.toml` for cargo-deny.
    pub path: String,
}

/// What one repository's check runs: host-scanner jobs, the lockfiles left to the built-in lookup,
/// and what to tell the operator about scanners that are missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanPlan {
    pub jobs: Vec<Job>,
    pub builtin: Vec<String>,
    /// Informational: a scanner was missing but the built-in lookup covered for it.
    pub notes: Vec<String>,
    /// Nothing checked a file: install one of these.
    pub missing: Vec<String>,
}

/// Plans one repository's check. Pure: `has` says which host tools are installed.
pub fn plan_scans(lockfiles: &[String], deny_tomls: &[String], has: impl Fn(Tool) -> bool, builtin: bool) -> ScanPlan {
    let mut plan = ScanPlan::default();
    let mut said: BTreeSet<String> = BTreeSet::new();
    let mut say = |list: &mut Vec<String>, line: String| {
        if said.insert(line.clone()) {
            list.push(line);
        }
    };
    for path in lockfiles {
        let tools = preferred(path);
        if let Some(tool) = tools.iter().copied().find(|t| has(*t)) {
            plan.jobs.push(Job {
                tool,
                path: path.clone(),
            });
            continue;
        }
        let file = path.rsplit('/').next().unwrap_or(path);
        let install = tools
            .iter()
            .map(|t| format!("{} ({})", t.label(), t.install_hint()))
            .collect::<Vec<_>>()
            .join(" or ");
        if builtin {
            plan.builtin.push(path.clone());
            if !install.is_empty() {
                say(
                    &mut plan.notes,
                    format!(
                        "no host scanner for {file}: checked with the built-in OSV lookup; install {install} for a fuller check"
                    ),
                );
            }
        } else if install.is_empty() {
            say(
                &mut plan.missing,
                format!("{path} was not checked: no host scanner reads {file}; switch the built-in OSV lookup on"),
            );
        } else {
            say(&mut plan.missing, format!("{path} was not checked: install {install}"));
        }
    }
    for policy in deny_tomls {
        if has(Tool::CargoDeny) {
            plan.jobs.push(Job {
                tool: Tool::CargoDeny,
                path: policy.clone(),
            });
        } else {
            say(
                &mut plan.missing,
                format!(
                    "{policy} sets a licence policy that was not checked: install cargo-deny ({})",
                    Tool::CargoDeny.install_hint()
                ),
            );
        }
    }
    plan
}

// --- grouping, duplicates, caps -----------------------------------------------------------------

/// One colony's worth of work: a repository's dispatchable findings in one ecosystem.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub repo: String,
    pub ecosystem: String,
    pub findings: Vec<Finding>,
    pub worst: Severity,
}

impl Target {
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.findings.iter().map(Finding::key).collect();
        keys.sort();
        keys.dedup();
        keys
    }

    pub fn origin(&self) -> String {
        format!("{ORIGIN_PREFIX}{}", self.ecosystem)
    }

    /// The package/advisory pairs this target's colony claims (issue #832): one per finding, the
    /// advisory empty where the finding names none.
    pub fn claims(&self) -> Vec<crate::supersede::SupplyChainTarget> {
        let mut out: Vec<crate::supersede::SupplyChainTarget> = Vec::new();
        for f in &self.findings {
            let t = crate::supersede::SupplyChainTarget::new(&f.package, f.id.as_deref().unwrap_or(""));
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    /// The work this target's colony would be on, as the shared duplicates service reads it.
    pub fn work(&self) -> crate::duplicates::Work {
        crate::duplicates::Work {
            repo: self.repo.clone(),
            issue: None,
            supply_chain: self.claims(),
            origin: Some(self.origin()),
        }
    }

    pub fn title(&self) -> String {
        let n = self.findings.len();
        format!(
            "{HAND_OFF_PREFIX}fix {n} {} finding{} ({} at worst)",
            self.ecosystem,
            if n == 1 { "" } else { "s" },
            self.worst.word()
        )
    }
}

/// Groups a repository's findings into targets: the dispatchable ones at or above `min`, one target
/// per ecosystem, the worst first. Duplicate findings from two scanners count once.
pub fn group(repo: &str, findings: &[Finding], min: Severity) -> Vec<Target> {
    let mut by_eco: BTreeMap<&str, Vec<Finding>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for f in findings.iter().filter(|f| f.dispatchable() && f.severity >= min) {
        if seen.insert((
            f.ecosystem.clone(),
            f.package.clone(),
            f.version.clone(),
            f.kind,
            f.id.clone(),
        )) {
            by_eco.entry(f.ecosystem.as_str()).or_default().push(f.clone());
        }
    }
    let mut targets: Vec<Target> = by_eco
        .into_iter()
        .map(|(eco, mut findings)| {
            findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.package.cmp(&b.package)));
            Target {
                repo: repo.to_string(),
                ecosystem: eco.to_string(),
                worst: findings.iter().map(|f| f.severity).max().unwrap_or(Severity::Low),
                findings,
            }
        })
        .collect();
    targets.sort_by(|a, b| b.worst.cmp(&a.worst).then(b.findings.len().cmp(&a.findings.len())));
    targets
}

/// A target colony the loop started: which findings it was given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetRecord {
    pub session: String,
    pub repo: String,
    pub ecosystem: String,
    pub keys: Vec<String>,
    pub at: DateTime<Utc>,
}

/// Whether a colony is still working on its target: live, queued, parked, publishing, or its pull
/// request is open — the shared supply-chain hold (duplicates.rs).
fn still_open(s: &Session) -> bool {
    crate::duplicates::holds_supply_chain(s)
}

/// The package/advisory pair a record's finding key (`eco:package@version:kind:id`) names.
fn key_target(key: &str) -> Option<crate::supersede::SupplyChainTarget> {
    let (_eco, rest) = key.split_once(':')?;
    let (rest, id) = rest.rsplit_once(':')?;
    let (versioned, _kind) = rest.rsplit_once(':')?;
    let (package, _version) = versioned.rsplit_once('@')?;
    (!package.is_empty()).then(|| crate::supersede::SupplyChainTarget::new(package, id))
}

/// The loop's records as the duplicates service reads them: what each loop colony was dispatched
/// with, for the colonies from before the targets rode on the session.
pub fn recorded_claims(records: &[TargetRecord]) -> Vec<crate::duplicates::Recorded> {
    records
        .iter()
        .map(|r| crate::duplicates::Recorded {
            session: r.session.clone(),
            targets: r.keys.iter().filter_map(|k| key_target(k)).collect(),
        })
        .collect()
}

/// [`recorded_claims`] of the loop's stored records.
pub async fn recorded(app: &Shared) -> Vec<crate::duplicates::Recorded> {
    recorded_claims(&app.supply_chain.state.read().await.targets)
}

/// The duplicate-supply-chain-target refusal, asked of the shared duplicates service (issue #832)
/// exactly as a launch is: the colony already on any of these findings — a supply-chain colony of
/// the same repository sharing a package and advisory (a loop colony with no record of its
/// findings counts as on all of its ecosystem's), or a Packages-view hand-off on one of these
/// packages — while it holds its claim.
pub fn duplicate_of(target: &Target, records: &[TargetRecord], sessions: &[Session]) -> Option<String> {
    match crate::duplicates::check(sessions, &recorded_claims(records), &target.work(), false, false) {
        crate::duplicates::Verdict::Refuse(refusal) => refusal.holder.colony,
        _ => None,
    }
}

/// A target the run did not dispatch, and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub repo: String,
    #[serde(default)]
    pub ecosystem: Option<String>,
    pub reason: String,
    #[serde(default)]
    pub findings: usize,
}

/// What a run dispatches and what it skips. Pure: the caps, the cooldown, the duplicate refusal and
/// the kill switch, in that order of precedence (the kill switch first).
pub fn plan_dispatch(
    targets: Vec<Target>,
    settings: &Settings,
    records: &[TargetRecord],
    cooldowns: &BTreeMap<String, DateTime<Utc>>,
    sessions: &[Session],
    now: DateTime<Utc>,
    blocked: bool,
) -> (Vec<Target>, Vec<Skipped>) {
    let mut go = Vec::new();
    let mut skipped = Vec::new();
    let mut per_repo: BTreeMap<String, u32> = BTreeMap::new();
    for t in targets {
        let skip = |reason: String| Skipped {
            repo: t.repo.clone(),
            ecosystem: Some(t.ecosystem.clone()),
            reason,
            findings: t.findings.len(),
        };
        if blocked {
            skipped.push(skip(
                "external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS): reported only, no colony started".to_string(),
            ));
            continue;
        }
        if let Some(session) = duplicate_of(&t, records, sessions) {
            skipped.push(skip(format!(
                "colony {session} already targets these findings (it is live or its pull request is open)"
            )));
            continue;
        }
        if let Some(last) = cooldowns.get(&t.repo.to_ascii_lowercase()) {
            let until = *last + ChronoDuration::hours(settings.cooldown_hours as i64);
            if until > now {
                skipped.push(skip(format!(
                    "the repository is cooling down until {} after the last dispatch",
                    until.format("%Y-%m-%d %H:%M UTC")
                )));
                continue;
            }
        }
        let n = per_repo.entry(t.repo.to_ascii_lowercase()).or_default();
        if *n >= settings.max_per_repo {
            skipped.push(skip(format!(
                "at most {} dispatch{} per repository per run",
                settings.max_per_repo,
                if settings.max_per_repo == 1 { "" } else { "es" }
            )));
            continue;
        }
        if go.len() as u32 >= settings.max_per_run {
            skipped.push(skip(format!(
                "at most {} dispatch{} per run",
                settings.max_per_run,
                if settings.max_per_run == 1 { "" } else { "es" }
            )));
            continue;
        }
        *n += 1;
        go.push(t);
    }
    (go, skipped)
}

/// A finding a colony cannot fix by bumping a version, bad enough that a person should look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub repo: String,
    pub ecosystem: String,
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    pub severity: Severity,
    pub reason: String,
}

/// The critical and high vulnerabilities with no fixed version.
pub fn attention_items(repo: &str, findings: &[Finding]) -> Vec<AttentionItem> {
    findings
        .iter()
        .filter(|f| f.kind == Kind::Vulnerability && f.severity >= Severity::High && !f.fix_available)
        .map(|f| AttentionItem {
            repo: repo.to_string(),
            ecosystem: f.ecosystem.clone(),
            package: f.package.clone(),
            version: f.version.clone(),
            id: f.id.clone(),
            severity: f.severity,
            reason: format!(
                "{} {}: no fixed version is published, so no colony can bump past it; replace the package, patch it, or accept the risk",
                f.severity.word(),
                f.id.as_deref().unwrap_or("vulnerability")
            ),
        })
        .collect()
}

/// What a target colony is told.
pub fn brief(t: &Target) -> String {
    let mut lines = vec![
        format!(
            "Supply-chain fix for {} ({}). The mothership's scheduled dependency check found these on the default branch:",
            t.repo, t.ecosystem
        ),
        String::new(),
    ];
    for f in t.findings.iter().take(MAX_BRIEF_FINDINGS) {
        let version = f.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
        let id = f.id.as_deref().map(|i| format!(" {i}")).unwrap_or_default();
        let fix = match (&f.fixed, &f.fix_via, f.kind) {
            (Some(v), Some(via), _) => format!("fixed by bumping {via} to {v}"),
            (Some(v), None, _) => format!("fixed in {v}"),
            (None, _, Kind::Yanked) => "move to the nearest release that is not yanked".to_string(),
            (None, _, _) => "a fix exists; use the lowest fixed version".to_string(),
        };
        let major = if f.major_bump { " — needs a major bump" } else { "" };
        lines.push(format!(
            "- [{}] {}{version} ({}){id}: {}. {fix}{major}. Locked in {}.",
            f.severity.word(),
            f.package,
            f.kind.word(),
            f.title,
            if f.lockfile.is_empty() { "the lockfile" } else { &f.lockfile }
        ));
    }
    if t.findings.len() > MAX_BRIEF_FINDINGS {
        lines.push(format!(
            "- … and {} more of the same kind; fix the ones above first.",
            t.findings.len() - MAX_BRIEF_FINDINGS
        ));
    }
    lines.extend([
        String::new(),
        "How to fix them:".to_string(),
        "- Make the minimal bump that reaches each fixed version (or, for a yanked release, the nearest release that is not yanked), and update the lockfiles to match."
            .to_string(),
        "- Change nothing else: no unrelated upgrades, no refreshing the whole lockfile, no new dependencies.".to_string(),
        "- No major version bumps unless one is the only way to reach a fixed version; if you make one, say which and why in the pull request."
            .to_string(),
        "- Run the repository's own checks (build, tests, lints) and fix what the bumps break.".to_string(),
        "- If a finding cannot be fixed this way, leave it and say why in the pull request.".to_string(),
        "- Title the pull request \"Supply chain: …\" and list every finding it fixes. Do not add any Claude/AI attribution."
            .to_string(),
    ]);
    lines.join("\n")
}

// --- settings and state -------------------------------------------------------------------------

fn default_cadence() -> Cadence {
    Cadence::Daily { hour: 6, minute: 17 }
}
fn one() -> u32 {
    1
}
fn three() -> u32 {
    3
}
fn twelve() -> u32 {
    12
}
fn yes() -> bool {
    true
}
fn moderate() -> Severity {
    Severity::Moderate
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The loop's switch. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// The orgs (`acme`) and repositories (`acme/app`) opted in. Empty by default: nothing runs.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Daily by default; as often as hourly.
    #[serde(default = "default_cadence")]
    pub cadence: Cadence,
    /// Colonies started per repository per run.
    #[serde(default = "one")]
    pub max_per_repo: u32,
    /// Colonies started per run, across every repository.
    #[serde(default = "three")]
    pub max_per_run: u32,
    /// Hours after a dispatch before the same repository gets another.
    #[serde(default = "twelve")]
    pub cooldown_hours: u32,
    /// The least severe finding that is dispatched; everything is reported.
    #[serde(default = "moderate")]
    pub min_severity: Severity,
    /// Also report direct dependencies a major version or more behind. Off by default; never dispatched.
    #[serde(default)]
    pub outdated: bool,
    /// Check lockfiles no host scanner reads with the mothership's own OSV lookup.
    #[serde(default = "yes")]
    pub builtin: bool,
    /// Whether a target colony publishes its pull request by itself.
    #[serde(default = "yes")]
    pub autopilot: bool,
}

impl Default for Settings {
    fn default() -> Self {
        serde_json::from_value(json!({})).expect("every field has a default")
    }
}

impl Settings {
    /// Refuses what could only fail or burst when the loop fires; tidies the allowlist.
    pub fn validated(mut self) -> Result<Self, String> {
        let mut allow = Vec::new();
        for entry in &self.allow {
            let e = entry.trim().trim_end_matches("/*").to_string();
            if e.is_empty() {
                continue;
            }
            let ok = crate::repo_scope::valid_entry(&e);
            if !ok {
                return Err(format!("{e:?} is not an org or an owner/repo"));
            }
            if !allow.iter().any(|a: &String| a.eq_ignore_ascii_case(&e)) {
                allow.push(e);
            }
        }
        self.allow = allow;
        self.cadence.check()?;
        match self.cadence {
            Cadence::SelfPaced {} => return Err("the supply-chain loop runs on a fixed cadence, not self-paced".into()),
            Cadence::Interval { minutes } if minutes < MIN_INTERVAL_MINUTES => {
                return Err(format!(
                    "the supply-chain loop runs at most hourly, got every {minutes} minutes"
                ));
            }
            _ => {}
        }
        if !(1..=5).contains(&self.max_per_repo) {
            return Err("max_per_repo must be 1 to 5".into());
        }
        if !(1..=10).contains(&self.max_per_run) {
            return Err("max_per_run must be 1 to 10".into());
        }
        if !(1..=24 * 30).contains(&self.cooldown_hours) {
            return Err("cooldown_hours must be 1 to 720".into());
        }
        Ok(self)
    }

    /// Whether the allowlist covers a repository, by its org or by name.
    pub fn covers(&self, repo: &str) -> bool {
        // `*` covers every repository here; a run resolves it first so hidden orgs fall out.
        self.allow.iter().any(|a| crate::repo_scope::entry_matches(a, repo))
    }

    fn active(&self) -> bool {
        self.enabled && !self.allow.is_empty()
    }
}

/// One repository's part of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoReport {
    pub repo: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub scanners: Vec<String>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dispatched {
    pub repo: String,
    pub ecosystem: String,
    /// The colony started; `None` in a dry run, which only says what it would start.
    #[serde(default)]
    pub session: Option<String>,
    pub title: String,
    pub findings: usize,
    pub worst: Severity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub dry_run: bool,
    /// `schedule` or `manual`.
    pub trigger: String,
    /// Whether the kill switch held every dispatch.
    #[serde(default)]
    pub blocked: bool,
    pub repos: Vec<RepoReport>,
    /// Findings by severity word.
    pub counts: BTreeMap<String, usize>,
    pub dispatched: Vec<Dispatched>,
    pub skipped: Vec<Skipped>,
    pub attention: Vec<AttentionItem>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Report {
    /// One line for the activity log and the loop's history.
    pub fn summary(&self) -> String {
        let findings = ["critical", "high", "moderate", "low"]
            .iter()
            .filter_map(|s| self.counts.get(*s).filter(|n| **n > 0).map(|n| format!("{n} {s}")))
            .collect::<Vec<_>>();
        let findings = if findings.is_empty() {
            "no findings".to_string()
        } else {
            findings.join(", ")
        };
        let verb = if self.dry_run { "would dispatch" } else { "dispatched" };
        format!(
            "{} repositor{}: {findings}; {verb} {}, skipped {}{}",
            self.repos.len(),
            if self.repos.len() == 1 { "y" } else { "ies" },
            self.dispatched.len(),
            self.skipped.len(),
            if self.attention.is_empty() {
                String::new()
            } else {
                format!(", {} need attention", self.attention.len())
            }
        )
    }
}

/// A run as the history lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub at: DateTime<Utc>,
    pub trigger: String,
    pub counts: BTreeMap<String, usize>,
    pub dispatched: usize,
    pub skipped: usize,
    pub attention: usize,
    pub summary: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LoopState {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_report: Option<Report>,
    #[serde(default)]
    pub history: Vec<RunSummary>,
    /// The open attention items, from the last real run.
    #[serde(default)]
    pub attention: Vec<AttentionItem>,
    #[serde(default)]
    pub targets: Vec<TargetRecord>,
    /// The last dispatch per repository (lowercased), for the cooldown.
    #[serde(default)]
    pub cooldowns: BTreeMap<String, DateTime<Utc>>,
}

pub struct Store {
    state: RwLock<LoopState>,
    file: PathBuf,
    persist: Mutex<()>,
    /// One run at a time, scheduled or pressed.
    running: Mutex<()>,
}

impl Store {
    pub fn new(config_dir: &FsPath) -> Self {
        let file = config_dir.join(FILE);
        let state = match std::fs::read(&file) {
            Err(_) => LoopState::default(),
            Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
                eprintln!(
                    "supply-chain loop: could not parse {}: {e}; starting switched off",
                    file.display()
                );
                LoopState::default()
            }),
        };
        Store {
            state: RwLock::new(state),
            file,
            persist: Mutex::new(()),
            running: Mutex::new(()),
        }
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.state.read().await)?;
        write_atomic(&self.file, &data).await
    }

    pub async fn snapshot(&self) -> LoopState {
        self.state.read().await.clone()
    }
}

// --- the host: the mirror, the scanners, the built-in lookup ------------------------------------

/// A repository's manifests, lockfiles and licence policies at its default branch.
#[derive(Clone, Debug, Default)]
pub struct RepoFiles {
    pub sha: String,
    pub files: Vec<(String, String)>,
    /// The bare mirror, for cargo-deny, which needs the whole tree.
    pub bare: Option<PathBuf>,
}

impl RepoFiles {
    fn lockfiles(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|(p, _)| p)
            .filter(|p| lockfile_ecosystem(p).is_some())
            .cloned()
            .collect()
    }

    fn deny_tomls(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|(p, _)| p)
            .filter(|p| {
                let file = p.rsplit('/').next().unwrap_or(p);
                file == "deny.toml" || file == ".deny.toml"
            })
            .cloned()
            .collect()
    }
}

/// Everything a run reads from outside itself. The real one reads the mirror and runs the host's
/// scanners; the tests' fake answers from fixtures, so no test touches the network.
pub trait Host: Send + Sync {
    fn has(&self, tool: Tool) -> bool;
    fn org_repos(&self, org: &str) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn files(&self, repo: &str) -> impl Future<Output = Result<RepoFiles>> + Send;
    /// Runs one scanner job; `(stdout, stderr)` whatever its exit status (scanners exit non-zero
    /// when they find something).
    fn run(&self, job: &Job, files: &RepoFiles) -> impl Future<Output = Result<(String, String)>> + Send;
    fn builtin(&self, repo: &str) -> impl Future<Output = Result<Vec<Finding>>> + Send;
    fn outdated(&self, repo: &str) -> impl Future<Output = Result<Vec<Finding>>> + Send;
}

/// Where an executable is on this host: `PATH`, then `~/.cargo/bin`. Nothing is ever fetched.
fn which(binary: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".cargo/bin"));
    }
    dirs.into_iter().map(|d| d.join(binary)).find(|p| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

pub struct RealHost {
    pub app: Shared,
}

/// A repository-relative path that stays inside the scratch directory.
fn safe_rel(path: &str) -> Option<&str> {
    (!path.is_empty() && !path.starts_with('/') && !path.split('/').any(|p| p == ".." || p.is_empty())).then_some(path)
}

impl RealHost {
    /// A scratch copy of the repository's manifests and lockfiles, for the scanners to read.
    async fn scratch(&self, files: &RepoFiles) -> Result<PathBuf> {
        let dir = self.app.cfg.data_dir.join("tmp").join(format!("supply-chain-{}", short_id()));
        for (path, text) in &files.files {
            let Some(rel) = safe_rel(path) else { continue };
            let dest = dir.join(rel);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&dest, text).await?;
        }
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir)
    }

    /// The whole tree at the scanned commit, for cargo-deny (it reads every workspace manifest).
    async fn tree(&self, files: &RepoFiles, into: &FsPath) -> Result<()> {
        let bare = files.bare.as_ref().context("no mirror to read the tree from")?;
        let tar = into.join("tree.tar");
        crate::util::exec_within(
            TOOL_LIMIT,
            self.app
                .git(bare)
                .args(["archive", "--format=tar", "-o"])
                .arg(&tar)
                .arg(&files.sha),
        )
        .await?;
        let tree = into.join("tree");
        tokio::fs::create_dir_all(&tree).await?;
        crate::util::exec_within(
            TOOL_LIMIT,
            tokio::process::Command::new("tar").arg("-xf").arg(&tar).arg("-C").arg(&tree),
        )
        .await?;
        Ok(())
    }
}

impl Host for RealHost {
    fn has(&self, tool: Tool) -> bool {
        which(tool.binary()).is_some()
    }

    async fn org_repos(&self, org: &str) -> Result<Vec<String>> {
        crate::deps::org_repos(&self.app, org).await
    }

    async fn files(&self, repo: &str) -> Result<RepoFiles> {
        let app = &self.app;
        let bare = crate::code::ensure_bare(app, repo).await?;
        let (_, sha) = crate::code::resolve(app, &bare, None).await?;
        let entries = crate::code::ls_tree(app, &bare, &sha).await?;
        let picked: Vec<(String, String)> = entries
            .into_iter()
            .filter(|(_, size, path)| {
                let file = path.rsplit('/').next().unwrap_or(path);
                *size < 16 * 1024 * 1024
                    && (crate::deps::wanted(path)
                        || ((file == "deny.toml" || file == ".deny.toml") && !path.contains("fixtures/")))
            })
            .map(|(blob, _, path)| (blob, path))
            .collect();
        let blobs: Vec<String> = picked.iter().map(|(b, _)| b.clone()).collect();
        let mut texts: Vec<Option<String>> = vec![None; picked.len()];
        crate::code::cat_batch(app, &bare, &blobs, |i, bytes| {
            texts[i] = Some(String::from_utf8_lossy(bytes).into_owned());
        })
        .await?;
        Ok(RepoFiles {
            sha,
            files: picked
                .into_iter()
                .zip(texts)
                .filter_map(|((_, path), text)| Some((path, text?)))
                .collect(),
            bare: Some(bare),
        })
    }

    async fn run(&self, job: &Job, files: &RepoFiles) -> Result<(String, String)> {
        let bin = which(job.tool.binary()).with_context(|| format!("{} is not installed", job.tool.label()))?;
        let rel = safe_rel(&job.path).context("unsafe path")?;
        let dir = self.scratch(files).await?;
        let file = dir.join(rel);
        let folder = file.parent().map(FsPath::to_path_buf).unwrap_or_else(|| dir.clone());
        let mut cmd = tokio::process::Command::new(&bin);
        // A scanner reads a lockfile; it never needs the operator's GitHub credentials.
        cmd.env_remove("GITHUB_TOKEN").env_remove("GH_TOKEN").current_dir(&folder);
        match job.tool {
            Tool::CargoAudit => {
                cmd.args(["audit", "--json", "--file"]).arg(&file);
            }
            Tool::NpmAudit => {
                cmd.args(["audit", "--json", "--package-lock-only"]);
            }
            Tool::OsvScanner => {
                cmd.args(["--format", "json", "--lockfile"]).arg(&file);
            }
            Tool::CargoDeny => {
                self.tree(files, &dir).await?;
                let manifest = dir.join("tree").join(rel).with_file_name("Cargo.toml");
                cmd.arg("--manifest-path")
                    .arg(&manifest)
                    .args(["--format", "json", "check", "licenses", "bans"]);
            }
        }
        let out = crate::util::exec_capture(TOOL_LIMIT, &mut cmd).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        out
    }

    async fn builtin(&self, repo: &str) -> Result<Vec<Finding>> {
        let org = repo.split('/').next().unwrap_or_default();
        let risks = crate::deps::supply_chain(&self.app, org, &[repo.to_string()]).await?;
        Ok(parse_builtin(&risks))
    }

    async fn outdated(&self, repo: &str) -> Result<Vec<Finding>> {
        let org = repo.split('/').next().unwrap_or_default();
        let deps = crate::deps::dependencies(&self.app, org, &[repo.to_string()]).await?;
        Ok(parse_outdated(&deps))
    }
}

// --- a run --------------------------------------------------------------------------------------

/// The repositories the allowlist covers: each named repository, and each org's repositories.
async fn covered<H: Host>(host: &H, settings: &Settings, notes: &mut Vec<String>) -> Vec<String> {
    let mut repos: Vec<String> = Vec::new();
    for entry in &settings.allow {
        let found = if entry.contains('/') {
            vec![entry.clone()]
        } else {
            match host.org_repos(entry).await {
                Ok(list) => list,
                Err(e) => {
                    notes.push(format!("could not list the repositories of {entry}: {e:#}"));
                    Vec::new()
                }
            }
        };
        for r in found {
            if !repos.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
                repos.push(r);
            }
        }
    }
    repos
}

/// Checks one repository. Never fails: what went wrong is the report's.
pub async fn check_repo<H: Host>(host: &H, repo: &str, settings: &Settings) -> RepoReport {
    let mut report = RepoReport {
        repo: repo.to_string(),
        ..RepoReport::default()
    };
    let files = match host.files(repo).await {
        Ok(f) => f,
        Err(e) => {
            report.error = Some(format!("could not read the mirror: {e:#}"));
            return report;
        }
    };
    report.sha = Some(files.sha.clone()).filter(|s| !s.is_empty());
    let lockfiles = files.lockfiles();
    if lockfiles.is_empty() {
        report.notes.push("no lockfile to check".to_string());
    }
    let plan = plan_scans(&lockfiles, &files.deny_tomls(), |t| host.has(t), settings.builtin);
    report.notes.extend(plan.notes);
    report.missing.extend(plan.missing);
    let mut findings = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for job in &plan.jobs {
        used.insert(job.tool.label().to_string());
        let parsed = match host.run(job, &files).await {
            Ok((stdout, stderr)) => match job.tool {
                Tool::CargoAudit => parse_cargo_audit(&stdout, &job.path),
                Tool::NpmAudit => parse_npm_audit(&stdout, &job.path),
                Tool::OsvScanner => parse_osv_scanner(&stdout, &job.path),
                Tool::CargoDeny => parse_cargo_deny(if stderr.contains('{') { &stderr } else { &stdout }, &job.path),
            }
            .map_err(|e| {
                let said = stderr.lines().next().unwrap_or_default();
                if said.is_empty() { e } else { e.context(said.to_string()) }
            }),
            Err(e) => Err(e),
        };
        match parsed {
            Ok(f) => findings.extend(f),
            Err(e) => report
                .notes
                .push(format!("{} on {} failed: {e:#}", job.tool.label(), job.path)),
        }
    }
    if !plan.builtin.is_empty() {
        used.insert("built-in OSV lookup".to_string());
        match host.builtin(repo).await {
            Ok(f) => findings.extend(f.into_iter().filter(|f| plan.builtin.contains(&f.lockfile))),
            Err(e) => report.notes.push(format!("the built-in OSV lookup failed: {e:#}")),
        }
    }
    if settings.outdated {
        match host.outdated(repo).await {
            Ok(f) => findings.extend(f),
            Err(e) => report.notes.push(format!("the outdated check failed: {e:#}")),
        }
    }
    // Two scanners on one lockfile (cargo-audit and cargo-deny) can report the same thing.
    let mut seen = BTreeSet::new();
    findings.retain(|f| seen.insert(f.key()));
    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.package.cmp(&b.package)));
    report.scanners = used.into_iter().collect();
    report.findings = findings;
    report
}

/// Starts one target colony through the normal admission path.
async fn dispatch(app: &Shared, t: &Target, autopilot: bool) -> Result<Session, crate::AppError> {
    let body = json!({
        "repo": t.repo,
        "title": t.title(),
        "instructions": brief(t),
        "autopilot": autopilot,
        "origin": t.origin(),
        // The findings ride on the colony, so every later launch reads its claim (issue #832).
        "supply_chain_targets": t.claims(),
    });
    let req: NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), None, Json(req)).await?;
    Ok(session)
}

/// What a run is asked to do.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RunRequest {
    #[serde(default)]
    pub dry_run: bool,
    /// Check this one repository. A dry run may name any repository; a real run only one the
    /// allowlist covers.
    #[serde(default)]
    pub repo: Option<String>,
}

/// One run: check every covered repository, plan the dispatches, and — unless it is a dry run —
/// start them, store the report and write the activity log.
pub async fn run_once<H: Host>(app: &Shared, host: &H, req: &RunRequest, trigger: &str, now: DateTime<Utc>) -> Report {
    let state = app.supply_chain.snapshot().await;
    let mut settings = state.settings.clone();
    // `*` is every visible org, resolved at use time so a hidden org is left out (issue #1213).
    settings.allow = app.resolve_scope(&settings.allow).await;
    let started_at = Utc::now();
    let mut notes = Vec::new();
    let repos = match &req.repo {
        Some(repo) => vec![repo.clone()],
        None => covered(host, &settings, &mut notes).await,
    };
    let mut reports = Vec::new();
    // One repository at a time: the scanners are the host's, and a burst of registry or GitHub
    // reads is exactly what this loop must never cause.
    for repo in &repos {
        reports.push(check_repo(host, repo, &settings).await);
    }
    let mut targets = Vec::new();
    let mut attention = Vec::new();
    for r in &reports {
        targets.extend(group(&r.repo, &r.findings, settings.min_severity));
        attention.extend(attention_items(&r.repo, &r.findings));
    }
    let blocked = authority::external_writes_blocked();
    let sessions = app.sessions.read().await.clone();
    let (go, mut skipped) = plan_dispatch(targets, &settings, &state.targets, &state.cooldowns, &sessions, now, blocked);
    for r in &reports {
        if r.findings
            .iter()
            .any(|f| !f.dispatchable() || f.severity < settings.min_severity)
        {
            let reported: Vec<&Finding> = r
                .findings
                .iter()
                .filter(|f| !f.dispatchable() || f.severity < settings.min_severity)
                .collect();
            let no_fix = reported
                .iter()
                .filter(|f| f.kind == Kind::Vulnerability && !f.fix_available)
                .count();
            let mut why = Vec::new();
            if no_fix > 0 {
                why.push(format!("{no_fix} with no fixed version"));
            }
            let other = reported
                .iter()
                .filter(|f| f.kind != Kind::Vulnerability && f.kind != Kind::Yanked)
                .count();
            if other > 0 {
                why.push(format!(
                    "{other} unmaintained, deprecated, licence or outdated (reported, not bumped)"
                ));
            }
            let below = reported
                .iter()
                .filter(|f| f.dispatchable() && f.severity < settings.min_severity)
                .count();
            if below > 0 {
                why.push(format!("{below} below the {} threshold", settings.min_severity.word()));
            }
            skipped.push(Skipped {
                repo: r.repo.clone(),
                ecosystem: None,
                reason: format!("not dispatched: {}", why.join("; ")),
                findings: reported.len(),
            });
        }
    }
    let mut dispatched = Vec::new();
    let mut records = Vec::new();
    for t in &go {
        let mut d = Dispatched {
            repo: t.repo.clone(),
            ecosystem: t.ecosystem.clone(),
            session: None,
            title: t.title(),
            findings: t.findings.len(),
            worst: t.worst,
        };
        if req.dry_run {
            dispatched.push(d);
            continue;
        }
        match dispatch(app, t, settings.autopilot).await {
            Ok(session) => {
                records.push(TargetRecord {
                    session: session.id.clone(),
                    repo: t.repo.clone(),
                    ecosystem: t.ecosystem.clone(),
                    keys: t.keys(),
                    at: now,
                });
                d.session = Some(session.id.clone());
                dispatched.push(d);
                let mut entry = Entry::new("loop.supply_chain", "colony").colony(&session);
                entry.target = Some(NAME.to_string());
                entry.section = Some("loops".to_string());
                entry.detail = Some(format!("{} finding(s), {} at worst", t.findings.len(), t.worst.word()));
                crate::activity::record(app, entry).await;
            }
            Err(e) => skipped.push(Skipped {
                repo: t.repo.clone(),
                ecosystem: Some(t.ecosystem.clone()),
                reason: format!("could not start the colony: {}", e.message()),
                findings: t.findings.len(),
            }),
        }
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for f in reports.iter().flat_map(|r| &r.findings) {
        *counts.entry(f.severity.word().to_string()).or_default() += 1;
    }
    if repos.is_empty() && notes.is_empty() {
        notes.push("nothing is opted in: add an org or a repository to the allowlist".to_string());
    }
    let mut report = Report {
        id: format!("scr_{}", short_id()),
        started_at,
        finished_at: Utc::now(),
        dry_run: req.dry_run,
        trigger: trigger.to_string(),
        blocked,
        repos: reports,
        counts,
        dispatched,
        skipped,
        attention,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    };
    if req.dry_run {
        return report;
    }
    for r in &mut report.repos {
        r.findings.truncate(MAX_STORED_FINDINGS);
    }
    let summary = report.summary();
    {
        let mut st = app.supply_chain.state.write().await;
        for rec in &records {
            st.cooldowns.insert(rec.repo.to_ascii_lowercase(), rec.at);
        }
        st.targets.extend(records);
        // A record is kept while its colony is still open, and for a month after it was made.
        st.targets.retain(|r| {
            now - r.at < ChronoDuration::days(RECORD_DAYS) || sessions.iter().any(|s| s.id == r.session && still_open(s))
        });
        st.attention = report.attention.clone();
        st.history.insert(
            0,
            RunSummary {
                id: report.id.clone(),
                at: report.started_at,
                trigger: trigger.to_string(),
                counts: report.counts.clone(),
                dispatched: report.dispatched.len(),
                skipped: report.skipped.len(),
                attention: report.attention.len(),
                summary: summary.clone(),
            },
        );
        st.history.truncate(HISTORY);
        st.last_report = Some(report.clone());
    }
    if let Err(e) = app.supply_chain.save().await {
        eprintln!("supply-chain loop: could not save {}: {e:#}", app.supply_chain.file.display());
    }
    let mut entry = Entry::new("loop.supply_chain", "colony");
    entry.target = Some(NAME.to_string());
    entry.section = Some("loops".to_string());
    entry.detail = Some(summary);
    if report.repos.len() == 1 {
        entry.repo = Some(report.repos[0].repo.clone());
        entry.org = report.repos[0].repo.split('/').next().map(str::to_string);
    }
    crate::activity::record(app, entry).await;
    crate::loop_history::record(app, crate::loop_history::from_supply_chain(&report)).await;
    for item in &report.attention {
        eprintln!(
            "supply-chain loop: attention: {} {} {} in {} has no fixed version",
            item.severity.word(),
            item.id.as_deref().unwrap_or("vulnerability"),
            item.package,
            item.repo
        );
    }
    report
}

/// Fires the loop when it is due. A run already in progress (a pressed one) skips this tick.
pub(crate) async fn tick<H: Host>(app: &Shared, host: &H, now: DateTime<Utc>) -> Option<Report> {
    let (active, due, cadence) = {
        let st = app.supply_chain.state.read().await;
        (
            st.settings.active(),
            st.next_run_at.is_some_and(|at| at <= now),
            st.settings.cadence.clone(),
        )
    };
    if !active || !due {
        return None;
    }
    let _running = app.supply_chain.running.try_lock().ok()?;
    // Book the next slot first, so a run that takes long cannot fire twice.
    app.supply_chain.state.write().await.next_run_at = Some(next_run_after(&cadence, now));
    let report = run_once(app, host, &RunRequest::default(), "schedule", now).await;
    Some(report)
}

async fn run(app: Shared) {
    let mut every = tokio::time::interval(Duration::from_secs(60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        every.tick().await;
        let host = RealHost { app: app.clone() };
        tick(&app, &host, Utc::now()).await;
    }
}

// --- routes -------------------------------------------------------------------------------------

async fn view(app: &App) -> Value {
    let st = app.supply_chain.snapshot().await;
    let scanners: BTreeMap<&str, bool> = Tool::ALL.iter().map(|t| (t.label(), which(t.binary()).is_some())).collect();
    json!({
        "name": NAME,
        "settings": st.settings,
        "next_run_at": st.next_run_at.filter(|_| st.settings.active()),
        "running": app.supply_chain.running.try_lock().is_err(),
        "scanners": scanners,
        "blocked": authority::external_writes_blocked(),
        "last_report": st.last_report,
        "history": st.history,
        "attention": st.attention,
    })
}

/// `GET /api/supply-chain-loop`: the loop's settings, the host's scanners, the last report and the history.
pub async fn get(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

/// `PUT /api/supply-chain-loop`: replaces the settings; the next run is booked from now.
pub async fn put(State(app): State<Shared>, Json(settings): Json<Settings>) -> ApiResult<Value> {
    let settings = settings.validated().map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    {
        let mut st = app.supply_chain.state.write().await;
        st.next_run_at = settings.active().then(|| next_run_after(&settings.cadence, Utc::now()));
        st.settings = settings;
    }
    app.supply_chain.save().await?;
    Ok(Json(view(&app).await))
}

/// `POST /api/supply-chain-loop/run`: a run now, or a dry run that lists the findings and what it
/// would dispatch and writes nothing. One run at a time.
pub async fn run_now(State(app): State<Shared>, body: Option<Json<RunRequest>>) -> ApiResult<Report> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    if let Some(repo) = &req.repo {
        if !valid_repo(repo) {
            return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
        }
        if !req.dry_run
            && !{
                let mut settings = app.supply_chain.state.read().await.settings.clone();
                settings.allow = app.resolve_scope(&settings.allow).await;
                settings.covers(repo)
            }
        {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                &format!("{repo} is not on the supply-chain loop's allowlist; add it, or ask for a dry run"),
            ));
        }
    }
    let Ok(_running) = app.supply_chain.running.try_lock() else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "a supply-chain run is already in progress",
        ));
    };
    let host = RealHost { app: app.clone() };
    Ok(Json(run_once(&app, &host, &req, "manual", Utc::now()).await))
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move { run(app).await });
}

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/supply-chain-loop", routing::get(get).put(put))
        .route("/api/supply-chain-loop/run", routing::post(run_now))
}

/// This module's feature descriptor (`features.rs`): its routes, scoped-token rule, activity rules,
/// kind and its background work, read through `features::ALL` by `server` and `api_tokens`.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "supply_chain_loop",
    routes,
    token_scope: Some(token_scope),
    activity: ACTIVITY,
    kinds: &["loop.supply_chain"],
    start_tasks: Some(start_tasks),
};

/// The activity lines the loop's two writes record: a settings change, and pressing run.
const ACTIVITY: &[crate::activity::Rule] = &[
    crate::activity::rule(
        "PUT",
        "/api/supply-chain-loop",
        "loop.update",
        crate::activity::Target::Fixed(NAME, "loops"),
    ),
    crate::activity::rule(
        "POST",
        "/api/supply-chain-loop/run",
        "loop.run_now",
        crate::activity::Target::Fixed(NAME, "loops"),
    ),
];

/// What a scoped token needs for the loop's settings and last report: a watch. Changing its
/// settings or pressing a run stays the owner's (it starts colonies on the allowlist).
fn token_scope<'a>(method: &axum::http::Method, segs: &[&'a str]) -> Option<crate::api_tokens::Need<'a>> {
    match segs {
        ["api", "supply-chain-loop"] if *method == axum::http::Method::GET => {
            Some(crate::api_tokens::Need::Bare(crate::api_tokens::Scope::Read))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
