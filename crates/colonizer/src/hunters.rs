//! Pluggable security-hunter modules (Strix, Shannon) for a future red-team run stage to drive
//! against a target: on-demand, checksum-verified, never vendored — Strix is Apache-2.0 and Shannon
//! is AGPL-3.0, so both are kept clear of the binary by shelling out rather than linking. Each
//! hunter's `Manifest` says how to install it, how to scan with it, and which parser would read its
//! output; findings parse into the same `Finding` the orchestrator flow files. Adding a hunter is a
//! manifest plus a parser (see docs/security-hunters.md).

use crate::{ApiResult, Shared, client_error, findings::Finding};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncWriteExt, process::Command};

/// How the hunter runs: a self-contained binary downloaded on demand, or a Node package via npx.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Binary,
    Node,
}

/// Which parser reads the hunter's output files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingsFormat {
    StrixJson,
    Sarif,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    /// Short stable id, used in routes, lock lookups and the on-disk cache.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// One line on what the hunter does.
    pub description: &'static str,
    /// Project homepage.
    pub homepage: &'static str,
    /// Name of the intended inline-SVG icon, or the short name as a text fallback.
    pub logo: &'static str,
    /// SPDX licence id.
    pub licence: &'static str,
    /// Hunter version this build drives.
    pub pinned_version: &'static str,
    /// How the hunter runs: a downloaded binary, or via npx.
    pub runtime: Runtime,
    /// Whether a scan needs a Docker daemon (colonies have none; see docs/security-hunters.md).
    pub needs_docker: bool,
    /// How the operator installs on demand — a documented command.
    pub install: &'static str,
    /// Headless scan invocation template with `{target}`/`{mode}`/`{out}` placeholders.
    pub scan: &'static str,
    /// Run-relative filenames the hunter writes.
    pub output: &'static [&'static str],
    /// Which parser reads the hunter's output.
    pub findings_format: FindingsFormat,
    /// Env var that points the hunter's LLM client at the Colonizer gateway.
    pub gateway_env: &'static str,
    /// True once a parser plus a checksum pin ship; false is a manifest-only stub.
    pub available: bool,
}

fn strix() -> Manifest {
    Manifest {
        id: "strix",
        name: "Strix",
        description: "Open-source AI penetration-testing agents that run code dynamically and validate findings with working PoCs.",
        homepage: "https://github.com/usestrix/strix",
        logo: "Strix",
        licence: "Apache-2.0",
        pinned_version: "1.6.2",
        runtime: Runtime::Binary,
        needs_docker: true,
        install: "POST /api/hunters/strix/install — pinned and checksum-verified from hunters.lock (requires COLONIZER_HUNTER_INSTALL=1)",
        scan: "strix -n --target {target} --scan-mode {mode}",
        output: &["vulnerabilities.json", "findings.sarif"],
        findings_format: FindingsFormat::StrixJson,
        gateway_env: "LLM_API_BASE",
        available: true,
    }
}

fn shannon() -> Manifest {
    Manifest {
        id: "shannon",
        name: "Shannon",
        description: "Keygraph's AI pentester for web apps and APIs — no exploit, no report.",
        homepage: "https://github.com/KeygraphHQ/shannon",
        logo: "Shannon",
        licence: "AGPL-3.0",
        pinned_version: "3.3.0",
        runtime: Runtime::Node,
        needs_docker: true,
        install: "npx @keygraph/shannon@3.3.0 setup",
        scan: "npx @keygraph/shannon@3.3.0 start -u {target} -r {repo} -o {out}",
        output: &["report.sarif", "report.json"],
        findings_format: FindingsFormat::Sarif,
        gateway_env: "SHANNON_AI_BASE_URL",
        available: false,
    }
}

/// Every hunter this build knows about.
pub fn builtin() -> Vec<Manifest> {
    vec![strix(), shannon()]
}

fn find(id: &str) -> Option<Manifest> {
    builtin().into_iter().find(|m| m.id == id)
}

