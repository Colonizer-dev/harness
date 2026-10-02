//! Scoped API tokens (issue #508): named, least-privilege keys the maintainer hands to CLIs and
//! automations, so they can drive the mothership without holding the per-install owner token.
//!
//! A token carries an ordered scope — `fleet` < `read` < `operate` < `launch` — optional org and
//! repo limits
//! (empty lists mean no limit), and optional launch caps: the most colonies it may keep unfinished,
//! and the most model spend its colonies may run up per UTC day. The registry lives at
//! `<config_dir>/api-tokens.json` and stores only a SHA-256 hash of each token: the plaintext is
//! returned once at creation and never again, by this process or by the file.
//!
//! `host_guard` (server.rs) accepts a scoped token as `Authorization: Bearer` only — a browser never
//! holds one, so the `colonizer_token` cookie stays owner-only — and [`authorize`] decides the
//! route: anything outside the scope's allowlist is a 403 naming the scope, and a colony-scoped
//! route for a colony outside the token's org/repo limits is a 404, the same answer an unknown id
//! gets, so the token learns nothing beyond what it was granted. Launch routes check the requested
//! repository in their handler, where the body is parsed ([`ScopedToken::covers`]).

use crate::{
    ApiResult, App, Shared, client_error,
    sessions::{Session, SessionStatus},
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

/// How much a token may do, ordered so `token.scope >= needed` reads as "may". `fleet` is the
/// trust scope one machine in a fleet holds (issue #686): it reaches only the fleet's own routes,
/// and nothing below `read` passes any other need. `read` watches, `operate` drives colonies that
/// exist (answer, stop, resume), `launch` starts colonies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Fleet,
    Read,
    Operate,
    Launch,
}

