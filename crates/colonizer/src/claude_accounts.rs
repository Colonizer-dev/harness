//! Multiple Claude credentials, chosen per org or colony (issue #95). Secrets live at
//! `<config>/claude-accounts/<id>` (0600 via `write_secret`); the metadata file
//! `<config>/claude-accounts.json` records `{default, accounts: {id: {label, added_at}}}`.
//! Installs that predate this module keep a single `<config>/claude-token`, which `migrate_legacy`
//! moves into the `default` account on first use.

use crate::{
    ApiResult, Shared, client_error,
    util::{read_trimmed, short_id, write_secret},
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

/// Writes `meta` to a fresh temp beside the record and returns its path. The temp carries a
/// per-call id, as `write_atomic`'s does: two writers sharing one fixed temp rename each other's
/// file away, and whichever renames second fails. Sync, rather than `write_atomic` itself, because
/// the migration that saves here runs on the sync credential hot path (`App::claude_cred_for`).
fn write_meta_temp(config_dir: &FsPath, meta: &AccountsMeta) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(config_dir)?;
    let path = meta_file(config_dir);
    let tmp = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        short_id()
    ));
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    Ok(tmp)
}

/// The record saves of `create` and `delete` run under the config-write lock, so a plain rename
/// publishes exclusively.
pub fn save_meta(config_dir: &FsPath, meta: &AccountsMeta) -> anyhow::Result<()> {
    let tmp = write_meta_temp(config_dir, meta)?;
    std::fs::rename(&tmp, meta_file(config_dir))?;
    Ok(())
}

/// The migration's publish of the record: create-if-absent, so it cannot write over a record that
/// appeared while the migration ran — a `create` saving on the config-write lock lands in the gap
/// between `migrate_legacy`'s exists check and here, and its account must survive. `hard_link`
/// refuses an existing target with `AlreadyExists`, which is the same "someone else migrated"
/// answer the exists check gives, without the gap; the caller then leaves the legacy token for the
/// record's owner, as it does for any record already on disk. Returns whether it published; the
/// temp is removed either way.
fn publish_meta_no_clobber(config_dir: &FsPath, meta: &AccountsMeta) -> anyhow::Result<bool> {
    let tmp = write_meta_temp(config_dir, meta)?;
    let published = match std::fs::hard_link(&tmp, meta_file(config_dir)) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
    };
    let _ = std::fs::remove_file(&tmp);
    Ok(published)
}

/// Moves a pre-accounts `<config>/claude-token` into the `default` account. Idempotent: when the
/// metadata file already exists there is nothing to migrate, and the publish is create-if-absent,
/// so a record that appears mid-migration is not written over either. Returns true only when it
/// migrated.
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
    if !publish_meta_no_clobber(config_dir, &meta)? {
        return Ok(false);
    }
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

