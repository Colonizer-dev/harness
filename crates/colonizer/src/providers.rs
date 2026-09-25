//! Model providers: endpoints that colonies can route models to, such as DeepSeek's Anthropic-compatible
//! API, OpenAI (the `openai` wire, translated by openai.rs), or a model served on this machine or the
//! operator's tailnet. Colonies reach them
//! through the mothership's provider gateway (gateway.rs), so keys stay here (0600) and never enter a
//! colony.

use crate::{
    ApiResult, App, Shared, client_error, config_unreadable,
    gateway::{COLONY_HEADER, DEFAULT_TIMEOUT_SECS, forget_probe, health},
    orgs::effective_agent,
    provider_quota,
    sessions::agent_env,
    util::{delete_secret, read_secret, write_secret},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
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

/// The token counts of one routed response, in Anthropic's terms, so both wires account on one scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub thinking_tokens: u64,
}

impl Usage {
    /// Anthropic's shape, the one colonies and session records speak.
    pub fn json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_input_tokens": self.cache_read_tokens,
            "cache_creation_input_tokens": self.cache_write_tokens,
        })
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens + self.thinking_tokens
    }
}

/// Dollars per million tokens, so the gateway can turn routed usage into spend and hold a colony to its
/// budget. A rate left at `0` prices that kind of token at nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    #[serde(default)]
    pub input_per_mtok: f64,
    #[serde(default)]
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: f64,
    #[serde(default)]
    pub cache_write_per_mtok: f64,
    #[serde(default)]
    pub thinking_per_mtok: f64,
}

impl Pricing {
    /// What one response costs at these rates. The rates are per million tokens.
    pub fn cost_usd(&self, usage: Usage) -> f64 {
        (usage.input_tokens as f64 * self.input_per_mtok
            + usage.output_tokens as f64 * self.output_per_mtok
            + usage.cache_read_tokens as f64 * self.cache_read_per_mtok
            + usage.cache_write_tokens as f64 * self.cache_write_per_mtok
            + usage.thinking_tokens as f64 * self.thinking_per_mtok)
            / 1_000_000.0
    }
}

/// Per-provider dialect quirks: what one endpoint rejects that the Anthropic wire otherwise allows.
/// Data, not branches: the next dialect gap becomes a row in [`PRESET_QUIRKS`], consulted at proxy
/// time, instead of another `if id == ...` in gateway.rs. Keyed by the provider's `preset` (the
/// catalogue id it was added from), so a hand-pointed custom endpoint keeps default behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderQuirks {
    /// Drop `ttl` from `cache_control` blocks (`{"type":"ephemeral","ttl":…}` → `{"type":"ephemeral"}`).
    pub strip_cache_ttl: bool,
    /// Floor for `max_tokens`; a request below it is raised to it. Meta answers 400 below 16.
    pub min_max_tokens: Option<u64>,
}

impl ProviderQuirks {
    /// Whether any rewrite applies: without quirks the gateway keeps the body byte-identical.
    pub fn needs_normalize(self) -> bool {
        self.strip_cache_ttl || self.min_max_tokens.is_some()
    }
}

/// One row per preset with a known dialect gap. `custom` (and anything unlisted) gets defaults.
const PRESET_QUIRKS: &[(&str, ProviderQuirks)] = &[(
    "meta",
    ProviderQuirks {
        strip_cache_ttl: true,
        min_max_tokens: Some(16),
    },
)];

/// The quirks for a preset id, or defaults when the preset has no known gaps.
pub fn quirks_for_preset(preset: &str) -> ProviderQuirks {
    PRESET_QUIRKS
        .iter()
        .find(|(id, _)| *id == preset)
        .map(|(_, quirks)| *quirks)
        .unwrap_or_default()
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
    /// Dollars per million tokens on this endpoint. Unset (or all `0`) means routed requests still count
    /// their tokens but cost, and spend, nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    /// Strip `ttl` from `cache_control` blocks on top of whatever the preset's quirks say. The preset
    /// is a UI label, not a capability, so a hand-pointed endpoint (preset `custom`) carrying this flag
    /// normalizes exactly like its catalogue twin; without it, a `custom` preset keeps default behaviour.
    #[serde(default)]
    pub normalize_cache_ttl: bool,
    /// Whether an operator has vetted this provider to receive restricted-sensitivity work — secrets,
    /// `.env` files, infra config (sensitivity.rs, issue #472). Defaults to `false`: a provider is not
    /// trusted with a colony's secrets just because it is configured, and "not marked trusted" is a safe
    /// default for a field nobody has set yet.
    #[serde(default)]
    pub trusted: bool,
}

impl Provider {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)
    }

    pub fn queue_timeout_secs(&self) -> u64 {
        self.queue_timeout_secs.unwrap_or_else(|| self.timeout_secs())
    }

    /// This provider's dialect quirks: its preset's row, plus the explicit per-provider flag. See
    /// [`ProviderQuirks`].
    pub fn quirks(&self) -> ProviderQuirks {
        let mut quirks = quirks_for_preset(self.preset.as_str());
        if self.normalize_cache_ttl {
            quirks.strip_cache_ttl = true;
        }
        quirks
    }

    /// What one routed response costs here: $0 when the provider has no pricing configured, whose tokens
    /// are still counted.
    pub fn cost_usd(&self, usage: Usage) -> f64 {
        self.pricing.map_or(0.0, |pricing| pricing.cost_usd(usage))
    }
}

