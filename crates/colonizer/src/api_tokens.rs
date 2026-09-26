//! Scoped API tokens (issue #508): named, least-privilege keys the maintainer hands to CLIs and
//! automations, so they can drive the mothership without holding the per-install owner token.
//!
//! A token carries an ordered scope — `read` < `operate` < `launch` — optional org and repo limits
//! (empty lists mean no limit), and optional launch caps: the most colonies it may keep unfinished,
//! and the most model spend its colonies may run up per UTC day. The registry lives at
//! `<config_dir>/api-tokens.json` and stores only a SHA-256 hash of each token: the plaintext is
//! returned once at creation and never again, by this process or by the file.
//!
//! `host_guard` (main.rs) accepts a scoped token as `Authorization: Bearer` only — a browser never
//! holds one, so the `colonizer_token` cookie stays owner-only — and [`authorize`] decides the
//! route: anything outside the scope's allowlist is a 403 naming the scope, and a colony-scoped
//! route for a colony outside the token's org/repo limits is a 404, the same answer an unknown id
//! gets, so the token learns nothing beyond what it was granted. Launch routes check the requested
//! repository in their handler, where the body is parsed ([`ScopedToken::covers`]).

use crate::{
    ApiResult, App, Shared, client_error,
    sessions::Session,
    util::{short_id, valid_repo},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// How much a token may do, ordered so `token.scope >= needed` reads as "may". `read` watches,
/// `operate` drives colonies that exist (answer, stop, resume), `launch` starts colonies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Operate,
    Launch,
}

impl Scope {
    /// The wire spelling, shared by the registry file and the API answers.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Operate => "operate",
            Scope::Launch => "launch",
        }
    }

    /// Parses the scope a create request names. Manual, so a bad one is refused with the
    /// vocabulary in the message rather than a deserialization error.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "read" => Some(Scope::Read),
            "operate" => Some(Scope::Operate),
            "launch" => Some(Scope::Launch),
            _ => None,
        }
    }
}

/// A token as persisted: metadata plus the hash. The plaintext is never here.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    id: String,
    name: String,
    scope: Scope,
    #[serde(default)]
    orgs: Vec<String>,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_concurrent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget_usd_per_day: Option<f64>,
    created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_at: Option<DateTime<Utc>>,
    /// SHA-256 of the plaintext token, hex. The hash is what makes the plaintext disposable.
    token_hash: String,
}

impl Stored {
    /// The view `GET /api/tokens` answers with: everything but the hash.
    fn meta(&self) -> TokenMeta {
        TokenMeta {
            id: self.id.clone(),
            name: self.name.clone(),
            scope: self.scope,
            orgs: self.orgs.clone(),
            repos: self.repos.clone(),
            max_concurrent: self.max_concurrent,
            budget_usd_per_day: self.budget_usd_per_day,
            created_at: self.created_at,
            last_used_at: self.last_used_at,
        }
    }

    /// What the middleware attaches to an authenticated request, and the handlers read.
    fn scoped(&self) -> ScopedToken {
        ScopedToken {
            id: self.id.clone(),
            name: self.name.clone(),
            scope: self.scope,
            orgs: self.orgs.clone(),
            repos: self.repos.clone(),
            max_concurrent: self.max_concurrent,
            budget_usd_per_day: self.budget_usd_per_day,
        }
    }

    /// Records a use, throttled: the stamp only moves once a minute, so a busy token does not make
    /// every request rewrite it, and the file is never rewritten on the serving path at all —
    /// in-memory on purpose. A restart loses the `last_used_at` of the final minutes, which is the
    /// honest price of not touching disk per request.
    fn touch(&mut self) {
        let now = Utc::now();
        if self
            .last_used_at
            .is_none_or(|at| (now - at).num_seconds() >= LAST_USED_THROTTLE_SECS)
        {
            self.last_used_at = Some(now);
        }
    }
}

