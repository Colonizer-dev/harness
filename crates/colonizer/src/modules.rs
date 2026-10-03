//! Module registry: which providers exist for each module kind, their settings schemas, and the
//! Settings → Modules API.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModuleChoice, Settings},
    sandbox::Secret,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};

pub const KINDS: [&str; 15] = [
    "source",
    "sandbox",
    "mesh",
    "agent",
    "interfaces",
    "publish",
    "memory",
    "watchdog",
    "resume",
    "autonomy",
    "notify",
    "burn_down",
    "voice",
    "screen",
    "observability",
];

/// An agent module discovered from `modules/agents/<id>/module.json` in the app assets.
#[derive(Clone, Debug)]
pub struct AgentModule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub entry: Vec<String>,
    pub needs_claude: bool,
    /// The module's declared secrets for a [`VENDOR_KEYS`] host: what a stored vendor key is pushed
    /// into the colony under (issue #629).
    pub vendor_secrets: Vec<DeclaredSecret>,
    pub schema: Value,
    /// The manifest's `requires` declaration; third-party modules may omit the section.
    pub requires: Requires,
    /// The manifest's `egress` declaration; third-party modules may omit the section.
    pub egress: Option<Egress>,
    /// The manifest's `session_resume.dir` — a path inside the VM where the runner keeps agent
    /// session transcripts — when the agent can pick an old conversation back up (`None` when it
    /// cannot). The harness mounts a host directory over the path so transcripts survive a stopped
    /// microVM, and a suspended colony's answer resumes the same agent session (issue #562).
    pub resume_dir: Option<String>,
    /// Whether the runner serves the loop MCP tools `loop_next` and `loop_stop` (issue #643),
    /// declared as `"loop_tools": true` in the manifest. A loop's brief only names the tools when
    /// the module it launches on declares them.
    pub loop_tools: bool,
}

/// The manifest's `requires` declaration (issue #633): the binaries a colony needs on its `PATH`,
/// the ones the runner fetches itself, and the versions pinned per binary or package. A pin keyed
/// by a package name that is not a required binary (acp pins `@google/gemini-cli` for the `gemini`
/// binary) is carried as declared and simply matches no binary in the preflight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Requires {
    pub binaries: Vec<String>,
    pub pins: BTreeMap<String, Pin>,
    pub fetched_by_runner: Vec<String>,
}

/// A pinned version, and the install command or script to get it when the manifest names one. The
/// other pin fields (`source_rev`, `integrity`, `source`) are the runners' to read and ignored here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub version: String,
    pub install: Option<String>,
}

/// The fixed network hosts an agent module's runner needs, declared under `egress` in `module.json`
/// as four optional arrays of bare hostnames (a leading `*.` wildcard allowed): the vendor's API,
/// login and telemetry hosts, and everything else fixed. A colony in `allowlist` mode adds `api`,
/// `auth` and `extra` to its allow list (#601); `telemetry` is never added — an operator who wants
/// a telemetry host lists it in `egress_allow` themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Egress {
    pub api: Vec<String>,
    pub auth: Vec<String>,
    pub telemetry: Vec<String>,
    pub extra: Vec<String>,
}

impl Egress {
    /// Every declared host, deduped, in declaration order.
    pub fn hosts(&self) -> Vec<String> {
        let mut hosts = Vec::new();
        for host in self.api.iter().chain(&self.auth).chain(&self.telemetry).chain(&self.extra) {
            if !hosts.contains(host) {
                hosts.push(host.clone());
            }
        }
        hosts
    }

    /// Whether a request host is declared, exactly or under a `*.suffix` wildcard (subdomains only).
    pub fn covers(&self, host: &str) -> bool {
        self.hosts().iter().any(|declared| match declared.strip_prefix("*.") {
            Some(suffix) => host.strip_suffix(suffix).is_some_and(|prefix| prefix.ends_with('.')),
            None => declared == host,
        })
    }
}

/// Parses the optional `egress` section: only the four known categories, bare hostnames only, so a
/// typo is a manifest problem rather than a silently narrower allowlist later.
fn parse_egress(manifest: &Value) -> Result<Option<Egress>, String> {
    let Some(section) = manifest.get("egress") else {
        return Ok(None);
    };
    let Some(map) = section.as_object() else {
        return Err("egress must be an object of host categories (api, auth, telemetry, extra)".into());
    };
    for key in map.keys() {
        if !matches!(key.as_str(), "api" | "auth" | "telemetry" | "extra") {
            return Err(format!(
                "egress: unknown category \"{key}\"; known: api, auth, telemetry, extra"
            ));
        }
    }
    let list = |key: &str| -> Result<Vec<String>, String> {
        let Some(value) = map.get(key) else { return Ok(Vec::new()) };
        let items = value
            .as_array()
            .ok_or(format!("egress.{key} must be an array of hostnames"))?;
        items
            .iter()
            .enumerate()
            .map(|(i, item)| match item.as_str() {
                Some(host) if is_bare_hostname(host) => Ok(host.to_string()),
                _ => Err(format!("egress.{key}[{i}]: {item} is not a bare hostname")),
            })
            .collect()
    };
    let (api, auth, telemetry, extra) = (list("api")?, list("auth")?, list("telemetry")?, list("extra")?);
    Ok(Some(Egress {
        api,
        auth,
        telemetry,
        extra,
    }))
}

/// One entry of a manifest's `secrets` section, as it names it: the env vars the runner reads the
/// key from and the hosts each is good for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredSecret {
    pub env: Vec<String>,
    pub hosts: Vec<String>,
}

/// The vendors whose key a module's runner may need on the wire itself (issue #629), by the host a
/// manifest declares: the gateway provider id whose stored key applies, and the mothership env to
/// fall back to when it has none. Keyed by host rather than module id, so a third-party module that
/// declares the same host gets the same push.
pub(crate) const VENDOR_KEYS: &[(&str, &str, &str)] = &[
    ("api.openai.com", "openai", "OPENAI_API_KEY"),
    ("api.x.ai", "xai-grok", "XAI_API_KEY"),
];

/// The secrets a module's colonies boot with for the gateway's vendor keys (issue #629): the stored
/// gateway key of the provider each declared host maps to, else the mothership env named for it,
/// under the module's own env names and hosts. Nothing configured means no secret — never a boot
/// failure. `stored` and `env` are injected lookups so the unit test needs no process state, and
/// `taken` holds env names a colony secret already grants, which keep the operator's own value.
pub fn vendor_boot_secrets(
    module: &AgentModule,
    stored: &dyn Fn(&str) -> Option<String>,
    env: &dyn Fn(&str) -> Option<String>,
    taken: &[String],
) -> Vec<Secret> {
    let mut out = Vec::new();
    for (host, provider, fallback) in VENDOR_KEYS {
        let Some(declared) = module.vendor_secrets.iter().find(|s| s.hosts.iter().any(|h| h == host)) else {
            continue;
        };
        let Some(value) = stored(provider).or_else(|| env(fallback).filter(|v| !v.trim().is_empty())) else {
            continue;
        };
        for name in &declared.env {
            if taken.contains(name) {
                continue;
            }
            out.push(Secret {
                env: name.clone(),
                value: value.clone(),
                hosts: declared.hosts.clone(),
            });
        }
    }
    out
}

/// A bare hostname: labels of letters, digits and hyphens, none empty or hyphen-led; no scheme,
/// port or path, and exactly one leading `*.` wildcard allowed.
fn is_bare_hostname(host: &str) -> bool {
    host.strip_prefix("*.").unwrap_or(host).split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Parses the optional `requires` section (issue #633). Only the shapes the preflight reads are
/// held to anything — a malformed `binaries`, `pins` or `fetched_by_runner` is a manifest problem,
/// the same as a broken `egress` — while `image` stays the free-text description it has always
/// been and any other key or pin field is carried or ignored, so a module declaring more than this
/// file knows still loads.
fn parse_requires(manifest: &Value) -> Result<Requires, String> {
    let Some(section) = manifest.get("requires") else {
        return Ok(Requires::default());
    };
    let Some(map) = section.as_object() else {
        return Err("requires must be an object".into());
    };
    let strings = |key: &str| -> Result<Vec<String>, String> {
        let Some(value) = map.get(key) else { return Ok(Vec::new()) };
        value
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(String::from))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or(format!("requires.{key} must be an array of strings"))
    };
    let mut pins = BTreeMap::new();
    if let Some(value) = map.get("pins") {
        let entries = value
            .as_object()
            .ok_or("requires.pins must be an object of package or binary name to pin")?;
        for (name, pin) in entries {
            let Some(version) = pin.get("version").and_then(Value::as_str) else {
                return Err(format!("requires.pins.{name} must name a string \"version\""));
            };
            pins.insert(
                name.clone(),
                Pin {
                    version: version.to_string(),
                    install: pin.get("install").and_then(Value::as_str).map(String::from),
                },
            );
        }
    }
    Ok(Requires {
        binaries: strings("binaries")?,
        pins,
        fetched_by_runner: strings("fetched_by_runner")?,
    })
}

/// The Claude Code build the harness stages as `bin/claude-guest`, pinned in crates/colonizer/claude-code.lock
/// (same six columns as images.lock: name, version, platform, kind, sha256, url). Compiled in, so
/// the pin always matches the harness that was built.
const CLAUDE_LOCK: &str = include_str!("../claude-code.lock");

/// The version the lock pins the Claude Code guest build to: its `agent` rows, whose version is the
/// same on both platforms. `None` when the lock names no build, which only a hand-edited tree causes.
fn claude_lock_version(lock: &str) -> Option<&str> {
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["claude-code", version, _platform, "agent", _sha256, _url] => Some(*version),
            _ => None,
        })
}

/// A binary the harness itself stages into every colony that needs it, with the version staged when
/// the harness knows one: its own vendored build. `claude` is the only one today — mounted from the
/// vendored `bin/claude-guest` when that is installed (app.rs `resolve_guest_claude_bin`), else the
/// host's own Claude Code, whose version the harness does not know and so does not pin-check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedBinary {
    pub name: String,
    pub version: Option<String>,
}

/// What the harness stages, read off the config: `claude`, at the version crates/colonizer/claude-code.lock
/// pins when the vendored guest build is present.
pub fn harness_staged_binaries(cfg: &crate::config::Settings) -> Vec<StagedBinary> {
    vec![StagedBinary {
        name: "claude".into(),
        version: cfg
            .asset("bin/claude-guest")
            .is_ok()
            .then(|| claude_lock_version(CLAUDE_LOCK))
            .flatten()
            .map(str::to_string),
    }]
}

/// Whether an image reference is one of the stock presets, bare tag or `@sha256:`-pinned: the
/// images the harness itself offers, which carry no agent CLIs. Anything else is a custom image the
/// operator chose.
fn is_stock_image(image: &str) -> bool {
    crate::presets::PRESETS
        .iter()
        .any(|p| image == p.image || image.strip_prefix(&format!("{}@sha256:", p.image)).is_some())
}

/// The base tools every stock preset carries, so a module requiring one is not refused out of a
/// stock image that in fact has it. The intersection across the presets, verified against the
/// Debian bookworm layers they are built on: `sh`, `bash`, `tar` and `gzip` are in the Debian base
/// under all of them, and `wget` is in the buildpack-deps layer node, python and go share and is
/// installed by the rust image itself. Deliberately smaller than what any one preset carries:
/// `git`, `curl` and `ssh` are absent from the rust preset (debian-slim installs neither), and
/// `gcc` and `make` from the go preset — a module requiring one of those is told to pick an image
/// that surely has it — as are the per-preset language runtimes.
const STOCK_IMAGE_TOOLS: [&str; 5] = ["bash", "gzip", "sh", "tar", "wget"];

