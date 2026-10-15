//! Living Brain (https://livingbrain.wiki): a team brain — wiki and memory — that colonies can
//! write their sessions into and read from while they work.
//!
//! Three things, all off until the operator turns them on:
//!
//! - **Connect** (Settings → Living Brain). The mothership runs the brain's OAuth device flow
//!   (RFC 8628) itself — the same `livingbrain-cli` client the brain's CLI uses — shows the code,
//!   and polls until the person approves it in a browser; or it takes a personal access token
//!   pasted from the brain's Settings → API tokens. The token is a saved secret like any provider
//!   key (`<config>/living-brain/token`: the keychain when it is usable, else a 0600 file, `.enc`
//!   under `COLONIZER_MASTER_KEY`), listed on the Secrets page, never returned by the API and
//!   scrubbed from everything the mothership logs. Disconnect revokes it at the brain
//!   (`DELETE /v1/tokens/{prefix}`) and forgets it here.
//! - **Send colony sessions** (queue.rs), per org or per repository, with a detail level. A colony
//!   that finished a turn and went idle, or ended, or opened or merged its pull request, becomes an
//!   `agent_log` source and a searchable note (record.rs), queued on disk and sent in the
//!   background with backoff — never in a colony's way.
//! - **Brain access for colonies.** Per org or repository, a colony boots with the brain's MCP
//!   server wired into its agent (Claude Code, Codex), the token reaching the guest only as an msb
//!   secret placeholder that is swapped for the value on TLS to the MCP host ([`colony_mcp`]).

pub(crate) mod client;
pub(crate) mod queue;
pub(crate) mod record;
#[cfg(test)]
mod tests;

use crate::{ApiResult, App, Shared, client_error, util};
use anyhow::Result;
use axum::{Json, extract::State, http::StatusCode};
use chrono::Utc;
use client::{Client, Poll};
use record::Detail;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "living_brain",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &[],
    start_tasks: Some(queue::start_tasks),
};

fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/living-brain", routing::get(get_state).put(put_settings))
        .route("/api/living-brain/backfill", routing::post(queue::backfill))
        .route("/api/living-brain/colonies/{id}", routing::get(queue::colony))
        .route("/api/living-brain/connect", routing::post(connect).delete(cancel_connect))
        .route("/api/living-brain/connection", routing::delete(disconnect))
        .route("/api/living-brain/queue/retry", routing::post(queue::retry))
        .route("/api/living-brain/token", routing::put(put_token))
}

const TARGET: crate::activity::Target = crate::activity::Target::Fixed("the Living Brain integration", "living-brain");

const ACTIVITY: &[crate::activity::Rule] = &[
    crate::activity::rule("PUT", "/api/living-brain", "settings.save", TARGET),
    crate::activity::rule("POST", "/api/living-brain/backfill", "settings.save", TARGET),
    crate::activity::rule("POST", "/api/living-brain/connect", "settings.save", TARGET),
    crate::activity::rule("DELETE", "/api/living-brain/connection", "settings.save", TARGET),
    crate::activity::rule("PUT", "/api/living-brain/token", "settings.save", TARGET),
];

pub(crate) const DEFAULT_SERVER: &str = "https://app.livingbrain.wiki";
pub(crate) const DEFAULT_MCP: &str = "https://mcp.livingbrain.wiki/v1/pages/mcp";
/// The env the token rides into a colony under, as an msb placeholder. Under the `COLONIZER_`
/// prefix a colony secret can never take (colony_secrets.rs), so no operator secret shadows it.
pub(crate) const MCP_TOKEN_ENV: &str = "COLONIZER_LIVINGBRAIN_TOKEN";
pub(crate) const MCP_URL_ENV: &str = "COLONIZER_LIVINGBRAIN_MCP_URL";
/// The agent modules whose runners know how to mount a remote MCP server with a bearer header.
pub(crate) const MCP_AGENTS: &[&str] = &["claude-code", "codex"];