/// One token as `GET /api/tokens` answers: metadata only, never the hash and never the plaintext.
#[derive(Clone, Debug, Serialize)]
pub struct TokenMeta {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub orgs: Vec<String>,
    pub repos: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_usd_per_day: Option<f64>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

/// The authenticated scoped token, carried as a request extension from `host_guard` to the
/// handlers that must know who is acting: the launch checks in `sessions::create`, the list
/// filter in `sessions::list`, and `GET /api/tokens/self`.
#[derive(Clone, Debug)]
pub struct ScopedToken {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    pub orgs: Vec<String>,
    pub repos: Vec<String>,
    pub max_concurrent: Option<u32>,
    pub budget_usd_per_day: Option<f64>,
}

/// Seconds a `last_used_at` stamp must age before it is refreshed in memory (see [`Stored::touch`]).
const LAST_USED_THROTTLE_SECS: i64 = 60;
/// A token name is a label, not a value: long enough to be descriptive, short enough for a log line.
const MAX_NAME: usize = 120;

impl ScopedToken {
    /// Whether this token's org/repo limits leave a colony of this org and repo within them. Both
    /// limits are conjunctions, and each empty list means no limit of that kind.
    pub(crate) fn covers(&self, org: &str, repo: &str) -> bool {
        (self.orgs.is_empty() || self.orgs.iter().any(|o| o == org))
            && (self.repos.is_empty() || self.repos.iter().any(|r| r == repo))
    }
}

/// The body of `POST /api/tokens`. `scope` stays a string until validated, so a bad one is refused
/// with the vocabulary in the message; the optional limit fields are validated the same way.
#[derive(Deserialize)]
pub struct NewToken {
    pub name: String,
    pub scope: String,
    #[serde(default)]
    pub orgs: Vec<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub max_concurrent: Option<u32>,
    #[serde(default)]
    pub budget_usd_per_day: Option<f64>,
}

/// What `POST /api/tokens` answers: the plaintext, shown exactly once, next to the metadata.
#[derive(Serialize)]
pub struct CreatedToken {
    pub token: String,
    #[serde(flatten)]
    pub meta: TokenMeta,
}

/// The registry: every scoped token this install has, in memory, written through to
/// `<config_dir>/api-tokens.json` on each change (a creation or a revocation — never a request).
pub struct Registry {
    path: PathBuf,
    tokens: tokio::sync::RwLock<Vec<Stored>>,
}

/// SHA-256 of a token, hex. Stored rather than the plaintext, so neither the file nor a leak of it
/// hands over a working credential.
fn hash_token(token: &str) -> String {
    crate::util::hex(ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref())
}

/// The registry file, next to the other saved credentials in the config dir.
pub fn file(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("api-tokens.json")
}

impl Registry {
    /// Loads the registry, never failing: a file that cannot be read or parsed is reported on
    /// stderr and answers an empty registry, which refuses every scoped token — the safe direction
    /// for a damaged credential store. The file is 0600 (`write_private`), like the owner token
    /// beside it.
    pub fn load(config_dir: &std::path::Path) -> Registry {
        let path = file(config_dir);
        let tokens = match std::fs::read(&path) {
            // A missing file is a first use, not a fault.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                eprintln!(
                    "api-tokens: could not read {} ({e}); refusing every scoped API token",
                    path.display()
                );
                Vec::new()
            }
            Ok(bytes) => match serde_json::from_slice::<Vec<Stored>>(&bytes) {
                Ok(tokens) => tokens,
                Err(e) => {
                    eprintln!(
                        "api-tokens: {} does not parse ({e}); refusing every scoped API token until the file is repaired or removed",
                        path.display()
                    );
                    Vec::new()
                }
            },
        };
        Registry {
            path,
            tokens: tokio::sync::RwLock::new(tokens),
        }
    }

    /// Writes the registry through to disk. A failure is loud but not fatal: the tokens just made
    /// or revoked live in memory for this run, and the next change retries the write.
    async fn save(&self, tokens: &[Stored]) {
        let Ok(line) = serde_json::to_vec_pretty(tokens) else {
            return; // a fixed-shape list cannot fail to serialize
        };
        if let Err(e) = crate::util::write_private(&self.path, &line) {
            eprintln!("api-tokens: could not save {}: {e}", self.path.display());
        }
    }

    /// Mints a token: validates, stores only the hash, and returns the plaintext exactly once.
    pub async fn create(&self, req: NewToken) -> Result<CreatedToken, String> {
        let name = req.name.trim();
        if name.is_empty() {
            return Err("name is required".to_string());
        }
        if name.len() > MAX_NAME {
            return Err(format!("name is {} characters; keep it under {MAX_NAME}", name.len()));
        }
        let scope = Scope::parse(&req.scope).ok_or_else(|| "scope must be one of: read, operate, launch".to_string())?;
        let orgs = clean_orgs(&req.orgs)?;
        let repos = clean_repos(&req.repos)?;
        if req.max_concurrent.is_some_and(|max| max == 0) {
            return Err("max_concurrent must be at least 1".to_string());
        }
        if req
            .budget_usd_per_day
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err("budget_usd_per_day must be a positive number of dollars".to_string());
        }
        let plaintext = format!("col_{}", crate::util::random_token());
        let stored = Stored {
            id: format!("tok_{}", short_id()),
            name: name.to_string(),
            scope,
            orgs,
            repos,
            max_concurrent: req.max_concurrent,
            budget_usd_per_day: req.budget_usd_per_day,
            created_at: Utc::now(),
            last_used_at: None,
            token_hash: hash_token(&plaintext),
        };
        let meta = stored.meta();
        let mut tokens = self.tokens.write().await;
        tokens.push(stored);
        self.save(&tokens).await;
        Ok(CreatedToken { token: plaintext, meta })
    }

    /// Removes a token; `None` when no token carries the id. Presentations of the revoked token
    /// stop authenticating at once — the next request finds nothing.
    pub async fn revoke(&self, id: &str) -> Option<TokenMeta> {
        let mut tokens = self.tokens.write().await;
        let at = tokens.iter().position(|t| t.id == id)?;
        let removed = tokens.remove(at);
        self.save(&tokens).await;
        Some(removed.meta())
    }

    /// Every token's metadata, oldest first. Hashes stay here; nothing outside answers them.
    pub async fn list(&self) -> Vec<TokenMeta> {
        self.tokens.read().await.iter().map(Stored::meta).collect()
    }

    /// The token a presented plaintext belongs to, if any — the Bearer check `host_guard` runs for
    /// a request that is not the owner's. Also refreshes `last_used_at`, best effort (see
    /// [`Stored::touch`]).
    pub async fn authenticate(&self, presented: &str) -> Option<ScopedToken> {
        let hash = hash_token(presented);
        let mut tokens = self.tokens.write().await;
        let stored = tokens
            .iter_mut()
            .find(|t| crate::gateway::constant_time_eq(t.token_hash.as_bytes(), hash.as_bytes()))?;
        stored.touch();
        Some(stored.scoped())
    }

    /// A token's display name by id, for the prompt marking: the launch record keeps the id
    /// (stable, revocation-proof), and the colony's prompt wants the name a person chose. A
    /// revoked token falls back to its id, so the marking never disappears.
    pub async fn name_of(&self, id: &str) -> Option<String> {
        self.tokens.read().await.iter().find(|t| t.id == id).map(|t| t.name.clone())
    }
}