/// Clips to `max` characters (never bytes, so no torn UTF-8); findings bounds come from findings.rs.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// A trimmed string field, tolerating missing or oddly-typed values.
fn field(record: &Value, key: &str) -> Option<String> {
    let value = record.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn single_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

// Consumed by the red-team run stage once it drives these modules; exercised by the unit tests below.
#[allow(dead_code)]
pub fn normalize(format: FindingsFormat, artifact: &str) -> Result<Vec<Finding>> {
    let json: Value = serde_json::from_str(artifact).context("parsing hunter output as JSON")?;
    Ok(match format {
        FindingsFormat::StrixJson => parse_strix(&json),
        FindingsFormat::Sarif => parse_sarif(&json),
    })
}

#[allow(dead_code)]
fn parse_strix(json: &Value) -> Vec<Finding> {
    let Some(records) = json.as_array() else {
        return Vec::new();
    };
    records
        .iter()
        .filter_map(|record| record.as_object().map(|_| parse_strix_record(record)))
        .collect()
}

#[allow(dead_code)]
fn parse_strix_record(record: &Value) -> Finding {
    let id = field(record, "id");
    let summary = field(record, "description")
        .or_else(|| field(record, "title"))
        .or_else(|| id.as_deref().map(|id| format!("Strix finding {id}")))
        .unwrap_or_else(|| "Strix finding".to_string());
    let title = clip(
        &single_line(
            &field(record, "title")
                .or_else(|| id.clone())
                .unwrap_or_else(|| "Strix finding".into()),
        ),
        200,
    );
    let mut head: Vec<String> = Vec::new();
    if let Some(severity) = field(record, "severity") {
        head.push(format!("**Severity:** {severity}"));
    }
    if let Some(cwe) = field(record, "cwe") {
        head.push(format!("**CWE:** {cwe}"));
    }
    if let Some(cve) = field(record, "cve") {
        head.push(format!("**CVE:** {cve}"));
    }
    let target: Vec<String> = ["target", "method", "endpoint"]
        .iter()
        .filter_map(|k| field(record, k))
        .collect();
    if !target.is_empty() {
        head.push(format!("**Target:** {}", target.join(" ")));
    }
    let mut rest: Vec<String> = Vec::new();
    if let Some(description) = field(record, "description") {
        rest.push(description);
    }
    if let Some(impact) = field(record, "impact") {
        rest.push(format!("**Impact:** {impact}"));
    }
    if let Some(remediation) = field(record, "remediation_steps") {
        rest.push(format!("**Remediation:** {remediation}"));
    }
    let mut body = head.join("\n");
    if !rest.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&rest.join("\n\n"));
    }
    if let Some(id) = id.as_deref() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("_Reported by Strix as {id}._"));
    }
    let body = clip(&body, 20_000);
    let body = if body.trim().is_empty() {
        clip(&summary, 20_000)
    } else {
        body
    };

    let mut parts: Vec<String> = Vec::new();
    if let Some(description) = field(record, "poc_description") {
        parts.push(description);
    }
    if let Some(code) = field(record, "poc_script_code") {
        parts.push(format!("```\n{code}\n```"));
    }
    if let Some(analysis) = field(record, "technical_analysis") {
        parts.push(analysis);
    }
    if let Some(evidence) = field(record, "evidence") {
        parts.push(evidence);
    }
    if let Some(locations) = record.get("code_locations").and_then(Value::as_array) {
        for location in locations {
            let Some(file) = location.get("file").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            if file.is_empty() {
                continue;
            }
            let mut entry = file.to_string();
            if let Some(snippet) = location.get("snippet").and_then(Value::as_str).map(str::trim)
                && !snippet.is_empty()
            {
                entry.push_str(&format!("\n```\n{snippet}\n```"));
            }
            parts.push(entry);
        }
    }
    let evidence = if parts.is_empty() {
        let id = id.as_deref().unwrap_or("unknown");
        let severity = field(record, "severity").unwrap_or_else(|| "unknown".into());
        format!(
            "Strix reported {id} ({severity}) with no attached PoC or code locations; see the raw {id} artifact in the run directory."
        )
    } else {
        parts.join("\n\n")
    };
    Finding {
        title,
        body,
        evidence: clip(&evidence, 5000),
    }
}

