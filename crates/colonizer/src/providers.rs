//! Model providers: Anthropic-compatible endpoints that colonies can route models to, such as
//! DeepSeek's API or a model served on this machine. Keys stay on the mothership (0600) and reach
//! colonies as microsandbox secrets scoped to the provider's host.

use crate::{
    client_error,
    sandbox::Secret,
    util::{read_trimmed, write_secret},
    ApiResult, App, Shared,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub auth: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub preset: String,
}

/// Models served by Anthropic with the Claude login, offered as suggestions in model pickers.
const ANTHROPIC_MODELS: &[(&str, &str)] = &[
    ("opus", "Claude Opus (latest)"),
    ("sonnet", "Claude Sonnet (latest)"),
    ("haiku", "Claude Haiku (latest)"),
    ("fable", "Claude Fable (latest)"),
    ("claude-opus-5", "Claude Opus 5"),
    ("claude-sonnet-5", "Claude Sonnet 5"),
    ("claude-haiku-4-5", "Claude Haiku 4.5"),
];

const AUTH_MODES: [&str; 3] = ["x-api-key", "bearer", "none"];
const PRESETS: [&str; 3] = ["deepseek", "local", "custom"];

impl App {
    fn providers_file(&self) -> PathBuf {
        self.cfg.config_dir.join("providers.json")
    }

    fn provider_key_file(&self, id: &str) -> PathBuf {
        self.cfg.config_dir.join("provider-keys").join(id)
    }

    pub fn providers(&self) -> Vec<Provider> {
        std::fs::read(self.providers_file()).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    fn save_providers(&self, providers: &[Provider]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cfg.config_dir)?;
        let path = self.providers_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(providers)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn provider_key(&self, id: &str) -> Option<String> {
        read_trimmed(&self.provider_key_file(id))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id != "anthropic"
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 120 && model.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/[]".contains(c))
}

/// Splits `scheme://host[:port][/path]`; only http and https are accepted.
pub fn split_url(url: &str) -> Option<(String, String, Option<u16>, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, String::new()),
    };
    if authority.is_empty() || authority.contains('@') || authority.contains(char::is_whitespace) {
        return None;
    }
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (host, after) = stripped.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (format!("[{host}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port.parse().ok()?)),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_string(), host, port, path))
}

/// Everything a colony needs to route models to the configured providers.
#[derive(Default)]
pub struct ColonyRoutes {
    pub routes: Vec<Value>,
    pub secrets: Vec<Secret>,
    pub env: Vec<(String, String)>,
    /// A provider runs on this machine, so the colony needs the `host` network profile.
    pub needs_host: bool,
}

pub fn colony_routes(app: &App) -> ColonyRoutes {
    let mut out = ColonyRoutes::default();
    for provider in app.providers() {
        let Some((scheme, host, port, path)) = split_url(provider.base_url.trim_end_matches('/')) else { continue };
        let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "0.0.0.0");
        if loopback {
            out.needs_host = true;
        }
        let colony_host = if loopback { "host.microsandbox.internal" } else { host.as_str() };
        let port = port.map(|p| format!(":{p}")).unwrap_or_default();
        let mut route = json!({
            "provider": provider.id,
            "prefix": format!("{}/", provider.id),
            "base_url": format!("{scheme}://{colony_host}{port}{path}"),
            "auth": provider.auth,
        });
        if provider.auth != "none" {
            if let Some(key) = app.provider_key(&provider.id) {
                let key_env = format!("COLONIZER_PROVIDER_KEY_{}", provider.id.to_uppercase().replace('-', "_"));
                if scheme == "https" {
                    // Substituted by microsandbox's TLS proxy for this host only; the colony sees a placeholder.
                    out.secrets.push(Secret { env: key_env.clone(), value: key, hosts: vec![host.clone()] });
                } else {
                    // Plain-HTTP (local) endpoints can't use TLS substitution, so the key is passed as is.
                    out.env.push((key_env.clone(), key));
                }
                route["key_env"] = json!(key_env);
            }
        }
        out.routes.push(route);
    }
    out
}

fn describe(app: &App, provider: &Provider) -> Value {
    json!({
        "id": provider.id,
        "name": provider.name,
        "base_url": provider.base_url,
        "auth": provider.auth,
        "has_key": app.provider_key(&provider.id).is_some(),
        "models": provider.models,
        "preset": if provider.preset.is_empty() { "custom" } else { provider.preset.as_str() },
    })
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    Json(app.providers().iter().map(|p| describe(&app, p)).collect())
}

#[derive(Deserialize)]
pub struct PutProvider {
    name: String,
    base_url: String,
    #[serde(default = "default_auth")]
    auth: String,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    preset: Option<String>,
    /// Omitted keeps the saved key; an empty string removes it.
    #[serde(default)]
    api_key: Option<String>,
}

fn default_auth() -> String {
    "x-api-key".into()
}

pub async fn put(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<PutProvider>) -> ApiResult<Value> {
    let bad = |message: &str| client_error(StatusCode::BAD_REQUEST, message);
    if !valid_id(&id) {
        return Err(bad("provider ids are lowercase letters, digits and dashes, and can't be \"anthropic\""));
    }
    let name = req.name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err(bad("provider name must be 1-60 characters"));
    }
    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    if base_url.len() > 300 || split_url(&base_url).is_none() {
        return Err(bad("base URL must be an http(s) URL like https://api.deepseek.com/anthropic"));
    }
    if !AUTH_MODES.contains(&req.auth.as_str()) {
        return Err(bad("auth must be x-api-key, bearer or none"));
    }
    let models: Vec<String> = req.models.iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    if models.len() > 50 || !models.iter().all(|m| valid_model(m)) {
        return Err(bad("models must be up to 50 model IDs without spaces"));
    }
    let preset = req.preset.unwrap_or_else(|| "custom".into());
    if !PRESETS.contains(&preset.as_str()) {
        return Err(bad("preset must be deepseek, local or custom"));
    }
    match req.api_key.as_deref().map(str::trim) {
        Some("") => {
            let _ = std::fs::remove_file(app.provider_key_file(&id));
        }
        Some(key) if key.len() > 500 || key.contains(char::is_whitespace) => return Err(bad("that doesn't look like an API key")),
        Some(key) => write_secret(&app.provider_key_file(&id), key)?,
        None => {}
    }

