//! The mothership's shared state: `App`, the one value every handler and background task holds
//! (as `Shared`), how it is built at startup, and the HTTP error type the handlers answer with.
//!
//! Adding a module's state: give it a type in its own module with a constructor that takes what it
//! needs from `Settings`, then add one field to the sorted "module state" block of `App` and one
//! line to the sorted block of `App::new`. Both blocks are alphabetical so that two pull requests
//! adding state land on different lines.

use crate::config::{ModulesConfig, Settings, setting_u64};
use crate::mesh::{Mesh, Ports};
use crate::modules::AgentModule;
use crate::sessions::Session;
use crate::util::{env_nonempty, exec_within, is_elf, is_plain_name, read_secret};
use crate::{AnswerCache, claude_accounts, modules, reclaim, sessions, util};
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
};

pub const CLAUDE_API_HOST: &str = "api.anthropic.com";

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
    pub(crate) session_persist: Mutex<()>,
    /// Serialises the read-modify-write of `orgs.json` and `providers.json` (`orgs::put`,
    /// `providers::put`/`delete`), of `known-orgs.json` (`orgs::record_known_sightings`, which
    /// `mark_org_known` and the refresh's record update go through) and of `claude-accounts.json`
    /// (`claude_accounts::create`/`delete`): one read, the modify, and the save happen as one
    /// critical section, so two saves at once cannot lose each other's orgs, providers, known
    /// orgs or accounts, and no settings save ever follows a read that failed (#408).
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
    pub(crate) repo_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// One lifecycle lock per colony id. It serialises the moments a colony gains or loses its
    /// microVM — the claim that starts a boot, and the teardown that removes one — so a resume can
    /// never claim a colony whose stop is still tearing that VM down, and a cleanup can never free
    /// the worktree a boot is starting on. See `session_lock` for what it deliberately does not cover.
    pub(crate) session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    pub(crate) mesh: Mutex<Option<Arc<Mesh>>>,

    // ---- Module state: one field per module, in alphabetical order. A module adds its field here and
    // its constructor to the same place in `App::new`, so parallel additions land on different lines.
    /// The activity log behind `GET /api/activity` and History (activity.rs).
    pub activity: crate::activity::ActivityLog,
    /// Slow read-only answers (`/api/repos`, `/api/storage`) kept so a page load does not wait on
    /// `gh` or a disk walk: see [`cached_answer`].
    pub answer_cache: AnswerCache,
    /// Scoped API tokens handed to CLIs and automations (issue #508, api_tokens.rs), saved to
    /// `<config_dir>/api-tokens.json`; `host_guard` checks a Bearer against them when it is not
    /// the owner token.
    pub api_tokens: crate::api_tokens::Registry,
    /// The last Anthropic profile lookup for the Claude credential, cached so the status poll does not
    /// hammer Anthropic. Keyed on a fingerprint of the token; the token itself is never stored.
    pub claude_account: Mutex<Option<crate::claude_login::AccountStatus>>,
    /// The Claude binaries found this run, keyed on whether the guest's ELF requirement was asked,
    /// so the status poll does not walk PATH and probe every candidate on every poll. Successes
    /// only: Settings re-renders the red "missing" row from every poll, so a binary installed while
    /// the harness runs must be picked up without a restart.
    pub claude_bins: Mutex<HashMap<bool, PathBuf>>,
    /// The fleet view's peer half (issue #231): last-known `HostSummary` per configured peer base URL,
    /// so a peer that goes quiet still shows its last real numbers instead of nulls. This machine's
    /// own entry is never cached here — `crate::fleet::self_summary` always computes it live.
    pub fleet_cache: crate::fleet::FleetCache,
    pub gateway: crate::gateway::Gateway,
    /// The last `gh api user` answer for the GitHub credential, cached so the status poll does not
    /// hammer GitHub. Keyed on a fingerprint of the token; the token itself is never stored.
    pub github_viewer: Mutex<Option<crate::github::ViewerStatus>>,
    /// The graft skillset download (graft.rs), for Settings.
    pub graft: Mutex<crate::graft::Status>,
    /// The Headroom bundle download, started when Headroom is switched on.
    pub headroom: Mutex<crate::headroom::Status>,
    /// The last host probe (name, size, disk), cached the same 10 s as the runtime probe. `?fresh=1`
    /// bypasses it.
    pub host_cache: Mutex<Option<crate::runtime::HostCached>>,
    /// Response bodies kept with their ETag / Last-Modified, so a refresh re-asks GitHub and the
    /// registries conditionally and a 304 reuses the body (`<data_dir>/cache/http`).
    pub http_cache: crate::cache_store::DiskCache,
    /// Avatars fetched by `/api/img` (`<data_dir>/cache/img`).
    pub img_cache: crate::cache_store::DiskCache,
    /// The shared anti-spam ledger (`<data_dir>/ledger.json`): notify and the autonomy judge count
    /// every outbound proactive action against it, so the operator's attention is one bounded rate.
    pub ledger: crate::ledger::LedgerStore,
    pub login: crate::claude_login::LoginManager,
    /// Scheduled colonies (loops.rs), saved to `<config_dir>/loops.json`.
    pub loops: crate::loops::LoopStore,
    pub memory: crate::memory::MemoryStore,
    /// GitHub orgs on the signed-in account that the operator has not answered for yet — login to
    /// avatar, shown with a prompt instead of being adopted silently. In-memory on purpose: after a
    /// restart `refresh_orgs` recomputes it from `known-orgs.json`.
    pub new_orgs: RwLock<BTreeMap<String, Option<String>>>,
    /// Each org's GitHub description, from the same `/user/orgs` fetch, for the workspace page.
    /// In-memory: the first refresh after a restart fills it again.
    pub org_descriptions: RwLock<BTreeMap<String, String>>,
    /// When the last org refresh failed, so a `gh` that keeps failing is retried once a minute
    /// rather than on every workspace poll.
    pub orgs_failed_at: Mutex<Option<std::time::Instant>>,
    /// When the user's GitHub orgs were last fetched.
    pub orgs_refreshed: Mutex<Option<std::time::Instant>>,
    /// Boot-time provider probe results, keyed on provider id + base URL
    /// (`crate::gateway::probe_cache_key`) so repointing a provider never serves the old endpoint's
    /// answer. Both reachable and unreachable answers are kept for [`crate::gateway::PROVIDER_PROBE_TTL`];
    /// a dead provider would otherwise cost every boot up to the 5 s probe timeout.
    pub provider_probe_cache: Mutex<HashMap<String, (Instant, Value)>>,
    /// The most recent background image pull, so Settings can show it.
    pub pull: Mutex<crate::sandbox::PullStatus>,
    pub redteam: crate::redteam::RedTeamStore,
    /// Remote access: the switch, the tunnel identity and the live link (remote.rs).
    pub remote: crate::remote::Remote,
    /// Owners seen in the repository list, so org workspaces can be offered before any colony exists.
    pub repo_owners: RwLock<BTreeSet<String>>,
    /// The last `runtime` probe for the status payload, cached so the poll does not re-spawn the
    /// version probes for every open tab. `GET /api/status?fresh=1` bypasses it.
    pub runtime_cache: Mutex<Option<crate::runtime::Cached>>,
    /// The `GET /api/stream` push hub: one shared broadcast diff task for all open tabs.
    pub stream: crate::stream::Hub,
    /// The live map on colonizer.dev, off until the user switches it on.
    pub telemetry: crate::telemetry::Telemetry,
    pub updater: crate::update::Updater,
    pub updates: crate::version::Updates,
    /// Anonymous usage reporting, local half only: the batch that would be sent and the switch for it.
    pub usage: crate::usage::Usage,
}