#[allow(dead_code)]
fn parse_sarif(json: &Value) -> Vec<Finding> {
    let Some(runs) = json.get("runs").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for run in runs {
        let mut rules: HashMap<&str, &Value> = HashMap::new();
        if let Some(defined) = run
            .get("tool")
            .and_then(|tool| tool.get("driver"))
            .and_then(|driver| driver.get("rules"))
            .and_then(Value::as_array)
        {
            for rule in defined {
                if let Some(id) = rule.get("id").and_then(Value::as_str) {
                    rules.insert(id, rule);
                }
            }
        }
        let Some(results) = run.get("results").and_then(Value::as_array) else {
            continue;
        };
        for result in results {
            let rule_id = result.get("ruleId").and_then(Value::as_str).map(str::trim).unwrap_or("");
            let rule = rules.get(rule_id).copied();
            let severity = result
                .get("properties")
                .and_then(|props| props.get("severity"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    match result.get("level").and_then(Value::as_str).unwrap_or("") {
                        "error" => "high",
                        "warning" => "medium",
                        "note" | "none" => "low",
                        _ => "medium",
                    }
                    .to_string()
                });
            let cwe = result
                .get("properties")
                .and_then(|props| props.get("cwe"))
                .and_then(Value::as_str)
                .or_else(|| {
                    rule.and_then(|rule| rule.get("properties"))
                        .and_then(|props| props.get("cwe"))
                        .and_then(Value::as_str)
                })
                .map(str::trim)
                .filter(|cwe| !cwe.is_empty())
                .map(str::to_string);
            let location: Option<String> = result.get("locations").and_then(Value::as_array).and_then(|locations| {
                locations.iter().find_map(|location| {
                    let physical = location.get("physicalLocation")?;
                    let uri = physical
                        .get("artifactLocation")
                        .and_then(|artifact| artifact.get("uri"))
                        .and_then(Value::as_str)?
                        .trim();
                    if uri.is_empty() {
                        return None;
                    }
                    Some(
                        match physical
                            .get("region")
                            .and_then(|region| region.get("startLine"))
                            .and_then(Value::as_u64)
                        {
                            Some(line) => format!("{uri}:{line}"),
                            None => uri.to_string(),
                        },
                    )
                })
            });
            let message = result
                .get("message")
                .and_then(|message| message.get("text"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty());
            let rule_label = if rule_id.is_empty() { "unknown" } else { rule_id };
            let title = match message {
                Some(text) => clip(text.lines().next().unwrap_or(text).trim(), 200),
                None => clip(rule_label, 200),
            };
            let title = if title.trim().is_empty() {
                "SARIF finding".to_string()
            } else {
                title
            };
            let mut body = format!("**Rule:** {rule_label}\n**Severity:** {severity}");
            if let Some(cwe) = cwe.as_deref() {
                body.push_str(&format!("\n**CWE:** {cwe}"));
            }
            if let Some(location) = location.as_deref() {
                body.push_str(&format!("\n**Location:** {location}"));
            }
            let body = clip(&body, 20_000);
            let mut evidence = message.unwrap_or_default().to_string();
            if let Some(location) = location.as_deref() {
                if !evidence.is_empty() {
                    evidence.push('\n');
                }
                evidence.push_str(&format!("Location: {location}"));
            }
            if evidence.trim().is_empty() {
                evidence = format!("SARIF result for rule {rule_label}; see the raw report in the run directory.");
            }
            out.push(Finding {
                title,
                body,
                evidence: clip(&evidence, 5000),
            });
        }
    }
    out
}

/// What the capability probe reports: whether the runtime and Docker are there, and what is missing.
#[derive(Clone, Debug, Serialize)]
pub struct Probe {
    pub runtime_ok: bool,
    pub docker_ok: bool,
    pub ready: bool,
    pub detail: String,
}

/// The pure half of [`probe`]: why this hunter can or cannot run here. A hunter that needs Docker is
/// never ready: colonies are microVMs without a Docker daemon yet, and the host's daemon is never
/// shared with a colony — Docker socket access is host root, and hunters run arbitrary PoCs.
fn probe_detail(runtime_ok: bool, runtime_msg: &str, docker_ok: bool) -> (bool, String) {
    if !runtime_ok {
        (false, runtime_msg.to_string())
    } else if !docker_ok {
        (
            false,
            "needs a Docker daemon running inside the colony microVM: colonies do not have one yet, and the host's daemon is never shared with a colony (Docker socket access is host root)".into(),
        )
    } else {
        (true, "ready".into())
    }
}

/// Checks the hunter can actually run here: its runtime exists, and Docker is not needed. Docker is
/// decided, not detected: nothing here may touch the host's Docker daemon, so a hunter with
/// `needs_docker` stays not-ready until Docker runs inside the colony microVM.
pub async fn probe(m: &Manifest, installed: bool) -> Probe {
    let (runtime_ok, runtime_msg) = match m.runtime {
        Runtime::Binary => (
            installed,
            format!("binary not installed yet; POST /api/hunters/{}/install first", m.id),
        ),
        Runtime::Node => {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("command -v node");
            let ok = crate::util::exec(&mut cmd).await.is_ok();
            (
                ok,
                "node runtime not found; install it before running this hunter".to_string(),
            )
        }
    };
    let docker_ok = !m.needs_docker;
    let (ready, detail) = probe_detail(runtime_ok, &runtime_msg, docker_ok);
    Probe {
        runtime_ok,
        docker_ok,
        ready,
        detail,
    }
}

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../hunters.lock");

#[derive(Clone, Debug, PartialEq)]
struct Pin {
    id: String,
    version: String,
    sha256: String,
    url: String,
}

/// The pin for this hunter on this OS and architecture: hunter binaries are Linux-only, so any
/// non-Linux host has no pin and is therefore neither installable nor installed.
fn pin_for(lock: &str, id: &str, os: &str, arch: &str) -> Option<Pin> {
    if os != "linux" {
        return None;
    }
    let platform = match arch {
        "x86_64" => "linux-x86_64",
        "aarch64" => "linux-aarch64",
        _ => return None,
    };
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [pid, version, p, "binary", sha256, url] if *pid == id && *p == platform => Some(Pin {
                    id: pid.to_string(),
                    version: version.to_string(),
                    sha256: sha256.to_string(),
                    url: url.to_string(),
                }),
                _ => None,
            }
        })
}

fn pin(id: &str) -> Option<Pin> {
    pin_for(LOCK, id, std::env::consts::OS, std::env::consts::ARCH)
}

fn root(app: &Shared, id: &str) -> PathBuf {
    app.cfg.data_dir.join("hunters").join(id)
}

/// The pinned version, once its binary is on disk.
fn installed_in(root: &FsPath, version: &str) -> Option<String> {
    root.join(version).join("strix").is_file().then(|| version.to_string())
}

fn installed(app: &Shared, m: &Manifest) -> Option<String> {
    installed_for(&root(app, m.id), pin(m.id).as_ref())
}

/// The installed check behind [`installed`], taking the pin explicitly so tests can show a binary on
/// disk still counts as not installed when there is no pin for this host (e.g. macOS).
fn installed_for(root: &FsPath, pin: Option<&Pin>) -> Option<String> {
    let pin = pin?;
    installed_in(root, &pin.version)
}

/// Largest hunter tarball the installer will buffer: Strix 1.6.2 ships ~89 MB per platform, so 256
/// MiB is roughly 3x headroom against a compromised mirror serving an unbounded body.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// Env var that opts the operator into hunter installs: install runs only when it holds a truthy
/// value (`1`/`true`/`on`/`yes`, case-insensitive); unset or anything else means disabled.
const INSTALL_ENV_VAR: &str = "COLONIZER_HUNTER_INSTALL";

