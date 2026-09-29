//! Fleet membership (issue #686): one mothership invites, another joins, and both end up seeing
//! each other in `GET /api/hosts` — and nothing more.
//!
//! The pairing is out-of-band verified, so no machine hands a credential to a stranger: the owner
//! shows a single-use invite code, the joiner spends it at an unauthenticated route that takes
//! only the code, and both sides independently derive a short confirm code from the code and a
//! joiner-made nonce. A person compares the two confirm codes — equal only if both hold the same
//! code and nonce — and approves; approval mints a fleet-scoped API token, handed over exactly
//! once at the joiner's next poll. The token reaches `GET /api/hosts` and
//! `POST /api/fleet/peer/leave` (watching the fleet, leaving it), plus the history push's two ingest
//! routes (`fleet_sync.rs`, #762), and nothing else
//! ([`crate::api_tokens::Scope::Fleet`]). Leaving, from either side, revokes it.
//!
//! State lives in `<config_dir>/fleet.json` (0600), loaded never-fail and written through on
//! every change. The plaintexts it holds — the nonce while a pairing is open, the minted token
//! until pickup, and a member's own token for as long as it belongs — are what the protocol
//! needs; the cockpit API serves none of them.

use crate::{ApiResult, Shared, client_error, gateway::constant_time_eq, util};
use anyhow::{Result, anyhow};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::sync::RwLock;

/// How long an invite and a pending pairing stay alive: long enough to read a code aloud and
/// compare two numbers, short enough that a code shown yesterday is not a door left open today.
const PAIRING_TTL: Duration = Duration::minutes(15);
/// A member name is a label on the owner's screens; a URL is stored and later polled.
const MAX_NAME: usize = 64;
const MAX_URL: usize = 256;
/// The invite code: 16 Crockford base32 symbols in four groups — 80 bits of randomness, typed in
/// chunks. The letters Crockford excludes (I, L, O, U) are the ones that read as digits or words.
const CODE_SYMBOLS: usize = 16;
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// How long a call into the owner may take: the fleet pages are interactive, and a wedged owner
/// is a 502, not a hung page.
const OWNER_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// One open invitation: the code is stored only as its hash, like every credential here.
#[derive(Clone, Serialize, Deserialize)]
struct Invite {
    id: String,
    code_hash: String,
    expires_at: DateTime<Utc>,
}

/// Where a pairing stands. `Approved` still waits for the joiner to pick its token up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PairingStatus {
    Pending,
    Approved,
    Rejected,
}

/// One machine's half-joined state, keyed by the nonce only its joiner holds.
#[derive(Clone, Serialize, Deserialize)]
struct Pairing {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    nonce_hash: String,
    confirm_code: String,
    expires_at: DateTime<Utc>,
    status: PairingStatus,
    /// The minted fleet token, only while `Approved` and unpicked; gone with the pairing itself
    /// once the joiner takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    member_id: String,
}

/// A joined machine, kept for the fleet view and the poll list. `token_id` is what ties it to the
/// credential it holds — and what a removal or a leave revokes by.
#[derive(Clone, Serialize, Deserialize)]
struct Member {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    token_id: String,
    joined_at: DateTime<Utc>,
}

/// This mothership's own join, while its confirm code waits to be compared.
#[derive(Clone, Serialize, Deserialize)]
struct Joining {
    owner_url: String,
    pairing_id: String,
    nonce: String,
    confirm_code: String,
    started_at: DateTime<Utc>,
}

/// A membership that survived its confirm code: who we belong to, and the token that says so.
#[derive(Clone, Serialize, Deserialize)]
struct Membership {
    owner_url: String,
    member_id: String,
    token: String,
    joined_at: DateTime<Utc>,
    /// Consent to push this machine's history to the owner (`fleet_sync.rs`, #762): off at every
    /// join, turned on by the member's operator after seeing the preview. A new membership — a
    /// leave and a re-join — starts it off again.
    #[serde(default)]
    history_sync: bool,
}

/// A member the owner removed: its token's hash, kept after the token itself is revoked, so the
/// removed machine's next call reads 403 "removed from the fleet" rather than an anonymous 401.
#[derive(Clone, Serialize, Deserialize)]
struct Tombstone {
    member_id: String,
    token_hash: String,
    removed_at: DateTime<Utc>,
}

/// How many removals are remembered; the oldest go first.
const MAX_TOMBSTONES: usize = 256;

/// Everything the fleet knows, as persisted. Both roles live here: a mothership is a member, an
/// owner, or alone — never two of those at once.
#[derive(Default, Serialize, Deserialize)]
struct FleetState {
    #[serde(default)]
    invites: Vec<Invite>,
    #[serde(default)]
    pending: Vec<Pairing>,
    #[serde(default)]
    members: Vec<Member>,
    #[serde(default)]
    joining: Option<Joining>,
    #[serde(default)]
    membership: Option<Membership>,
    #[serde(default)]
    removed: Vec<Tombstone>,
}

impl FleetState {
    /// "member" when this mothership belongs to a fleet, "owner" when machines belong to it,
    /// "none" otherwise. Invites and pending pairings make no owner on their own — they are
    /// prospective.
    fn role(&self) -> &'static str {
        if self.membership.is_some() {
            "member"
        } else if !self.members.is_empty() {
            "owner"
        } else {
            "none"
        }
    }

    /// Drops what time has closed: expired invites, and pairings past theirs. An `Approved`
    /// pairing that expires unpicked leaves its member row behind — the operator sees it and
    /// removes it, which revokes the token nobody ever picked up.
    fn prune(&mut self) {
        let now = Utc::now();
        self.invites.retain(|i| i.expires_at > now);
        self.pending.retain(|p| p.expires_at > now);
    }

    /// The cockpit's view: everything but the secrets — no code hashes, no nonces, no tokens.
    fn view(&self) -> Value {
        json!({
            "role": self.role(),
            "invites": self.invites.iter().map(|i| json!({"id": i.id, "expires_at": i.expires_at})).collect::<Vec<_>>(),
            "pending": self.pending.iter().map(|p| json!({
                "id": p.id, "name": p.name, "url": p.url, "confirm_code": p.confirm_code,
                "expires_at": p.expires_at, "status": p.status,
            })).collect::<Vec<_>>(),
            "members": self.members.iter().map(|m| json!({
                "id": m.id, "name": m.name, "url": m.url, "joined_at": m.joined_at,
            })).collect::<Vec<_>>(),
            "membership": self.membership.as_ref().map(|m| json!({
                "owner_url": m.owner_url, "member_id": m.member_id, "joined_at": m.joined_at,
                "history_sync": m.history_sync,
            })),
            "joining": self.joining.as_ref().map(|j| json!({
                "owner_url": j.owner_url, "confirm_code": j.confirm_code, "started_at": j.started_at,
            })),
        })
    }
}

/// The fleet store: the state in memory behind a lock, written through to
/// `<config_dir>/fleet.json` on every change (a mutation — never a read).
pub struct FleetStore {
    state: RwLock<FleetState>,
    path: PathBuf,
}

impl FleetStore {
    /// Loads the store, never failing: a file that cannot be read or parsed is reported on
    /// stderr and answered with an empty fleet, which forgets every membership — the safe
    /// direction for a damaged one, and the rule the token registry beside it keeps.
    pub fn load(config_dir: &std::path::Path) -> FleetStore {
        let path = config_dir.join("fleet.json");
        let state = match std::fs::read(&path) {
            // A missing file is a first use, not a fault.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FleetState::default(),
            Err(e) => {
                eprintln!("fleet: could not read {} ({e}); starting with no fleet", path.display());
                FleetState::default()
            }
            Ok(bytes) => match serde_json::from_slice::<FleetState>(&bytes) {
                Ok(state) => state,
                Err(e) => {
                    eprintln!(
                        "fleet: {} does not parse ({e}); starting with no fleet until the file is repaired or removed",
                        path.display()
                    );
                    FleetState::default()
                }
            },
        };
        FleetStore {
            state: RwLock::new(state),
            path,
        }
    }

