//! Org workspaces: per-GitHub-org defaults layered over the global module settings.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModuleChoice, ModulesConfig, setting, setting_f64, setting_str, setting_u64},
    modules::schema_for,
    notify::NotifySettings,
    util::parse_disk_size,
    watchdog::WatchdogSettings,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentOverrides {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub subagent_model: Option<String>,
    #[serde(default)]
    pub background_model: Option<String>,
    /// Skillsets (plugin directories) this org switches on (`true`) or off (`false`) on top of the global
    /// `plugins` setting. A skillset it doesn't name follows the global switch.
    #[serde(default)]
    pub skillsets: Option<BTreeMap<String, bool>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WatchdogOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub stall_minutes: Option<u64>,
    #[serde(default)]
    pub max_nudges: Option<u64>,
    #[serde(default)]
    pub waiting_minutes: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NotifyOverrides {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub on_question: Option<bool>,
    #[serde(default)]
    pub on_attention: Option<bool>,
    #[serde(default)]
    pub on_failed: Option<bool>,
    #[serde(default)]
    pub on_pull_request: Option<bool>,
    #[serde(default)]
    pub desktop: Option<bool>,
    #[serde(default)]
    pub webhook_url: Option<String>,
}

/// Every field is optional; `None` inherits the global module setting.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct OrgSettings {
    #[serde(default)]
    pub agent: Option<AgentOverrides>,
    #[serde(default)]
    pub max_parallel: Option<u64>,
    /// Dollars one colony of this org may spend on models in total, Claude and routed together. `0`
    /// opts the org out of a global budget; `None` inherits the sandbox module's `budget_usd`.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// The most disk one colony of this org may leave on the host, as a size like `16G`. `0` opts the org
    /// out of a global quota; `None` inherits the sandbox module's `host_disk`.
    #[serde(default)]
    pub host_disk: Option<String>,
    /// The sandbox stack this org's colonies boot, pinning what the global `preset` would otherwise
    /// choose. `None` inherits the sandbox module's `preset`.
    #[serde(default)]
    pub stack: Option<String>,
    #[serde(default)]
    pub memory: Option<MemoryOverrides>,
    #[serde(default)]
    pub watchdog: Option<WatchdogOverrides>,
    #[serde(default)]
    pub notify: Option<NotifyOverrides>,
}