/// One async mutex per hunter id, so two concurrent installs of the same hunter serialise: the
/// second re-checks after acquiring the lock and returns without downloading again.
static INSTALL_LOCKS: LazyLock<std::sync::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn install_lock(id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    INSTALL_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(id.to_string())
        .or_default()
        .clone()
}

/// Monotonic process-local counter folded into staging/temp names so two installs never share one.
static NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let n = NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{n}", std::process::id())
}

/// Whether hunter installs are allowed right now (see [`INSTALL_ENV_VAR`]).
fn hunter_install_allowed() -> bool {
    hunter_install_allowed_for(std::env::var(INSTALL_ENV_VAR).ok().as_deref())
}

fn hunter_install_allowed_for(value: Option<&str>) -> bool {
    matches!(
        value.unwrap_or("").trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Downloads the pinned archive, checks its sha256 against the pin, and unpacks the single binary
/// into `<data>/hunters/<id>/<version>/strix`. The verified bytes are piped straight into
/// `tar -xzf -` over stdin — no tarball file ever lands on disk, so nothing can swap the archive
/// between the hash check and the unpack — and the binary lands atomically (a uniquely-named temp
/// file in the version dir, chmodded, then renamed over the final path), so readers never see a
/// half-written binary. Staging uses a unique directory that is removed on every path.
pub async fn install(app: &Shared, id: &str) -> Result<String> {
    let Some(pin) = pin(id) else {
        bail!(
            "no pinned {id} artifact for this machine ({}/{}); hunter binaries are Linux-only, see hunters.lock",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    };
    install_pinned(&root(app, id), &pin, MAX_DOWNLOAD_BYTES).await
}

/// The core of [`install`], taking the root dir, the pin and the byte cap as parameters so tests can
/// drive it against a local server with a tiny cap.
async fn install_pinned(root: &FsPath, pin: &Pin, max_bytes: u64) -> Result<String> {
    let _guard = install_lock(&pin.id).lock_owned().await;
    if let Some(version) = installed_in(root, &pin.version) {
        return Ok(version);
    }
    tokio::fs::create_dir_all(root).await?;
    let bytes = download_bytes(pin, max_bytes).await?;

    let staging = root.join(format!(".{}.staging-{}", pin.version, unique_suffix()));
    tokio::fs::create_dir(&staging).await?;
    let placed = unpack_and_place(root, &staging, &bytes, &pin.version).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    placed
}

/// Unpacks already-verified `bytes` via `tar` over stdin and atomically places the single binary.
async fn unpack_and_place(root: &FsPath, staging: &FsPath, bytes: &[u8], version: &str) -> Result<String> {
    let mut child = Command::new("tar")
        .arg("-xzf")
        .arg("-")
        .arg("--no-same-owner")
        .arg("-C")
        .arg(staging)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting tar to unpack the hunter archive")?;
    // The writer runs on its own task while `wait_with_output` drains stderr: pushing ~90 MB into
    // a 64 KiB stdin pipe while tar fills its own stderr pipe would otherwise stall both sides.
    let mut stdin = child.stdin.take().context("tar started without a stdin pipe")?;
    let owned = bytes.to_vec();
    let writer = tokio::spawn(async move {
        let result = stdin.write_all(&owned).await;
        drop(stdin);
        result
    });
    let output = child.wait_with_output().await.context("unpacking the hunter archive")?;
    writer
        .await
        .context("waiting for the tar stdin writer")?
        .context("piping the verified archive into tar")?;
    if !output.status.success() {
        bail!(
            "unpacking the hunter archive failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let binary = find_staged_binary(staging).await?;
    let dest = root.join(version);
    tokio::fs::create_dir_all(&dest).await?;
    let tmp = dest.join(format!(".strix-install-{}", unique_suffix()));
    let placed = async {
        tokio::fs::rename(&binary, &tmp)
            .await
            .context("moving the hunter binary into place")?;
        tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o555)).await?;
        tokio::fs::rename(&tmp, dest.join("strix"))
            .await
            .context("moving the hunter binary into place")?;
        Ok(version.to_string())
    }
    .await;
    if placed.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    placed
}

/// The single binary inside the staging dir: the archive holds exactly one regular file, preferring
/// the one named like the release binary. Metadata is read without following links and symlinks are
/// never accepted, so a hostile archive cannot point the install at a host path.
async fn find_staged_binary(staging: &FsPath) -> Result<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut entries = tokio::fs::read_dir(staging).await?;
    while let Some(entry) = entries.next_entry().await? {
        if is_plain_file(&entry.path()).await? {
            files.push(entry.path());
        }
    }
    if files.is_empty() {
        let mut top = tokio::fs::read_dir(staging).await?;
        while let Some(entry) = top.next_entry().await? {
            let meta = tokio::fs::symlink_metadata(entry.path()).await?;
            if meta.is_dir() {
                let mut inner = tokio::fs::read_dir(entry.path()).await?;
                while let Some(entry) = inner.next_entry().await? {
                    if is_plain_file(&entry.path()).await? {
                        files.push(entry.path());
                    }
                }
            }
        }
    }
    files
        .iter()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("strix"))
        })
        .or_else(|| files.first())
        .cloned()
        .context("unpacked archive has no strix binary")
}

