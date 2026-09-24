//! Colonizer: turn a task into a pull request by running a coding agent in a microVM, with a
//! web UI for chat (questions as choice cards), a terminal in the VM, and a private mesh network
//! between the harness and every VM. Every moving part is a module; see docs/architecture.md.
//!
//! Trust model: a microVM only sees its worktree (rw), the repository's git objects (ro), its
//! session files (ro) and an output directory (rw). The GitHub token never enters a VM; the Claude
//! credential is injected by microsandbox's host-side TLS proxy for the API host only, and model
//! provider keys are added by the mothership's provider gateway.

mod auth;
mod authority;
mod autonomy;
mod burn_down;
mod claims;
mod claude_accounts;
mod claude_login;
mod colony_secrets;
mod config;
mod diagnosis;
mod events;
mod exec_bits;
mod execution;
mod findings;
mod fleet;
mod gateway;
mod github;
mod headroom;
mod hunters;
mod jev;
mod lifecycle;
mod mem0;
mod memory;
mod mesh;
mod modules;
mod notify;
mod openai;
mod orgs;
mod packages;
mod plugins;
mod presets;
mod protocol;
mod provider_quota;
mod providers;
mod publish;
mod queue;
mod rebase;
mod reclaim;
mod redteam;
mod restack;
mod routing;
mod runtime;
mod sandbox;
mod secrets;
mod sessions;
mod spend;
mod stack;
mod stale;
mod stream;
mod telemetry;
mod timing;
mod update;
mod usage;
mod util;
mod validation;
mod version;
mod voice;
mod watchdog;

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Query, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post, put},
};
use chrono::{DateTime, Utc};
use config::{ModulesConfig, Settings, setting_u64};
use mesh::{Mesh, Ports};
use modules::AgentModule;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sessions::Session;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    path::{Path as FsPath, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
};
use tower_http::services::{ServeDir, ServeFile};
use util::{delete_secret, env_nonempty, exec_within, is_elf, is_plain_name, read_secret};

pub const CLAUDE_API_HOST: &str = "api.anthropic.com";

const UI_MISSING_HTML: &str = "<!doctype html><title>Colonizer</title>\
<body style=\"font:15px system-ui;margin:3rem\"><h1>Colonizer is running</h1>\
<p>The web UI isn't built yet. Run <code>scripts/install.sh</code> (or <code>npm run build</code> in <code>web/</code>).</p>";

pub struct ClaudeCred {
    pub env: &'static str,
    pub value: String,
    pub source: &'static str,
}

/// What a `StorageAlert` reports, which decides whether it can recover. Serialized as the `kind` of
/// `/api/status`'s `storage` key: `"write"` or `"load_damage"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAlertKind {
    /// A write failed; the next one that goes through recovers it.
    Write,
    /// `sessions.json` was damaged at startup and colonies went missing from the list. No later
    /// save brings them back, so it never recovers: it stays until the operator dismisses it.
    LoadDamage,
}

/// A confirmed storage failure, shown by the UI until it is dismissed. Sticky on purpose: a later
/// successful write does not clear it, because the gap the alert reports did happen. What the
/// write does change, for a `Write` alert, is `recovered_at`, so the alert can say the mothership
/// is writing again instead of reporting a healthy disk as broken until the next restart.
#[derive(Clone, Debug)]
pub struct StorageAlert {
    pub kind: StorageAlertKind,
    pub message: String,
    pub ts: DateTime<Utc>,
    pub failures: u64,
    /// When a write first succeeded after this failure; `None` while writes are still failing.
    pub recovered_at: Option<DateTime<Utc>>,
}

pub struct App {
    pub cfg: Settings,
    /// The per-install cockpit API token (`<config_dir>/api-token`), checked by `host_guard`.
    pub api_token: String,
    pub modules: RwLock<ModulesConfig>,
    pub agents: Vec<AgentModule>,
    /// Agent manifests that are present but unusable, one line each naming the file and the fault;
    /// logged at boot and reported as the agent kind's `manifest_errors` in GET /api/modules.
    pub agent_problems: Vec<String>,
    pub sessions: RwLock<Vec<Session>>,
    pub redteam: redteam::RedTeamStore,
    session_persist: Mutex<()>,
    /// Serialises the read-modify-write of `orgs.json` and `providers.json` (`orgs::put`,
    /// `providers::put`/`delete`): one strict read, the modify, and the save happen as one
    /// critical section, so two settings saves at once cannot lose each other's orgs or
    /// providers, and no save ever follows a read that failed (#408).
    pub config_write: Mutex<()>,
    /// The `LoadDamage` a strict read of `orgs.json` or `providers.json` raised, recorded by
    /// [`App::read_config_loud`]. A std mutex because those readers are sync and hot (the
    /// gateway reads `providers()` for every request), unlike the tokio `storage_alert`, which
    /// only writers touch. Cleared when the file it names reads cleanly again.
    pub config_damage: std::sync::Mutex<Option<StorageAlert>>,
    /// The latest write failure. Only `Write` alerts go here.
    pub storage_alert: RwLock<Option<StorageAlert>>,
    /// The last queue-tick free-space verdict, refreshed where the queue
    /// computes its admission pause so `/api/status` reads it with no I/O.
    pub disk_verdict: Mutex<reclaim::FreeSpaceVerdict>,
    /// The `LoadDamage` alert `load_sessions` raised at startup, set once and never changed. Kept
    /// apart from `storage_alert` so a later write failure cannot overwrite it and its `.corrupt-`
    /// path, nor a later save mark it recovered.
    pub load_damage: Option<StorageAlert>,
    pub runtimes: Mutex<HashMap<String, Arc<sessions::Runtime>>>,
    repo_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// One lifecycle lock per colony id. It serialises the moments a colony gains or loses its
    /// microVM — the claim that starts a boot, and the teardown that removes one — so a resume can
    /// never claim a colony whose stop is still tearing that VM down, and a cleanup can never free
    /// the worktree a boot is starting on. See `session_lock` for what it deliberately does not cover.
    session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    mesh: Mutex<Option<Arc<Mesh>>>,
    pub login: claude_login::LoginManager,
    pub memory: memory::MemoryStore,
    pub gateway: gateway::Gateway,
    /// Owners seen in the repository list, so org workspaces can be offered before any colony exists.
    pub repo_owners: RwLock<BTreeSet<String>>,
    /// Slow read-only answers (`/api/repos`, `/api/storage`) kept so a page load does not wait on
    /// `gh` or a disk walk: see [`cached_answer`].
    pub answer_cache: AnswerCache,
    /// GitHub orgs on the signed-in account that the operator has not answered for yet — login to
    /// avatar, shown with a prompt instead of being adopted silently. In-memory on purpose: after a
    /// restart `refresh_orgs` recomputes it from `known-orgs.json`.
    pub new_orgs: RwLock<BTreeMap<String, Option<String>>>,
    /// Each org's GitHub description, from the same `/user/orgs` fetch, for the workspace page.
    /// In-memory: the first refresh after a restart fills it again.
    pub org_descriptions: RwLock<BTreeMap<String, String>>,
    /// When the user's GitHub orgs were last fetched.
    pub orgs_refreshed: Mutex<Option<std::time::Instant>>,
    /// When the last org refresh failed, so a `gh` that keeps failing is retried once a minute
    /// rather than on every workspace poll.
    pub orgs_failed_at: Mutex<Option<std::time::Instant>>,
    /// The last Anthropic profile lookup for the Claude credential, cached so the status poll does not
    /// hammer Anthropic. Keyed on a fingerprint of the token; the token itself is never stored.
    pub claude_account: Mutex<Option<claude_login::AccountStatus>>,
    /// The last `gh api user` answer for the GitHub credential, cached so the status poll does not
    /// hammer GitHub. Keyed on a fingerprint of the token; the token itself is never stored.
    pub github_viewer: Mutex<Option<github::ViewerStatus>>,
    /// The Claude binaries found this run, keyed on whether the guest's ELF requirement was asked,
    /// so the status poll does not walk PATH and probe every candidate on every poll. Successes
    /// only: Settings re-renders the red "missing" row from every poll, so a binary installed while
    /// the harness runs must be picked up without a restart.
    pub claude_bins: Mutex<HashMap<bool, PathBuf>>,
    /// The last `runtime` probe for the status payload, cached so the poll does not re-spawn the
    /// version probes for every open tab. `GET /api/status?fresh=1` bypasses it.
    pub runtime_cache: Mutex<Option<runtime::Cached>>,
    /// The last host probe (name, size, disk), cached the same 10 s as the runtime probe. `?fresh=1`
    /// bypasses it.
    pub host_cache: Mutex<Option<runtime::HostCached>>,
    /// Boot-time provider probe results, keyed on provider id + base URL
    /// (`gateway::probe_cache_key`) so repointing a provider never serves the old endpoint's
    /// answer. Both reachable and unreachable answers are kept for [`gateway::PROVIDER_PROBE_TTL`];
    /// a dead provider would otherwise cost every boot up to the 5 s probe timeout.
    pub provider_probe_cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// The fleet view's peer half (issue #231): last-known `HostSummary` per configured peer base URL,
    /// so a peer that goes quiet still shows its last real numbers instead of nulls. This machine's
    /// own entry is never cached here — `fleet::self_summary` always computes it live.
    pub fleet_cache: fleet::FleetCache,
    /// The most recent background image pull, so Settings can show it.
    pub pull: Mutex<sandbox::PullStatus>,
    /// The Headroom bundle download, started when Headroom is switched on.
    pub headroom: Mutex<headroom::Status>,
    /// The live map on colonizer.dev, off until the user switches it on.
    pub telemetry: telemetry::Telemetry,
    pub updates: version::Updates,
    pub updater: update::Updater,
    /// Anonymous usage reporting, local half only: the batch that would be sent and the switch for it.
    pub usage: usage::Usage,
    /// The `GET /api/stream` push hub: one shared broadcast diff task for all open tabs.
    pub stream: stream::Hub,
}

pub type Shared = Arc<App>;

impl App {
    pub fn modules_file(&self) -> PathBuf {
        self.cfg.config_dir.join("modules.json")
    }

    pub fn claude_token_file(&self) -> PathBuf {
        self.cfg.config_dir.join("claude-token")
    }

    pub fn claude_cred(&self) -> Option<ClaudeCred> {
        self.claude_cred_for(None)
    }

    /// The credential for one Claude account: the requested account's stored secret, else the
    /// single-token file a pre-accounts install left behind, else the environment. `None` selects
    /// the install default, so `claude_cred` — every existing caller — keeps working unchanged.
    pub fn claude_cred_for(&self, account: Option<&str>) -> Option<ClaudeCred> {
        let _ = claude_accounts::migrate_legacy(&self.cfg.config_dir);
        let meta = claude_accounts::load_meta(&self.cfg.config_dir);
        let id = account.filter(|a| !a.is_empty()).map(str::to_string).unwrap_or_else(|| {
            if meta.default.is_empty() {
                "default".to_string()
            } else {
                meta.default.clone()
            }
        });
        let token = claude_accounts::cred_for(&self.cfg.config_dir, &id)
            .map(|(_, token)| token)
            .or_else(|| read_secret(&self.claude_token_file()));
        if let Some(token) = token {
            let (env, source) = claude_accounts::sniff(&token);
            return Some(ClaudeCred {
                env,
                value: token,
                source,
            });
        }
        if let Some(value) = env_nonempty("CLAUDE_CODE_OAUTH_TOKEN") {
            return Some(ClaudeCred {
                env: "CLAUDE_CODE_OAUTH_TOKEN",
                value,
                source: "CLAUDE_CODE_OAUTH_TOKEN",
            });
        }
        env_nonempty("ANTHROPIC_API_KEY").map(|value| ClaudeCred {
            env: "ANTHROPIC_API_KEY",
            value,
            source: "ANTHROPIC_API_KEY",
        })
    }

    pub async fn repo_lock(&self, repo: &str) -> Arc<Mutex<()>> {
        self.repo_locks.lock().await.entry(repo.to_string()).or_default().clone()
    }

