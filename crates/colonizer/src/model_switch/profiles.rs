//! Saved model profiles for the header's model switcher: a named set of role → model choices
//! (orchestrator, subagents, background, summary, small and large tasks), kept in the install's
//! config directory (`model-profiles.json`) so every device sees the same list.
//!
//! A profile only remembers choices. Applying one is the switcher's ordinary
//! `POST /api/models/switch` with the profile's roles, so it is validated, saved and (with
//! `apply: "running"`) rolled out to running colonies exactly as a hand-made switch is.
//!
//! The starters are not stored: they are derived on every read from what the install actually has
//! configured — Claude models only with a Claude login, a provider's model only when the provider
//! lists one — and with nothing configured there are none.

use crate::{ApiResult, Shared, client_error, config_unreadable, providers};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How many profiles an install keeps: a picker, not a database.
const MAX_PROFILES: usize = 50;
/// A profile name, as the picker shows it.
const MAX_NAME: usize = 60;
/// Starter ids carry this prefix, so a PUT or DELETE can refuse them by id alone.
const STARTER_PREFIX: &str = "starter-";

/// One saved profile. `roles` maps a model role to a model id; `""` is the module's own default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModelProfile {
    pub id: String,
    pub name: String,
    /// The agent module the profile was saved from, as a hint for the picker; applying never
    /// changes a scope's module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    pub roles: BTreeMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Serializes the read-modify-write of the profiles file: two saves from two devices at once must
/// not drop one of them.
static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn profiles_file(app: &Shared) -> std::path::PathBuf {
    app.cfg.config_dir.join("model-profiles.json")
}

/// The saved profiles, read strictly: a file that will not parse is refused, never read as empty
/// and then overwritten (#408).
fn read(app: &Shared) -> Result<Vec<ModelProfile>, crate::AppError> {
    let path = profiles_file(app);
    crate::util::read_json_or_default(&path).map_err(|e| config_unreadable(&path, &e))
}

async fn write(app: &Shared, profiles: &[ModelProfile]) -> Result<(), crate::AppError> {
    std::fs::create_dir_all(&app.cfg.config_dir)?;
    crate::util::write_atomic(&profiles_file(app), &serde_json::to_vec_pretty(profiles)?).await?;
    Ok(())
}

fn bad(message: &str) -> crate::AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

/// Whether a key names a model role, as a module schema declares them (`model`, `*_model`, `model_*`).
fn is_role_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 40
        && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        && (key == "model" || key.ends_with("_model") || key.starts_with("model_"))
}

/// The name, trimmed, or why it is not one.
fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name the profile".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("profile names are at most {MAX_NAME} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("profile names are one line of text".into());
    }
    Ok(name.to_string())
}

/// The roles, trimmed, or why they are not a profile's. The models are not checked against the
/// models on offer here — a provider can come and go — the switch checks them when it is applied.
fn valid_roles(roles: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    if roles.is_empty() {
        return Err("a profile names at least one role".into());
    }
    if roles.len() > 16 {
        return Err("a profile names at most 16 roles".into());
    }
    let mut out = BTreeMap::new();
    for (role, model) in roles {
        if !is_role_key(role) {
            return Err(format!("`{role}` is not a model role (model, *_model or model_*)"));
        }
        let model = model.trim();
        if model.len() > 120 || model.contains(char::is_whitespace) {
            return Err(format!("{role}: model names can't contain spaces or exceed 120 characters"));
        }
        out.insert(role.clone(), model.to_string());
    }
    Ok(out)
}

fn valid_module(module: Option<&str>) -> Result<Option<String>, String> {
    let Some(module) = module.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    if module.len() > 64
        || !module
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("agent module ids are lowercase letters, digits, dashes and underscores".into());
    }
    Ok(Some(module.to_string()))
}

fn name_taken(profiles: &[ModelProfile], name: &str, except: Option<&str>) -> bool {
    profiles
        .iter()
        .any(|p| Some(p.id.as_str()) != except && p.name.eq_ignore_ascii_case(name))
}