pub fn valid_org(org: &str) -> bool {
    !org.is_empty() && org.len() <= 39 && org.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl App {
    fn orgs_file(&self) -> PathBuf {
        self.cfg.config_dir.join("orgs.json")
    }

    pub fn all_org_settings(&self) -> BTreeMap<String, OrgSettings> {
        std::fs::read(self.orgs_file())
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn org_settings(&self, org: &str) -> OrgSettings {
        self.all_org_settings().remove(org).unwrap_or_default()
    }

    fn save_org_settings(&self, all: &BTreeMap<String, OrgSettings>) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.cfg.config_dir)?;
        let path = self.orgs_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// The agent module choice with the org's model and skillset overrides applied.
pub fn effective_agent(modules: &ModulesConfig, org: &OrgSettings) -> ModuleChoice {
    let mut choice = modules.agent.clone();
    if let Some(agent) = &org.agent {
        for (key, value) in [
            ("model", &agent.model),
            ("subagent_model", &agent.subagent_model),
            ("background_model", &agent.background_model),
        ] {
            if let Some(value) = value {
                choice.settings.insert(key.to_string(), Value::String(value.clone()));
            }
        }
        if let Some(skillsets) = agent.skillsets.as_ref().filter(|s| !s.is_empty()) {
            let global = choice.settings.get("plugins").and_then(Value::as_str).unwrap_or_default();
            let mut names = crate::plugins::parse_list(global);
            for (name, on) in skillsets {
                if !*on {
                    names.retain(|n| n != name);
                } else if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            choice.settings.insert("plugins".into(), Value::String(names.join(",")));
        }
    }
    choice
}

/// The mothership-wide colony limit from the sandbox module.
pub fn global_max_parallel(modules: &ModulesConfig) -> u64 {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    setting_u64(&modules.sandbox, &schema, "max_parallel").max(1)
}

/// An org's own colony limit, if it sets one. The global limit always applies as well.
pub fn org_max_parallel(org: &OrgSettings) -> Option<u64> {
    org.max_parallel.map(|n| n.max(1))
}

/// The mothership-wide per-colony spend budget from the sandbox module, in dollars. The default is `0`:
/// unlike cpus or memory there is no dollar figure the harness can pick for someone else's deployment,
/// and a default that silently stopped running colonies on upgrade would be a surprise.
pub fn global_budget_usd(modules: &ModulesConfig) -> f64 {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    setting_f64(&modules.sandbox, &schema, "budget_usd").max(0.0)
}

/// An org's own budget, if it sets one — `Some(0.0)` included, so an org can opt out of a global budget.
pub fn org_budget_usd(org: &OrgSettings) -> Option<f64> {
    org.budget_usd.map(|n| n.max(0.0))
}

/// The budget one colony's whole model spend answers to: the org's own if it set one, else the
/// mothership default. `0` means unlimited.
pub fn budget_usd(modules: &ModulesConfig, org: &OrgSettings) -> f64 {
    org_budget_usd(org).unwrap_or_else(|| global_budget_usd(modules))
}

/// Which budget that is, for messages about it: the org's own, or the mothership default.
pub fn budget_source(org: &OrgSettings) -> &'static str {
    if org.budget_usd.is_some() {
        "the org's own budget"
    } else {
        "the default budget"
    }
}

/// The mothership-wide per-colony host-disk quota from the sandbox module, in bytes. The default is `0`:
/// like the spend budget, how much disk a colony deserves is a decision about someone else's deployment,
/// so colonies are unlimited until the operator names a size.
pub fn global_host_disk(modules: &ModulesConfig) -> u64 {
    let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
    parse_disk_size(&setting_str(&modules.sandbox, &schema, "host_disk")).unwrap_or(0)
}

/// An org's own host-disk quota, if it sets one — `Some("0")` included, so an org can opt out of a global
/// quota. A hand-edited unparseable value counts as not set, so the global quota still applies.
pub fn org_host_disk(org: &OrgSettings) -> Option<u64> {
    org.host_disk.as_deref().and_then(parse_disk_size)
}

/// The host-disk quota one colony's host footprint answers to: the org's own if it set one, else the
/// mothership default. In bytes; `0` means unlimited.
pub fn host_disk(modules: &ModulesConfig, org: &OrgSettings) -> u64 {
    org_host_disk(org).unwrap_or_else(|| global_host_disk(modules))
}

/// Which quota that is, for messages about it: the org's own, or the mothership default.
pub fn host_disk_source(org: &OrgSettings) -> &'static str {
    if org_host_disk(org).is_some() {
        "the org's own host-disk quota"
    } else {
        "the default host-disk quota"
    }
}

/// The sandbox stack one of this org's colonies boots: the org's own pin when it set a non-blank one,
/// else the sandbox module's `preset`. `auto` at either level still means detect the stack from the
/// repository — a pin of `auto` is how an org opts into detection when the install is pinned to
/// something concrete.
pub fn effective_stack(modules: &ModulesConfig, schema: &Value, org: &OrgSettings) -> String {
    match org.stack.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(stack) => stack.to_string(),
        None => setting_str(&modules.sandbox, schema, "preset"),
    }
}

pub fn effective_memory_enabled(modules: &ModulesConfig, org: &OrgSettings) -> bool {
    let global = modules.memory.enabled
        && modules
            .memory
            .settings
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    org.memory.as_ref().and_then(|m| m.enabled).unwrap_or(global)
}

