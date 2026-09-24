//! Multiple Claude credentials, chosen per org or colony (issue #95). Secrets live at
//! `<config>/claude-accounts/<id>` (0600 via `write_secret`); the metadata file
//! `<config>/claude-accounts.json` records `{default, accounts: {id: {label, added_at}}}`.
//! Installs that predate this module keep a single `<config>/claude-token`, which `migrate_legacy`
//! moves into the `default` account on first use.

use crate::{
    ApiResult, Shared, client_error,
    util::{read_trimmed, write_secret},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path as FsPath, PathBuf},
};

/// Secrets live here, one 0600 file per account id, beside the metadata file.
pub const ACCOUNTS_DIR: &str = "claude-accounts";

/// Lowercase letters, digits and dashes, 1-40 chars — the providers.rs `valid_id` shape, widened
/// from 32 to 40 chars for human labels-turned-ids.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountMeta {
    #[serde(default)]
    pub label: String,
    /// `DateTime<Utc>` has no `Default` (see `Session::default` for why), so a record written
    /// before this field existed lands on the epoch rather than failing the whole file.
    #[serde(default = "default_added_at")]
    pub added_at: DateTime<Utc>,
}

fn default_added_at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AccountsMeta {
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountMeta>,
}

fn meta_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("claude-accounts.json")
}

pub fn account_file(config_dir: &FsPath, id: &str) -> PathBuf {
    config_dir.join(ACCOUNTS_DIR).join(id)
}

