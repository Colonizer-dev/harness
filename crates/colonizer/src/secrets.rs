//! Saved secrets: the system keychain first, the 0600 file second.
//!
//! Every saved secret — provider keys, the GitHub and Claude tokens, voice, memory and notification
//! keys — is read and written through `util::{read,write,delete}_secret`, keyed by its file path
//! under the config directory. This module sits underneath those three: a secret whose index entry
//! says `keychain` lives in the system keychain (Keychain on macOS, the Secret Service on Linux) as
//! a generic password under service [`SERVICE`] and account = its path relative to the config dir;
//! every other secret stays the file it always was, so an existing install keeps working unchanged.
//!
//! Nothing moves by itself. A secret already on file stays on file until the operator moves it
//! (`POST /api/secrets/{id}/move`); only a secret saved for the first time goes straight to the
//! keychain, and only when the keychain answered the startup probe. A headless Linux host (no
//! D-Bus session, or a locked keyring) fails the probe and keeps saving to files.
//!
//! The index (`<config>/secrets.json`) records where each secret lives and when it last changed —
//! never a value. Keychain values are cached in memory once read, so a gateway request does not
//! cross into the keychain on every call; writes, deletes and moves keep the cache in step.

use crate::{ApiResult, App, Shared, client_error, util};
use anyhow::{Context, Result, anyhow};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path as FsPath, PathBuf},
    sync::{Mutex, OnceLock, mpsc},
    time::Duration,
};

/// The keychain service every item is saved under.
pub const SERVICE: &str = "dev.colonizer";
/// The account the startup probe writes, reads back and deletes.
const PROBE_ACCOUNT: &str = "__colonizer_probe__";
/// How long a keychain call may take before the store treats the keychain as unavailable: a locked
/// Secret Service can sit waiting for an unlock prompt nobody will answer on a headless host.
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// A saved secret is a key or a token, never a document.
const MAX_VALUE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Keychain,
    File,
    /// Only an environment variable supplies it; nothing is saved.
    Env,
    Unset,
}

/// One keychain. The real one is [`OsKeychain`]; tests use an in-memory stand-in.
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// The system keychain through the `keyring` crate.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct OsKeychain;

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Backend for OsKeychain {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Secret Service"
        }
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        match keyring::Entry::new(SERVICE, account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keychain read failed: {e}")),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, account)?
            .set_password(value)
            .map_err(|e| anyhow!("keychain write failed: {e}"))
    }

    fn delete(&self, account: &str) -> Result<()> {
        match keyring::Entry::new(SERVICE, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!("keychain delete failed: {e}")),
        }
    }
}

/// The platform's keychain, where there is one.
pub fn os_backend() -> Option<Box<dyn Backend>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return Some(Box::new(OsKeychain));
    #[allow(unreachable_code)]
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub location: Location,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Health {
    pub available: bool,
    pub backend: &'static str,
    /// Why the keychain is not in use, in words the cockpit shows as they are.
    pub reason: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
}

pub struct Store {
    config_dir: PathBuf,
    backend: Option<Box<dyn Backend>>,
    index: Mutex<BTreeMap<String, Meta>>,
    cache: Mutex<HashMap<String, String>>,
    health: Mutex<Health>,
}

static STORE: OnceLock<Store> = OnceLock::new();

/// Installs the mothership's store; `util`'s secret helpers route through it from then on. Without
/// one (unit tests, the CLI) they read and write files exactly as before.
pub fn install(store: Store) {
    let _ = STORE.set(store);
}

pub fn global() -> Option<&'static Store> {
    STORE.get()
}