    /// Writes the state through. A failure is loud but not fatal: the change lives in memory for
    /// this run, and the next one retries the write.
    async fn save(&self, state: &FleetState) {
        let Ok(bytes) = serde_json::to_vec_pretty(state) else {
            return; // a fixed-shape struct cannot fail to serialize
        };
        if let Err(e) = util::write_private(&self.path, &bytes) {
            eprintln!("fleet: could not save {}: {e}", self.path.display());
        }
    }

    /// The fleet peers `GET /api/hosts` polls: on a member, its owner; on an owner, every member
    /// that published its URL. Empty when this mothership is alone.
    pub async fn peer_urls(&self) -> Vec<String> {
        let state = self.state.read().await;
        if let Some(m) = &state.membership {
            return vec![m.owner_url.clone()];
        }
        state.members.iter().filter_map(|m| m.url.clone()).collect()
    }

    /// Whether any machine belongs to this fleet — the mesh ACL's trigger.
    pub async fn has_members(&self) -> bool {
        !self.state.read().await.members.is_empty()
    }

    /// This mothership's own membership, when it has joined a fleet: where the owner answers,
    /// the member id the owner knows us by, and the fleet token that proves it. What the history
    /// drain (`fleet_sync.rs`, issue #762) pushes with.
    pub async fn membership(&self) -> Option<crate::fleet_sync::Target> {
        self.state
            .read()
            .await
            .membership
            .as_ref()
            .map(|m| crate::fleet_sync::Target {
                owner_url: m.owner_url.clone(),
                member_id: m.member_id.clone(),
                token: m.token.clone(),
            })
    }

    /// On an owner, the member a fleet token belongs to — `None` when the token names no current
    /// member (it left, or was removed while its token still authenticated).
    pub async fn member_for_token(&self, token_id: &str) -> Option<String> {
        let state = self.state.read().await;
        state.members.iter().find(|m| m.token_id == token_id).map(|m| m.id.clone())
    }

    /// Whether this member's operator has consented to the history push; `None` when this
    /// mothership belongs to no fleet.
    pub async fn history_sync(&self) -> Option<bool> {
        self.state.read().await.membership.as_ref().map(|m| m.history_sync)
    }

    /// Records the operator's consent (or its withdrawal) for the current membership. `false`
    /// when there is no membership to record it on.
    pub async fn set_history_sync(&self, enabled: bool) -> bool {
        let mut state = self.state.write().await;
        let Some(m) = state.membership.as_mut() else {
            return false;
        };
        m.history_sync = enabled;
        self.save(&state).await;
        true
    }

    /// Whether a presented Bearer token belongs to a member this owner removed.
    pub async fn is_removed_token(&self, presented: &str) -> bool {
        let hash = crate::api_tokens::hash_token(presented);
        self.state
            .read()
            .await
            .removed
            .iter()
            .any(|t| constant_time_eq(t.token_hash.as_bytes(), hash.as_bytes()))
    }

    /// Stands a membership up directly, as a completed join would: the history-drain tests.
    #[cfg(test)]
    pub(crate) async fn set_membership_for_tests(&self, target: Option<crate::fleet_sync::Target>) {
        let mut state = self.state.write().await;
        state.membership = target.map(|t| Membership {
            owner_url: t.owner_url,
            member_id: t.member_id,
            token: t.token,
            joined_at: Utc::now(),
            history_sync: false,
        });
        self.save(&state).await;
    }

    /// Adds a member directly, the token minted as approval would: the history-drain tests stand
    /// an owner up without walking the pairing each time. Answers `(member_id, token)`.
    #[cfg(test)]
    pub(crate) async fn add_member_for_tests(app: &Shared, name: &str) -> (String, String) {
        let (token, token_id) = app.api_tokens.create_fleet_token(name).await.unwrap();
        let id = format!("mem_{}", util::short_id());
        let mut state = app.fleet_members.state.write().await;
        state.members.push(Member {
            id: id.clone(),
            name: name.to_string(),
            url: None,
            token_id,
            joined_at: Utc::now(),
        });
        app.fleet_members.save(&state).await;
        (id, token)
    }

    /// Removes a member the way the owner's Remove does, for the history-drain tests.
    #[cfg(test)]
    pub(crate) async fn remove_member_for_tests(app: &Shared, id: &str) {
        remove_member_where(app, true, |m| m.id == id).await;
    }
}

// ---------------------------------------------------------------------------
// Codes and hashes.
// ---------------------------------------------------------------------------

/// SHA-256, lowercase hex — what every stored secret here is reduced to.
fn sha256_hex(data: &[u8]) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

/// Uppercases, drops dashes and whitespace, and folds the look-alike letters Crockford excludes,
/// so a code read aloud and typed back hashes to what was stored.
fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c {
            'i' | 'I' | 'l' | 'L' => '1',
            'o' | 'O' => '0',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

/// The pieces `ring`'s source of randomness is asked for, and what each becomes.
fn random_bytes(count: usize) -> Result<Vec<u8>> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = vec![0u8; count];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow!("the random source failed"))?;
    Ok(bytes)
}

/// A fresh invite code: `CODE_SYMBOLS` Crockford symbols in groups of four. A byte mod 32 is
/// uniform (256 splits evenly), so each symbol carries 5 bits — 80 in all.
fn new_invite_code() -> Result<String> {
    let mut code = String::new();
    for (at, byte) in random_bytes(CODE_SYMBOLS)?.iter().enumerate() {
        if at > 0 && at % 4 == 0 {
            code.push('-');
        }
        code.push(CROCKFORD[(*byte % 32) as usize] as char);
    }
    Ok(code)
}

/// The joiner's half of the pairing: 32 random bytes, hex.
fn new_nonce() -> Result<String> {
    Ok(util::hex(&random_bytes(32)?))
}

/// The confirm code both sides compute independently from the invite code and the joiner's nonce:
/// the digest's first four bytes as a number under a million, in two three-digit groups. Equal on
/// both screens means both hold the same code and the same nonce, whatever carried them.
fn confirm_code(normalized_code: &str, nonce: &str) -> String {
    let mut seeded = Vec::with_capacity(normalized_code.len() + nonce.len() + 22);
    seeded.extend_from_slice(b"colonizer-fleet-pair\0");
    seeded.extend_from_slice(normalized_code.as_bytes());
    seeded.extend_from_slice(b"\0");
    seeded.extend_from_slice(nonce.as_bytes());
    let digest = ring::digest::digest(&ring::digest::SHA256, &seeded);
    let number = u32::from_be_bytes(digest.as_ref()[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", number / 1000, number % 1000)
}

/// A URL a machine publishes about itself: an http(s) base URL, bounded. It is stored and later
/// dialed, so it is validated where it arrives rather than trusted.
fn clean_url(raw: Option<&str>) -> Result<Option<String>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(url) if url.len() <= MAX_URL && (url.starts_with("http://") || url.starts_with("https://")) => {
            Ok(Some(url.trim_end_matches('/').to_string()))
        }
        Some(_) => Err(format!("url must be an http(s) base URL of at most {MAX_URL} characters")),
    }
}

// ---------------------------------------------------------------------------
// Shared plumbing: the mesh rule, member removal, calls into the owner.
// ---------------------------------------------------------------------------

/// Rewrites the mesh policy from the current membership, so a joined machine's headscale user may
/// reach the harness node exactly while the fleet has members. Best effort, and silent where
/// there is no mesh to reconfigure.
async fn update_mesh_acl(app: &Shared) {
    if app.cfg.assets.is_none() {
        return; // no assets, no mesh: there is nothing to reconfigure (tests, mostly)
    }
    let members = app.fleet_members.has_members().await;
    match app.mesh().await {
        Ok(mesh) => mesh.set_fleet_acl(members).await,
        Err(e) => eprintln!("fleet: the mesh policy was not updated: {e:#}"),
    }
}