/// Models served by Anthropic with the Claude login, offered as suggestions in model pickers.
const ANTHROPIC_MODELS: &[(&str, &str)] = &[
    ("opus", "Claude Opus (latest)"),
    ("sonnet", "Claude Sonnet (latest)"),
    ("haiku", "Claude Haiku (latest)"),
    ("fable", "Claude Fable (latest)"),
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-opus-5", "Claude Opus 5"),
    ("claude-sonnet-5", "Claude Sonnet 5"),
    ("claude-haiku-4-5", "Claude Haiku 4.5"),
];

const AUTH_MODES: [&str; 3] = ["x-api-key", "bearer", "none"];
/// The preset a provider was added from: a label for the UI, not a capability. The catalogue names
/// dozens, so this is checked for shape rather than against a list.
fn valid_preset(preset: &str) -> bool {
    !preset.is_empty()
        && preset.len() <= 48
        && preset
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The model env vars a colony can be pointed at, matched against the configured providers. The
/// first three reach the runner as is; the two tier models are the mothership's per-task routing and
/// are stripped from the runner env before launch, so only `used_by` reads them — counting the tier
/// settings in the module's configured env, while a colony booted onto a tier meets the tier's
/// provider through the substituted `COLONIZER_MODEL`. Order matches the settings array in [`used_by`].
const MODEL_VARS: [&str; 5] = [
    "COLONIZER_MODEL",
    "COLONIZER_SUBAGENT_MODEL",
    "COLONIZER_BACKGROUND_MODEL",
    "COLONIZER_MODEL_LOW",
    "COLONIZER_MODEL_HIGH",
];

impl App {
    fn providers_file(&self) -> PathBuf {
        self.cfg.config_dir.join("providers.json")
    }

    pub(crate) fn provider_key_file(&self, id: &str) -> PathBuf {
        self.cfg.config_dir.join("provider-keys").join(id)
    }

    pub fn providers(&self) -> Vec<Provider> {
        let providers: Vec<Provider> = self.read_config_loud(&self.providers_file(), "model providers");
        self.warn_duplicate_ids(&providers);
        providers
    }

    /// A hand-edited file can list one id twice: the first entry wins and any later one is ignored,
    /// so the loser is named rather than silently shadowed (#326). This reader runs per request, so
    /// the warning goes through the `config_damage` record [`App::read_config_loud`] dedups on
    /// instead of the log — which is also why the message names no file: the strict read clears
    /// that record by file name, and a record it kept clearing would be re-raised (and re-printed)
    /// every request. The slot is shared with the strict read's file-damage alerts, and a file that
    /// will not read at all is the more urgent fault: a warning only ever takes an empty slot or one
    /// already holding a warning of its own, and a read that comes back without the duplicate drops
    /// it here.
    fn warn_duplicate_ids(&self, providers: &[Provider]) {
        let dups = duplicate_provider_ids(providers);
        let mut damage = self.config_damage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if dups.is_empty() {
            if damage.as_ref().is_some_and(|a| a.message.starts_with(DUPLICATE_IDS_WARNING)) {
                *damage = None;
            }
            return;
        }
        let message = format!(
            "{} {} — the first entry wins and any later one is ignored",
            DUPLICATE_IDS_WARNING,
            dups.join(", ")
        );
        match damage.as_ref() {
            Some(a) if a.message == message => return,
            // Not ours: a file-damage alert holds the slot and takes precedence until its own
            // reader clears it, so this warning stays log-less rather than hiding it.
            Some(a) if !a.message.starts_with(DUPLICATE_IDS_WARNING) => return,
            _ => {}
        }
        eprintln!("providers: {message}");
        *damage = Some(crate::StorageAlert {
            kind: crate::StorageAlertKind::LoadDamage,
            message,
            ts: chrono::Utc::now(),
            failures: 1,
            recovered_at: None,
        });
    }

    async fn save_providers(&self, providers: &[Provider]) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cfg.config_dir)?;
        crate::util::write_atomic(&self.providers_file(), &serde_json::to_vec_pretty(providers)?).await
    }

    pub fn provider_key(&self, id: &str) -> Option<String> {
        read_secret(&self.provider_key_file(id))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id != "anthropic"
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What a duplicate-ids warning starts with, so the load only ever clears or replaces its own
/// damage record and never the strict read's file-damage alerts.
const DUPLICATE_IDS_WARNING: &str = "these providers are listed more than once:";

/// The ids `providers` lists more than once, in first-appearance order: the load keeps the first
/// entry of each, the PUT refuses to save over the shadow, and both name them (#326).
fn duplicate_provider_ids(providers: &[Provider]) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut dups: Vec<String> = Vec::new();
    for provider in providers {
        if seen.iter().any(|id| *id == provider.id) {
            if !dups.iter().any(|id| id == &provider.id) {
                dups.push(provider.id.clone());
            }
        } else {
            seen.push(&provider.id);
        }
    }
    dups
}

pub(crate) fn valid_model(model: &str) -> bool {
    !model.is_empty() && model.len() <= 120 && model.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/[]".contains(c))
}

