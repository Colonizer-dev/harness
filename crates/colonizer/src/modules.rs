//! Module registry: which providers exist for each module kind, their settings schemas, and the
//! Settings → Modules API.

use crate::{
    ApiResult, Shared, client_error,
    config::{ModuleChoice, ModulesConfig},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::{Path as FsPath, PathBuf};

pub const KINDS: [&str; 10] = [
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

pub fn discover_agents(assets: Option<&FsPath>) -> Vec<AgentModule> {
    let Some(root) = assets else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(root.join("modules/agents")) else {
        return Vec::new();
    };
    let mut modules: Vec<AgentModule> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("module.json")).ok()?).ok()?;
            let entry_cmd: Vec<String> = manifest["entry"]
                .as_array()?
                .iter()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            if entry_cmd.is_empty() {
                return None;
            }
            let secrets = manifest["secrets"].to_string();
            let binaries = manifest["requires"]["binaries"].to_string();
            Some(AgentModule {
                id: manifest["id"].as_str()?.to_string(),
                name: manifest["name"].as_str().unwrap_or_default().to_string(),
                description: manifest["description"].as_str().unwrap_or_default().to_string(),
                entry: entry_cmd,
                needs_claude: binaries.contains("\"claude\"") || secrets.contains("CLAUDE_CODE_OAUTH_TOKEN"),
                schema: normalize_schema(&manifest["settings"]),
                dir,
            })
        })
        .collect();
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    modules
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
            json!({"type":"object","properties":{}}),
        )],
        "sandbox" => vec![p(
            "microsandbox",
            "microsandbox",
            "Rootless libkrun microVMs with per-VM TLS secret injection",
            json!({"type": "object", "properties": {
                "preset": {
                    "type": "string", "title": "Stack",
                    "description": "Picks the image and machine size for a colony. Every preset is glibc-based, which the agent binary requires. Choose 'custom' to set the fields below yourself; anything you set explicitly wins over the preset either way.",
                    "enum": crate::presets::ids(), "default": "node"
                },
                "image": {"type": "string", "title": "Image", "description": "glibc-based OCI image with the tools your projects need, pinned by digest. Set by the stack unless you change it.", "default": crate::presets::pinned_image("node")},
                "cpus": {"type": "integer", "title": "vCPUs", "minimum": 1, "maximum": 64, "default": 4},
                "memory": {"type": "string", "title": "Memory", "description": "e.g. 8G", "default": "8G"},
                "root_disk": {"type": "string", "title": "Root disk", "default": "16G"},
                "max_duration": {"type": "string", "title": "Max session length", "description": "e.g. 8h", "default": "8h"},
                "max_parallel": {"type": "integer", "title": "Parallel sessions", "minimum": 1, "maximum": 32, "default": 3},
                "budget_usd": {"type": "number", "title": "Budget per colony (USD)", "minimum": 0, "default": 0,
                    "description": "Dollars one colony may spend on models in total, Claude and every routed provider together. 0, the default, means unlimited: there is no figure that suits every deployment. Providers need pricing set for their routed tokens to count toward it. When a colony passes the budget its next routed request is refused and the colony is stopped on the host with its worktree kept; raise the budget and press Resume to continue."},
                "host_disk": {"type": "string", "title": "Host disk per colony", "default": "0", "format": "disk-size",
                    "description": "How much disk one colony may leave on the host: its worktree, where everything built inside the colony lands, plus its session files and logs. The microVM's own root disk is the Root disk setting above and is not counted here. 0, the default, means unlimited: there is no size that suits every deployment. Measured every few minutes. When a colony passes the quota it is stopped on the host and its worktree is kept; clean up or raise the quota and press Resume to continue."}
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
                "draft": {"type": "boolean", "title": "Open as draft", "default": false},
                "file_findings": {"type": "boolean", "title": "File validated findings as issues", "description": "When a colony notices a problem outside its task, its orchestrator has it confirmed and files it as an issue on the same repository, labelled colonizer-finding. Open issues with the same title are not filed again, and one colony files at most five.", "default": true}
            }}),
        )],
        "memory" => vec![
            p(
                "files",
                "Shared memory",
                "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context", "default": true}
                }}),
            ),
            p(
                "mem0",
                "mem0",
                "Approved notes stored in your mem0 project. Colonies read them exactly as they read files, most relevant to the task first; the key never enters a colony",
                json!({"type": "object", "properties": {
                    "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context", "default": true},
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
                    "free_text": {"type": "boolean", "title": "Answer questions that have no options", "description": "Off by default: a free-text box is where an automatic answer can do the most damage. With it off, those questions wait for you.", "default": false}
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
                "desktop": {"type": "boolean", "title": "Desktop notifications", "description": "Notify the desktop the mothership runs on. Does nothing over SSH or on a headless machine, and says so once in the log", "default": false},
                "webhook_url": {"type": "string", "title": "Webhook URL", "description": "POSTs a short JSON note per event to an address outside this machine. It carries no repository content — colony, event and time only — and it is unsigned unless a signing secret is set in Settings", "default": ""}
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

fn describe_kind(kind: &str, choice: &ModuleChoice, agents: &[AgentModule]) -> Value {
    let providers = providers(kind, agents);
    let schema = providers
        .iter()
        .find(|p| p.id == choice.provider)
        .map(|p| p.schema.clone())
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "kind": kind,
        "provider": choice.provider,
        "enabled": choice.enabled,
        "providers": providers.iter().map(|p| json!({"id": p.id, "name": p.name, "description": p.description})).collect::<Vec<_>>(),
        "settings": choice.settings,
        "schema": schema,
    })
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    let modules = app.modules.read().await;
    Json(
        KINDS
            .iter()
            .filter_map(|k| modules.get(k).map(|c| describe_kind(k, c, &app.agents)))
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
    let settings =
        validate_settings(&provider.schema, &req.settings).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;

    let mut modules = app.modules.write().await;
    let choice = modules
        .get_mut(&kind)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "unknown module kind"))?;
    *choice = ModuleChoice {
        provider: req.provider,
        enabled: req.enabled,
        settings,
    };
    let described = describe_kind(&kind, choice, &app.agents);
    save_modules(&app.modules_file(), &modules)?;
    Ok(Json(described))
}