impl Store {
    pub fn new(config_dir: &FsPath, backend: Option<Box<dyn Backend>>) -> Self {
        let index = std::fs::read(index_file(config_dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let name = backend.as_ref().map(|b| b.name()).unwrap_or("none");
        Self {
            config_dir: config_dir.to_path_buf(),
            backend,
            index: Mutex::new(index),
            cache: Mutex::new(HashMap::new()),
            health: Mutex::new(Health {
                available: false,
                backend: name,
                reason: Some("not checked yet".into()),
                checked_at: None,
            }),
        }
    }

    /// The account a secret file maps to: its path under the config directory, `/`-separated.
    /// `None` for anything outside it, which stays file-only.
    pub fn account(&self, path: &FsPath) -> Option<String> {
        let rel = path.strip_prefix(&self.config_dir).ok()?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| match c {
                Component::Normal(p) => p.to_str().map(str::to_string),
                _ => None,
            })
            .collect::<Option<_>>()?;
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    pub fn health(&self) -> Health {
        lock(&self.health).clone()
    }

    /// Writes, reads back and deletes a canary item, each under [`KEYCHAIN_TIMEOUT`], and records
    /// whether the keychain can be used. Keychain secrets are then read into the cache, so the one
    /// macOS access prompt a rebuilt binary can trigger comes at startup, not mid-request.
    pub fn probe(&self) {
        let outcome = match &self.backend {
            None => Err("this platform has no supported keychain".to_string()),
            Some(backend) => probe_backend(backend.as_ref()),
        };
        {
            let mut health = lock(&self.health);
            health.checked_at = Some(Utc::now());
            match outcome {
                Ok(()) => {
                    health.available = true;
                    health.reason = None;
                }
                Err(reason) => {
                    health.available = false;
                    health.reason = Some(reason);
                }
            }
        }
        let keychain: Vec<String> = lock(&self.index)
            .iter()
            .filter(|(_, m)| m.location == Location::Keychain)
            .map(|(a, _)| a.clone())
            .collect();
        for account in keychain {
            let _ = self.keychain_get(&account);
        }
    }

    /// Where a secret lives now: the index for keychain items, else whether its file exists.
    pub fn location(&self, path: &FsPath) -> Location {
        if let Some(account) = self.account(path)
            && lock(&self.index)
                .get(&account)
                .is_some_and(|m| m.location == Location::Keychain)
        {
            return Location::Keychain;
        }
        if file_present(path) { Location::File } else { Location::Unset }
    }

    pub fn updated_at(&self, path: &FsPath) -> Option<DateTime<Utc>> {
        let account = self.account(path)?;
        lock(&self.index).get(&account).map(|m| m.updated_at)
    }

    /// `Some` when the secret lives in the keychain — its value, or `None` if the keychain could
    /// not give it — and `None` when the caller should read the file as before.
    pub fn read(&self, path: &FsPath) -> Option<Option<String>> {
        let account = self.account(path)?;
        let in_keychain = lock(&self.index)
            .get(&account)
            .is_some_and(|m| m.location == Location::Keychain);
        in_keychain.then(|| self.keychain_get(&account).ok().flatten())
    }

    /// Saves to the keychain when the secret already lives there, or when it is new and the keychain
    /// is available, and says whether it did. `false` leaves the file write to the caller.
    pub fn write(&self, path: &FsPath, value: &str) -> Result<bool> {
        let Some(account) = self.account(path) else { return Ok(false) };
        let to_keychain = match self.location(path) {
            Location::Keychain => true,
            Location::Unset => self.health().available,
            Location::File | Location::Env => false,
        };
        if !to_keychain {
            return Ok(false);
        }
        self.keychain_set(&account, value)?;
        remove_files(path);
        self.record(&account, Location::Keychain)?;
        Ok(true)
    }

    /// Records a file write, for the page's "last changed".
    pub fn note_file(&self, path: &FsPath) {
        if let Some(account) = self.account(path) {
            let _ = self.record(&account, Location::File);
        }
    }

    /// Forgets a secret everywhere it could be: the keychain item, the cache and the index.
    pub fn forget(&self, path: &FsPath) {
        let Some(account) = self.account(path) else { return };
        let was_keychain = lock(&self.index)
            .remove(&account)
            .is_some_and(|m| m.location == Location::Keychain);
        lock(&self.cache).remove(&account);
        if was_keychain && let Some(backend) = &self.backend {
            let _ = with_timeout(|| backend.delete(&account).map_err(|e| e.to_string()));
        }
        let _ = self.save_index();
    }

    /// Moves a saved secret between the keychain and its file, value intact.
    pub fn move_to(&self, path: &FsPath, to: Location) -> Result<()> {
        let account = self.account(path).context("this secret is not under the config directory")?;
        let from = self.location(path);
        if from == to {
            return Ok(());
        }
        match (from, to) {
            (Location::File, Location::Keychain) => {
                if !self.health().available {
                    anyhow::bail!("the keychain is not available on this host");
                }
                let value = util::read_file_secret(path).context("the saved file could not be read")?;
                self.keychain_set(&account, &value)?;
                remove_files(path);
                self.record(&account, Location::Keychain)
            }
            (Location::Keychain, Location::File) => {
                let value = self
                    .keychain_get(&account)?
                    .context("the keychain no longer holds this secret")?;
                util::write_file_secret(path, &value)?;
                if let Some(backend) = &self.backend {
                    with_timeout(|| backend.delete(&account).map_err(|e| e.to_string())).map_err(|e| anyhow!(e))?;
                }
                lock(&self.cache).remove(&account);
                self.record(&account, Location::File)
            }
            _ => anyhow::bail!("nothing is saved to move"),
        }
    }

    fn keychain_get(&self, account: &str) -> Result<Option<String>> {
        if let Some(hit) = lock(&self.cache).get(account) {
            return Ok(Some(hit.clone()));
        }
        let backend = self.backend.as_ref().context("no keychain on this platform")?;
        let value = with_timeout(|| backend.get(account).map_err(|e| e.to_string())).map_err(|e| anyhow!(e))?;
        if let Some(value) = &value {
            lock(&self.cache).insert(account.to_string(), value.clone());
        }
        Ok(value)
    }

    fn keychain_set(&self, account: &str, value: &str) -> Result<()> {
        let backend = self.backend.as_ref().context("no keychain on this platform")?;
        with_timeout(|| backend.set(account, value).map_err(|e| e.to_string())).map_err(|e| anyhow!(e))?;
        lock(&self.cache).insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn record(&self, account: &str, location: Location) -> Result<()> {
        lock(&self.index).insert(
            account.to_string(),
            Meta {
                location,
                updated_at: Utc::now(),
            },
        );
        self.save_index()
    }

    fn save_index(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&*lock(&self.index))?;
        util::write_private(&index_file(&self.config_dir), &bytes)
    }
}

fn index_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("secrets.json")
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn file_present(path: &FsPath) -> bool {
    path.exists() || util::enc_path(path).exists()
}

fn remove_files(path: &FsPath) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(util::enc_path(path));
}

/// Runs one keychain call on its own thread under [`KEYCHAIN_TIMEOUT`]. A call that never returns
/// (a Secret Service waiting on an unlock prompt) is abandoned, not waited on.
fn with_timeout<T: Send + 'static>(call: impl FnOnce() -> Result<T, String> + Send) -> Result<T, String> {
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || {
            let _ = tx.send(call());
        });
        match rx.recv_timeout(KEYCHAIN_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(format!(
                "the keychain did not answer within {}s — it may be locked, or this session has none",
                KEYCHAIN_TIMEOUT.as_secs()
            )),
        }
    })
}