/// The stored secret for one account: its file path and trimmed, non-empty contents. `None` when
/// the account was never added or its secret was deleted.
pub fn cred_for(config_dir: &FsPath, account: &str) -> Option<(PathBuf, String)> {
    let path = account_file(config_dir, account);
    let token = read_trimmed(&path)?;
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
    // The read-modify-write of claude-accounts.json is one critical section over `config_write`,
    // like the settings saves: two accounts created at once cannot lose each other's entry.
    let _config = app.config_write.lock().await;
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
    // Same critical section as `create`: the removal's read and its save cannot lose an account a
    // concurrent create added. The checks in between read other state only, so the section spans
    // nothing that takes `config_write` itself.
    let _config = app.config_write.lock().await;
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

    fn account_app() -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-accounts-app-{}", short_id()));
        (crate::tests::test_app(&root), root)
    }

    /// `delete` reads the index and then parks on the sessions read while it checks the account is
    /// not live, so a create that ran during that pause used to be erased by delete's save of its
    /// older copy. Inside the config-write section the parked delete saves first and the create
    /// re-reads what it saved, so its account survives — and the account delete came for still goes.
    #[tokio::test]
    async fn a_delete_parked_on_its_checks_does_not_lose_a_concurrent_create() {
        let (app, root) = account_app();
        for id in ["a", "x"] {
            let Json(created) = create(
                State(app.clone()),
                Json(CreateAccount {
                    id: Some(id.into()),
                    label: None,
                    token: Some(format!("sk-ant-oat-{id}")),
                }),
            )
            .await
            .unwrap();
            assert_eq!(created["id"], *id);
        }
        // Hold the sessions lock so `delete` parks mid-handler, and let the create run there.
        let sessions = app.sessions.write().await;
        let del = tokio::spawn(delete(State(app.clone()), Path("x".to_string())));
        tokio::task::yield_now().await;
        let add = tokio::spawn(create(
            State(app.clone()),
            Json(CreateAccount {
                id: Some("b".into()),
                label: None,
                token: Some("sk-ant-oat-b".into()),
            }),
        ));
        tokio::task::yield_now().await;
        drop(sessions);
        let Json(_) = del.await.unwrap().unwrap();
        let Json(_) = add.await.unwrap().unwrap();
        let meta = load_meta(&app.cfg.config_dir);
        assert!(
            meta.accounts.contains_key("b"),
            "the create that ran while delete was parked survives delete's save, got {:?}",
            meta.accounts.keys().collect::<Vec<_>>()
        );
        assert!(meta.accounts.contains_key("a"), "the untouched account stays");
        assert!(!meta.accounts.contains_key("x"), "the account delete came for still goes");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `save_meta` writes a temp beside the target before renaming it into place. Four threads
    /// saving at once must share no temp file: a fixed one made whichever renamed second find the
    /// file already consumed — a failed save with nothing wrong with its contents.
    #[test]
    fn concurrent_saves_share_no_temp_file() {
        let dir = temp_config();
        let errors = std::sync::Mutex::new(Vec::<String>::new());
        std::thread::scope(|scope| {
            for t in 0..4 {
                let dir = dir.clone();
                let errors = &errors;
                scope.spawn(move || {
                    for r in 0..25 {
                        let id = format!("t{t}-r{r}");
                        let meta = AccountsMeta {
                            default: id.clone(),
                            accounts: BTreeMap::from([(
                                id.clone(),
                                AccountMeta {
                                    label: id.clone(),
                                    added_at: Utc::now(),
                                },
                            )]),
                        };
                        if let Err(e) = save_meta(&dir, &meta) {
                            errors.lock().unwrap().push(format!("{id}: {e:#}"));
                        }
                    }
                });
            }
        });
        let errors = errors.into_inner().unwrap();
        assert!(
            errors.is_empty(),
            "every save succeeds on its own temp, got: {}",
            errors.join("; ")
        );
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp is left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The migration's publish is create-if-absent. `migrate_legacy` checks for the record, then
    /// reads the legacy token and writes the secret before publishing, and a locked `create` can
    /// save the record in that gap — the migration's copy of just `default` must not write over
    /// it. The gap itself has no hook to pause on, so this drives the publish helper with the
    /// record already there, which is the state the migration is in when it reaches the publish.
    #[test]
    fn a_migration_publish_does_not_clobber_a_record_that_landed_mid_migration() {
        let dir = temp_config();
        // The `create` wins the gap: its account is in the record before the migration publishes.
        save_meta(
            &dir,
            &AccountsMeta {
                default: "new".to_string(),
                accounts: BTreeMap::from([(
                    "new".to_string(),
                    AccountMeta {
                        label: "new".to_string(),
                        added_at: Utc::now(),
                    },
                )]),
            },
        )
        .unwrap();
        // The migration, still holding its pre-gap view that the record is absent, publishes.
        let migrated = publish_meta_no_clobber(
            &dir,
            &AccountsMeta {
                default: "default".to_string(),
                accounts: BTreeMap::from([(
                    "default".to_string(),
                    AccountMeta {
                        label: "default".to_string(),
                        added_at: Utc::now(),
                    },
                )]),
            },
        )
        .unwrap();
        assert!(
            !migrated,
            "a record that exists is nothing to migrate, not something to replace"
        );
        let meta = load_meta(&dir);
        assert_eq!(meta.accounts.len(), 1, "the migration's copy did not merge in");
        assert_eq!(
            meta.accounts["new"].label, "new",
            "the concurrent create survives the migration"
        );
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "the refused temp is cleaned up: {leftovers:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