/// Removes the member `matches` names — remembering its token as removed when `tombstone` (the
/// owner's Remove, not the member's own leave): token revoked, row dropped, mesh node (`fleet-<id>`)
/// deleted and the rule closed if this was the last — both mesh steps best effort. `None` when
/// nothing matches.
async fn remove_member_where(app: &Shared, tombstone: bool, matches: impl Fn(&Member) -> bool) -> Option<Member> {
    let mut state = app.fleet_members.state.write().await;
    let at = state.members.iter().position(matches)?;
    let member = state.members.remove(at);
    if tombstone && let Some(token_hash) = app.api_tokens.token_hash(&member.token_id).await {
        // The owner's Remove: the revoked token keeps answering "removed from the fleet".
        state.removed.push(Tombstone {
            member_id: member.id.clone(),
            token_hash,
            removed_at: Utc::now(),
        });
        let excess = state.removed.len().saturating_sub(MAX_TOMBSTONES);
        state.removed.drain(..excess);
    }
    app.fleet_members.save(&state).await;
    drop(state);
    app.api_tokens.revoke(&member.token_id).await;
    if app.cfg.assets.is_some() {
        match app.mesh().await {
            Ok(mesh) => {
                if let Err(e) = mesh.delete_nodes_named(&format!("fleet-{}", member.id)).await {
                    eprintln!("fleet: the member's mesh node was not removed: {e:#}");
                }
            }
            Err(e) => eprintln!("fleet: the mesh was not reached: {e:#}"),
        }
    }
    update_mesh_acl(app).await;
    Some(member)
}

/// How much of the owner's answer is read: these are small JSON verdicts, and whatever answers in
/// the owner's name is not to be trusted with unbounded memory.
const MAX_OWNER_ANSWER: usize = 64 * 1024;

/// POSTs `body` to the owner — no redirects; a dead owner or an unusable client is the caller's
/// 502.
async fn post_owner(owner_url: &str, path: &str, bearer: Option<&str>, body: Value) -> Result<reqwest::Response> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut req = client.post(format!("{}{path}", owner_url.trim_end_matches('/')));
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    Ok(req.json(&body).timeout(OWNER_CALL_TIMEOUT).send().await?)
}

/// The owner's JSON answer, bounded, with its errors mapped for the operator: its 4xx names the
/// problem and passes through — a 404 as a 400, the joiner's own input being the spent code —
/// while its auth wall and its 5xx come back as 502, the other machine's fault and not a bug of
/// ours.
async fn owner_json(res: reqwest::Response) -> std::result::Result<Value, crate::AppError> {
    let status = res.status();
    let body = read_bounded(res).await?;
    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("the owner answered {status}"));
        let code = match status.as_u16() {
            401 | 403 => StatusCode::BAD_GATEWAY,
            404 => StatusCode::BAD_REQUEST,
            n if (400..500).contains(&n) => status,
            _ => StatusCode::BAD_GATEWAY,
        };
        return Err(client_error(code, &message));
    }
    serde_json::from_slice(&body)
        .map_err(|_| client_error(StatusCode::BAD_GATEWAY, "the owner's answer was not the expected JSON"))
}