/// Claude Code resolves aliases itself, but a fallback request goes to the API as is, so it needs a model ID.
pub(crate) fn api_model(model: &str) -> &str {
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
    value
        .split(',')
        .map(str::trim)
        .filter(|beta| !beta.is_empty() && !beta.starts_with("oauth-"))
        .collect::<Vec<_>>()
        .join(",")
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
            .filter(|p| {
                models
                    .iter()
                    .any(|m| m.strip_prefix(p.id.as_str()).is_some_and(|rest| rest.starts_with('/')))
            })
            .cloned()
            .collect()
    }

    /// The first model setting that names a provider nobody configured, as `(value, prefix)` — enough
    /// to refuse the boot with. The same MODEL_VARS walk as [`ColonyRoutes::used`] read the other way
    /// round: a value shaped like `<provider>/<model>` that no configured provider's `id/` matches
    /// would have the runner quietly send those requests to Anthropic, so the boot says no instead.
    /// Bare names and values that don't shape up as a prefix are Claude's own models and pass.
    pub fn unrouted_provider<'a>(&self, runner_env: &'a Map<String, Value>) -> Option<(&'a str, &'a str)> {
        MODEL_VARS
            .iter()
            .filter_map(|var| runner_env.get(*var)?.as_str())
            .find_map(|m| {
                let prefix = provider_prefix(m)?;
                let configured = self
                    .providers
                    .iter()
                    .any(|p| m.strip_prefix(p.id.as_str()).is_some_and(|rest| rest.starts_with('/')));
                (!configured).then_some((m, prefix))
            })
    }
}

/// The `<provider>` half of a `<provider>/<model>` setting, when the value shapes up as one: a leading
/// `[A-Za-z0-9]`, then `[A-Za-z0-9._-]*`, then `/`. Mirrors the runner's PROVIDER_PREFIX
/// (modules/agents/claude-code/router.mjs) so the two stay in step.
fn provider_prefix(model: &str) -> Option<&str> {
    let (prefix, _) = model.split_once('/')?;
    let mut rest = prefix.chars();
    match rest.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return None,
    }
    rest.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)).then_some(prefix)
}

/// The pricing the gateway would actually charge for `model`, if any provider's id prefixes it in
/// `<provider>/<model>` form and that provider has pricing configured. `None` for a bare model name
/// (no gateway involved) or a provider with no pricing on file.
pub(crate) fn pricing_for(providers: &[Provider], model: &str) -> Option<Pricing> {
    let prefix = provider_prefix(model)?;
    providers.iter().find(|p| p.id == prefix)?.pricing
}

pub fn colony_routes(app: &App, gateway_token: &str) -> ColonyRoutes {
    let port = app.cfg.gateway_bind.port();
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

/// The model settings (`model`, `subagent_model`, `background_model`, `model_low`, `model_high`)
/// whose resolved value — schema default, global setting or org override — routes to this provider as
/// `<provider-id>/<model>`, named for a human, e.g. `["subagent_model"]`. Empty means no model setting
/// points at it. A bare alias or a partial id prefix is Claude's or another provider's model, so it
/// doesn't match, same rule as [`ColonyRoutes::used`].
fn used_by(provider_id: &str, envs: &[Map<String, Value>]) -> Vec<&'static str> {
    let mut used: Vec<&'static str> = Vec::new();
    let settings = ["model", "subagent_model", "background_model", "model_low", "model_high"];
    for env in envs {
        for (var, setting) in MODEL_VARS.iter().zip(settings) {
            let points_here = env
                .get(*var)
                .and_then(Value::as_str)
                .is_some_and(|m| m.strip_prefix(provider_id).is_some_and(|rest| rest.starts_with('/')));
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
    let Some(agent) = app.agents.iter().find(|a| a.id == "claude-code") else {
        return Vec::new();
    };
    let modules = app.modules.read().await;
    let mut envs = vec![agent_env(agent, &modules.agent)];
    for org in app.all_org_settings().into_values() {
        envs.push(agent_env(agent, &effective_agent(&modules, &org)));
    }
    envs
}

fn describe(app: &App, provider: &Provider, envs: &[Map<String, Value>]) -> Value {
    let (in_flight, queued) = app.gateway.load(&provider.id);
    let usage = app.gateway.usage(&provider.id);
    // An exhausted plan degrades the provider whatever its failure rate says: the verdict shares
    // the one health rule every surface reads. The surfaced record shares it too, so a lapsed
    // reset (or TTL) hides the badge at the same instant the provider stops reading degraded.
    let mut health = health(&usage);
    let quota_exhausted = app.gateway.is_quota_exhausted(&provider.id);
    if quota_exhausted {
        health.degraded = true;
    }
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
        "pricing": provider.pricing,
        "normalize_cache_ttl": provider.normalize_cache_ttl,
        "in_flight": in_flight,
        "queued": queued,
        "usage": usage,
        "health": health,
        "used_by": used_by(&provider.id, envs),
        "quota_exhausted": if quota_exhausted {
            app.gateway.quota_state(&provider.id).map(|q| json!({"reset_at": q.reset_at, "reset_unix": q.reset_unix}))
        } else {
            None::<Value>
        },
    })
}

/// Quota exhaustion across providers for the status poll and the queue (issue #225): whether every
/// routable provider is out, and the earliest reset when it is.
pub(crate) struct QuotaStatus {
    pub paused: bool,
    pub reason: Option<String>,
    pub reset_at: Option<String>,
    pub reset_unix: Option<i64>,
    pub providers: Vec<String>,
    /// Which scope the pause covers: the Claude account's own cap (`"account"`) or named exhausted
    /// providers (`"provider"`). `None` when the queue is not paused. The cockpit honors it when
    /// present and otherwise derives the scope from `providers`, so older web builds keep working.
    pub kind: Option<String>,
}