/// Missing or unparseable file reads as empty, so a first run and a hand-edited file both start
/// with no accounts rather than failing the launch that asked.
pub fn load_meta(config_dir: &FsPath) -> AccountsMeta {
    std::fs::read(meta_file(config_dir))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

pub fn save_meta(config_dir: &FsPath, meta: &AccountsMeta) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = meta_file(config_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Moves a pre-accounts `<config>/claude-token` into the `default` account. Idempotent: when the
/// metadata file already exists there is nothing to migrate. Returns true only when it migrated.
pub fn migrate_legacy(config_dir: &FsPath) -> anyhow::Result<bool> {
    if meta_file(config_dir).exists() {
        return Ok(false);
    }
    let legacy = config_dir.join("claude-token");
    let Some(token) = read_trimmed(&legacy) else {
        return Ok(false);
    };
    let now = Utc::now();
    let meta = AccountsMeta {
        default: "default".to_string(),
        accounts: BTreeMap::from([(
            "default".to_string(),
            AccountMeta {
                label: "default".to_string(),
                added_at: now,
            },
        )]),
    };
    write_secret(&account_file(config_dir, "default"), &token)?;
    save_meta(config_dir, &meta)?;
    let _ = std::fs::remove_file(&legacy);
    Ok(true)
}

/// Which account a colony launches with: the explicit per-colony choice, else the org's override,
/// else the install default. Pure, so the precedence is testable without a config dir.
pub fn resolve_account(explicit: Option<&str>, org_override: Option<&str>, meta: &AccountsMeta) -> String {
    if let Some(id) = explicit.filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Some(id) = org_override.filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if meta.default.is_empty() {
        "default".to_string()
    } else {
        meta.default.clone()
    }
}

/// The stored secret for one account: its path and trimmed, non-empty contents. `None` when
/// the account was never added or its secret was deleted. Read through the secret store, so an
/// account moved into the keychain (its file gone) still resolves.
pub fn cred_for(config_dir: &FsPath, account: &str) -> Option<(PathBuf, String)> {
    let path = account_file(config_dir, account);
    let token = crate::util::read_secret(&path)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())?;
    Some((path, token))
}

/// Which env var a stored token is injected as, and what the status payload calls it — exactly the
/// rule the single-token path always used: `sk-ant-api` is a saved API key, anything else is the
/// subscription token `claude setup-token` mints.
pub fn sniff(token: &str) -> (&'static str, &'static str) {
    if token.starts_with("sk-ant-api") {
        ("ANTHROPIC_API_KEY", "saved API key")
    } else {
        ("CLAUDE_CODE_OAUTH_TOKEN", "Claude subscription")
    }
}

/// Why an account cannot be deleted, if it cannot: the default account, an org override naming it,
/// or a live colony running on it. Pure, so every refusal is testable without the app.
pub fn in_use_msg(is_default: bool, orgs: &[String], sessions: &[String]) -> Option<String> {
    if is_default {
        return Some("this is the default account; make another account the default first".to_string());
    }
    if !orgs.is_empty() {
        return Some(format!("org settings still name this account ({})", orgs.join(", ")));
    }
    if !sessions.is_empty() {
        return Some(format!("live colonies still run on this account ({})", sessions.join(", ")));
    }
    None
}

pub async fn list(State(app): State<Shared>) -> Json<Vec<Value>> {
    let _ = migrate_legacy(&app.cfg.config_dir);
    let meta = load_meta(&app.cfg.config_dir);
    Json(
        meta.accounts
            .iter()
            .map(|(id, account)| {
                let (kind, source) = cred_for(&app.cfg.config_dir, id)
                    .map(|(_, token)| sniff(&token))
                    .map(|(env, source)| (Some(env), Some(source)))
                    .unwrap_or((None, None));
                json!({
                    "id": id,
                    "label": account.label,
                    "is_default": *id == meta.default,
                    "kind": kind,
                    "source": source,
                    "added_at": account.added_at.to_rfc3339(),
                })
            })
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct CreateAccount {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

/// A label turned into an id: lowercased, runs of anything else folded into one dash, trimmed and
/// capped at 40 chars — so "Acme Corp!" becomes "acme-corp".
fn slugify(label: &str) -> String {
    let mut out = String::new();
    for c in label.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    trimmed.chars().take(40).collect::<String>().trim_end_matches('-').to_string()
}

pub async fn create(State(app): State<Shared>, Json(req): Json<CreateAccount>) -> ApiResult<Value> {
    let bad = |message: &str| client_error(StatusCode::BAD_REQUEST, message);
    let token = req.token.as_deref().unwrap_or_default().trim();
    if !token.starts_with("sk-ant-") || token.contains(char::is_whitespace) {
        return Err(bad(
            "expected a token from `claude setup-token` (sk-ant-oat…) or an API key (sk-ant-api…)",
        ));
    }
    let id = match req.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => {
            if !valid_id(id) {
                return Err(bad("account ids are lowercase letters, digits and dashes, 1-40 characters"));
            }
            id.to_string()
        }
        None => {
            let slug = slugify(req.label.as_deref().unwrap_or_default());
            if !valid_id(&slug) {
                return Err(bad("name the account with an id, or a label it can be derived from"));
            }
            slug
        }
    };
    let _ = migrate_legacy(&app.cfg.config_dir);
    let mut meta = load_meta(&app.cfg.config_dir);
    let label = req
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_string();
    write_secret(&account_file(&app.cfg.config_dir, &id), token)?;
    // Re-adding an id keeps its original `added_at`; only the label is refreshed.
    let account = meta.accounts.entry(id.clone()).or_insert_with(|| AccountMeta {
        label: label.clone(),
        added_at: Utc::now(),
    });
    account.label = label.clone();
    let (label_out, added_at) = (account.label.clone(), account.added_at);
    // The first account added becomes the default, so a fresh install keeps working.
    if meta.default.is_empty() {
        meta.default = id.clone();
    }
    let is_default = id == meta.default;
    save_meta(&app.cfg.config_dir, &meta)?;
    let (kind, source) = sniff(token);
    Ok(Json(json!({
        "id": id,
        "label": label_out,
        "is_default": is_default,
        "kind": kind,
        "source": source,
        "added_at": added_at.to_rfc3339(),
    })))
}

pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let _ = migrate_legacy(&app.cfg.config_dir);
    let meta = load_meta(&app.cfg.config_dir);
    if !meta.accounts.contains_key(&id) {
        return Err(client_error(StatusCode::NOT_FOUND, "no such Claude account"));
    }
    let orgs: Vec<String> = app
        .all_org_settings()
        .into_iter()
        .filter(|(_, settings)| settings.agent.as_ref().and_then(|a| a.claude_account.as_deref()) == Some(id.as_str()))
        .map(|(org, _)| org)
        .collect();
    let sessions: Vec<String> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status.is_live() && s.claude_account.as_deref() == Some(id.as_str()))
        .map(|s| s.id.clone())
        .collect();
    if let Some(reason) = in_use_msg(id == meta.default, &orgs, &sessions) {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("cannot delete the '{id}' Claude account: {reason}"),
        ));
    }
    let mut meta = meta;
    meta.accounts.remove(&id);
    save_meta(&app.cfg.config_dir, &meta)?;
    let _ = std::fs::remove_file(account_file(&app.cfg.config_dir, &id));
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::short_id;

    fn temp_config() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-accounts-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn legacy_token_migrates_to_the_default_account_once() {
        let dir = temp_config();
        std::fs::write(dir.join("claude-token"), "sk-ant-oat-legacy").unwrap();
        assert!(migrate_legacy(&dir).unwrap(), "the legacy token moves on first use");
        let meta = load_meta(&dir);
        assert_eq!(meta.default, "default");
        assert_eq!(meta.accounts["default"].label, "default");
        assert_eq!(cred_for(&dir, "default").map(|(_, t)| t), Some("sk-ant-oat-legacy".into()));
        assert!(!dir.join("claude-token").exists(), "the legacy file is gone after the move");

        assert!(!migrate_legacy(&dir).unwrap(), "a second run migrates nothing");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn selection_prefers_the_colony_then_the_org_then_the_default() {
        let meta = AccountsMeta {
            default: "work".into(),
            accounts: BTreeMap::new(),
        };
        assert_eq!(resolve_account(Some("a"), Some("b"), &meta), "a");
        assert_eq!(resolve_account(None, Some("b"), &meta), "b");
        assert_eq!(resolve_account(Some(""), Some(""), &meta), "work");
        assert_eq!(resolve_account(None, None, &meta), "work");
        assert_eq!(
            resolve_account(None, None, &AccountsMeta::default()),
            "default",
            "no default configured falls back to \"default\""
        );
    }

    #[test]
    fn account_ids_are_lowercase_dashes_up_to_40_chars() {
        assert!(valid_id("default"));
        assert!(valid_id("acme-corp-2"));
        for bad in ["", "Upper", "has space", "under_score"] {
            assert!(!valid_id(bad), "{bad:?} should be rejected");
        }
        assert!(!valid_id(&"x".repeat(41)), "41 chars is one too many");
        assert_eq!(slugify("Acme Corp!"), "acme-corp");
        assert_eq!(slugify("  --a__b-- "), "a-b");
    }

    #[test]
    fn traversal_style_ids_never_survive_resolution_into_a_path() {
        // `sessions::create` resolves the request's explicit account and refuses anything `valid_id`
        // rejects, so a traversal-style id is never joined under the claude-accounts directory.
        for bad in ["..", "../foo", "a/b", "../../etc/hostname"] {
            let resolved = resolve_account(Some(bad), None, &AccountsMeta::default());
            assert!(!valid_id(&resolved), "{bad:?} should be rejected");
        }
        assert!(valid_id(&resolve_account(Some("acme-corp"), None, &AccountsMeta::default())));
        assert!(
            valid_id(&resolve_account(None, None, &AccountsMeta::default())),
            "no account specified still resolves to an acceptable default"
        );
    }

    #[test]
    fn tokens_sniff_to_the_same_env_and_source_as_before() {
        assert_eq!(sniff("sk-ant-api-123"), ("ANTHROPIC_API_KEY", "saved API key"));
        assert_eq!(sniff("sk-ant-oat-123"), ("CLAUDE_CODE_OAUTH_TOKEN", "Claude subscription"));
    }

    #[test]
    fn deletion_is_refused_while_the_account_is_in_use() {
        assert!(in_use_msg(false, &[], &[]).is_none(), "an unused account deletes cleanly");
        assert!(
            in_use_msg(true, &[], &[]).is_some_and(|m| m.contains("default")),
            "the default account names the way out"
        );
        assert!(
            in_use_msg(false, &["acme".into()], &[]).is_some_and(|m| m.contains("acme")),
            "an org override names the org"
        );
        assert!(
            in_use_msg(false, &[], &["abc123".into()]).is_some_and(|m| m.contains("abc123")),
            "a live colony names the colony"
        );
    }
}
