//! Hot-switching models from the cockpit header (issue #1051): which model each role of each agent
//! module runs on, install-wide and per org, and one atomic switch that can move the running
//! colonies too.
//!
//! The settings a switch changes are the ones Settings already writes — the agent module's
//! (`PUT /api/modules/agent`) for the install, an org's `agent` overrides (`PUT /api/orgs/{org}`)
//! for one org — and the switch saves them through those handlers, so it is validated and written
//! exactly as a Settings save would be. Moving running colonies reuses the provider-out-of-quota
//! card's restart path ([`crate::quota_cards::restart_all`]): their per-colony model overrides are
//! pointed at the new values and they are restarted cold, as the card's switch does.
//!
//! What a role resolves to, and where that comes from, is pure ([`resolve_roles`]), so it is tested
//! without an app; the handlers only read the settings files, the providers and the gateway.

use crate::{
    ApiResult, Shared, client_error,
    config::{ModuleChoice, ModulesConfig, setting_str},
    config_unreadable,
    gateway::health,
    modules::AgentModule,
    orgs::{self, OrgSettings},
    providers, quota_cards,
    sessions::{Session, SessionStatus},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The roles an org can override (`orgs.json` → `agent.{model, subagent_model, background_model}`);
/// every other role is install-wide only.
pub(crate) const ORG_ROLES: [&str; 3] = ["model", "subagent_model", "background_model"];

/// The order roles are listed in: the orchestrator first, then the rest by how often they run.
/// A role a module declares that is not here sorts after these, by name.
const ROLE_ORDER: [&str; 7] = [
    "model",
    "subagent_model",
    "background_model",
    "summary_model",
    "small_model",
    "model_low",
    "model_high",
];

/// Whether a module setting is a model role: a string named `model`, `*_model` or `model_*`.
fn is_role(key: &str, spec: &Value) -> bool {
    spec["type"].as_str() == Some("string") && (key == "model" || key.ends_with("_model") || key.starts_with("model_"))
}

/// The model roles an agent module's schema declares, as `(role, title)`, in [`ROLE_ORDER`].
pub(crate) fn module_roles(schema: &Value) -> Vec<(String, String)> {
    let Some(properties) = schema["properties"].as_object() else {
        return Vec::new();
    };
    let mut roles: Vec<(String, String)> = properties
        .iter()
        .filter(|(key, spec)| is_role(key, spec))
        .map(|(key, spec)| (key.clone(), spec["title"].as_str().unwrap_or(key).to_string()))
        .collect();
    let rank = |role: &str| ROLE_ORDER.iter().position(|r| *r == role).unwrap_or(ROLE_ORDER.len());
    roles.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0)));
    roles
}

/// An org's override for an org role, if it sets one.
fn org_override<'a>(org: &'a OrgSettings, role: &str) -> Option<&'a str> {
    let agent = org.agent.as_ref()?;
    match role {
        "model" => agent.model.as_deref(),
        "subagent_model" => agent.subagent_model.as_deref(),
        "background_model" => agent.background_model.as_deref(),
        _ => None,
    }
}

/// What one role of `module` resolves to, and where it comes from: `org` (the org's override),
/// `install` (the install's agent settings — only for the install's own module, as boot reads
/// them, see [`orgs::effective_agent_for`]) or `default` (the module's schema default; an empty
/// value is the agent's own default). `org` is `None` for the install row.
pub(crate) fn resolve_role(
    modules: &ModulesConfig,
    org: Option<&OrgSettings>,
    module: &str,
    schema: &Value,
    role: &str,
) -> (String, &'static str) {
    if let Some(value) = org.and_then(|o| org_override(o, role)) {
        return (value.to_string(), "org");
    }
    if module == modules.agent.provider
        && let Some(value) = modules.agent.settings.get(role).and_then(Value::as_str)
    {
        return (value.to_string(), "install");
    }
    let bare = ModuleChoice {
        provider: module.to_string(),
        enabled: true,
        settings: Map::new(),
    };
    (setting_str(&bare, schema, role), "default")
}