impl Scope {
    /// The wire spelling, shared by the registry file and the API answers.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Fleet => "fleet",
            Scope::Read => "read",
            Scope::Operate => "operate",
            Scope::Launch => "launch",
        }
    }

    /// Parses the scope a create request names. Manual, so a bad one is refused with the
    /// vocabulary in the message rather than a deserialization error. `fleet` parses — the fleet
    /// module mints its tokens through [`Registry::create_fleet_token`] — but [`Registry::create`]
    /// refuses it: it is never a scope the cockpit's token form hands out.
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "fleet" => Some(Scope::Fleet),
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
        if scope == Scope::Fleet {
            // Fleet scope is the pairing machinery's to mint, one token per joined machine; it is
            // never a scope a token-holder asks for at the token form.
            return Err("scope must be one of: read, operate, launch".to_string());
        }
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

    /// Mints the one fleet-scoped token a joined machine holds, named for it. Deliberately outside
    /// [`Registry::create`], which refuses the scope: a fleet token is the pairing machinery's
    /// artifact, handed to exactly one machine at approve time, so the token form can neither ask
    /// for the scope nor mint one outside a pairing. Returns the plaintext once and its id, which
    /// the member row keeps for the revocation that removal and leaving both run.
    pub(crate) async fn create_fleet_token(&self, member_name: &str) -> Result<(String, String), String> {
        let name = format!("fleet: {member_name}");
        if name.len() > MAX_NAME {
            return Err(format!("name is {} characters; keep it under {MAX_NAME}", name.len()));
        }
        let plaintext = format!("col_{}", crate::util::random_token());
        let stored = Stored {
            id: format!("tok_{}", short_id()),
            name,
            scope: Scope::Fleet,
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
            created_at: Utc::now(),
            last_used_at: None,
            token_hash: hash_token(&plaintext),
        };
        let id = stored.id.clone();
        let mut tokens = self.tokens.write().await;
        tokens.push(stored);
        self.save(&tokens).await;
        Ok((plaintext, id))
    }

    /// Removes a token; `None` when no token carries the id. Presentations of the revoked token
    /// stop authenticating at once — the next request finds nothing.
    pub async fn revoke(&self, id: &str) -> Option<TokenMeta> {
        let mut tokens = self.tokens.write().await;
        let at = tokens.iter().position(|t| t.id == id)?;
        let removed = tokens.remove(at);
        self.save(&tokens).await;
        // Its open sockets and streams end now, not at its next request (issue #746).
        crate::auth::Revocation::fire(&format!("token:{id}"));
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

    /// The live token an id names, rebuilt exactly the way `authenticate` attaches one. A loop
    /// (issue #627, loops.rs) stores only the creating token's id, and each firing needs the
    /// limits, caps and budget back; a revoked token names nothing.
    pub async fn scoped(&self, id: &str) -> Option<ScopedToken> {
        self.tokens.read().await.iter().find(|t| t.id == id).map(Stored::scoped)
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
    /// A launch route — starting a colony, or a loop mutation, each of which starts or reshapes
    /// colonies: the body (or the loop) is the handler's to check.
    Launch,
    /// A fleet route (issue #686): watching the host list across the fleet, or leaving it. Only a
    /// fleet-scoped token passes — the trust scope buys exactly these, nothing else.
    Fleet,
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
    let put = method == Method::PUT;
    let delete = method == Method::DELETE;
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
        ["api", "sessions", id, "question" | "events" | "diff" | "commits"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        // Artifacts (§7.5, issue #651): a colony's `out/` files read at watch scope, like the
        // colony itself — the listing, the archive and the per-file download are one read.
        ["api", "sessions", id, "files"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["api", "sessions", id, "files", "archive"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["api", "sessions", id, "files", name, "content"] if get && !id.is_empty() && !name.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        // `messages` is the offline queue's twin of the socket's `user_message` (issue #746): a
        // colony drive, like answering.
        ["api", "sessions", id, "answer" | "messages" | "stop" | "resume"] if post && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Operate,
        },
        // `seen` (issue #744) is looking at a colony, not driving it — it clears the badge's
        // unseen-failure flag — so watching it is enough, however it arrives.
        ["api", "sessions", id, "seen"] if post && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        // The same reads under the UHP names (§7.1, issue #651): the colony list, and the
        // artifacts by session id or by the `cntr_<id>` container wrapper §7.5 puts in every
        // artifact row — the wrapper's colony is what the limits apply to, so an unparseable
        // container reads as an unknown colony (404), never as a forbidden one.
        ["uhp", "v1", "sessions"] if get => Need::Bare(Scope::Read),
        // The read-side core (§7, issue #650): the same need as the `/api` reads over the same
        // data. Discovery needs no credential at all — `host_guard` admits it before this runs —
        // but is listed anyway, so a scoped token is not refused on a public route. The single
        // colony hides behind its org/repo limits like `/api/sessions/{id}`.
        ["uhp", "v1", "uhp" | "harnesses" | "models"] if get => Need::Bare(Scope::Read),
        ["uhp", "v1", "harnesses", id] if get && !id.is_empty() => Need::Bare(Scope::Read),
        ["uhp", "v1", "sessions", id] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["uhp", "v1", "sessions", id, "files"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["uhp", "v1", "sessions", id, "files", "archive"] if get && !id.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["uhp", "v1", "sessions", id, "files", name, "content"] if get && !id.is_empty() && !name.is_empty() => Need::Session {
            id,
            at_least: Scope::Read,
        },
        ["uhp", "v1", "containers", cid, "files", fid, "content"] if get && !cid.is_empty() && !fid.is_empty() => Need::Session {
            id: cid.strip_prefix("cntr_").unwrap_or(cid),
            at_least: Scope::Read,
        },
        // Launching: start a colony.
        ["api", "sessions"] if post => Need::Launch,
        // Loops (issue #627): listing loops and reading a loop's runs is a watch; creating,
        // editing, deleting or running a loop can each start a colony, so they need launch —
        // operate never reaches a loop mutation. Which loops a token may touch, and what its runs
        // answer to, is the handlers' to check, where the loop is in hand.
        ["api", "loops"] if get => Need::Bare(Scope::Read),
        ["api", "loops", id, "runs"] if get && !id.is_empty() => Need::Bare(Scope::Read),
        // The merge train's last tick (issue #671): a watch, like the loops list.
        ["api", "merge-train"] if get => Need::Bare(Scope::Read),
        ["api", "merge-train", "loop"] if get => Need::Bare(Scope::Read),
        ["api", "loops"] if post => Need::Launch,
        ["api", "loops", id] if (put || delete) && !id.is_empty() => Need::Launch,
        ["api", "loops", id, "run-now"] if post && !id.is_empty() => Need::Launch,
        // Fleet (issue #686): the host list is what a joined machine's token is for, and leaving
        // the fleet is the one write it may do. Every other fleet route is the owner's cockpit's.
        ["api", "hosts"] if get => Need::Fleet,
        ["api", "fleet", "peer", "leave"] if post => Need::Fleet,
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
        Need::Fleet => "fleet".to_string(),
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
        // The fleet scope reaches only the fleet's own routes; being the lowest scope, it passes
        // every other need's `>=` check already, so this is the one place it admits anything.
        Need::Fleet if token.scope == Scope::Fleet => Ok(()),
        Need::Fleet => Err(Deny::forbidden(token, method, path)),
        Need::Owner => Err(Deny::forbidden(token, method, path)),
    }
}

/// The launch caps a scoped token runs under, checked in `sessions::create` before any colony is
/// made: `Some(message)` refuses the launch. `max_concurrent` counts the token's colonies that are
/// not yet terminal — queued ones hold a place in line, so they count, but a parked one does not
/// (issue #213): it holds no slot and may sit for days until its quota resets, and counting it
/// would let one parked colony spend the whole cap. The budget sums what the token's colonies
/// created today (UTC) have spent so far ([`Session::total_cost_usd`], Claude's own estimate plus
/// the gateway's routed pricing). Pure, so both refusals are testable without a boot.
pub(crate) fn launch_cap_error(token: &ScopedToken, sessions: &[Session], now: DateTime<Utc>) -> Option<String> {
    let mine = |s: &Session| s.launched_by_token.as_deref() == Some(token.id.as_str());
    if let Some(max) = token.max_concurrent {
        let live = sessions
            .iter()
            .filter(|s| mine(s) && !s.status.is_terminal() && s.status != SessionStatus::Parked)
            .count();
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
            "/api/sessions/abc/commits",
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
        // Marking a colony seen (issue #744) is looking at it, not driving it: read may POST it.
        for path in ["/api/sessions/abc/seen"] {
            assert!(authorize(&app, &read, &post, path).await.is_ok(), "read {path}");
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
        // Loops (issue #627): listing a loop's runs is a read at every scope; every loop mutation
        // can start or reshape a colony, so it takes launch — read and operate are refused.
        for path in ["/api/loops", "/api/loops/loop_1/runs"] {
            assert!(authorize(&app, &read, &get, path).await.is_ok(), "read {path}");
            assert!(authorize(&app, &operate, &get, path).await.is_ok(), "operate {path}");
            assert!(authorize(&app, &launch, &get, path).await.is_ok(), "launch {path}");
        }
        for (method, path) in [
            (&post, "/api/loops"),
            (&Method::PUT, "/api/loops/loop_1"),
            (&Method::DELETE, "/api/loops/loop_1"),
            (&post, "/api/loops/loop_1/run-now"),
        ] {
            assert!(
                matches!(authorize(&app, &read, method, path).await, Err(Deny::Forbidden(_))),
                "read {method} {path}"
            );
            assert!(
                matches!(authorize(&app, &operate, method, path).await, Err(Deny::Forbidden(_))),
                "operate {method} {path}"
            );
            assert!(authorize(&app, &launch, method, path).await.is_ok(), "launch {method} {path}");
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

    /// A parked colony (issue #213) is not finished, but it holds no slot and may sit for days
    /// until its quota resets, so it does not count against `max_concurrent` — one parked colony
    /// must not spend the token's whole cap.
    #[test]
    fn a_parked_colony_does_not_count_against_the_concurrency_cap() {
        let now = Utc::now();
        let mut token = scoped(Scope::Launch, &[], &[]);
        token.max_concurrent = Some(1);
        let mut parked = colony("acme", SessionStatus::Parked);
        parked.launched_by_token = Some("tok_test".into());
        assert!(
            launch_cap_error(&token, &[parked.clone()], now).is_none(),
            "a parked colony holds no concurrency"
        );
        // The same colony live does count: the exclusion is the park, not the record.
        let mut running = parked.clone();
        running.status = SessionStatus::Running;
        let err = launch_cap_error(&token, &[running.clone()], now).unwrap();
        assert!(err.contains("concurrency cap is 1"), "{err}");
        // And launching under a cap of 2 with one parked and one live still passes.
        token.max_concurrent = Some(2);
        assert!(launch_cap_error(&token, &[parked, running.clone()], now).is_none());
        // A second live colony would be one over.
        assert!(launch_cap_error(&token, &[running.clone(), running], now).is_some());
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
        use axum::routing::{get, post, put};
        Router::new()
            .route("/api/status", get(crate::status::status))
            .route("/api/sessions", get(crate::sessions::list).post(crate::sessions::create))
            .route("/api/sessions/{id}", get(crate::sessions::get))
            .route("/api/sessions/{id}/question", get(crate::sessions::question))
            .route("/api/sessions/{id}/answer", post(crate::sessions::answer))
            .route("/api/loops", get(crate::loops::list).post(crate::loops::create))
            .route("/api/loops/{id}", put(crate::loops::update).delete(crate::loops::delete))
            .route("/api/loops/{id}/run-now", post(crate::loops::run_now))
            .route("/api/loops/{id}/runs", get(crate::loops::runs))
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
        // A loop mutation starts colonies, so creating a loop is refused at the guard too.
        let res = router
            .clone()
            .oneshot(send(
                Method::POST,
                "/api/loops",
                bearer,
                Some(r#"{"name":"Triage","repo":"acme/web","prompt":"Triage","cadence":{"every":"interval","minutes":60}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(body_text(res).await.contains("scope (read) does not cover POST /api/loops"));
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

    /// A launch token keeps loops of its own (issue #627): the created loop records the token,
    /// creation outside the repo limits and map-loop creation are refused, an owner's loop is not
    /// in the list and reads as unknown everywhere, and an edit of its own loop keeps the token.
    #[tokio::test]
    async fn a_launch_token_creates_loops_under_its_token_and_sees_only_its_own() {
        let root = root();
        let (app, token) = app_with_token(
            &root,
            NewToken {
                name: "cron".into(),
                scope: "launch".into(),
                orgs: Vec::new(),
                repos: vec!["acme/web".into()],
                max_concurrent: None,
                budget_usd_per_day: None,
            },
        )
        .await;
        let tok_id = app.api_tokens.authenticate(&token).await.unwrap().id;
        let bearer = Some(token.as_str());
        let owner = Some(app.api_token.as_str());
        let router = guard_router(&app);
        let body =
            r#"{"name":"Triage","repo":"acme/web","prompt":"Triage new issues","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/loops", bearer, Some(body)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        let made = body_json(res).await;
        assert_eq!(
            made["created_by_token"],
            tok_id.as_str(),
            "the loop records the token: {made}"
        );
        let mine = made["id"].as_str().unwrap().to_string();
        // The owner still sees every loop.
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/loops", owner, None))
            .await
            .unwrap();
        assert_eq!(body_json(res).await.as_array().unwrap().len(), 1);

        // Outside the repo limit: 403, the launch refusal's words. A map loop is refused outright:
        // its runs would launch outside the token's caps and marking.
        let outside = r#"{"name":"Spy","repo":"acme/api","prompt":"x","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/loops", bearer, Some(outside)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(body_text(res).await.contains("do not include acme/api"));
        let map = r#"{"name":"Maps","repo":"acme/web","kind":"map","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/loops", bearer, Some(map)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(body_text(res).await.contains("map loop"));

        // The list is filtered to the limits, as the colony list is: an owner's loop on another
        // repository is not in it, and its id reads as unknown everywhere.
        let owner_body = r#"{"name":"Owner's","repo":"other/web","prompt":"x","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/loops", owner, Some(owner_body)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        let res = router
            .clone()
            .oneshot(send(Method::GET, "/api/loops", bearer, None))
            .await
            .unwrap();
        let listed = body_json(res).await;
        let ids: Vec<&str> = listed.as_array().unwrap().iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec![mine.as_str()], "only the token's own loop is listed: {listed}");
        for (method, uri) in [
            (Method::PUT, "/api/loops/owner_loop".to_string()),
            (Method::DELETE, "/api/loops/owner_loop".to_string()),
            (Method::POST, "/api/loops/owner_loop/run-now".to_string()),
            (Method::GET, "/api/loops/owner_loop/runs".to_string()),
        ] {
            let res = router
                .clone()
                .oneshot(send(method.clone(), &uri, bearer, (method == Method::PUT).then_some(body)))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method} {uri} reads as unknown");
        }
        // Its own loop reads fine, and editing it keeps the token on the loop.
        let res = router
            .clone()
            .oneshot(send(Method::GET, &format!("/api/loops/{mine}/runs"), bearer, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let renamed = r#"{"name":"Triage renamed","repo":"acme/web","prompt":"Triage new issues","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::PUT, &format!("/api/loops/{mine}"), bearer, Some(renamed)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        assert_eq!(
            body_json(res).await["created_by_token"],
            tok_id.as_str(),
            "an edit cannot shed the token"
        );
        // And it deletes its own loop.
        let res = router
            .oneshot(send(Method::DELETE, &format!("/api/loops/{mine}"), bearer, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Run-now by the token passes the guard and the ownership check, and the colony it launches
    /// carries the token (issue #627). After revocation the loop's next run-now is refused (409)
    /// and the loop ends: nothing launches after revocation, even at the owner's hand.
    #[tokio::test]
    async fn run_now_on_a_token_loop_runs_under_the_token_until_it_is_revoked() {
        let root = root();
        std::fs::create_dir_all(root.join("config")).unwrap();
        // The smallest install `create` insists on, so the run-now launch completes here.
        let app = crate::sessions::tests::app_that_can_create(&root);
        let made = app
            .api_tokens
            .create(NewToken {
                name: "cron".into(),
                scope: "launch".into(),
                orgs: Vec::new(),
                repos: vec!["acme/app".into()],
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        let tok_id = made.meta.id.clone();
        let bearer = Some(made.token.as_str());
        let router = guard_router(&app);
        let body =
            r#"{"name":"Triage","repo":"acme/app","prompt":"Triage new issues","cadence":{"every":"interval","minutes":60}}"#;
        let res = router
            .clone()
            .oneshot(send(Method::POST, "/api/loops", bearer, Some(body)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        let id = body_json(res).await["id"].as_str().unwrap().to_string();
        let res = router
            .clone()
            .oneshot(send(Method::POST, &format!("/api/loops/{id}/run-now"), bearer, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{}", body_text(res).await);
        let run = body_json(res).await;
        assert_eq!(
            run["launched_by_token"],
            tok_id.as_str(),
            "the run-now colony knows the token it ran under: {run}"
        );
        // That run finished (the boot task is never polled; the test says so), so it holds the
        // loop no longer.
        let run_id = run["id"].as_str().unwrap().to_string();
        app.sessions.write().await.iter_mut().for_each(|s| {
            if s.id == run_id {
                s.status = SessionStatus::Stopped;
            }
        });
        // Revoking the token stops the loop at its next run, even at the owner's hand.
        assert!(app.api_tokens.revoke(&tok_id).await.is_some());
        let res = router
            .oneshot(send(
                Method::POST,
                &format!("/api/loops/{id}/run-now"),
                Some(app.api_token.as_str()),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT, "revocation stops the next run");
        assert!(body_text(res).await.contains("revoked"));
        let l = app.loops.get(&id).await.unwrap();
        assert!(!l.enabled && l.next_run_at.is_none(), "the loop is ended");
        assert_eq!(l.ended_reason.as_deref(), Some("its API token was revoked"));
        assert_eq!(l.runs, 1, "nothing launched after the revocation");
        let _ = std::fs::remove_dir_all(root);
    }
}