/// A starter, as the picker lists it: never stored, never renamed or deleted.
fn starter(id: &str, name: String, roles: &[(&str, String)]) -> Value {
    json!({
        "id": format!("{STARTER_PREFIX}{id}"),
        "name": name,
        "module": null,
        "roles": roles.iter().map(|(r, m)| ((*r).to_string(), Value::String(m.clone()))).collect::<serde_json::Map<_, _>>(),
        "builtin": true,
    })
}

/// The starters this install can actually run: "Claude only" with a Claude login, and for each of
/// the first three providers that list a model, that model on the subagent and background roles —
/// under a Claude orchestrator when there is a login, alone otherwise. Only the three roles an org
/// can override, so a starter applies to any scope. Nothing configured, no starters.
pub(crate) fn starters(claude: bool, providers: &[(String, String, String)]) -> Vec<Value> {
    let mut out = Vec::new();
    if claude {
        out.push(starter(
            "claude",
            "Claude only".into(),
            &[
                ("model", "opus".into()),
                ("subagent_model", "sonnet".into()),
                ("background_model", "haiku".into()),
            ],
        ));
    }
    for (id, name, model) in providers.iter().take(3) {
        let routed = format!("{id}/{model}");
        if claude {
            out.push(starter(
                &format!("claude-{id}"),
                format!("Claude lead, {name} crew"),
                &[
                    ("model", "opus".into()),
                    ("subagent_model", routed.clone()),
                    ("background_model", routed),
                ],
            ));
        } else {
            out.push(starter(
                &format!("all-{id}"),
                format!("All {name}"),
                &[
                    ("model", routed.clone()),
                    ("subagent_model", routed.clone()),
                    ("background_model", routed),
                ],
            ));
        }
    }
    out
}

/// Whether the install's default Claude account is logged in — what a Claude model needs to run.
fn claude_configured(app: &Shared) -> bool {
    let meta = crate::claude_accounts::load_meta(&app.cfg.config_dir);
    let account = crate::claude_accounts::resolve_account(None, None, &meta);
    app.claude_cred_for(Some(&account)).is_some()
}

/// The configured providers that list a model: `(id, name, first model)`.
fn provider_models(all: &[providers::Provider]) -> Vec<(String, String, String)> {
    all.iter()
        .filter_map(|p| {
            let model = p.models.iter().map(|m| m.trim()).find(|m| !m.is_empty())?;
            let name = if p.name.trim().is_empty() {
                p.id.clone()
            } else {
                p.name.trim().to_string()
            };
            Some((p.id.clone(), name, model.to_string()))
        })
        .collect()
}

fn saved_json(profile: &ModelProfile) -> Value {
    let mut value = serde_json::to_value(profile).unwrap_or(Value::Null);
    value["builtin"] = Value::Bool(false);
    value
}

/// `GET /api/models/profiles`: the saved profiles, oldest first, then the starters whose names no
/// saved profile has taken.
pub async fn list(State(app): State<Shared>) -> ApiResult<Value> {
    let saved = read(&app)?;
    let mut out: Vec<Value> = saved.iter().map(saved_json).collect();
    for s in starters(claude_configured(&app), &provider_models(&app.providers())) {
        let name = s["name"].as_str().unwrap_or_default();
        if !name_taken(&saved, name, None) {
            out.push(s);
        }
    }
    Ok(Json(json!({"profiles": out})))
}

#[derive(Deserialize)]
pub struct CreateProfile {
    name: String,
    #[serde(default)]
    module: Option<String>,
    roles: BTreeMap<String, String>,
}