fn save_modules(path: &FsPath, modules: &ModulesConfig) -> anyhow::Result<()> {
    modules.save(path)
}

/// Keeps only known keys and checks types, enums and ranges.
fn validate_settings(schema: &Value, input: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    let Some(properties) = schema["properties"].as_object() else {
        return Ok(out);
    };
    for (key, value) in input {
        let Some(spec) = properties.get(key) else { continue };
        let ok = match spec["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.is_i64() || value.is_u64(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            _ => true,
        };
        if !ok {
            return Err(format!("setting `{key}` has the wrong type"));
        }
        if let Some(options) = spec["enum"].as_array()
            && !options.contains(value)
        {
            return Err(format!("setting `{key}` must be one of the listed options"));
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
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validation_filters_and_checks() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        let mut input = Map::new();
        input.insert("cpus".into(), json!(8));
        input.insert("unknown".into(), json!("x"));
        let out = validate_settings(&schema, &input).unwrap();
        assert_eq!(out.get("cpus"), Some(&json!(8)));
        assert!(!out.contains_key("unknown"));

        input.insert("cpus".into(), json!(0));
        assert!(validate_settings(&schema, &input).is_err());
        input.insert("cpus".into(), json!("eight"));
        assert!(validate_settings(&schema, &input).is_err());
    }

    #[test]
    fn the_sandbox_budget_defaults_to_off_and_rejects_negatives() {
        let schema = providers("sandbox", &[]).remove(0).schema;
        assert_eq!(
            schema["properties"]["budget_usd"]["default"],
            json!(0),
            "no budget unless the operator names one"
        );
        let mut input = Map::new();
        input.insert("budget_usd".into(), json!(-1));
        assert!(validate_settings(&schema, &input).is_err());
        input.insert("budget_usd".into(), json!(12.5));
        assert_eq!(
            validate_settings(&schema, &input).unwrap().get("budget_usd"),
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
            validate_settings(&schema, &input).unwrap().get("host_disk"),
            Some(&json!("16G"))
        );
        input.insert("host_disk".into(), json!(""));
        assert!(
            validate_settings(&schema, &input).is_ok(),
            "empty means unlimited, which is a size"
        );
        for bad in ["eight", "1.5G", "16 GB"] {
            input.insert("host_disk".into(), json!(bad));
            assert!(
                validate_settings(&schema, &input).is_err(),
                "{bad:?} must be refused while the operator is looking"
            );
        }
    }

    #[test]
    fn schemas_are_normalized() {
        assert!(normalize_schema(&json!({"model": {"type": "string"}}))["properties"]["model"].is_object());
        assert!(normalize_schema(&Value::Null)["properties"].is_object());
    }

    #[test]
    fn notify_is_a_kind_and_it_stays_disablable() {
        assert!(KINDS.contains(&"notify"));
        assert!(!is_required("notify"), "announcing colonies to the world is opt-in by design");
        assert!(is_required("source") && is_required("publish"));
    }
}