    let provider = Provider { id: id.clone(), name: name.to_string(), base_url, auth: req.auth, models, preset };
    let mut providers = app.providers();
    match providers.iter_mut().find(|p| p.id == id) {
        Some(existing) => *existing = provider.clone(),
        None => providers.push(provider.clone()),
    }
    app.save_providers(&providers)?;
    Ok(Json(describe(&app, &provider)))
}

pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let mut providers = app.providers();
    let before = providers.len();
    providers.retain(|p| p.id != id);
    if providers.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no such provider"));
    }
    app.save_providers(&providers)?;
    if valid_id(&id) {
        let _ = std::fs::remove_file(app.provider_key_file(&id));
    }
    Ok(Json(json!({"ok": true})))
}

pub async fn models(State(app): State<Shared>) -> Json<Vec<Value>> {
    let mut out: Vec<Value> =
        ANTHROPIC_MODELS.iter().map(|(id, label)| json!({"id": id, "label": label, "provider": "anthropic"})).collect();
    for provider in app.providers() {
        for model in &provider.models {
            out.push(json!({
                "id": format!("{}/{model}", provider.id),
                "label": format!("{model} · {}", provider.name),
                "provider": provider.id,
            }));
        }
    }
    Json(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_split_and_validated() {
        assert_eq!(
            split_url("https://api.deepseek.com/anthropic"),
            Some(("https".into(), "api.deepseek.com".into(), None, "/anthropic".into()))
        );
        assert_eq!(split_url("http://127.0.0.1:8080"), Some(("http".into(), "127.0.0.1".into(), Some(8080), String::new())));
        assert_eq!(split_url("http://[::1]:9000/v1"), Some(("http".into(), "[::1]".into(), Some(9000), "/v1".into())));
        assert!(split_url("ftp://example.com").is_none());
        assert!(split_url("https://user:pass@example.com").is_none());
        assert!(split_url("https://example.com:notaport").is_none());
    }

    #[test]
    fn ids_and_models_are_validated() {
        assert!(valid_id("deepseek"));
        assert!(!valid_id("anthropic"));
        assert!(!valid_id("Deep Seek"));
        assert!(valid_model("deepseek-ai/DeepSeek-V4.1-Flash"));
        assert!(!valid_model("has space"));
    }
}