    /// The colony's lifecycle lock, held from a resume's claim through the spawn of its boot, across a
    /// stop's (and `stop_colony`'s) claim and teardown, across a cleanup's claim and worktree removal,
    /// and across `watch_sandboxes`' teardown. The load-bearing hold is the stopping side: `stop` keeps
    /// the lock across its status flip and its teardown, so a resume cannot be admitted on the freshly
    /// written `stopped` while the removal is still in flight — without it, that resume would claim
    /// `starting` and boot a microVM under the one deterministic sandbox name the stop's in-flight
    /// `msb rm --force` is about to remove. A cleanup leans on the same serialisation: its `cleaned_up`
    /// claim and worktree removal are one critical section, so a resume waiting on the lock is refused
    /// outright rather than admitted onto the worktree being deleted. The resume's own hold through the
    /// spawn is deliberate but belt-and-braces — it keeps the claim and the handoff to `boot` in one
    /// critical section, and a stop landing before the spawn only makes the boot's first
    /// `ensure_starting` bail, there being no microVM yet to tear down.
    ///
    /// What it deliberately does not cover:
    ///
    /// - a whole boot. A boot runs for minutes and `stop` has to stay responsive, so a boot holds
    ///   nothing and is interrupted instead: the `ensure_starting` checkpoints bail once a stop has
    ///   flipped the status out of `starting`, and `boot`'s failure teardown runs before it flips the
    ///   colony to `failed`, so a resume admitted after that flip finds the teardown already done.
    /// - `publish`. Its claim (`can_publish`) refuses `starting` and `queued` — the only statuses a
    ///   boot can be in — so it can never tear a microVM down while a boot is creating one.
    /// - `delete`. It checks `deletable` and removes the record under one write guard, so a colony a
    ///   boot is starting on (`starting` is live) cannot be deleted mid-boot, and once the record is
    ///   gone nothing can claim the colony at all.
    pub async fn session_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.session_locks.lock().await.entry(id.to_string()).or_default().clone()
    }

    /// Records a confirmed storage failure: printed loudly here, kept as the sticky alert the UI
    /// shows, and counted so a run of failures reads as more than one.
    pub async fn storage_failed(&self, what: &str, err: &anyhow::Error) {
        eprintln!("storage: {what}: {err:#}");
        let mut alert = self.storage_alert.write().await;
        let failures = alert.as_ref().map_or(0, |a| a.failures) + 1;
        *alert = Some(StorageAlert {
            kind: StorageAlertKind::Write,
            message: format!("{what} failed: {err:#}"),
            ts: Utc::now(),
            failures,
            recovered_at: None,
        });
    }

    /// Records that a write went through, which is what turns a standing write alert into a
    /// recovered one. Only the first success after a failure stamps it, and the common case — no
    /// alert, or one already recovered — takes only the read lock, since every session-list save
    /// calls this. Only a `Write` alert can recover; load damage is kept out of `storage_alert` anyway.
    pub async fn storage_succeeded(&self) {
        let recoverable = |a: &StorageAlert| a.kind == StorageAlertKind::Write && a.recovered_at.is_none();
        if !self.storage_alert.read().await.as_ref().is_some_and(recoverable) {
            return;
        }
        if let Some(alert) = self.storage_alert.write().await.as_mut()
            && recoverable(alert)
        {
            alert.recovered_at = Some(Utc::now());
        }
    }

    /// The alert `/api/status` shows: a write failure that has not recovered, else damage to
    /// `orgs.json`/`providers.json` a reader is still falling back over, else the startup's load
    /// damage, else a write failure that has. The live reader damage outranks the sticky startup
    /// damage because only it can clear, and load damage hidden by a write failure comes back,
    /// unchanged, once writes go through again.
    async fn shown_storage_alert(&self) -> Option<StorageAlert> {
        match self.storage_alert.read().await.clone() {
            Some(write) if write.recovered_at.is_none() => Some(write),
            write => self
                .config_damage
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .or_else(|| self.load_damage.clone())
                .or(write),
        }
    }

    /// The read half of the strict-config rule (#408): `orgs.json` and `providers.json` have
    /// infallible, sync, hot readers (the gateway calls `providers()` per request), so a file that
    /// will not read or parse becomes the default — loudly. The failure is printed and recorded as
    /// a `LoadDamage` alert in [`App::config_damage`], deduped on its message so a request storm
    /// neither logs nor replaces it every call, and a file that reads cleanly again clears an
    /// alert that named it. The writers refuse to save over what this strict read rejects
    /// ([`config_unreadable`]), so the defaults never reach the disk from here.
    pub(crate) fn read_config_loud<T: serde::de::DeserializeOwned + Default>(&self, path: &FsPath, what: &str) -> T {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match util::read_json_or_default(path) {
            Ok(value) => {
                let mut damage = self.config_damage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if damage.as_ref().is_some_and(|a| a.message.contains(&file)) {
                    damage.take();
                }
                value
            }
            Err(e) => {
                let message = format!(
                    "{} could not be read ({e:#}); defaults are in effect for {what} until it is fixed or removed, and saves that would overwrite it are refused",
                    path.display()
                );
                // The lock guard is a plain mutex on a hot, infallible path, so a poison elsewhere
                // must not turn every read into a panic; the printing stays out of the guard.
                let mut damage = self.config_damage.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let fresh = !damage.as_ref().is_some_and(|a| a.message == message);
                if fresh {
                    *damage = Some(StorageAlert {
                        kind: StorageAlertKind::LoadDamage,
                        message: message.clone(),
                        ts: Utc::now(),
                        failures: 1,
                        recovered_at: None,
                    });
                }
                drop(damage);
                if fresh {
                    eprintln!("config: {message}");
                }
                T::default()
            }
        }
    }

    /// The mesh manager, created on first use from the bundled binaries and mesh module settings.
    pub async fn mesh(&self) -> Result<Arc<Mesh>> {
        let mut mesh = self.mesh.lock().await;
        if let Some(m) = mesh.as_ref() {
            return Ok(m.clone());
        }
        let assets = self
            .cfg
            .assets
            .clone()
            .context("app assets not found: run scripts/install.sh")?;
        let modules = self.modules.read().await;
        let schema = modules::schema_for("mesh", "headscale", &self.agents);
        let port = |key: &str| u16::try_from(setting_u64(&modules.mesh, &schema, key)).unwrap_or_default();
        let ports = Ports {
            control: port("control_port"),
            udp: port("udp_port"),
            socks: port("socks_port"),
        };
        let created = Arc::new(Mesh::new(&assets, &self.cfg.data_dir, &self.cfg.runtime_dir, ports));
        *mesh = Some(created.clone());
        Ok(created)
    }
}

/// A version probe is a local binary answering in milliseconds when healthy; 5s is orders of
/// magnitude above that, keeps the status handler's probes inside the 30s poll interval, and turns
/// a wedged binary into a skipped candidate instead of a caller that hangs forever.
const PROBE_LIMIT: Duration = Duration::from_secs(5);

/// The cap on the whole candidate walk, not just its probes. A walk that finds nothing repeats on
/// every 30s status poll (failures are not memoised), and N wedged candidates cost N ×
/// `PROBE_LIMIT`; 10s keeps even that inside a single poll.
const CLAUDE_WALK_LIMIT: Duration = Duration::from_secs(10);

/// The Claude Code binary mounted read-only into a colony. A colony is a Linux microVM, so this has
/// to be a Linux build: on a Mac the host's own is Mach-O and `scripts/install.sh` fetches one
/// beside the app instead.
pub async fn resolve_guest_claude_bin(app: &App) -> Result<PathBuf> {
    if app.cfg.asset("bin/claude-guest").is_ok() {
        return app.cfg.linux_binary("bin/claude-guest");
    }
    memoised_claude_bin(app, true, find_claude_bin(&app.cfg, true))
        .await
        .context("no Linux Claude Code binary found for the guest; run scripts/install.sh or set COLONIZER_CLAUDE_BIN")
}

/// The Claude Code binary the mothership runs itself, for `claude setup-token`. Never the guest's:
/// on a Mac that one is a Linux ELF, and the host cannot execute it.
pub async fn resolve_host_claude_bin(app: &App) -> Result<PathBuf> {
    memoised_claude_bin(app, false, find_claude_bin(&app.cfg, false))
        .await
        .context("no native Claude Code binary found; install Claude Code or set COLONIZER_CLAUDE_BIN")
}

/// The PATH walk behind the resolvers, memoised per `elf_only` on the app. The lock is held across
/// the lookup so concurrent polls share one walk instead of stacking several; a failure is not
/// kept, so the next poll looks again and finds a binary installed in the meantime.
async fn memoised_claude_bin(app: &App, elf_only: bool, lookup: impl Future<Output = Result<PathBuf>>) -> Result<PathBuf> {
    let mut memo = app.claude_bins.lock().await;
    if let Some(found) = memo.get(&elf_only) {
        return Ok(found.clone());
    }
    match lookup.await {
        Ok(bin) => {
            memo.insert(elf_only, bin.clone());
            Ok(bin)
        }
        Err(e) => Err(e),
    }
}

/// Walks the usual install locations and returns the first one that answers `--version`. `elf_only`
/// is the guest's requirement; for the host, being able to run it at all is the test.
async fn find_claude_bin(cfg: &Settings, elf_only: bool) -> Result<PathBuf> {
    walk_claude_candidates(&claude_bin_candidates(cfg), elf_only).await
}

/// Every place a Claude Code binary is looked for, most specific first: the configured override,
/// then PATH, then the usual home installs.
fn claude_bin_candidates(cfg: &Settings) -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = &cfg.claude_bin {
        candidates.push(p.into());
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join("claude")));
    }
    candidates.extend([
        home.join(".local/share/mise/installs/claude/latest/claude"),
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
    ]);
    candidates
}

