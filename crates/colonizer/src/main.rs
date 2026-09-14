//! Colonizer: turn a task into a pull request by running a coding agent in a microVM, with a
//! web UI for chat (questions as choice cards), a terminal in the VM, and a private mesh network
//! between the harness and every VM. Every moving part is a module; see docs/architecture.md.
//!
//! Trust model: a microVM only sees its worktree (rw), the repository's git objects (ro), its
//! session files (ro) and an output directory (rw). The GitHub token never enters a VM; the Claude
//! credential is injected by microsandbox's host-side TLS proxy for the API host only, and model
//! provider keys are added by the mothership's provider gateway.

mod claude_login;
mod config;
mod gateway;
mod github;
mod memory;
mod mesh;
mod modules;
mod orgs;
mod providers;
mod sandbox;
mod sessions;
mod util;
mod watchdog;

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use config::{setting_u64, ModulesConfig, Settings};
use mesh::{Mesh, Ports};
use modules::AgentModule;
use serde_json::{json, Value};
use sessions::Session;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path as FsPath, PathBuf},
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

pub struct App {
    pub cfg: Settings,
    pub modules: RwLock<ModulesConfig>,
    pub agents: Vec<AgentModule>,
    pub sessions: RwLock<Vec<Session>>,
    session_persist: Mutex<()>,
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
            let env = if token.starts_with("sk-ant-api") { "ANTHROPIC_API_KEY" } else { "CLAUDE_CODE_OAUTH_TOKEN" };
            let source = if env == "ANTHROPIC_API_KEY" { "saved API key" } else { "Claude subscription" };
            return Some(ClaudeCred { env, value: token, source });
        }
        if let Some(value) = env_nonempty("CLAUDE_CODE_OAUTH_TOKEN") {
            return Some(ClaudeCred { env: "CLAUDE_CODE_OAUTH_TOKEN", value, source: "CLAUDE_CODE_OAUTH_TOKEN" });
        }
        env_nonempty("ANTHROPIC_API_KEY").map(|value| ClaudeCred { env: "ANTHROPIC_API_KEY", value, source: "ANTHROPIC_API_KEY" })
    }

    pub async fn repo_lock(&self, repo: &str) -> Arc<Mutex<()>> {
        self.repo_locks.lock().await.entry(repo.to_string()).or_default().clone()
    }

    /// The mesh manager, created on first use from the bundled binaries and mesh module settings.
    pub async fn mesh(&self) -> Result<Arc<Mesh>> {
        let mut mesh = self.mesh.lock().await;
        if let Some(m) = mesh.as_ref() {
            return Ok(m.clone());
        }
        let assets = self.cfg.assets.clone().context("app assets not found: run scripts/install.sh")?;
        let modules = self.modules.read().await;
        let schema = modules::schema_for("mesh", "headscale", &self.agents);
        let port = |key: &str| u16::try_from(setting_u64(&modules.mesh, &schema, key)).unwrap_or_default();
        let ports = Ports { control: port("control_port"), udp: port("udp_port"), socks: port("socks_port") };
        let created = Arc::new(Mesh::new(&assets, &self.cfg.data_dir, &self.cfg.runtime_dir, ports));
        *mesh = Some(created.clone());
        Ok(created)
    }
}

/// Finds a native Claude Code binary on the host to mount read-only into microVMs.
pub async fn resolve_claude_bin(cfg: &Settings) -> Result<PathBuf> {
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
        let Ok(real) = std::fs::canonicalize(&candidate) else { continue };
        if !is_elf(&real) {
            continue;
        }
        if let Ok(version) = exec(Command::new(&real).arg("--version")).await {
            if version.contains("Claude Code") {
                return Ok(real);
            }
        }
    }
    bail!("no native Claude Code binary found; install Claude Code or set COLONIZER_CLAUDE_BIN")
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