fn probe_backend(backend: &dyn Backend) -> Result<(), String> {
    let canary = util::random_token();
    with_timeout(|| backend.set(PROBE_ACCOUNT, &canary).map_err(|e| e.to_string()))?;
    let read = with_timeout(|| backend.get(PROBE_ACCOUNT).map_err(|e| e.to_string()))?;
    let _ = with_timeout(|| backend.delete(PROBE_ACCOUNT).map_err(|e| e.to_string()));
    if read.as_deref() == Some(canary.as_str()) {
        Ok(())
    } else {
        Err("the keychain accepted a test item but did not give it back".into())
    }
}

// ---------------------------------------------------------------------------------------------
// The catalogue and the API
// ---------------------------------------------------------------------------------------------

/// One secret the Secrets page lists.
struct Item {
    id: String,
    label: String,
    group: &'static str,
    used_by: String,
    icon: &'static str,
    /// The file it is saved as; `None` for a secret only an environment variable supplies.
    path: Option<PathBuf>,
    env: Option<&'static str>,
    /// Whether the page may set, replace, remove and move it.
    editable: bool,
    /// What a colony gets of it.
    colonies: Access,
}

/// How a secret reaches colonies, if at all.
enum Access {
    /// The mothership's gateway adds it to model requests; it never enters a microVM.
    Gateway,
    /// Handed to the colony as a placeholder that msb swaps for the value on TLS to these hosts.
    Injected(Vec<String>),
    /// Used by the mothership alone.
    None,
}

impl Access {
    fn json(&self) -> Value {
        match self {
            Access::Gateway => json!({"kind": "gateway", "hosts": []}),
            Access::Injected(hosts) => json!({"kind": "injected", "hosts": hosts}),
            Access::None => json!({"kind": "none", "hosts": []}),
        }
    }
}