/// One per-org or per-repository rule. A repository's rule wins over its org's; no rule is off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rule {
    /// `org` or `org/repo`, as typed (compared case-insensitively).
    pub(crate) target: String,
    /// Send this target's colony sessions to the brain.
    #[serde(default)]
    pub(crate) send: bool,
    #[serde(default)]
    pub(crate) detail: Detail,
    /// Give this target's colonies the brain's MCP server.
    #[serde(default)]
    pub(crate) mcp: bool,
    /// RFC3339: when `send` was switched on. Only colonies active since then go out on their own;
    /// older ones wait for the backfill button.
    #[serde(default)]
    pub(crate) since: Option<String>,
}

/// `<config>/living-brain.json`. Never a token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Saved {
    #[serde(default = "default_server")]
    pub(crate) server_url: String,
    #[serde(default = "default_mcp")]
    pub(crate) mcp_url: String,
    #[serde(default)]
    pub(crate) rules: Vec<Rule>,
    #[serde(default)]
    pub(crate) connected_at: Option<String>,
    /// The token's public half (`lbp_<id>`), for the status line and the revoke.
    #[serde(default)]
    pub(crate) token_prefix: Option<String>,
    /// The token's name at the brain, when `GET /v1/tokens` lists it.
    #[serde(default)]
    pub(crate) token_name: Option<String>,
    /// The brain's last 401: the token was revoked or expired. Cleared by a reconnect.
    #[serde(default)]
    pub(crate) auth_error: Option<String>,
}

fn default_server() -> String {
    DEFAULT_SERVER.into()
}
fn default_mcp() -> String {
    DEFAULT_MCP.into()
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            server_url: default_server(),
            mcp_url: default_mcp(),
            rules: Vec::new(),
            connected_at: None,
            token_prefix: None,
            token_name: None,
            auth_error: None,
        }
    }
}

impl Saved {
    /// The rule that decides for `repo` (`owner/name`): its own, else its org's.
    pub(crate) fn rule_for(&self, repo: &str) -> Option<&Rule> {
        let org = repo.split('/').next().unwrap_or("");
        self.rules.iter().find(|r| r.target.eq_ignore_ascii_case(repo)).or_else(|| {
            self.rules
                .iter()
                .find(|r| !r.target.contains('/') && r.target.eq_ignore_ascii_case(org))
        })
    }
}

pub(crate) fn state_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("living-brain.json")
}

/// Where the token is saved: through `util::{read,write,delete}_secret`, so the keychain or a 0600 file.
pub(crate) fn token_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("living-brain").join("token")
}

/// Every read-modify-write of `living-brain.json`. Never held across an await.
pub(crate) static SETTINGS: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub(crate) fn load(config_dir: &FsPath) -> Saved {
    match std::fs::read(state_file(config_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "living-brain: living-brain.json could not be parsed; reading it as not connected");
            Saved::default()
        }),
        Err(_) => Saved::default(),
    }
}

pub(crate) fn save(config_dir: &FsPath, saved: &Saved) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    queue::write_private_atomic(&state_file(config_dir), &serde_json::to_vec_pretty(saved)?)
}

/// Load, change, save — under [`SETTINGS`].
pub(crate) fn update(config_dir: &FsPath, change: impl FnOnce(&mut Saved)) -> Result<Saved> {
    let _lock = SETTINGS.lock().expect("the Living Brain settings lock");
    let mut saved = load(config_dir);
    change(&mut saved);
    save(config_dir, &saved)?;
    Ok(saved)
}

/// The saved token, when one is.
pub(crate) fn token(config_dir: &FsPath) -> Option<String> {
    util::read_secret(&token_file(config_dir))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// A brain URL the mothership may send the token to: `https://`, or `http://` to this machine
/// (a local brain under development). No userinfo, no query, no fragment.
fn valid_url(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if !matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
            return Err(format!("{url} must be https:// (plain http is only for localhost)"));
        }
        rest
    } else {
        return Err(format!("{url} must start with https://"));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || rest.contains(['@', '?', '#', ' ']) {
        return Err(format!("{url} is not a plain server address"));
    }
    Ok(url.to_owned())
}