/// `POST /api/models/profiles`: saves a new profile, usually the switcher's current selection.
pub async fn create(State(app): State<Shared>, Json(req): Json<CreateProfile>) -> ApiResult<Value> {
    let name = valid_name(&req.name).map_err(|e| bad(&e))?;
    let roles = valid_roles(&req.roles).map_err(|e| bad(&e))?;
    let module = valid_module(req.module.as_deref()).map_err(|e| bad(&e))?;
    let _guard = WRITE.lock().await;
    let mut profiles = read(&app)?;
    if profiles.len() >= MAX_PROFILES {
        return Err(bad(&format!(
            "an install keeps at most {MAX_PROFILES} profiles; delete one first"
        )));
    }
    if name_taken(&profiles, &name, None) {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("a profile named \"{name}\" already exists"),
        ));
    }
    let now = Utc::now();
    let profile = ModelProfile {
        id: format!("p-{}", crate::util::short_id()),
        name,
        module,
        roles,
        created_at: now,
        updated_at: now,
    };
    profiles.push(profile.clone());
    write(&app, &profiles).await?;
    Ok(Json(saved_json(&profile)))
}

#[derive(Deserialize)]
pub struct UpdateProfile {
    #[serde(default)]
    name: Option<String>,
    /// Omitted keeps the hint; `""` clears it.
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    roles: Option<BTreeMap<String, String>>,
}

fn refuse_starter(id: &str) -> Result<(), crate::AppError> {
    if id.starts_with(STARTER_PREFIX) {
        return Err(bad("a starter can't be changed or deleted; save it as a profile of your own"));
    }
    Ok(())
}

/// `PUT /api/models/profiles/{id}`: renames a profile, or replaces its roles.
pub async fn update(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<UpdateProfile>) -> ApiResult<Value> {
    refuse_starter(&id)?;
    let name = req.name.as_deref().map(valid_name).transpose().map_err(|e| bad(&e))?;
    let roles = req.roles.as_ref().map(valid_roles).transpose().map_err(|e| bad(&e))?;
    let module = req
        .module
        .as_deref()
        .map(|m| valid_module(Some(m)))
        .transpose()
        .map_err(|e| bad(&e))?;
    if name.is_none() && roles.is_none() && module.is_none() {
        return Err(bad("nothing to change: send a name, roles or a module"));
    }
    let _guard = WRITE.lock().await;
    let mut profiles = read(&app)?;
    if let Some(name) = &name
        && name_taken(&profiles, name, Some(&id))
    {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("a profile named \"{name}\" already exists"),
        ));
    }
    let Some(profile) = profiles.iter_mut().find(|p| p.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such profile"));
    };
    if let Some(name) = name {
        profile.name = name;
    }
    if let Some(roles) = roles {
        profile.roles = roles;
    }
    if let Some(module) = module {
        profile.module = module;
    }
    profile.updated_at = Utc::now();
    let updated = profile.clone();
    write(&app, &profiles).await?;
    Ok(Json(saved_json(&updated)))
}