/// Reads a response body up to [`MAX_OWNER_ANSWER`]; more is a 502, not a slower death.
async fn read_bounded(mut res: reqwest::Response) -> std::result::Result<Vec<u8>, crate::AppError> {
    if res.content_length().is_some_and(|len| len > MAX_OWNER_ANSWER as u64) {
        return Err(client_error(StatusCode::BAD_GATEWAY, "the owner's answer is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|_| client_error(StatusCode::BAD_GATEWAY, "the owner's answer could not be read"))?
    {
        if body.len() + chunk.len() > MAX_OWNER_ANSWER {
            return Err(client_error(StatusCode::BAD_GATEWAY, "the owner's answer is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// The cockpit's routes. Owner-only: `api_tokens::classify` leaves them there.
// ---------------------------------------------------------------------------

/// `GET /api/fleet`: the state both pages need, minus every secret — and on each member, its
/// `health` (issue #764): `{state, code, reason, hint}`, from signals this owner already holds.
/// Reading it never dials a member; the numbers are those of the last `GET /api/hosts` poll.
pub async fn view(State(app): State<Shared>) -> Json<Value> {
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    let mut view = state.view();
    let members = state.members.clone();
    drop(state);
    if let Some(rows) = view["members"].as_array_mut() {
        for (row, member) in rows.iter_mut().zip(&members) {
            let signals = member_signals(&app, member).await;
            row["health"] = crate::fleet_health::evaluate(&signals).to_json();
        }
    }
    Json(view)
}

/// What this owner knows about one member, as health signals (fleet_health.rs names which are
/// wired). The heartbeat's age is measured at the latest poll, not now: a member does not go
/// stale because the cockpit was closed; a member never answered counts from when it joined.
async fn member_signals(app: &Shared, member: &Member) -> crate::fleet_health::Signals {
    let token_valid = Some(app.api_tokens.scoped(&member.token_id).await.is_some());
    let observation = match &member.url {
        Some(url) => app.fleet_cache.observation(url).await,
        None => None,
    };
    let mut signals = crate::fleet_health::Signals {
        token_valid,
        has_url: member.url.is_some(),
        ..Default::default()
    };
    if let Some(seen) = observation {
        let since = seen.last_answer.unwrap_or(member.joined_at).max(member.joined_at);
        signals.heartbeat_age = Some((seen.polled_at - since).max(Duration::zero()));
        signals.reachable = Some(seen.reachable);
        signals.disk_free_bytes = seen.disk_free_bytes;
        signals.disk_total_bytes = seen.disk_total_bytes;
        signals.runner_tick_age = seen.runner_tick_age_s.map(Duration::seconds);
        if let Some(sync) = seen.fleet_sync {
            signals.sync_consent = Some(sync.consent);
            signals.last_sync_error = sync
                .last_error_class
                .as_deref()
                .and_then(crate::fleet_health::SyncError::from_class);
            signals.sync_backlog_rows = Some(sync.backlog_rows);
            signals.sync_backlog_age = sync.oldest_unsent_age_s.map(Duration::seconds);
        }
    }
    signals
}

/// `POST /api/fleet/invites`: a fresh single-use code, shown here and never again.
pub async fn create_invite(State(app): State<Shared>) -> ApiResult<Value> {
    if app.fleet_members.state.read().await.membership.is_some() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this mothership has joined a fleet; only a fleet's owner invites",
        ));
    }
    let code = new_invite_code().map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let invite = Invite {
        id: format!("inv_{}", util::short_id()),
        code_hash: sha256_hex(normalize_code(&code).as_bytes()),
        expires_at: Utc::now() + PAIRING_TTL,
    };
    let id = invite.id.clone();
    let expires_at = invite.expires_at;
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    state.invites.push(invite);
    app.fleet_members.save(&state).await;
    Ok(Json(json!({"id": id, "code": code, "expires_at": expires_at})))
}

/// `DELETE /api/fleet/invites/{id}`: take an invitation back before it is spent.
pub async fn delete_invite(State(app): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, crate::AppError> {
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    let before = state.invites.len();
    state.invites.retain(|i| i.id != id);
    if state.invites.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no such invite"));
    }
    app.fleet_members.save(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/fleet/pending/{id}/approve`: mint the member's token, add it, and open the mesh.
pub async fn approve(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    if state.membership.is_some() {
        // This mothership joined a fleet itself; it cannot be another fleet's owner too.
        return Err(client_error(
            StatusCode::CONFLICT,
            "this mothership has joined a fleet and cannot own one",
        ));
    }
    let Some(pairing) = state.pending.iter_mut().find(|p| p.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such pairing"));
    };
    if pairing.status != PairingStatus::Pending {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("this pairing was already {:?}", pairing.status),
        ));
    }
    let (token, token_id) = app
        .api_tokens
        .create_fleet_token(&pairing.name)
        .await
        .map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    pairing.status = PairingStatus::Approved;
    pairing.token = Some(token);
    let member = Member {
        id: pairing.member_id.clone(),
        name: pairing.name.clone(),
        url: pairing.url.clone(),
        token_id,
        joined_at: Utc::now(),
    };
    let answer = json!({"member": {
        "id": member.id, "name": member.name, "url": member.url, "joined_at": member.joined_at,
    }});
    state.members.push(member);
    app.fleet_members.save(&state).await;
    drop(state);
    update_mesh_acl(&app).await;
    Ok(Json(answer))
}

/// `POST /api/fleet/pending/{id}/reject`: no. The pairing stays until it expires, so the joiner's
/// next poll reads `rejected` instead of `unknown`.
pub async fn reject(State(app): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, crate::AppError> {
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    let Some(pairing) = state.pending.iter_mut().find(|p| p.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such pairing"));
    };
    if pairing.status != PairingStatus::Pending {
        return Err(client_error(
            StatusCode::CONFLICT,
            &format!("this pairing was already {:?}", pairing.status),
        ));
    }
    pairing.status = PairingStatus::Rejected;
    app.fleet_members.save(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/fleet/members/{id}`: remove a machine — token revoked, mesh rule closed.
pub async fn remove(State(app): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, crate::AppError> {
    if remove_member_where(&app, true, |m| m.id == id).await.is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no such member"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The body of `POST /api/fleet/join`.
#[derive(Deserialize)]
pub struct JoinBody {
    owner_url: String,
    code: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// `POST /api/fleet/join`: spend an invite code at the owner, hold the pairing open, and show the
/// confirm code a person compares against the owner's screen.
pub async fn join(State(app): State<Shared>, Json(req): Json<JoinBody>) -> ApiResult<Value> {
    let owner_url = req.owner_url.trim().trim_end_matches('/').to_string();
    if !owner_url.starts_with("http://") && !owner_url.starts_with("https://") {
        return Err(client_error(StatusCode::BAD_REQUEST, "owner_url must be an http(s) base URL"));
    }
    let normalized = normalize_code(&req.code);
    if normalized.chars().count() != CODE_SYMBOLS {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "the invite code is 16 characters in four groups, like XXXX-XXXX-XXXX-XXXX",
        ));
    }
    let url = clean_url(req.url.as_deref()).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    {
        let state = app.fleet_members.state.read().await;
        if state.membership.is_some() || !state.members.is_empty() {
            return Err(client_error(
                StatusCode::CONFLICT,
                "this mothership already has a fleet of its own; leave or disband it before joining another",
            ));
        }
    }
    let name = match req.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => crate::runtime::probe_hostname()
            .await
            .unwrap_or_else(|| "colonizer".to_string()),
    };
    if name.len() > MAX_NAME {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("name must be at most {MAX_NAME} characters"),
        ));
    }
    let nonce = new_nonce().map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let confirm = confirm_code(&normalized, &nonce);
    let redeem = post_owner(
        &owner_url,
        "/api/fleet/peer/redeem",
        None,
        json!({"code": normalized, "nonce": nonce, "name": name, "url": url}),
    )
    .await;
    let redeem = match redeem {
        Ok(redeem) => owner_json(redeem).await?,
        Err(e) => {
            return Err(client_error(
                StatusCode::BAD_GATEWAY,
                &format!("the owner could not be reached: {e:#}"),
            ));
        }
    };
    let Some(pairing_id) = redeem["pairing_id"].as_str().map(str::to_string) else {
        return Err(client_error(StatusCode::BAD_GATEWAY, "the owner's answer named no pairing"));
    };
    if redeem["confirm_code"].as_str() != Some(confirm.as_str()) {
        // Whatever carried the code and the nonce between the two machines did not carry them
        // whole. Nothing is stored: the invite is spent, and the operator starts over.
        return Err(client_error(
            StatusCode::BAD_GATEWAY,
            "the owner's confirm code does not match ours; the pairing is not trusted, so nothing was joined",
        ));
    }
    let mut state = app.fleet_members.state.write().await;
    // Re-checked under the write lock: a fleet of our own may have appeared while the owner had
    // the connection.
    if state.membership.is_some() || !state.members.is_empty() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "this mothership already has a fleet of its own; leave or disband it before joining another",
        ));
    }
    state.joining = Some(Joining {
        owner_url,
        pairing_id,
        nonce,
        confirm_code: confirm.clone(),
        started_at: Utc::now(),
    });
    app.fleet_members.save(&state).await;
    Ok(Json(json!({"confirm_code": confirm, "status": "pending"})))
}

/// `POST /api/fleet/join/confirm`: the codes matched — poll the owner for the verdict. The
/// pairing stays in `joining` throughout, so a cancel or a fresh join during the call into the
/// owner wins: every outcome below acts only while the slot still holds this pairing.
pub async fn join_confirm(State(app): State<Shared>) -> ApiResult<Value> {
    let joining = {
        let mut state = app.fleet_members.state.write().await;
        state.prune();
        state.joining.clone()
    };
    let Some(joining) = joining else {
        return Err(client_error(StatusCode::CONFLICT, "no pairing is in progress"));
    };
    let res = post_owner(
        &joining.owner_url,
        &format!("/api/fleet/peer/pairings/{}", joining.pairing_id),
        None,
        json!({"nonce": joining.nonce}),
    )
    .await;
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            // The pairing itself is untouched: the operator can confirm again.
            return Err(client_error(
                StatusCode::BAD_GATEWAY,
                &format!("the owner could not be reached: {e:#}"),
            ));
        }
    };
    if res.status() == StatusCode::NOT_FOUND {
        // Gone: expired, picked up by nobody who held the nonce, or never ours. The join is over.
        end_joining(&app, &joining.pairing_id).await;
        return Ok(Json(json!({"status": "expired"})));
    }
    let answer = owner_json(res).await?;
    match answer["status"].as_str() {
        Some("pending") => Ok(Json(json!({"status": "pending"}))),
        Some("rejected") => {
            end_joining(&app, &joining.pairing_id).await;
            Ok(Json(json!({"status": "rejected"})))
        }
        Some("approved") => {
            let (Some(token), Some(member_id)) = (
                answer["token"].as_str().map(str::to_string),
                answer["member_id"].as_str().map(str::to_string),
            ) else {
                return Err(client_error(StatusCode::BAD_GATEWAY, "the owner's approval named no token"));
            };
            let mut state = app.fleet_members.state.write().await;
            if !state.joining.as_ref().is_some_and(|j| j.pairing_id == joining.pairing_id) {
                return Err(client_error(
                    StatusCode::CONFLICT,
                    "the pairing in progress changed; join again",
                ));
            }
            state.joining = None;
            state.membership = Some(Membership {
                owner_url: joining.owner_url,
                member_id,
                token,
                joined_at: Utc::now(),
                // Joining is not consent to push history: the operator turns it on after the
                // preview (`POST /api/fleet/sync/consent`).
                history_sync: false,
            });
            app.fleet_members.save(&state).await;
            Ok(Json(json!({"status": "joined"})))
        }
        _ => Err(client_error(
            StatusCode::BAD_GATEWAY,
            "the owner's answer said nothing this join knows",
        )),
    }
}

/// Ends the pairing `pairing_id` — clears `joining` only while the slot still holds it. A cancel
/// or a fresh join during a confirm has already replaced or removed it; that one wins.
async fn end_joining(app: &Shared, pairing_id: &str) {
    let mut state = app.fleet_members.state.write().await;
    if state.joining.as_ref().is_some_and(|j| j.pairing_id == pairing_id) {
        state.joining = None;
        app.fleet_members.save(&state).await;
    }
}