/// Every role of `module` resolved for the install (`org` `None`) or one org, as the switcher's
/// rows: `{role, title, value, source, org_settable}`.
pub(crate) fn resolve_roles(modules: &ModulesConfig, org: Option<&OrgSettings>, module: &str, schema: &Value) -> Vec<Value> {
    module_roles(schema)
        .into_iter()
        .map(|(role, title)| {
            let (value, source) = resolve_role(modules, org, module, schema, &role);
            json!({
                "role": role,
                "title": title,
                "value": value,
                "source": source,
                "org_settable": ORG_ROLES.contains(&role.as_str()),
            })
        })
        .collect()
}

/// Why `agent` cannot launch on this install, in the words a launch's refusal uses
/// (`sessions::create`): its `requires` preflight on the install's colony image, or a Claude login
/// it needs and the default account does not have.
fn launch_refusal(app: &Shared, modules: &ModulesConfig, agent: &AgentModule) -> Option<String> {
    let sandbox_schema = crate::modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let stack = orgs::effective_stack(modules, &sandbox_schema, &OrgSettings::default());
    let image = crate::sessions::colony_image(&app.agents, modules, &stack);
    if let Err(problem) = crate::modules::check_requires(agent, &image, &crate::modules::harness_staged_binaries(&app.cfg)) {
        return Some(problem);
    }
    if agent.needs_claude {
        let meta = crate::claude_accounts::load_meta(&app.cfg.config_dir);
        let account = crate::claude_accounts::resolve_account(None, None, &meta);
        if app.claude_cred_for(Some(&account)).is_none() {
            return Some(format!("log in with Claude in Settings first (account '{account}')"));
        }
    }
    None
}

/// Every model the mothership offers (`GET /api/models`), with its provider's health and quota: the
/// switcher's pickers. A model on a provider whose plan is out — or a Claude model while the
/// account's own cap holds — is listed with `out_of_quota` and its reset, so the picker can show it
/// disabled rather than hide it.
pub(crate) fn model_options(app: &Shared) -> Vec<Value> {
    let account = app.gateway.is_account_quota_exhausted();
    let account_state = app.gateway.account_quota_state().filter(|_| account);
    let mut out: Vec<Value> = providers::ANTHROPIC_MODELS
        .iter()
        .map(|(id, label)| {
            json!({
                "id": id, "label": label, "provider": "anthropic", "provider_name": "Anthropic", "wire": null,
                "failure_pct": 0.0, "rated": false, "degraded": account, "healthy": !account,
                "out_of_quota": account,
                "reset_at": account_state.as_ref().and_then(|q| q.reset_at.clone()),
                "reset_unix": account_state.as_ref().and_then(|q| q.reset_unix),
            })
        })
        .collect();
    for provider in app.providers() {
        let h = health(&app.gateway.usage(&provider.id));
        let out_of_quota = app.gateway.is_quota_exhausted(&provider.id);
        let state = app.gateway.quota_state(&provider.id).filter(|_| out_of_quota);
        for model in &provider.models {
            out.push(json!({
                "id": format!("{}/{model}", provider.id),
                "label": format!("{model} · {}", provider.name),
                "provider": provider.id,
                "provider_name": provider.name,
                "wire": provider.wire,
                "failure_pct": h.failure_pct,
                "rated": h.rated,
                "degraded": h.degraded || out_of_quota,
                "healthy": !h.degraded && !out_of_quota,
                "out_of_quota": out_of_quota,
                "reset_at": state.as_ref().and_then(|q| q.reset_at.clone()),
                "reset_unix": state.as_ref().and_then(|q| q.reset_unix),
            }));
        }
    }
    out
}

/// The orgs file, read strictly: one that will not parse is refused, never read as empty (#408).
fn read_orgs(app: &Shared) -> Result<BTreeMap<String, OrgSettings>, crate::AppError> {
    crate::util::read_json_or_default(&app.orgs_file()).map_err(|e| config_unreadable(&app.orgs_file(), &e))
}

/// The orgs the switcher lists: every org with settings of its own, a colony or the account's
/// membership — the workspace set `GET /api/orgs` reads, without the GitHub refresh.
async fn listed_orgs(app: &Shared, saved: &BTreeMap<String, OrgSettings>) -> Vec<String> {
    let mut orgs: std::collections::BTreeSet<String> = saved.keys().cloned().collect();
    orgs.extend(app.sessions.read().await.iter().map(|s| s.org.clone()));
    orgs.extend(app.repo_owners.read().await.iter().cloned());
    orgs.into_iter().filter(|o| orgs::valid_org(o)).collect()
}