/// The host of an `https://host[:port]/path` URL.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split('/').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn valid_target(target: &str) -> bool {
    let mut parts = target.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    };
    match (parts.next(), parts.next(), parts.next()) {
        (org, None, None) => ok(org),
        (org, repo, None) => ok(org) && ok(repo),
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// The device flow

/// The device authorization in flight, if any. The device code itself lives only in the poll task.
#[derive(Clone, Debug, Serialize)]
struct Pending {
    state: &'static str,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Bumped per connect, so an older poll task stops when a newer flow replaced it.
    #[serde(skip)]
    generation: u64,
}

static PENDING: LazyLock<Mutex<Option<Pending>>> = LazyLock::new(|| Mutex::new(None));

fn pending() -> Option<Pending> {
    PENDING.lock().expect("the Living Brain pending lock").clone()
}

fn set_pending_state(generation: u64, state: &'static str, error: Option<String>) {
    let mut guard = PENDING.lock().expect("the Living Brain pending lock");
    if let Some(p) = guard.as_mut().filter(|p| p.generation == generation) {
        p.state = state;
        p.error = error;
    }
}

fn still_current(generation: u64) -> bool {
    PENDING
        .lock()
        .expect("the Living Brain pending lock")
        .as_ref()
        .is_some_and(|p| p.generation == generation && p.state == "pending")
}

/// Saves a token the brain accepted and records whose it is. `GET /v1/tokens` is the one identity
/// read a bearer token is allowed; a 401 there means the token is no good and nothing is saved.
async fn adopt_token(app: &App, server: &str, token: String) -> Result<Saved, crate::AppError> {
    let client = Client::new(server, Some(token.clone()));
    let prefix = client::token_prefix(&token);
    let name = match client.tokens().await {
        Ok(listing) => prefix.as_deref().and_then(|prefix| {
            listing["tokens"]
                .as_array()?
                .iter()
                .find(|t| t["prefix"].as_str() == Some(prefix))
                .and_then(|t| t["name"].as_str().map(str::to_owned))
        }),
        Err(e) if e.unauthorized() => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                "Living Brain refused this token (401); create a new one and try again",
            ));
        }
        // The brain may be briefly down: the token is still the one the person gave or approved.
        Err(e) => {
            tracing::warn!(error = %e, "living-brain: the token could not be checked; saving it anyway");
            None
        }
    };
    util::write_secret(&token_file(&app.cfg.config_dir), &token)?;
    let saved = update(&app.cfg.config_dir, |s| {
        s.connected_at = Some(Utc::now().to_rfc3339());
        s.token_prefix = prefix;
        s.token_name = name;
        s.auth_error = None;
    })?;
    queue::wake();
    Ok(saved)
}

/// `POST /api/living-brain/connect`: starts the device flow at the brain and answers the code to
/// show and the page to open; a background task polls until it is approved, denied or expired.
pub async fn connect(State(app): State<Shared>) -> ApiResult<Value> {
    let saved = load(&app.cfg.config_dir);
    let server = saved.server_url.clone();
    let auth = Client::new(&server, None)
        .device_code()
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e.message))?;
    let text = |k: &str| auth.get(k).and_then(Value::as_str).map(str::to_owned);
    let (Some(device_code), Some(user_code), Some(verification_uri)) =
        (text("device_code"), text("user_code"), text("verification_uri"))
    else {
        return Err(client_error(
            StatusCode::BAD_GATEWAY,
            "Living Brain's device authorization answer was incomplete",
        ));
    };
    let expires_in = auth.get("expires_in").and_then(Value::as_u64).unwrap_or(600).clamp(60, 3600);
    let mut interval = auth.get("interval").and_then(Value::as_u64).unwrap_or(5).max(5);
    let generation = {
        let mut guard = PENDING.lock().expect("the Living Brain pending lock");
        let generation = guard.as_ref().map_or(1, |p| p.generation + 1);
        *guard = Some(Pending {
            state: "pending",
            user_code,
            verification_uri,
            verification_uri_complete: text("verification_uri_complete"),
            expires_at: (Utc::now() + chrono::Duration::seconds(expires_in as i64)).to_rfc3339(),
            error: None,
            generation,
        });
        generation
    };
    let task_app = app.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(expires_in);
        let poller = Client::new(&server, None);
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if !still_current(generation) {
                return; // cancelled, or replaced by a newer connect
            }
            if tokio::time::Instant::now() >= deadline {
                set_pending_state(generation, "expired", Some("the code expired before it was approved".into()));
                return;
            }
            match poller.device_token(&device_code).await {
                Ok(Poll::Pending) => {}
                Ok(Poll::SlowDown) => interval += 5,
                Ok(Poll::Refused(word)) => {
                    let state = if word == "access_denied" { "denied" } else { "expired" };
                    set_pending_state(generation, state, Some(word));
                    return;
                }
                Ok(Poll::Issued(token)) => {
                    match adopt_token(&task_app, &server, token).await {
                        Ok(_) => set_pending_state(generation, "connected", None),
                        Err(e) => set_pending_state(generation, "failed", Some(e.message())),
                    }
                    return;
                }
                // A transport blip is not the end of the flow; the deadline is.
                Err(e) if e.status.is_none() || e.status.is_some_and(|s| s >= 500) => {}
                Err(e) => {
                    set_pending_state(generation, "failed", Some(e.message));
                    return;
                }
            }
        }
    });
    Ok(Json(view(&app).await))
}