/// `DELETE /api/fleet/join`: walk away from a pairing still waiting to be compared.
pub async fn cancel_join(State(app): State<Shared>) -> Result<StatusCode, crate::AppError> {
    let mut state = app.fleet_members.state.write().await;
    if state.joining.take().is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no pairing is in progress"));
    }
    app.fleet_members.save(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/fleet/leave`: tell the owner (best effort — a 401 or a 404 only means it already
/// dropped us), then forget the membership. Everything local stays.
pub async fn leave(State(app): State<Shared>) -> Result<StatusCode, crate::AppError> {
    let (owner_url, token) = {
        let state = app.fleet_members.state.read().await;
        let Some(m) = &state.membership else {
            return Err(client_error(StatusCode::CONFLICT, "this mothership has not joined a fleet"));
        };
        (m.owner_url.clone(), m.token.clone())
    };
    if let Ok(res) = post_owner(&owner_url, "/api/fleet/peer/leave", Some(&token), json!({})).await {
        let status = res.status();
        if !(status.is_success() || status == StatusCode::UNAUTHORIZED || status == StatusCode::NOT_FOUND) {
            eprintln!("fleet: the owner answered {status} to our leave; leaving locally anyway");
        }
    }
    let mut state = app.fleet_members.state.write().await;
    state.membership = None;
    app.fleet_members.save(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// The owner's peer routes, machine-to-machine: `host_guard` admits the first two
// unauthenticated (they are authenticated by what they carry), the third by a fleet token.
// ---------------------------------------------------------------------------

/// The body of `POST /api/fleet/peer/redeem`.
#[derive(Deserialize)]
pub struct RedeemBody {
    code: String,
    nonce: String,
    name: String,
    #[serde(default)]
    url: Option<String>,
}

/// `POST /api/fleet/peer/redeem`: spend an invite. The code is the whole authentication, so a
/// spent, expired or wrong one reads exactly the same — 404, like any unknown route.
pub async fn peer_redeem(State(app): State<Shared>, Json(req): Json<RedeemBody>) -> ApiResult<Value> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("name must be 1 to {MAX_NAME} characters"),
        ));
    }
    let url = clean_url(req.url.as_deref()).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    let nonce = req.nonce;
    if nonce.is_empty() || nonce.len() > 128 || nonce.chars().any(char::is_whitespace) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "nonce must be 1 to 128 characters with no whitespace",
        ));
    }
    let normalized = normalize_code(&req.code);
    if normalized.chars().count() != CODE_SYMBOLS {
        return Err(client_error(StatusCode::NOT_FOUND, "no such invite"));
    }
    let hash = sha256_hex(normalized.as_bytes());
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    if state.membership.is_some() {
        // This mothership joined a fleet itself; it cannot be another fleet's owner too. The
        // answer is the same 404 a spent or wrong code gets.
        return Err(client_error(StatusCode::NOT_FOUND, "no such invite"));
    }
    // Single-use, whatever follows: the first presentation of the code spends the invite.
    let Some(at) = state
        .invites
        .iter()
        .position(|i| constant_time_eq(i.code_hash.as_bytes(), hash.as_bytes()))
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such invite"));
    };
    state.invites.remove(at);
    let pairing = Pairing {
        id: format!("pair_{}", util::short_id()),
        name: name.to_string(),
        url,
        nonce_hash: sha256_hex(nonce.as_bytes()),
        confirm_code: confirm_code(&normalized, &nonce),
        expires_at: Utc::now() + PAIRING_TTL,
        status: PairingStatus::Pending,
        token: None,
        member_id: format!("mem_{}", util::short_id()),
    };
    let answer = json!({"pairing_id": pairing.id, "confirm_code": pairing.confirm_code});
    state.pending.push(pairing);
    app.fleet_members.save(&state).await;
    Ok(Json(answer))
}

/// The body of `POST /api/fleet/peer/pairings/{id}`.
#[derive(Deserialize)]
pub struct NonceBody {
    nonce: String,
}

/// `POST /api/fleet/peer/pairings/{id}`: the joiner's poll. The nonce is the whole
/// authentication, and a wrong one is the same 404 a wrong id gets.
pub async fn peer_pairing(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<NonceBody>) -> ApiResult<Value> {
    let mut state = app.fleet_members.state.write().await;
    state.prune();
    let Some(pairing) = state.pending.iter().find(|p| p.id == id) else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such pairing"));
    };
    if !constant_time_eq(pairing.nonce_hash.as_bytes(), sha256_hex(req.nonce.as_bytes()).as_bytes()) {
        return Err(client_error(StatusCode::NOT_FOUND, "no such pairing"));
    }
    match pairing.status {
        PairingStatus::Pending => Ok(Json(json!({"status": "pending"}))),
        PairingStatus::Rejected => Ok(Json(json!({"status": "rejected"}))),
        PairingStatus::Approved => {
            // The handover is the pairing's last act: the token leaves exactly once, and the
            // entry goes with it.
            let (Some(token), member_id) = (pairing.token.clone(), pairing.member_id.clone()) else {
                return Err(client_error(StatusCode::NOT_FOUND, "no such pairing"));
            };
            state.pending.retain(|p| p.id != id);
            app.fleet_members.save(&state).await;
            Ok(Json(json!({"status": "approved", "token": token, "member_id": member_id})))
        }
    }
}

/// `POST /api/fleet/peer/leave`: a member drops itself. The fleet token in the request is the
/// member — the route is `Need::Fleet`, so its scope has already been checked by the guard.
pub async fn peer_leave(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Result<StatusCode, crate::AppError> {
    let Some(axum::Extension(token)) = scoped else {
        // The owner's own token passes the guard ahead of any scope check; leaving is a member's
        // act, keyed by a member's token.
        return Err(client_error(StatusCode::FORBIDDEN, "this route takes a fleet token"));
    };
    if token.scope != crate::api_tokens::Scope::Fleet {
        return Err(client_error(StatusCode::FORBIDDEN, "this route takes a fleet token"));
    }
    if remove_member_where(&app, false, |m| m.token_id == token.id).await.is_none() {
        // No matching member is already-left, not an error; the token still goes.
        app.api_tokens.revoke(&token.id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/api/fleet", get(view))
        .route("/api/fleet/invites", post(create_invite))
        .route("/api/fleet/invites/{id}", delete(delete_invite))
        .route("/api/fleet/pending/{id}/approve", post(approve))
        .route("/api/fleet/pending/{id}/reject", post(reject))
        .route("/api/fleet/members/{id}", delete(remove))
        .route("/api/fleet/join", post(join).delete(cancel_join))
        .route("/api/fleet/join/confirm", post(join_confirm))
        .route("/api/fleet/leave", post(leave))
        .route("/api/fleet/peer/redeem", post(peer_redeem))
        .route("/api/fleet/peer/pairings/{id}", post(peer_pairing))
        .route("/api/fleet/peer/leave", post(peer_leave))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_tokens::{self, NewToken};
    use crate::tests::test_app;
    use axum::{
        Router,
        http::{Method, Request, header},
        middleware,
    };
    use tower::ServiceExt as _;

    /// A temp config root that removes itself, so a failed test cannot leak it.
    struct TempRoot(std::path::PathBuf);
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The fleet routes behind the real `host_guard`, so the tests exercise the unauthenticated
    /// peer allowlist and the owner wall the way production does. The hosts stub answers the shape
    /// the real handler answers.
    fn fleet_router(app: &Shared) -> Router<()> {
        Router::new()
            .route(
                "/api/hosts",
                axum::routing::get(|| async { axum::Json(json!({"hosts": []})) }),
            )
            .merge(routes())
            .layer(middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    /// A fresh app behind that router, with its owner token — the rig most tests share.
    fn rig() -> (TempRoot, Shared, Router<()>, Option<String>) {
        let dir = std::env::temp_dir().join(format!("colonizer-fleet-members-{}", util::short_id()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let root = TempRoot(dir);
        let app = test_app(&root.0);
        let router = fleet_router(&app);
        let owner = Some(app.api_token.clone());
        (root, app, router, owner)
    }

    /// A request through the guard: loopback Host, an optional Bearer, an optional JSON body.
    fn send(method: Method, uri: &str, bearer: Option<&str>, body: Option<String>) -> Request<axum::body::Body> {
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
        builder.body(axum::body::Body::from(body.unwrap_or_default())).unwrap()
    }

    /// `(status, JSON)` of a request — the shape most assertions take. A body that is not JSON
    /// (a `204`, the guard's plain-text 401) reads as null, so the status carries the assertion.
    async fn ask(
        router: &Router<()>,
        method: Method,
        uri: &str,
        bearer: Option<&str>,
        body: Option<String>,
    ) -> (StatusCode, Value) {
        let res = router.clone().oneshot(send(method, uri, bearer, body)).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let answer = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, answer)
    }

    /// The fleet view `bearer` sees.
    async fn fleet_view(router: &Router<()>, bearer: Option<&str>) -> Value {
        ask(router, Method::GET, "/api/fleet", bearer, None).await.1
    }

    /// The joiner's poll of `pairing_id` with `nonce`.
    async fn pickup(router: &Router<()>, pairing_id: &str, nonce: &str) -> (StatusCode, Value) {
        ask(
            router,
            Method::POST,
            &format!("/api/fleet/peer/pairings/{pairing_id}"),
            None,
            Some(json!({"nonce": nonce}).to_string()),
        )
        .await
    }

    /// An invite on `app`, created the way the cockpit creates one; answers the plaintext code.
    async fn an_invite(router: &Router<()>, owner_token: &str) -> String {
        let (status, answer) = ask(router, Method::POST, "/api/fleet/invites", Some(owner_token), None).await;
        assert_eq!(status, StatusCode::OK);
        answer["code"].as_str().unwrap().to_string()
    }

    /// One redemption of `code`: the nonce the test played the joiner with, and the pairing id
    /// and confirm code the owner answered.
    struct Redeemed {
        pairing_id: String,
        confirm: String,
        nonce: String,
    }

    async fn redeem(router: &Router<()>, code: &str) -> Redeemed {
        let nonce = new_nonce().unwrap();
        let body = json!({"code": code, "nonce": nonce, "name": "worker", "url": "http://10.0.0.2:7878"}).to_string();
        let (status, answer) = ask(router, Method::POST, "/api/fleet/peer/redeem", None, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "the unauthenticated redeem is admitted");
        Redeemed {
            pairing_id: answer["pairing_id"].as_str().unwrap().to_string(),
            confirm: answer["confirm_code"].as_str().unwrap().to_string(),
            nonce,
        }
    }

    /// The whole owner-side pairing: redeem an invite, approve it, and the joiner picks its token
    /// up. Answers `(member_id, member_token)`.
    async fn join_member(router: &Router<()>, owner: Option<&str>, code: &str) -> (String, String) {
        let pairing = redeem(router, code).await;
        let approve = format!("/api/fleet/pending/{}/approve", pairing.pairing_id);
        let (status, answer) = ask(router, Method::POST, &approve, owner, None).await;
        assert_eq!(status, StatusCode::OK);
        let member = answer["member"]["id"].as_str().unwrap().to_string();
        let (status, handed) = pickup(router, &pairing.pairing_id, &pairing.nonce).await;
        assert_eq!(status, StatusCode::OK);
        (member, handed["token"].as_str().unwrap().to_string())
    }

    /// A token minted through the cockpit's form; `Err` is the form's refusal.
    async fn form_token(app: &Shared, name: &str, scope: &str) -> Result<String, String> {
        app.api_tokens
            .create(NewToken {
                name: name.into(),
                scope: scope.into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .map(|created| created.token)
    }

    #[tokio::test]
    async fn an_invite_redeems_once_and_both_sides_derive_the_same_confirm_code() {
        let (root, app, router, _owner) = rig();
        let code = an_invite(&router, &app.api_token).await;

        // The joiner's own computation, from the code it read and a nonce it made.
        let nonce = new_nonce().unwrap();
        let mine = confirm_code(&normalize_code(&code), &nonce);
        let body = json!({"code": code.to_lowercase(), "nonce": nonce, "name": "worker"}).to_string();
        let (_, answer) = ask(&router, Method::POST, "/api/fleet/peer/redeem", None, Some(body)).await;
        assert_eq!(answer["confirm_code"], mine, "both sides derive the same number");
        assert_eq!(mine.split(' ').count(), 2, "{mine} reads as two three-digit groups");

        // A different nonce derives a different number: the code alone decides nothing.
        assert_ne!(confirm_code(&normalize_code(&code), &new_nonce().unwrap()), mine);

        // The plaintext code never reached the file; a second redeem is a 404, like a wrong one.
        let raw = std::fs::read_to_string(root.0.join("config/fleet.json")).unwrap();
        assert!(!raw.contains(&code), "the code is stored only hashed: {raw}");
        let again = json!({"code": code, "nonce": new_nonce().unwrap(), "name": "worker"}).to_string();
        let (status, _) = ask(&router, Method::POST, "/api/fleet/peer/redeem", None, Some(again)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "single-use");
    }

    /// An expired invite redeems as unknown, the same answer a wrong code gets.
    #[tokio::test]
    async fn an_expired_invite_redeems_as_unknown() {
        let (_root, app, router, _owner) = rig();
        let code = "AAAA-AAAA-AAAA-AAAA";
        app.fleet_members.state.write().await.invites.push(Invite {
            id: "inv_old".into(),
            code_hash: sha256_hex(normalize_code(code).as_bytes()),
            expires_at: Utc::now() - Duration::seconds(1),
        });
        let body = json!({"code": code, "nonce": new_nonce().unwrap(), "name": "worker"}).to_string();
        let (status, _) = ask(&router, Method::POST, "/api/fleet/peer/redeem", None, Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "time has closed this door");
    }

    /// Approve mints the token and adds the member; the pickup hands the token over exactly once;
    /// a rejected pairing says so; nothing unbounded gets in through the open route.
    #[tokio::test]
    async fn approve_hands_the_token_over_once_and_reject_reads_as_rejected() {
        let (_root, app, router, owner) = rig();
        let owner = owner.as_deref();

        // Bounds are checked where the unauthenticated body lands.
        let long = json!({"code": "XXXX-XXXX-XXXX-XXXX", "nonce": new_nonce().unwrap(), "name": "x".repeat(65)}).to_string();
        let (status, _) = ask(&router, Method::POST, "/api/fleet/peer/redeem", None, Some(long)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a 65-character name is refused");

        let pairing = redeem(&router, &an_invite(&router, &app.api_token).await).await;
        let approve_uri = format!("/api/fleet/pending/{}/approve", pairing.pairing_id);
        let (status, answer) = ask(&router, Method::POST, &approve_uri, owner, None).await;
        assert_eq!(status, StatusCode::OK);
        let member = answer["member"]["id"].as_str().unwrap().to_string();
        let view = fleet_view(&router, owner).await;
        assert_eq!(view["role"], "owner");
        assert_eq!(view["pending"][0]["status"], "approved");
        assert_eq!(view["pending"][0]["confirm_code"], pairing.confirm);
        assert_eq!(view["members"][0]["id"], member);
        let stamped = view["members"][0]["joined_at"].as_str().is_some();
        assert!(stamped, "timestamps are RFC 3339 strings");
        let shown = serde_json::to_string(&view).unwrap();
        assert!(!shown.contains("col_"), "no token in the view");

        // The pickup: a wrong nonce is the same 404 a wrong id gets; the right one hands the
        // token over; the entry is gone, and a second pickup finds nothing.
        let (status, _) = pickup(&router, &pairing.pairing_id, "wrong").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a wrong nonce is unknown");
        let (status, handed) = pickup(&router, &pairing.pairing_id, &pairing.nonce).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(handed["status"], "approved");
        assert_eq!(handed["member_id"], member);
        assert!(handed["token"].as_str().unwrap().starts_with("col_"));
        let (status, _) = pickup(&router, &pairing.pairing_id, &pairing.nonce).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "the token left exactly once");

        // The reject path: a fresh pairing, rejected before approval, says so at the poll — and
        // stays unredeemable by an approve.
        let pairing = redeem(&router, &an_invite(&router, &app.api_token).await).await;
        let reject_uri = format!("/api/fleet/pending/{}/reject", pairing.pairing_id);
        let (status, _) = ask(&router, Method::POST, &reject_uri, owner, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = ask(&router, Method::POST, &reject_uri, owner, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "a second reject is a conflict, not a redo");
        let (status, _) = ask(&router, Method::POST, &reject_uri.replace("reject", "approve"), owner, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "a rejected pairing cannot be approved");
        let (_, answer) = pickup(&router, &pairing.pairing_id, &pairing.nonce).await;
        assert_eq!(answer["status"], "rejected");
    }

    /// Role exclusivity: a mothership that is itself a member can neither invite nor approve, and
    /// its redeem door reads as unknown — the very answer a spent code gets.
    #[tokio::test]
    async fn a_member_mothership_cannot_redeem_invite_or_approve() {
        let (_root, app, router, owner) = rig();
        let owner = owner.as_deref();
        app.fleet_members.state.write().await.membership = Some(Membership {
            owner_url: "http://owner.example".into(),
            member_id: "mem_us".into(),
            token: "col_ours".into(),
            joined_at: Utc::now(),
            history_sync: false,
        });
        let body = json!({"code": "AAAA-AAAA-AAAA-AAAA", "nonce": new_nonce().unwrap(), "name": "worker"}).to_string();
        let (status, _) = ask(&router, Method::POST, "/api/fleet/peer/redeem", None, Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "indistinguishable from a spent code");
        let (status, _) = ask(&router, Method::POST, "/api/fleet/invites", owner, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = ask(&router, Method::POST, "/api/fleet/pending/pair_x/approve", owner, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    /// A confirm's cleanup only ends the pairing still in the slot: a cancel or a fresh join
    /// during the call into the owner is neither undone nor clobbered.
    #[tokio::test]
    async fn a_confirms_cleanup_never_touches_a_newer_joining() {
        let (_root, app, _router, _owner) = rig();
        let joining = |pairing_id: &str, owner_url: &str| Joining {
            owner_url: owner_url.into(),
            pairing_id: pairing_id.into(),
            nonce: "nonce".into(),
            confirm_code: "000 000".into(),
            started_at: Utc::now(),
        };
        app.fleet_members.state.write().await.joining = Some(joining("pair_old", "http://owner.example"));
        // The slot changed hands while the old pairing's confirm was in flight.
        app.fleet_members.state.write().await.joining = Some(joining("pair_new", "http://other.example"));
        end_joining(&app, "pair_old").await;
        let state = app.fleet_members.state.read().await;
        let surviving = state.joining.as_ref().unwrap().pairing_id.clone();
        assert_eq!(surviving, "pair_new", "the newer pairing survives");
        drop(state);
        end_joining(&app, "pair_new").await;
        assert!(app.fleet_members.state.read().await.joining.is_none());
    }

    /// The fleet scope reaches the fleet's routes and nothing else; the token form cannot mint
    /// it; and a revoked fleet token authenticates as nothing.
    #[tokio::test]
    async fn a_fleet_token_watches_the_hosts_list_and_leaves_and_nothing_else() {
        let (_root, app, router, _owner) = rig();
        let (token, _id) = app.api_tokens.create_fleet_token("worker").await.unwrap();
        let fleet = app.api_tokens.authenticate(&token).await.unwrap();
        assert_eq!(fleet.scope, api_tokens::Scope::Fleet);

        // Its two routes: the hosts list, and leaving.
        assert!(api_tokens::authorize(&app, &fleet, &Method::GET, "/api/hosts").await.is_ok());
        assert!(
            api_tokens::authorize(&app, &fleet, &Method::POST, "/api/fleet/peer/leave")
                .await
                .is_ok(),
            "a member may drop itself"
        );
        // Everything else is out of scope — watching colonies included.
        for (method, path) in [
            (&Method::GET, "/api/sessions"),
            (&Method::POST, "/api/sessions"),
            (&Method::GET, "/api/fleet"),
            (&Method::POST, "/api/fleet/invites"),
            (&Method::DELETE, "/api/fleet/members/mem_x"),
        ] {
            assert!(
                matches!(
                    api_tokens::authorize(&app, &fleet, method, path).await,
                    Err(api_tokens::Deny::Forbidden(_))
                ),
                "{method} {path} is beyond a fleet token"
            );
        }
        // A read token is below the fleet routes, where before this change no token reached at all.
        let read_token = form_token(&app, "ci", "read").await.unwrap();
        let read = app.api_tokens.authenticate(&read_token).await.unwrap();
        assert!(matches!(
            api_tokens::authorize(&app, &read, &Method::GET, "/api/hosts").await,
            Err(api_tokens::Deny::Forbidden(_))
        ));

        // Through the guard: the fleet token reaches a route, and once revoked it is a 401.
        let (status, _) = ask(&router, Method::GET, "/api/hosts", Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        app.api_tokens.revoke(&fleet.id).await;
        assert!(app.api_tokens.authenticate(&token).await.is_none(), "revocation is immediate");
        let (status, _) = ask(&router, Method::GET, "/api/hosts", Some(&token), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked fleet token is nobody");

        // The token form refuses the scope: the pairing machinery is its only minter.
        let err = form_token(&app, "sneak", "fleet").await.unwrap_err();
        assert!(err.contains("read, operate, launch"), "{err}");
    }

    /// Removing a member from the cockpit and a member's own leave both revoke the token and drop
    /// the row; the leave of an already-removed machine still ends cleanly.
    /// Issue #764: each member in `GET /api/fleet` carries a `health` verdict. A member whose last
    /// answer is 12 minutes older than the owner's latest poll reads degraded, with its hint; a
    /// revoked token reads stopped.
    #[tokio::test]
    async fn a_member_with_a_stale_heartbeat_reads_degraded_with_a_hint() {
        let (_root, app, router, owner) = rig();
        let owner = owner.as_deref();
        let (member_id, _token) = join_member(&router, owner, &an_invite(&router, &app.api_token).await).await;

        // Never polled yet: nobody checked it, so unknown — never ok.
        let view = fleet_view(&router, owner).await;
        let health = &view["members"][0]["health"];
        assert_eq!(health["state"], "unknown", "{view}");
        assert_eq!(health["reason"], "Not checked yet");
        assert_eq!(health["hint"], "open the cockpit or wait for the next poll");

        // It joined an hour ago; it last answered 12 minutes before the latest poll, which failed.
        let now = Utc::now();
        let (url, token_id) = {
            let mut state = app.fleet_members.state.write().await;
            let member = state.members.iter_mut().find(|m| m.id == member_id).unwrap();
            member.joined_at = now - Duration::hours(1);
            (member.url.clone().unwrap(), member.token_id.clone())
        };
        let row = crate::fleet::HostSummary {
            id: "worker-host".into(),
            name: "worker".into(),
            platform: "linux-x86_64".into(),
            os: "Debian".into(),
            version: Some("0.1.10".into()),
            slots_in_use: 0,
            slots_ceiling: 4,
            queue_depth: 0,
            disk_free_bytes: Some(400 << 30),
            disk_total_bytes: Some(500 << 30),
            last_heartbeat: Some((now - Duration::minutes(12)).to_rfc3339()),
            health: crate::fleet::HostHealth::Unreachable,
            runner_tick_age_s: None,
            fleet_sync: None,
        };
        app.fleet_cache.record_poll(&url, now, row).await;

        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "degraded", "{health}");
        assert_eq!(health["code"], "no_heartbeat", "the heartbeat outranks plain unreachability");
        assert_eq!(health["reason"], "No heartbeat for 12 min");
        assert_eq!(health["hint"], "the machine may be asleep");

        // Revoke its token behind the fleet's back: stopped, and the hint says re-pair.
        app.api_tokens.revoke(&token_id).await.unwrap();
        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "stopped", "{health}");
        assert_eq!(health["reason"], "Token revoked");
        assert_eq!(health["hint"], "re-pair this machine");
    }

    /// Issue #764: the sync and runner signals a member reports in its status reach its health —
    /// a 401 or 403 sync stops it, an hour-old backlog or a stalled queue loop degrades it, and
    /// consent off is only a note.
    #[tokio::test]
    async fn a_members_sync_and_runner_reports_map_to_its_health() {
        let (_root, app, router, owner) = rig();
        let owner = owner.as_deref();
        let (member_id, _token) = join_member(&router, owner, &an_invite(&router, &app.api_token).await).await;
        let url = {
            let state = app.fleet_members.state.read().await;
            state.members.iter().find(|m| m.id == member_id).unwrap().url.clone().unwrap()
        };
        let poll = |runner: Option<i64>, sync: Option<crate::fleet::PeerSync>| {
            let app = app.clone();
            let url = url.clone();
            async move {
                let now = Utc::now();
                let row = crate::fleet::HostSummary {
                    id: "worker-host".into(),
                    name: "worker".into(),
                    platform: "linux-x86_64".into(),
                    os: "Debian".into(),
                    version: Some("0.1.10".into()),
                    slots_in_use: 0,
                    slots_ceiling: 4,
                    queue_depth: 0,
                    disk_free_bytes: Some(400 << 30),
                    disk_total_bytes: Some(500 << 30),
                    last_heartbeat: Some(now.to_rfc3339()),
                    health: crate::fleet::HostHealth::Online,
                    runner_tick_age_s: runner,
                    fleet_sync: sync,
                };
                app.fleet_cache.record_poll(&url, now, row).await;
            }
        };
        let synced = crate::fleet::PeerSync {
            state: "synced".into(),
            consent: true,
            ..Default::default()
        };

        poll(Some(3), Some(synced.clone())).await;
        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "ok", "{health}");
        assert_eq!(health["note"], Value::Null);

        let behind = crate::fleet::PeerSync {
            state: "error".into(),
            backlog_rows: 5,
            oldest_unsent_age_s: Some(2 * 3600),
            last_error_class: Some("error".into()),
            consent: true,
        };
        poll(Some(3), Some(behind)).await;
        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "degraded", "{health}");
        assert_eq!(health["code"], "sync_backlog");
        assert_eq!(health["reason"], "Sync behind by 5 rows");

        for (state, class) in [("unauthorized", "unauthorized"), ("removed", "forbidden")] {
            let rejected = crate::fleet::PeerSync {
                state: state.into(),
                last_error_class: Some(class.into()),
                consent: true,
                ..Default::default()
            };
            poll(Some(3), Some(rejected)).await;
            let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
            assert_eq!(health["state"], "stopped", "{state}: {health}");
            assert_eq!(health["code"], "sync_rejected");
            assert_eq!(health["reason"], "Token revoked");
            assert_eq!(health["hint"], "re-pair this machine");
        }

        poll(Some(600), Some(synced)).await;
        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "degraded", "{health}");
        assert_eq!(health["code"], "runner_down");
        assert_eq!(health["reason"], "Colony runner not ticking");

        let off = crate::fleet::PeerSync {
            state: "consent_required".into(),
            consent: false,
            ..Default::default()
        };
        poll(Some(3), Some(off)).await;
        let health = fleet_view(&router, owner).await["members"][0]["health"].clone();
        assert_eq!(health["state"], "ok", "consent off is not a fault: {health}");
        assert_eq!(health["note"], "History sync off");
    }

    #[tokio::test]
    async fn removal_and_leaving_both_revoke_the_token_and_drop_the_member() {
        let (_root, app, router, owner) = rig();
        let owner = owner.as_deref();

        // Join one member the long way, through the guard.
        let (member_id, member_token) = join_member(&router, owner, &an_invite(&router, &app.api_token).await).await;
        assert!(app.api_tokens.authenticate(&member_token).await.is_some());

        // The owner removes it: row gone, token dead, role back to none.
        let member_uri = format!("/api/fleet/members/{member_id}");
        let (status, _) = ask(&router, Method::DELETE, &member_uri, owner, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = ask(&router, Method::DELETE, &member_uri, owner, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a second removal is a 404");
        let gone = app.api_tokens.authenticate(&member_token).await.is_none();
        assert!(gone, "the token is revoked");
        assert_eq!(fleet_view(&router, owner).await["role"], "none");
        // The removed member's token is remembered: 403 "removed", where an unknown one is 401.
        let (status, answer) = ask(&router, Method::GET, "/api/hosts", Some(&member_token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(answer["error"], "removed from the fleet");
        let (status, _) = ask(&router, Method::GET, "/api/hosts", Some("col_never_minted"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A second member leaves by itself, with its own token.
        let (_second, second_token) = join_member(&router, owner, &an_invite(&router, &app.api_token).await).await;
        let leave_body = Some("{}".into());
        let (status, _) = ask(
            &router,
            Method::POST,
            "/api/fleet/peer/leave",
            Some(&second_token),
            leave_body,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "the member drops itself with its fleet token");
        assert!(app.api_tokens.authenticate(&second_token).await.is_none());
        let (status, _) = ask(&router, Method::GET, "/api/hosts", Some(&second_token), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a member that left is not a removed one");
        let view = fleet_view(&router, owner).await;
        assert_eq!(view["role"], "none");
        assert_eq!(view["members"].as_array().unwrap().len(), 0);
        // Without a fleet token the leave is refused — the owner's token is not a member's.
        let (status, _) = ask(&router, Method::POST, "/api/fleet/peer/leave", owner, Some("{}".into())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// The whole dance, both machines in one test: the joiner dials a really-serving owner over
    /// loopback, spends the code, its operator compares the codes, and the fleet exists — until
    /// the member leaves.
    #[tokio::test]
    async fn a_mothership_joins_a_fleet_and_leaves_it_end_to_end() {
        let (_owner_root, owner, owner_r, owner_bearer) = rig();
        let (_joiner_root, _joiner, joiner_r, joiner_bearer) = rig();
        let (owner_bearer, joiner_bearer) = (owner_bearer.as_deref(), joiner_bearer.as_deref());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let owner_url = format!("http://{}", listener.local_addr().unwrap());
        let served = owner.clone();
        tokio::spawn(async move { axum::serve(listener, crate::server::router(&served)).await.unwrap() });

        let code = an_invite(&owner_r, &owner.api_token).await;
        let join_body =
            json!({"owner_url": owner_url, "code": code, "name": "worker", "url": "http://10.0.0.2:7878"}).to_string();
        let (status, joining) = ask(&joiner_r, Method::POST, "/api/fleet/join", joiner_bearer, Some(join_body)).await;
        assert_eq!(status, StatusCode::OK, "{joining}");
        assert_eq!(joining["status"], "pending");
        let confirm = joining["confirm_code"].as_str().unwrap().to_string();

        // The owner sees the pairing with the same number, and approves it.
        let view = fleet_view(&owner_r, owner_bearer).await;
        assert_eq!(view["role"], "none", "a pairing alone makes no owner");
        assert_eq!(view["pending"][0]["confirm_code"], confirm);
        let pairing_id = view["pending"][0]["id"].as_str().unwrap().to_string();
        let approve_uri = format!("/api/fleet/pending/{pairing_id}/approve");
        let (status, _) = ask(&owner_r, Method::POST, &approve_uri, owner_bearer, None).await;
        assert_eq!(status, StatusCode::OK);

        // Confirm: the token comes over, the membership is stored, the joining is cleared.
        let (_, answer) = ask(&joiner_r, Method::POST, "/api/fleet/join/confirm", joiner_bearer, None).await;
        assert_eq!(answer["status"], "joined");
        let view = fleet_view(&joiner_r, joiner_bearer).await;
        assert_eq!(view["role"], "member");
        assert_eq!(view["membership"]["owner_url"], owner_url);
        assert!(view["joining"].is_null());
        let owner_role = fleet_view(&owner_r, owner_bearer).await["role"].clone();
        assert_eq!(owner_role, "owner", "the owner has a member now");

        // A second confirm finds the pairing gone: the join is over either way.
        let (status, _) = ask(&joiner_r, Method::POST, "/api/fleet/join/confirm", joiner_bearer, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "no pairing is in progress any more");

        // The member leaves: the owner's token registry is clean on both sides.
        let (status, _) = ask(&joiner_r, Method::POST, "/api/fleet/leave", joiner_bearer, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(fleet_view(&joiner_r, joiner_bearer).await["role"], "none");
        let left = fleet_view(&owner_r, owner_bearer).await["role"] == "none";
        assert!(left, "the member left the owner's list too");
    }
}