/// Probes the candidates in order and returns the first that answers `--version`. Each probe is
/// bounded by `PROBE_LIMIT`, so only the overall walk limit keeps a PATH full of wedged candidates
/// from costing minutes — and the two outcomes read differently, because an operator who sees a
/// timeout looks for the wedged binary, not for a missing install.
async fn walk_claude_candidates(candidates: &[PathBuf], elf_only: bool) -> Result<PathBuf> {
    let walk = async {
        for candidate in candidates {
            let Ok(real) = std::fs::canonicalize(candidate) else {
                continue;
            };
            if elf_only && !is_elf(&real) {
                continue;
            }
            if let Ok(version) = exec_within(PROBE_LIMIT, Command::new(&real).arg("--version")).await
                && version.contains("Claude Code")
            {
                return Ok(real);
            }
        }
        Err(anyhow!("no Claude Code binary found"))
    };
    match tokio::time::timeout(CLAUDE_WALK_LIMIT, walk).await {
        Ok(found) => found,
        Err(_) => bail!("the search for a Claude Code binary timed out after {CLAUDE_WALK_LIMIT:?}"),
    }
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AppError(StatusCode, anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": format!("{:#}", self.1)}))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(StatusCode::INTERNAL_SERVER_ERROR, e.into())
    }
}

pub fn client_error(status: StatusCode, message: &str) -> AppError {
    AppError(status, anyhow!(message.to_string()))
}

/// The error every settings writer answers with when the file it would overwrite cannot be read:
/// refusing the save is the whole fix for a damaged `orgs.json`/`providers.json` (#408) — the old
/// silent-default read meant the next save replaced the operator's file with those defaults.
pub(crate) fn config_unreadable(path: &FsPath, err: &anyhow::Error) -> AppError {
    client_error(
        StatusCode::CONFLICT,
        &format!(
            "{} could not be read ({err:#}); fix or remove it — refusing to overwrite it",
            path.display()
        ),
    )
}

impl AppError {
    /// The HTTP status the response would carry.
    pub fn status(&self) -> StatusCode {
        self.0
    }

    /// The message a handler returns, as the client sees it in `{"error": …}`.
    pub fn message(&self) -> String {
        format!("{:#}", self.1)
    }
}

pub type ApiResult<T> = Result<Json<T>, AppError>;

/// The `storage` key of `/api/status`: `ok` while every write was confirmed, else the sticky alert
/// (`ts` and `recovered_at` in the same RFC 3339 form the `harness_log` frames use). A recovered
/// alert is `ok` again but keeps its message, time and count: the gap it reports still happened.
/// `ok` says whether writes are going through, so load damage is `ok` with a null `recovered_at`:
/// its `kind` is what keeps it on screen, since the colonies it reports never come back.
/// Every payload also carries the last queue-tick free-space verdict: `free_bytes` (null before the
/// first reading or when `df` fails), `warn_free_bytes` and `min_free_bytes` (0 = off), `low_disk`
/// (below the higher of the two), and `admission_paused` (below the floor, so the queue holds).
fn storage_status(alert: Option<StorageAlert>, verdict: &reclaim::FreeSpaceVerdict) -> Value {
    let mut value = match alert {
        None => json!({"ok": true}),
        Some(alert) => json!({
            "ok": alert.kind == StorageAlertKind::LoadDamage || alert.recovered_at.is_some(),
            "kind": alert.kind,
            "message": alert.message,
            "ts": alert.ts,
            "failures": alert.failures,
            "recovered_at": alert.recovered_at,
        }),
    };
    value["free_bytes"] = json!(verdict.free_bytes);
    value["warn_free_bytes"] = json!(verdict.warn_free_bytes);
    value["min_free_bytes"] = json!(verdict.min_free_bytes);
    value["low_disk"] = json!(verdict.low_disk);
    value["admission_paused"] = json!(verdict.admission_paused);
    value
}

/// An overall bound on the mesh branch of `/api/status`, not just its individual subprocesses:
/// `Mesh::status` waits on `Mesh`'s internal `running` lock, which `ensure_started` can hold across
/// calls that are themselves unbounded, so without this a wedged `headscale` during a boot parks
/// the status poll forever behind a mutex.
const MESH_STATUS_LIMIT: Duration = Duration::from_secs(15);

/// The `mesh` key of `/api/status`. `live` is awaited only when the mesh is enabled and its
/// binaries are vendored: the running mesh's own status, or the reason none could be built.
async fn mesh_status(modules: &ModulesConfig, assets: Option<&FsPath>, live: impl Future<Output = Result<Value>>) -> Value {
    if !modules.mesh_enabled() {
        json!({"enabled": false, "provider": "none"})
    } else if !assets.is_some_and(mesh::binaries_present) {
        // Not an error the operator can clear: this platform has no mesh binaries to
        // vendor. It goes in `detail`, not `error`: anything in `error` is read as a
        // fault, and this one used to paint every Mac's runtime red.
        json!({"enabled": true, "provider": "headscale", "state": "unavailable",
               "detail": "colonies use a loopback port on this platform", "error": Value::Null})
    } else {
        let live = tokio::time::timeout(MESH_STATUS_LIMIT, live)
            .await
            .unwrap_or_else(|_| Err(anyhow!("mesh status timed out after {MESH_STATUS_LIMIT:?}")));
        match live {
            Ok(status) => status,
            Err(e) => json!({"enabled": true, "provider": "headscale", "state": "error", "error": format!("{e:#}")}),
        }
    }
}

/// The query parameters of `GET /api/status`. `fresh=1` skips the runtime cache and re-probes, so
/// the UI's "Check again" button gets a real answer instead of a cached one; any value but `0` or
/// `false` counts, so a bare `?fresh` works too.
#[derive(Deserialize)]
pub struct StatusQuery {
    fresh: Option<String>,
}

/// The `GET /api/status` body for callers without the API token: an allowlist — version, counts,
/// capacity figures and health only — built from scratch, never the full body with fields removed.
/// Fleet peers poll this endpoint without a token, so `fleet::summary_from_status_json` reads
/// exactly these keys and defaults the rest. NEVER add identity here: no repo or issue names, no
/// org names, no paths or URLs, no hostnames or host ids, no GitHub or Claude account details.
async fn reduced_status(app: &Shared) -> Value {
    let modules = app.modules.read().await.clone();
    let sessions = app.sessions.read().await;
    let queue_depth = sessions
        .iter()
        .filter(|s| s.status == sessions::SessionStatus::Queued)
        .count();
    // The same "holds a slot" definition the full body counts.
    let microvms_live = sessions.iter().filter(|s| s.holds_slot()).count();
    drop(sessions);
    let microvms_ceiling = orgs::global_max_parallel(&modules);
    // Cached probes only: strangers must not force the probe subprocesses to rerun.
    let (runtime, host) = tokio::join!(runtime::status_runtime(app, false), runtime::status_host(app, false),);
    let mut host_value = json!({
        "microvms_live": microvms_live,
        "microvms_ceiling": microvms_ceiling,
    });
    // Numeric capacity figures only: the id, the hostname and the probe timestamp stay in the full
    // body, and a measurement that failed is omitted rather than faked as zero.
    for (key, value) in [
        ("cpu_cores", host.cpu_cores.map(|n| json!(n))),
        ("memory_total_bytes", host.memory_total_bytes.map(|n| json!(n))),
        ("memory_used_bytes", host.memory_used_bytes.map(|n| json!(n))),
        ("load", host.load.map(|load| json!(load))),
        ("disk_total_bytes", host.disk_total_bytes.map(|n| json!(n))),
        ("disk_used_bytes", host.disk_used_bytes.map(|n| json!(n))),
        ("disk_free_bytes", host.disk_free_bytes.map(|n| json!(n))),
    ] {
        if let Some(value) = value {
            host_value[key] = value;
        }
    }
    // The storage message names files, so only its verdict crosses over.
    let storage_ok = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())
        .get("ok")
        .cloned()
        .unwrap_or(json!(true));
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "queue_depth": queue_depth,
        "host": host_value,
        "runtime": {
            "platform": runtime.platform,
            "os": {
                "vendor": runtime.os.vendor,
                "name": runtime.os.name,
                "version": runtime.os.version,
            },
        },
        "storage": {"ok": storage_ok},
    })
}

async fn status(
    State(app): State<Shared>,
    Extension(authenticated): Extension<auth::Authenticated>,
    Query(query): Query<StatusQuery>,
) -> Json<Value> {
    // No token: the allowlist body for fleet peers and other strangers, built fresh below — never
    // the full body with fields removed.
    if !authenticated.0 {
        return Json(reduced_status(&app).await);
    }
    let mut msb = Command::new(&app.cfg.msb);
    msb.arg("--version");
    let cred = app.claude_cred();
    let fresh = query.fresh.as_deref().is_some_and(|v| !matches!(v, "0" | "false"));
    let (user, msb_version, claude_bin, claude, runtime, host) = tokio::join!(
        github::viewer(&app),
        exec_within(PROBE_LIMIT, &mut msb),
        resolve_guest_claude_bin(&app),
        claude_login::claude_status(&app, cred.as_ref()),
        runtime::status_runtime(&app, fresh),
        runtime::status_host(&app, fresh),
    );
    let modules = app.modules.read().await.clone();
    // The same "holds a slot" definition `queue::has_room` counts against the parallel limit — a live colony
    // or a live-origin publish keeps its microVM claimed, while a publish from a stopped colony holds
    // nothing. Kept in step with it; `Session::holds_slot` is the shared predicate both sides express.
    let microvms_live = app.sessions.read().await.iter().filter(|s| s.holds_slot()).count();
    let microvms_ceiling = orgs::global_max_parallel(&modules);
    // Queued colonies hold no microVM (see `Session::holds_slot`), so this is disjoint from
    // `microvms_live`. Carried in `/api/status` so a fleet peer-poll of this endpoint (issue #231)
    // gets everything `fleet::HostSummary` needs without a second round trip.
    let queue_depth = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status == sessions::SessionStatus::Queued)
        .count();
    // Cheap, no filesystem I/O: both predicates read in-memory session fields only.
    let (reclaimable, unpushed) = {
        let cfg = reclaim::ReclaimConfig::from_modules(&modules);
        let now = chrono::Utc::now();
        let sessions = app.sessions.read().await;
        (
            sessions
                .iter()
                .filter(|s| reclaim::reclaim_due(s, now, cfg.retention_secs))
                .count(),
            sessions.iter().filter(|s| reclaim::unpushed_work(s)).count(),
        )
    };
    // Computed before the `json!` literal below, which moves `runtime` into the payload: the host
    // object borrows its kvm answer, so the borrow must end before the move.
    let host_value = runtime::host_json(&host, runtime.kvm.as_ref(), microvms_live, microvms_ceiling);
    let live = async {
        match app.mesh().await {
            Ok(mesh) => Ok(mesh.status().await),
            Err(e) => Err(e),
        }
    };
    let mesh = mesh_status(&modules, app.cfg.assets.as_deref(), live).await;
    let sandbox_schema = modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let asset = |rel: &str| app.cfg.assets.as_ref().is_some_and(|a| a.join(rel).exists());
    let storage_alert = app.shown_storage_alert().await;
    // The last queue-tick verdict, read with no I/O: the queue refreshes it every tick.
    let disk_verdict = app.disk_verdict.lock().await.clone();
    // One entry per configured model provider, so a provider the colony fan-out is degrading is visible
    // from the status poll without opening the providers screen. Named `model_providers` because the
    // `modules` section's `sandbox`/`source`/`mesh` entries are this status's other "providers".
    let model_providers: Vec<Value> = app
        .providers()
        .iter()
        .map(|p| {
            let usage = app.gateway.usage(&p.id);
            let health = gateway::health(&usage);
            json!({
                "id": p.id,
                "name": p.name,
                "requests": usage.requests,
                "failure_pct": health.failure_pct,
                "avg_latency_ms": health.avg_latency_ms,
                "degraded": health.degraded,
            })
        })
        .collect();
    // Whether every routable provider's plan is out, and the earliest reset: the queue holder's
    // own words, so the overview banner and the queue gate never disagree.
    let quota = providers::quota_status(&app).await;
    // The host-wide stall (§diagnosis): live colonies, a waiting queue, and no colony producing
    // an event for ten minutes. Cheap — runtime stamps, else file mtimes, never file contents.
    let stall = diagnosis::status_stall(&app).await;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "queue_depth": queue_depth,
        "stall": stall,
        "reclaim": {"reclaimable": reclaimable, "unpushed": unpushed},
        "github": match user {
            Ok(u) => json!({"connected": true, "login": u["login"], "name": u["name"], "avatar_url": u["avatar_url"], "source": github::token_source(&app)}),
            Err(e) => json!({"connected": false, "error": format!("{e:#}")}),
        },
        "claude": claude,
        "sandbox": {
            "provider": modules.sandbox.provider,
            "image": config::setting_str(&modules.sandbox, &sandbox_schema, "image"),
            "cpus": setting_u64(&modules.sandbox, &sandbox_schema, "cpus"),
            "memory": config::setting_str(&modules.sandbox, &sandbox_schema, "memory"),
            "max_parallel": setting_u64(&modules.sandbox, &sandbox_schema, "max_parallel"),
            "msb_version": msb_version.ok().map(|v| v.trim().to_string()),
            "claude_bin": claude_bin.as_ref().ok().map(|p| p.display().to_string()),
            "claude_bin_error": claude_bin.err().map(|e| format!("{e:#}")),
        },
        "mesh": mesh,
        "storage": storage_status(storage_alert, &disk_verdict),
        "runtime": runtime,
        "host": host_value,
        "model_providers": model_providers,
        "quota": json!({
            "paused": quota.paused,
            "reason": quota.reason,
            "reset_at": quota.reset_at,
            "reset_unix": quota.reset_unix,
            "providers": quota.providers,
            "kind": quota.kind,
        }),
        "modules": {
            "source": modules.source.provider,
            "sandbox": modules.sandbox.provider,
            "mesh": if modules.mesh.enabled { modules.mesh.provider.as_str() } else { "none" },
            "agent": modules.agent.provider,
            "publish": modules.publish.provider,
            // Off, or never configured, reads as "none" like the mesh's loopback provider does.
            "notify": modules
                .notify
                .as_ref()
                .filter(|c| c.enabled)
                .map(|c| c.provider.as_str())
                .unwrap_or("none"),
        },
        "assets": {
            "path": app.cfg.assets.as_ref().map(|p| p.display().to_string()),
            "agentd": asset("bin/colonizer-agentd"),
            "headscale": asset("vendor/headscale"),
            "tailscale": asset("vendor/tailscale/tailscaled"),
            "web": asset("web/index.html"),
            "agents": app.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
        },
    }))
}

async fn set_claude_token(State(app): State<Shared>, Json(body): Json<Value>) -> ApiResult<Value> {
    let token = body["token"].as_str().unwrap_or_default().trim();
    if !token.starts_with("sk-ant-") || token.contains(char::is_whitespace) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "expected a token from `claude setup-token` (sk-ant-oat…) or an API key (sk-ant-api…)",
        ));
    }
    util::write_secret(&app.claude_token_file(), token)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_claude_token(State(app): State<Shared>) -> ApiResult<Value> {
    delete_secret(&app.claude_token_file());
    Ok(Json(json!({"ok": true})))
}