pub(crate) async fn quota_status(app: &Shared) -> QuotaStatus {
    let waiting = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status == crate::sessions::SessionStatus::Queued)
        .count();
    // The Claude account's own cap pauses the queue on its own record — even with no providers
    // configured — and names no real provider as exhausted.
    if app.gateway.is_account_quota_exhausted() {
        let record = app.gateway.account_quota_state();
        let pause = provider_quota::account_pause(
            record.as_ref().and_then(|q| q.reset_at.clone()),
            record.as_ref().and_then(|q| q.reset_unix),
            waiting,
        );
        return QuotaStatus {
            paused: true,
            reason: Some(pause.reason),
            reset_at: pause.reset_at,
            reset_unix: pause.reset_unix,
            providers: pause.providers,
            kind: Some("account".to_string()),
        };
    }
    let envs = runner_envs(app).await;
    let providers = app.providers();
    let exhausted = app.gateway.quota_exhausted();
    let states: Vec<provider_quota::ProviderQuota> = providers
        .iter()
        .map(|p| {
            let hit = exhausted.iter().find(|(id, _, _)| id == &p.id);
            provider_quota::ProviderQuota {
                id: p.id.clone(),
                exhausted: hit.is_some(),
                reset_at: hit.and_then(|(_, reset, _)| reset.clone()),
                reset_unix: hit.and_then(|(_, _, unix)| *unix),
                routable: !used_by(&p.id, &envs).is_empty(),
            }
        })
        .collect();
    match provider_quota::quota_pause(&states, waiting) {
        Some(pause) => QuotaStatus {
            paused: true,
            reason: Some(pause.reason),
            reset_at: pause.reset_at,
            reset_unix: pause.reset_unix,
            providers: pause.providers,
            kind: Some("provider".to_string()),
        },
        None => QuotaStatus {
            paused: false,
            reason: None,
            reset_at: None,
            reset_unix: None,
            providers: Vec::new(),
            kind: None,
        },
    }
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
    /// Omitted keeps the saved pricing, like the key: a Settings save from a web build that predates the
    /// field must not quietly stop a budget from counting. All-`0` rates are how a caller clears it,
    /// which also is exactly what "no pricing" means, so nothing becomes unreachable.
    #[serde(default)]
    pricing: Option<Pricing>,
    /// Omitted keeps the saved flag, like pricing: an older save must not quietly re-enable the 400s
    /// this flag suppresses.
    #[serde(default)]
    normalize_cache_ttl: Option<bool>,
    /// Omitted keeps the saved trust mark (sensitivity.rs, issue #472): a Settings save from a web
    /// build that predates the field must not quietly un-vet a restricted-capable provider.
    #[serde(default)]
    trusted: Option<bool>,
}

fn default_auth() -> String {
    "x-api-key".into()
}

fn in_range(value: Option<u64>, min: u64, max: u64) -> bool {
    value.is_none_or(|v| (min..=max).contains(&v))
}

/// A price is a dollar amount per million tokens: finite, never negative.
fn valid_price(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

/// The gateway appends the request's own path to an anthropic-wire base_url (e.g. `/v1/messages`, and
/// `/v1/models` for the health probe), so a base already ending in `/v1` doubles it and 404s silently
/// until the first real call surfaces it. `openai`-wire providers are unaffected: their base_url
/// legitimately ends in `/v1` (e.g. xai-grok), since the translator appends `/chat/completions` itself.
fn base_url_needs_stripping(base_url: &str, wire: Wire) -> bool {
    wire == Wire::Anthropic && base_url.ends_with("/v1")
}

pub async fn put(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<PutProvider>) -> ApiResult<Value> {
    let bad = |message: &str| client_error(StatusCode::BAD_REQUEST, message);
    if !valid_id(&id) {
        return Err(bad(
            "provider ids are lowercase letters, digits and dashes, and can't be \"anthropic\"",
        ));
    }
    let name = req.name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err(bad("provider name must be 1-60 characters"));
    }
    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    if base_url.len() > 300 || split_url(&base_url).is_none() {
        return Err(bad("base URL must be an http(s) URL like https://api.deepseek.com/anthropic"));
    }
    if base_url_needs_stripping(&base_url, req.wire) {
        return Err(bad(
            "base URL for an Anthropic-wire provider must not end in /v1 — the gateway appends its own \
             path (e.g. /v1/messages); strip the trailing /v1",
        ));
    }
    if !AUTH_MODES.contains(&req.auth.as_str()) {
        return Err(bad("auth must be x-api-key, bearer or none"));
    }
    let models: Vec<String> = req
        .models
        .iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    if models.len() > 50 || !models.iter().all(|m| valid_model(m)) {
        return Err(bad("models must be up to 50 model IDs without spaces"));
    }
    let preset = req.preset.unwrap_or_else(|| "custom".into());
    if !valid_preset(&preset) {
        return Err(bad(
            "preset ids are lowercase letters, digits and dashes, up to 48 characters",
        ));
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
    let pricing_ok = req.pricing.as_ref().is_none_or(|p| {
        [
            p.input_per_mtok,
            p.output_per_mtok,
            p.cache_read_per_mtok,
            p.cache_write_per_mtok,
        ]
        .iter()
        .all(|rate| valid_price(*rate))
    });
    if !pricing_ok {
        return Err(bad("pricing rates must be dollar amounts per million tokens, zero or more"));
    }
    let fallback_model = req.fallback_model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
    if fallback_model.as_deref().is_some_and(|m| !valid_model(m) || m.contains('/')) {
        return Err(bad("fallback model must be a Claude model such as sonnet or claude-sonnet-5"));
    }
    // The whole read-modify-write of providers.json — the api-key secret and the pricing and
    // normalize-cache-ttl keep-lookups included — is one critical section over a strict read
    // (#408): a file that will not parse is refused before the key is written or deleted, rather
    // than silently replaced by defaults, the keep-lookups read the locked copy instead of racing
    // a concurrent save, and two saves at once cannot each lose the other's provider. The guard
    // is dropped at the save: the cache and env work below it reads other state.
    let _config = app.config_write.lock().await;
    let mut providers: Vec<Provider> =
        crate::util::read_json_or_default(&app.providers_file()).map_err(|e| config_unreadable(&app.providers_file(), &e))?;
    // A hand-edited file can list this id twice: the first-match update below would refresh the
    // first entry and leave the stale shadow in place, so the save is refused naming the id
    // instead (#326). Deleting the provider removes every entry with the id, which is the way out.
    if duplicate_provider_ids(&providers).iter().any(|dup| dup == &id) {
        return Err(bad(&format!(
            "provider \"{id}\" is listed more than once in providers.json; delete it and add it again (or remove the duplicate by hand), then save"
        )));
    }
    match req.api_key.as_deref().map(str::trim) {
        Some("") => {
            delete_secret(&app.provider_key_file(&id));
        }
        Some(key) if key.len() > 500 || key.contains(char::is_whitespace) => {
            return Err(bad("that doesn't look like an API key"));
        }
        Some(key) => write_secret(&app.provider_key_file(&id), key)?,
        None => {}
    }

    // Omitted keeps the saved pricing: a Settings save from a web build that predates the field must not
    // quietly stop a budget from counting. An all-`0` object clears it in effect, so nothing is unreachable.
    let pricing = match req.pricing {
        Some(pricing) => Some(pricing),
        None => providers.iter().find(|p| p.id == id).and_then(|p| p.pricing),
    };

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
        pricing,
        normalize_cache_ttl: req.normalize_cache_ttl.unwrap_or_else(|| {
            providers
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.normalize_cache_ttl)
                .unwrap_or(false)
        }),
        trusted: req
            .trusted
            .unwrap_or_else(|| providers.iter().find(|p| p.id == id).map(|p| p.trusted).unwrap_or(false)),
    };
    match providers.iter_mut().find(|p| p.id == id) {
        Some(existing) => *existing = provider.clone(),
        None => providers.push(provider.clone()),
    }
    app.save_providers(&providers).await?;
    drop(_config);
    // The probe cache key carries no credential, so a rotated key or a changed auth mode /
    // endpoint would otherwise keep serving the old answer for up to the probe TTL.
    forget_probe(&app, &id).await;
    let envs = runner_envs(&app).await;
    Ok(Json(describe(&app, &provider, &envs)))
}

pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    // One critical section over a strict read (#408): a providers.json that will not parse is
    // refused rather than silently replaced by the remainder, and a concurrent put cannot race
    // the retain.
    let _config = app.config_write.lock().await;
    let mut providers: Vec<Provider> =
        crate::util::read_json_or_default(&app.providers_file()).map_err(|e| config_unreadable(&app.providers_file(), &e))?;
    let before = providers.len();
    providers.retain(|p| p.id != id);
    if providers.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no such provider"));
    }
    app.save_providers(&providers).await?;
    drop(_config);
    if valid_id(&id) {
        delete_secret(&app.provider_key_file(&id));
    }
    // A provider that no longer exists must not keep its usage record forever.
    app.gateway.forget_usage(&id);
    app.gateway.forget_quota(&id);
    // Nor its probe answer: a later provider reusing the id must be probed fresh.
    forget_probe(&app, &id).await;
    Ok(Json(json!({"ok": true})))
}

pub async fn models(State(app): State<Shared>) -> Json<Vec<Value>> {
    let mut out: Vec<Value> = ANTHROPIC_MODELS
        .iter()
        .map(|(id, label)| json!({"id": id, "label": label, "provider": "anthropic"}))
        .collect();
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
            pricing: None,
            normalize_cache_ttl: false,
            trusted: false,
        }
    }

    #[test]
    fn explicit_flag_strips_ttl_even_on_a_custom_preset() {
        let mut p = provider("meta-handpointed");
        p.preset = "custom".into();
        assert!(!p.quirks().strip_cache_ttl, "custom preset alone normalizes nothing");
        p.normalize_cache_ttl = true;
        assert!(p.quirks().strip_cache_ttl, "the explicit flag must win over the preset row");
        p.preset = "meta".into();
        p.normalize_cache_ttl = false;
        assert!(p.quirks().strip_cache_ttl, "the preset row still applies on its own");
    }

    #[test]
    fn routed_tokens_are_priced_per_million_and_unpriced_providers_cost_nothing() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_read_tokens: 2_000_000,
            cache_write_tokens: 0,
            thinking_tokens: 200_000,
        };
        let pricing = Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.3,
            cache_write_per_mtok: 3.75,
            thinking_per_mtok: 5.0,
        };
        let cost = pricing.cost_usd(usage);
        assert!(
            (cost - (3.0 + 7.5 + 0.6 + 1.0)).abs() < 1e-9,
            "3 in + 0.5 out at 15 + 2 cache read at 0.3 + 0.2 thinking at 5, got {cost}"
        );
        // Cached reads are priced separately from fresh input: the same million tokens twice, once each way.
        let fresh = Usage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let cached = Usage {
            cache_read_tokens: 1_000_000,
            ..Default::default()
        };
        let one_rate = Pricing {
            input_per_mtok: 1.0,
            cache_read_per_mtok: 0.1,
            ..Default::default()
        };
        assert!((one_rate.cost_usd(fresh) - 1.0).abs() < 1e-9);
        assert!(
            (one_rate.cost_usd(cached) - 0.1).abs() < 1e-9,
            "{:?}",
            one_rate.cost_usd(cached)
        );

        let unpriced = provider("local");
        assert_eq!(
            unpriced.cost_usd(usage),
            0.0,
            "no pricing configured: tokens counted, dollars none"
        );
        assert_eq!(Usage::default().total_tokens(), 0);
        assert_eq!(usage.total_tokens(), 3_700_000, "thinking tokens count toward the total too");
    }

    /// The cost gate (issue #470) prices models through this lookup, so the shapes it must tell apart
    /// matter: a routed `<provider>/<model>` reaches its provider's pricing, a bare alias reaches
    /// nothing, and a provider with no pricing on file prices nothing either.
    #[test]
    fn pricing_for_resolves_a_prefixed_model_and_nothing_else() {
        let mut priced = provider("deepseek");
        priced.pricing = Some(Pricing {
            input_per_mtok: 0.27,
            output_per_mtok: 1.1,
            ..Default::default()
        });
        let providers = vec![provider("strix"), priced];

        // Extra slashes are the provider's model id, as in `ColonyRoutes::used`.
        assert_eq!(
            pricing_for(&providers, "deepseek/deepseek-ai/DeepSeek-V4.1-Flash"),
            Some(Pricing {
                input_per_mtok: 0.27,
                output_per_mtok: 1.1,
                ..Default::default()
            })
        );
        // A bare alias or ID never routes through the gateway, so nothing prices it.
        assert_eq!(pricing_for(&providers, "opus"), None);
        // A prefix no configured provider answers to.
        assert_eq!(pricing_for(&providers, "unknown/model"), None);
        // The provider is found but carries no pricing on file.
        assert_eq!(pricing_for(&providers, "strix/qwen3"), None);
    }

    /// providers.json on disk predates `pricing`, and a Settings save from an older web build omits it.
    /// Either one must leave the provider priced as it was, not silently free.
    #[test]
    fn a_provider_saved_before_pricing_deserialises_without_it() {
        let saved = r#"{"id":"deepseek","name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic","auth":"x-api-key"}"#;
        let provider: Provider = serde_json::from_str(saved).unwrap();
        assert_eq!(provider.pricing, None);

        let priced: Provider = serde_json::from_str(
            r#"{"id":"deepseek","name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic","auth":"x-api-key","pricing":{"input_per_mtok":0.27,"output_per_mtok":1.1}}"#,
        )
        .unwrap();
        assert_eq!(
            priced.pricing,
            Some(Pricing {
                input_per_mtok: 0.27,
                output_per_mtok: 1.1,
                ..Default::default()
            })
        );
        assert_eq!(
            priced.cost_usd(Usage {
                input_tokens: 1_000_000,
                ..Default::default()
            }),
            0.27
        );
    }

    #[test]
    fn prices_must_be_amounts_never_negatives_or_infinities() {
        assert!(valid_price(0.0), "a zero rate prices that token kind at nothing");
        assert!(valid_price(3.0));
        assert!(!valid_price(-0.01));
        assert!(!valid_price(f64::NAN));
        assert!(!valid_price(f64::INFINITY));
    }

    #[test]
    fn urls_are_split_and_validated() {
        assert_eq!(
            split_url("https://api.deepseek.com/anthropic"),
            Some(("https".into(), "api.deepseek.com".into(), None, "/anthropic".into()))
        );
        assert_eq!(
            split_url("http://127.0.0.1:8080"),
            Some(("http".into(), "127.0.0.1".into(), Some(8080), String::new()))
        );
        assert_eq!(
            split_url("http://[::1]:9000/v1"),
            Some(("http".into(), "[::1]".into(), Some(9000), "/v1".into()))
        );
        assert!(split_url("ftp://example.com").is_none());
        assert!(split_url("https://user:pass@example.com").is_none());
        assert!(split_url("https://example.com:notaport").is_none());
    }

    /// An anthropic-wire base_url ending in `/v1` doubles up with the path the gateway appends
    /// (`/v1/messages`, and `/v1/models` for the health probe) and 404s silently. `openai`-wire
    /// providers legitimately end in `/v1` (e.g. the xai-grok catalog entry), since the translator
    /// appends `/chat/completions` itself, so the check only applies to `wire: anthropic`.
    #[test]
    fn an_anthropic_wire_base_url_ending_in_v1_is_rejected() {
        assert!(base_url_needs_stripping("https://api.example.com/v1", Wire::Anthropic));
        assert!(!base_url_needs_stripping(
            "https://api.example.com/anthropic",
            Wire::Anthropic
        ));
        assert!(
            !base_url_needs_stripping("https://api.x.ai/v1", Wire::Openai),
            "an openai-wire provider legitimately ends in /v1"
        );
    }

    /// providers.json on disk predates `wire`, and a Settings save from an older web build omits it.
    /// Either one deserialising as anything but `anthropic` would silently reroute a working provider
    /// into the (unimplemented) translator.
    #[test]
    fn a_provider_without_a_wire_is_anthropic() {
        let saved = r#"{"id":"deepseek","name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic","auth":"x-api-key"}"#;
        let provider: Provider = serde_json::from_str(saved).unwrap();
        assert_eq!(provider.wire, Wire::Anthropic);

        let put: PutProvider =
            serde_json::from_str(r#"{"name":"DeepSeek","base_url":"https://api.deepseek.com/anthropic"}"#).unwrap();
        assert_eq!(put.wire, Wire::Anthropic);

        assert_eq!(serde_json::to_value(Wire::Openai).unwrap(), serde_json::json!("openai"));
    }

    #[test]
    fn preset_ids_are_checked_for_shape_not_membership() {
        // The catalogue names dozens of vendors, and its longest id today is 34 characters.
        for ok in [
            "custom",
            "deepseek",
            "kimi-for-coding",
            "9527code",
            "tencent-token-plan-enterprise-lite",
        ] {
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
    fn quirks_are_data_keyed_by_preset_not_provider_branches() {
        assert_eq!(
            quirks_for_preset("meta"),
            ProviderQuirks {
                strip_cache_ttl: true,
                min_max_tokens: Some(16),
            }
        );
        assert_eq!(quirks_for_preset("custom"), ProviderQuirks::default());
        assert_eq!(quirks_for_preset(""), ProviderQuirks::default());
        assert_eq!(quirks_for_preset("deepseek"), ProviderQuirks::default());
        assert!(quirks_for_preset("meta").needs_normalize());
        assert!(!ProviderQuirks::default().needs_normalize());

        let mut meta = provider("meta");
        meta.preset = "meta".into();
        assert!(meta.quirks().needs_normalize());
        assert!(!provider("deepseek").quirks().needs_normalize());
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
        let routes = ColonyRoutes {
            routes: vec![],
            providers: vec![provider("strix"), provider("str"), provider("deepseek")],
        };
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("opus"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strix/deepseek-v4-flash"));
        env.insert("COLONIZER_EFFORT".into(), json!("deepseek/not-a-model-var"));
        let used: Vec<String> = routes.used(&env).into_iter().map(|p| p.id).collect();
        assert_eq!(used, vec!["strix"]);
    }

    #[test]
    fn unrouted_providers_are_reported_with_the_value_and_prefix() {
        let routes = ColonyRoutes {
            routes: vec![],
            providers: vec![provider("strix"), provider("str")],
        };
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("strix/qwen3"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strixish/qwen"));
        // "strixish" shares "strix" as a partial id prefix but is its own provider, so it is the
        // first setting naming a provider nobody configured.
        assert_eq!(routes.unrouted_provider(&env), Some(("strixish/qwen", "strixish")));

        // The runner warns per setting; the boot refuses on the first one it would meet.
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("locall/deepseek-flash"));
        env.insert("COLONIZER_BACKGROUND_MODEL".into(), json!("deepseek/v4"));
        assert_eq!(routes.unrouted_provider(&env), Some(("locall/deepseek-flash", "locall")));

        // `anthropic` can never be a provider id (valid_id refuses it), so it is always unrouted.
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("anthropic/claude-x"));
        assert_eq!(routes.unrouted_provider(&env), Some(("anthropic/claude-x", "anthropic")));

        // An empty providers list leaves every prefixed setting unrouted.
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL_LOW".into(), json!("deepseek/v4"));
        assert_eq!(
            ColonyRoutes::default().unrouted_provider(&env),
            Some(("deepseek/v4", "deepseek"))
        );
    }

    #[test]
    fn configured_prefixes_and_bare_names_pass_the_unrouted_check() {
        let routes = ColonyRoutes {
            routes: vec![],
            providers: vec![provider("deepseek"), provider("str")],
        };
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("deepseek/deepseek-ai/DeepSeek-V4.1-Flash"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("str/llama"));
        env.insert("COLONIZER_BACKGROUND_MODEL".into(), json!("opus"));
        assert_eq!(
            routes.unrouted_provider(&env),
            None,
            "a configured id/ prefix matches at the first slash, extra slashes included"
        );

        // Bare aliases and full Claude ids never look like a provider route.
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("claude-opus-5-5"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("us.anthropic.claude-opus-5-5"));
        env.insert("COLONIZER_BACKGROUND_MODEL".into(), json!("fable"));
        assert_eq!(routes.unrouted_provider(&env), None);

        // Shapes the runner's PROVIDER_PREFIX rejects are Claude's to interpret, not ours.
        let mut env = Map::new();
        env.insert("COLONIZER_MODEL".into(), json!("/leading-slash"));
        env.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("two words/x"));
        env.insert("COLONIZER_BACKGROUND_MODEL".into(), json!(".hidden/x"));
        assert_eq!(routes.unrouted_provider(&env), None);
    }

    #[test]
    fn used_by_names_the_settings_that_point_at_a_provider() {
        let mut global = Map::new();
        global.insert("COLONIZER_MODEL".into(), json!("opus"));
        global.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strix/deepseek-v4-flash"));
        global.insert("COLONIZER_BACKGROUND_MODEL".into(), json!("str/llama"));
        let mut org = Map::new();
        org.insert("COLONIZER_MODEL".into(), json!("strix/qwen3"));
        assert_eq!(
            used_by("strix", &[global.clone(), org]),
            vec!["subagent_model", "model"],
            "an org override counts"
        );
        // "strix/deepseek-v4-flash" shares "str" as a prefix but only "str/llama" is provider str's model.
        assert_eq!(used_by("str", &[global.clone()]), vec!["background_model"]);

        // A bare alias is a Claude model and a partial id prefix is another provider's, so neither matches.
        let mut aliases = Map::new();
        aliases.insert("COLONIZER_MODEL".into(), json!("strix"));
        aliases.insert("COLONIZER_SUBAGENT_MODEL".into(), json!("strixish/qwen"));
        assert_eq!(used_by("strix", &[aliases]), Vec::<&str>::new());
    }

    // -- the strict providers.json rule (#408) -----------------------------------------------------

    fn providers_app() -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-providers-{}", crate::util::short_id()));
        (crate::tests::test_app(&root), root)
    }

    fn put_req(name: &str) -> PutProvider {
        PutProvider {
            name: name.into(),
            base_url: "https://api.deepseek.com/anthropic".into(),
            auth: "none".into(),
            wire: Wire::Anthropic,
            models: Vec::new(),
            preset: None,
            api_key: None,
            timeout_secs: None,
            max_concurrent: None,
            queue_timeout_secs: None,
            context_tokens: None,
            fallback_model: None,
            pricing: None,
            normalize_cache_ttl: None,
            trusted: None,
        }
    }

    fn damaged_providers(app: &crate::Shared, bytes: &[u8]) -> PathBuf {
        let path = app.cfg.config_dir.join("providers.json");
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[tokio::test]
    async fn a_put_over_a_damaged_providers_json_is_refused_and_leaves_the_bytes_alone() {
        let (app, root) = providers_app();
        let damaged = b"][ nope";
        let path = damaged_providers(&app, damaged);

        let mut req = put_req("DeepSeek");
        req.api_key = Some("sk-live-123".into()); // a refused save must not touch the key either
        let err = put(State(app.clone()), Path("deepseek".into()), Json(req)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(
            err.message().contains("providers.json") && err.message().contains("refusing to overwrite it"),
            "{}",
            err.message()
        );
        assert_eq!(std::fs::read(&path).unwrap(), damaged, "the file is not overwritten");
        assert!(
            !app.provider_key_file("deepseek").exists(),
            "the key from the refused save is not persisted"
        );

        // The gateway's reader still answers — with defaults — and the damage reaches /api/status.
        assert!(app.providers().is_empty());
        let alert = app.config_damage.lock().unwrap().clone().unwrap();
        assert_eq!(alert.kind, crate::StorageAlertKind::LoadDamage);
        assert!(alert.message.contains("providers.json"), "{}", alert.message);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_delete_over_a_damaged_providers_json_is_refused_and_leaves_the_bytes_alone() {
        let (app, root) = providers_app();
        let path = damaged_providers(&app, b"nonsense");

        let err = delete(State(app.clone()), Path("deepseek".into())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(err.message().contains("refusing to overwrite it"), "{}", err.message());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nonsense");
        let _ = std::fs::remove_dir_all(root);
    }

    // -- duplicated ids in a hand-edited providers.json (#326) --------------------------------------

    /// Two entries answering to one id, as a hand edit leaves them.
    fn duplicated_file(app: &crate::Shared) -> PathBuf {
        let path = app.cfg.config_dir.join("providers.json");
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(
            &path,
            r#"[
                {"id": "dupped", "name": "First", "base_url": "https://api.example.com", "auth": "none"},
                {"id": "dupped", "name": "Second", "base_url": "https://api.example.com", "auth": "none"}
            ]"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn duplicate_ids_are_found_in_first_appearance_order() {
        let dups = duplicate_provider_ids(&[provider("a"), provider("b"), provider("a"), provider("b"), provider("a")]);
        assert_eq!(dups, ["a", "b"]);
        assert!(duplicate_provider_ids(&[provider("a"), provider("b")]).is_empty());
    }

    #[tokio::test]
    async fn a_duplicated_load_keeps_the_first_entry_and_names_the_loser() {
        let (app, root) = providers_app();
        duplicated_file(&app);

        let providers = app.providers();
        assert_eq!(providers.len(), 2, "the list is not deduped; consumers take the first match");
        assert_eq!(providers[0].name, "First");
        let alert = app.config_damage.lock().unwrap().clone().unwrap();
        assert_eq!(alert.kind, crate::StorageAlertKind::LoadDamage);
        assert!(
            alert
                .message
                .starts_with("these providers are listed more than once: dupped — ")
                && alert.message.contains("the first entry wins"),
            "{}",
            alert.message
        );

        // Fixing the file clears the warning instead of leaving it stuck on.
        std::fs::write(app.cfg.config_dir.join("providers.json"), "[]").unwrap();
        app.providers();
        assert!(
            app.config_damage.lock().unwrap().is_none(),
            "the fixed file clears the damage"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_put_over_a_duplicated_id_is_refused_naming_it() {
        let (app, root) = providers_app();
        let path = duplicated_file(&app);

        let err = put(State(app.clone()), Path("dupped".into()), Json(put_req("Second")))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert!(
            err.message().contains("\"dupped\"") && err.message().contains("more than once"),
            "{}",
            err.message()
        );
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("First"),
            "the file is not touched by the refused save"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_duplicate_warning_never_displaces_a_file_damage_alert() {
        let (app, root) = providers_app();
        // What a failed orgs.json read leaves in the slot the two readers share.
        *app.config_damage.lock().unwrap() = Some(crate::StorageAlert {
            kind: crate::StorageAlertKind::LoadDamage,
            message: "/config/orgs.json could not be read (…); defaults are in effect for org settings".into(),
            ts: chrono::Utc::now(),
            failures: 1,
            recovered_at: None,
        });
        duplicated_file(&app);

        app.providers();
        let held = app.config_damage.lock().unwrap().clone().unwrap();
        assert!(
            held.message.starts_with("/config/orgs.json"),
            "the file-damage alert takes precedence: {}",
            held.message
        );

        // …and once the file damage is gone, the warning takes the vacated slot.
        *app.config_damage.lock().unwrap() = None;
        app.providers();
        let held = app.config_damage.lock().unwrap().clone().unwrap();
        assert!(held.message.starts_with(DUPLICATE_IDS_WARNING), "{}", held.message);
        let _ = std::fs::remove_dir_all(root);
    }
}