/// `GET /api/models/assignments`: the effective model per role for the install and for each org,
/// with where each comes from; the agent modules with their roles and whether they can launch; and
/// the models on offer with their health and quota.
pub async fn assignments(State(app): State<Shared>) -> ApiResult<Value> {
    let modules = app.modules.read().await.clone();
    let saved = read_orgs(&app)?;
    let schema_of = |module: &str| crate::modules::schema_for("agent", module, &app.agents);
    let install_module = modules.agent.provider.clone();
    let install = json!({
        "module": install_module,
        "roles": resolve_roles(&modules, None, &install_module, &schema_of(&install_module)),
    });
    let mut org_rows = Vec::new();
    for org in listed_orgs(&app, &saved).await {
        let settings = saved.get(&org).cloned().unwrap_or_default();
        let module = orgs::effective_agent_module(&settings, &modules);
        let module_source = if settings
            .agent
            .as_ref()
            .and_then(|a| a.module.as_deref())
            .is_some_and(|m| !m.trim().is_empty())
        {
            "org"
        } else {
            "install"
        };
        org_rows.push(json!({
            "org": org,
            "module": module,
            "module_source": module_source,
            "roles": resolve_roles(&modules, Some(&settings), &module, &schema_of(&module)),
        }));
    }
    let module_rows: Vec<Value> = app
        .agents
        .iter()
        .map(|agent| {
            let roles: Vec<Value> = module_roles(&agent.schema)
                .into_iter()
                .map(|(role, title)| json!({"role": role, "title": title, "org_settable": ORG_ROLES.contains(&role.as_str())}))
                .collect();
            json!({
                "id": agent.id,
                "name": agent.name,
                "roles": roles,
                "blocked": launch_refusal(&app, &modules, agent),
            })
        })
        .collect();
    Ok(Json(json!({
        "install": install,
        "orgs": org_rows,
        "modules": module_rows,
        "models": model_options(&app),
    })))
}

#[derive(Deserialize)]
pub struct SwitchRequest {
    /// `install` (every org without an override of its own) or `org`.
    scope: String,
    /// The org, with `org` scope.
    #[serde(default)]
    org: Option<String>,
    /// The agent module the scope runs on. Omitted keeps it; for an org, `""` returns it to the
    /// install's module.
    #[serde(default)]
    module: Option<String>,
    /// Role → model. `null` or `""` clears it: an org's override goes back to the install's value,
    /// the install's to the module's default.
    #[serde(default)]
    roles: BTreeMap<String, Option<String>>,
    /// `new` (the default: colonies started from now on) or `running` (also restart the colonies in
    /// the scope on the new models).
    #[serde(default)]
    apply: Option<String>,
    /// Plan only: say what would change and which colonies would restart, change nothing.
    #[serde(default)]
    dry_run: bool,
}

fn bad(message: &str) -> crate::AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

/// Why `model` cannot fill `role`, if it cannot: the shape an org save accepts, a model on offer
/// (`GET /api/models`), not out of quota, and the quota card's own per-role rules
/// ([`quota_cards::role_error`]: a provider's model map, summaries' API-key need).
fn model_error(app: &Shared, offered: &[Value], role: &str, model: &str) -> Option<String> {
    if model.len() > 120 || model.contains(char::is_whitespace) {
        return Some("model names can't contain spaces or exceed 120 characters".into());
    }
    let Some(option) = offered.iter().find(|m| m["id"] == model) else {
        return Some(format!("{model} is not a model on offer; pick one from GET /api/models"));
    };
    if option["out_of_quota"] == true {
        let reset = option["reset_at"].as_str().map(|r| format!(" until {r}")).unwrap_or_default();
        return Some(format!("{model} is out of quota{reset}; pick a model on another provider"));
    }
    quota_cards::role_error(app, role, model)
}

/// A colony's agent module: the one it recorded at create, else the install's.
fn colony_module<'a>(s: &'a Session, modules: &'a ModulesConfig) -> &'a str {
    if s.agent.is_empty() {
        &modules.agent.provider
    } else {
        &s.agent
    }
}