/// `DELETE /api/living-brain/connect`: forgets the device flow in flight.
pub async fn cancel_connect(State(app): State<Shared>) -> ApiResult<Value> {
    *PENDING.lock().expect("the Living Brain pending lock") = None;
    Ok(Json(view(&app).await))
}

#[derive(Deserialize)]
pub struct TokenRequest {
    token: String,
}

/// `PUT /api/living-brain/token {"token": "lbp_…"}`: a personal access token made at the brain's
/// Settings → API tokens, checked against the brain before it is saved.
pub async fn put_token(State(app): State<Shared>, Json(body): Json<TokenRequest>) -> ApiResult<Value> {
    let token = body.token.trim().to_owned();
    if token.is_empty() || token.contains(char::is_whitespace) || token.len() > 4096 {
        return Err(client_error(StatusCode::BAD_REQUEST, "paste the token as one line"));
    }
    let server = load(&app.cfg.config_dir).server_url;
    adopt_token(&app, &server, token).await?;
    *PENDING.lock().expect("the Living Brain pending lock") = None;
    Ok(Json(view(&app).await))
}

/// `DELETE /api/living-brain/connection`: revokes the token at the brain (best effort — the answer
/// says when the brain could not be told), forgets it here, and empties the queue.
pub async fn disconnect(State(app): State<Shared>) -> ApiResult<Value> {
    let saved = load(&app.cfg.config_dir);
    let mut revoke_error = None;
    if let Some(token) = token(&app.cfg.config_dir) {
        let prefix = saved.token_prefix.clone().or_else(|| client::token_prefix(&token));
        match prefix {
            Some(prefix) => {
                if let Err(e) = Client::new(&saved.server_url, Some(token)).revoke(&prefix).await
                    && !e.unauthorized()
                    && e.status != Some(404)
                {
                    revoke_error = Some(e.message);
                }
            }
            None => {
                revoke_error = Some("the token is not a Living Brain personal access token, so it was only forgotten here".into())
            }
        }
    }
    util::delete_secret(&token_file(&app.cfg.config_dir));
    update(&app.cfg.config_dir, |s| {
        s.connected_at = None;
        s.token_prefix = None;
        s.token_name = None;
        s.auth_error = None;
    })?;
    queue::clear(&app.cfg.config_dir)?;
    *PENDING.lock().expect("the Living Brain pending lock") = None;
    let mut out = view(&app).await;
    out["revoke_error"] = json!(revoke_error);
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct SettingsRequest {
    server_url: Option<String>,
    mcp_url: Option<String>,
    rules: Option<Vec<Rule>>,
}

/// `PUT /api/living-brain {server_url?, mcp_url?, rules?}`. The server and MCP addresses change
/// only while disconnected — the token is never sent to a host it was not issued by. Rules are
/// replaced whole; switching a target's `send` on stamps `since`.
pub async fn put_settings(State(app): State<Shared>, Json(body): Json<SettingsRequest>) -> ApiResult<Value> {
    let connected = token(&app.cfg.config_dir).is_some();
    let current = load(&app.cfg.config_dir);
    let server = match &body.server_url {
        Some(url) => Some(valid_url(url).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?),
        None => None,
    };
    let mcp = match &body.mcp_url {
        Some(url) => Some(valid_url(url).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?),
        None => None,
    };
    let moved = server.as_ref().is_some_and(|s| *s != current.server_url) || mcp.as_ref().is_some_and(|m| *m != current.mcp_url);
    if moved && connected {
        return Err(client_error(
            StatusCode::CONFLICT,
            "disconnect first: the token stays with the server that issued it",
        ));
    }
    if let Some(rules) = &body.rules {
        if rules.len() > 500 {
            return Err(client_error(StatusCode::BAD_REQUEST, "at most 500 rules"));
        }
        for rule in rules {
            if !valid_target(&rule.target) {
                return Err(client_error(
                    StatusCode::BAD_REQUEST,
                    &format!("{:?} is not an org or an owner/repo", rule.target),
                ));
            }
        }
    }
    let now = Utc::now().to_rfc3339();
    update(&app.cfg.config_dir, |s| {
        if let Some(server) = server {
            s.server_url = server;
        }
        if let Some(mcp) = mcp {
            s.mcp_url = mcp;
        }
        if let Some(rules) = body.rules {
            let mut next: Vec<Rule> = Vec::new();
            for mut rule in rules {
                rule.target = rule.target.trim().to_owned();
                if next.iter().any(|r| r.target.eq_ignore_ascii_case(&rule.target)) {
                    continue;
                }
                let before = s.rules.iter().find(|r| r.target.eq_ignore_ascii_case(&rule.target));
                rule.since = match (rule.send, before) {
                    (false, _) => None,
                    (true, Some(b)) if b.send => b.since.clone(),
                    (true, _) => Some(now.clone()),
                };
                next.push(rule);
            }
            next.sort_by_key(|r| r.target.to_ascii_lowercase());
            s.rules = next;
        }
    })?;
    queue::wake();
    Ok(Json(view(&app).await))
}

/// The settings, the connection and the queue — never the token.
pub(crate) async fn view(app: &App) -> Value {
    let saved = load(&app.cfg.config_dir);
    let connected = token(&app.cfg.config_dir).is_some();
    let state = if !connected {
        "disconnected"
    } else if saved.auth_error.is_some() {
        "unauthorized"
    } else {
        "connected"
    };
    json!({
        "state": state,
        "server_url": saved.server_url,
        "mcp_url": saved.mcp_url,
        "connected_at": if connected { saved.connected_at.clone() } else { None },
        "token_prefix": if connected { saved.token_prefix.clone() } else { None },
        "token_name": if connected { saved.token_name.clone() } else { None },
        "auth_error": saved.auth_error,
        "device": pending(),
        "rules": saved.rules,
        "mcp_agents": MCP_AGENTS,
        "queue": queue::summary(&app.cfg.config_dir),
    })
}

/// `GET /api/living-brain`.
pub async fn get_state(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(view(&app).await))
}