/// `DELETE /api/models/profiles/{id}`.
pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    refuse_starter(&id)?;
    let _guard = WRITE.lock().await;
    let mut profiles = read(&app)?;
    let before = profiles.len();
    profiles.retain(|p| p.id != id);
    if profiles.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no such profile"));
    }
    write(&app, &profiles).await?;
    Ok(Json(json!({"deleted": id})))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("colonizer-model-profiles-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn roles(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(r, m)| ((*r).to_string(), (*m).to_string())).collect()
    }

    #[test]
    fn starters_map_only_to_what_the_install_has() {
        assert!(starters(false, &[]).is_empty(), "nothing configured, no starters");
        let byteplus = vec![("byteplus".to_string(), "BytePlus".to_string(), "seed-code".to_string())];
        let alone = starters(false, &byteplus);
        assert_eq!(alone.len(), 1);
        assert_eq!(alone[0]["name"], "All BytePlus");
        assert_eq!(alone[0]["roles"]["model"], "byteplus/seed-code");
        let with_claude = starters(true, &byteplus);
        let names: Vec<&str> = with_claude.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Claude only", "Claude lead, BytePlus crew"]);
        assert_eq!(with_claude[1]["roles"]["subagent_model"], "byteplus/seed-code");
        assert!(
            with_claude
                .iter()
                .all(|s| s["builtin"] == true && s["id"].as_str().unwrap().starts_with(STARTER_PREFIX))
        );
        // Only the roles an org can override, so a starter applies to any scope.
        for s in &with_claude {
            for role in s["roles"].as_object().unwrap().keys() {
                assert!(crate::model_switch::ORG_ROLES.contains(&role.as_str()), "{role}");
            }
        }
    }

    #[test]
    fn names_and_roles_are_validated() {
        assert_eq!(valid_name("  Cheap crew ").unwrap(), "Cheap crew");
        assert!(valid_name("   ").is_err());
        assert!(valid_name(&"x".repeat(61)).is_err());
        assert!(valid_name("two\nlines").is_err());
        assert!(valid_roles(&BTreeMap::new()).is_err());
        assert!(valid_roles(&roles(&[("plugins", "x")])).is_err(), "not a model role");
        assert!(valid_roles(&roles(&[("model", "has space")])).is_err());
        assert_eq!(
            valid_roles(&roles(&[("model", " opus "), ("model_high", "")])).unwrap(),
            roles(&[("model", "opus"), ("model_high", "")]),
            "an empty model is the module default"
        );
        assert!(valid_module(Some("Claude Code")).is_err());
        assert_eq!(valid_module(Some(" ")).unwrap(), None);
    }

    #[tokio::test]
    async fn profiles_are_created_renamed_and_deleted_in_the_install_config() {
        let root = test_root("crud");
        let app = crate::tests::test_app(&root);
        let made = create(
            State(app.clone()),
            Json(CreateProfile {
                name: "Cheap crew".into(),
                module: Some("claude-code".into()),
                roles: roles(&[("model", "opus"), ("subagent_model", "byteplus/seed-code")]),
            }),
        )
        .await
        .unwrap()
        .0;
        let id = made["id"].as_str().unwrap().to_string();
        assert_eq!(made["builtin"], false);
        assert!(app.cfg.config_dir.join("model-profiles.json").exists(), "stored server-side");

        // The same name again (any case) is a conflict.
        let dup = create(
            State(app.clone()),
            Json(CreateProfile {
                name: "cheap CREW".into(),
                module: None,
                roles: roles(&[("model", "opus")]),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(dup.status(), StatusCode::CONFLICT);

        let renamed = update(
            State(app.clone()),
            Path(id.clone()),
            Json(UpdateProfile {
                name: Some("Night shift".into()),
                module: None,
                roles: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(renamed["name"], "Night shift");
        assert_eq!(
            renamed["roles"]["subagent_model"], "byteplus/seed-code",
            "a rename keeps the roles"
        );

        let listed = list(State(app.clone())).await.unwrap().0;
        let saved: Vec<&Value> = listed["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["builtin"] == false)
            .collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0]["name"], "Night shift");

        let starter = delete(State(app.clone()), Path("starter-claude".into())).await.unwrap_err();
        assert_eq!(starter.status(), StatusCode::BAD_REQUEST);
        let _ = delete(State(app.clone()), Path(id.clone())).await.unwrap();
        let gone = delete(State(app.clone()), Path(id)).await.unwrap_err();
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreadable_profiles_file_is_refused_not_overwritten() {
        let root = test_root("corrupt");
        let app = crate::tests::test_app(&root);
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(app.cfg.config_dir.join("model-profiles.json"), b"{not json").unwrap();
        assert!(list(State(app.clone())).await.is_err());
        let refused = create(
            State(app.clone()),
            Json(CreateProfile {
                name: "x".into(),
                module: None,
                roles: roles(&[("model", "opus")]),
            }),
        )
        .await;
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read(app.cfg.config_dir.join("model-profiles.json")).unwrap(),
            b"{not json",
            "the file stands as it was"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
