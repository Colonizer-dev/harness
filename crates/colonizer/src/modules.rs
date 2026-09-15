//! Module registry: which providers exist for each module kind, their settings schemas, and the
//! Settings → Modules API.

use crate::{
    client_error,
    config::{ModuleChoice, ModulesConfig},
    ApiResult, Shared,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

pub const KINDS: [&str; 8] = ["source", "sandbox", "mesh", "agent", "interfaces", "publish", "memory", "watchdog"];

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
            .map(|arg| if self.dir.join(arg).exists() { format!("/opt/colonizer/agent/{arg}") } else { arg.clone() })
            .collect()
    }
}

pub fn discover_agents(assets: Option<&FsPath>) -> Vec<AgentModule> {
    let Some(root) = assets else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(root.join("modules/agents")) else { return Vec::new() };
    let mut modules: Vec<AgentModule> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("module.json")).ok()?).ok()?;
            let entry_cmd: Vec<String> = manifest["entry"].as_array()?.iter().filter_map(|a| a.as_str().map(String::from)).collect();
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
        "source" => vec![p("github", "GitHub", "Issues from repositories your GitHub account can access", json!({"type":"object","properties":{}}))],
        "sandbox" => vec![p(
            "microsandbox",
            "microsandbox",
            "Rootless libkrun microVMs with per-VM TLS secret injection",
            json!({"type": "object", "properties": {
                "image": {"type": "string", "title": "Image", "description": "glibc-based OCI image with the tools your projects need", "default": "node:24-bookworm"},
                "cpus": {"type": "integer", "title": "vCPUs", "minimum": 1, "maximum": 64, "default": 4},
                "memory": {"type": "string", "title": "Memory", "description": "e.g. 8G", "default": "8G"},
                "root_disk": {"type": "string", "title": "Root disk", "default": "16G"},
                "max_duration": {"type": "string", "title": "Max session length", "description": "e.g. 8h", "default": "8h"},
                "max_parallel": {"type": "integer", "title": "Parallel sessions", "minimum": 1, "maximum": 32, "default": 3}
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
            p("none", "Loopback port", "No mesh: reach each VM through a published loopback port", json!({"type":"object","properties":{}})),
        ],
        "agent" => agents.iter().map(|a| p(&a.id, &a.name, &a.description, a.schema.clone())).collect(),
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
                "draft": {"type": "boolean", "title": "Open as draft", "default": false}
            }}),
        )],
        "memory" => vec![p(
            "files",
            "Shared memory",
            "Markdown notes per repository, org and globally, mounted read-only into colonies; agents propose new notes",
            json!({"type": "object", "properties": {
                "require_review": {"type": "boolean", "title": "Review proposals before they become memory", "description": "Recommended: an approved note becomes part of every future colony's context", "default": true}
            }}),
        )],
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
    Json(KINDS.iter().filter_map(|k| modules.get(k).map(|c| describe_kind(k, c, &app.agents))).collect())
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

pub async fn update(
    State(app): State<Shared>,
    Path(kind): Path<String>,
    Json(req): Json<UpdateModule>,
) -> ApiResult<Value> {
    let providers = providers(&kind, &app.agents);
    let Some(provider) = providers.iter().find(|p| p.id == req.provider) else {
        return Err(client_error(StatusCode::BAD_REQUEST, "unknown module kind or provider"));
    };
    if matches!(kind.as_str(), "source" | "sandbox" | "agent" | "publish") && !req.enabled {
        return Err(client_error(StatusCode::BAD_REQUEST, "this module kind is required and can't be disabled"));
    }
    let settings = validate_settings(&provider.schema, &req.settings)
        .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;

    let mut modules = app.modules.write().await;
    let choice = modules.get_mut(&kind).ok_or_else(|| client_error(StatusCode::NOT_FOUND, "unknown module kind"))?;
    *choice = ModuleChoice { provider: req.provider, enabled: req.enabled, settings };
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
    let Some(properties) = schema["properties"].as_object() else { return Ok(out) };
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
        if let Some(options) = spec["enum"].as_array() {
            if !options.contains(value) {
                return Err(format!("setting `{key}` must be one of the listed options"));
            }
        }
        if let Some(n) = value.as_f64() {
            if spec["minimum"].as_f64().is_some_and(|min| n < min) || spec["maximum"].as_f64().is_some_and(|max| n > max) {
                return Err(format!("setting `{key}` is out of range"));
            }
        }
        if let Some(s) = value.as_str() {
            if s.len() > 500 || s.contains('\n') {
                return Err(format!("setting `{key}` is too long"));
            }
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
    fn schemas_are_normalized() {
        assert!(normalize_schema(&json!({"model": {"type": "string"}}))["properties"]["model"].is_object());
        assert!(normalize_schema(&Value::Null)["properties"].is_object());
    }
}