/// What a colony's role resolves to today: its own launch override for the orchestrator and
/// subagent, else its org's and the install's settings — what boot reads.
fn colony_role(modules: &ModulesConfig, org: &OrgSettings, s: &Session, schema: &Value, role: &str) -> String {
    let own = match role {
        "model" => s.model_override.clone(),
        "subagent_model" => s.subagent_model_override.clone(),
        _ => None,
    };
    own.unwrap_or_else(|| resolve_role(modules, Some(org), colony_module(s, modules), schema, role).0)
}

/// One colony a `running` switch moves, with the overrides it gets.
struct ColonyMove {
    id: String,
    /// `(role, was, now)` for each role whose value changes.
    roles: Vec<(String, String, String)>,
}

/// The colonies a switch reaches and what changes for each: in play (live, parked or queued), on
/// the scope's module after the switch, in the scope — the org's, or for the install every org
/// that does not override the role itself — and whose role actually resolves to something else
/// after the switch.
fn plan_colonies(
    sessions: &[Session],
    before: (&ModulesConfig, &BTreeMap<String, OrgSettings>),
    after: (&ModulesConfig, &BTreeMap<String, OrgSettings>),
    scope_org: Option<&str>,
    module: &str,
    schema: &Value,
    roles: &[String],
) -> Vec<ColonyMove> {
    let none = OrgSettings::default();
    sessions
        .iter()
        .filter(|s| {
            !s.cleaned_up
                && (s.status.is_live() || matches!(s.status, SessionStatus::Parked | SessionStatus::Queued))
                && colony_module(s, before.0) == module
                && scope_org.is_none_or(|o| s.org == o)
        })
        .filter_map(|s| {
            let org_before = before.1.get(&s.org).unwrap_or(&none);
            let org_after = after.1.get(&s.org).unwrap_or(&none);
            let mut moved = Vec::new();
            for role in roles {
                // Install-wide, an org that overrides the role keeps its own value.
                if scope_org.is_none() && org_override(org_after, role).is_some() {
                    continue;
                }
                let was = colony_role(before.0, org_before, s, schema, role);
                let now = resolve_role(after.0, Some(org_after), module, schema, role).0;
                if was != now {
                    moved.push((role.clone(), was, now));
                }
            }
            (!moved.is_empty()).then(|| ColonyMove {
                id: s.id.clone(),
                roles: moved,
            })
        })
        .collect()
}

