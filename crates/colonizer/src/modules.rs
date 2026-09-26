//! Module registry: which providers exist for each module kind, their settings schemas, and the
//! Settings → Modules API.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModuleChoice, Settings},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path as FsPath, PathBuf};

pub const KINDS: [&str; 13] = [
    "source",
    "sandbox",
    "mesh",
    "agent",
    "interfaces",
    "publish",
    "memory",
    "watchdog",
    "autonomy",
    "notify",
    "burn_down",
    "voice",
    "screen",
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
    pub schema: Value,
    /// The manifest's `egress` declaration; third-party modules may omit the section.
    pub egress: Option<Egress>,
    /// The manifest's `session_resume.dir` — a path inside the VM where the runner keeps agent
    /// session transcripts — when the agent can pick an old conversation back up (`None` when it
    /// cannot). The harness mounts a host directory over the path so transcripts survive a stopped
    /// microVM, and a suspended colony's answer resumes the same agent session (issue #562).
    pub resume_dir: Option<String>,
}

/// The fixed network hosts an agent module's runner needs, declared under `egress` in `module.json`
/// as four optional arrays of bare hostnames (a leading `*.` wildcard allowed): the vendor's API,
/// login and telemetry hosts, and everything else fixed. The #304 allowlist is this plus the task's.
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
    let binaries = manifest["requires"]["binaries"].to_string();
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
        needs_claude: binaries.contains("\"claude\"") || secrets.contains("CLAUDE_CODE_OAUTH_TOKEN"),
        schema: normalize_schema(&manifest["settings"]),
        dir: path.parent().map(FsPath::to_path_buf).unwrap_or_default(),
        egress,
        resume_dir,
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

pub struct Provider {
    pub id: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub fn providers(kind: &str, agents: &[AgentModule]) -> Vec<Provider> {
    let p = |id: &str, name: &str, description: &str, schema: Value| Provider {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        schema,
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
            .map(|a| p(&a.id, &a.name, &a.description, a.schema.clone()))
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
                "draft": {"type": "boolean", "title": "Open as draft", "default": false},
                "file_findings": {"type": "boolean", "title": "File validated findings as issues", "description": "When a colony notices a problem outside its task, its orchestrator has it confirmed and files it as an issue on the same repository, labelled colonizer-finding. Open issues with the same title are not filed again, and one colony files at most five.", "default": true},
                "autofix": {"type": "boolean", "title": "Autofix validated findings", "description": "When a colony files a validated finding, spawn a fix colony for it: a fresh colony whose pull request is reviewed by an independent session before anything merges. Can be switched off per colony at launch.", "default": false},
                "automerge": {"type": "boolean", "title": "Merge fixes whose review passes", "description": "Merge a fix colony's pull request when its independent review passes; requires autofix, since with no fix colonies there is nothing to merge. Can be switched off per colony at launch.", "default": false}
            }}),
        )],
        "memory" => vec![
            p(
                "files",
                "Shared memory",
                "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context. Off only lets repo notes through; org and global notes are always reviewed", "default": true}
                }}),
            ),
            p(
                "mem0",
                "mem0",
                "Approved notes stored in your mem0 project. Colonies read them exactly as they read files, most relevant to the task first; the key never enters a colony",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context. Off only lets repo notes through; org and global notes are always reviewed", "default": true},
                    "base_url": {"type": "string", "title": "API base URL", "description": "The mem0 Platform API. Self-hosted mem0 serves a different API and is not supported", "default": "https://api.mem0.ai"}
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
        "providers": providers.iter().map(|p| json!({"id": p.id, "name": p.name, "description": p.description})).collect::<Vec<_>>(),
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
    provider: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    settings: Map<String, Value>,
}

fn yes() -> bool {
    true
}

/// The kinds a harness is not a harness without; every other kind may be switched off.
fn is_required(kind: &str) -> bool {
    matches!(kind, "source" | "sandbox" | "agent" | "publish")
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
fn validate_settings(
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
        let ok = match spec["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.is_i64() || value.is_u64(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("array") => value.is_array() && value.as_array().is_some_and(|items| items.iter().all(Value::is_string)),
            _ => true,
        };
        if !ok {
            return Err(format!("setting `{key}` has the wrong type"));
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
            return Err(format!("setting `{key}` is out of range"));
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
        let module = AgentModule {
            id: "claude-code".into(),
            name: String::new(),
            description: String::new(),
            dir,
            entry: vec!["node".into(), "runner.mjs".into()],
            needs_claude: true,
            schema: Value::Null,
            egress: None,
            resume_dir: None,
        };
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
        assert_eq!(pi.vm_command(), ["node", "/opt/colonizer/agent/runner.mjs"]);
        assert_eq!(pi.schema["properties"]["model"]["env"], "COLONIZER_MODEL");
        std::fs::remove_dir_all(&root).ok();
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
        let agent = AgentModule {
            id: "claude-code".into(),
            name: String::new(),
            description: String::new(),
            dir: root.clone(),
            entry: vec!["node".into()],
            needs_claude: true,
            schema: json!({"type": "object", "properties": {"plugins": {"type": "string", "format": "plugin-dirs"}}}),
            egress: None,
            resume_dir: None,
        };
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
        assert!(validate_settings("sandbox", &schema, &input, &stored).is_err());
        input.insert("cpus".into(), json!("eight"));
        // Being stored buys a key nothing once the schema declares it: `cpus` is checked like any
        // other, and only keys no schema has pass through untouched.
        stored.insert("cpus".into(), json!(4));
        assert!(validate_settings("sandbox", &schema, &input, &stored).is_err());
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
        assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
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

        // A non-array, or an array of non-strings, is the wrong type for the setting.
        for bad in [json!("secrets/credentials.json"), json!(["ok", 4])] {
            input.insert("mask_paths".into(), bad);
            assert!(validate_settings("sandbox", &schema, &input, &Map::new()).is_err());
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
            assert!(
                validate_settings("sandbox", &schema, &input, &Map::new()).is_err(),
                "the timeout is 1 to 1440 minutes"
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
}
