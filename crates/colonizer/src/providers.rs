//! Model providers: endpoints that colonies can route models to, such as DeepSeek's Anthropic-compatible
//! API, OpenAI (the `openai` wire, translated by openai.rs), or a model served on this machine or the
//! operator's tailnet. Colonies reach them
//! through the mothership's provider gateway (gateway.rs), so keys stay here (0600) and never enter a
//! colony.

use crate::{
    client_error,
    gateway::{DEFAULT_TIMEOUT_SECS, COLONY_HEADER},
    orgs::effective_agent,
    sessions::agent_env,
    util::{read_trimmed, write_secret},
    ApiResult, App, Shared,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// The protocol an endpoint speaks. An `anthropic` endpoint is proxied byte-for-byte; an `openai` one
/// has to be translated in both directions, so the wire is recorded per provider rather than guessed
/// from the URL. Providers saved before this field existed deserialise as `anthropic`, unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    #[default]
    Anthropic,
    Openai,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub auth: String,
    #[serde(default)]
    pub wire: Wire,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub preset: String,
    /// Headers and body-idle timeout for one request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Requests at once across all colonies; more wait in the gateway's queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// Anthropic model used when the provider is unreachable, times out or its queue is full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_model: Option<String>,
}

impl Provider {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)
    }

    pub fn queue_timeout_secs(&self) -> u64 {
        self.queue_timeout_secs.unwrap_or_else(|| self.timeout_secs())
    }
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
/// The preset a provider was added from: a label for the UI, not a capability. The catalogue names
/// dozens, so this is checked for shape rather than against a list.
fn valid_preset(preset: &str) -> bool {
    !preset.is_empty() && preset.len() <= 48 && preset.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The runner reads these to decide which routes a colony actually uses.
const MODEL_VARS: [&str; 3] = ["COLONIZER_MODEL", "COLONIZER_SUBAGENT_MODEL", "COLONIZER_BACKGROUND_MODEL"];

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

/// Claude Code resolves aliases itself, but a fallback request goes to the API as is, so it needs a model ID.
fn api_model(model: &str) -> &str {
    match model {
        "opus" => "claude-opus-5",
        "sonnet" => "claude-sonnet-5",
        "haiku" => "claude-haiku-4-5",
        "fable" => "claude-fable-5-1",
        other => other,
    }
}

/// Removes OAuth capability betas, which only Anthropic understands.
pub fn strip_oauth_betas(value: &str) -> String {
    value.split(',').map(str::trim).filter(|beta| !beta.is_empty() && !beta.starts_with("oauth-")).collect::<Vec<_>>().join(",")
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

/// Everything a colony needs to route models through the gateway.
#[derive(Default)]
pub struct ColonyRoutes {
    pub routes: Vec<Value>,
    pub providers: Vec<Provider>,
}

impl ColonyRoutes {
    /// Providers the colony's model settings actually point at.
    pub fn used(&self, runner_env: &Map<String, Value>) -> Vec<Provider> {
        let models: Vec<&str> = MODEL_VARS.iter().filter_map(|var| runner_env.get(*var)?.as_str()).collect();
        self.providers
            .iter()
            .filter(|p| models.iter().any(|m| m.strip_prefix(p.id.as_str()).is_some_and(|rest| rest.starts_with('/'))))
            .cloned()
            .collect()
    }
}

pub fn colony_routes(app: &App, gateway_token: &str) -> ColonyRoutes {
    let port = app.cfg.gateway_bind.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41750);
    let providers = app.providers();
    let routes = providers
        .iter()
        .map(|provider| {
            json!({
                "provider": provider.id,
                "prefix": format!("{}/", provider.id),
                "base_url": format!("http://host.microsandbox.internal:{port}/providers/{}", provider.id),
                "auth": "none",
                "headers": {COLONY_HEADER: gateway_token},
                "timeout_secs": provider.timeout_secs(),
                "context_tokens": provider.context_tokens,
                "fallback_model": provider.fallback_model.as_deref().map(api_model),
            })
        })
        .collect();
    ColonyRoutes { routes, providers }
}

/// The model settings (`model`, `subagent_model`, `background_model`) whose resolved value — schema
/// default, global setting or org override — routes to this provider as `<provider-id>/<model>`, named
/// for a human, e.g. `["subagent_model"]`. Empty means no model setting points at it. A bare alias or a
/// partial id prefix is Claude's or another provider's model, so it doesn't match, same rule as
/// [`ColonyRoutes::used`].
fn used_by(provider_id: &str, envs: &[Map<String, Value>]) -> Vec<&'static str> {
    let mut used: Vec<&'static str> = Vec::new();
    for env in envs {
        for (var, setting) in MODEL_VARS.iter().zip(["model", "subagent_model", "background_model"]) {
            let points_here =
                env.get(*var).and_then(Value::as_str).is_some_and(|m| m.strip_prefix(provider_id).is_some_and(|rest| rest.starts_with('/')));
            if points_here && !used.contains(&setting) {
                used.push(setting);
            }
        }
    }
    used
}