/// True for a regular file, without following a trailing symlink: symlinks fail this check.
async fn is_plain_file(path: &FsPath) -> Result<bool> {
    Ok(tokio::fs::symlink_metadata(path).await?.is_file())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads the pinned URL into memory (bounded by `max_bytes`), rejecting early on an oversized
/// Content-Length and while streaming, then verifies the sha256 over those same bytes.
async fn download_bytes(pin: &Pin, max_bytes: u64) -> Result<Vec<u8>> {
    // GitHub release downloads redirect to object storage, so redirects are followed here (the
    // provider gateway's client deliberately does not) — but only over https, and at most ten hops.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 || attempt.url().scheme() != "https" {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let client = reqwest::Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()?;
    let response = client.get(&pin.url).send().await?;
    // A refused non-https redirect (or any other non-success) must fail here as a status error,
    // never fall through to a confusing "checksum mismatch" over an error page.
    let status = response.status();
    if !status.is_success() {
        bail!("refusing {} {}: {} answered HTTP {status}", pin.id, pin.version, pin.url);
    }
    if let Some(len) = response.content_length()
        && len > max_bytes
    {
        bail!(
            "refusing {} {}: Content-Length {len} exceeds the download cap of {max_bytes} bytes",
            pin.id,
            pin.version
        );
    }
    let mut bytes = Vec::new();
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was interrupted")?;
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            bail!(
                "refusing {} {}: download exceeds the cap of {max_bytes} bytes",
                pin.id,
                pin.version
            );
        }
        digest.update(&chunk);
        bytes.extend_from_slice(&chunk);
    }

    let got = hex(digest.finish().as_ref());
    if got != pin.sha256 {
        bail!(
            "checksum mismatch for {} {}: expected {}, got {got}",
            pin.id,
            pin.version,
            pin.sha256
        );
    }
    Ok(bytes)
}

/// `POST /api/hunters/{id}/install` — download and verify the pinned binary, or explain how to
/// install a hunter that is not a downloaded binary. Installs run only when the operator opts in
/// via `COLONIZER_HUNTER_INSTALL=1`; the check happens before any network I/O.
pub async fn install_handler(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    install_handler_inner(State(app), Path(id), hunter_install_allowed()).await
}

/// The body of [`install_handler`], taking the opt-in explicitly so tests exercise both branches
/// without touching the process environment.
async fn install_handler_inner(State(app): State<Shared>, Path(id): Path<String>, allowed: bool) -> ApiResult<Value> {
    let Some(m) = find(&id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such hunter"));
    };
    if !allowed {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            &format!("hunter installs are disabled; set {INSTALL_ENV_VAR}=1 to allow this install"),
        ));
    }
    if !m.available {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "{} is a manifest-only stub in this build; install it with: {}",
                m.name, m.install
            ),
        ));
    }
    if !matches!(m.runtime, Runtime::Binary) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("{} is not a downloaded binary; install it with: {}", m.name, m.install),
        ));
    }
    if pin(&id).is_none() {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!(
                "no pinned {id} artifact for this machine ({}/{}); hunter binaries are Linux-only, see hunters.lock",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ));
    }
    let version = install(&app, &id).await?;
    Ok(Json(json!({"id": id, "installed": true, "version": version, "manifest": m})))
}