/// Every secret this mothership knows about, whether or not it is set.
fn catalog(app: &App) -> Vec<Item> {
    let dir = &app.cfg.config_dir;
    let mut items = vec![
        Item {
            id: "github-token".into(),
            label: "GitHub token".into(),
            group: "connections",
            used_by: "Issues, pushes and pull requests".into(),
            icon: "github",
            path: Some(app.github_token_file()),
            env: None,
            editable: true,
            colonies: Access::None,
        },
        Item {
            id: "claude-token".into(),
            label: "Claude token".into(),
            group: "connections",
            used_by: "Every Claude colony".into(),
            icon: "claude",
            path: Some(app.claude_token_file()),
            env: None,
            editable: true,
            colonies: Access::Injected(vec![crate::CLAUDE_API_HOST.into()]),
        },
    ];
    for (id, meta) in crate::claude_accounts::load_meta(dir).accounts {
        items.push(Item {
            id: format!("claude-accounts:{id}"),
            label: if meta.label.is_empty() {
                format!("Claude account {id}")
            } else {
                format!("Claude account · {}", meta.label)
            },
            group: "connections",
            used_by: "Colonies assigned to this account".into(),
            icon: "claude",
            path: Some(crate::claude_accounts::account_file(dir, &id)),
            env: None,
            editable: true,
            colonies: Access::Injected(vec![crate::CLAUDE_API_HOST.into()]),
        });
    }
    items.push(Item {
        id: "api-token".into(),
        label: "Cockpit API token".into(),
        group: "connections",
        used_by: "The cockpit sign-in and the colonizer CLI".into(),
        icon: "key",
        path: Some(crate::auth::token_file(dir)),
        env: None,
        // The CLI reads it straight off disk, so it stays a file and is not edited here.
        editable: false,
        colonies: Access::None,
    });
    for provider in app.providers() {
        items.push(Item {
            id: format!("provider-keys:{}", provider.id),
            label: provider.name.clone(),
            group: "providers",
            used_by: format!("Models routed to {}", provider.id),
            icon: "plug",
            path: Some(app.provider_key_file(&provider.id)),
            env: None,
            editable: true,
            colonies: Access::Gateway,
        });
    }
    for svc in crate::voice::SERVICES.iter() {
        items.push(Item {
            id: format!("voice-keys:{}", svc.id),
            label: format!("{} (voice)", svc.name),
            group: "integrations",
            used_by: "Speech to text in the composer".into(),
            icon: "mic",
            path: Some(dir.join("voice-keys").join(svc.id)),
            env: Some(svc.env),
            editable: true,
            colonies: Access::None,
        });
    }
    items.push(Item {
        id: "memory-keys:mem0".into(),
        label: "mem0".into(),
        group: "integrations",
        used_by: "Shared memory (mem0 provider)".into(),
        icon: "memory",
        path: Some(dir.join("memory-keys").join("mem0")),
        env: Some("MEM0_API_KEY"),
        editable: true,
        colonies: Access::None,
    });
    items.push(Item {
        id: "notify-secret".into(),
        label: "Notification signing secret".into(),
        group: "integrations",
        used_by: "Signs outgoing notification webhooks".into(),
        icon: "bell",
        path: Some(dir.join("notify-secret")),
        env: None,
        editable: true,
        colonies: Access::None,
    });
    items.push(Item {
        id: "jev".into(),
        label: "TypeSafe (Jev)".into(),
        group: "integrations",
        used_by: "Jev compaction".into(),
        icon: "spark",
        path: None,
        env: Some("JEV_API_KEY"),
        editable: false,
        colonies: Access::Injected(vec!["api.typesafe.ai".into()]),
    });
    for secret in crate::colony_secrets::load(dir).unwrap_or_default() {
        items.push(Item {
            id: secret.id(),
            label: secret.env.clone(),
            group: "colonies",
            used_by: match &secret.scope {
                crate::colony_secrets::Scope::All => "Every colony".into(),
                crate::colony_secrets::Scope::Org { org } => format!("Colonies on {org} repositories"),
                crate::colony_secrets::Scope::Repo { repo } => format!("Colonies on {repo}"),
            },
            icon: "key",
            path: Some(crate::colony_secrets::value_path(dir, &secret.env)),
            env: None,
            editable: true,
            colonies: Access::Injected(secret.hosts.clone()),
        });
    }
    items
}

fn find(app: &App, id: &str) -> Result<Item, crate::AppError> {
    catalog(app)
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such secret"))
}

