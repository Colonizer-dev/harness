//! Org workspaces: per-GitHub-org defaults layered over the global module settings.

use crate::{
    ApiResult, App, Shared, client_error,
    config::{ModuleChoice, ModulesConfig, setting_u64},
    modules::schema_for,
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

/// Every field is optional; `None` inherits the global module setting.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct OrgSettings {
    #[serde(default)]
    pub agent: Option<AgentOverrides>,
    #[serde(default)]
    pub max_parallel: Option<u64>,
    #[serde(default)]
    pub memory: Option<MemoryOverrides>,
    #[serde(default)]
    pub watchdog: Option<WatchdogOverrides>,
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
                choice
                    .settings
                    .insert(key.to_string(), Value::String(value.clone()));
            }
        }
        if let Some(skillsets) = agent.skillsets.as_ref().filter(|s| !s.is_empty()) {
            let global = choice
                .settings
                .get("plugins")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut names = crate::plugins::parse_list(global);
            for (name, on) in skillsets {
                if !*on {
                    names.retain(|n| n != name);
                } else if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            choice
                .settings
                .insert("plugins".into(), Value::String(names.join(",")));
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

pub fn effective_memory_enabled(modules: &ModulesConfig, org: &OrgSettings) -> bool {
    let global = modules.memory.enabled
        && modules
            .memory
            .settings
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    org.memory
        .as_ref()
        .and_then(|m| m.enabled)
        .unwrap_or(global)
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
        stall_minutes: overrides
            .stall_minutes
            .unwrap_or_else(|| number("stall_minutes"))
            .max(1),
        max_nudges: overrides.max_nudges.unwrap_or_else(|| number("max_nudges")),
        waiting_minutes: overrides
            .waiting_minutes
            .unwrap_or_else(|| number("waiting_minutes"))
            .max(1),
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
    if settings
        .max_parallel
        .is_some_and(|n| !(1..=32).contains(&n))
    {
        return Err("parallel limit must be between 1 and 32".into());
    }
    if let Some(watchdog) = &settings.watchdog {
        if watchdog
            .stall_minutes
            .is_some_and(|n| !(1..=1440).contains(&n))
        {
            return Err("stall minutes must be between 1 and 1440".into());
        }
        if watchdog
            .waiting_minutes
            .is_some_and(|n| !(1..=10080).contains(&n))
        {
            return Err("waiting minutes must be between 1 and 10080".into());
        }
        if watchdog.max_nudges.is_some_and(|n| n > 20) {
            return Err("max nudges must be 20 or fewer".into());
        }
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
                let live = sessions
                    .iter()
                    .filter(|s| s.org == org && s.status.is_live())
                    .count();
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

pub async fn put(
    State(app): State<Shared>,
    Path(org): Path<String>,
    Json(req): Json<PutOrg>,
) -> ApiResult<Value> {
    if !valid_org(&org) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "invalid GitHub org name",
        ));
    }
    validate(&req.settings).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    let mut req = req;
    // No skillset overrides is the same as inheriting all of them.
    if let Some(agent) = req.settings.agent.as_mut()
        && agent.skillsets.as_ref().is_some_and(BTreeMap::is_empty)
    {
        agent.skillsets = None;
    }
    let mut all = app.all_org_settings();
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
        assert_eq!(
            agent.settings.get("subagent_model"),
            Some(&json!("deepseek/deepseek-flash"))
        );
        assert!(!agent.settings.contains_key("model"));

        let watchdog = effective_watchdog(&modules, &org);
        assert_eq!(watchdog.stall_minutes, 5);
        assert_eq!(watchdog.max_nudges, 3);
        assert!(watchdog.enabled);
        assert_eq!(
            effective_watchdog(&modules, &OrgSettings::default()).stall_minutes,
            15
        );

        assert!(effective_memory_enabled(&modules, &OrgSettings::default()));
        let disabled = OrgSettings {
            memory: Some(MemoryOverrides {
                enabled: Some(false),
            }),
            ..Default::default()
        };
        assert!(!effective_memory_enabled(&modules, &disabled));
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
    fn org_skillsets_switch_single_skillsets_on_and_off_over_the_global_list() {
        let mut modules = ModulesConfig::default();
        modules
            .agent
            .settings
            .insert("plugins".into(), json!("ecc, team-skills"));
        let org = |names: &[(&str, bool)]| OrgSettings {
            agent: Some(AgentOverrides {
                skillsets: Some(names.iter().map(|(n, on)| (n.to_string(), *on)).collect()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let plugins = |org: &OrgSettings| {
            effective_agent(&modules, org)
                .settings
                .get("plugins")
                .cloned()
        };

        // Unnamed skillsets follow the global switches, in the global order.
        assert_eq!(
            plugins(&org(&[("ecc", false), ("google-skills", true)])),
            Some(json!("team-skills,google-skills"))
        );
        // Switching on what is already on changes nothing.
        assert_eq!(
            plugins(&org(&[("ecc", true)])),
            Some(json!("ecc,team-skills"))
        );
        // No overrides leaves the global setting exactly as the operator wrote it.
        assert_eq!(
            plugins(&OrgSettings::default()),
            Some(json!("ecc, team-skills"))
        );
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