/// `POST /api/models/switch`: moves the scope's agent module and role models, validated as a whole
/// before anything is written, then — with `apply: "running"` — points the scope's colonies at the
/// new models and restarts them through the quota card's restart path.
pub async fn switch(State(app): State<Shared>, Json(req): Json<SwitchRequest>) -> ApiResult<Value> {
    let running = match req.apply.as_deref().unwrap_or("new") {
        "new" => false,
        "running" => true,
        other => return Err(bad(&format!("apply {other:?} is not supported; use \"new\" or \"running\""))),
    };
    let modules = app.modules.read().await.clone();
    let saved = read_orgs(&app)?;
    let scope_org = match req.scope.as_str() {
        "install" => None,
        "org" => {
            let org = req.org.as_deref().map(str::trim).unwrap_or_default();
            if !orgs::valid_org(org) {
                return Err(bad("an org switch names a valid GitHub org"));
            }
            Some(org.to_string())
        }
        other => return Err(bad(&format!("scope {other:?} is not supported; use \"install\" or \"org\""))),
    };
    let org_settings = scope_org.as_ref().map(|o| saved.get(o).cloned().unwrap_or_default());
    let current = match &org_settings {
        Some(settings) => orgs::effective_agent_module(settings, &modules),
        None => modules.agent.provider.clone(),
    };

    // The module after the switch, refused when it is not installed or cannot launch here.
    let wanted = req.module.as_deref().map(str::trim);
    let module = match wanted {
        None => current.clone(),
        Some("") if scope_org.is_some() => modules.agent.provider.clone(),
        Some("") => return Err(bad("the install always runs one agent module; name it")),
        Some(m) => m.to_string(),
    };
    let Some(agent) = app.agents.iter().find(|a| a.id == module) else {
        let ids: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
        return Err(bad(&format!(
            "unknown agent module {module:?}; available: {}",
            if ids.is_empty() { "none".into() } else { ids.join(", ") }
        )));
    };
    if module != current
        && let Some(why) = launch_refusal(&app, &modules, agent)
    {
        return Err(bad(&format!("{module} can't launch on this install: {why}")));
    }
    let schema = crate::modules::schema_for("agent", &module, &app.agents);
    let declared = module_roles(&schema);

    // Every role named must be the module's, settable in this scope, and its model allowed.
    let offered = model_options(&app);
    let mut roles: Vec<(String, Option<String>)> = Vec::new();
    for (role, value) in &req.roles {
        if !declared.iter().any(|(r, _)| r == role) {
            let names: Vec<&str> = declared.iter().map(|(r, _)| r.as_str()).collect();
            return Err(bad(&format!(
                "`{role}` is not a model role of {module}; its roles: {}",
                if names.is_empty() { "none".into() } else { names.join(", ") }
            )));
        }
        if scope_org.is_some() && !ORG_ROLES.contains(&role.as_str()) {
            return Err(bad(&format!("`{role}` is set install-wide only; switch it under All orgs")));
        }
        let value = value.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
        if let Some(model) = &value
            && let Some(error) = model_error(&app, &offered, role, model)
        {
            return Err(bad(&format!("{role}: {error}")));
        }
        roles.push((role.clone(), value));
    }
    let module_changes = wanted.is_some() && (module != current || scope_org.is_some());
    if roles.is_empty() && !module_changes {
        return Err(bad("nothing to switch: name a module or at least one role"));
    }

    // The settings after the switch, planned and validated as the save handlers will.
    let mut after_modules = modules.clone();
    let mut after_orgs = saved.clone();
    let mut changes: Vec<Value> = Vec::new();
    let install_update = match &scope_org {
        None => {
            let mut settings = modules.agent.settings.clone();
            for (role, value) in &roles {
                match value {
                    Some(model) => settings.insert(role.clone(), Value::String(model.clone())),
                    None => settings.remove(role),
                };
            }
            if module != current {
                changes.push(quota_cards::change("install", "agent", "module", Some(&current), &module));
            }
            let current_schema = crate::modules::schema_for("agent", &current, &app.agents);
            let mut next = modules.clone();
            next.agent.provider = module.clone();
            next.agent.settings = settings.clone();
            for (role, _) in &roles {
                let (was, _) = resolve_role(&modules, None, &current, &current_schema, role);
                let (now, _) = resolve_role(&next, None, &module, &schema, role);
                if was != now || modules.agent.settings.get(role) != next.agent.settings.get(role) {
                    changes.push(quota_cards::change("install", &module, role, Some(&was), &now));
                }
            }
            crate::modules::validate_settings(&module, &schema, &settings, &modules.agent.settings)
                .map_err(|e| bad(&format!("the agent module's settings would not save: {e}")))?;
            after_modules.agent.provider = module.clone();
            after_modules.agent.settings = settings.clone();
            Some(settings)
        }
        Some(_) => None,
    };
    let org_update = match (&scope_org, &org_settings) {
        (Some(org), Some(settings)) => {
            let mut next = settings.clone();
            let agent = next.agent.get_or_insert_with(Default::default);
            if let Some(m) = wanted {
                let was = current.clone();
                agent.module = (!m.is_empty()).then(|| module.clone());
                if was != module || m.is_empty() != settings.agent.as_ref().and_then(|a| a.module.as_ref()).is_none() {
                    changes.push(quota_cards::change("org", org, "module", Some(&was), &module));
                }
            }
            for (role, value) in &roles {
                let slot = match role.as_str() {
                    "model" => &mut agent.model,
                    "subagent_model" => &mut agent.subagent_model,
                    _ => &mut agent.background_model,
                };
                *slot = value.clone();
            }
            let current_schema = crate::modules::schema_for("agent", &current, &app.agents);
            for (role, _) in &roles {
                let (was, _) = resolve_role(&modules, Some(settings), &current, &current_schema, role);
                let (now, _) = resolve_role(&modules, Some(&next), &module, &schema, role);
                if was != now || org_override(settings, role).is_some() != org_override(&next, role).is_some() {
                    changes.push(quota_cards::change("org", org, role, Some(&was), &now));
                }
            }
            orgs::validate(&next).map_err(|m| bad(&m))?;
            after_orgs.insert(org.clone(), next.clone());
            Some((org.clone(), next))
        }
        _ => None,
    };

    let sessions = app.sessions.read().await.clone();
    let role_names: Vec<String> = roles.iter().map(|(r, _)| r.clone()).collect();
    let moves = if running {
        plan_colonies(
            &sessions,
            (&modules, &saved),
            (&after_modules, &after_orgs),
            scope_org.as_deref(),
            &module,
            &schema,
            &role_names,
        )
    } else {
        Vec::new()
    };
    let colony_ids: Vec<&str> = moves.iter().map(|m| m.id.as_str()).collect();
    if req.dry_run {
        return Ok(Json(json!({
            "dry_run": true,
            "scope": req.scope,
            "org": scope_org,
            "module": module,
            "changes": changes,
            "affected": colony_ids,
            "colonies": [],
            "failed": [],
        })));
    }

    // Validated as a whole: now write, through the handlers Settings saves with.
    if let Some(settings) = install_update {
        let update = crate::modules::UpdateModule {
            provider: module.clone(),
            enabled: modules.agent.enabled,
            settings,
            save_anyway: false,
            confirm_content: false,
        };
        let _ = crate::modules::update(State(app.clone()), Path("agent".into()), Json(update)).await?;
    }
    if let Some((org, settings)) = org_update {
        let body = json!({"settings": {"agent": settings.agent}});
        let _ = orgs::put(State(app.clone()), Path(org), Json(body)).await?;
    }

    // The running colonies: their per-colony overrides follow the new values (boot re-derives the
    // routes from them), then the quota card's restart path takes each down and up again.
    let mut targets: Vec<Session> = Vec::new();
    for m in &moves {
        let updated = app
            .update_session(&m.id, |x| {
                for (role, _, now) in &m.roles {
                    let next = (!now.is_empty()).then(|| now.clone());
                    match role.as_str() {
                        "model" => x.model_override = next,
                        "subagent_model" => x.subagent_model_override = next,
                        _ => {}
                    }
                }
            })
            .await;
        for (role, was, now) in &m.roles {
            changes.push(quota_cards::change("colony", &m.id, role, Some(was), now));
        }
        let said: Vec<String> = m
            .roles
            .iter()
            .map(|(role, was, now)| format!("{role} {} → {}", or_default(was), or_default(now)))
            .collect();
        app.session_log(
            &m.id,
            "info",
            format!(
                "models switched from the cockpit: {}; restarting on the new models",
                said.join(", ")
            ),
        )
        .await;
        if let Some((s, _)) = updated {
            targets.push(s);
        }
    }
    let results = quota_cards::restart_all(&app, &targets).await;
    let failed: Vec<&Value> = results.iter().filter(|r| r["ok"] != true).collect();
    Ok(Json(json!({
        "dry_run": false,
        "scope": req.scope,
        "org": scope_org,
        "module": module,
        "changes": changes,
        "affected": colony_ids,
        "colonies": results.iter().filter(|r| r["ok"] == true).map(|r| r["id"].clone()).collect::<Vec<_>>(),
        "failed": failed,
    })))
}

fn or_default(model: &str) -> &str {
    if model.is_empty() { "the agent's default" } else { model }
}

/// The API routes this module serves; `server::api_routes` merges them through `features::ALL`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/models/assignments", routing::get(assignments))
        .route("/api/models/switch", routing::post(switch))
}

/// This module's feature descriptor (`features.rs`). No `token_scope`: both routes are owner-only,
/// like every other model setting, so a scoped API token gets 403.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "model_switch",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &[],
    start_tasks: None,
};

/// A switch is a settings save, like the Settings forms it stands in for.
const ACTIVITY: &[crate::activity::Rule] = &[crate::activity::rule(
    "POST",
    "/api/models/switch",
    "settings.save",
    crate::activity::Target::Fixed("models", "module:agent"),
)];

#[cfg(test)]
mod tests;