/// Trims an org list, refusing empties. Orgs are GitHub owners, so `acme/web` as an org is a typo
/// caught here rather than a limit that never matches.
fn clean_orgs(orgs: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for org in orgs {
        let org = org.trim();
        if org.is_empty() || org.contains('/') {
            return Err(format!(
                "orgs entries must be organization names, not repositories (got \"{org}\")"
            ));
        }
        out.push(org.to_string());
    }
    Ok(out)
}

/// Trims a repo list, refusing anything `valid_repo` would.
fn clean_repos(repos: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for repo in repos {
        let repo = repo.trim();
        if !valid_repo(repo) {
            return Err(format!("repos entries must be owner/repo (got \"{repo}\")"));
        }
        out.push(repo.to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Enforcement: what a scoped token may call.
// ---------------------------------------------------------------------------

/// What a route needs from a scoped token.
enum Need<'a> {
    /// Any scope at or above this may call it; nothing else to check.
    Bare(Scope),
    /// A colony-scoped route: the scope first, then the colony's org/repo against the token's
    /// limits — outside them reads as an unknown id (404), never as a 403.
    Session { id: &'a str, at_least: Scope },
    /// A map route for one repository: read scope, then the repository against the token's
    /// limits — outside them the map reads as unknown (404), like an out-of-limits colony.
    Map { owner: &'a str, name: &'a str },
    /// A launch route: the repository the body names is the handler's to check.
    Launch,
    /// No scoped token: owner only.
    Owner,
}

/// What a route needs, decided on the method and the raw path segments. `host_guard` runs before
/// the router matches, so no `MatchedPath` exists yet; this segment match stands in for the route
/// table. Segments stay percent-encoded — the router matches on the raw path too — so a `%2F`
/// inside a segment is one segment here exactly as it is there, and an encoded slash cannot rename
/// one route into another. Everything unrecognized is owner-only: the allowlist is closed by
/// default, so a route added to the API later is refused to scoped tokens until its scope is
/// decided here.
fn classify<'a>(method: &Method, path: &'a str) -> Need<'a> {
    let segs: Vec<&'a str> = path.split('/').skip(1).collect();
    let get = method == Method::GET;
    let post = method == Method::POST;
    match segs.as_slice() {
        // Reads: watch the install and its colonies, never drive them.
        ["api", "status" | "version"] if get => Need::Bare(Scope::Read),
        ["api", "sessions"] if get => Need::Bare(Scope::Read),
        ["api", "maps", owner, name] if get && !owner.is_empty() && !name.is_empty() => Need::Map { owner, name },
        ["api", "maps", owner, name, "files" | "file"] if get && !owner.is_empty() && !name.is_empty() => {
            Need::Map { owner, name }
        }
        ["api", "tokens", "self"] if get => Need::Bare(Scope::Read),
        // Colony-scoped: watch at read, drive (answer, stop, resume) at operate.
        ["api", "sessions", id] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["api", "sessions", id, "question" | "events" | "diff"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["api", "sessions", id, "answer" | "stop" | "resume"] if post && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Operate,
        },
        // Launching: start a colony. Loops stay with the owner (issue #508's first slice): a loop
        // spawns colonies on a schedule, out of reach of a token's caps, budget and external-input
        // marking, so no scope of token may create or run one.
        ["api", "sessions"] if post => Need::Launch,
        // Everything else — settings, secrets, provider keys, token management itself — stays
        // with the owner: managing credentials is not a thing a credential may do.
        _ => Need::Owner,
    }
}

/// The scoped-token rule `classify` applies to a request, spelled for the route-table snapshot
/// (server.rs's tests): `read`, `session>=operate`, `map`, `launch` or `owner`.
#[cfg(test)]
pub(crate) fn describe_need(method: &Method, path: &str) -> String {
    match classify(method, path) {
        Need::Bare(scope) => scope.as_str().to_string(),
        Need::Session { at_least, .. } => format!("session>={}", at_least.as_str()),
        Need::Map { .. } => "map".to_string(),
        Need::Launch => "launch".to_string(),
        Need::Owner => "owner".to_string(),
    }
}

/// What `authorize` refuses, as the HTTP response it becomes.
pub(crate) enum Deny {
    /// The scope does not reach this route: 403, naming the token's scope and the route.
    Forbidden(String),
    /// A colony-scoped route whose colony is unknown or outside the token's org/repo limits:
    /// 404, worded exactly like the handlers' own unknown-id answer.
    NoSession,
    /// A map route whose repository is outside the token's org/repo limits: 404, the same
    /// nothing-to-see an out-of-limits colony gets.
    NoMap,
}

impl Deny {
    fn forbidden(token: &ScopedToken, method: &Method, path: &str) -> Self {
        Deny::Forbidden(format!(
            "this API token's scope ({}) does not cover {} {}",
            token.scope.as_str(),
            method,
            path
        ))
    }

    pub(crate) fn into_response(self) -> Response {
        match self {
            Deny::Forbidden(message) => (StatusCode::FORBIDDEN, message).into_response(),
            Deny::NoSession => (StatusCode::NOT_FOUND, "no such session").into_response(),
            Deny::NoMap => (StatusCode::NOT_FOUND, "no such map").into_response(),
        }
    }
}

/// Whether a request carrying this scoped token may proceed down the route it names. Called by
/// `host_guard` after the token has authenticated; the colony lookup is what turns the org/repo
/// limits into 404s here rather than leaking them as 403s.
pub(crate) async fn authorize(app: &App, token: &ScopedToken, method: &Method, path: &str) -> Result<(), Deny> {
    match classify(method, path) {
        Need::Bare(at_least) | Need::Session { at_least, .. } if token.scope < at_least => {
            Err(Deny::forbidden(token, method, path))
        }
        Need::Bare(_) => Ok(()),
        Need::Session { id, .. } => {
            // Outside the token's org/repo limits, the colony does not exist for this caller:
            // the same words an unknown id gets, so the answer leaks no existence either way.
            let Some(session) = app.session(id).await else {
                return Err(Deny::NoSession);
            };
            if token.covers(&session.org, &session.repo) {
                Ok(())
            } else {
                Err(Deny::NoSession)
            }
        }
        Need::Map { owner, name } => {
            // The map's repository is held to the same limits, decided here where the path
            // segments are already in hand: outside them the map reads as unknown.
            if token.covers(owner, &format!("{owner}/{name}")) {
                Ok(())
            } else {
                Err(Deny::NoMap)
            }
        }
        Need::Launch if token.scope >= Scope::Launch => Ok(()),
        Need::Launch => Err(Deny::forbidden(token, method, path)),
        Need::Owner => Err(Deny::forbidden(token, method, path)),
    }
}

/// The launch caps a scoped token runs under, checked in `sessions::create` before any colony is
/// made: `Some(message)` refuses the launch. `max_concurrent` counts the token's colonies that are
/// not yet terminal — queued ones hold a place in line, so they count; the budget sums what the
/// token's colonies created today (UTC) have spent so far ([`Session::total_cost_usd`], Claude's
/// own estimate plus the gateway's routed pricing). Pure, so both refusals are testable without a
/// boot.
pub(crate) fn launch_cap_error(token: &ScopedToken, sessions: &[Session], now: DateTime<Utc>) -> Option<String> {
    let mine = |s: &Session| s.launched_by_token.as_deref() == Some(token.id.as_str());
    if let Some(max) = token.max_concurrent {
        let live = sessions.iter().filter(|s| mine(s) && !s.status.is_terminal()).count();
        if live >= max as usize {
            return Some(format!(
                "this API token's concurrency cap is {max} and it already has {live} colonies that are not finished; stop one or raise max_concurrent"
            ));
        }
    }
    if let Some(budget) = token.budget_usd_per_day {
        let today = now.date_naive();
        let spent: f64 = sessions
            .iter()
            .filter(|s| mine(s) && s.created_at.date_naive() == today)
            .map(Session::total_cost_usd)
            .sum();
        if spent >= budget {
            return Some(format!(
                "this API token's colonies have spent ${spent:.2} of its ${budget:.2} budget for today (UTC); raise budget_usd_per_day or wait for the next day"
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The routes themselves. Owner-only ones (`list`, `create`, `revoke`) need no scope check here:
// `authorize` already refused every scoped token at the middleware, and an unauthenticated
// request never got past `host_guard`.
// ---------------------------------------------------------------------------

/// `GET /api/tokens`: the registry's metadata.
pub async fn list(State(app): State<Shared>) -> Json<Vec<TokenMeta>> {
    Json(app.api_tokens.list().await)
}

/// `POST /api/tokens`: mint a token. The plaintext comes back here and nowhere else.
pub async fn create(State(app): State<Shared>, Json(req): Json<NewToken>) -> ApiResult<CreatedToken> {
    let made = app
        .api_tokens
        .create(req)
        .await
        .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    Ok(Json(made))
}

/// `DELETE /api/tokens/{id}`: revoke a token.
pub async fn revoke(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    if app.api_tokens.revoke(&id).await.is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no such token"));
    }
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/tokens/self`: who is asking. The one route a scoped token may read about itself; an
/// owner — the cookie or the install token — reads the constant owner answer.
pub async fn self_view(scoped: Option<axum::Extension<ScopedToken>>) -> Json<Value> {
    match scoped {
        Some(axum::Extension(token)) => Json(json!({
            "owner": false,
            "name": token.name,
            "scope": token.scope.as_str(),
            "orgs": token.orgs,
            "repos": token.repos,
        })),
        None => Json(json!({"owner": true, "scope": "owner", "orgs": [], "repos": []})),
    }
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        // Scoped API tokens (issue #508): the owner mints and revokes them; `self` is the one
        // route a scoped token may read, and `host_guard` decides the rest from the token's scope.
        .route("/api/tokens", routing::get(list).post(create))
        .route("/api/tokens/self", routing::get(self_view))
        .route("/api/tokens/{id}", routing::delete(revoke))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use crate::sessions::tests::colony;
    use crate::tests::test_app;

    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-api-tokens-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn new_token(name: &str, scope: Scope) -> NewToken {
        NewToken {
            name: name.to_string(),
            scope: scope.as_str().to_string(),
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        }
    }

    fn scoped(scope: Scope, orgs: &[&str], repos: &[&str]) -> ScopedToken {
        ScopedToken {
            id: "tok_test".into(),
            name: "test".into(),
            scope,
            orgs: orgs.iter().map(|s| s.to_string()).collect(),
            repos: repos.iter().map(|s| s.to_string()).collect(),
            max_concurrent: None,
            budget_usd_per_day: None,
        }
    }

    #[tokio::test]
    async fn the_registry_keeps_hashes_and_the_plaintext_works_once_created() {
        let dir = root();
        let registry = Registry::load(&dir);
        let made = registry
            .create(NewToken {
                name: "ci".into(),
                scope: "operate".into(),
                orgs: vec!["acme".into()],
                repos: vec!["acme/web".into()],
                max_concurrent: Some(2),
                budget_usd_per_day: Some(5.0),
            })
            .await
            .unwrap();
        assert!(made.token.starts_with("col_"), "{}", made.token);
        let body = made.token.trim_start_matches("col_");
        assert_eq!(body.len(), 64, "64 hex chars, like util::random_token");
        // The plaintext never reached the file; its hash did.
        let raw = std::fs::read_to_string(file(&dir)).unwrap();
        assert!(!raw.contains(&made.token), "the plaintext must not be stored: {raw}");
        assert!(raw.contains(&hash_token(&made.token)), "{raw}");
        assert!(raw.contains("\"operate\""), "{raw}");
        // A fresh registry (a restart) authenticates the same plaintext, and lists the metadata
        // without the hash.
        let again = Registry::load(&dir);
        let who = again.authenticate(&made.token).await.unwrap();
        assert_eq!((who.name.as_str(), who.scope), ("ci", Scope::Operate));
        assert_eq!(who.orgs, vec!["acme".to_string()]);
        let listed = again.list().await;
        assert_eq!(listed.len(), 1);
        // A wrong token authenticates nothing.
        assert!(again.authenticate("col_deadc0de").await.is_none());
        // Revocation takes effect at once, and survives a reload.
        let id = made.meta.id.clone();
        assert!(again.revoke(&id).await.is_some());
        assert!(again.revoke(&id).await.is_none(), "a second revoke is a 404");
        assert!(again.authenticate(&made.token).await.is_none());
        assert!(Registry::load(&dir).authenticate(&made.token).await.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn token_validation_refuses_bad_names_scopes_and_limits() {
        let dir = root();
        let registry = Registry::load(&dir);
        let err = registry.create(new_token("  ", Scope::Read)).await.map(|_| ()).unwrap_err();
        assert!(err.contains("name is required"), "{err}");
        let mut bad_scope = new_token("ci", Scope::Read);
        bad_scope.scope = "root".into();
        let err = registry.create(bad_scope).await.map(|_| ()).unwrap_err();
        assert!(err.contains("read, operate, launch"), "{err}");
        let mut bad_org = new_token("ci", Scope::Read);
        bad_org.orgs = vec!["acme/web".into()];
        let err = registry.create(bad_org).await.map(|_| ()).unwrap_err();
        assert!(err.contains("organization names"), "{err}");
        let mut bad_repo = new_token("ci", Scope::Read);
        bad_repo.repos = vec!["../etc".into()];
        let err = registry.create(bad_repo).await.map(|_| ()).unwrap_err();
        assert!(err.contains("owner/repo"), "{err}");
        let mut bad_cap = new_token("ci", Scope::Launch);
        bad_cap.max_concurrent = Some(0);
        let err = registry.create(bad_cap).await.map(|_| ()).unwrap_err();
        assert!(err.contains("max_concurrent"), "{err}");
        let mut bad_budget = new_token("ci", Scope::Launch);
        bad_budget.budget_usd_per_day = Some(-1.0);
        let err = registry.create(bad_budget).await.map(|_| ()).unwrap_err();
        assert!(err.contains("budget_usd_per_day"), "{err}");
        assert!(registry.list().await.is_empty(), "nothing refused was stored");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The route table, end to end through `authorize`: reads at `read`, driving at `operate`,
    /// launching at `launch`, everything else with the owner only.
    #[tokio::test]
    async fn the_route_table_refuses_outside_a_scope_and_hides_other_orgs_colonies() {
        let (app, dir) = {
            let root = root();
            let app = test_app(&root);
            let mut s = colony("acme", SessionStatus::Running);
            s.id = "abc".into();
            app.sessions.write().await.push(s);
            (app, root)
        };
        let get = Method::GET;
        let post = Method::POST;
        let read = scoped(Scope::Read, &[], &[]);
        let operate = scoped(Scope::Operate, &[], &[]);
        let launch = scoped(Scope::Launch, &[], &[]);

        // Reads are fine at every scope.
        for path in [
            "/api/status",
            "/api/version",
            "/api/sessions",
            "/api/sessions/abc",
            "/api/sessions/abc/question",
            "/api/sessions/abc/events",
            "/api/sessions/abc/diff",
            "/api/maps/acme/web",
            "/api/maps/acme/web/files",
            "/api/tokens/self",
        ] {
            assert!(authorize(&app, &read, &get, path).await.is_ok(), "read {path}");
            assert!(authorize(&app, &launch, &get, path).await.is_ok(), "launch {path}");
        }
        // Driving needs operate.
        for path in [
            "/api/sessions/abc/answer",
            "/api/sessions/abc/stop",
            "/api/sessions/abc/resume",
        ] {
            assert!(
                matches!(authorize(&app, &read, &post, path).await, Err(Deny::Forbidden(_))),
                "read {path}"
            );
            assert!(authorize(&app, &operate, &post, path).await.is_ok(), "operate {path}");
        }
        // Launching needs launch.
        for path in ["/api/sessions"] {
            assert!(matches!(
                authorize(&app, &operate, &post, path).await,
                Err(Deny::Forbidden(_))
            ));
            assert!(authorize(&app, &launch, &post, path).await.is_ok(), "launch {path}");
        }
        // Loops are the owner's alone at any scope (issue #508's first slice): a loop spawns
        // colonies on a schedule, out of reach of a token's caps, budget and marking.
        for (method, path) in [
            (&get, "/api/loops"),
            (&post, "/api/loops"),
            (&post, "/api/loops/loop_1/run-now"),
        ] {
            assert!(
                matches!(authorize(&app, &launch, method, path).await, Err(Deny::Forbidden(_))),
                "{method} {path} must be owner-only"
            );
        }
        // The management routes and every other route stay with the owner, at any scope.
        for (method, path) in [
            (&get, "/api/tokens"),
            (&post, "/api/tokens"),
            (&Method::DELETE, "/api/tokens/tok_1"),
            (&get, "/api/secrets"),
            (&post, "/api/sessions/abc/publish"),
            (&get, "/api/sessions/abc/terminal"),
            (&post, "/api/sessions/abc/answer/extra"),
            (&get, "/api/sessions/abc/events/../terminal"),
        ] {
            assert!(
                matches!(authorize(&app, &launch, method, path).await, Err(Deny::Forbidden(_))),
                "{method} {path} must be owner-only"
            );
        }
        // An unknown colony reads as unknown, not forbidden.
        assert!(matches!(
            authorize(&app, &read, &get, "/api/sessions/nope").await,
            Err(Deny::NoSession)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Org and repo limits hide colonies rather than refusing them: a 404, exactly like an
    /// unknown id, so the token cannot probe what exists.
    #[tokio::test]
    async fn org_and_repo_limits_turn_out_of_scope_colonies_into_404s() {
        let root = root();
        let app = test_app(&root);
        let mut s = colony("acme", SessionStatus::Running);
        s.id = "abc".into();
        app.sessions.write().await.push(s);
        let get = Method::GET;
        let path = "/api/sessions/abc";
        // No limits: everything.
        assert!(authorize(&app, &scoped(Scope::Read, &[], &[]), &get, path).await.is_ok());
        // Org matches, repo matches: in.
        assert!(
            authorize(&app, &scoped(Scope::Read, &["acme"], &[]), &get, path)
                .await
                .is_ok(),
            "org limit acme covers acme/repo"
        );
        assert!(
            authorize(&app, &scoped(Scope::Read, &[], &["acme/repo"]), &get, path)
                .await
                .is_ok(),
            "repo limit acme/repo covers acme/repo"
        );
        // Either limit missing the colony hides it.
        for token in [
            scoped(Scope::Read, &["other"], &[]),
            scoped(Scope::Read, &[], &["acme/api"]),
            scoped(Scope::Read, &["acme"], &["acme/api"]),
        ] {
            assert!(
                matches!(authorize(&app, &token, &get, path).await, Err(Deny::NoSession)),
                "{token:?}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// The map reads are held to the org/repo limits too (issue #508): outside them a map reads
    /// as unknown — a 404, never a 403 — exactly like an out-of-limits colony, so the answers
    /// leak no existence either way.
    #[tokio::test]
    async fn map_reads_outside_the_limits_read_as_unknown() {
        let root = root();
        let app = test_app(&root);
        let get = Method::GET;
        for path in ["/api/maps/acme/web", "/api/maps/acme/web/files", "/api/maps/acme/web/file"] {
            assert!(
                authorize(&app, &scoped(Scope::Read, &[], &[]), &get, path).await.is_ok(),
                "no limits: {path}"
            );
            assert!(
                authorize(&app, &scoped(Scope::Read, &["acme"], &[]), &get, path)
                    .await
                    .is_ok(),
                "org limit acme covers acme/web: {path}"
            );
            assert!(
                authorize(&app, &scoped(Scope::Read, &[], &["acme/web"]), &get, path)
                    .await
                    .is_ok(),
                "repo limit acme/web covers acme/web: {path}"
            );
            for token in [scoped(Scope::Read, &["other"], &[]), scoped(Scope::Read, &[], &["acme/api"])] {
                assert!(
                    matches!(authorize(&app, &token, &get, path).await, Err(Deny::NoMap)),
                    "{token:?} at {path}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn launch_caps_refuse_at_the_concurrency_and_budget_limits() {
        let now = Utc::now();
        let mut token = scoped(Scope::Launch, &[], &[]);
        let mut s = colony("acme", SessionStatus::Running);
        s.id = "one".into();
        s.launched_by_token = Some("tok_test".into());
        // No caps, no refusal.
        assert!(launch_cap_error(&token, &[s.clone()], now).is_none());
        // Under the cap: the colony itself counts against nothing — a launch is checked before it
        // exists, so one live colony passes a cap of 2.
        token.max_concurrent = Some(2);
        assert!(launch_cap_error(&token, &[s.clone()], now).is_none());
        token.max_concurrent = Some(1);
        let err = launch_cap_error(&token, &[s.clone()], now).unwrap();
        assert!(err.contains("concurrency cap is 1") && err.contains("1 colonies"), "{err}");
        // A terminal colony has finished: it holds no concurrency.
        let mut done = s.clone();
        done.status = SessionStatus::PrOpened;
        assert!(
            launch_cap_error(&token, &[done], now).is_none(),
            "terminal colonies do not count"
        );
        // Other tokens' colonies are not this token's problem.
        let mut other = s.clone();
        other.launched_by_token = Some("tok_other".into());
        assert!(launch_cap_error(&token, &[other], now).is_none());
        // The budget sums today's spend of the token's own colonies.
        token.max_concurrent = None;
        token.budget_usd_per_day = Some(10.0);
        assert!(
            launch_cap_error(&token, &[s.clone()], now).is_none(),
            "$5 of $10 still launches"
        );
        s.cost_usd = Some(4.0);
        s.routed_cost_usd = Some(6.5);
        let err = launch_cap_error(&token, &[s], now).unwrap();
        assert!(err.contains("$10.50") && err.contains("$10.00"), "{err}");
        // Yesterday's spend is yesterday's: a colony created before today does not count.
        let mut token = scoped(Scope::Launch, &[], &[]);
        token.budget_usd_per_day = Some(10.0);
        let mut old = colony("acme", SessionStatus::Running);
        old.launched_by_token = Some("tok_test".into());
        old.cost_usd = Some(99.0);
        old.created_at = now - chrono::Duration::hours(30);
        assert!(
            launch_cap_error(&token, &[old], now).is_none(),
            "spend is daily, not lifetime"
        );
    }

    /// `covers` is what the handlers check the launch bodies against, so it is pinned here too.
    #[test]
    fn the_org_and_repo_limits_are_conjunctions_with_empty_meaning_all() {
        assert!(scoped(Scope::Launch, &[], &[]).covers("acme", "acme/web"));
        assert!(scoped(Scope::Launch, &["acme"], &[]).covers("acme", "acme/web"));
        assert!(!scoped(Scope::Launch, &["acme"], &[]).covers("other", "other/web"));
        assert!(scoped(Scope::Launch, &["acme"], &["acme/web"]).covers("acme", "acme/web"));
        assert!(!scoped(Scope::Launch, &["acme"], &["acme/web"]).covers("acme", "acme/api"));
        assert!(!scoped(Scope::Launch, &["other"], &["acme/web"]).covers("acme", "acme/web"));
    }

    // -- Through the guard: the real `host_guard` over the routes a scoped token may touch,
    // driven with `oneshot` the way main.rs's auth tests drive theirs.

    use axum::{
        Router,
        http::{Request, header},
    };
    use tower::ServiceExt as _;

    fn guard_router(app: &Shared) -> axum::Router<()> {
        use axum::routing::{get, post};
        Router::new()
            .route("/api/status", get(crate::status::status))
            .route("/api/sessions", get(crate::sessions::list).post(crate::sessions::create))
            .route("/api/sessions/{id}", get(crate::sessions::get))
            .route("/api/sessions/{id}/question", get(crate::sessions::question))
            .route("/api/sessions/{id}/answer", post(crate::sessions::answer))
            .route("/api/tokens", get(list).post(create))
            .route("/api/tokens/self", get(self_view))
            .layer(axum::middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    /// A request through the guard: loopback Host, an optional Bearer, an optional JSON body.
    fn send(method: Method, uri: &str, bearer: Option<&str>, body: Option<&str>) -> axum::extract::Request {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7878");
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let body = body.map(String::from).unwrap_or_default();
        builder.body(axum::body::Body::from(body)).unwrap()
    }

    async fn body_json(res: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_text(res: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// An app whose config dir exists (a token creation writes through to it) plus the plaintext
    /// of a freshly minted token, known to the app's own registry.
    async fn app_with_token(root: &std::path::Path, req: NewToken) -> (Shared, String) {
        std::fs::create_dir_all(root.join("config")).unwrap();
        let app = test_app(root);
        let made = app.api_tokens.create(req).await.unwrap();
        (app, made.token)
    }

    #[tokio::test]
    async fn a_read_token_watches_but_cannot_launch_or_manage() {
        let root = root();
        let (app, token) = app_with_token(
            &root,
            NewToken {
                name: "ci".into(),
                scope: "read".into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            },
        )
        .await;
        let bearer = Some(token.as_str());
        let router = guard_router(&app);
        // Reads pass the guard and reach the handler.
        for uri in ["/api/status", "/api/sessions"] {
            let res = router.clone().oneshot(send(Method::GET, uri, bearer, None)).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
        }
        // No token at all is still a 401 — a scoped token replaces the owner's, it does not
        // lower the wall.
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/sessions", None, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // Launching is refused with the scope named, before the body matters.
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/sessions", bearer, Some(r#"{"repo":"acme/web"}"#)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = body_text(res).await;
        assert!(body.contains("scope (read) does not cover POST /api/sessions"), "{body}");
        // Token management is the owner's alone, at any scope.
        for (method, uri) in [
            (Method::GET, "/api/tokens"),
            (Method::POST, "/api/tokens"),
            (Method::DELETE, "/api/tokens/tok_x"),
        ] {
            let res = router.clone().oneshot(send(method.clone(), uri, bearer, None)).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
        // Who the token is — and the owner's constant answer on the same route.
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/tokens/self", bearer, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let who = body_json(res).await;
        assert_eq!(who["owner"], false);
        assert_eq!(who["name"], "ci");
        assert_eq!(who["scope"], "read");
        let res = router
            .oneshot(send(Method::GET, "/api/tokens/self", Some(&app.api_token), None))
            .await
            .unwrap();
        let who = body_json(res).await;
        assert_eq!(who["owner"], true);
        assert_eq!(who["scope"], "owner");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The org limit filters the list and hides single colonies — a 404, the same answer an
    /// unknown id gets — while the owner still sees everything.
    #[tokio::test]
    async fn an_org_scoped_token_sees_only_its_own_orgs_colonies() {
        let root = root();
        let (app, token) = app_with_token(
            &root,
            NewToken {
                name: "watcher".into(),
                scope: "read".into(),
                orgs: vec!["acme".into()],
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            },
        )
        .await;
        let mut mine = colony("acme", SessionStatus::Running);
        mine.id = "mine".into();
        let mut theirs = colony("other", SessionStatus::Running);
        theirs.id = "theirs".into();
        app.sessions.write().await.extend([mine, theirs]);
        let bearer = Some(token.as_str());
        let router = guard_router(&app);
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/sessions", bearer, None))
            .await
            .unwrap();
        let listed = body_json(res).await;
        let ids: Vec<&str> = listed.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["mine"], "the other org's colony is not in the list: {listed}");
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/sessions/mine", bearer, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "a colony inside the limit reads fine");
        for uri in ["/api/sessions/theirs", "/api/sessions/theirs/question"] {
            let res = router.clone().oneshot(send(Method::GET, uri, bearer, None)).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri} reads as unknown, not forbidden");
        }
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/sessions", Some(&app.api_token), None))
            .await
            .unwrap();
        let listed = body_json(res).await;
        assert_eq!(listed.as_array().unwrap().len(), 2, "the owner sees both: {listed}");
        let res = router
            .oneshot(send(Method::GET, "/api/sessions/theirs", Some(&app.api_token), None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(root);
    }

    /// `operate` answers a pending question over HTTP; `read` is refused at the guard. A stale
    /// `question_id` and an unknown colony each get the refusal that names them.
    #[tokio::test]
    async fn an_operate_token_answers_while_a_read_token_cannot() {
        let root = root();
        let (app, read) = app_with_token(
            &root,
            NewToken {
                name: "watcher".into(),
                scope: "read".into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            },
        )
        .await;
        let operate = app
            .api_tokens
            .create(NewToken {
                name: "ci".into(),
                scope: "operate".into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        let mut s = colony("acme", SessionStatus::WaitingForAnswer);
        s.id = "abc".into();
        app.sessions.write().await.push(s);
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), crate::protocol::QuestionRisk::ReadOnly));
        let router = guard_router(&app);
        let body = r#"{"question_id":"q1","answers":{"a":"b"},"response":"go"}"#;
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/sessions/abc/answer",
                Some(read.as_str()),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "read cannot answer");
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/sessions/abc/answer",
                Some(operate.token.as_str()),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT, "operate answers");
        // The answer reached the colony's command channel — with the note marked as coming from
        // an external token.
        let mut rx = rt.commands_rx.lock().await.take().unwrap();
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["type"], "answer");
        assert_eq!(
            forwarded["response"].as_str().unwrap(),
            "[external input from API token \"ci\"] go",
            "the note is marked as external input"
        );
        // A stale id, and a colony that is not asking, each refuse with their own words. The
        // forwarded answer took `q1` down (the runner's `question_answered` echo would), so a
        // newer question is open for the stale one to miss.
        *rt.open_question.lock().await = Some(("q2".into(), Vec::new(), crate::protocol::QuestionRisk::ReadOnly));
        let stale = r#"{"question_id":"q0","answers":{},"response":""}"#;
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/sessions/abc/answer",
                Some(operate.token.as_str()),
                Some(stale),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let text = body_text(res).await;
        assert!(text.contains("different question now"), "{text}");
        let malformed = r#"{"answers":{}}"#;
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/sessions/abc/answer",
                Some(operate.token.as_str()),
                Some(malformed),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/sessions/ghost/answer",
                Some(operate.token.as_str()),
                Some(body),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "an unknown colony is unknown");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A launch token refuses a launch outside its org/repo limits (403, the body check), and its
    /// concurrency cap refuses the next launch with the reason (429) — while under the cap the
    /// request gets past every token gate and fails later, on what a bare test app lacks (an
    /// agent module), proving the earlier refusals were the token's.
    #[tokio::test]
    async fn a_launch_token_is_refused_outside_its_limits_and_at_its_concurrency_cap() {
        let root = root();
        let (app, token) = app_with_token(
            &root,
            NewToken {
                name: "launcher".into(),
                scope: "launch".into(),
                orgs: Vec::new(),
                repos: vec!["acme/web".into()],
                max_concurrent: Some(1),
                budget_usd_per_day: None,
            },
        )
        .await;
        let bearer = Some(token.as_str());
        let router = guard_router(&app);
        // Outside the repo limit: 403 before anything is launched.
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/sessions", bearer, Some(r#"{"repo":"acme/api"}"#)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let body = body_text(res).await;
        assert!(body.contains("do not include acme/api"), "{body}");
        // At the cap: the token's own live colony holds the one place, so the next launch inside
        // the limits is refused with the cap named.
        let made = app.api_tokens.authenticate(&token).await.unwrap();
        let mut live = colony("acme", SessionStatus::Running);
        live.id = "live".into();
        live.launched_by_token = Some(made.id.clone());
        app.sessions.write().await.push(live);
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/sessions", bearer, Some(r#"{"repo":"acme/web"}"#)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS, "the cap refuses the launch");
        let body = body_text(res).await;
        assert!(body.contains("concurrency cap"), "{body}");
        // Inside the limit, under the cap: every token gate passes and the launch fails later, on
        // the bare test app's missing agent module — proof the refusals above were the token's.
        app.sessions.write().await.clear();
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/sessions", bearer, Some(r#"{"repo":"acme/web"}"#)))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "past the token gates, the launch fails on the app"
        );
        let body = body_text(res).await;
        assert!(body.contains("agent module"), "{body}");
        let _ = std::fs::remove_dir_all(root);
    }
}