async fn status(State(app): State<Shared>) -> Json<Value> {
    let mut msb = Command::new(&app.cfg.msb);
    msb.arg("--version");
    let (user, msb_version, claude_bin) = tokio::join!(github::viewer(&app), exec(&mut msb), resolve_claude_bin(&app.cfg));
    let cred = app.claude_cred();
    let modules = app.modules.read().await.clone();
    let mesh = if modules.mesh_enabled() {
        match app.mesh().await {
            Ok(mesh) => mesh.status().await,
            Err(e) => json!({"enabled": true, "provider": "headscale", "state": "error", "error": format!("{e:#}")}),
        }
    } else {
        json!({"enabled": false, "provider": "none"})
    };
    let sandbox_schema = modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let asset = |rel: &str| app.cfg.assets.as_ref().is_some_and(|a| a.join(rel).exists());
    Json(json!({
        "github": match user {
            Ok(u) => json!({"connected": true, "login": u["login"], "name": u["name"], "source": github::token_source(&app)}),
            Err(e) => json!({"connected": false, "error": format!("{e:#}")}),
        },
        "claude": {
            "configured": cred.is_some(),
            "source": cred.as_ref().map(|c| c.source),
            "kind": cred.as_ref().map(|c| c.env),
        },
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
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
    let hostname = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default().to_string()
    };
    let bind_host = app.cfg.bind.rsplit_once(':').map_or(app.cfg.bind.as_str(), |(h, _)| h);
    let allowed = matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || hostname == bind_host
        || app.cfg.allowed_hosts.iter().any(|h| *h == hostname);
    if !allowed {
        return (StatusCode::FORBIDDEN, "Host not allowed (set COLONIZER_ALLOWED_HOSTS)").into_response();
    }
    if req.method() != Method::GET || req.headers().contains_key(header::UPGRADE) {
        if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
            if origin.split("://").nth(1) != Some(host.as_str()) {
                return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
            }
        }
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

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Settings::from_env()?;
    for dir in ["sessions", "repos", "worktrees", "memory"] {
        std::fs::create_dir_all(cfg.data_dir.join(dir))?;
    }
    let mut sessions: Vec<Session> = std::fs::read(cfg.data_dir.join("sessions.json"))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default();
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
        runtimes: Mutex::new(HashMap::new()),
        repo_locks: Mutex::new(HashMap::new()),
        mesh: Mutex::new(None),
        login: Default::default(),
        memory: memory::MemoryStore::new(cfg.data_dir.join("memory")),
        gateway: gateway::Gateway::new()?,
        repo_owners: RwLock::new(BTreeSet::new()),
        orgs_refreshed: Mutex::new(None),
        cfg,
    });

    let api = Router::new()
        .route("/api/status", get(status))
        .route("/api/modules", get(modules::list))
        .route("/api/modules/{kind}", put(modules::update))
        .route("/api/settings/github-token", post(github::set_token).delete(github::delete_token))
        .route("/api/settings/claude-token", post(set_claude_token).delete(delete_claude_token))
        .route("/api/claude-login", get(claude_login::status))
        .route("/api/claude-login/start", post(claude_login::start))
        .route("/api/claude-login/code", post(claude_login::submit_code))
        .route("/api/claude-login/cancel", post(claude_login::cancel))
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
        .route("/api/repos", get(github::list_repos))
        .route("/api/repos/{owner}/{name}/issues", get(github::list_issues))
        .route("/api/sessions", get(sessions::list).post(sessions::create))
        .route("/api/sessions/{id}", get(sessions::get))
        .route("/api/sessions/{id}/publish", post(sessions::publish))
        .route("/api/sessions/{id}/stop", post(sessions::stop))
        .route("/api/sessions/{id}/cleanup", post(sessions::cleanup))
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
        Err(e) => eprintln!("provider gateway: cannot bind {}: {e}; colonies can't use model providers", app.cfg.gateway_bind),
    }

    let recovery = app.clone();
    tokio::spawn(async move { sessions::recover(&recovery).await });
    tokio::spawn(watchdog::run(app.clone()));
    if app.modules.read().await.mesh_enabled() && app.cfg.assets.is_some() {
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
            let mesh = app.mesh.lock().await.clone();
            if let Some(mesh) = mesh {
                mesh.shutdown().await;
            }
        }
    }
    Ok(())
}