/// The preflight behind launch and boot (issue #633): every binary a module's `requires.binaries`
/// names must be available to the colony before one starts. A binary the runner fetches itself is
/// available by construction; one the harness stages is available unless the module pins it at a
/// version other than the staged one; anything else has to come from the colony image, and on the
/// stock presets only the base tools every preset carries ([`STOCK_IMAGE_TOOLS`]) is taken as
/// carried — anything else is refused naming the way out, even where some preset happens to ship
/// it, because the check does not model each preset's contents. A custom `sandbox.image` is the
/// operator's word — trusted here, still checked inside the VM by the runner's own preflight.
pub fn check_requires(agent: &AgentModule, image: &str, staged: &[StagedBinary]) -> Result<(), String> {
    for binary in &agent.requires.binaries {
        if agent.requires.fetched_by_runner.contains(binary) {
            continue;
        }
        let pin = agent.requires.pins.get(binary);
        if let Some(staged_binary) = staged.iter().find(|staged| &staged.name == binary) {
            if let (Some(staged_version), Some(pin)) = (&staged_binary.version, pin)
                && staged_version != &pin.version
            {
                return Err(format!(
                    "agent module `{}` pins the `{binary}` binary at {}, but the harness stages {}; \
                     set the module's pin to {staged_version}, or boot an image with {binary} {} on PATH",
                    agent.id, pin.version, staged_version, pin.version
                ));
            }
            continue;
        }
        if !is_stock_image(image) || STOCK_IMAGE_TOOLS.contains(&binary.as_str()) {
            continue;
        }
        let pinned = pin.map(|pin| format!(" (pinned {})", pin.version)).unwrap_or_default();
        let install = pin
            .and_then(|pin| pin.install.as_deref())
            .map(|install| format!(" (install: {install})"))
            .unwrap_or_default();
        return Err(format!(
            "agent module `{}` needs the `{binary}` binary{pinned}, which the harness does not stage \
             and the stock preset images ({image} among them) do not all carry; set the sandbox \
             module's image to one with {binary} on PATH{install}",
            agent.id
        ));
    }
    Ok(())
}

impl AgentModule {
    /// The runner command as seen inside the VM, where the module is mounted at `/opt/colonizer/agent`.
    pub fn vm_command(&self) -> Vec<String> {
        self.entry
            .iter()
            .map(|arg| {
                if self.dir.join(arg).exists() {
                    format!("/opt/colonizer/agent/{arg}")
                } else {
                    arg.clone()
                }
            })
            .collect()
    }
}

/// Test-only construction of `AgentModule` (issue #707): every field starts at a neutral default in
/// this one place, so adding a field means editing here rather than every test helper that builds a
/// module. Chain the setters for what a test actually varies, or mutate the fields directly. The
/// manifest parser (`read_agent`) keeps its struct literal on purpose: it is the one production
/// place that must name every field.
#[cfg(test)]
impl AgentModule {
    /// A module with the given `id` and neutral defaults everywhere else (`name` follows the id).
    pub(crate) fn test(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            dir: PathBuf::new(),
            entry: Vec::new(),
            needs_claude: false,
            vendor_secrets: Vec::new(),
            schema: json!({}),
            requires: Requires::default(),
            egress: None,
            resume_dir: None,
            loop_tools: false,
        }
    }

    pub(crate) fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub(crate) fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub(crate) fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = dir.into();
        self
    }

    pub(crate) fn entry(mut self, entry: Vec<String>) -> Self {
        self.entry = entry;
        self
    }

    pub(crate) fn needs_claude(mut self, needs_claude: bool) -> Self {
        self.needs_claude = needs_claude;
        self
    }

    pub(crate) fn vendor_secrets(mut self, vendor_secrets: Vec<DeclaredSecret>) -> Self {
        self.vendor_secrets = vendor_secrets;
        self
    }

    pub(crate) fn schema(mut self, schema: Value) -> Self {
        self.schema = schema;
        self
    }

    pub(crate) fn requires(mut self, requires: Requires) -> Self {
        self.requires = requires;
        self
    }

    pub(crate) fn egress(mut self, egress: Option<Egress>) -> Self {
        self.egress = egress;
        self
    }

    pub(crate) fn resume_dir(mut self, resume_dir: Option<String>) -> Self {
        self.resume_dir = resume_dir;
        self
    }
}

/// Agent modules discovered under `modules/agents`, plus one problem per manifest that is there but
/// unusable. A broken manifest is a misconfiguration, so it is named rather than silently skipped.
pub fn discover_agents(assets: Option<&FsPath>) -> (Vec<AgentModule>, Vec<String>) {
    let Some(root) = assets else { return (Vec::new(), Vec::new()) };
    let Ok(entries) = std::fs::read_dir(root.join("modules/agents")) else {
        return (Vec::new(), Vec::new());
    };
    let mut modules = Vec::new();
    let mut problems = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("module.json");
        // A directory without a manifest is not a module; one with a broken manifest is.
        if !path.is_file() {
            continue;
        }
        match read_agent(&path) {
            Ok(module) => modules.push(module),
            Err(e) => {
                let problem = format!("{}: {e}", path.display());
                eprintln!("modules: {problem}");
                problems.push(problem);
            }
        }
    }
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    problems.sort();
    (modules, problems)
}

fn read_agent(path: &FsPath) -> Result<AgentModule, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let manifest: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let Some(id) = manifest["id"].as_str() else {
        return Err("missing \"id\"".into());
    };
    let entry_cmd: Vec<String> = manifest["entry"]
        .as_array()
        .and_then(|args| args.iter().map(|a| a.as_str().map(String::from)).collect::<Option<Vec<_>>>())
        .filter(|args| !args.is_empty())
        .ok_or("\"entry\" must be a non-empty array of strings")?;
    let secrets = manifest["secrets"].to_string();
    let requires = parse_requires(&manifest)?;
    let egress = parse_egress(&manifest)?;
    // An agent that declares session resumability must name the directory, or a suspended colony
    // would later boot with its transcript nowhere to be found: name the manifest problem now.
    let resume_dir = match manifest.get("session_resume") {
        None => None,
        Some(section) => Some(
            section["dir"]
                .as_str()
                .filter(|dir| !dir.is_empty() && dir.starts_with('/'))
                .ok_or("\"session_resume\" must name an absolute in-VM transcript directory as \"dir\"")?
                .to_string(),
        ),
    };
    // A loop-tools flag that is anything but a boolean would quietly strip a loop's colony of the
    // tools its brief goes on to promise, so name the manifest problem now.
    let loop_tools = match manifest.get("loop_tools") {
        None => false,
        Some(value) => value.as_bool().ok_or("\"loop_tools\" must be a boolean")?,
    };
    // A declared egress omitting a host its secrets are for would have the allowlist (#304) break
    // the requests those secrets authenticate; with no section, nothing is held to this.
    if let Some(egress) = &egress {
        let mut hosts = manifest["secrets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|secret| secret["hosts"].as_array())
            .flatten()
            .filter_map(Value::as_str);
        if let Some(host) = hosts.find(|host| !egress.covers(host)) {
            return Err(format!("secrets host \"{host}\" is not in the egress declaration"));
        }
    }
    Ok(AgentModule {
        id: id.to_string(),
        name: manifest["name"].as_str().unwrap_or_default().to_string(),
        description: manifest["description"].as_str().unwrap_or_default().to_string(),
        entry: entry_cmd,
        needs_claude: requires.binaries.iter().any(|binary| binary == "claude") || secrets.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        vendor_secrets: vendor_secrets(&manifest),
        schema: normalize_schema(&manifest["settings"]),
        dir: path.parent().map(FsPath::to_path_buf).unwrap_or_default(),
        requires,
        egress,
        resume_dir,
        loop_tools,
    })
}

/// Accepts either a full `{type: object, properties}` schema or a bare properties map.
fn normalize_schema(value: &Value) -> Value {
    if value["properties"].is_object() {
        value.clone()
    } else if value.is_object() {
        json!({"type": "object", "properties": value})
    } else {
        json!({"type": "object", "properties": {}})
    }
}

/// The manifest's `secrets` entries that name a [`VENDOR_KEYS`] host — the ones a stored vendor key
/// is pushed under (issue #629). Anything else a module declares for its own hosts is not the
/// gateway's to fill.
fn vendor_secrets(manifest: &Value) -> Vec<DeclaredSecret> {
    let strings = |value: &Value| {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect::<Vec<_>>()
    };
    manifest["secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|secret| DeclaredSecret {
            env: strings(&secret["env"]),
            hosts: strings(&secret["hosts"]),
        })
        .filter(|secret| {
            secret
                .hosts
                .iter()
                .any(|host| VENDOR_KEYS.iter().any(|(known, _, _)| known == host))
        })
        .collect()
}

pub struct Provider {
    pub id: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// Agent kind only: whether the module's runner serves the loop tools (`loop_tools`, #643).
    pub loop_tools: bool,
}

