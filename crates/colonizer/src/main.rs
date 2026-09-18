//! Colonizer: turn a task into a pull request by running a coding agent in a microVM, with a
//! web UI for chat (questions as choice cards), a terminal in the VM, and a private mesh network
//! between the harness and every VM. Every moving part is a module; see docs/architecture.md.
//!
//! Trust model: a microVM only sees its worktree (rw), the repository's git objects (ro), its
//! session files (ro) and an output directory (rw). The GitHub token never enters a VM; the Claude
//! credential is injected by microsandbox's host-side TLS proxy for the API host only, and model
//! provider keys are added by the mothership's provider gateway.

mod autonomy;
mod claude_login;
mod config;
mod events;
mod findings;
mod gateway;
mod github;
mod headroom;
mod lifecycle;
mod mem0;
mod memory;
mod mesh;
mod modules;
mod openai;
mod orgs;
mod plugins;
mod presets;
mod protocol;
mod providers;
mod publish;
mod queue;
mod sandbox;
mod sessions;
mod telemetry;
mod timing;
mod update;
mod usage;
mod util;
mod version;
mod watchdog;

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post, put},
};
use chrono::{DateTime, Utc};
use config::{ModulesConfig, Settings, setting_u64};
use mesh::{Mesh, Ports};
use modules::AgentModule;
use serde_json::{Value, json};
use sessions::Session;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path as FsPath, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
};
use tower_http::services::{ServeDir, ServeFile};
use util::{env_nonempty, exec, is_elf, read_trimmed};

pub const CLAUDE_API_HOST: &str = "api.anthropic.com";

const UI_MISSING_HTML: &str = "<!doctype html><title>Colonizer</title>\
<body style=\"font:15px system-ui;margin:3rem\"><h1>Colonizer is running</h1>\
<p>The web UI isn't built yet. Run <code>scripts/install.sh</code> (or <code>npm run build</code> in <code>web/</code>).</p>";

pub struct ClaudeCred {
    pub env: &'static str,
    pub value: String,
    pub source: &'static str,
}

/// The latest confirmed storage failure, shown by the UI until it is dismissed. Sticky on purpose:
/// a later successful write does not clear it, because the gap the alert reports did happen.
#[derive(Clone, Debug)]
pub struct StorageAlert {
    pub message: String,
    pub ts: DateTime<Utc>,
    pub failures: u64,
}