/// The claude-code runner env for the global agent settings (schema defaults layered under
/// modules.json), plus one per org that overrides them: every configuration a colony could start with.
async fn runner_envs(app: &App) -> Vec<Map<String, Value>> {
    let Some(agent) = app.agents.iter().find(|a| a.id == "claude-code") else { return Vec::new() };
    let modules = app.modules.read().await;
    let mut envs = vec![agent_env(agent, &modules.agent)];
    for org in app.all_org_settings().into_values() {
        envs.push(agent_env(agent, &effective_agent(&modules, &org)));
    }
    envs
}

fn describe(app: &App, provider: &Provider, envs: &[Map<String, Value>]) -> Value {
    let (in_flight, queued) = app.gateway.load(&provider.id);
    json!({
        "id": provider.id,
        "name": provider.name,
        "base_url": provider.base_url,
        "auth": provider.auth,
        "wire": provider.wire,
        "has_key": app.provider_key(&provider.id).is_some(),
        "models": provider.models,
        "preset": if provider.preset.is_empty() { "custom" } else { provider.preset.as_str() },
        "timeout_secs": provider.timeout_secs(),
        "max_concurrent": provider.max_concurrent,
        "queue_timeout_secs": provider.queue_timeout_secs,
        "context_tokens": provider.context_tokens,
        "fallback_model": provider.fallback_model,
        "in_flight": in_flight,
        "queued": queued,
        "usage": app.gateway.usage(&provider.id),
        "used_by": used_by(&provider.id, envs),
    })
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    let envs = runner_envs(&app).await;
    Json(app.providers().iter().map(|p| describe(&app, p, &envs)).collect())
}

#[derive(Deserialize)]
pub struct PutProvider {
    name: String,
    base_url: String,
    #[serde(default = "default_auth")]
    auth: String,
    /// Omitted means `anthropic`, so a client that predates the field cannot flip an existing provider.
    #[serde(default)]
    wire: Wire,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    preset: Option<String>,
    /// Omitted keeps the saved key; an empty string removes it.
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    max_concurrent: Option<u64>,
    #[serde(default)]
    queue_timeout_secs: Option<u64>,
    #[serde(default)]
    context_tokens: Option<u64>,
    #[serde(default)]
    fallback_model: Option<String>,
}

fn default_auth() -> String {
    "x-api-key".into()
}