pub fn providers(kind: &str, agents: &[AgentModule]) -> Vec<Provider> {
    let p = |id: &str, name: &str, description: &str, schema: Value| Provider {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        schema,
        loop_tools: false,
    };
    match kind {
        "source" => vec![p(
            "github",
            "GitHub",
            "Issues from repositories your GitHub account can access",
            json!({"type":"object","properties":{
                "include_labels": {
                    "type": "string", "title": "Only issues labelled",
                    "description": "Comma-separated labels, e.g. 'ready, colonize'. An issue is offered for a colony only if it carries at least one of them. Empty offers every open issue.",
                    "default": ""
                },
                "exclude_labels": {
                    "type": "string", "title": "Never issues labelled",
                    "description": "Comma-separated labels, e.g. 'blocked, wontfix'. An issue carrying any of them is never offered, whatever else it carries.",
                    "default": ""
                }
            }}),
        )],
        "sandbox" => vec![p(
            "microsandbox",
            "microsandbox",
            "Rootless libkrun microVMs with per-VM TLS secret injection",
            json!({"type": "object", "properties": {
                "preset": {
                    "type": "string", "title": "Stack",
                    "description": "Picks the image and machine size for a colony. Every preset is glibc-based, which the agent binary requires. Automatic reads the stack off each repository's own marker files when the colony's worktree is checked out (a 'Cargo.toml' makes it Rust), a marker at the repository root beats one in a subdirectory, and a repository that names no stack falls back to Node. Choose 'custom' to set the fields below yourself; anything you set explicitly wins over the preset either way.",
                    "enum": crate::presets::ids(), "default": crate::presets::AUTO
                },
                // The schema default is what an install boots before any repository is in hand, so
                // `auto` lands on its fallback here; detection is a per-colony decision made later.
                "image": {"type": "string", "title": "Image", "description": "glibc-based OCI image with the tools your projects need, pinned by digest. Set by the stack unless you change it.", "default": crate::presets::pinned_image(crate::presets::AUTO_FALLBACK)},
                "cpus": {"type": "integer", "title": "vCPUs", "minimum": 1, "maximum": 64, "default": 4},
                "memory": {"type": "string", "title": "Memory", "description": "e.g. 8G", "default": "8G"},
                "root_disk": {"type": "string", "title": "Root disk", "default": "16G"},
                "max_duration": {"type": "string", "title": "Max session length", "description": "e.g. 8h", "default": "8h"},
                "max_parallel": {"type": "integer", "title": "Parallel sessions", "minimum": 1, "maximum": 32, "default": 3},
                "repo_max_parallel": {"type": "integer", "title": "Parallel sessions per repository", "minimum": 1, "maximum": 32, "default": 3,
                    "description": "Live colonies one repository may run at once, on top of the overall limit above and any org's own. An org can set its own figure in its settings."},
                "egress": {"type": "string", "title": "Egress policy", "enum": ["open", "allowlist"], "default": "open",
                    "description": "What a colony's microVM may reach. Open is today's fence: any public destination, with network-internal addresses, cloud metadata and msb's classifier gaps always denied on top. Allowlist denies all egress except what the harness itself needs (model gateway, TLS-edge secret hosts on 443, mesh control, DNS) plus the allow list below. Either way the always-blocked set cannot be reopened, and a change reaches a colony when it next boots — msb's policy is fixed per sandbox, so a running colony is not rewritten; stop it and press Resume. An org can pin its own mode in its settings."},
                "egress_allow": {"type": "string", "title": "Egress allow list", "default": "", "format": "egress-entries",
                    "description": "Comma-separated host[:port] entries a colony may reach on top of what the harness allows by construction: api.anthropic.com:443, 10.0.0.0/8, [2001:db8::/32]. A host is a name (lowercase, at least two labels; *.example.com covers a whole domain), an IPv4, a bracketed IPv6, or a CIDR; the port is TCP and 1-65535, with :0 or no port meaning every port. Entries compile after the always-blocked denies, so nothing here can reopen a blocked destination. An org's list adds to this one."},
                "egress_block": {"type": "string", "title": "Egress block list", "default": "", "format": "egress-entries",
                    "description": "Comma-separated host[:port] entries a colony is denied, on top of the always-blocked set, in the same form as the allow list. Blocks win over allows: both are the operator's word, and the deny is the cautious reading. An org's list adds to this one."},
                "hold_timeout_minutes": {"type": "integer", "title": "Held colony timeout (minutes)", "minimum": 1, "maximum": 1440, "default": 30,
                    "description": "How long a colony waiting on a human (an autopilot hold) keeps its microVM slot before the queue parks it to free the slot: the microVM is removed and the worktree kept, so the colony resumes where it left off. Within the timeout a held colony still counts against the parallel limits."},
                "suspend_waiting": {"type": "boolean", "title": "Suspend colonies waiting on you", "default": true,
                    "description": "When a colony has asked you something and you have not answered within the grace period below, its microVM is torn down to free the slot: the worktree and the agent's session transcript are kept, and answering the question re-boots the colony and continues the conversation where it left off. The question stays open and answerable the whole time. Only agents that can resume their session (Claude Code today) are suspended; anything else keeps running."},
                "suspend_after_minutes": {"type": "integer", "title": "Suspend waiting colonies after (minutes)", "minimum": 1, "maximum": 1440, "default": 10,
                    "description": "How long a colony keeps its microVM after asking you something before the suspension above stops it. Minimum 1."},
                "prewarm_timeout_minutes": {"type": "integer", "title": "Pre-warm timeout (minutes)", "minimum": 1, "maximum": 1440, "default": 5,
                    "description": "When you open a suspended colony's question, the colony is booted for you so the answer lands in a running VM. If no answer arrives within this many minutes of the boot, the colony is suspended again and the slot freed. Minimum 1."},
                "budget_usd": {"type": "number", "title": "Budget per colony (USD)", "minimum": 0, "default": 0,
                    "description": "Dollars one colony may spend on models in total, Claude and every routed provider together. 0, the default, means unlimited: there is no figure that suits every deployment. Providers need pricing set for their routed tokens to count toward it — a provider without pricing, such as a prepaid token plan, costs nothing here, so hold it to the token budget below instead. When a colony passes the budget its next routed request is refused and the colony is stopped on the host with its worktree kept; raise the budget and press Resume to continue."},
                "budget_tokens": {"type": "integer", "title": "Token budget per colony", "minimum": 0, "default": 0,
                    "description": "Tokens one colony may route through the gateway in total, counted whether or not the provider prices them. For prepaid token or coding plans, whose pricing is empty, every routed request costs $0 and the USD budget above can never trip — this budget is what holds them. 0, the default, means unlimited. When a colony passes the budget its next routed request is refused and the colony is stopped on the host with its worktree kept, the same way the USD budget stops one; raise the budget and press Resume to continue."},
                "host_disk": {"type": "string", "title": "Host disk per colony", "default": "0", "format": "disk-size",
                    "description": "How much disk one colony may leave on the host: its worktree, where everything built inside the colony lands, plus its session files and logs. The microVM's own root disk is the Root disk setting above and is not counted here. 0, the default, means unlimited: there is no size that suits every deployment. Measured every few minutes. When a colony passes the quota it is stopped on the host and its worktree is kept; clean up or raise the quota and press Resume to continue."},
                "warn_free_disk": {"type": "string", "title": "Warn below free disk", "default": "10G", "format": "disk-size",
                    "description": "The cockpit warns when the volume holding the data dir has less free space than this. 0 turns the warning off."},
                "min_free_disk": {"type": "string", "title": "Pause the queue below free disk", "default": "5G", "format": "disk-size",
                    "description": "Queued colonies are not started while free space on the data dir's volume is below this; the pause itself deletes nothing and never stops running colonies, and admission resumes by itself when space returns. Below the floor the reclaim sweep (unless off with COLONIZER_RECLAIM=0) also reclaims finished colonies whose work is already pushed without waiting for the retention window; unpushed work is never deleted. 0 turns the floor off."},
                "mask_paths": {"type": "array", "items": {"type": "string"}, "title": "Also mask", "default": [], "format": "mask-path-list",
                    "description": "Extra worktree-relative paths a colony never sees, on top of the built-in masked files (.env, .envrc, .npmrc, .netrc, .git-credentials, .pypirc). One path per entry, e.g. secrets/credentials.json; a trailing / means the whole directory, and matching is at any depth, so .env also covers vendor/lib/.env. Enforced in the guest before the agent starts: the colony gets an empty file or directory instead."},
                "protect_paths": {"type": "array", "items": {"type": "string"}, "title": "Also protect", "default": [], "format": "path-list",
                    "description": "Extra worktree-relative paths a colony may read but never write, on top of the built-in protected paths (.git/config, .git/hooks/, .gitmodules, .claude/, .codex/, .mcp.json, .devcontainer/, .vscode/, .idea/). One path per entry; a trailing / means the whole directory, and matching is at any depth. Enforced in the guest before the agent starts: the colony gets the path read-only."},
                "unmask_paths": {"type": "array", "items": {"type": "string"}, "title": "Unmask (opt out)", "default": [], "format": "path-list",
                    "description": "Paths a colony may see again: each entry is removed from both the masked and the protected sets, built-ins included — an explicit opt-out for repositories that genuinely ship one of them. Every opt-out is logged on the colony at boot."}
            }}),
        )],
        "mesh" => vec![
            p(
                "headscale",
                "Private mesh",
                "Bundled Headscale + Tailscale: every microVM joins a private network with the harness, separate from your own tailnet",
                json!({"type": "object", "properties": {
                    "control_port": {"type": "integer", "title": "Control port (loopback)", "minimum": 1024, "maximum": 65535, "default": 41740},
                    "udp_port": {"type": "integer", "title": "Harness WireGuard UDP port", "minimum": 1024, "maximum": 65535, "default": 41743},
                    "socks_port": {"type": "integer", "title": "Harness SOCKS5 port (loopback)", "minimum": 1024, "maximum": 65535, "default": 41744}
                }}),
            ),
            p(
                "none",
                "Loopback port",
                "No mesh: reach each VM through a published loopback port",
                json!({"type":"object","properties":{}}),
            ),
        ],
        "agent" => agents
            .iter()
            .map(|a| Provider {
                id: a.id.clone(),
                name: a.name.clone(),
                description: a.description.clone(),
                schema: a.schema.clone(),
                loop_tools: a.loop_tools,
            })
            .collect(),
        "interfaces" => vec![p(
            "default",
            "Session panels",
            "Panels shown in the session view",
            json!({"type": "object", "properties": {
                "chat": {"type": "boolean", "title": "Chat with choice cards", "default": true},
                "terminal": {"type": "boolean", "title": "Terminal", "default": true}
            }}),
        )],
        "publish" => vec![p(
            "github-pr",
            "GitHub pull request",
            "Commit on the host, push the branch and open a pull request",
            json!({"type": "object", "properties": {
                "autopilot": {"type": "boolean", "title": "Open the PR automatically", "description": "Default for new colonies: when the agent finishes cleanly and has written its PR description, push its colonizer/ branch and open the pull request. Can be switched off per colony at launch.", "default": true},
                "verify": {"type": "string", "title": "Verify completion claims", "description": "Default for new colonies: when an agent finishes cleanly and has written its PR description, the mothership verifies the claim before autopilot publishes — it checks the described files are on the branch and runs the repository's test command in a fresh sandbox. `auto` (the default) resolves the command from package.json, Cargo.toml or a Makefile on the base branch; `none` records the claim as unverifiable without checking; any other string is the test command itself. Can be overridden per colony at launch.", "default": "auto"},
                "verify_focus": {"type": "string", "title": "Focused checks first", "enum": ["off", "shadow", "act"], "description": "When a diff owes more than one check, the check owning most of the changed files can run first. `shadow` (the default) runs the checks as before and only records, in the data dir's jev_focus.jsonl and the colony's log, which check would have gone first and whether it would have caught the failure sooner; `act` runs it first and stops at its failure; `off` records nothing. A confirmed verdict always needs every check to pass.", "default": "shadow"},
                "draft": {"type": "boolean", "title": "Open as draft", "default": false},
                "file_findings": {"type": "boolean", "title": "File validated findings as issues", "description": "When a colony notices a problem outside its task, its orchestrator has it confirmed and files it as an issue on the same repository, labelled colonizer-finding. Open issues with the same title are not filed again, and one colony files at most five.", "default": true},
                "autofix": {"type": "boolean", "title": "Autofix validated findings", "description": "When a colony files a validated finding, spawn a fix colony for it: a fresh colony whose pull request is reviewed by an independent session before anything merges. Can be switched off per colony at launch.", "default": false},
                "automerge": {"type": "boolean", "title": "Merge fixes whose review passes", "description": "Merge a fix colony's pull request when its independent review passes; requires autofix, since with no fix colonies there is nothing to merge. Can be switched off per colony at launch.", "default": false},
                "merge_train": {"type": "string", "title": "Merge train", "enum": ["off", "on"], "description": "Opt in: a background loop squash-merges colonies' open pull requests, one per repository every two minutes, oldest first, and only when the repository's base branch is green, the pull request contains the base branch's tip, its own checks pass, and every commit is authored by an allowed identity and says nothing forbidden. Drafts, WIP/HOLD marks and failing checks are never merged; a pull request behind its base is caught up once per base commit, a conflicted one is left to the auto-rebase, and the branch is kept whenever another colony is stacked on it. The default for every repository; the overrides below switch it per owner or repository.", "default": "off"},
                "merge_train_overrides": {"type": "string", "title": "Merge train per repository", "format": "merge-train-overrides", "description": "Comma-separated `owner=on|off` or `owner/repo=on|off` entries that switch the train per organization or per repository; a repository entry wins over an organization entry, and either wins over the switch above. Example: `acme=on, acme/widget=off`.", "default": ""},
                "merge_train_deny_orgs": {"type": "string", "title": "Organizations the train never merges in", "description": "Comma-separated organization or owner names the train never merges in, whatever the switch above or the overrides say.", "default": ""},
                "merge_train_authors": {"type": "string", "title": "Allowed commit authors", "description": "Comma-separated GitHub logins or email addresses that a pull request's commits may be authored by; a pull request carrying any other author is left open. Empty means the identity this mothership publishes as: the `gh` login and its noreply email, the author of every publish and catch-up commit. Compared case-insensitively.", "default": ""},
                "merge_train_forbid": {"type": "string", "title": "Refuse commit messages containing", "description": "Comma-separated, case-insensitive substrings; a pull request any of whose commit messages contains one is never merged. For example `Co-Authored-By: Claude`.", "default": ""}
            }}),
        )],
        "memory" => vec![
            p(
                "files",
                "Shared memory",
                "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context. Off only lets repo notes through; org and global notes are always reviewed", "default": true},
                    "deja": {"type": "boolean", "title": "Transcript recall (deja)", "description": "After a colony finishes, index its transcripts (secrets scrubbed) into its org's local deja index; later colonies of the same org can recall them with a recall tool. Off by default, and off unless this switch is on; each org can opt in or out separately.", "default": false}
                }}),
            ),
            p(
                "mem0",
                "mem0",
                "Approved notes stored in your mem0 project. Colonies read them exactly as they read files, most relevant to the task first; the key never enters a colony",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context. Off only lets repo notes through; org and global notes are always reviewed", "default": true},
                    "base_url": {"type": "string", "title": "API base URL", "description": "The mem0 Platform API. Self-hosted mem0 serves a different API and is not supported", "default": "https://api.mem0.ai"},
                    "deja": {"type": "boolean", "title": "Transcript recall (deja)", "description": "After a colony finishes, index its transcripts (secrets scrubbed) into its org's local deja index; later colonies of the same org can recall them with a recall tool. Off by default, and off unless this switch is on; each org can opt in or out separately.", "default": false}
                }}),
            ),
        ],
        "watchdog" => vec![p(
            "default",
            "Watchdog",
            "Nudges colonies that stop making progress and flags the ones that need you",
            json!({"type": "object", "properties": {
                "stall_minutes": {"type": "integer", "title": "Nudge after minutes without progress", "minimum": 1, "maximum": 1440, "default": 15},
                "max_nudges": {"type": "integer", "title": "Nudges before flagging", "minimum": 0, "maximum": 20, "default": 3},
                "waiting_minutes": {"type": "integer", "title": "Flag unanswered questions after minutes", "minimum": 1, "maximum": 10080, "default": 30}
            }}),
        )],
        "resume" => vec![p(
            "default",
            "Resume",
            "How a parked colony comes back: what parking does to its microVM, and how resuming re-enters the work",
            json!({"type": "object", "properties": {
                "discard_vm": {"type": "boolean", "title": "Discard the microVM when parking", "default": true,
                    "description": "What parking does to a colony's microVM (quota exhaustion, an expired hold). On, the default, the microVM is torn down and the colony resumes cold: a fresh microVM boots on the kept worktree, re-entering the original instructions plus a compact summary of what was done. Off, the microVM keeps running while the colony parks (its slot is still released), and resume is warm: the idle agent is prompted to continue in the machine it never left. Either way the worktree, the branch and the event log are kept, and a park always persists its record before anything is torn down; if the worktree cannot be verified readable before a discard, the microVM is kept instead, never dropped blind."}
            }}),
        )],
        "autonomy" => vec![
            p(
                "off",
                "Off",
                "Questions wait for you, however long that takes",
                json!({"type": "object", "properties": {}}),
            ),
            p(
                "judge",
                "Judge model",
                "A model answers a colony's questions when nobody does, choosing only among the options the agent offered",
                json!({"type": "object", "properties": {
                    "model": {"type": "string", "title": "Judge model", "description": "Any model you have added in Model providers: provider/model, or a plain id such as fable or opus once one of those providers' base URL host is api.anthropic.com. Judging spends that provider's key, never your Claude login. A frontier model judges best — it is deciding for you, on less context than you have, and the difference shows.", "default": ""},
                    "after_minutes": {"type": "integer", "title": "Answer after minutes unanswered", "description": "How long a question waits for you first. 0 answers as soon as it is asked.", "minimum": 0, "maximum": 1440, "default": 10},
                    "max_answers": {"type": "integer", "title": "Answers per colony", "description": "A colony that keeps asking is one to look at yourself, so the judge stops here and the watchdog flags it.", "minimum": 1, "maximum": 50, "default": 5},
                    "free_text": {"type": "boolean", "title": "Answer questions that have no options", "description": "Off by default: a free-text box is where an automatic answer can do the most damage. With it off, those questions wait for you.", "default": false},
                    "risk_ceiling": {"type": "string", "title": "Answer questions up to this risk", "enum": ["read_only", "workspace_write", "publish_affecting", "credential_adjacent"], "description": "Each runner question carries a risk class. The judge answers only at or below this ceiling; a question above it waits for you however long, with a note in the colony's log saying why. Workspace write means the judge may settle anything confined to the colony's own workspace; publish-affecting touches what other people see; credential-adjacent is anything near a key or a login.", "default": "workspace_write"}
                }}),
            ),
        ],
        "notify" => vec![p(
            "default",
            "Notify",
            "Tells you when a colony needs an answer, stalls, fails or opens a pull request",
            json!({"type": "object", "properties": {
                "on_question": {"type": "boolean", "title": "When a colony asks a question", "description": "A colony that stopped to ask is often the one that most needs you", "default": true},
                "on_attention": {"type": "boolean", "title": "When the watchdog flags a colony", "description": "A colony that stalled or ran out of nudges — the watchdog's flags, not autopilot's", "default": true},
                "on_failed": {"type": "boolean", "title": "When a colony fails", "default": true},
                "on_pull_request": {"type": "boolean", "title": "When a colony opens a pull request", "default": true},
                "on_provider": {"type": "boolean", "title": "When a model provider starts failing", "description": "A provider failing under fan-out does not look like a failing provider — it looks like every colony running slowly, because they all wait on it at once. One line when a provider's failure rate reaches 10% of its requests; announced once, and again only after the rate clearly recovers", "default": true},
                "desktop": {"type": "boolean", "title": "Desktop notifications", "description": "Notify the desktop the mothership runs on. Does nothing over SSH or on a headless machine, and says so once in the log", "default": false},
                "webhook_url": {"type": "string", "title": "Webhook URL", "description": "POSTs a short JSON note per event to an address outside this machine. It carries no repository content — the event, the time, and the colony or provider counters behind it — and it is unsigned unless a signing secret is set in Settings", "default": ""}
            }}),
        )],
        "burn_down" => vec![p(
            "default",
            "Burn down",
            "Maxes out the weekly plan: near the weekly reset it launches bug-hunt colonies, paced across the window, until the allowance is down to whatever reserve you set",
            json!({"type": "object", "properties": {
                "reset_weekday": {"type": "string", "title": "Weekly reset day (UTC)", "description": "The day of the week your plan's allowance resets", "enum": ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"], "default": "Monday"},
                "reset_time": {"type": "string", "title": "Weekly reset time (UTC)", "description": "24-hour HH:MM in UTC", "default": "00:00"},
                "lead_hours": {"type": "number", "title": "Hours before reset to start burning", "description": "How long before the weekly reset burn-down starts launching colonies", "minimum": 1, "maximum": 168, "default": 48},
                "reserve_pct": {"type": "number", "title": "Reserve (percent of allowance)", "description": "Percent of the weekly allowance left untouched when the reset lands", "minimum": 0, "maximum": 90, "default": 5},
                "allowance_usd": {"type": "number", "title": "Weekly allowance (USD, your estimate)", "description": "Your estimate of the weekly plan allowance. Burn-down never launches without it — an invented number would be worse than no number at all"},
                "spend_usd_per_colony": {"type": "number", "title": "Estimated spend per colony (USD)", "description": "What one bug-hunt colony roughly burns, used to pace launches across the window", "minimum": 0.5, "default": 5},
                "max_live": {"type": "integer", "title": "Concurrent live burn-down colonies", "description": "Cap on how many burn-down colonies run at once", "minimum": 1, "maximum": 8, "default": 2},
                "repos": {"type": "string", "title": "Repositories to hunt in", "description": "Comma-separated owner/repo list. Empty means burn-down is not configured and launches nothing", "default": ""},
                "instructions": {"type": "string", "title": "Custom hunt instructions", "description": "When empty, a built-in bug-hunt prompt is used", "default": ""}
            }}),
        )],
        "voice" => crate::voice::module_providers()
            .into_iter()
            .map(|(id, name, description, schema)| p(id, name, description, schema))
            .collect(),
        "observability" => crate::observability::settings::providers(),
        "screen" => vec![p(
            "promptdecode",
            "Prompt screening",
            "Screens the diff and the pull request body for hidden code points before anything is published: a local, deterministic decoder (tag characters, bidi controls, variation selectors) that sends nothing anywhere — see promptdeco.de",
            json!({"type":"object","properties":{
                "publish": {
                    "type": "string", "title": "On findings",
                    "description": "'warn' publishes and lists the findings at the foot of the pull request; 'block' holds the publish — no push, no pull request — until the branch is fixed or you lower this to 'warn'.",
                    "enum": ["off", "warn", "block"], "default": "warn"
                }
            }}),
        )],
        _ => Vec::new(),
    }
}