/// `GET /api/hunters/{id}/probe` — the manifest, what is installed, and whether it can run here.
pub async fn probe_handler(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let Some(m) = find(&id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such hunter"));
    };
    let installed = installed(&app, &m);
    let p = probe(&m, installed.is_some()).await;
    Ok(Json(json!({"manifest": m, "installed": installed, "probe": p})))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/hunters/{id}/install", routing::post(install_handler))
        .route("/api/hunters/{id}/probe", routing::get(probe_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strix_findings_carry_severity_cwe_and_the_poc_as_evidence() {
        let output = json!([
            {
                "id": "vuln-0001",
                "title": "SQL Injection in login endpoint",
                "severity": "high",
                "cwe": "CWE-89",
                "description": "User input reaches the SQL query unsanitized.",
                "impact": "Full read of users table.",
                "target": "https://app.example.com",
                "endpoint": "/api/login",
                "method": "POST",
                "poc_description": "Boolean-based blind injection.",
                "poc_script_code": "curl -X POST .../api/login -d \"user=admin' OR '1'='1\"",
                "code_locations": [{"file": "app/auth.py", "snippet": "query = ... + name + ..."}],
                "remediation_steps": "Use parameterized queries.",
            },
            {
                "id": "vuln-0002",
                "title": "Verbose error message",
                "severity": "low",
                "description": "Stack traces exposed.",
            },
        ]);
        let findings = parse_strix(&output);
        assert_eq!(findings.len(), 2, "one finding per vulnerability record");
        assert!(
            findings[0].title.contains("SQL Injection"),
            "title carries the record title: {}",
            findings[0].title
        );
        assert!(
            findings[0].body.contains("high") && findings[0].body.contains("CWE-89"),
            "body carries severity and CWE: {}",
            findings[0].body
        );
        assert!(
            findings[0].evidence.contains("OR '1'='1") && findings[0].evidence.contains("app/auth.py"),
            "evidence carries the PoC and the code location: {}",
            findings[0].evidence
        );
        assert!(
            !findings[1].evidence.is_empty(),
            "a record with no PoC still gets fallback evidence"
        );
    }

    #[test]
    fn sarif_results_become_findings() {
        let output = json!({
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"rules": [{"id": "shannon/injection", "properties": {"cwe": "CWE-89"}}]}},
                "results": [{
                    "ruleId": "shannon/injection",
                    "level": "error",
                    "message": {"text": "SQL injection in /api/search"},
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {"uri": "src/search.ts"},
                            "region": {"startLine": 42},
                        },
                    }],
                }],
            }],
        });
        let findings = parse_sarif(&output);
        assert_eq!(findings.len(), 1, "one finding per SARIF result");
        assert!(
            findings[0].body.contains("high") && findings[0].body.contains("CWE-89"),
            "level error maps to high and the rule carries the CWE: {}",
            findings[0].body
        );
        assert!(
            findings[0].evidence.contains("src/search.ts"),
            "evidence points at the result location: {}",
            findings[0].evidence
        );
    }

    #[test]
    fn malformed_output_is_an_error_not_a_panic() {
        assert!(
            normalize(FindingsFormat::StrixJson, "not json").is_err(),
            "unparseable output is an error"
        );
        assert_eq!(
            normalize(FindingsFormat::StrixJson, "[]").unwrap().len(),
            0,
            "an empty Strix array is no findings"
        );
        assert_eq!(
            normalize(FindingsFormat::Sarif, "{}").unwrap().len(),
            0,
            "a SARIF envelope with no runs is no findings"
        );
    }

    #[test]
    fn the_lock_pins_strix_for_linux_on_each_architecture() {
        let pin = pin_for(LOCK, "strix", "linux", "x86_64").expect("hunters.lock pins strix for x86_64");
        assert_eq!(pin.sha256.len(), 64, "sha256 must be 64 hex characters");
        assert!(
            pin.url.starts_with("https://github.com/usestrix/strix/releases/download/"),
            "unexpected pin url: {}",
            pin.url
        );
        assert!(
            pin_for(LOCK, "strix", "linux", "aarch64").is_some(),
            "hunters.lock pins strix for aarch64"
        );
        assert_eq!(
            pin_for(LOCK, "strix", "linux", "riscv64"),
            None,
            "no pin for an unknown architecture"
        );
        assert_eq!(pin_for(LOCK, "nope", "linux", "x86_64"), None, "no pin for an unknown hunter");
    }

    #[test]
    fn pins_are_linux_only() {
        assert_eq!(
            pin_for(LOCK, "strix", "macos", "aarch64"),
            None,
            "no pin off Linux, even for a known hunter and arch"
        );
        assert_eq!(pin_for(LOCK, "strix", "windows", "x86_64"), None, "no pin off Linux");
        assert!(
            pin_for(LOCK, "strix", "linux", "x86_64").is_some() && pin_for(LOCK, "strix", "linux", "aarch64").is_some(),
            "Linux still resolves both architectures"
        );
    }

    #[test]
    fn a_binary_on_disk_counts_as_not_installed_without_a_pin() {
        let tmp = std::env::temp_dir().join(format!("colonizer-hunters-test-{}", crate::util::short_id()));
        std::fs::create_dir_all(tmp.join("1.6.2")).unwrap();
        std::fs::write(tmp.join("1.6.2/strix"), "").unwrap();
        assert_eq!(
            installed_for(&tmp, None),
            None,
            "a binary file with no pin (e.g. macOS) is not installed"
        );
        let pin = pin_for(LOCK, "strix", "linux", std::env::consts::ARCH).expect("a pin for this test host");
        assert_eq!(
            installed_for(&tmp, Some(&pin)),
            Some("1.6.2".to_string()),
            "the binary marks the pinned version installed where a pin exists"
        );
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_hunter_install_opt_in_needs_a_truthy_value() {
        for truthy in ["1", "true", "TRUE", "on", "On", "yes", "YES", " 1 "] {
            assert!(hunter_install_allowed_for(Some(truthy)), "{truthy:?} must allow installs");
        }
        for falsy in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("off"),
            Some("no"),
            Some("2"),
            Some("maybe"),
        ] {
            assert!(!hunter_install_allowed_for(falsy), "{falsy:?} must not allow installs");
        }
    }

    #[test]
    fn a_hunter_counts_as_installed_only_once_its_binary_is_there() {
        let tmp = std::env::temp_dir().join(format!("colonizer-hunters-test-{}", crate::util::short_id()));
        assert_eq!(installed_in(&tmp, "1.6.2"), None, "nothing on disk is nothing installed");
        std::fs::create_dir_all(tmp.join("1.6.2")).unwrap();
        std::fs::write(tmp.join("1.6.2/strix"), "").unwrap();
        assert_eq!(
            installed_in(&tmp, "1.6.2"),
            Some("1.6.2".to_string()),
            "the binary marks the pinned version installed"
        );
        assert_eq!(installed_in(&tmp, "9.9.9"), None, "a new pin needs its own download");
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn the_probe_never_points_at_the_host_daemon() {
        let (ready, detail) = probe_detail(true, "binary not installed yet; POST /api/hunters/strix/install first", false);
        assert!(!ready, "a hunter needing Docker is not ready");
        assert!(
            !detail.contains("DOCKER_HOST"),
            "the detail must never suggest the host daemon: {detail}"
        );
        assert!(
            detail.contains("never shared"),
            "the detail says the host daemon is never shared: {detail}"
        );
        let (ready, detail) = probe_detail(false, "node runtime not found; install it before running this hunter", true);
        assert!(!ready, "a hunter without its runtime is not ready");
        assert!(detail.contains("node"), "the detail names the missing runtime: {detail}");
        let (ready, detail) = probe_detail(true, "binary not installed yet", true);
        assert!(ready, "runtime without a Docker need is ready");
        assert_eq!(detail, "ready");
    }

    #[test]
    fn strix_records_without_a_body_still_get_one() {
        for output in [json!([{}]), json!([{"title": "only a title"}])] {
            let findings = parse_strix(&output);
            assert_eq!(findings.len(), 1, "one finding per record: {output}");
            assert!(!findings[0].title.trim().is_empty(), "title is non-empty: {output}");
            assert!(
                !findings[0].body.trim().is_empty(),
                "empty records fall back to a summary body: {output}"
            );
            assert!(!findings[0].evidence.trim().is_empty(), "evidence is non-empty: {output}");
        }
    }

    #[test]
    fn sarif_bodies_stay_within_the_findings_cap() {
        let long = "x".repeat(30_000);
        let output = json!({
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"rules": []}},
                "results": [{
                    "ruleId": long,
                    "level": "error",
                    "message": {"text": "boom"},
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": {"uri": long},
                            "region": {"startLine": 1},
                        },
                    }],
                }],
            }],
        });
        let findings = parse_sarif(&output);
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].body.chars().count() <= 20_000,
            "the composed body is clipped: {} chars",
            findings[0].body.chars().count()
        );
    }

    #[test]
    fn sarif_blank_rule_ids_still_get_a_title() {
        let output = json!({
            "version": "2.1.0",
            "runs": [{
                "tool": {"driver": {"rules": []}},
                "results": [{"ruleId": "   ", "level": "warning"}],
            }],
        });
        let findings = parse_sarif(&output);
        assert_eq!(findings.len(), 1);
        assert!(
            !findings[0].title.trim().is_empty(),
            "a whitespace ruleId with no message still titles the finding"
        );
    }

    #[test]
    fn builtin_lists_strix_and_shannon_with_their_licences() {
        assert_eq!(builtin().len(), 2, "strix plus the shannon stub");
        let strix = find("strix").expect("strix is built in");
        assert_eq!(strix.licence, "Apache-2.0");
        assert!(strix.available, "strix ships a parser and a pin");
        let shannon = find("shannon").expect("shannon is built in");
        assert_eq!(shannon.licence, "AGPL-3.0");
        assert!(!shannon.available, "shannon is a manifest-only stub");
        assert!(find("nope").is_none(), "unknown hunters stay unknown");
    }

    // The install tests below drive `install_pinned` against a local axum server on 127.0.0.1:0
    // serving a tar.gz built by shelling out to `tar` over a temp dir containing a `strix` file.

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-hunters-{label}-{}", crate::util::short_id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn make_tarball(payload: &[u8]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("colonizer-hunters-tar-{}", crate::util::short_id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/strix"), payload).unwrap();
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join("strix.tar.gz"))
            .arg("-C")
            .arg(dir.join("src"))
            .arg("strix")
            .status()
            .expect("tar must exist for the test");
        assert!(status.success(), "tar builds the test archive");
        let bytes = std::fs::read(dir.join("strix.tar.gz")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        bytes
    }

    fn test_pin(url: String, bytes: &[u8]) -> Pin {
        Pin {
            id: "strix".into(),
            version: "9.9.9".into(),
            sha256: hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref()),
            url,
        }
    }

    async fn serve(router: axum::Router) -> std::net::SocketAddr {
        serve_with(|_| router).await
    }

    async fn serve_with(build: impl FnOnce(std::net::SocketAddr) -> axum::Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("loopback binds");
        let addr = listener.local_addr().expect("a local addr");
        let router = build(addr);
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("the test server serves");
        });
        addr
    }

    fn version_entries(root: &FsPath) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    }

    #[tokio::test]
    async fn concurrent_installs_download_once_and_leave_no_leftovers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let payload = b"fake-strix-binary";
        let archive = make_tarball(payload);
        let served = archive.clone();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let router = axum::Router::new().route(
            "/file",
            axum::routing::get(move || {
                let hits = hits.clone();
                let served = served.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    served
                }
            }),
        );
        let addr = serve(router).await;
        let pin = test_pin(format!("http://{addr}/file"), &archive);
        let root = test_root("race");

        let (first, second) = tokio::join!(
            install_pinned(&root, &pin, MAX_DOWNLOAD_BYTES),
            install_pinned(&root, &pin, MAX_DOWNLOAD_BYTES)
        );
        assert_eq!(first.unwrap(), "9.9.9");
        assert_eq!(second.unwrap(), "9.9.9");
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the second install re-checks instead of downloading"
        );
        assert_eq!(version_entries(&root), ["9.9.9"], "only the version dir lands in the root");
        let binary = root.join("9.9.9/strix");
        assert_eq!(
            std::fs::read(&binary).unwrap(),
            payload,
            "the installed binary is the served one"
        );
        assert_eq!(
            std::fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
            0o555,
            "the installed binary is read-execute only"
        );
        assert_eq!(
            version_entries(&root.join("9.9.9")),
            ["strix"],
            "no temp leftovers in the version dir"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn an_oversized_content_length_is_refused_before_installing() {
        let archive = make_tarball(b"too big for the test cap");
        let served = archive.clone();
        let router = axum::Router::new().route(
            "/file",
            axum::routing::get(move || {
                let served = served.clone();
                async move { served }
            }),
        );
        let addr = serve(router).await;
        let pin = test_pin(format!("http://{addr}/file"), &archive);
        let root = test_root("sizecap");

        let err = install_pinned(&root, &pin, 16).await.expect_err("the cap refuses");
        assert!(
            err.to_string().contains("Content-Length"),
            "the Content-Length check fires before the body is read: {err:#}"
        );
        assert!(!root.join("9.9.9").exists(), "nothing is installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_chunked_body_past_the_cap_is_refused_while_streaming() {
        // No Content-Length here, so the running-total check is the one that fires.
        let router = axum::Router::new().route(
            "/file",
            axum::routing::get(|| async {
                let chunks = vec![Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(vec![0u8; 64])); 4];
                axum::body::Body::from_stream(futures_util::stream::iter(chunks))
            }),
        );
        let addr = serve(router).await;
        let pin = Pin {
            id: "strix".into(),
            version: "9.9.9".into(),
            sha256: "0".repeat(64),
            url: format!("http://{addr}/file"),
        };
        let root = test_root("streamsize");

        let err = install_pinned(&root, &pin, 16).await.expect_err("the cap refuses");
        assert!(
            err.to_string().contains("exceeds the cap"),
            "the streaming check fires: {err:#}"
        );
        assert!(!root.join("9.9.9").exists(), "nothing is installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_checksum_mismatch_installs_nothing() {
        let archive = make_tarball(b"bytes the pin does not expect");
        let served = archive.clone();
        let router = axum::Router::new().route(
            "/file",
            axum::routing::get(move || {
                let served = served.clone();
                async move { served }
            }),
        );
        let addr = serve(router).await;
        let pin = Pin {
            id: "strix".into(),
            version: "9.9.9".into(),
            sha256: "0".repeat(64),
            url: format!("http://{addr}/file"),
        };
        let root = test_root("checksum");

        let err = install_pinned(&root, &pin, MAX_DOWNLOAD_BYTES)
            .await
            .expect_err("the mismatch refuses");
        assert!(
            err.to_string().contains("checksum mismatch"),
            "the refusal names the checksum: {err:#}"
        );
        assert!(!root.join("9.9.9").exists(), "nothing is installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_redirect_off_https_is_refused() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let followed = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = followed.clone();
        let addr = serve_with(|addr| {
            let target = format!("http://{addr}/other");
            axum::Router::new()
                .route(
                    "/redir",
                    axum::routing::get(move || {
                        let target = target.clone();
                        async move {
                            (
                                axum::http::StatusCode::FOUND,
                                [(
                                    axum::http::header::LOCATION,
                                    axum::http::HeaderValue::from_str(&target).expect("a valid Location"),
                                )],
                                "",
                            )
                        }
                    }),
                )
                .route(
                    "/other",
                    axum::routing::get(move || {
                        let followed = followed.clone();
                        async move {
                            followed.fetch_add(1, Ordering::SeqCst);
                            "must never be fetched"
                        }
                    }),
                )
        })
        .await;
        let pin = Pin {
            id: "strix".into(),
            version: "9.9.9".into(),
            sha256: "0".repeat(64),
            url: format!("http://{addr}/redir"),
        };
        let root = test_root("redirect");

        let err = install_pinned(&root, &pin, MAX_DOWNLOAD_BYTES)
            .await
            .expect_err("the http redirect is refused");
        assert!(
            err.to_string().contains("302"),
            "the refusal reports the redirect status, not a checksum mismatch: {err:#}"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 0, "the redirect target is never fetched");
        assert!(!root.join("9.9.9").exists(), "nothing is installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn the_install_route_needs_the_operator_opt_in() {
        use axum::response::IntoResponse;
        let root = test_root("optin");
        let app = crate::tests::test_app(&root);

        let denied = install_handler_inner(State(app.clone()), Path("strix".to_string()), false).await;
        let denied = denied.expect_err("disabled installs are refused");
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(denied.into_response().into_body(), 1024)
            .await
            .expect("a body");
        assert!(
            String::from_utf8_lossy(&body).contains(INSTALL_ENV_VAR),
            "the refusal names the env var: {}",
            String::from_utf8_lossy(&body)
        );
        let unknown = install_handler_inner(State(app.clone()), Path("nope".to_string()), false).await;
        assert_eq!(
            unknown.expect_err("unknown hunters stay unknown").status(),
            StatusCode::NOT_FOUND
        );

        let stub = install_handler_inner(State(app.clone()), Path("shannon".to_string()), true).await;
        assert_eq!(
            stub.expect_err("the stub still refuses").status(),
            StatusCode::BAD_REQUEST,
            "opted in, the next check runs — without any network I/O"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }
}