fn in_range(value: Option<u64>, min: u64, max: u64) -> bool {
    value.is_none_or(|v| (min..=max).contains(&v))
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
    if !valid_preset(&preset) {
        return Err(bad("preset ids are lowercase letters, digits and dashes, up to 48 characters"));
    }
    if !in_range(req.timeout_secs, 30, 3600) {
        return Err(bad("request timeout must be 30-3600 seconds"));
    }
    if !in_range(req.max_concurrent, 1, 64) {
        return Err(bad("max concurrent requests must be 1-64, or empty for no limit"));
    }
    if !in_range(req.queue_timeout_secs, 1, 3600) {
        return Err(bad("queue timeout must be 1-3600 seconds"));
    }
    if !in_range(req.context_tokens, 1024, 2_000_000) {
        return Err(bad("context window must be 1,024-2,000,000 tokens"));
    }
    let fallback_model = req.fallback_model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
    if fallback_model.as_deref().is_some_and(|m| !valid_model(m) || m.contains('/')) {
        return Err(bad("fallback model must be a Claude model such as sonnet or claude-sonnet-5"));
    }
    match req.api_key.as_deref().map(str::trim) {
        Some("") => {
            let _ = std::fs::remove_file(app.provider_key_file(&id));
        }
        Some(key) if key.len() > 500 || key.contains(char::is_whitespace) => return Err(bad("that doesn't look like an API key")),
        Some(key) => write_secret(&app.provider_key_file(&id), key)?,
        None => {}
    }

    let provider = Provider {
        id: id.clone(),
        name: name.to_string(),
        base_url,
        auth: req.auth,
        wire: req.wire,
        models,
        preset,
        timeout_secs: req.timeout_secs,
        max_concurrent: req.max_concurrent,
        queue_timeout_secs: req.queue_timeout_secs,
        context_tokens: req.context_tokens,
        fallback_model,
    };
    let mut providers = app.providers();
    match providers.iter_mut().find(|p| p.id == id) {
        Some(existing) => *existing = provider.clone(),
        None => providers.push(provider.clone()),
    }
    app.save_providers(&providers)?;
    let envs = runner_envs(&app).await;
    Ok(Json(describe(&app, &provider, &envs)))
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
    // A provider that no longer exists must not keep its usage record forever.
    app.gateway.forget_usage(&id);
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

    fn provider(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: "http://100.80.225.14:8000".into(),
            auth: "none".into(),
            wire: Wire::Anthropic,
            models: vec![],
            preset: "local".into(),
            timeout_secs: None,
            max_concurrent: None,
            queue_timeout_secs: None,
            context_tokens: None,
            fallback_model: None,
        }
    }

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

    /// providers.json on disk predates `wire`, and a Settings save from an older web build omits it.
    /// Either one deserialising as anything but `anthropic` would silently reroute a working provider
    /// into the (unimplemented) translator.
    #[test]
    fn a_provider_without_a_wire_is_anthropic() {
        let saved = r#"{"id":"deepseek","name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic","auth":"x-api-key"}"#;
        let provider: Provider = serde_json::from_str(saved).unwrap();
        assert_eq!(provider.wire, Wire::Anthropic);

        let put: PutProvider = serde_json::from_str(r#"{"name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic"}"#).unwrap();
        assert_eq!(put.wire, Wire::Anthropic);

        assert_eq!(serde_json::to_value(Wire::Openai).unwrap(), serde_json::json!("openai"));
    }

    #[test]
    fn preset_ids_are_checked_for_shape_not_membership() {
        // The catalogue names dozens of vendors, and its longest id today is 34 characters.
        for ok in ["custom", "deepseek", "kimi-for-coding", "9527code", "tencent-token-plan-enterprise-lite"] {
            assert!(valid_preset(ok), "{ok}");
        }
        for bad in ["", "Custom", "has space", "under_score", &"x".repeat(49)] {
            assert!(!valid_preset(bad), "{bad}");
        }
    }

    #[test]
    fn ids_and_models_are_validated() {
        assert!(valid_id("deepseek"));
        assert!(!valid_id("anthropic"));
        assert!(!valid_id("Deep Seek"));
        assert!(valid_model("deepseek-ai/DeepSeek-V4.1-Flash"));
        assert!(!valid_model("has space"));
    }

    #[test]
    fn timeouts_default_and_aliases_resolve() {
        let mut p = provider("strix");
        assert_eq!((p.timeout_secs(), p.queue_timeout_secs()), (600, 600));
        p.timeout_secs = Some(900);
        assert_eq!(p.queue_timeout_secs(), 900);
        p.queue_timeout_secs = Some(30);
        assert_eq!(p.queue_timeout_secs(), 30);
        assert_eq!(api_model("sonnet"), "claude-sonnet-5");
        assert_eq!(api_model("claude-opus-5"), "claude-opus-5");
        assert_eq!(strip_oauth_betas("oauth-2025-04-20, a ,b"), "a,b");
    }

    #[test]
    fn used_providers_follow_the_model_settings() {
        let routes = ColonyRoutes { routes: vec![], providers: vec![provider("strix"), provider("str"), provider("deepseek")] };
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("opus"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strix/deepseek-v4-flash"));
        env.insert("COLONIZER_EFFORT".into(), json!("deepseek/not-a-model-var"));
        let used: Vec<String> = routes.used(&env).into_iter().map(|p| p.id).collect();
        assert_eq!(used, vec!["strix"]);
    }

    #[test]
    fn used_by_names_the_settings_that_point_at_a_provider() {
        let mut global = Map::new();
        global.insert("COLONIZER_MODEL".into(), json!("opus"));
        global.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strix/deepseek-v4-flash"));
        global.insert("COLONIZER_BACKGROUND_MODEL".into(), json!("str/llama"));
        let mut org = Map::new();
        org.insert("COLONIZER_MODEL".into(), json!("strix/qwen3"));
        assert_eq!(used_by("strix", &[global.clone(), org]), vec!["subagent_model", "model"], "an org override counts");
        // "strix/deepseek-v4-flash" shares "str" as a prefix but only "str/llama" is provider str's model.
        assert_eq!(used_by("str", &[global.clone()]), vec!["background_model"]);

        // A bare alias is a Claude model and a partial id prefix is another provider's, so neither matches.
        let mut aliases = Map::new();
        aliases.insert("COLONIZER_MODEL".into(), json!("strix"));
        aliases.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strixish/qwen"));
        assert_eq!(used_by("strix", &[aliases]), Vec::<&str>::new());
    }
}