/// Rejects DNS rebinding (unexpected Host), then requires the per-install API token (issue #405):
/// `Authorization: Bearer` or the `colonizer_token` cookie. Bearer requests skip the `Origin`
/// check (no CORS preflight is ever granted); cookie writes and upgrades keep the same-origin
/// requirement. Unauthenticated `GET /api/status` answers the reduced body; other `/api` requests
/// get a 401; page loads get the sign-in page, or set the cookie from the link's `?token=`.
async fn host_guard(State(app): State<Shared>, mut req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let hostname = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default().to_string()
    };
    let bind_host = app.cfg.bind.rsplit_once(':').map_or(app.cfg.bind.as_str(), |(h, _)| h);
    let allowed = matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || hostname == bind_host
        || app.cfg.allowed_hosts.contains(&hostname);
    if !allowed {
        return (StatusCode::FORBIDDEN, "Host not allowed (set COLONIZER_ALLOWED_HOSTS)").into_response();
    }
    // Writes and upgrades need a same-origin `Origin`, or a request no browser could have made:
    // a missing `Origin` is rejected rather than trusted (see #375), and only an authenticated
    // request passes at all.
    let upgrade = req.headers().contains_key(header::UPGRADE);
    let bearer_ok = auth::bearer_token(req.headers()).is_some_and(|token| auth::tokens_match(&token, &app.api_token));
    let cookie_ok = auth::cookie_token(req.headers()).is_some_and(|token| auth::tokens_match(&token, &app.api_token));
    if bearer_ok || cookie_ok {
        // Cookie-authenticated writes and upgrades keep the same-origin requirement; header
        // authentication already proves a non-browser caller.
        if !bearer_ok && (req.method() != Method::GET || upgrade) {
            let same_origin = req
                .headers()
                .get(header::ORIGIN)
                .and_then(|o| o.to_str().ok())
                .is_some_and(|origin| origin.split("://").nth(1) == Some(host.as_str()));
            if !same_origin {
                return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
            }
        }
        req.extensions_mut().insert(auth::Authenticated(true));
        return next.run(req).await;
    }
    // No valid token: the reduced status, the sign-in link's cookie, or how to sign in.
    let path = req.uri().path().to_string();
    if path == "/api" || path.starts_with("/api/") {
        if req.method() == Method::GET && path == "/api/status" {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        return (StatusCode::UNAUTHORIZED, auth::UNAUTHORIZED_BODY).into_response();
    }
    if req.method() == Method::GET
        && let Some(token) = auth::query_token(req.uri().query())
        && auth::tokens_match(&token, &app.api_token)
    {
        // The token must not linger in caches or leak via the Referer header on the next click.
        let mut res = Html(auth::login_page()).into_response();
        let headers = res.headers_mut();
        if let Ok(cookie) = auth::set_cookie_header(&app.api_token).parse() {
            headers.insert(header::SET_COOKIE, cookie);
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        return res;
    }
    let mut res = (StatusCode::UNAUTHORIZED, Html(auth::locked_page())).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

fn web_router(assets: Option<&FsPath>) -> Router<Shared> {
    match assets.map(|a| a.join("web")).filter(|dir| dir.join("index.html").exists()) {
        // Hashed build assets 404 when missing instead of falling back to the page: a tab left open
        // across an update asks for chunks the new build no longer has, and HTML served as a
        // module script fails with a MIME error the page cannot tell apart from a real bug.
        Some(dir) => Router::new()
            .nest_service("/assets", ServeDir::new(dir.join("assets")))
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => Router::new().fallback(|| async { Html(UI_MISSING_HTML) }),
    }
}

/// Load the session list from `sessions.json`. A file we cannot read or parse as a list at all is
/// moved aside to `sessions.json.corrupt-<unix-timestamp>` — never overwritten, so its bytes stay
/// recoverable — and the harness starts with an empty list and a sticky alert: the colonies on that
/// list are missing from it although their worktrees, branches and microVMs may still exist. A list
/// where records are damaged — some or all of them — is salvaged instead: the good colonies load,
/// and the original is copied aside untouched, so the next startup can salvage from it again if the
/// harness stops before the next save. If even the aside fails, the next save would overwrite the
/// file, so that is an error rather than a degraded start.
fn load_sessions(path: &FsPath) -> Result<(Vec<Session>, Option<StorageAlert>)> {
    let data = match std::fs::read(path) {
        // A missing file is a first run, not a corruption.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => {
            let saved = move_corrupt_aside(path)?;
            return Ok(unusable(path, &saved, format!("could not be read ({e})")));
        }
        Ok(data) => data,
    };
    let values = match serde_json::from_slice::<Vec<Value>>(&data) {
        Ok(values) => values,
        Err(e) => {
            let saved = move_corrupt_aside(path)?;
            return Ok(unusable(path, &saved, format!("could not be parsed ({e})")));
        }
    };
    let (sessions, damaged) = salvage(values);
    if damaged == 0 {
        // Unchanged happy path: nothing damaged, so the file is left exactly as it is.
        return Ok((sessions, None));
    }
    let saved = copy_corrupt_aside(path)?;
    let message = format!(
        "{} kept {} of {} records and copied the original to {}, but {} of them could not be loaded; those colonies are missing from the list, although their worktrees, branches and microVMs may still exist",
        path.display(),
        sessions.len(),
        sessions.len() + damaged,
        saved.display(),
        damaged
    );
    eprintln!("sessions: {message}");
    Ok((
        sessions,
        Some(StorageAlert {
            kind: StorageAlertKind::LoadDamage,
            message,
            ts: Utc::now(),
            failures: 1,
            recovered_at: None,
        }),
    ))
}

/// What a file with nothing usable in it turns into: an empty list and a sticky alert saying where
/// the bytes went.
fn unusable(path: &FsPath, saved: &FsPath, reason: String) -> (Vec<Session>, Option<StorageAlert>) {
    let message = format!(
        "{} {reason} and was saved as {}; colonies are missing from the list, although their worktrees, branches and microVMs may still exist",
        path.display(),
        saved.display()
    );
    eprintln!("sessions: {message}");
    (
        Vec::new(),
        Some(StorageAlert {
            kind: StorageAlertKind::LoadDamage,
            message,
            ts: Utc::now(),
            failures: 1,
            recovered_at: None,
        }),
    )
}

/// Splits a parsed list into the records that load and the ones that cannot. A record is damaged
/// when it fails to deserialize or its `id` is not a plain name: with every field defaulting, an
/// unrelated object would otherwise load as a blank colony, and since an `id` names the colony's
/// directory under `data/sessions` — which `lifecycle::delete` removes whole — a record like
/// `../../victim` must be refused before anything can act on it.
fn salvage(values: Vec<Value>) -> (Vec<Session>, usize) {
    let mut sessions = Vec::new();
    let mut damaged = 0;
    for value in values {
        let record = serde_json::from_value::<Session>(value).ok().filter(|s| is_plain_name(&s.id));
        match record {
            Some(s) => sessions.push(s),
            None => damaged += 1,
        }
    }
    (sessions, damaged)
}

/// The timestamped name both ruin paths save under: `sessions.json.corrupt-<unix-timestamp>`, in the
/// same directory. The stamp only ever moves forward, so an old copy is not silently replaced.
fn corrupt_aside_name(path: &FsPath) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sessions.json".into());
    path.with_file_name(format!("{name}.corrupt-{stamp}"))
}

/// Moves a file the harness cannot use aside (`sessions.json`, `modules.json`), into the same
/// directory, so its bytes survive.
pub(crate) fn move_corrupt_aside(path: &FsPath) -> Result<PathBuf> {
    let saved = corrupt_aside_name(path);
    let aside = || {
        format!(
            "could not move the unusable {} aside to {}; move it aside yourself and restart",
            path.display(),
            saved.display()
        )
    };
    util::faults::check(path, util::faults::Op::Rename).with_context(aside)?;
    std::fs::rename(path, &saved).with_context(aside)?;
    Ok(saved)
}

/// Copies a partly usable `sessions.json` aside, leaving the original in place: the good records are
/// in memory and the next save rewrites the file, but if the harness stops before that, the next
/// startup salvages from the original again, and the copy keeps the bytes as they were found. A copy
/// is a write of the aside file, so it goes through the fault seam as a `Write`, like the temp write
/// in `write_atomic` — and if even the copy fails, startup aborts rather than go on with the damaged
/// bytes unpreserved: the next save would drop them.
fn copy_corrupt_aside(path: &FsPath) -> Result<PathBuf> {
    let saved = corrupt_aside_name(path);
    let aside = || {
        format!(
            "could not copy the partly unusable {} aside to {}; move it aside yourself and restart",
            path.display(),
            saved.display()
        )
    };
    util::faults::check(path, util::faults::Op::Write).with_context(aside)?;
    std::fs::copy(path, &saved).with_context(aside)?;
    Ok(saved)
}

const USAGE: &str = "colonizer — turn a task into a pull request; see https://colonizer.dev/docs

usage: colonizer
       colonizer version | update | open
       colonizer telemetry show|on|off

  (no arguments)  start the mothership and serve the web UI (default 127.0.0.1:7878)
  version         print what this build is, and whether it is a release (also --version, -V)
  update [--force]  install the newest release against a running mothership and restart into it (refuses a development build, or one newer than the latest release, unless --force)
  open            print the cockpit sign-in link and open it in a browser
  telemetry show  print the exact anonymous usage batch that would be sent
  telemetry on    record yes to anonymous usage reporting (no network, no daemon needed)
  telemetry off   record no to anonymous usage reporting
  --help, -h      print this help

Settings come from the environment, not flags: COLONIZER_BIND, COLONIZER_DATA_DIR,
COLONIZER_HOME and the rest are in docs/install.md.";

/// What the binary was asked to do. Starting the mothership is the default; every other command
/// runs without one, except `update`, which is a client of a mothership that is already running.
///
/// An argument nobody planned for is an error with the usage text, not a silently started server:
/// a typo like `colonizer updat` should say so rather than take over the port for an afternoon.
enum Args {
    Serve,
    Version,
    Update { force: bool },
    Open,
    TelemetryShow,
    TelemetrySet(bool),
}