fn store() -> Result<&'static Store, crate::AppError> {
    global().ok_or_else(|| client_error(StatusCode::SERVICE_UNAVAILABLE, "the secret store is not running"))
}

fn row(store: &Store, item: &Item) -> Value {
    let saved = item.path.as_deref().map(|p| store.location(p)).unwrap_or(Location::Unset);
    let env_set = item.env.is_some_and(|name| util::env_nonempty(name).is_some());
    let location = match saved {
        Location::Unset if env_set => Location::Env,
        other => other,
    };
    json!({
        "id": item.id,
        "label": item.label,
        "group": item.group,
        "used_by": item.used_by,
        "icon": item.icon,
        "location": location,
        "env": item.env,
        "env_set": env_set,
        "updated_at": item.path.as_deref().and_then(|p| store.updated_at(p)),
        "editable": item.editable && item.path.is_some(),
        "colonies": item.colonies.json(),
    })
}

/// `GET /api/secrets`: every secret with where it lives, never a value.
pub async fn list(State(app): State<Shared>) -> ApiResult<Value> {
    let store = store()?;
    let rows: Vec<Value> = catalog(&app).iter().map(|item| row(store, item)).collect();
    Ok(Json(json!({ "keychain": store.health(), "secrets": rows })))
}

/// `GET /api/secrets/health`: whether the keychain answered.
pub async fn health(State(_app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(json!(store()?.health())))
}

#[derive(Deserialize)]
pub struct PutSecret {
    value: String,
    /// Where to save it; omitted keeps it where it is (a new secret: the keychain when available).
    #[serde(default)]
    location: Option<Location>,
}

/// `PUT /api/secrets/{id}`: sets or replaces a secret. The value is never echoed back.
pub async fn put(State(app): State<Shared>, Path(id): Path<String>, Json(body): Json<PutSecret>) -> ApiResult<Value> {
    let store = store()?;
    let item = find(&app, &id)?;
    let path = editable_path(&item)?;
    let value = body.value.trim().to_string();
    if value.is_empty() || value.len() > MAX_VALUE_BYTES || value.contains(['\n', '\r']) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "a secret is one non-empty line of at most 16 KB",
        ));
    }
    let target = body.location;
    let saved = tokio::task::spawn_blocking(move || -> Result<()> {
        util::write_secret(&path, &value)?;
        if let Some(to @ (Location::Keychain | Location::File)) = target {
            store.move_to(&path, to)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!(e))?;
    saved.map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))?;
    Ok(Json(row(store, &item)))
}

/// `DELETE /api/secrets/{id}`: removes a saved secret from the keychain and its file.
pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let store = store()?;
    let item = find(&app, &id)?;
    let path = editable_path(&item)?;
    tokio::task::spawn_blocking(move || util::delete_secret(&path))
        .await
        .map_err(|e| anyhow!(e))?;
    // A colony secret is its registry entry as much as its value: removing it stops offering it.
    if let Some(env) = id.strip_prefix("colony:") {
        crate::colony_secrets::remove(&app.cfg.config_dir, env)
            .map_err(|e| client_error(StatusCode::CONFLICT, &format!("{e:#}")))?;
        return Ok(Json(json!({ "id": id, "removed": true })));
    }
    Ok(Json(row(store, &item)))
}

#[derive(Deserialize)]
pub struct MoveSecret {
    to: Location,
}

/// `POST /api/secrets/{id}/move`: between the keychain and the 0600 file.
pub async fn move_secret(State(app): State<Shared>, Path(id): Path<String>, Json(body): Json<MoveSecret>) -> ApiResult<Value> {
    let store = store()?;
    let item = find(&app, &id)?;
    let path = editable_path(&item)?;
    if !matches!(body.to, Location::Keychain | Location::File) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "move a secret to \"keychain\" or \"file\"",
        ));
    }
    let moved = tokio::task::spawn_blocking(move || store.move_to(&path, body.to))
        .await
        .map_err(|e| anyhow!(e))?;
    moved.map_err(|e| client_error(StatusCode::CONFLICT, &format!("{e:#}")))?;
    Ok(Json(row(store, &item)))
}