pub fn schema_for(kind: &str, provider: &str, agents: &[AgentModule]) -> Value {
    providers(kind, agents)
        .into_iter()
        .find(|p| p.id == provider)
        .map(|p| p.schema)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
}

fn describe_kind(kind: &str, choice: &ModuleChoice, app: &App) -> Value {
    let providers = providers(kind, &app.agents);
    let schema = providers
        .iter()
        .find(|p| p.id == choice.provider)
        .map(|p| p.schema.clone())
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    let mut described = json!({
        "kind": kind,
        "provider": choice.provider,
        "enabled": choice.enabled,
        "providers": providers.iter().map(|p| {
            let mut row = json!({"id": p.id, "name": p.name, "description": p.description});
            // Only the agent kind's rows say it: the loop tools are a runner capability (#643).
            if kind == "agent" {
                row["loop_tools"] = json!(p.loop_tools);
            }
            row
        }).collect::<Vec<_>>(),
        "settings": choice.settings,
        "schema": schema,
    });
    if kind == "agent" {
        described["manifest_errors"] = json!(app.agent_problems);
    }
    described
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    let modules = app.modules.read().await;
    // Voice is listed even before it is saved, as the browser it reads as: its settings are the only
    // way to connect a service, so hiding it until then would leave nothing to click.
    let voice_default = ModuleChoice {
        provider: crate::voice::BROWSER.into(),
        enabled: true,
        settings: Map::new(),
    };
    Json(
        KINDS
            .iter()
            .filter_map(|k| {
                let choice = modules.get(k).or((*k == "voice").then_some(&voice_default));
                choice.map(|c| describe_kind(k, c, &app))
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct UpdateModule {
    pub(crate) provider: String,
    #[serde(default = "yes")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) settings: Map<String, Value>,
    /// Observability only: a save that turns on conversation content or thinking must also carry
    /// `"confirm_content": true`. A request field, not a setting — it is validated, never persisted.
    #[serde(default)]
    pub(crate) confirm_content: bool,
}

fn yes() -> bool {
    true
}

/// The kinds a harness is not a harness without; every other kind may be switched off.
pub(crate) fn is_required(kind: &str) -> bool {
    matches!(kind, "source" | "sandbox" | "agent" | "publish" | "resume")
}

pub async fn update(State(app): State<Shared>, Path(kind): Path<String>, Json(req): Json<UpdateModule>) -> ApiResult<Value> {
    let providers = providers(&kind, &app.agents);
    let Some(provider) = providers.iter().find(|p| p.id == req.provider) else {
        return Err(client_error(StatusCode::BAD_REQUEST, "unknown module kind or provider"));
    };
    if is_required(&kind) && !req.enabled {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "this module kind is required and can't be disabled",
        ));
    }
    // Keys already stored pass the unknown-key check below: the UI saves back everything
    // modules.json holds (switching providers keeps the previous provider's keys), so refusing
    // those would brick every save after an upgrade until the file was hand-edited.
    let stored = app
        .modules
        .read()
        .await
        .get(&kind)
        .map(|choice| choice.settings.clone())
        .unwrap_or_default();
    let settings = validate_settings(&provider.id, &provider.schema, &req.settings, &stored)
        .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    // A kind with rules the generic checks cannot express (observability today) says so by name
    // here, on the settings this save would leave in place: the keys already stored ride under
    // what the request set, since a save that omits a key does not reset it. `confirm_content` is
    // a request field, passed in and never persisted.
    if kind == "observability" {
        let mut effective = stored.clone();
        for (key, value) in settings.iter() {
            effective.insert(key.clone(), value.clone());
        }
        crate::observability::settings::validate(&provider.id, &stored, &effective, req.enabled, req.confirm_content)
            .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    }
    check_plugin_dirs(&app.cfg, &provider.schema, &settings)
        .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;

    let mut modules = app.modules.write().await;
    let choice = modules
        .get_mut(&kind)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "unknown module kind"))?;
    *choice = ModuleChoice {
        provider: req.provider,
        enabled: req.enabled,
        settings,
    };
    let described = describe_kind(&kind, choice, &app);
    // modules.json writes are serialised by the `app.modules` write guard this handler already
    // holds; the save itself is `write_atomic`, so the file is fsynced into place through a
    // unique temp rather than a fixed `.json.tmp` two savers could share.
    modules.save(&app.modules_file()).await?;
    Ok(Json(described))
}