/// The brain's MCP server for one colony, when its repository's rule asks for it and the agent can
/// mount it: the secret msb holds (the token, swapped in only on TLS to the MCP host) and the URL
/// the runner registers. `Err` names why a colony whose rule asks for the server does not get it.
pub(crate) fn colony_mcp(app: &App, repo: &str, agent: &str) -> Option<Result<(crate::sandbox::Secret, String), String>> {
    let saved = load(&app.cfg.config_dir);
    if !saved.rule_for(repo)?.mcp {
        return None;
    }
    Some(mcp_grant(&saved, token(&app.cfg.config_dir), agent))
}

pub(crate) fn mcp_grant(saved: &Saved, token: Option<String>, agent: &str) -> Result<(crate::sandbox::Secret, String), String> {
    if !MCP_AGENTS.contains(&agent) {
        return Err(format!("the {agent} agent module cannot mount a remote MCP server yet"));
    }
    let Some(token) = token else {
        return Err("Living Brain is not connected".into());
    };
    if saved.auth_error.is_some() {
        return Err("Living Brain refused the saved token; reconnect in Settings".into());
    }
    let Some(host) = url_host(&saved.mcp_url) else {
        return Err(format!("{} names no host", saved.mcp_url));
    };
    Ok((
        crate::sandbox::Secret {
            env: MCP_TOKEN_ENV.into(),
            value: token,
            hosts: vec![host],
        },
        saved.mcp_url.clone(),
    ))
}