fn editable_path(item: &Item) -> Result<PathBuf, crate::AppError> {
    match (&item.path, item.editable) {
        (Some(path), true) => Ok(path.clone()),
        _ => Err(client_error(
            StatusCode::BAD_REQUEST,
            "this secret is not managed here (it comes from the environment or is read by the CLI)",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory keychain; `broken` makes every call fail, as a headless Linux host's would.
    #[derive(Default)]
    struct Fake {
        items: Mutex<HashMap<String, String>>,
        broken: bool,
    }

    impl Backend for std::sync::Arc<Fake> {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn get(&self, account: &str) -> Result<Option<String>> {
            if self.broken {
                anyhow::bail!("no session bus");
            }
            Ok(lock(&self.items).get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<()> {
            if self.broken {
                anyhow::bail!("no session bus");
            }
            lock(&self.items).insert(account.into(), value.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<()> {
            lock(&self.items).remove(account);
            Ok(())
        }
    }

    fn setup(broken: bool) -> (Store, std::sync::Arc<Fake>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("colonizer-secrets-{}", util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = std::sync::Arc::new(Fake {
            broken,
            ..Default::default()
        });
        let store = Store::new(&dir, Some(Box::new(fake.clone())));
        store.probe();
        (store, fake, dir)
    }

    #[test]
    fn accounts_are_paths_under_the_config_dir_and_nothing_else() {
        let (store, _, dir) = setup(false);
        assert_eq!(
            store.account(&dir.join("provider-keys").join("zai")).as_deref(),
            Some("provider-keys/zai")
        );
        assert_eq!(store.account(FsPath::new("/etc/passwd")), None);
        assert_eq!(store.account(&dir.join("..").join("x")), None, "no escaping the config dir");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_secret_goes_to_the_keychain_and_an_old_file_stays_put() {
        let (store, fake, dir) = setup(false);
        assert!(store.health().available);

        let new = dir.join("provider-keys").join("zai");
        assert!(store.write(&new, "sk-new").unwrap(), "new + keychain available = keychain");
        assert_eq!(store.location(&new), Location::Keychain);
        assert_eq!(lock(&fake.items).get("provider-keys/zai").map(String::as_str), Some("sk-new"));
        assert!(!new.exists(), "no plaintext copy is left behind");
        assert_eq!(store.read(&new), Some(Some("sk-new".into())));

        let old = dir.join("github-token");
        std::fs::write(&old, "ghp_old").unwrap();
        assert!(
            !store.write(&old, "ghp_newer").unwrap(),
            "a file secret is not moved by a save"
        );
        assert_eq!(store.location(&old), Location::File);
        assert_eq!(store.read(&old), None, "the caller reads the file as before");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_probe_keeps_every_save_on_file() {
        let (store, fake, dir) = setup(true);
        let health = store.health();
        assert!(!health.available);
        assert!(health.reason.unwrap().contains("no session bus"));
        let new = dir.join("memory-keys").join("mem0");
        assert!(!store.write(&new, "m0").unwrap());
        assert!(lock(&fake.items).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn moves_carry_the_value_both_ways_and_forget_clears_everything() {
        let (store, fake, dir) = setup(false);
        let path = dir.join("voice-keys").join("openai");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        util::write_file_secret(&path, "sk-voice").unwrap();
        assert_eq!(store.location(&path), Location::File);

        store.move_to(&path, Location::Keychain).unwrap();
        assert_eq!(store.location(&path), Location::Keychain);
        assert!(!path.exists());
        assert_eq!(store.read(&path), Some(Some("sk-voice".into())));

        store.move_to(&path, Location::File).unwrap();
        assert_eq!(store.location(&path), Location::File);
        assert_eq!(util::read_file_secret(&path).as_deref(), Some("sk-voice"));
        assert!(lock(&fake.items).is_empty(), "the keychain copy is gone");

        store.move_to(&path, Location::Keychain).unwrap();
        store.forget(&path);
        assert_eq!(store.location(&path), Location::Unset);
        assert!(lock(&fake.items).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_index_survives_a_restart_and_holds_no_values() {
        let (store, fake, dir) = setup(false);
        let path = dir.join("notify-secret");
        store.write(&path, "whsec").unwrap();
        let index = std::fs::read_to_string(index_file(&dir)).unwrap();
        assert!(!index.contains("whsec"), "{index}");
        let again = Store::new(&dir, Some(Box::new(fake.clone())));
        assert_eq!(again.location(&path), Location::Keychain);
        assert_eq!(again.read(&path), Some(Some("whsec".into())));
        let _ = std::fs::remove_dir_all(dir);
    }
}