/// Every skillset a `plugin-dirs` setting names must resolve now, not first fail when a colony boots.
fn check_plugin_dirs(cfg: &Settings, schema: &Value, settings: &Map<String, Value>) -> Result<(), String> {
    let Some(properties) = schema["properties"].as_object() else {
        return Ok(());
    };
    for (key, spec) in properties {
        if spec["format"].as_str() == Some("plugin-dirs")
            && let Some(value) = settings.get(key).and_then(Value::as_str)
        {
            crate::plugins::check_skillsets(cfg, crate::plugins::parse_list(value).iter().map(String::as_str))?;
        }
    }
    Ok(())
}

/// Keeps known keys (plus anything already stored) and checks types, enums and ranges. An unknown
/// key is refused naming it and what the provider does take, never dropped: a setting the operator
/// sent and lost to a typo would otherwise read as the default silently (#326).
pub(crate) fn validate_settings(
    provider: &str,
    schema: &Value,
    input: &Map<String, Value>,
    stored: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    let Some(properties) = schema["properties"].as_object() else {
        return Ok(out);
    };
    for (key, value) in input {
        let Some(spec) = properties.get(key) else {
            if stored.contains_key(key) {
                // A key no schema declares anymore, kept as is: see the `stored` read in `update`.
                out.insert(key.clone(), value.clone());
                continue;
            }
            let mut known: Vec<&str> = properties.keys().map(String::as_str).collect();
            known.sort();
            let known = if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            };
            return Err(format!("`{key}` is not a {provider} setting; known settings: {known}"));
        };
        // A type or range refusal names what the schema asks for (#642), so the fix needs no schema
        // reading: the expected type, or the bounds the value must sit inside, one-sided or both.
        let expected = |want: &str| format!("setting `{key}` must be {want}");
        match spec["type"].as_str() {
            Some("string") if !value.is_string() => return Err(expected("a string")),
            Some("integer") if !value.is_i64() && !value.is_u64() => return Err(expected("an integer")),
            Some("number") if !value.is_number() => return Err(expected("a number")),
            Some("boolean") if !value.is_boolean() => return Err(expected("a boolean")),
            Some("array") if !value.is_array() || !value.as_array().is_some_and(|items| items.iter().all(Value::is_string)) => {
                return Err(expected("an array of strings"));
            }
            _ => {}
        }
        if let Some(options) = spec["enum"].as_array()
            && !options.contains(value)
        {
            let named = options
                .iter()
                .map(|option| option.as_str().map(str::to_string).unwrap_or_else(|| option.to_string()))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!("setting `{key}` must be one of {named}"));
        }
        if let Some(n) = value.as_f64()
            && (spec["minimum"].as_f64().is_some_and(|min| n < min) || spec["maximum"].as_f64().is_some_and(|max| n > max))
        {
            let bounds = match (spec["minimum"].as_f64(), spec["maximum"].as_f64()) {
                (Some(min), Some(max)) => format!("between {min} and {max}"),
                (Some(min), None) => format!("at least {min}"),
                (None, Some(max)) => format!("at most {max}"),
                (None, None) => unreachable!("a range refusal fired, so the schema declared a bound"),
            };
            return Err(format!("setting `{key}` must be {bounds}"));
        }
        if let Some(s) = value.as_str()
            && (s.len() > 500 || s.contains('\n'))
        {
            return Err(format!("setting `{key}` is too long"));
        }
        // A size string is parsed where its quota is enforced, so garbage is refused here, at save time,
        // while the operator is looking — not silently read as no quota at all.
        if spec["format"].as_str() == Some("disk-size")
            && let Some(s) = value.as_str()
            && crate::util::parse_disk_size(s).is_none()
        {
            return Err(format!(
                "setting `{key}` is not a disk size like 512M or 16G (0 means unlimited)"
            ));
        }
        // Egress lists are refused the same way, with the parser's own message: an entry a boot
        // would have to drop should never pass a save unnoticed.
        if spec["format"].as_str() == Some("egress-entries")
            && let Some(s) = value.as_str()
            && let Err(problem) = crate::egress::validate_entries(s)
        {
            return Err(format!("setting `{key}`: {problem}"));
        }
        // The merge train's per-repository overrides, checked with the same parser the train reads
        // them by: a malformed entry is refused here, at save time, never dropped where it matters.
        if spec["format"].as_str() == Some("merge-train-overrides")
            && let Some(s) = value.as_str()
            && let Err(problem) = crate::merge_train::validate_overrides(s)
        {
            return Err(format!("setting `{key}`: {problem}"));
        }
        // A path list is checked entry by entry with the same gate the boot resolves through
        // (path_policy): a path the colony could not honour — absolute, traversal, the worktree
        // root, or a masked reach into `.git` — is refused here, at save time, never discovered
        // from a boot log afterwards.
        if matches!(spec["format"].as_str(), Some("path-list") | Some("mask-path-list"))
            && let Some(items) = value.as_array()
        {
            for item in items {
                let Some(s) = item.as_str() else { continue };
                let checked = if spec["format"].as_str() == Some("mask-path-list") {
                    crate::path_policy::validate_masked(s)
                } else {
                    crate::path_policy::validate_path(s)
                };
                if let Err(e) = checked {
                    return Err(format!("setting `{key}` has an unusable path {s:?}: {e}"));
                }
                if s.len() > 500 {
                    return Err(format!("setting `{key}` has a path over 500 characters"));
                }
            }
        }
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/modules", routing::get(list))
        .route("/api/modules/{kind}", routing::put(update))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModulesConfig;
    use std::sync::Arc;

    #[test]
    fn the_agent_entry_leaves_node_to_path() {
        // The runner command is `["node", "/opt/colonizer/agent/runner.mjs"]`:
        // `node` is not a file in the module directory, so `vm_command` leaves
        // it bare for the VM's `PATH` (`/opt/node/bin` first) to resolve, while
        // the runner script maps to its read-only mount. That split is the
        // premise issue #249's vendored Node runtime exists for.
        let dir = std::env::temp_dir().join(format!("colonizer-vm-command-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
        let module = AgentModule::test("claude-code")
            .dir(dir)
            .entry(vec!["node".into(), "runner.mjs".into()])
            .needs_claude(true);
        let command = module.vm_command();
        assert_eq!(command.first().map(String::as_str), Some("node"), "{command:?}");
        assert_eq!(
            command.get(1).map(String::as_str),
            Some("/opt/colonizer/agent/runner.mjs"),
            "{command:?}"
        );
        // End to end without KVM: that bare `node` is exactly what mounts the
        // vendored runtime (sessions::agent_needs_node over the resolved command).
        assert!(
            crate::sessions::agent_needs_node(&command),
            "a bare `node` entrypoint must mount the vendored runtime: {command:?}"
        );
        std::fs::remove_dir_all(&module.dir).ok();
    }

    #[test]
    fn the_pi_manifest_is_discovered_as_an_agent_that_needs_no_claude() {
        // `modules/agents/pi/module.json` ships beside claude-code's and must ride the
        // same discovery: still a `node` runner to mount, but an agent that reaches
        // models only through the provider gateway — no `claude` binary and no Claude
        // login — so it cannot need Claude, and its one model setting is the whole split.
        const MANIFEST: &str = include_str!("../../../modules/agents/pi/module.json");
        let root = std::env::temp_dir().join(format!("colonizer-pi-manifest-{}", crate::util::short_id()));
        let dir = root.join("modules/agents/pi");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("module.json"), MANIFEST).unwrap();
        std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
        let (modules, problems) = discover_agents(Some(&root));
        assert!(problems.is_empty(), "{problems:?}");
        let pi = modules.iter().find(|m| m.id == "pi").expect("the pi manifest is discovered");
        assert!(!pi.needs_claude, "pi holds no claude binary and no Claude credential");
        assert!(!pi.loop_tools, "pi serves no colonizer MCP server, so no loop tools");
        assert_eq!(pi.vm_command(), ["node", "/opt/colonizer/agent/runner.mjs"]);
        assert_eq!(pi.schema["properties"]["model"]["env"], "COLONIZER_MODEL");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_loop_tools_flag_is_parsed_from_the_manifest() {
        // The loop tools (`loop_next`, `loop_stop`) are a per-module capability: the manifest
        // declares them, and a loop's brief only names them when the module does (issue #643).
        let dir = std::env::temp_dir().join(format!("colonizer-loop-tools-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("module.json");
        let write = |manifest: &str| std::fs::write(&path, manifest).unwrap();

        write(include_str!("../../../modules/agents/claude-code/module.json"));
        assert!(read_agent(&path).unwrap().loop_tools, "claude-code serves the loop tools");
        write(r#"{"id": "x", "entry": ["node", "runner.mjs"], "loop_tools": true}"#);
        assert!(read_agent(&path).unwrap().loop_tools);
        // Anything but a boolean is a manifest problem, not a silent default.
        write(r#"{"id": "x", "entry": ["node", "runner.mjs"], "loop_tools": "yes"}"#);
        assert_eq!(read_agent(&path).unwrap_err(), "\"loop_tools\" must be a boolean");
        // And absent stays the default: no declaration, no loop tools.
        write(r#"{"id": "x", "entry": ["node", "runner.mjs"]}"#);
        assert!(!read_agent(&path).unwrap().loop_tools);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn manifests_that_declare_a_vendor_host_expose_its_secret_for_the_boot_push() {
        // The runner-wire agents (issue #629) each name one vendor host in their manifest's
        // `secrets`: that declaration is the whole trigger for pushing a gateway key, so
        // discovery must distil exactly the vendor-facing entries from whatever else the
        // manifest grants. An agent without a vendor host stays empty.
        const CODEX: &str = include_str!("../../../modules/agents/codex/module.json");
        const GROK: &str = include_str!("../../../modules/agents/grok-build/module.json");
        const PI: &str = include_str!("../../../modules/agents/pi/module.json");
        let root = std::env::temp_dir().join(format!("colonizer-vendor-manifests-{}", crate::util::short_id()));
        for (dir, manifest) in [("codex", CODEX), ("grok-build", GROK), ("pi", PI)] {
            let dir = root.join("modules/agents").join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("module.json"), manifest).unwrap();
            std::fs::write(dir.join("runner.mjs"), "// test fixture").unwrap();
        }
        let (modules, problems) = discover_agents(Some(&root));
        assert!(problems.is_empty(), "{problems:?}");
        let secrets = |id: &str| {
            modules
                .iter()
                .find(|m| m.id == id)
                .expect("the manifest is discovered")
                .vendor_secrets
                .clone()
        };
        assert_eq!(
            secrets("codex"),
            vec![DeclaredSecret {
                env: vec!["CODEX_API_KEY".into()],
                hosts: vec!["api.openai.com".into()],
            }],
            "{:?}",
            secrets("codex")
        );
        assert_eq!(
            secrets("grok-build"),
            vec![DeclaredSecret {
                env: vec!["XAI_API_KEY".into()],
                hosts: vec!["api.x.ai".into()],
            }],
            "{:?}",
            secrets("grok-build")
        );
        assert!(secrets("pi").is_empty(), "pi declares no vendor host");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_vendor_key_is_resolved_stored_first_then_env_and_never_over_a_taken_env() {
        // `Secret` carries a key value and so has no `Debug`; the push is compared as plain rows.
        fn rows(secrets: &[Secret]) -> Vec<(&str, &str, Vec<&str>)> {
            secrets
                .iter()
                .map(|s| (s.env.as_str(), s.value.as_str(), s.hosts.iter().map(String::as_str).collect()))
                .collect()
        }

        // Plain `fn` items, not closures: a closure literal fixes its argument's lifetime, which the
        // higher-ranked `Fn(&str)` the lookups are declared with needs.
        fn stored_openai(id: &str) -> Option<String> {
            (id == "openai").then(|| "sk-test-stored".to_string())
        }
        fn stored_none(_: &str) -> Option<String> {
            None
        }
        fn env_xai_blank(name: &str) -> Option<String> {
            (name == "XAI_API_KEY").then(|| "  ".to_string())
        }
        fn env_xai(name: &str) -> Option<String> {
            (name == "XAI_API_KEY").then(|| "sk-test-env".to_string())
        }
        fn env_none(_: &str) -> Option<String> {
            None
        }

        // The push order the colony sees: a stored gateway key wins over the mothership's own env,
        // a blank or missing stored key falls back to that env, and nothing configured means no
        // secret — never a boot failure. An env name a colony secret already grants is skipped, so
        // the operator's own value keeps precedence without a duplicate flag.
        // The push reads only the manifest's declarations, not the module's id or entry.
        let module = AgentModule::test("codex").vendor_secrets(vec![
            DeclaredSecret {
                env: vec!["CODEX_API_KEY".into()],
                hosts: vec!["api.openai.com".into()],
            },
            DeclaredSecret {
                env: vec!["XAI_API_KEY".into()],
                hosts: vec!["api.x.ai".into()],
            },
        ]);
        assert_eq!(
            rows(&vendor_boot_secrets(&module, &stored_openai, &env_xai_blank, &[])),
            vec![("CODEX_API_KEY", "sk-test-stored", vec!["api.openai.com"])],
        );

        // No stored openai key and a blank XAI env: the env fallback only fires on a value.
        assert_eq!(
            rows(&vendor_boot_secrets(&module, &stored_none, &env_xai, &[])),
            vec![("XAI_API_KEY", "sk-test-env", vec!["api.x.ai"])],
        );

        // Nothing configured anywhere, or the env name taken by a colony secret: silence.
        assert!(vendor_boot_secrets(&module, &stored_none, &env_none, &[]).is_empty());
        let taken = ["CODEX_API_KEY".to_string()];
        assert!(vendor_boot_secrets(&module, &stored_openai, &env_none, &taken).is_empty());
    }

    #[test]
    fn a_broken_agent_manifest_is_reported_by_path_and_cause_instead_of_vanishing() {
        let root = std::env::temp_dir().join(format!("colonizer-discover-{}", crate::util::short_id()));
        let agents = root.join("modules/agents");
        for (dir, manifest) in [
            ("good", r#"{"id": "good", "entry": ["node", "runner.mjs"]}"#),
            ("broken-json", "{not json"),
            ("no-entry", r#"{"id": "no-entry", "entry": []}"#),
            ("no-id", r#"{"entry": ["node"]}"#),
        ] {
            std::fs::create_dir_all(agents.join(dir)).unwrap();
            std::fs::write(agents.join(dir).join("module.json"), manifest).unwrap();
        }
        // Not modules at all, so not problems either.
        std::fs::create_dir_all(agents.join("test")).unwrap();
        std::fs::write(agents.join("README.md"), "").unwrap();

        let (modules, problems) = discover_agents(Some(&root));
        assert_eq!(modules.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["good"]);
        let manifest = |dir: &str| agents.join(dir).join("module.json").display().to_string();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems.contains(&format!(
                "{}: key must be a string at line 1 column 2",
                manifest("broken-json")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!(
                "{}: \"entry\" must be a non-empty array of strings",
                manifest("no-entry")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!("{}: missing \"id\"", manifest("no-id"))),
            "{problems:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn an_agent_setting_naming_an_unknown_skillset_is_refused_at_save_time() {
        let root = std::env::temp_dir().join(format!("colonizer-plugin-dirs-{}", crate::util::short_id()));
        let agent = AgentModule::test("claude-code")
            .dir(root.clone())
            .entry(vec!["node".into()])
            .needs_claude(true)
            .schema(json!({"type": "object", "properties": {"plugins": {"type": "string", "format": "plugin-dirs"}}}));
        let app = crate::tests::test_app_with_agents(&root, vec![agent], |_| {});
        // A skillset needs a manifest to pass validation (plugins::validate).
        std::fs::create_dir_all(app.cfg.data_dir.join("plugins/ecc")).unwrap();
        std::fs::write(app.cfg.data_dir.join("plugins/ecc/plugin.json"), r#"{"name": "ecc"}"#).unwrap();
        let save = |plugins: &str| {
            let mut settings = Map::new();
            settings.insert("plugins".into(), json!(plugins));
            let req = UpdateModule {
                provider: "claude-code".into(),
                enabled: true,
                settings,
                confirm_content: false,
            };
            update(State(app.clone()), Path("agent".into()), Json(req))
        };
        let err = save("ecc, superpower").await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "unknown skillset \"superpower\"; available: ecc");

        let saved = save(" ecc, ,").await.unwrap_or_else(|e| panic!("save refused: {:#}", e.1)).0;
        assert_eq!(saved["settings"]["plugins"], " ecc, ,");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn a_save_refuses_an_unknown_setting_by_name_but_keeps_stored_ones() {
        let root = std::env::temp_dir().join(format!("colonizer-unknown-key-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let save = |settings: Map<String, Value>| {
            let req = UpdateModule {
                provider: "github".into(),
                enabled: true,
                settings,
                confirm_content: false,
            };
            update(State(app.clone()), Path("source".into()), Json(req))
        };
        let mut settings = Map::new();
        settings.insert("include_lables".into(), json!("ready"));
        let err = save(settings.clone()).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(
            err.message()
                .starts_with("`include_lables` is not a github setting; known settings: "),
            "{}",
            err.message()
        );

        // What a removed setting leaves behind rides along, whatever the schema now says.
        app.modules
            .write()
            .await
            .get_mut("source")
            .unwrap()
            .settings
            .insert("include_lables".into(), json!("ready"));
        let saved = save(settings).await.unwrap_or_else(|e| panic!("save refused: {:#}", e.1)).0;
        assert_eq!(saved["settings"]["include_lables"], "ready");
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn manifest_problems_found_at_boot_are_listed_on_the_agent_kind() {
        let root = std::env::temp_dir().join(format!("colonizer-manifest-errors-{}", crate::util::short_id()));
        let mut app = crate::tests::test_app(&root);
        let problem = "/app/modules/agents/broken/module.json: missing \"id\"".to_string();
        Arc::get_mut(&mut app).unwrap().agent_problems = vec![problem.clone()];
        let listed = list(State(app)).await.0;
        let agent = listed.iter().find(|k| k["kind"] == "agent").unwrap();
        assert_eq!(agent["manifest_errors"], json!([problem]));
        assert!(
            listed
                .iter()
                .filter(|k| k["kind"] != "agent")
                .all(|k| k.get("manifest_errors").is_none()),
            "only the agent kind has manifests"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn settings_validation_names_unknown_keys_enums_and_types() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        let mut input = Map::new();
        input.insert("cpus".into(), json!(8));
        input.insert("unknwon".into(), json!("x"));
        let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
        assert!(
            err.starts_with("`unknwon` is not a sandbox setting; known settings: ")
                && err.contains("cpus")
                && err.contains("preset"),
            "{err}"
        );
        // A key already stored passes: the UI saves back everything modules.json holds, so refusing
        // those would brick every save after a provider switch or a removed setting.
        let mut stored = Map::new();
        stored.insert("unknwon".into(), json!("x"));
        let out = validate_settings("sandbox", &schema, &input, &stored).unwrap();
        assert_eq!(out.get("unknwon"), Some(&json!("x")), "the grandfathered key rides along");

        input.remove("unknwon").unwrap();
        input.insert("cpus".into(), json!(0));
        let err = validate_settings("sandbox", &schema, &input, &stored).unwrap_err();
        assert_eq!(err, "setting `cpus` must be between 1 and 64", "{err}");
        input.insert("cpus".into(), json!("eight"));
        // Being stored buys a key nothing once the schema declares it: `cpus` is checked like any
        // other, and only keys no schema has pass through untouched.
        stored.insert("cpus".into(), json!(4));
        let err = validate_settings("sandbox", &schema, &input, &stored).unwrap_err();
        assert_eq!(err, "setting `cpus` must be an integer", "{err}");
    }

    #[test]
    fn an_enum_refusal_names_the_options() {
        let schema = providers("burn_down", &[]).remove(0).schema;
        let mut input = Map::new();
        input.insert("reset_weekday".into(), json!("Funday"));
        let err = validate_settings("burn_down", &schema, &input, &Map::new()).unwrap_err();
        assert_eq!(
            err, "setting `reset_weekday` must be one of Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday",
            "{err}"
        );
    }

    #[test]
    fn the_sandbox_budget_defaults_to_off_and_rejects_negatives() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["budget_usd"]["default"],
            json!(0),
            "no budget unless the operator names one"
        );
        assert_eq!(
            schema["properties"]["budget_tokens"]["default"],
            json!(0),
            "no token budget unless the operator names one"
        );
        let mut input = Map::new();
        input.insert("budget_usd".into(), json!(-1));
        let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
        assert_eq!(err, "setting `budget_usd` must be at least 0", "{err}");
        input.remove("budget_usd");
        input.insert("budget_tokens".into(), json!(-1));
        assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
        input.remove("budget_tokens");
        input.insert("budget_usd".into(), json!(12.5));
        assert_eq!(
            validate_settings("sandbox", &schema, &input, &Map::new())
                .unwrap()
                .get("budget_usd"),
            Some(&json!(12.5))
        );
    }

    #[test]
    fn the_sandbox_host_disk_quota_is_a_size_and_malformed_ones_are_refused_at_save_time() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["host_disk"]["default"],
            json!("0"),
            "no quota unless the operator names one"
        );
        let mut input = Map::new();
        input.insert("host_disk".into(), json!("16G"));
        assert_eq!(
            validate_settings("sandbox", &schema, &input, &Map::new())
                .unwrap()
                .get("host_disk"),
            Some(&json!("16G"))
        );
        input.insert("host_disk".into(), json!(""));
        assert!(
            validate_settings("sandbox", &schema, &input, &Map::new()).is_ok(),
            "empty means unlimited, which is a size"
        );
        for bad in ["eight", "1.5G", "16 GB"] {
            input.insert("host_disk".into(), json!(bad));
            assert!(
                validate_settings("sandbox", &schema, &input, &Map::new()).is_err(),
                "{bad:?} must be refused while the operator is looking"
            );
        }
    }

    #[test]
    fn the_sandbox_egress_lists_are_entry_lists_validated_at_save_time() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["egress"]["default"],
            json!("open"),
            "an install that never hears of egress boots as before"
        );
        let mut input = Map::new();
        input.insert("egress".into(), json!("fenced"));
        assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
        input.insert("egress".into(), json!("open"));
        input.insert(
            "egress_allow".into(),
            json!("api.anthropic.com:443, 10.0.0.0/8, *.example.com"),
        );
        assert_eq!(
            validate_settings("sandbox", &schema, &input, &Map::new())
                .unwrap()
                .get("egress_allow"),
            input.get("egress_allow")
        );
        // A list a boot would have to drop is refused while the operator is looking, with the
        // parser's own word for why.
        input.insert("egress_allow".into(), json!("api.anthropic.com:443, host"));
        let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
        assert!(
            err.starts_with("setting `egress_allow`: `host` is not a host or host:port"),
            "{err}"
        );
    }

    #[test]
    fn the_sandbox_path_lists_take_strings_and_refuse_unusable_paths_at_save_time() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        for key in ["mask_paths", "protect_paths", "unmask_paths"] {
            assert_eq!(schema["properties"][key]["type"], "array", "{key} is an array setting");
            assert_eq!(schema["properties"][key]["default"], json!([]));
        }
        let mut input = Map::new();
        input.insert("mask_paths".into(), json!(["secrets/credentials.json", "vendor/keys/"]));
        input.insert("protect_paths".into(), json!(["tools/run.sh", ".airplane/"]));
        input.insert("unmask_paths".into(), json!([".envrc"]));
        let out = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap();
        assert_eq!(out.get("mask_paths"), input.get("mask_paths"));
        assert_eq!(out.get("protect_paths"), input.get("protect_paths"));

        // A non-array, or an array of non-strings, is the wrong type for the setting, and the
        // refusal says the type the schema asks for.
        for bad in [json!("secrets/credentials.json"), json!(["ok", 4])] {
            input.insert("mask_paths".into(), bad);
            let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
            assert_eq!(err, "setting `mask_paths` must be an array of strings", "{err}");
        }
        // And the entries themselves are checked with the boot's own gate: absolute paths,
        // traversal, the worktree root and — for the masked list only — the git dir.
        let refused = [
            ("mask_paths", "/etc/passwd"),
            ("mask_paths", "../outside"),
            ("mask_paths", "."),
            ("mask_paths", ".git/config"),
            ("protect_paths", "../../outside"),
            ("unmask_paths", "with,comma"),
        ];
        for (key, path) in refused {
            let mut one = Map::new();
            one.insert(key.to_string(), json!([path]));
            let err = validate_settings("sandbox", &schema, &one, &Map::new()).unwrap_err();
            assert!(
                err.starts_with(&format!("setting `{key}` has an unusable path")),
                "{path:?}: {err}"
            );
        }
        // Protecting the git dir is allowed: the read-only mount already covers it, but the
        // operator may underline it.
        let mut protect_git = Map::new();
        protect_git.insert("protect_paths".into(), json!([".git/config"]));
        assert!(validate_settings("sandbox", &schema, &protect_git, &Map::new()).is_ok());
    }

    #[test]
    fn the_sandbox_free_disk_thresholds_are_sizes_validated_like_host_disk() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["warn_free_disk"]["default"],
            json!("10G"),
            "the cockpit warns below 10G unless the operator says otherwise"
        );
        assert_eq!(
            schema["properties"]["min_free_disk"]["default"],
            json!("5G"),
            "the queue holds below 5G unless the operator says otherwise"
        );
        let mut input = Map::new();
        input.insert("warn_free_disk".into(), json!("8G"));
        input.insert("min_free_disk".into(), json!("5G"));
        let out = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap();
        assert_eq!(out.get("warn_free_disk"), Some(&json!("8G")));
        assert_eq!(out.get("min_free_disk"), Some(&json!("5G")));
        input.insert("min_free_disk".into(), json!("0"));
        assert_eq!(
            validate_settings("sandbox", &schema, &input, &Map::new())
                .unwrap()
                .get("min_free_disk"),
            Some(&json!("0")),
            "0 turns the floor off"
        );
        for bad in ["eight", "1.5G", "16 GB"] {
            input.insert("warn_free_disk".into(), json!(bad));
            assert!(
                validate_settings("sandbox", &schema, &input, &Map::new()).is_err(),
                "{bad:?} must be refused while the operator is looking"
            );
        }
    }

    #[test]
    fn the_held_colony_timeout_defaults_to_30_minutes_and_rejects_out_of_range() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["hold_timeout_minutes"]["default"],
            json!(30),
            "a held colony keeps its slot for half an hour unless the operator says otherwise"
        );
        let mut input = Map::new();
        for bad in [json!(0), json!(1441)] {
            input.insert("hold_timeout_minutes".into(), bad);
            let err = validate_settings("sandbox", &schema, &input, &Map::new()).unwrap_err();
            assert_eq!(
                err, "setting `hold_timeout_minutes` must be between 1 and 1440",
                "the refusal names the bounds: {err}"
            );
        }
        for ok in [1, 30, 1440] {
            input.insert("hold_timeout_minutes".into(), json!(ok));
            assert_eq!(
                validate_settings("sandbox", &schema, &input, &Map::new())
                    .unwrap()
                    .get("hold_timeout_minutes"),
                Some(&json!(ok))
            );
        }
    }

    #[test]
    fn schemas_are_normalized() {
        assert!(normalize_schema(&json!({"model": {"type": "string"}}))["properties"]["model"].is_object());
        assert!(normalize_schema(&Value::Null)["properties"].is_object());
    }

    #[test]
    fn voice_is_a_kind_whose_services_follow_the_browser() {
        assert!(KINDS.contains(&"voice"));
        let ids: Vec<_> = providers("voice", &[]).into_iter().map(|p| p.id).collect();
        assert_eq!(ids.first().map(String::as_str), Some("browser"));
        assert!(ids.iter().any(|id| id == "openai_compatible"));
        assert!(schema_for("voice", "openai_compatible", &[])["properties"]["base_url"].is_object());
        assert!(
            schema_for("voice", "openai", &[])["properties"]["base_url"].is_null(),
            "a hosted service's URL is not a setting"
        );
        let mut modules = ModulesConfig::default();
        assert!(modules.get("voice").is_none(), "absent until saved: the browser");
        assert_eq!(modules.get_mut("voice").unwrap().provider, "browser");
    }

    #[test]
    fn notify_is_a_kind_and_it_stays_disablable() {
        assert!(KINDS.contains(&"notify"));
        assert!(!is_required("notify"), "announcing colonies to the world is opt-in by design");
        assert!(is_required("source") && is_required("publish"));
    }

    #[test]
    fn burn_down_is_a_kind_and_it_is_opt_in() {
        assert!(KINDS.contains(&"burn_down"));
        assert!(!is_required("burn_down"), "spending a plan is opt-in by design");
        // The settings schema carries its defaults, so the cockpit can render it automatically.
        let schema = providers("burn_down", &[]).remove(0).schema;
        for (key, default) in [
            ("reset_weekday", json!("Monday")),
            ("reset_time", json!("00:00")),
            ("lead_hours", json!(48)),
            ("reserve_pct", json!(5)),
            ("spend_usd_per_colony", json!(5)),
            ("max_live", json!(2)),
            ("repos", json!("")),
            ("instructions", json!("")),
        ] {
            assert_eq!(schema["properties"][key]["default"], default, "{key}");
        }
        assert_eq!(
            schema["properties"]["allowance_usd"].get("default"),
            None,
            "the allowance is the one setting with no default: burn-down must never invent a budget"
        );
        // An invalid weekday saved by hand is refused at save time by the enum check; see
        // `an_enum_refusal_names_the_options` for the exact refusal.
        let mut input = Map::new();
        input.insert("reset_weekday".into(), json!("Funday"));
        assert!(
            validate_settings(
                "burn_down",
                &providers("burn_down", &[]).remove(0).schema,
                &input,
                &Map::new()
            )
            .is_err()
        );
    }

    #[test]
    fn egress_parsing_accepts_an_absent_section_and_names_what_it_refuses() {
        assert_eq!(parse_egress(&json!({})).unwrap(), None);
        // A repeated host keeps one union entry; a wildcard covers subdomains only.
        let section = json!({"api": ["api.anthropic.com", "api.anthropic.com"], "telemetry": ["*.sentry.io"]});
        let egress = parse_egress(&json!({"egress": section}))
            .unwrap()
            .expect("a valid section parses");
        assert_eq!(egress.hosts(), ["api.anthropic.com", "*.sentry.io"]);
        assert!(egress.covers("o447895.sentry.io") && !egress.covers("sentry.io"));
        for (section, expected) in [
            (
                json!({"api": ["https://x"]}),
                r#"egress.api[0]: "https://x" is not a bare hostname"#,
            ),
            (
                json!({"api": [], "foo": []}),
                r#"egress: unknown category "foo"; known: api, auth, telemetry, extra"#,
            ),
            (json!({"api": "x"}), "egress.api must be an array of hostnames"),
        ] {
            assert_eq!(parse_egress(&json!({"egress": section})).unwrap_err(), expected);
        }
    }

    #[test]
    fn every_shipped_agent_module_declares_an_egress_that_covers_its_secret_hosts() {
        // CI enforcement (#304): a shipped module without the declaration is a named failure, and
        // one whose secrets reach hosts it does not declare is refused by read_agent itself.
        let agents = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
        let mut checked = 0;
        for entry in std::fs::read_dir(&agents).unwrap().flatten() {
            let dir = entry.path();
            if !dir.join("module.json").is_file() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy();
            let egress = read_agent(&dir.join("module.json"))
                .unwrap_or_else(|e| panic!("modules/agents/{id}/module.json: {e}"))
                .egress
                .unwrap_or_else(|| panic!("modules/agents/{id}/module.json: missing egress declaration"));
            assert!(egress.api.iter().all(|host| egress.covers(host)) && !egress.hosts().iter().any(String::is_empty));
            checked += 1;
        }
        assert!(checked >= 6, "expected the six shipped agent modules, walked {checked}");
    }

    #[test]
    fn every_shipped_agent_module_appears_in_the_provider_compatibility_table() {
        // CI enforcement (#304): a shipped runner missing from the docs is a named failure, so the
        // connection → backends table in docs/providers.md cannot silently rot when an agent module
        // is added.
        const TABLE: &str = include_str!("../../../docs/providers.md");
        let agents = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/agents");
        let mut listed = 0;
        for entry in std::fs::read_dir(&agents).unwrap().flatten() {
            let dir = entry.path();
            if !dir.join("module.json").is_file() {
                continue;
            }
            let id = dir.file_name().unwrap().to_string_lossy();
            let row = format!("| `{id}` |");
            assert!(
                TABLE.contains(&row),
                "docs/providers.md: no compatibility-table row for `{id}`; add the runner to the connection → backends table (a line starting with \"{row}\")"
            );
            listed += 1;
        }
        assert!(listed >= 6, "expected the six shipped agent modules, walked {listed}");
    }

    /// An agent module carrying exactly these `requires`, for the preflight tests below.
    fn agent_with(requires: Requires) -> AgentModule {
        AgentModule::test("grok-build").requires(requires)
    }

    #[test]
    fn the_requires_section_parses_binaries_pins_and_the_runner_fetched_marker() {
        // The shipped grok-build manifest: one required binary, pinned with an install command and
        // a source_rev the preflight has no use for.
        const GROK: &str = include_str!("../../../modules/agents/grok-build/module.json");
        let grok: Value = serde_json::from_str(GROK).unwrap();
        let requires = parse_requires(&grok).unwrap();
        assert_eq!(requires.binaries, ["grok"]);
        let pin = requires.pins.get("grok").expect("the grok pin parses");
        assert_eq!(pin.version, "1.0.34");
        assert_eq!(pin.install.as_deref(), Some("https://x.ai/cli/install.sh"));
        // acp pins a package name that is not a required binary: carried as declared, matched to
        // no binary by the preflight. The gemini preset's CLI is fetched by the runner, so the
        // fetched marker must list it and the preflight must leave it alone.
        const ACP: &str = include_str!("../../../modules/agents/acp/module.json");
        let acp: Value = serde_json::from_str(ACP).unwrap();
        let requires = parse_requires(&acp).unwrap();
        assert_eq!(requires.binaries, ["gemini"]);
        assert_eq!(requires.pins.keys().next().map(String::as_str), Some("@google/gemini-cli"));
        assert!(!requires.pins.contains_key("gemini"), "{:?}", requires.pins);
        assert_eq!(requires.fetched_by_runner, ["gemini"]);
        // opencode fetches its own binary at runtime, so the preflight must leave it alone.
        const OPENCODE: &str = include_str!("../../../modules/agents/opencode/module.json");
        let opencode: Value = serde_json::from_str(OPENCODE).unwrap();
        let requires = parse_requires(&opencode).unwrap();
        assert_eq!(requires.fetched_by_runner, ["opencode"]);
        assert_eq!(requires.binaries, ["opencode"]);
        // No section at all parses as none; pi declares none of the checked keys.
        assert_eq!(parse_requires(&json!({})).unwrap(), Requires::default());
        const PI: &str = include_str!("../../../modules/agents/pi/module.json");
        let pi: Value = serde_json::from_str(PI).unwrap();
        assert_eq!(parse_requires(&pi).unwrap(), Requires::default());
    }

    #[test]
    fn a_required_binary_no_stock_image_carries_is_refused_naming_the_binary_and_the_fix() {
        let agent = agent_with(Requires {
            binaries: vec!["grok".into()],
            pins: BTreeMap::from([(
                "grok".into(),
                Pin {
                    version: "1.0.34".into(),
                    install: Some("https://x.ai/cli/install.sh".into()),
                },
            )]),
            ..Default::default()
        });
        let image = crate::presets::pinned_image("node").unwrap();
        let err = check_requires(&agent, &image, &[]).unwrap_err();
        assert!(
            err.contains("agent module `grok-build` needs the `grok` binary (pinned 1.0.34)")
                && err.contains(&image)
                && err.contains("set the sandbox module's image to one with grok on PATH")
                && err.contains("install: https://x.ai/cli/install.sh"),
            "{err}"
        );
        // The bare tag is the same stock image, and a pin without an install command refuses just
        // the same, without pretending to know how to install the binary.
        assert!(check_requires(&agent, "node:24-bookworm", &[]).is_err());
        let unpinned_install = agent_with(Requires {
            binaries: vec!["hermes".into()],
            ..Default::default()
        });
        let err = check_requires(&unpinned_install, "rust:1-bookworm", &[]).unwrap_err();
        assert!(
            err.contains("needs the `hermes` binary, which") && !err.contains("install:") && !err.contains("pinned"),
            "{err}"
        );
    }

    #[test]
    fn a_required_binary_a_custom_image_may_carry_is_trusted_until_the_vm_checks() {
        let agent = agent_with(Requires {
            binaries: vec!["grok".into()],
            ..Default::default()
        });
        assert!(
            check_requires(&agent, "ghcr.io/me/grok-toolchain:1", &[]).is_ok(),
            "a custom image is the operator's word; the runner's in-VM preflight still checks"
        );
    }

    #[test]
    fn a_base_tool_every_stock_preset_carries_passes_the_check() {
        let wget = agent_with(Requires {
            binaries: vec!["wget".into()],
            ..Default::default()
        });
        assert!(
            check_requires(&wget, &crate::presets::pinned_image("node").unwrap(), &[]).is_ok(),
            "the Debian base tools ride every stock preset, whatever else it ships"
        );
        // The list is what the presets share, not what the widest one carries: `git` ships on the
        // node, python and go images but not the rust one, so it still refuses — without claiming
        // the configured image carries nothing like it.
        let git = agent_with(Requires {
            binaries: vec!["git".into()],
            ..Default::default()
        });
        let image = crate::presets::pinned_image("node").unwrap();
        let err = check_requires(&git, &image, &[]).unwrap_err();
        assert!(err.contains("do not all carry") && err.contains(&image), "{err}");
    }

    #[test]
    fn a_runner_fetched_binary_passes_whatever_the_image() {
        let agent = agent_with(Requires {
            binaries: vec!["opencode".into()],
            fetched_by_runner: vec!["opencode".into()],
            ..Default::default()
        });
        assert!(check_requires(&agent, &crate::presets::pinned_image("node").unwrap(), &[]).is_ok());
    }

    #[test]
    fn the_staged_claude_passes_and_a_mismatching_pin_is_refused_naming_both_versions() {
        let pinned = |version: &str| {
            agent_with(Requires {
                binaries: vec!["claude".into()],
                pins: BTreeMap::from([(
                    "claude".into(),
                    Pin {
                        version: version.into(),
                        install: None,
                    },
                )]),
                ..Default::default()
            })
        };
        let staged = [StagedBinary {
            name: "claude".into(),
            version: Some("2.1.280".into()),
        }];
        let image = crate::presets::pinned_image("node").unwrap();
        // The shipped claude-code module pins nothing: the staged binary is what it needs.
        assert!(check_requires(&pinned("2.1.280"), &image, &staged).is_ok());
        assert!(
            check_requires(
                &agent_with(Requires {
                    binaries: vec!["claude".into()],
                    ..Default::default()
                }),
                &image,
                &staged
            )
            .is_ok()
        );
        let err = check_requires(&pinned("9.9.9"), &image, &staged).unwrap_err();
        assert!(
            err.contains("pins the `claude` binary at 9.9.9, but the harness stages 2.1.280"),
            "{err}"
        );
        // A staged version the harness does not know — the host's own install standing in for a
        // missing vendored build — cannot be pin-checked, so it is not.
        let unknown = [StagedBinary {
            name: "claude".into(),
            version: None,
        }];
        assert!(check_requires(&pinned("9.9.9"), &image, &unknown).is_ok());
    }

    #[test]
    fn a_malformed_requires_section_is_a_manifest_problem_by_path_and_cause() {
        let root = std::env::temp_dir().join(format!("colonizer-requires-{}", crate::util::short_id()));
        let agents = root.join("modules/agents");
        for (dir, manifest) in [
            (
                "bad-binaries",
                r#"{"id": "x", "entry": ["node"], "requires": {"binaries": "claude"}}"#,
            ),
            ("bad-pins", r#"{"id": "x", "entry": ["node"], "requires": {"pins": []}}"#),
            (
                "pin-no-version",
                r#"{"id": "x", "entry": ["node"], "requires": {"pins": {"claude": {}}}}"#,
            ),
            (
                "bad-marker",
                r#"{"id": "x", "entry": ["node"], "requires": {"fetched_by_runner": "opencode"}}"#,
            ),
            ("not-an-object", r#"{"id": "x", "entry": ["node"], "requires": "claude"}"#),
        ] {
            std::fs::create_dir_all(agents.join(dir)).unwrap();
            std::fs::write(agents.join(dir).join("module.json"), manifest).unwrap();
        }
        let (modules, problems) = discover_agents(Some(&root));
        assert!(modules.is_empty(), "{:?}", modules.iter().map(|m| &m.id));
        assert_eq!(problems.len(), 5, "{problems:?}");
        let manifest = |dir: &str| agents.join(dir).join("module.json").display().to_string();
        assert!(
            problems.contains(&format!(
                "{}: requires.binaries must be an array of strings",
                manifest("bad-binaries")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!(
                "{}: requires.pins must be an object of package or binary name to pin",
                manifest("bad-pins")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!(
                "{}: requires.pins.claude must name a string \"version\"",
                manifest("pin-no-version")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!(
                "{}: requires.fetched_by_runner must be an array of strings",
                manifest("bad-marker")
            )),
            "{problems:?}"
        );
        assert!(
            problems.contains(&format!("{}: requires must be an object", manifest("not-an-object"))),
            "{problems:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_claude_lock_version_reads_the_agent_rows_only() {
        let version = claude_lock_version(CLAUDE_LOCK).expect("the shipped lock pins a Claude Code build");
        assert!(!version.is_empty(), "{version}");
        const SAMPLE: &str = "\
# a comment naming the row that is not an entry
claude-code  2.1.280  linux-arm64  agent  aa  https://x/arm64
claude-code  2.1.280  linux-x64    agent  bb  https://x/x64
other        9.9.9    linux-x64    agent  cc  https://x/other
";
        assert_eq!(claude_lock_version(SAMPLE), Some("2.1.280"));
        assert_eq!(
            claude_lock_version("claude-code 2.1.280 linux-x64 binary aa https://x/x64"),
            None,
            "a non-agent kind never matches"
        );
        assert_eq!(claude_lock_version(""), None);
    }

    #[test]
    fn the_harness_stages_claude_at_the_lock_version_only_when_the_guest_build_is_installed() {
        let mut cfg = crate::config::Settings {
            bind: "127.0.0.1:0".into(),
            data_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            runtime_dir: PathBuf::new(),
            assets: None,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: Vec::new(),
            fleet_peers: Vec::new(),
            bench_pool: None,
        };
        let staged = harness_staged_binaries(&cfg);
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].name, "claude");
        assert_eq!(staged[0].version, None, "no vendored build, no version to pin-check");
        let dir = std::env::temp_dir().join(format!("colonizer-staged-claude-{}", crate::util::short_id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/claude-guest"), b"\x7fELF padding").unwrap();
        cfg.assets = Some(dir.clone());
        let staged = harness_staged_binaries(&cfg);
        assert_eq!(
            staged[0].version.as_deref(),
            claude_lock_version(CLAUDE_LOCK),
            "the vendored build is the lock's"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