pub fn memory_requires_review(modules: &ModulesConfig) -> bool {
    modules
        .memory
        .settings
        .get("require_review")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub fn effective_watchdog(modules: &ModulesConfig, org: &OrgSettings) -> WatchdogSettings {
    let schema = schema_for("watchdog", &modules.watchdog.provider, &[]);
    let number = |key: &str| setting_u64(&modules.watchdog, &schema, key);
    let global_enabled = modules.watchdog.enabled
        && modules
            .watchdog
            .settings
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    let overrides = org.watchdog.clone().unwrap_or_default();
    WatchdogSettings {
        enabled: overrides.enabled.unwrap_or(global_enabled),
        stall_minutes: overrides.stall_minutes.unwrap_or_else(|| number("stall_minutes")).max(1),
        max_nudges: overrides.max_nudges.unwrap_or_else(|| number("max_nudges")),
        waiting_minutes: overrides.waiting_minutes.unwrap_or_else(|| number("waiting_minutes")).max(1),
    }
}

/// The notify module's settings for an org's colonies. There is nothing to resolve until the module
/// has been configured: an org can narrow what announces, never switch the module on by itself.
pub fn effective_notify(modules: &ModulesConfig, org: &OrgSettings) -> NotifySettings {
    let schema = schema_for("notify", "default", &[]);
    let empty = ModuleChoice {
        provider: "default".into(),
        enabled: false,
        settings: serde_json::Map::new(),
    };
    let choice = modules.notify.as_ref().unwrap_or(&empty);
    let flag = |key: &str, default: bool| setting(choice, &schema, key).and_then(Value::as_bool).unwrap_or(default);
    let overrides = org.notify.clone().unwrap_or_default();
    NotifySettings {
        enabled: choice.enabled && overrides.enabled.unwrap_or(true),
        on_question: overrides.on_question.unwrap_or_else(|| flag("on_question", true)),
        on_attention: overrides.on_attention.unwrap_or_else(|| flag("on_attention", true)),
        on_failed: overrides.on_failed.unwrap_or_else(|| flag("on_failed", true)),
        on_pull_request: overrides.on_pull_request.unwrap_or_else(|| flag("on_pull_request", true)),
        desktop: overrides.desktop.unwrap_or_else(|| flag("desktop", false)),
        webhook_url: overrides
            .webhook_url
            .clone()
            .unwrap_or_else(|| setting_str(choice, &schema, "webhook_url")),
    }
}

fn validate(settings: &OrgSettings) -> Result<(), String> {
    if let Some(agent) = &settings.agent {
        for model in [&agent.model, &agent.subagent_model, &agent.background_model]
            .into_iter()
            .flatten()
        {
            if model.len() > 120 || model.contains(char::is_whitespace) {
                return Err("model names can't contain spaces or exceed 120 characters".into());
            }
        }
        if let Some(skillsets) = &agent.skillsets
            && (skillsets.len() > 64
                || !skillsets
                    .keys()
                    .all(|name| crate::util::is_plain_name(name) && name.len() <= 64))
        {
            return Err("skillset names are plain directory names, at most 64 of them".into());
        }
    }
    if settings.max_parallel.is_some_and(|n| !(1..=32).contains(&n)) {
        return Err("parallel limit must be between 1 and 32".into());
    }
    if settings.budget_usd.is_some_and(|n| !n.is_finite() || n < 0.0) {
        return Err("budget must be 0 or more dollars (0 means no budget)".into());
    }
    if settings.host_disk.as_deref().is_some_and(|s| parse_disk_size(s).is_none()) {
        return Err("host disk must be a size like 512M or 16G (0 means no quota)".into());
    }
    // A blank stack is treated as not set, like the webhook URL's empty string: the resolver reads it
    // as "inherit", so refusing it here would reject a value the harness would have honoured. An id
    // the harness has never heard of, though, would only degrade to schema defaults at boot — say so
    // while the operator is still looking at the form.
    if let Some(stack) = settings.stack.as_deref().filter(|s| !s.trim().is_empty())
        && !crate::presets::ids().contains(&stack)
    {
        return Err(format!("stack must be one of {}", crate::presets::ids().join(", ")));
    }
    if let Some(watchdog) = &settings.watchdog {
        if watchdog.stall_minutes.is_some_and(|n| !(1..=1440).contains(&n)) {
            return Err("stall minutes must be between 1 and 1440".into());
        }
        if watchdog.waiting_minutes.is_some_and(|n| !(1..=10080).contains(&n)) {
            return Err("waiting minutes must be between 1 and 10080".into());
        }
        if watchdog.max_nudges.is_some_and(|n| n > 20) {
            return Err("max nudges must be 20 or fewer".into());
        }
    }
    if let Some(url) = settings.notify.as_ref().and_then(|n| n.webhook_url.as_deref())
        && !url.is_empty()
        && !(url.starts_with("http://") || url.starts_with("https://"))
    {
        return Err("the webhook URL must be an http:// or https:// address".into());
    }
    Ok(())
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    crate::github::refresh_orgs(&app).await;
    let saved = app.all_org_settings();
    let sessions = app.sessions.read().await.clone();
    let mut orgs: BTreeSet<String> = saved.keys().cloned().collect();
    orgs.extend(sessions.iter().map(|s| s.org.clone()));
    orgs.extend(app.repo_owners.read().await.iter().cloned());
    let proposals = app.memory.proposals().await;
    Json(
        orgs.into_iter()
            .filter(|org| valid_org(org))
            .map(|org| {
                let live = sessions.iter().filter(|s| s.org == org && s.status.is_live()).count();
                let total = sessions.iter().filter(|s| s.org == org).count();
                let pending = proposals
                    .iter()
                    .filter(|p| match p.note.scope.as_str() {
                        "org" => p.note.key == org,
                        "repo" => p.note.key.split('/').next() == Some(org.as_str()),
                        _ => false,
                    })
                    .count();
                json!({
                    "org": org,
                    "colonies": {"live": live, "total": total},
                    "pending_memory": pending,
                    "settings": saved.get(&org).cloned().unwrap_or_default(),
                })
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct PutOrg {
    #[serde(default)]
    settings: OrgSettings,
}

/// A settings save replaces the whole object, but a field the client's JSON never names keeps its saved
/// value — and the same inside an object the client does name, for the sub-fields it doesn't. A Settings
/// save from a web build that predates a field must not quietly clear what the operator had set there
/// (the same reason `providers::put` keeps a saved pricing an old build doesn't know about). What the
/// client does name always wins, `null` included: that is how the form says "inherit".
fn keep_unnamed_fields(incoming: &mut OrgSettings, saved: &OrgSettings, raw: Option<&Value>) {
    let named = |field: &str| raw.and_then(|settings| settings.get(field)).is_some();
    let unnamed_sub = |object: &str, field: &str| {
        raw.and_then(|settings| settings.get(object))
            .and_then(|object| object.get(field))
            .is_none()
    };
    if !named("agent") {
        incoming.agent = saved.agent.clone();
    } else if let (Some(saved_agent), Some(agent)) = (saved.agent.as_ref(), incoming.agent.as_mut())
        && unnamed_sub("agent", "skillsets")
    {
        agent.skillsets = saved_agent.skillsets.clone();
    }
    if !named("max_parallel") {
        incoming.max_parallel = saved.max_parallel;
    }
    if !named("budget_usd") {
        incoming.budget_usd = saved.budget_usd;
    }
    if !named("host_disk") {
        incoming.host_disk = saved.host_disk.clone();
    }
    if !named("stack") {
        incoming.stack = saved.stack.clone();
    }
    if !named("memory") {
        incoming.memory = saved.memory.clone();
    }
    if !named("watchdog") {
        incoming.watchdog = saved.watchdog.clone();
    } else if let (Some(saved_watchdog), Some(watchdog)) = (saved.watchdog.as_ref(), incoming.watchdog.as_mut())
        && unnamed_sub("watchdog", "waiting_minutes")
    {
        watchdog.waiting_minutes = saved_watchdog.waiting_minutes;
    }
    if !named("notify") {
        incoming.notify = saved.notify.clone();
    }
}

pub async fn put(State(app): State<Shared>, Path(org): Path<String>, Json(body): Json<Value>) -> ApiResult<Value> {
    if !valid_org(&org) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid GitHub org name"));
    }
    // What the client actually named is gone once `from_value` eats the body, so keep the named fields
    // around for [`keep_unnamed_fields`]. A settings object is a few short strings; cloning is nothing.
    let named = body.get("settings").cloned();
    let req: PutOrg =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("invalid org settings: {e}")))?;
    validate(&req.settings).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    let mut req = req;
    // No skillset overrides is the same as inheriting all of them.
    if let Some(agent) = req.settings.agent.as_mut()
        && agent.skillsets.as_ref().is_some_and(BTreeMap::is_empty)
    {
        agent.skillsets = None;
    }
    let mut all = app.all_org_settings();
    keep_unnamed_fields(
        &mut req.settings,
        all.get(&org).unwrap_or(&OrgSettings::default()),
        named.as_ref(),
    );
    if req.settings == OrgSettings::default() {
        all.remove(&org);
    } else {
        all.insert(org.clone(), req.settings.clone());
    }
    app.save_org_settings(&all)?;
    Ok(Json(json!({"org": org, "settings": req.settings})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_overrides_layer_over_global_settings() {
        let modules = ModulesConfig::default();
        let org = OrgSettings {
            agent: Some(AgentOverrides {
                subagent_model: Some("deepseek/deepseek-flash".into()),
                ..Default::default()
            }),
            watchdog: Some(WatchdogOverrides {
                stall_minutes: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };
        let agent = effective_agent(&modules, &org);
        assert_eq!(agent.settings.get("subagent_model"), Some(&json!("deepseek/deepseek-flash")));
        assert!(!agent.settings.contains_key("model"));

        let watchdog = effective_watchdog(&modules, &org);
        assert_eq!(watchdog.stall_minutes, 5);
        assert_eq!(watchdog.max_nudges, 3);
        assert!(watchdog.enabled);
        assert_eq!(effective_watchdog(&modules, &OrgSettings::default()).stall_minutes, 15);

        assert!(effective_memory_enabled(&modules, &OrgSettings::default()));
        let disabled = OrgSettings {
            memory: Some(MemoryOverrides { enabled: Some(false) }),
            ..Default::default()
        };
        assert!(!effective_memory_enabled(&modules, &disabled));
    }

    #[test]
    fn a_colonys_budget_is_the_orgs_own_when_set_and_zero_means_unlimited() {
        let mut modules = ModulesConfig::default();
        // With nothing set anywhere, no colony is ever over a budget.
        assert_eq!(global_budget_usd(&modules), 0.0);
        assert_eq!(budget_usd(&modules, &OrgSettings::default()), 0.0);
        assert_eq!(budget_source(&OrgSettings::default()), "the default budget");

        modules.sandbox.settings.insert("budget_usd".into(), json!(10.0));
        assert_eq!(budget_usd(&modules, &OrgSettings::default()), 10.0);
        let org = OrgSettings {
            budget_usd: Some(5.5),
            ..Default::default()
        };
        assert_eq!(
            budget_usd(&modules, &org),
            5.5,
            "the org's own budget beats the global default"
        );
        assert_eq!(budget_source(&org), "the org's own budget");
        let opted_out = OrgSettings {
            budget_usd: Some(0.0),
            ..Default::default()
        };
        assert_eq!(
            budget_usd(&modules, &opted_out),
            0.0,
            "an org can opt out of a global budget with 0"
        );
    }

    #[test]
    fn a_colonys_host_disk_quota_is_the_orgs_own_when_set_and_zero_means_unlimited() {
        let mut modules = ModulesConfig::default();
        // With nothing set anywhere, no colony is ever over a quota.
        assert_eq!(global_host_disk(&modules), 0);
        assert_eq!(host_disk(&modules, &OrgSettings::default()), 0);
        assert_eq!(host_disk_source(&OrgSettings::default()), "the default host-disk quota");

        modules.sandbox.settings.insert("host_disk".into(), json!("16G"));
        assert_eq!(host_disk(&modules, &OrgSettings::default()), 16 * 1024 * 1024 * 1024);
        let org = OrgSettings {
            host_disk: Some("512M".into()),
            ..Default::default()
        };
        assert_eq!(
            host_disk(&modules, &org),
            512 * 1024 * 1024,
            "the org's own quota beats the global default"
        );
        assert_eq!(host_disk_source(&org), "the org's own host-disk quota");
        let opted_out = OrgSettings {
            host_disk: Some("0".into()),
            ..Default::default()
        };
        assert_eq!(
            host_disk(&modules, &opted_out),
            0,
            "an org can opt out of a global quota with 0"
        );
        let malformed = OrgSettings {
            host_disk: Some("a lot".into()),
            ..Default::default()
        };
        assert_eq!(
            host_disk(&modules, &malformed),
            16 * 1024 * 1024 * 1024,
            "a hand-edited bad value counts as not set, so the global quota still applies"
        );
        assert_eq!(host_disk_source(&malformed), "the default host-disk quota");
    }

    #[test]
    fn a_colonys_stack_is_the_orgs_own_pin_when_set_and_auto_still_means_detect() {
        let modules = ModulesConfig::default();
        let schema = schema_for("sandbox", &modules.sandbox.provider, &[]);
        // With nothing set anywhere, the stack is the sandbox module's own preset — `auto` now that
        // detection is the visible default.
        assert_eq!(
            effective_stack(&modules, &schema, &OrgSettings::default()),
            crate::presets::AUTO
        );

        let mut global = ModulesConfig::default();
        global.sandbox.settings.insert("preset".into(), json!("rust"));
        assert_eq!(
            effective_stack(&global, &schema, &OrgSettings::default()),
            "rust",
            "an org without a pin inherits the sandbox module's preset"
        );
        let org = OrgSettings {
            stack: Some("go".into()),
            ..Default::default()
        };
        assert_eq!(
            effective_stack(&global, &schema, &org),
            "go",
            "the org's own pin beats the global preset"
        );
        let auto = OrgSettings {
            stack: Some(crate::presets::AUTO.into()),
            ..Default::default()
        };
        assert_eq!(
            effective_stack(&global, &schema, &auto),
            crate::presets::AUTO,
            "a pin of auto is how an org opts into detection over a pinned install"
        );
        let blank = OrgSettings {
            stack: Some("  ".into()),
            ..Default::default()
        };
        assert_eq!(effective_stack(&global, &schema, &blank), "rust", "a blank pin is no pin");
    }

    #[test]
    fn org_settings_are_validated() {
        assert!(
            validate(&OrgSettings {
                max_parallel: Some(0),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            validate(&OrgSettings {
                budget_usd: Some(-1.0),
                ..Default::default()
            })
            .is_err(),
            "a budget can't be negative"
        );
        assert!(
            validate(&OrgSettings {
                budget_usd: Some(0.0),
                ..Default::default()
            })
            .is_ok(),
            "0 is a budget of none"
        );
        assert!(
            validate(&OrgSettings {
                budget_usd: Some(12.5),
                ..Default::default()
            })
            .is_ok()
        );
        assert!(
            validate(&OrgSettings {
                host_disk: Some("a lot".into()),
                ..Default::default()
            })
            .is_err(),
            "a quota must parse as a size"
        );
        assert!(
            validate(&OrgSettings {
                host_disk: Some("16G".into()),
                ..Default::default()
            })
            .is_ok()
        );
        assert!(
            validate(&OrgSettings {
                host_disk: Some("0".into()),
                ..Default::default()
            })
            .is_ok(),
            "0 is no quota"
        );
        assert!(
            validate(&OrgSettings {
                stack: Some("acme-private-stack".into()),
                ..Default::default()
            })
            .is_err(),
            "the stack must be one the harness knows"
        );
        assert!(
            validate(&OrgSettings {
                stack: Some("go".into()),
                ..Default::default()
            })
            .is_ok()
        );
        assert!(
            validate(&OrgSettings {
                stack: Some(crate::presets::AUTO.into()),
                ..Default::default()
            })
            .is_ok(),
            "auto is offered first in Settings, so it is a valid pin"
        );
        assert!(
            validate(&OrgSettings {
                stack: Some("".into()),
                ..Default::default()
            })
            .is_ok(),
            "an empty stack is not set, not an error"
        );
        let bad_model = OrgSettings {
            agent: Some(AgentOverrides {
                model: Some("two words".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate(&bad_model).is_err());
        assert!(valid_org("Colonizer-dev"));
        assert!(!valid_org("../etc"));

        let skillsets = |names: &[(&str, bool)]| OrgSettings {
            agent: Some(AgentOverrides {
                skillsets: Some(names.iter().map(|(n, on)| (n.to_string(), *on)).collect()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate(&skillsets(&[("google-skills", true)])).is_ok());
        assert!(
            validate(&skillsets(&[("../ecc", true)])).is_err(),
            "a skillset is a directory name, never a path"
        );
        assert!(
            validate(&skillsets(&[("a,b", true)])).is_err(),
            "a comma would split into two names in the setting"
        );
    }

    #[test]
    fn notify_settings_layer_over_the_module_like_the_watchdogs() {
        let modules = ModulesConfig::default();
        // Not configured anywhere: nothing announces, whatever the org says — an org narrows, never enables.
        let eager = OrgSettings {
            notify: Some(NotifyOverrides {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!effective_notify(&modules, &eager).enabled);

        let mut configured = ModulesConfig {
            notify: Some(ModuleChoice {
                provider: "default".into(),
                enabled: true,
                settings: serde_json::from_str(
                    r#"{"on_failed": false, "desktop": true, "webhook_url": "https://example.com/hook"}"#,
                )
                .unwrap(),
            }),
            ..Default::default()
        };
        let plain = effective_notify(&configured, &OrgSettings::default());
        assert!(plain.enabled);
        assert!(plain.on_question, "events the module doesn't name default on");
        assert!(!plain.on_failed);
        assert!(plain.desktop);
        assert_eq!(plain.webhook_url, "https://example.com/hook");

        let overridden = OrgSettings {
            notify: Some(NotifyOverrides {
                on_attention: Some(false),
                webhook_url: Some("http://127.0.0.1:9000/hook".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let org = effective_notify(&configured, &overridden);
        assert!(!org.on_attention);
        assert_eq!(org.webhook_url, "http://127.0.0.1:9000/hook");
        assert!(org.desktop, "a field the org doesn't name inherits the module's");
        // A module switched off stays off, however much the org overrides.
        configured.notify.as_mut().unwrap().enabled = false;
        assert!(!effective_notify(&configured, &overridden).enabled);
    }

    #[test]
    fn a_settings_save_only_replaces_the_fields_the_client_names() {
        let saved = OrgSettings {
            agent: Some(AgentOverrides {
                model: Some("opus".into()),
                skillsets: Some(BTreeMap::from([("ecc".to_string(), true)])),
                ..Default::default()
            }),
            max_parallel: Some(4),
            budget_usd: Some(20.0),
            host_disk: Some("16G".into()),
            stack: Some("go".into()),
            watchdog: Some(WatchdogOverrides {
                waiting_minutes: Some(45),
                ..Default::default()
            }),
            notify: Some(NotifyOverrides {
                on_failed: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        // A web build from before the budget and the quota names only the fields it knows: those it sends
        // as null are genuinely cleared, the ones it has never heard of keep their saved values. The form
        // of today still never sends the watchdog's `waiting_minutes`, and that survives the same way.
        let old_build = json!({
            "agent": {"model": null, "subagent_model": null, "background_model": null, "skillsets": null},
            "max_parallel": null,
            "memory": {"enabled": null},
            "watchdog": {"enabled": null, "stall_minutes": null, "max_nudges": null},
        });
        let mut incoming: OrgSettings = serde_json::from_value(old_build.clone()).unwrap();
        keep_unnamed_fields(&mut incoming, &saved, Some(&old_build));
        assert_eq!(
            incoming.budget_usd,
            Some(20.0),
            "a budget the client never heard of survives the save"
        );
        assert_eq!(
            incoming.host_disk.as_deref(),
            Some("16G"),
            "so does a host-disk quota it never heard of"
        );
        assert_eq!(
            incoming.stack.as_deref(),
            Some("go"),
            "and a stack pin, or an older web build's save would silently clear the org's own"
        );
        assert_eq!(
            incoming.max_parallel, None,
            "a field the client names as null is a real request to inherit"
        );
        assert_eq!(
            incoming.agent.map(|a| (a.model, a.skillsets)),
            Some((None, None)),
            "named nulls clear, named values win"
        );
        assert_eq!(
            incoming.watchdog,
            Some(WatchdogOverrides {
                waiting_minutes: Some(45),
                ..Default::default()
            }),
            "a sub-field the client never sends keeps its saved value"
        );
        assert_eq!(
            incoming.notify,
            saved.notify.clone(),
            "so does a whole module a web build from before it has never heard of"
        );

        // A save with no settings object at all — an empty PUT — changes nothing.
        let mut blank: OrgSettings = Default::default();
        keep_unnamed_fields(&mut blank, &saved, None);
        assert_eq!(blank.budget_usd, Some(20.0), "nothing named, nothing replaced");
        assert_eq!(blank.host_disk.as_deref(), Some("16G"));
        assert_eq!(blank.stack, saved.stack);
        assert_eq!(blank.agent, saved.agent);
        assert_eq!(blank.watchdog, saved.watchdog);
    }

    #[test]
    fn org_skillsets_switch_single_skillsets_on_and_off_over_the_global_list() {
        let mut modules = ModulesConfig::default();
        modules.agent.settings.insert("plugins".into(), json!("ecc, team-skills"));
        let org = |names: &[(&str, bool)]| OrgSettings {
            agent: Some(AgentOverrides {
                skillsets: Some(names.iter().map(|(n, on)| (n.to_string(), *on)).collect()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plugins = |org: &OrgSettings| effective_agent(&modules, org).settings.get("plugins").cloned();

        // Unnamed skillsets follow the global switches, in the global order.
        assert_eq!(
            plugins(&org(&[("ecc", false), ("google-skills", true)])),
            Some(json!("team-skills,google-skills"))
        );
        // Switching on what is already on changes nothing.
        assert_eq!(plugins(&org(&[("ecc", true)])), Some(json!("ecc,team-skills")));
        // No overrides leaves the global setting exactly as the operator wrote it.
        assert_eq!(plugins(&OrgSettings::default()), Some(json!("ecc, team-skills")));
        assert_eq!(plugins(&org(&[])), Some(json!("ecc, team-skills")));

        // With nothing on globally (the default), an org can still switch one on.
        let fresh = ModulesConfig::default();
        assert_eq!(
            effective_agent(&fresh, &org(&[("superpowers", true)]))
                .settings
                .get("plugins"),
            Some(&json!("superpowers"))
        );
    }
}