pub type Shared = Arc<App>;

/// What `App::new` takes from startup rather than building itself: the colonies and module
/// settings read from disk, the agent modules found, any damage met while loading them, and the
/// cockpit API token.
pub struct Boot {
    pub sessions: Vec<Session>,
    pub modules: ModulesConfig,
    pub agents: Vec<AgentModule>,
    pub agent_problems: Vec<String>,
    pub load_damage: Option<StorageAlert>,
    pub api_token: String,
}

impl App {
    /// The one place an `App` is built, for the mothership and for tests alike. The module state
    /// block is alphabetical, like the struct's: a new module adds its line in its own place.
    pub fn new(cfg: Settings, boot: Boot) -> Result<App> {
        Ok(App {
            api_token: boot.api_token,
            modules: RwLock::new(boot.modules),
            agents: boot.agents,
            agent_problems: boot.agent_problems,
            sessions: RwLock::new(boot.sessions),
            session_persist: Mutex::new(()),
            config_write: Mutex::new(()),
            config_damage: std::sync::Mutex::new(None),
            storage_alert: RwLock::new(None),
            disk_verdict: Mutex::new(Default::default()),
            load_damage: boot.load_damage,
            runtimes: Mutex::new(HashMap::new()),
            repo_locks: Mutex::new(HashMap::new()),
            session_locks: Mutex::new(HashMap::new()),
            mesh: Mutex::new(None),

            // ---- Module state: one line per module, in alphabetical order.
            activity: crate::activity::ActivityLog::new(),
            answer_cache: AnswerCache::persistent(cfg.data_dir.join("cache/answers")),
            api_tokens: crate::api_tokens::Registry::load(&cfg.config_dir),
            claude_account: Mutex::new(None),
            claude_bins: Mutex::new(HashMap::new()),
            fleet_cache: crate::fleet::FleetCache::new(),
            gateway: crate::gateway::Gateway::new(&cfg.data_dir)?,
            github_viewer: Mutex::new(None),
            graft: Mutex::new(Default::default()),
            headroom: Mutex::new(Default::default()),
            host_cache: Mutex::new(None),
            http_cache: crate::cache_store::DiskCache::new(cfg.data_dir.join("cache/http"), crate::cache_store::HTTP_MAX_BYTES),
            img_cache: crate::cache_store::DiskCache::new(cfg.data_dir.join("cache/img"), crate::cache_store::IMG_MAX_BYTES),
            ledger: crate::ledger::LedgerStore::load(&cfg.data_dir),
            login: Default::default(),
            loops: crate::loops::LoopStore::new(&cfg.config_dir),
            memory: crate::memory::MemoryStore::new(cfg.data_dir.join("memory")),
            new_orgs: RwLock::new(BTreeMap::new()),
            org_descriptions: RwLock::new(BTreeMap::new()),
            orgs_failed_at: Mutex::new(None),
            orgs_refreshed: Mutex::new(None),
            provider_probe_cache: Mutex::new(HashMap::new()),
            pull: Mutex::new(Default::default()),
            redteam: crate::redteam::RedTeamStore::new(&cfg.data_dir, &cfg.config_dir),
            remote: crate::remote::Remote::new(&cfg.config_dir)?,
            repo_owners: RwLock::new(BTreeSet::new()),
            runtime_cache: Mutex::new(None),
            stream: crate::stream::Hub::new(),
            telemetry: crate::telemetry::Telemetry::new(&cfg.config_dir)?,
            updater: crate::update::Updater::new(),
            updates: crate::version::Updates::new(&cfg.config_dir)?,
            usage: crate::usage::Usage::new(&cfg.config_dir),

            cfg,
        })
    }

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
    pub(crate) async fn shown_storage_alert(&self) -> Option<StorageAlert> {
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
pub(crate) const PROBE_LIMIT: Duration = Duration::from_secs(5);

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
pub struct AppError(pub(crate) StatusCode, pub(crate) anyhow::Error);

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

/// Load the session list from `sessions.json`. A file we cannot read or parse as a list at all is
/// moved aside to `sessions.json.corrupt-<unix-timestamp>` — never overwritten, so its bytes stay
/// recoverable — and the harness starts with an empty list and a sticky alert: the colonies on that
/// list are missing from it although their worktrees, branches and microVMs may still exist. A list
/// where records are damaged — some or all of them — is salvaged instead: the good colonies load,
/// and the original is copied aside untouched, so the next startup can salvage from it again if the
/// harness stops before the next save. If even the aside fails, the next save would overwrite the
/// file, so that is an error rather than a degraded start.
pub(crate) fn load_sessions(path: &FsPath) -> Result<(Vec<Session>, Option<StorageAlert>)> {
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
        let boot = Boot {
            sessions: Vec::new(),
            modules,
            agents,
            agent_problems: Vec::new(),
            load_damage: None,
            // A throwaway token: these tests never bind a port, and each one reads the token it
            // needs off the App itself.
            api_token: crate::util::random_token(),
        };
        Arc::new(App::new(cfg, boot).unwrap())
    }

    pub(crate) fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-load-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }

    /// A session list with exactly the fields the format requires; everything else defaults.
    pub(crate) fn session_json() -> String {
        json!([record_json("abc123")]).to_string()
    }

    /// One colony record with exactly the fields the format requires, keyed by `id`.
    pub(crate) fn record_json(id: &str) -> Value {
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