impl Args {
    /// `Ok(None)` means the command was fully handled (`--help`).
    fn parse(argv: Vec<String>) -> Result<Option<Self>, String> {
        let mut iter = argv.into_iter();
        let Some(arg) = iter.next() else {
            return Ok(Some(Self::Serve));
        };
        let command = match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "version" | "--version" | "-V" => Self::Version,
            "update" => match iter.next().as_deref() {
                None => Self::Update { force: false },
                Some("--force") => Self::Update { force: true },
                Some(other) => return Err(format!("unknown argument: {other}")),
            },
            "open" => Self::Open,
            "telemetry" => {
                let sub = iter
                    .next()
                    .ok_or_else(|| "telemetry needs a command: show, on or off".to_string())?;
                match sub.as_str() {
                    "show" => Self::TelemetryShow,
                    "on" => Self::TelemetrySet(true),
                    "off" => Self::TelemetrySet(false),
                    other => return Err(format!("unknown telemetry command: {other}")),
                }
            }
            _ => return Err(format!("unknown argument: {arg}")),
        };
        if let Some(extra) = iter.next() {
            return Err(format!("unknown argument: {extra}"));
        }
        Ok(Some(command))
    }

    async fn run(self) -> Result<()> {
        match self {
            Self::Serve => serve().await,
            // The stamped build, not CARGO_PKG_VERSION: the crate version says nothing about
            // which commit an install came from.
            Self::Version => {
                println!("{}", version::build().line());
                Ok(())
            }
            Self::Update { force } => update::command(force).await,
            // Reprints the sign-in link (startup prints it too) and opens it the same way.
            Self::Open => {
                let cfg = Settings::from_env()?;
                let token = auth::load_or_create(&cfg.config_dir)?;
                let url = auth::login_url(&cfg.bind, &token);
                println!("{url}");
                auth::open_browser(&url);
                Ok(())
            }
            Self::TelemetryShow => {
                let cfg = Settings::from_env()?;
                usage::cli_show(&cfg.config_dir)
            }
            Self::TelemetrySet(enabled) => {
                let cfg = Settings::from_env()?;
                usage::cli_set(&cfg.config_dir, enabled)
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1).collect()) {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("colonizer: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match args.run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("colonizer: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The mothership itself: load state from the data dir, serve the API and the web UI, and run the
/// background loops.
async fn serve() -> Result<()> {
    let cfg = Settings::from_env()?;
    for dir in ["sessions", "repos", "worktrees", "memory", "plugins"] {
        std::fs::create_dir_all(cfg.data_dir.join(dir))?;
    }
    let (mut sessions, corrupt) = load_sessions(&cfg.data_dir.join("sessions.json"))?;
    for s in &mut sessions {
        if s.org.is_empty() {
            s.org = s.repo.split('/').next().unwrap_or_default().to_string();
        }
    }
    // Colonies persisted as finished while still carrying an attention flag predate the clearing
    // every terminal transition now does; drop those stale flags before `recover` runs, so a
    // stopped colony does not look like it still needs attention.
    let stale_attention = sessions::clear_stale_attention(&mut sessions);
    if stale_attention > 0 {
        println!("sessions: cleared a stale attention flag from {stale_attention} finished colonies");
    }
    let (modules, modules_damage) = ModulesConfig::load(&cfg.config_dir.join("modules.json"))?;
    // Both startup ruin reports show as one alert when both happen: the operator dismisses one
    // banner, not two about the same bad disk.
    let load_damage = match (corrupt, modules_damage) {
        (Some(sessions), Some(modules)) => Some(StorageAlert {
            message: format!("{}\n{}", sessions.message, modules.message),
            ..sessions
        }),
        (sessions, modules) => sessions.or(modules),
    };
    let (agents, agent_problems) = modules::discover_agents(cfg.assets.as_deref());

    // The cockpit API token, minted on first run: every request to the API proves itself with it.
    let api_token = auth::load_or_create(&cfg.config_dir)?;

    // Saved secrets: the system keychain where it answers, the 0600 files otherwise (secrets.rs).
    // The probe can wait on a locked keyring, so it runs off the startup path.
    secrets::install(secrets::Store::new(&cfg.config_dir, secrets::os_backend()));
    std::thread::spawn(|| {
        if let Some(store) = secrets::global() {
            store.probe();
        }
    });

    let app = Arc::new(App {
        modules: RwLock::new(modules),
        agents,
        agent_problems,
        sessions: RwLock::new(sessions),
        redteam: redteam::RedTeamStore::new(&cfg.data_dir, &cfg.config_dir),
        session_persist: Mutex::new(()),
        config_write: Mutex::new(()),
        config_damage: std::sync::Mutex::new(None),
        storage_alert: RwLock::new(None),
        disk_verdict: Mutex::new(Default::default()),
        load_damage,
        runtimes: Mutex::new(HashMap::new()),
        repo_locks: Mutex::new(HashMap::new()),
        session_locks: Mutex::new(HashMap::new()),
        mesh: Mutex::new(None),
        login: Default::default(),
        memory: memory::MemoryStore::new(cfg.data_dir.join("memory")),
        gateway: gateway::Gateway::new(&cfg.data_dir)?,
        repo_owners: RwLock::new(BTreeSet::new()),
        answer_cache: AnswerCache::default(),
        new_orgs: RwLock::new(BTreeMap::new()),
        org_descriptions: RwLock::new(BTreeMap::new()),
        orgs_refreshed: Mutex::new(None),
        orgs_failed_at: Mutex::new(None),
        claude_account: Mutex::new(None),
        github_viewer: Mutex::new(None),
        claude_bins: Mutex::new(HashMap::new()),
        runtime_cache: Mutex::new(None),
        host_cache: Mutex::new(None),
        provider_probe_cache: Mutex::new(HashMap::new()),
        fleet_cache: fleet::FleetCache::new(),
        pull: Mutex::new(Default::default()),
        headroom: Mutex::new(Default::default()),
        telemetry: telemetry::Telemetry::new(&cfg.config_dir)?,
        updates: version::Updates::new(&cfg.config_dir)?,
        updater: update::Updater::new(),
        usage: usage::Usage::new(&cfg.config_dir),
        stream: stream::Hub::new(),
        api_token,
        cfg,
    });

    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/hosts", get(fleet::list_hosts_handler))
        .route("/api/modules", get(modules::list))
        .route("/api/modules/{kind}", put(modules::update))
        .route("/api/secrets", get(secrets::list))
        .route("/api/secrets/health", get(secrets::health))
        .route("/api/secrets/colony", post(colony_secrets::upsert))
        .route("/api/secrets/{id}", put(secrets::put).delete(secrets::delete))
        .route("/api/secrets/{id}/move", post(secrets::move_secret))
        .route(
            "/api/settings/github-token",
            post(github::set_token).delete(github::delete_token),
        )
        .route(
            "/api/settings/claude-token",
            post(set_claude_token).delete(delete_claude_token),
        )
        .route("/api/claude-login", get(claude_login::status))
        .route("/api/claude-login/start", post(claude_login::start))
        .route("/api/claude-login/code", post(claude_login::submit_code))
        .route("/api/claude-login/cancel", post(claude_login::cancel))
        .route(
            "/api/claude-accounts",
            get(claude_accounts::list).post(claude_accounts::create),
        )
        .route("/api/claude-accounts/{id}", delete(claude_accounts::delete))
        .route("/api/sandbox/pull", post(sandbox::pull_configured).get(sandbox::pull_status))
        .route("/api/headroom", get(headroom::status))
        .route("/api/headroom/download", post(headroom::download))
        .route("/api/hunters/{id}/install", post(hunters::install_handler))
        .route("/api/hunters/{id}/probe", get(hunters::probe_handler))
        .route("/api/telemetry", get(telemetry::status).put(telemetry::put))
        .route("/api/version", get(version::version))
        .route("/api/update", get(version::status).put(version::put))
        .route("/api/update/apply", post(update::apply))
        .route("/api/telemetry/usage", get(usage::status).put(usage::put))
        .route("/api/plugins", get(plugins::list))
        .route("/api/providers", get(providers::list))
        .route("/api/providers/{id}", put(providers::put).delete(providers::delete))
        .route("/api/providers/{id}/health", get(gateway::provider_health))
        .route("/api/models", get(providers::models))
        .route("/api/orgs", get(orgs::list))
        .route("/api/orgs/{org}", put(orgs::put))
        .route("/api/spend/history", get(spend::history))
        .route("/api/memory", get(memory::get))
        .route("/api/memory/proposals", get(memory::list_proposals))
        .route("/api/memory/proposals/{id}/approve", post(memory::approve))
        .route("/api/memory/proposals/{id}/reject", post(memory::reject))
        .route("/api/memory/notes", post(memory::create_note))
        .route("/api/memory/notes/{id}", delete(memory::delete_note))
        .route("/api/memory/mem0", get(memory::mem0_status).put(memory::put_mem0_key))
        .route("/api/memory/mem0/check", post(memory::check_mem0))
        .route("/api/notify/secret", get(notify::secret_status).put(notify::put_secret))
        .route("/api/voice", get(voice::status))
        .route("/api/voice/key", put(voice::put_key))
        // A clip is larger than axum's 2 MB default body limit; the handler checks the cap itself too.
        .route(
            "/api/voice/transcribe",
            post(voice::transcribe).layer(DefaultBodyLimit::max(voice::MAX_BYTES + 1)),
        )
        .route("/api/repos", get(github::list_repos))
        .route("/api/repos/{owner}/{name}/issues", get(github::list_issues))
        .route("/api/repos/{owner}/{name}/packages", get(packages::list_packages))
        .route("/api/sessions", get(sessions::list).post(sessions::create))
        .route("/api/sessions/{id}", get(sessions::get).delete(lifecycle::delete))
        .route("/api/sessions/{id}/resume", post(lifecycle::resume))
        .route("/api/sessions/{id}/publish", post(publish::publish))
        .route("/api/sessions/{id}/behind", get(stale::behind))
        .route("/api/sessions/{id}/catch-up", post(stale::catch_up))
        .route("/api/sessions/{id}/stop", post(lifecycle::stop))
        .route("/api/sessions/{id}/cleanup", post(lifecycle::cleanup))
        .route("/api/storage", get(reclaim::storage))
        .route("/api/stream", get(stream::handler))
        .route("/api/sessions/{id}/retain", post(reclaim::retain))
        .route("/api/sessions/{id}/events", get(sessions::events_ws))
        .route("/api/sessions/{id}/terminal", get(sessions::terminal_ws))
        .route("/api/sessions/{id}/findings", get(findings::list))
        .route("/api/findings", get(findings::list_all))
        .route("/api/redteam/runs", get(redteam::list).post(redteam::create))
        .route("/api/redteam/runs/{id}", get(redteam::get))
        .route("/api/redteam/runs/{id}/stop", post(redteam::stop))
        .route(
            "/api/redteam/schedules",
            get(redteam::list_schedules).post(redteam::create_schedule),
        )
        .route(
            "/api/redteam/schedules/{id}",
            put(redteam::update_schedule).delete(redteam::delete_schedule),
        )
        .route("/api/burn-down", get(burn_down::status))
        .route("/api/burn-down/stop", post(burn_down::stop));
    let router = api
        .merge(web_router(app.cfg.assets.as_deref()))
        .layer(middleware::from_fn_with_state(app.clone(), host_guard))
        .with_state(app.clone());

    let listener = tokio::net::TcpListener::bind(&app.cfg.bind)
        .await
        .with_context(|| format!("cannot bind {}", app.cfg.bind))?;
    println!("colonizer listening on http://{}", app.cfg.bind);
    println!("data: {}", app.cfg.data_dir.display());
    match &app.cfg.assets {
        Some(assets) => println!("assets: {}", assets.display()),
        None => println!("assets: not found (run scripts/install.sh)"),
    }
    // The sign-in link: printed always, opened when there is a browser to open in.
    let login_url = auth::login_url(&app.cfg.bind, &app.api_token);
    println!("cockpit: {login_url}");
    if !auth::bind_is_loopback(&app.cfg.bind) {
        eprintln!(
            "warning: colonizer is bound to {}, so the API answers to the network; requests need the API token, but plain HTTP exposes the token to anyone on the path — use a TLS reverse proxy or an SSH tunnel",
            app.cfg.bind
        );
    }
    auth::open_browser(&login_url);
    // The first-run notice: once, while nobody has answered yet, show the exact usage batch on stderr.
    usage::first_run_notice(&app).await;
    match tokio::net::TcpListener::bind(app.cfg.gateway_bind).await {
        Ok(listener) => {
            println!("provider gateway on http://{}", app.cfg.gateway_bind);
            let gateway = gateway::router(app.clone());
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, gateway).await {
                    eprintln!("provider gateway stopped: {e}");
                }
            });
        }
        Err(e) => eprintln!(
            "provider gateway: cannot bind {}: {e}; colonies can't use model providers",
            app.cfg.gateway_bind
        ),
    }

    let recovery = app.clone();
    tokio::spawn(async move {
        lifecycle::recover(&recovery).await;
        // Once recovery has settled, an app directory kept by an earlier update
        // can go, unless a colony that survived it still mounts from there.
        let live: Vec<std::path::PathBuf> = recovery
            .sessions
            .read()
            .await
            .iter()
            .filter(|s| update::will_reconnect(s.status))
            .filter_map(|s| s.app_slot.as_deref().map(std::path::PathBuf::from))
            .collect();
        for gone in update::sweep_slots(recovery.cfg.assets.as_deref(), &live) {
            println!("removed the app directory left by an earlier update: {}", gone.display());
        }
    });
    let sandbox_watch = app.clone();
    tokio::spawn(async move { lifecycle::watch_sandboxes(sandbox_watch).await });
    let queue = app.clone();
    tokio::spawn(async move { queue::run_queue(queue).await });
    let redteam = app.clone();
    tokio::spawn(async move { redteam::run(redteam).await });
    let schedules = app.clone();
    tokio::spawn(async move { redteam::run_schedules(schedules).await });
    let disk_watch = app.clone();
    tokio::spawn(async move { lifecycle::watch_host_disks(disk_watch).await });
    tokio::spawn(reclaim::run(app.clone()));
    let pr_watch = app.clone();
    tokio::spawn(async move { publish::watch_pull_requests(pr_watch).await });
    // Merged colonies from before `merged_at` existed gain GitHub's time where it still reports
    // one; best effort, off the serving path.
    let merged_at_backfill = app.clone();
    tokio::spawn(async move { publish::backfill_merged_at(merged_at_backfill).await });
    // Colonies whose pull request predates `changed_paths` gain its file list, for the monorepo
    // package rows; best effort, off the serving path.
    tokio::spawn(publish::backfill_changed_paths(app.clone()));
    tokio::spawn(watchdog::run(app.clone()));
    tokio::spawn(autonomy::run(app.clone()));
    tokio::spawn(burn_down::run(app.clone()));
    tokio::spawn(notify::run(app.clone()));
    tokio::spawn(telemetry::run(app.clone()));
    tokio::spawn(version::run(app.clone()));
    tokio::spawn(gateway::flush_loop(app.clone()));
    let mesh_vendored = app.cfg.assets.as_deref().is_some_and(mesh::binaries_present);
    if app.modules.read().await.mesh_enabled() && !mesh_vendored && app.cfg.assets.is_some() {
        // Restarting would never help: the binaries are missing from this install, and the app does
        // not fetch them at runtime. Colonies use a loopback port instead.
        println!("mesh: no mesh binaries for this platform; colonies will use a loopback port");
    }
    if app.modules.read().await.mesh_enabled() && mesh_vendored {
        let mesh_app = app.clone();
        tokio::spawn(async move {
            let mut delay = Duration::from_secs(2);
            for attempt in 1..=8 {
                match async { mesh_app.mesh().await?.ensure_started().await }.await {
                    Ok(()) => {
                        if attempt > 1 {
                            println!("mesh: started on attempt {attempt}");
                        }
                        break;
                    }
                    Err(e) => {
                        eprintln!("mesh: attempt {attempt} failed: {e:#}");
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(60));
                    }
                }
            }
        });
    }

    tokio::select! {
        result = async { axum::serve(listener, router).await } => result?,
        // microVMs are detached and keep running; sessions reconnect on the next start.
        _ = shutdown_signal() => {
            println!("shutting down; running sessions keep their microVMs");
            app.telemetry.goodbye().await;
            let mesh = app.mesh.lock().await.clone();
            if let Some(mesh) = mesh {
                mesh.shutdown().await;
            }
            // The usage counters flush every few seconds; one last flush loses nothing.
            app.gateway.flush_usage();
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal App over a throwaway directory, shared with sessions.rs's tests. Nothing here
    /// binds a port, spawns a microVM or reaches the network.
    pub(crate) fn test_app(root: &FsPath) -> Shared {
        test_app_with(root, |_| {})
    }

    /// The same App, with a hook to adjust the settings before it is built: `Settings` lives
    /// inside an `Arc<App>`, so a test that needs its own `claude_bin` cannot patch one afterwards.
    pub(crate) fn test_app_with(root: &FsPath, settings: impl FnOnce(&mut Settings)) -> Shared {
        test_app_full(root, Vec::new(), settings)
    }

    /// The same App, with agent modules installed: `create` refuses to start a colony when no agent
    /// matches the configured provider, so a test that runs it needs one to find.
    pub(crate) fn test_app_with_agents(root: &FsPath, agents: Vec<AgentModule>, settings: impl FnOnce(&mut Settings)) -> Shared {
        test_app_full(root, agents, settings)
    }

    fn test_app_full(root: &FsPath, agents: Vec<AgentModule>, settings: impl FnOnce(&mut Settings)) -> Shared {
        std::fs::create_dir_all(root.join("data")).unwrap();
        let (modules, _) = ModulesConfig::load(&root.join("config/modules.json")).unwrap();
        let mut cfg = Settings {
            bind: "127.0.0.1:0".into(),
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            runtime_dir: root.join("run"),
            assets: None,
            msb: "msb".into(),
            claude_bin: None,
            gateway_bind: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: Vec::new(),
            fleet_peers: Vec::new(),
        };
        settings(&mut cfg);
        Arc::new(App {
            cfg,
            // A throwaway token: these tests never bind a port, and each one reads the token it
            // needs off the App itself.
            api_token: crate::util::random_token(),
            modules: RwLock::new(modules),
            agents,
            agent_problems: Vec::new(),
            sessions: RwLock::new(Vec::new()),
            redteam: redteam::RedTeamStore::new(&root.join("data"), &root.join("config")),
            session_persist: Mutex::new(()),
            config_write: Mutex::new(()),
            config_damage: std::sync::Mutex::new(None),
            storage_alert: RwLock::new(None),
            disk_verdict: Mutex::new(Default::default()),
            load_damage: None,
            runtimes: Mutex::new(HashMap::new()),
            repo_locks: Mutex::new(HashMap::new()),
            session_locks: Mutex::new(HashMap::new()),
            mesh: Mutex::new(None),
            login: Default::default(),
            memory: memory::MemoryStore::new(root.join("memory")),
            // Added on main while this branch was open; kept in step with the real constructor.
            claude_account: Mutex::new(None),
            github_viewer: Mutex::new(None),
            claude_bins: Mutex::new(HashMap::new()),
            runtime_cache: Mutex::new(None),
            host_cache: Mutex::new(None),
            provider_probe_cache: Mutex::new(HashMap::new()),
            fleet_cache: fleet::FleetCache::new(),
            usage: usage::Usage::new(&root.join("config")),
            updates: version::Updates::new(&root.join("config")).unwrap(),
            updater: update::Updater::new(),
            gateway: gateway::Gateway::new(&root.join("data")).unwrap(),
            repo_owners: RwLock::new(BTreeSet::new()),
            answer_cache: AnswerCache::default(),
            new_orgs: RwLock::new(BTreeMap::new()),
            org_descriptions: RwLock::new(BTreeMap::new()),
            orgs_refreshed: Mutex::new(None),
            orgs_failed_at: Mutex::new(None),
            pull: Mutex::new(Default::default()),
            headroom: Mutex::new(Default::default()),
            telemetry: telemetry::Telemetry::new(&root.join("config")).unwrap(),
            stream: stream::Hub::new(),
        })
    }

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-load-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }

    #[test]
    fn update_takes_an_optional_force_flag_and_nothing_else() {
        let parse = |args: &[&str]| Args::parse(args.iter().map(ToString::to_string).collect());
        assert!(matches!(parse(&["update"]), Ok(Some(Args::Update { force: false }))));
        assert!(matches!(
            parse(&["update", "--force"]),
            Ok(Some(Args::Update { force: true }))
        ));
        // An argument nobody planned for is an error, including trailing ones.
        for args in [
            &["update", "extra"][..],
            &["update", "--bogus"][..],
            &["update", "--force", "extra"][..],
        ] {
            assert!(parse(args).is_err(), "{args:?} should error");
        }
    }

    /// A session list with exactly the fields the format requires; everything else defaults.
    fn session_json() -> String {
        json!([record_json("abc123")]).to_string()
    }

    /// One colony record with exactly the fields the format requires, keyed by `id`.
    fn record_json(id: &str) -> Value {
        json!({
            "id": id,
            "repo": "acme/app",
            "issue_title": "Fix the deploy",
            "status": "idle",
            "branch": format!("colonizer/issue-1-{id}"),
            "worktree": format!("/colonizer/worktrees/acme/app/issue-1-{id}"),
            "sandbox": format!("colonizer-{id}"),
            "agent": "claude",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        })
    }

    /// The real `host_guard` and `status`, a dummy POST route and page: `Router<()>` so tests can
    /// drive it with `oneshot`.
    fn auth_router(app: &Shared) -> Router<()> {
        Router::new()
            .route("/api/status", get(status))
            .route("/api/sessions", post(|| async { "created" }))
            .route("/api/stream", get(stream::handler))
            .fallback(|| async { Html("test page") })
            .layer(middleware::from_fn_with_state(app.clone(), host_guard))
            .with_state(app.clone())
    }

    use axum::http::HeaderName;
    use tower::ServiceExt as _;

    async fn body_text(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// A request to the auth test router: loopback Host plus the given headers. The guard runs
    /// before routing, so reaching the dummy POST route proves the guard passed.
    fn guarded(method: Method, uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        let mut request = Request::builder().method(method).uri(uri);
        request = request.header(header::HOST, "127.0.0.1:7878");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request.body(axum::body::Body::from("")).unwrap()
    }

    fn bearer(app: &Shared) -> (HeaderName, String) {
        (header::AUTHORIZATION, format!("Bearer {}", app.api_token))
    }

    fn cookie(app: &Shared) -> (HeaderName, String) {
        (header::COOKIE, format!("{}={}", auth::COOKIE_NAME, app.api_token))
    }

    fn origin(value: &str) -> (HeaderName, String) {
        (header::ORIGIN, value.to_string())
    }

    /// The WebSocket handshake headers: their presence is what marks a request as an upgrade.
    fn upgrade() -> Vec<(HeaderName, String)> {
        [
            (header::UPGRADE, "websocket".to_string()),
            (header::CONNECTION, "Upgrade".to_string()),
            (header::SEC_WEBSOCKET_VERSION, "13".to_string()),
            (header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==".to_string()),
        ]
        .to_vec()
    }

    #[tokio::test]
    async fn unauthenticated_api_requests_are_rejected_even_with_a_same_origin_origin() {
        let root = temp_root();
        let app = test_app(&root);
        // A correct same-origin Origin used to be enough for scripts; now the token is required.
        for (method, uri) in [
            (Method::POST, "/api/sessions"),
            (Method::GET, "/api/sessions"),
            (Method::GET, "/api/version"),
        ] {
            let res = auth_router(&app)
                .oneshot(guarded(method.clone(), uri, vec![origin("http://127.0.0.1:7878")]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert!(body_text(res).await.contains("colonizer open"), "{method} {uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_bad_host_is_forbidden_even_with_a_valid_token() {
        let root = temp_root();
        let app = test_app(&root);
        let mut req = guarded(Method::POST, "/api/sessions", vec![bearer(&app)]);
        req.headers_mut()
            .insert(header::HOST, HeaderValue::from_static("evil.example"));
        let res = auth_router(&app).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn bearer_requests_pass_without_an_origin_and_fail_with_a_wrong_token() {
        let root = temp_root();
        let app = test_app(&root);
        // No Origin anywhere: the token decides, upgrade or not; without one, told to sign in.
        let token = format!("Bearer {}", app.api_token);
        for (auth, is_upgrade, expected) in [
            (Some(token.clone()), false, StatusCode::OK),
            (Some("Bearer wrong-token".to_string()), false, StatusCode::UNAUTHORIZED),
            (Some(token), true, StatusCode::OK),
            (None, false, StatusCode::UNAUTHORIZED),
            (None, true, StatusCode::UNAUTHORIZED),
        ] {
            let mut headers: Vec<_> = auth.into_iter().map(|auth| (header::AUTHORIZATION, auth)).collect();
            if is_upgrade {
                headers.extend(upgrade());
            }
            let res = auth_router(&app)
                .oneshot(guarded(Method::POST, "/api/sessions", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "upgrade={is_upgrade}");
            if expected == StatusCode::OK {
                assert_eq!(body_text(res).await, "created");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cookie_posts_keep_the_same_origin_origin_requirement() {
        let root = temp_root();
        let app = test_app(&root);
        for (value, is_upgrade, expected) in [
            ("http://127.0.0.1:7878", false, StatusCode::OK),
            ("http://evil.example", false, StatusCode::FORBIDDEN),
            ("http://127.0.0.1:7878", true, StatusCode::OK),
            ("http://evil.example", true, StatusCode::FORBIDDEN),
        ] {
            let mut headers = vec![cookie(&app), origin(value)];
            if is_upgrade {
                headers.extend(upgrade());
            }
            let res = auth_router(&app)
                .oneshot(guarded(Method::POST, "/api/sessions", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "origin {value} upgrade={is_upgrade}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_upgrade_without_a_token_is_rejected() {
        let root = temp_root();
        let app = test_app(&root);
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/stream", upgrade()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_cookie_upgrade_from_elsewhere_is_forbidden() {
        let root = temp_root();
        let app = test_app(&root);
        let mut headers = vec![cookie(&app), origin("http://evil.example")];
        headers.extend(upgrade());
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/stream", headers))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_upgrade_with_a_token_reaches_the_handler() {
        let root = temp_root();
        let app = test_app(&root);
        // Bearer needs no Origin; cookie auth needs the same-origin one. Either way the guard
        // passes and routing reaches the real WebSocket extractor — which answers 426 here only
        // because `oneshot` carries no hyper upgrade state (production answers 101). A guard
        // failure would be 401/403 instead, and a missing route the fallback page.
        let mut cookie_headers = vec![cookie(&app), origin("http://127.0.0.1:7878")];
        cookie_headers.extend(upgrade());
        let mut bearer_headers = vec![bearer(&app)];
        bearer_headers.extend(upgrade());
        for headers in [bearer_headers, cookie_headers] {
            let res = auth_router(&app)
                .oneshot(guarded(Method::GET, "/api/stream", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_sign_in_link_sets_the_cookie_and_a_bad_token_is_rejected() {
        let root = temp_root();
        let app = test_app(&root);
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, &format!("/?token={}", app.api_token), vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let set_cookie = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
        // The token must not linger in caches or leak via the Referer header on the next click.
        assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(res.headers().get(header::REFERRER_POLICY).unwrap(), "no-referrer");
        assert!(
            body_text(res).await.contains("location.replace"),
            "the page signs in with JS, not a redirect"
        );

        for uri in ["/", "/?token=wrong"] {
            let res = auth_router(&app).oneshot(guarded(Method::GET, uri, vec![])).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "GET {uri}");
            assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store", "GET {uri}");
            assert!(body_text(res).await.contains("colonizer open"), "GET {uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_public_status_is_an_allowlist_and_the_signed_in_status_is_not() {
        let root = temp_root();
        let app = test_app(&root);
        let public = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/status", vec![]))
            .await
            .unwrap();
        assert_eq!(public.status(), StatusCode::OK);
        let public_text = body_text(public).await;
        let public_body: Value = serde_json::from_str(&public_text).unwrap();
        let mut keys: Vec<&str> = public_body.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["host", "queue_depth", "runtime", "storage", "version"]);

        let signed_in = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/status", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(signed_in.status(), StatusCode::OK);
        let full: Value = serde_json::from_str(&body_text(signed_in).await).unwrap();
        for key in ["github", "claude", "assets", "modules", "sandbox", "mesh"] {
            assert!(full.get(key).is_some(), "the signed-in body keeps {key}");
            assert!(public_body.get(key).is_none(), "the public body drops {key}");
        }
        // Whatever the probes found — the host id, the hostname — must not cross over.
        let id = full["host"]["id"].as_str().unwrap().to_string();
        assert!(!public_text.contains(&id), "the host id stays in the signed-in body");
        if let Some(hostname) = full["host"]["hostname"].as_str() {
            assert!(!public_text.contains(hostname), "the hostname stays in the signed-in body");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_sessions_file_loads_as_empty_with_no_alert() {
        let root = temp_root();
        let (sessions, alert) = load_sessions(&root.join("data/sessions.json")).unwrap();
        assert!(sessions.is_empty());
        assert!(alert.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_valid_sessions_file_loads_with_no_alert() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, session_json()).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "abc123");
        assert!(alert.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_corrupt_sessions_file_is_saved_aside_and_the_harness_starts_empty_with_an_alert() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, b"this is not json").unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert!(sessions.is_empty());
        let alert = alert.unwrap();
        assert_eq!(alert.kind, StorageAlertKind::LoadDamage);
        assert!(alert.message.contains(".corrupt-"), "{}", alert.message);
        assert!(
            alert.message.contains("worktrees, branches and microVMs"),
            "{}",
            alert.message
        );
        // The original path no longer holds the corrupt bytes; the saved copy keeps them byte for byte.
        let saved = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), b"this is not json");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// One damaged record must not cost the others: the good colonies load, the alert names the
    /// counts, and the original file stays on disk with a byte-identical copy beside it.
    #[test]
    fn a_damaged_record_is_salvaged_around_and_the_original_file_is_kept() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        let mut damaged = record_json("broken");
        damaged["cost_usd"] = json!("not a number");
        let file = json!([record_json("first"), damaged, record_json("third")]).to_string();
        std::fs::write(&path, &file).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert_eq!(sessions.len(), 2, "the good records survive the damaged one");
        assert_eq!(sessions[0].id, "first");
        assert_eq!(sessions[1].id, "third");
        let alert = alert.unwrap();
        assert_eq!(alert.kind, StorageAlertKind::LoadDamage);
        assert!(alert.message.contains("kept 2 of 3 records"), "{}", alert.message);
        assert!(alert.message.contains("1 of them could not be loaded"), "{}", alert.message);
        assert!(alert.message.contains(".corrupt-"), "{}", alert.message);
        assert!(path.exists(), "the original stays for the next startup to salvage again");
        let saved = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), file.as_bytes(), "the copy is the original");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A file where every record is damaged still parses as a list, so it is salvaged, not moved
    /// aside: nothing is renamed, the original stays put for the next startup, the copy holds the
    /// bytes, and the alert says all the records were lost.
    #[test]
    fn a_file_where_every_record_is_damaged_is_salvaged_to_an_empty_list() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        let mut damaged = record_json("broken");
        damaged["cost_usd"] = json!("not a number");
        let file = json!([damaged, {"repo": "acme/app"}]).to_string();
        std::fs::write(&path, &file).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert!(sessions.is_empty(), "nothing was salvageable");
        let alert = alert.unwrap();
        assert!(alert.message.contains("kept 0 of 2 records"), "{}", alert.message);
        assert!(alert.message.contains("2 of them could not be loaded"), "{}", alert.message);
        assert!(path.exists(), "the original stays put rather than being renamed aside");
        let saved = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains(".corrupt-"))
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), file.as_bytes(), "the copy is the original");
        let _ = std::fs::remove_dir_all(root);
    }

    /// With every field defaulting, an unrelated object would otherwise load as a blank colony, so a
    /// record whose `id` is not a plain name counts as damaged and is dropped; here the `id` is
    /// missing altogether and so defaults to empty.
    #[test]
    fn a_record_without_an_id_is_damaged_and_dropped() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        let file = json!([record_json("kept"), {"repo": "acme/app"}]).to_string();
        std::fs::write(&path, file).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "kept");
        let alert = alert.unwrap();
        assert!(alert.message.contains("kept 1 of 2 records"), "{}", alert.message);
        let _ = std::fs::remove_dir_all(root);
    }

    /// An `id` names the colony's directory under `data/sessions`, and deleting a colony removes that
    /// directory whole, so a traversal-shaped id must be refused at load: a record that deserializes
    /// fine but names a directory elsewhere counts as damaged and is dropped.
    #[test]
    fn a_record_with_a_traversal_id_is_damaged_and_dropped() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        let file = json!([record_json("kept"), record_json("../../victim")]).to_string();
        std::fs::write(&path, file).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "kept");
        let alert = alert.unwrap();
        assert!(alert.message.contains("kept 1 of 2 records"), "{}", alert.message);
        assert!(path.exists(), "the original stays for the next startup to salvage again");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The salvage copy must not fire on the happy path: a fully valid file loads with no alert and
    /// leaves no `.corrupt-*` file behind.
    #[test]
    fn a_fully_valid_file_loads_with_no_alert_and_no_copy_made() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, session_json()).unwrap();
        let (sessions, alert) = load_sessions(&path).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(alert.is_none());
        assert!(path.exists(), "the file is untouched");
        assert!(
            !std::fs::read_dir(path.parent().unwrap()).unwrap().any(|e| e
                .unwrap()
                .path()
                .to_string_lossy()
                .contains(".corrupt-")),
            "nothing was copied aside"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unsalvageable_sessions_file_stops_startup_and_is_left_untouched() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, b"this is not json").unwrap();
        let _guard = util::faults::inject("sessions.json", util::faults::Op::Rename, || {
            std::io::Error::from_raw_os_error(5)
        });
        let err = load_sessions(&path).unwrap_err();
        assert!(err.to_string().contains("move it aside yourself"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"this is not json", "the file is untouched");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The salvage copy rides the same fault seam as the move-aside: inject a failure at the copy and
    /// startup must abort, because going on would leave the damaged bytes unpreserved and the next
    /// save would silently drop them. The original is left exactly as it was found.
    #[test]
    fn a_salvage_copy_that_fails_stops_startup_and_the_original_is_left_untouched() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        let mut damaged = record_json("broken");
        damaged["cost_usd"] = json!("not a number");
        let file = json!([record_json("first"), damaged]).to_string();
        std::fs::write(&path, &file).unwrap();
        let _guard = util::faults::inject("sessions.json", util::faults::Op::Write, || {
            std::io::Error::from_raw_os_error(5)
        });
        let err = load_sessions(&path).unwrap_err();
        assert!(err.to_string().contains("move it aside yourself"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), file.as_bytes(), "the file is untouched");
        assert!(
            !std::fs::read_dir(path.parent().unwrap()).unwrap().any(|e| e
                .unwrap()
                .path()
                .to_string_lossy()
                .contains(".corrupt-")),
            "no copy was made"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn storage_failures_are_counted_and_sticky() {
        let root = temp_root();
        let app = test_app(&root);
        app.storage_failed("write the list", &anyhow!("disk is full")).await;
        app.storage_failed("write the list", &anyhow!("disk is still full")).await;
        let alert = app.storage_alert.read().await.clone().unwrap();
        assert_eq!(alert.failures, 2, "each failure increments the counter");
        assert!(alert.message.contains("write the list failed"), "{}", alert.message);
        assert!(
            alert.message.contains("disk is still full"),
            "the latest failure wins: {}",
            alert.message
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_status_storage_key_is_ok_until_a_write_goes_unconfirmed() {
        let verdict = reclaim::FreeSpaceVerdict::default();
        let value = storage_status(None, &verdict);
        assert_eq!(value["ok"], true);
        assert!(value["free_bytes"].is_null(), "no queue tick has measured yet: {value}");
        assert_eq!(value["warn_free_bytes"], json!(verdict.warn_free_bytes));
        assert_eq!(value["min_free_bytes"], json!(verdict.min_free_bytes));
        assert_eq!(value["low_disk"], false);
        assert_eq!(value["admission_paused"], false);
        let alert = StorageAlert {
            kind: StorageAlertKind::Write,
            message: "save the session list failed: disk is full".into(),
            ts: Utc::now(),
            failures: 3,
            recovered_at: None,
        };
        let value = storage_status(Some(alert), &verdict);
        assert_eq!(value["ok"], false);
        assert_eq!(value["kind"], "write");
        assert_eq!(value["message"], "save the session list failed: disk is full");
        assert_eq!(value["failures"], 3);
        assert!(
            value["ts"].is_string(),
            "the ts is the RFC 3339 string the harness_log frames use: {value}"
        );
        assert!(value["recovered_at"].is_null(), "still failing: {value}");
    }

    /// The sequence from #220's disk-full incident: writes fail, space is freed and they succeed,
    /// then the disk fills again. Each step must read differently, and the count never resets.
    #[tokio::test]
    async fn a_storage_alert_reports_recovery_and_a_later_failure_undoes_it() {
        let root = temp_root();
        let app = test_app(&root);
        app.storage_succeeded().await;
        assert!(
            app.storage_alert.read().await.is_none(),
            "a success with no failure behind it raises nothing"
        );

        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["ok"], false);
        assert!(failing["recovered_at"].is_null(), "{failing}");
        assert_eq!(failing["failures"], 1, "{failing}");
        assert!(failing["message"].as_str().unwrap().contains("disk is full"), "{failing}");
        assert!(failing["ts"].is_string(), "{failing}");

        app.persist_sessions().await.unwrap();
        let recovered = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(recovered["ok"], true, "a write went through, so the disk is not broken now");
        assert!(recovered["recovered_at"].is_string(), "{recovered}");
        assert_eq!(
            recovered["ts"], failing["ts"],
            "the failure it recovered from is still the one shown"
        );
        assert_eq!(recovered["failures"], 1);
        assert!(recovered["message"].as_str().unwrap().contains("disk is full"));
        app.storage_succeeded().await;
        let again = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(
            again["recovered_at"], recovered["recovered_at"],
            "recovery is stamped by the first success, not moved by every later one"
        );

        app.storage_failed("save the session list", &anyhow!("disk is full again"))
            .await;
        let failing_again = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing_again["ok"], false, "a new failure makes the alert current again");
        assert!(failing_again["recovered_at"].is_null(), "{failing_again}");
        assert_eq!(failing_again["failures"], 2, "failures stay cumulative across a recovery");
        assert_ne!(
            failing_again["ts"], recovered["ts"],
            "the new failure is stamped afresh: {failing_again}"
        );
        assert!(
            failing_again["message"].as_str().unwrap().contains("disk is full again"),
            "{failing_again}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Load damage reports colonies no save brings back (#371): the saves after startup must not mark
    /// it recovered, and a write failure on top of it is shown while it lasts but must not overwrite
    /// it, so once writes go through again the load damage is shown as it was.
    #[tokio::test]
    async fn load_damage_is_never_recovered_and_outlasts_a_write_failure() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, b"this is not json").unwrap();
        let (_, damage) = load_sessions(&path).unwrap();
        let mut app = test_app(&root);
        Arc::get_mut(&mut app).unwrap().load_damage = damage;

        app.persist_sessions().await.unwrap();
        let damaged = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(damaged["kind"], "load_damage");
        assert_eq!(damaged["ok"], true, "writes are going through; the kind keeps it shown");
        assert!(
            damaged["recovered_at"].is_null(),
            "a save does not bring the colonies back: {damaged}"
        );
        assert!(damaged["message"].as_str().unwrap().contains(".corrupt-"), "{damaged}");

        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["kind"], "write");
        assert_eq!(failing["ok"], false);
        assert_eq!(failing["failures"], 1, "the load damage is not counted as a failed write");

        app.persist_sessions().await.unwrap();
        let after = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(after, damaged, "the load damage is shown again, unchanged");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Damage a settings file the readers fall back over (#408): the alert shows in /api/status,
    /// yields to a failing disk while it lasts, comes back once writes go through, and clears when
    /// the file reads cleanly again.
    #[tokio::test]
    async fn config_damage_shows_until_the_file_reads_cleanly_again() {
        let root = temp_root();
        let mut app = test_app(&root);
        let path = app.cfg.config_dir.join("orgs.json");
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(&path, b"broken").unwrap();

        assert!(app.all_org_settings().is_empty(), "the reader still answers");
        let shown = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(shown["kind"], "load_damage");
        assert_eq!(shown["ok"], true, "writes still go through; the kind keeps it shown");
        assert!(
            shown["message"].as_str().unwrap().contains("orgs.json")
                && shown["message"].as_str().unwrap().contains("defaults are in effect"),
            "{shown}"
        );

        // Live damage outranks the sticky startup damage: only the live one can clear.
        Arc::get_mut(&mut app).unwrap().load_damage = Some(StorageAlert {
            kind: StorageAlertKind::LoadDamage,
            message: "sessions.json was moved aside at startup".into(),
            ts: Utc::now(),
            failures: 1,
            recovered_at: None,
        });
        assert!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["message"]
                .as_str()
                .unwrap()
                .contains("orgs.json"),
            "the alert that can still clear is the one shown"
        );

        // A failing disk outranks it while it lasts, then it comes back unchanged.
        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["kind"], "write");
        app.storage_succeeded().await;
        assert_eq!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["kind"],
            "load_damage"
        );

        std::fs::write(&path, b"{}").unwrap();
        assert!(app.all_org_settings().is_empty());
        assert!(
            app.config_damage.lock().unwrap().is_none(),
            "a clean read clears the alert that named the file"
        );
        assert!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["message"]
                .as_str()
                .unwrap()
                .contains("sessions.json"),
            "with the live alert cleared, the startup damage shows again"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A platform whose assets vendor no mesh binaries — every Mac — is a fact to explain, not a
    /// fault (#128). Before the fix the explanation sat in `error`, and everything the web UI reads
    /// from `error` turns red, so every Mac's Runtime section showed "Something is missing" for its
    /// whole life. The real handler over the stock fixture does exactly that shape: mesh enabled,
    /// no assets, nothing broken.
    #[tokio::test]
    async fn the_status_mesh_is_unavailable_with_a_null_error_where_no_binaries_are_vendored() {
        let root = temp_root();
        let app = test_app(&root);
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        let mesh = &payload["mesh"];
        assert_eq!(mesh["enabled"], true);
        assert_eq!(mesh["provider"], "headscale");
        assert_eq!(mesh["state"], "unavailable", "{mesh}");
        assert!(mesh["detail"].as_str().is_some_and(|d| !d.is_empty()), "{mesh}");
        assert!(
            mesh["error"].is_null(),
            "a non-null error here is the regression: the UI paints it as a fault: {mesh}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The other half of the contract: a mesh that genuinely cannot be built is a fault, and the
    /// reason lands in `error`, the field the web UI reads as "something is missing".
    #[tokio::test]
    async fn a_mesh_that_cannot_be_built_is_reported_as_an_error_carrying_the_reason() {
        let root = temp_root();
        let assets = root.join("assets");
        for rel in [
            "vendor/headscale",
            "vendor/tailscale/tailscale",
            "vendor/tailscale/tailscaled",
        ] {
            let path = assets.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let broken: Result<Value> = Err(anyhow!("headscale refused to start"));
        let value = mesh_status(&ModulesConfig::default(), Some(&assets), async move { broken }).await;
        assert_eq!(value["state"], "error", "{value}");
        assert_eq!(value["error"], "headscale refused to start", "{value}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A mesh branch that never resolves must not take `/api/status` with it either: the bound here
    /// covers the wait on `Mesh`'s `running` lock, which a boot can hold across calls nothing else
    /// bounds. Paused time fires the 15s branch limit without waiting for it; a regression to an
    /// unbounded await would hang this test instead of passing it.
    #[tokio::test(start_paused = true)]
    async fn a_mesh_branch_that_never_resolves_is_reported_as_an_error_when_the_branch_limit_fires() {
        let root = temp_root();
        let assets = root.join("assets");
        for rel in [
            "vendor/headscale",
            "vendor/tailscale/tailscale",
            "vendor/tailscale/tailscaled",
        ] {
            let path = assets.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let started = std::time::Instant::now();
        let value = mesh_status(&ModulesConfig::default(), Some(&assets), std::future::pending()).await;
        assert_eq!(value["enabled"], true, "{value}");
        assert_eq!(value["provider"], "headscale", "{value}");
        assert_eq!(value["state"], "error", "{value}");
        let message = value["error"].as_str().unwrap_or_default();
        assert!(message.contains("timed out"), "{value}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the branch answered in {:?}; the pending future was waited out",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A probe that never returns must not take `/api/status` with it: the handler still answers,
    /// with the probe reported as absent. Paused time fires the 5s probe limit without waiting for
    /// it; a regression to an unbounded exec would hang this test instead of passing it.
    #[tokio::test(start_paused = true)]
    async fn the_status_handler_still_answers_when_the_msb_probe_never_returns() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root();
        let mut app = test_app(&root);
        let wedged = root.join("wedged-msb");
        std::fs::write(&wedged, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&wedged, std::fs::Permissions::from_mode(0o755)).unwrap();
        Arc::get_mut(&mut app).unwrap().cfg.msb = wedged.display().to_string();
        let started = std::time::Instant::now();
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        assert!(
            payload["sandbox"]["msb_version"].is_null(),
            "the wedged probe is reported as absent, not hung: {payload}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the handler answered in {:?}; the probe was waited out",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `host` object rides every poll: a stable id, the admission numbers, and the RFC 3339
    /// `checked_at` that says how fresh the rest of it is. On this Linux machine the probe also
    /// fills most of the measurables; the contract only promises `id` and `checked_at` wherever the
    /// mothership runs.
    #[tokio::test]
    async fn the_status_payload_carries_a_host_object_with_the_microvm_counts() {
        let root = temp_root();
        let app = test_app(&root);
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        let host = &payload["host"];
        assert!(host["id"].as_str().is_some_and(|s| !s.is_empty()), "{host}");
        assert!(
            host["checked_at"].is_string(),
            "checked_at is the RFC 3339 string the contract sends: {host}"
        );
        assert!(host["microvms_live"].is_u64(), "{host}");
        assert!(host["microvms_ceiling"].is_u64(), "{host}");
        assert!(
            !payload["host"].is_null(),
            "host is a top-level key, not nested under runtime: {payload}"
        );
        assert!(
            payload["runtime"]["host"].is_null(),
            "Runtime carries no host of its own (`os` aside): host lives only at the top level: {payload}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The memo serves a second lookup for the same `elf_only` key without looking again, and keeps
    /// the guest's and the host's answers apart. The lookup is stubbed: only the memo is on trial.
    #[tokio::test]
    async fn a_found_claude_bin_is_memoised_under_its_own_elf_only_key() {
        let root = temp_root();
        let app = test_app(&root);
        let guest = PathBuf::from("/opt/claude-guest/claude");
        let found = memoised_claude_bin(&app, true, async { Ok(guest.clone()) }).await.unwrap();
        assert_eq!(found, guest);
        let again = memoised_claude_bin(&app, true, async { panic!("the memo must serve the second call") })
            .await
            .unwrap();
        assert_eq!(again, guest);
        let host = PathBuf::from("/usr/local/bin/claude");
        let other = memoised_claude_bin(&app, false, async { Ok(host.clone()) }).await.unwrap();
        assert_eq!(other, host, "the host lookup has its own key");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A failed lookup must not be kept: Settings re-renders the red "missing" row from every 30 s
    /// poll, so a Claude Code installed while the harness runs has to be found without a restart.
    #[tokio::test]
    async fn a_failed_claude_bin_lookup_is_not_memoised_so_a_later_install_is_found() {
        let root = temp_root();
        let app = test_app(&root);
        assert!(memoised_claude_bin(&app, true, async { bail!("nothing yet") }).await.is_err());
        let installed = PathBuf::from("/usr/local/bin/claude");
        let found = memoised_claude_bin(&app, true, async { Ok(installed.clone()) })
            .await
            .unwrap();
        assert_eq!(found, installed);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A bundled `bin/claude-guest` must still be an ELF the colony can exec: without the
    /// enforcement the resolver would mount a stale Mach-O artefact and the colony would die
    /// with ENOEXEC after boot instead of failing fast here with the install.sh hint.
    #[tokio::test]
    async fn resolve_guest_claude_bin_refuses_a_bundled_non_elf_asset() {
        let root = temp_root();
        let assets = root.join("assets");
        std::fs::create_dir_all(assets.join("bin")).unwrap();
        std::fs::write(assets.join("bin/claude-guest"), b"\xcf\xfa\xed\xfe").unwrap();
        let app = test_app_with(&root, |cfg| cfg.assets = Some(assets.clone()));
        let err = resolve_guest_claude_bin(&app).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("is not an ELF binary"), "{message}");
        assert!(message.contains("scripts/install.sh"), "{message}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The bundled-asset branch resolves a valid ELF without touching the PATH walk.
    #[tokio::test]
    async fn resolve_guest_claude_bin_accepts_a_bundled_elf_asset() {
        let root = temp_root();
        let assets = root.join("assets");
        std::fs::create_dir_all(assets.join("bin")).unwrap();
        std::fs::write(assets.join("bin/claude-guest"), b"\x7fELF padding").unwrap();
        let app = test_app_with(&root, |cfg| cfg.assets = Some(assets.clone()));
        let found = resolve_guest_claude_bin(&app).await.unwrap();
        assert_eq!(found, assets.join("bin/claude-guest"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A walk whose candidates all wedge must give up on the overall limit and say so, not grind
    /// through every candidate at `PROBE_LIMIT` apiece. Paused time fires the 10s walk limit
    /// without waiting for it; each probe burns its whole 5s, so the probe file can only ever show
    /// the first three of the five candidates — a walk with no limit would leave all five.
    #[tokio::test(start_paused = true)]
    async fn a_walk_over_candidates_that_all_wedge_times_out_rather_than_probing_every_one() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root();
        let probed = root.join("probed");
        let candidates: Vec<PathBuf> = (0..5)
            .map(|i| {
                let path = root.join(format!("wedged-{i}"));
                std::fs::write(&path, format!("#!/bin/sh\necho x >> {}\nexec sleep 60\n", probed.display())).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                path
            })
            .collect();
        let started = std::time::Instant::now();
        let err = walk_claude_candidates(&candidates, false).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("timed out"), "the walk names its cause: {message}");
        assert!(!message.contains("no Claude Code binary found"), "{message}");
        let reached = std::fs::read_to_string(&probed).unwrap_or_default().lines().count();
        assert!(
            reached < candidates.len(),
            "{reached} of {} candidates probed; the walk ground through every one",
            candidates.len()
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the walk answered in {:?}; the wedged probes were waited out",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Cached answers for slow read-only endpoints, by key: when each was computed, and the value.
#[derive(Default)]
pub struct AnswerCache {
    entries: std::sync::Mutex<HashMap<String, (Instant, serde_json::Value)>>,
    refreshing: std::sync::Mutex<std::collections::HashSet<String>>,
}

/// Stale-while-revalidate for a slow read-only answer. Within `fresh` the cached value is returned
/// as is; past it the cached value is still returned at once and one background refresh is started
/// (never two for the same key); with nothing cached the caller waits for the first computation. A
/// failed computation keeps the last good value and reports the error only when there is none.
pub async fn cached_answer<F, Fut>(
    app: &Shared,
    key: impl Into<String>,
    fresh: Duration,
    compute: F,
) -> anyhow::Result<serde_json::Value>
where
    F: Fn(Shared) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + 'static,
{
    let key: String = key.into();
    let hit = app
        .answer_cache
        .entries
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
        .cloned();
    if let Some((at, value)) = hit {
        if at.elapsed() >= fresh {
            let first = app
                .answer_cache
                .refreshing
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key.clone());
            if first {
                let app = app.clone();
                tokio::spawn(async move {
                    if let Ok(value) = compute(app.clone()).await {
                        app.answer_cache
                            .entries
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .insert(key.clone(), (Instant::now(), value));
                    }
                    app.answer_cache
                        .refreshing
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&key);
                });
            }
        }
        return Ok(value);
    }
    let value = compute(app.clone()).await?;
    app.answer_cache
        .entries
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key, (Instant::now(), value.clone()));
    Ok(value)
}

#[cfg(test)]
mod answer_cache_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_fresh_answer_is_served_from_the_cache_and_a_stale_one_refreshes_behind_it() {
        let root = std::env::temp_dir().join(format!("colonizer-answer-cache-{}", util::short_id()));
        let app = tests::test_app(&root);
        let calls = Arc::new(AtomicUsize::new(0));
        let compute = {
            let calls = calls.clone();
            move |_app: Shared| {
                let calls = calls.clone();
                async move { Ok(serde_json::json!(calls.fetch_add(1, Ordering::SeqCst) + 1)) }
            }
        };
        // Nothing cached: the caller waits for the first answer.
        assert_eq!(
            cached_answer(&app, "k", Duration::from_secs(60), compute.clone())
                .await
                .unwrap(),
            1
        );
        // Fresh: no second computation.
        assert_eq!(
            cached_answer(&app, "k", Duration::from_secs(60), compute.clone())
                .await
                .unwrap(),
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Stale: the old value comes back at once and one refresh runs behind it.
        assert_eq!(cached_answer(&app, "k", Duration::ZERO, compute.clone()).await.unwrap(), 1);
        for _ in 0..50 {
            if calls.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(cached_answer(&app, "k", Duration::from_secs(60), compute).await.unwrap(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