pub struct App {
    pub cfg: Settings,
    pub modules: RwLock<ModulesConfig>,
    pub agents: Vec<AgentModule>,
    pub sessions: RwLock<Vec<Session>>,
    session_persist: Mutex<()>,
    pub storage_alert: RwLock<Option<StorageAlert>>,
    pub runtimes: Mutex<HashMap<String, Arc<sessions::Runtime>>>,
    repo_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    mesh: Mutex<Option<Arc<Mesh>>>,
    pub login: claude_login::LoginManager,
    pub memory: memory::MemoryStore,
    pub gateway: gateway::Gateway,
    /// Owners seen in the repository list, so org workspaces can be offered before any colony exists.
    pub repo_owners: RwLock<BTreeSet<String>>,
    /// When the user's GitHub orgs were last fetched.
    pub orgs_refreshed: Mutex<Option<std::time::Instant>>,
    /// The last Anthropic profile lookup for the Claude credential, cached so the status poll does not
    /// hammer Anthropic. Keyed on a fingerprint of the token; the token itself is never stored.
    pub claude_account: Mutex<Option<claude_login::AccountStatus>>,
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
        if let Some(token) = read_trimmed(&self.claude_token_file()) {
            let env = if token.starts_with("sk-ant-api") {
                "ANTHROPIC_API_KEY"
            } else {
                "CLAUDE_CODE_OAUTH_TOKEN"
            };
            let source = if env == "ANTHROPIC_API_KEY" {
                "saved API key"
            } else {
                "Claude subscription"
            };
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

    /// Records a confirmed storage failure: printed loudly here, kept as the sticky alert the UI
    /// shows, and counted so a run of failures reads as more than one.
    pub async fn storage_failed(&self, what: &str, err: &anyhow::Error) {
        eprintln!("storage: {what}: {err:#}");
        let mut alert = self.storage_alert.write().await;
        let failures = alert.as_ref().map_or(0, |a| a.failures) + 1;
        *alert = Some(StorageAlert {
            message: format!("{what} failed: {err:#}"),
            ts: Utc::now(),
            failures,
        });
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

/// The Claude Code binary mounted read-only into a colony. A colony is a Linux microVM, so this has
/// to be a Linux build: on a Mac the host's own is Mach-O and `scripts/install.sh` fetches one
/// beside the app instead.
pub async fn resolve_guest_claude_bin(cfg: &Settings) -> Result<PathBuf> {
    if let Ok(guest) = cfg.asset("bin/claude-guest") {
        return Ok(guest);
    }
    find_claude_bin(cfg, true)
        .await
        .context("no Linux Claude Code binary found for the guest; run scripts/install.sh or set COLONIZER_CLAUDE_BIN")
}

/// The Claude Code binary the mothership runs itself, for `claude setup-token`. Never the guest's:
/// on a Mac that one is a Linux ELF, and the host cannot execute it.
pub async fn resolve_host_claude_bin(cfg: &Settings) -> Result<PathBuf> {
    find_claude_bin(cfg, false)
        .await
        .context("no native Claude Code binary found; install Claude Code or set COLONIZER_CLAUDE_BIN")
}

/// Walks the usual install locations and returns the first one that answers `--version`. `elf_only`
/// is the guest's requirement; for the host, being able to run it at all is the test.
async fn find_claude_bin(cfg: &Settings, elf_only: bool) -> Result<PathBuf> {
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
    for candidate in candidates {
        let Ok(real) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if elf_only && !is_elf(&real) {
            continue;
        }
        if let Ok(version) = exec(Command::new(&real).arg("--version")).await
            && version.contains("Claude Code")
        {
            return Ok(real);
        }
    }
    bail!("no Claude Code binary found")
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

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

pub type ApiResult<T> = Result<Json<T>, AppError>;

/// The `storage` key of `/api/status`: `ok` while every write was confirmed, else the sticky alert
/// (`ts` in the same RFC 3339 form the `harness_log` frames use).
fn storage_status(alert: Option<StorageAlert>) -> Value {
    match alert {
        None => json!({"ok": true}),
        Some(alert) => json!({"ok": false, "message": alert.message, "ts": alert.ts, "failures": alert.failures}),
    }
}

async fn status(State(app): State<Shared>) -> Json<Value> {
    let mut msb = Command::new(&app.cfg.msb);
    msb.arg("--version");
    let cred = app.claude_cred();
    let (user, msb_version, claude_bin, claude) = tokio::join!(
        github::viewer(&app),
        exec(&mut msb),
        resolve_guest_claude_bin(&app.cfg),
        claude_login::claude_status(&app, cred.as_ref()),
    );
    let modules = app.modules.read().await.clone();
    let mesh = if !modules.mesh_enabled() {
        json!({"enabled": false, "provider": "none"})
    } else if !app.cfg.assets.as_deref().is_some_and(mesh::binaries_present) {
        // Not an error the operator can clear: this platform has no mesh binaries to
        // vendor. It goes in `detail`, not `error`: anything in `error` is read as a
        // fault, and this one used to paint every Mac's runtime red.
        json!({"enabled": true, "provider": "headscale", "state": "unavailable",
               "detail": "colonies use a loopback port on this platform", "error": Value::Null})
    } else {
        match app.mesh().await {
            Ok(mesh) => mesh.status().await,
            Err(e) => json!({"enabled": true, "provider": "headscale", "state": "error", "error": format!("{e:#}")}),
        }
    };
    let sandbox_schema = modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let asset = |rel: &str| app.cfg.assets.as_ref().is_some_and(|a| a.join(rel).exists());
    let storage_alert = app.storage_alert.read().await.clone();
    Json(json!({
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
        "storage": storage_status(storage_alert),
        "modules": {
            "source": modules.source.provider,
            "sandbox": modules.sandbox.provider,
            "mesh": if modules.mesh.enabled { modules.mesh.provider.as_str() } else { "none" },
            "agent": modules.agent.provider,
            "publish": modules.publish.provider,
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
    let _ = std::fs::remove_file(app.claude_token_file());
    Ok(Json(json!({"ok": true})))
}

/// Rejects DNS rebinding (unexpected Host) and cross-origin writes or WebSocket upgrades; the API has
/// no other authentication, so it binds to loopback by default.
async fn host_guard(State(app): State<Shared>, req: Request, next: Next) -> Response {
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
    if (req.method() != Method::GET || req.headers().contains_key(header::UPGRADE))
        && let Some(origin) = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok())
        && origin.split("://").nth(1) != Some(host.as_str())
    {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }
    next.run(req).await
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
        Some(dir) => Router::new().fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => Router::new().fallback(|| async { Html(UI_MISSING_HTML) }),
    }
}

/// Load the session list from `sessions.json`. A file we cannot read or parse is moved aside to
/// `sessions.json.corrupt-<unix-timestamp>` — never overwritten, so its bytes stay recoverable —
/// and the harness starts with an empty list and a sticky alert: the colonies on that list are
/// missing from it although their worktrees, branches and microVMs may still exist. If even the
/// move-aside fails, the next save would overwrite the file, so that is an error rather than a
/// degraded start.
fn load_sessions(path: &FsPath) -> Result<(Vec<Session>, Option<StorageAlert>)> {
    let reason = match std::fs::read(path) {
        // A missing file is a first run, not a corruption.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => format!("could not be read ({e})"),
        Ok(data) => match serde_json::from_slice::<Vec<Session>>(&data) {
            Ok(sessions) => return Ok((sessions, None)),
            Err(e) => format!("could not be parsed ({e})"),
        },
    };
    let saved = move_corrupt_aside(path)?;
    let message = format!(
        "{} {reason} and was saved as {}; colonies are missing from the list, although their worktrees, branches and microVMs may still exist",
        path.display(),
        saved.display()
    );
    eprintln!("sessions: {message}");
    Ok((
        Vec::new(),
        Some(StorageAlert {
            message,
            ts: Utc::now(),
            failures: 1,
        }),
    ))
}

/// Moves a `sessions.json` the harness cannot use aside, into the same directory, so its bytes survive.
fn move_corrupt_aside(path: &FsPath) -> Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sessions.json".into());
    let saved = path.with_file_name(format!("{name}.corrupt-{stamp}"));
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

const USAGE: &str = "colonizer — turn a task into a pull request; see https://colonizer.dev/docs

usage: colonizer
       colonizer version | update
       colonizer telemetry show|on|off

  (no arguments)  start the mothership and serve the web UI (default 127.0.0.1:7878)
  version         print what this build is, and whether it is a release (also --version, -V)
  update          install the newest release against a running mothership and restart into it
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
    Update,
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
            "update" => Self::Update,
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
            Self::Update => update::command().await,
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
    let modules = ModulesConfig::load(&cfg.config_dir.join("modules.json"));
    let agents = modules::discover_agents(cfg.assets.as_deref());

    let app = Arc::new(App {
        modules: RwLock::new(modules),
        agents,
        sessions: RwLock::new(sessions),
        session_persist: Mutex::new(()),
        storage_alert: RwLock::new(corrupt),
        runtimes: Mutex::new(HashMap::new()),
        repo_locks: Mutex::new(HashMap::new()),
        mesh: Mutex::new(None),
        login: Default::default(),
        memory: memory::MemoryStore::new(cfg.data_dir.join("memory")),
        gateway: gateway::Gateway::new(&cfg.data_dir)?,
        repo_owners: RwLock::new(BTreeSet::new()),
        orgs_refreshed: Mutex::new(None),
        claude_account: Mutex::new(None),
        pull: Mutex::new(Default::default()),
        headroom: Mutex::new(Default::default()),
        telemetry: telemetry::Telemetry::new(&cfg.config_dir)?,
        updates: version::Updates::new(&cfg.config_dir)?,
        updater: update::Updater::new(),
        usage: usage::Usage::new(&cfg.config_dir),
        cfg,
    });

    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/modules", get(modules::list))
        .route("/api/modules/{kind}", put(modules::update))
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
        .route("/api/sandbox/pull", post(sandbox::pull_configured).get(sandbox::pull_status))
        .route("/api/headroom", get(headroom::status))
        .route("/api/headroom/download", post(headroom::download))
        .route("/api/telemetry", get(telemetry::status).put(telemetry::put))
        .route("/api/version", get(version::version))
        .route("/api/update", get(version::status).put(version::put))
        .route("/api/update/apply", post(update::apply))
        .route("/api/telemetry/usage", get(usage::status).put(usage::put))
        .route("/api/plugins", get(plugins::list))
        .route("/api/providers", get(providers::list))
        .route("/api/providers/{id}", put(providers::put).delete(providers::delete))
        .route("/api/providers/{id}/health", get(gateway::health))
        .route("/api/models", get(providers::models))
        .route("/api/orgs", get(orgs::list))
        .route("/api/orgs/{org}", put(orgs::put))
        .route("/api/memory", get(memory::get))
        .route("/api/memory/proposals", get(memory::list_proposals))
        .route("/api/memory/proposals/{id}/approve", post(memory::approve))
        .route("/api/memory/proposals/{id}/reject", post(memory::reject))
        .route("/api/memory/notes", post(memory::create_note))
        .route("/api/memory/notes/{id}", delete(memory::delete_note))
        .route("/api/memory/mem0", get(memory::mem0_status).put(memory::put_mem0_key))
        .route("/api/memory/mem0/check", post(memory::check_mem0))
        .route("/api/repos", get(github::list_repos))
        .route("/api/repos/{owner}/{name}/issues", get(github::list_issues))
        .route("/api/sessions", get(sessions::list).post(sessions::create))
        .route("/api/sessions/{id}", get(sessions::get).delete(lifecycle::delete))
        .route("/api/sessions/{id}/resume", post(lifecycle::resume))
        .route("/api/sessions/{id}/publish", post(publish::publish))
        .route("/api/sessions/{id}/stop", post(lifecycle::stop))
        .route("/api/sessions/{id}/cleanup", post(lifecycle::cleanup))
        .route("/api/sessions/{id}/events", get(sessions::events_ws))
        .route("/api/sessions/{id}/terminal", get(sessions::terminal_ws));
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
    // The first-run notice: once, while nobody has answered yet, show the exact usage batch on stderr.
    usage::first_run_notice(&app).await;
    match tokio::net::TcpListener::bind(&app.cfg.gateway_bind).await {
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
    let disk_watch = app.clone();
    tokio::spawn(async move { lifecycle::watch_host_disks(disk_watch).await });
    let pr_watch = app.clone();
    tokio::spawn(async move { publish::watch_pull_requests(pr_watch).await });
    tokio::spawn(watchdog::run(app.clone()));
    tokio::spawn(autonomy::run(app.clone()));
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
        std::fs::create_dir_all(root.join("data")).unwrap();
        Arc::new(App {
            cfg: Settings {
                bind: "127.0.0.1:0".into(),
                data_dir: root.join("data"),
                config_dir: root.join("config"),
                runtime_dir: root.join("run"),
                assets: None,
                msb: "msb".into(),
                claude_bin: None,
                gateway_bind: "127.0.0.1:0".into(),
                allowed_hosts: Vec::new(),
            },
            modules: RwLock::new(ModulesConfig::load(&root.join("config/modules.json"))),
            agents: Vec::new(),
            sessions: RwLock::new(Vec::new()),
            session_persist: Mutex::new(()),
            storage_alert: RwLock::new(None),
            runtimes: Mutex::new(HashMap::new()),
            repo_locks: Mutex::new(HashMap::new()),
            mesh: Mutex::new(None),
            login: Default::default(),
            memory: memory::MemoryStore::new(root.join("memory")),
            // Added on main while this branch was open; kept in step with the real constructor.
            claude_account: Mutex::new(None),
            usage: usage::Usage::new(&root.join("config")),
            updates: version::Updates::new(&root.join("config")).unwrap(),
            updater: update::Updater::new(),
            gateway: gateway::Gateway::new(&root.join("data")).unwrap(),
            repo_owners: RwLock::new(BTreeSet::new()),
            orgs_refreshed: Mutex::new(None),
            pull: Mutex::new(Default::default()),
            headroom: Mutex::new(Default::default()),
            telemetry: telemetry::Telemetry::new(&root.join("config")).unwrap(),
        })
    }

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-load-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }

    /// A session list with exactly the fields the format requires; everything else defaults.
    fn session_json() -> String {
        json!([{
            "id": "abc123",
            "repo": "acme/app",
            "issue_title": "Fix the deploy",
            "status": "idle",
            "branch": "colonizer/issue-1-abc123",
            "worktree": "/colonizer/worktrees/acme/app/issue-1-abc123",
            "sandbox": "colonizer-abc123",
            "agent": "claude",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }])
        .to_string()
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
        assert_eq!(storage_status(None), json!({"ok": true}));
        let alert = StorageAlert {
            message: "save the session list failed: disk is full".into(),
            ts: Utc::now(),
            failures: 3,
        };
        let value = storage_status(Some(alert));
        assert_eq!(value["ok"], false);
        assert_eq!(value["message"], "save the session list failed: disk is full");
        assert_eq!(value["failures"], 3);
        assert!(
            value["ts"].is_string(),
            "the ts is the RFC 3339 string the harness_log frames use: {value}"
        );
    }
}
