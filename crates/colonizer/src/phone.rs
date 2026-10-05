//! Add your phone (issue #746): a phone signs in with a credential of its own, paired the way
//! remote access pairs its owner (remote.rs, #534/#599) and the fleet pairs a machine
//! (fleet_members.rs): a single-use code starts it, a short code confirmed in the local cockpit
//! finishes it, and nothing is handed over before a person on this machine said yes.
//!
//! 1. The owner mints an invite (`POST /api/phone/invites`): 256 random bits, hashed at rest,
//!    five minutes, single use. The cockpit shows it as a QR code of `<origin>/?pair=<invite>`.
//!    It is a ticket to *ask*, never a credential: the API token appears in no QR and no URL.
//! 2. The phone opens it. `host_guard` spends the invite on that first presentation and binds the
//!    pairing to this one browser: a device secret goes back in an `HttpOnly` cookie scoped to
//!    the claim route, and the page shows a six-digit confirm code.
//! 3. The owner types that code into Settings → Add your phone on this machine
//!    (`POST /api/phone/pairings/confirm`). Local only, like confirming a relay pairing: a request
//!    through the tunnel, or from a phone, cannot approve a phone.
//! 4. The phone's page polls `POST /api/phone/claim` with its device cookie. Once approved, the
//!    claim mints the phone's own credential (`cph_…`), sets it as the cockpit cookie, and forgets
//!    the pairing. Each phone is a row in Settings that revokes its credential alone.
//!
//! Every unauthenticated step — presenting an invite, claiming — and every wrong confirm code
//! counts against one rate limit, so neither the codes nor the confirm step can be ground through.
//! Invites and pairings live in memory (a restart forgets them; mint another); the paired phones
//! persist to `<config_dir>/phones.json` (0600) as hashes only.
//!
//! The invite route also answers where the phone could reach this mothership — the relay's
//! `https://<install>.my.colonizer.dev`, the tailnet address, the LAN address, best first.

use crate::{Shared, client_error, gateway::constant_time_eq, remote, util};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing,
};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, to_value};
use std::{
    collections::VecDeque,
    net::{IpAddr, SocketAddr, UdpSocket},
    path::{Path as FsPath, PathBuf},
    sync::{Mutex, MutexGuard, RwLock},
};

/// How long an invite and a pairing live, in seconds: long enough to pick the phone up and read a
/// code off it, short enough that a QR code left on a screen is worthless soon after.
pub(crate) const TTL_SECS: i64 = 300;
/// The most invites, and the most pairings, open at once. Minting refuses rather than evicts, so
/// hammering the route cannot invalidate the code the owner is about to scan.
const MAX_OPEN: usize = 4;
/// The rate limit on pairing attempts: an invite that opens nothing, a claim that finds nothing and
/// a wrong confirm code each count as a failure, across every caller. With the window full, every
/// pairing step is refused until it slides — a legitimate pairing fails at most once or twice
/// (a mistyped code); a guesser stops cold.
const ATTEMPT_WINDOW_SECS: i64 = 60;
const MAX_FAILURES: usize = 10;
/// The most phones paired at once.
const MAX_DEVICES: usize = 32;
/// The device secret's cookie, scoped to the claim route so no other request ever carries it.
pub(crate) const PAIR_COOKIE: &str = "colonizer_pair";
/// A phone credential's prefix, which tells it apart from the install's API token and the scoped
/// `col_` tokens at a glance.
const TOKEN_PREFIX: &str = "cph_";

/// SHA-256, hex: what every stored secret here is reduced to.
fn digest(secret: &str) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, secret.as_bytes()).as_ref())
}

/// Six random digits, shown as `123 456` on the phone and typed into the cockpit.
fn new_confirm_code() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 4];
    // Pairing through a failed random source would be worse than refusing: a fixed code.
    SystemRandom::new().fill(&mut bytes).expect("the random source failed");
    let number = u32::from_be_bytes(bytes) % 1_000_000;
    format!("{:03} {:03}", number / 1000, number % 1000)
}

/// The confirm code as typed: digits only, so `123456`, `123 456` and `123-456` are one code.
fn normalize_confirm(code: &str) -> String {
    code.chars().filter(char::is_ascii_digit).collect()
}

// ---------------------------------------------------------------------------
// The pairing book: invites, pairings and the attempt window, pure over a clock the caller passes.
// ---------------------------------------------------------------------------

struct Invite {
    hash: String,
    expires_at: i64,
    /// A remote-link invite (remote.rs, review finding R3): it opens only through the tunnel, and
    /// its pairing, once confirmed in Settings → Remote access, mints a link credential, not a
    /// phone's.
    link: bool,
}

struct Pairing {
    id: String,
    /// The hash of the secret only the browser that spent the invite holds (its pairing cookie).
    secret_hash: String,
    confirm_code: String,
    label: String,
    expires_at: i64,
    approved: bool,
    link: bool,
}

/// What a claim found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    /// Waiting for the owner.
    Pending,
    /// Approved: the pairing is gone, and the caller mints the credential under this label.
    Approved(String),
    /// An approved remote-link pairing: the caller mints a link credential (remote.rs).
    ApprovedLink(String),
    /// Unknown, wrong secret, or expired — one answer for all three.
    Gone,
}

/// What a spent invite hands the phone: the pairing's id and secret (its cookie) and the code.
#[derive(Debug)]
pub(crate) struct Opened {
    /// A remote-link pairing: the page sends the owner to Settings → Remote access.
    pub(crate) link: bool,
    pub(crate) id: String,
    pub(crate) secret: String,
    pub(crate) confirm_code: String,
}

#[derive(Default)]
pub(crate) struct Book {
    invites: Vec<Invite>,
    pairings: Vec<Pairing>,
    failures: VecDeque<i64>,
}

impl Book {
    fn prune(&mut self, now: i64) {
        self.invites.retain(|i| i.expires_at > now);
        self.pairings.retain(|p| p.expires_at > now);
        while self.failures.front().is_some_and(|at| *at <= now - ATTEMPT_WINDOW_SECS) {
            self.failures.pop_front();
        }
    }

    /// Whether pairing is shut for now: [`MAX_FAILURES`] failures inside the window.
    pub(crate) fn limited(&mut self, now: i64) -> bool {
        self.prune(now);
        self.failures.len() >= MAX_FAILURES
    }

    /// Records one failed pairing step.
    pub(crate) fn failed(&mut self, now: i64) {
        self.failures.push_back(now);
    }

    /// Mints an invite, storing only its hash; `None` when [`MAX_OPEN`] are already open.
    fn mint(&mut self, now: i64) -> Option<String> {
        self.mint_kind(now, false)
    }

    /// Mints a remote-link invite ("Sign in on another device", remote.rs).
    pub(crate) fn mint_link(&mut self, now: i64) -> Option<String> {
        self.mint_kind(now, true)
    }

    fn mint_kind(&mut self, now: i64, link: bool) -> Option<String> {
        self.prune(now);
        if self.invites.len() >= MAX_OPEN {
            return None;
        }
        let code = format!("{}{}", util::random_token(), util::random_token())[..64].to_string();
        self.invites.push(Invite {
            hash: digest(&code),
            expires_at: now + TTL_SECS,
            link,
        });
        Some(code)
    }

    /// Spends an invite on its first presentation and opens a pairing bound to the presenting
    /// browser. `None` for an unknown, spent or expired invite, and when [`MAX_OPEN`] pairings
    /// already wait — the invite is spent either way, so it can never be presented twice.
    #[cfg(test)]
    fn open(&mut self, code: &str, label: &str, now: i64) -> Option<Opened> {
        self.open_from(code, label, false, now)
    }

    /// [`Book::open`], knowing whether the browser came through the tunnel: a remote-link invite
    /// is spent but opens nothing anywhere else, since its credential works only there.
    fn open_from(&mut self, code: &str, label: &str, tunnelled: bool, now: i64) -> Option<Opened> {
        self.prune(now);
        let hash = digest(code);
        let at = self
            .invites
            .iter()
            .position(|i| constant_time_eq(i.hash.as_bytes(), hash.as_bytes()))?;
        let invite = self.invites.remove(at);
        if self.pairings.len() >= MAX_OPEN || (invite.link && !tunnelled) {
            return None;
        }
        // A link invite is usually opened on a computer, which the phone labels miss.
        let label = if invite.link && label == "Phone" { "Browser" } else { label };
        let opened = Opened {
            link: invite.link,
            id: format!("ph_{}", util::short_id()),
            secret: util::random_token(),
            confirm_code: new_confirm_code(),
        };
        self.pairings.push(Pairing {
            id: opened.id.clone(),
            secret_hash: digest(&opened.secret),
            confirm_code: opened.confirm_code.clone(),
            label: label.to_string(),
            expires_at: now + TTL_SECS,
            approved: false,
            link: invite.link,
        });
        Some(opened)
    }

    /// Approves the waiting pairing whose confirm code this is; its label, or `None` for a code
    /// that matches no live, unapproved pairing.
    fn confirm(&mut self, code: &str, now: i64) -> Option<String> {
        self.confirm_kind(code, false, now)
    }

    /// [`Book::confirm`] for a remote-link pairing (Settings → Remote access).
    pub(crate) fn confirm_link(&mut self, code: &str, now: i64) -> Option<String> {
        self.confirm_kind(code, true, now)
    }

    fn confirm_kind(&mut self, code: &str, link: bool, now: i64) -> Option<String> {
        self.prune(now);
        let typed = normalize_confirm(code);
        if typed.len() != 6 {
            return None;
        }
        let pairing = self.pairings.iter_mut().find(|p| {
            !p.approved && p.link == link && constant_time_eq(normalize_confirm(&p.confirm_code).as_bytes(), typed.as_bytes())
        })?;
        pairing.approved = true;
        Some(pairing.label.clone())
    }

    /// Drops a waiting pairing, so that phone is never approved; `false` when there is none.
    fn reject(&mut self, id: &str, now: i64) -> bool {
        self.prune(now);
        let before = self.pairings.len();
        self.pairings.retain(|p| p.id != id);
        self.pairings.len() != before
    }

    /// The phone's poll. Only the browser holding the pairing's secret gets an answer other than
    /// [`Claim::Gone`]; an approved pairing is consumed by the claim that finds it.
    fn claim(&mut self, id: &str, secret: &str, now: i64) -> Claim {
        self.prune(now);
        let hash = digest(secret);
        let Some(at) = self
            .pairings
            .iter()
            .position(|p| p.id == id && constant_time_eq(p.secret_hash.as_bytes(), hash.as_bytes()))
        else {
            return Claim::Gone;
        };
        if !self.pairings[at].approved {
            return Claim::Pending;
        }
        let pairing = self.pairings.remove(at);
        if pairing.link {
            Claim::ApprovedLink(pairing.label)
        } else {
            Claim::Approved(pairing.label)
        }
    }

    fn pending_view(&mut self, now: i64) -> Vec<Value> {
        self.pending_kind(now, false)
    }

    /// The remote-link pairings waiting for their code (Settings → Remote access).
    pub(crate) fn pending_link_view(&mut self, now: i64) -> Vec<Value> {
        self.pending_kind(now, true)
    }

    fn pending_kind(&mut self, now: i64, link: bool) -> Vec<Value> {
        self.prune(now);
        self.pairings
            .iter()
            .filter(|p| !p.approved && p.link == link)
            .map(|p| json!({"id": p.id, "label": p.label, "expires_at": rfc3339(p.expires_at)}))
            .collect()
    }
}

pub(crate) fn rfc3339(unix: i64) -> String {
    Utc.timestamp_opt(unix, 0)
        .single()
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The pairing rate limit said no.
#[derive(Debug)]
pub(crate) struct Limited;

/// Unix seconds now.
pub(crate) fn now_secs() -> i64 {
    Utc::now().timestamp()
}

// ---------------------------------------------------------------------------
// The paired phones, persisted, and the store on `App` that holds both.
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
struct Device {
    id: String,
    label: String,
    token_hash: String,
    paired_at: DateTime<Utc>,
}

/// Marks a request signed in by a phone credential; `host_guard` attaches it, and the routes that
/// pair or revoke phones refuse it.
#[derive(Clone, Debug)]
pub(crate) struct PhoneDevice {
    pub(crate) id: String,
}

impl PhoneDevice {
    /// The key its revocation fires under (`auth::Revocation`).
    pub(crate) fn revocation_key(&self) -> String {
        format!("phone:{}", self.id)
    }
}

pub struct PhoneStore {
    book: Mutex<Book>,
    devices: RwLock<Vec<Device>>,
    path: PathBuf,
}

impl PhoneStore {
    /// Loads the paired phones, never failing: an unreadable file is reported and answered with no
    /// phones — every phone signed out, the safe direction (api_tokens.rs's rule).
    pub fn load(config_dir: &FsPath) -> PhoneStore {
        let path = config_dir.join("phones.json");
        let devices = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                eprintln!("phones: could not read {} ({e}); no phone is signed in", path.display());
                Vec::new()
            }
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("phones: {} does not parse ({e}); no phone is signed in", path.display());
                Vec::new()
            }),
        };
        PhoneStore {
            book: Mutex::new(Book::default()),
            devices: RwLock::new(devices),
            path,
        }
    }

    pub(crate) fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn save(&self, devices: &[Device]) {
        let Ok(bytes) = serde_json::to_vec_pretty(devices) else {
            return;
        };
        if let Err(e) = util::write_private(&self.path, &bytes) {
            eprintln!("phones: could not save {}: {e}", self.path.display());
        }
    }

    /// The phone a `colonizer_token` cookie belongs to, if it is a live phone credential.
    pub(crate) fn authenticate(&self, token: &str) -> Option<PhoneDevice> {
        if !token.starts_with(TOKEN_PREFIX) {
            return None;
        }
        let hash = digest(token);
        let devices = self.devices.read().unwrap_or_else(|p| p.into_inner());
        devices
            .iter()
            .find(|d| constant_time_eq(d.token_hash.as_bytes(), hash.as_bytes()))
            .map(|d| PhoneDevice { id: d.id.clone() })
    }

    /// Mints a phone's credential and records the phone; the plaintext is returned once, to be set
    /// as that phone's cookie, and never stored.
    pub(crate) fn add(&self, label: &str) -> Option<String> {
        let mut devices = self.devices.write().unwrap_or_else(|p| p.into_inner());
        if devices.len() >= MAX_DEVICES {
            return None;
        }
        let token = format!("{TOKEN_PREFIX}{}", util::random_token());
        devices.push(Device {
            id: format!("dev_{}", util::short_id()),
            label: label.to_string(),
            token_hash: digest(&token),
            paired_at: Utc::now(),
        });
        self.save(&devices);
        Some(token)
    }

    /// Revokes one phone at once: its credential stops working, and its in-flight requests,
    /// streams and open sockets end now (`auth::Revocation`), not at its next request.
    pub(crate) fn revoke(&self, id: &str) -> Option<String> {
        let mut devices = self.devices.write().unwrap_or_else(|p| p.into_inner());
        let at = devices.iter().position(|d| d.id == id)?;
        let gone = devices.remove(at);
        self.save(&devices);
        drop(devices);
        crate::auth::Revocation::fire(&format!("phone:{id}"));
        Some(gone.label)
    }

    /// A paired phone's label, for naming its push subscription.
    pub(crate) fn label(&self, id: &str) -> Option<String> {
        let devices = self.devices.read().unwrap_or_else(|p| p.into_inner());
        devices.iter().find(|d| d.id == id).map(|d| d.label.clone())
    }

    pub(crate) fn view(&self) -> Value {
        let pending = self.book().pending_view(now_secs());
        let devices = self.devices.read().unwrap_or_else(|p| p.into_inner());
        json!({
            "devices": devices.iter().map(|d| json!({"id": d.id, "label": d.label, "paired_at": d.paired_at})).collect::<Vec<_>>(),
            "pending": pending,
        })
    }

    /// `host_guard`'s half of step 2: spends the invite and opens the pairing. `Err(Limited)`
    /// when the rate limit refused the attempt outright; an invite that opens nothing counts.
    pub(crate) fn open(&self, code: &str, user_agent: &str, tunnelled: bool) -> Result<Option<Opened>, Limited> {
        let now = now_secs();
        let mut book = self.book();
        if book.limited(now) {
            return Err(Limited);
        }
        let opened = book.open_from(code, device_label(user_agent), tunnelled, now);
        if opened.is_none() {
            book.failed(now);
        }
        Ok(opened)
    }
}

/// A readable name for the phone, from its User-Agent: a display label, trusted for nothing else.
fn device_label(user_agent: &str) -> &'static str {
    let ua = user_agent.to_ascii_lowercase();
    if ua.contains("iphone") {
        "iPhone"
    } else if ua.contains("ipad") {
        "iPad"
    } else if ua.contains("android") {
        "Android phone"
    } else {
        "Phone"
    }
}

/// Whether a phone credential may make this request. A phone runs the whole cockpit, but it does
/// not mint, approve or revoke credentials, or change the remote link: those stay with the owner's
/// own browser and the CLI, so a lost phone cannot mint itself a successor.
pub(crate) fn phone_may(method: &Method, path: &str) -> bool {
    if method == Method::GET {
        return true;
    }
    !["/api/phone", "/api/tokens", "/api/remote", "/api/fleet"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

/// Refuses a caller that is a phone: pairing and revoking phones is the owner's.
fn not_a_phone(phone: Option<&Extension<PhoneDevice>>) -> Result<(), crate::AppError> {
    match phone {
        Some(_) => Err(client_error(
            StatusCode::FORBIDDEN,
            "a phone cannot pair or revoke phones; use the cockpit on your computer",
        )),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Routes.
// ---------------------------------------------------------------------------

/// `GET /api/phone`: the paired phones and the pairings waiting for a confirm code — labels and
/// times only, never a code, a secret or a hash — plus where a phone could reach this mothership.
/// The origins are the same ranked, non-secret list the invite route answers (relay → tailnet →
/// lan), so the cockpit can show a bookmarkable address without minting a single-use invite.
async fn list(State(app): State<Shared>) -> Json<Value> {
    Json(view_with_origins(app.phones.view(), detect_origins(&app).await))
}

/// The list view with the ranked origins folded in. Only bare origins go in — no code, no secret.
fn view_with_origins(view: Value, origins: Vec<Origin>) -> Value {
    let mut view = view;
    if let Value::Object(fields) = &mut view {
        fields.insert("origins".to_string(), to_value(origins).unwrap_or(Value::Null));
    }
    view
}

/// The origins a phone could open this mothership on, ranked best first. Detection is a route
/// lookup per candidate (a UDP `connect` sends nothing), off the async threads all the same.
async fn detect_origins(app: &Shared) -> Vec<Origin> {
    let (tailnet, lan) = tokio::task::spawn_blocking(|| {
        let tailnet = detect_tailnet();
        (tailnet, detect_lan(tailnet))
    })
    .await
    .unwrap_or_default();
    let relay = app.remote.link().await;
    origins(
        relay.as_ref().map(|(host, up)| (host.as_str(), *up)),
        tailnet,
        lan,
        &app.cfg.bind,
        &app.cfg.allowed_hosts,
    )
}

/// `POST /api/phone/invites`: mint an invite and answer the origins a phone could open it on.
async fn invite(State(app): State<Shared>, phone: Option<Extension<PhoneDevice>>) -> Result<Json<Value>, crate::AppError> {
    not_a_phone(phone.as_ref())?;
    let now = now_secs();
    let Some(code) = app.phones.book().mint(now) else {
        return Err(client_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many open invites; use one or wait a few minutes for them to expire",
        ));
    };
    let origins = detect_origins(&app).await;
    Ok(Json(json!({
        "code": code,
        "expires_at": rfc3339(now + TTL_SECS),
        "ttl_secs": TTL_SECS,
        "origins": origins,
    })))
}

#[derive(Deserialize)]
struct ConfirmBody {
    code: String,
}

/// `POST /api/phone/pairings/confirm {"code"}`: approve the phone showing this code. Local only
/// and never from a phone. A wrong code counts against the pairing rate limit; 404 names no reason.
async fn confirm(
    State(app): State<Shared>,
    tunnelled: Option<Extension<remote::Tunnelled>>,
    phone: Option<Extension<PhoneDevice>>,
    Json(body): Json<ConfirmBody>,
) -> Result<Json<Value>, crate::AppError> {
    remote::local_only(tunnelled.as_ref(), "approve a phone")?;
    not_a_phone(phone.as_ref())?;
    let now = now_secs();
    let mut book = app.phones.book();
    if book.limited(now) {
        return Err(client_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed pairing attempts; wait a minute",
        ));
    }
    match book.confirm(&body.code, now) {
        Some(label) => Ok(Json(json!({"label": label}))),
        None => {
            book.failed(now);
            Err(client_error(
                StatusCode::NOT_FOUND,
                "no phone is waiting with that code: it is wrong, expired or already used",
            ))
        }
    }
}

/// `POST /api/phone/pairings/{id}/reject`: that phone is never approved. Local only.
async fn reject(
    State(app): State<Shared>,
    Path(id): Path<String>,
    tunnelled: Option<Extension<remote::Tunnelled>>,
    phone: Option<Extension<PhoneDevice>>,
) -> Result<StatusCode, crate::AppError> {
    remote::local_only(tunnelled.as_ref(), "reject a phone")?;
    not_a_phone(phone.as_ref())?;
    match app.phones.book().reject(&id, now_secs()) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(client_error(StatusCode::NOT_FOUND, "no such pairing")),
    }
}

/// `DELETE /api/phone/devices/{id}`: sign one phone out for good. Allowed through the relay too —
/// revoking a lost phone should not wait until you are home — but never from a phone.
async fn revoke(
    State(app): State<Shared>,
    Path(id): Path<String>,
    phone: Option<Extension<PhoneDevice>>,
) -> Result<StatusCode, crate::AppError> {
    not_a_phone(phone.as_ref())?;
    match forget(&app, &id).await {
        Some(_) => Ok(StatusCode::NO_CONTENT),
        None => Err(client_error(StatusCode::NOT_FOUND, "no such phone")),
    }
}

/// Revokes a phone and everything that still reaches it: its credential and open sockets
/// ([`PhoneStore::revoke`]), its push subscriptions (push.rs) and the answer tokens its
/// notifications carry (answer_tokens.rs, issue #742). Answers the phone's label, `None` for an
/// unknown phone.
pub(crate) async fn forget(app: &crate::App, id: &str) -> Option<String> {
    let label = app.phones.revoke(id)?;
    if let Err(e) = crate::push::drop_phone(&app.cfg.config_dir, id) {
        eprintln!("phones: the push subscriptions of {id} could not be dropped ({e:#})");
    }
    app.answer_tokens.revoke_phone(id).await;
    Some(label)
}

/// The pairing cookie, `<pairing id>.<secret>`.
fn pair_cookie(headers: &HeaderMap) -> Option<(String, String)> {
    for values in headers.get_all(header::COOKIE) {
        let Ok(cookies) = values.to_str() else { continue };
        for part in cookies.split(';') {
            if let Some((name, value)) = part.split_once('=')
                && name.trim() == PAIR_COOKIE
                && let Some((id, secret)) = value.trim().split_once('.')
            {
                return Some((id.to_string(), secret.to_string()));
            }
        }
    }
    None
}

/// `POST /api/phone/claim`: the pairing page's poll, admitted without a token — its whole
/// authentication is the pairing cookie only the phone that spent the invite holds. 202 while the
/// owner has not confirmed; 200 with the phone's own credential as its cookie once they have;
/// 404 for anything else. Rate-limited like every pairing step.
pub(crate) async fn claim(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let now = now_secs();
    let found = {
        let mut book = app.phones.book();
        if book.limited(now) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "too many failed pairing attempts; wait a minute"})),
            )
                .into_response();
        }
        let found = match pair_cookie(&headers) {
            Some((id, secret)) => book.claim(&id, &secret, now),
            None => Claim::Gone,
        };
        if found == Claim::Gone {
            book.failed(now);
        }
        found
    };
    let over = found != Claim::Pending;
    let mut res = match found {
        Claim::Pending => (StatusCode::ACCEPTED, Json(json!({"status": "pending"}))).into_response(),
        Claim::Gone => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "this pairing expired or was turned down; scan a new code"})),
        )
            .into_response(),
        Claim::ApprovedLink(label) => match app.remote.add_link(&label) {
            None => (
                StatusCode::CONFLICT,
                Json(json!({"error": "too many devices are signed in to the link; reset it first"})),
            )
                .into_response(),
            Some(token) => {
                let mut res = (StatusCode::OK, Json(json!({"status": "paired"}))).into_response();
                if let Ok(cookie) = crate::auth::set_cookie_header(&token).parse() {
                    res.headers_mut().append(header::SET_COOKIE, cookie);
                }
                res
            }
        },
        Claim::Approved(label) => match app.phones.add(&label) {
            None => (
                StatusCode::CONFLICT,
                Json(json!({"error": "too many phones are paired; revoke one first"})),
            )
                .into_response(),
            Some(token) => {
                let mut res = (StatusCode::OK, Json(json!({"status": "paired"}))).into_response();
                if let Ok(cookie) = crate::auth::set_cookie_header(&token).parse() {
                    res.headers_mut().append(header::SET_COOKIE, cookie);
                }
                res
            }
        },
    };
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // A pairing that is not pending any more is over, whatever the answer: drop the device secret.
    if over && let Ok(clear) = format!("{PAIR_COOKIE}=; HttpOnly; SameSite=Strict; Path=/api/phone/claim; Max-Age=0").parse() {
        res.headers_mut().append(header::SET_COOKIE, clear);
    }
    res
}

/// The page a spent invite answers (`host_guard`): the confirm code to type into the cockpit, the
/// device secret as a cookie only the claim route receives, and a poll that finishes the sign-in.
pub(crate) fn pairing_response(opened: &Opened) -> Response {
    let mut page = include_str!("pages/phone_pairing.html").replace("{{CONFIRM_CODE}}", &opened.confirm_code);
    if opened.link {
        page = page
            .replace(
                "Settings &rarr; Add your phone",
                "Settings &rarr; Remote access &rarr; Sign in on another device",
            )
            .replace("signs this phone in", "signs this browser in");
    }
    let mut res = Html(page).into_response();
    let headers = res.headers_mut();
    let cookie = format!(
        "{PAIR_COOKIE}={}.{}; HttpOnly; SameSite=Strict; Path=/api/phone/claim; Max-Age={TTL_SECS}",
        opened.id, opened.secret
    );
    if let Ok(cookie) = cookie.parse() {
        headers.insert(header::SET_COOKIE, cookie);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new()
        .route("/api/phone", routing::get(list))
        .route("/api/phone/invites", routing::post(invite))
        .route("/api/phone/pairings/confirm", routing::post(confirm))
        .route("/api/phone/pairings/{id}/reject", routing::post(reject))
        .route("/api/phone/devices/{id}", routing::delete(revoke))
        .route("/api/phone/claim", routing::post(claim))
}

// ---------------------------------------------------------------------------
// Where a phone could reach this mothership.
// ---------------------------------------------------------------------------

/// One place the phone could reach the mothership, as the route answers it.
#[derive(Serialize, PartialEq, Eq, Debug)]
pub(crate) struct Origin {
    kind: &'static str,
    url: String,
    reachable: bool,
    secure: bool,
    note: Option<String>,
}

/// The candidates, best first: the relay (its TLS front door, when remote access is on), then the
/// tailnet, then the LAN. Pure, so the order and the operator hints are pinned by tests — the
/// probes are [`detect_tailnet`] and [`detect_lan`], and the caller passes what they found.
/// `relay` is the tunnel host and whether the tunnel is up right now.
pub(crate) fn origins(
    relay: Option<(&str, bool)>,
    tailnet: Option<IpAddr>,
    lan: Option<IpAddr>,
    bind: &str,
    allowed_hosts: &[String],
) -> Vec<Origin> {
    let mut out = Vec::new();
    if let Some((host, up)) = relay {
        out.push(Origin {
            kind: "relay",
            url: format!("https://{host}"),
            reachable: up,
            secure: true,
            note: (!up).then(|| "Remote access is on, but its link is not connected right now".to_string()),
        });
    }
    if let Some(ip) = tailnet {
        out.push(plain_http_origin("tailnet", ip, bind, allowed_hosts));
    }
    if let Some(ip) = lan {
        out.push(plain_http_origin("lan", ip, bind, allowed_hosts));
    }
    out
}

/// An `http://<ip>:<bind port>` origin, reachable when the listener answers on that address AND
/// `host_guard` would accept the `Host` header a phone's browser sends there.
fn plain_http_origin(kind: &'static str, ip: IpAddr, bind: &str, allowed_hosts: &[String]) -> Origin {
    let port = bind.rsplit_once(':').map_or("7878", |(_, port)| port);
    let host = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(_) => ip.to_string(),
    };
    let (reachable, note) = if !bind_answers_to(ip, bind) {
        (
            false,
            format!("The mothership listens on {bind} only; set COLONIZER_BIND=0.0.0.0:{port}"),
        )
    } else if !allowlist_accepts(&host, bind, allowed_hosts) {
        (
            false,
            format!("The mothership refuses the Host {host}; add {host} to COLONIZER_ALLOWED_HOSTS"),
        )
    } else {
        (
            true,
            "Plain http: the pairing and the sign-in cookie cross this network unencrypted, and the \
             phone cannot install the app or get notifications without HTTPS. Prefer the relay link"
                .to_string(),
        )
    };
    Origin {
        kind,
        url: format!("http://{host}:{port}"),
        reachable,
        secure: false,
        note: Some(note),
    }
}

/// Whether the bind's listener answers on `ip`: the bind names the IP exactly, or is a wildcard.
fn bind_answers_to(ip: IpAddr, bind: &str) -> bool {
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host {
        "0.0.0.0" => ip.is_ipv4(),
        "::" => true,
        other => other.parse::<IpAddr>().is_ok_and(|bound| bound == ip),
    }
}

/// What `host_guard`'s Host allowlist accepts for the name `hostname` (bracketed for IPv6): the
/// bind's own host and COLONIZER_ALLOWED_HOSTS. A wildcard bind widens nothing — the guard compares
/// the name, never the interface a connection arrived on.
fn allowlist_accepts(hostname: &str, bind: &str, allowed_hosts: &[String]) -> bool {
    let bind_host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    hostname == bind_host || allowed_hosts.iter().any(|h| h == hostname)
}

/// Which local address a packet to `remote` would leave from: a UDP `connect` records the route
/// and reads back the source address — nothing goes on the wire.
fn local_addr_for(remote: IpAddr) -> Option<IpAddr> {
    let bind = if remote.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(SocketAddr::new(remote, 53)).ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// The Tailscale address, if this machine is on a tailnet: the source address toward Tailscale's
/// resolver 100.100.100.100, accepted only inside 100.64.0.0/10.
fn detect_tailnet() -> Option<IpAddr> {
    let ip = local_addr_for(IpAddr::from([100, 100, 100, 100]))?;
    is_cg_nat(&ip).then_some(ip)
}

/// The LAN address: the private source address toward the internet — never the tailnet's.
fn detect_lan(exclude: Option<IpAddr>) -> Option<IpAddr> {
    let ip = local_addr_for(IpAddr::from([1, 1, 1, 1]))?;
    (is_site_local(&ip) && Some(ip) != exclude).then_some(ip)
}

/// 100.64.0.0/10, the carrier-grade NAT range Tailscale numbers its nodes in.
fn is_cg_nat(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
}

/// RFC1918 for IPv4, fc00::/7 (ULA) for IPv6.
fn is_site_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => match v4.octets() {
            [10, ..] => true,
            [172, second, ..] => (16..=31).contains(&second),
            [192, 168, ..] => true,
            _ => false,
        },
        IpAddr::V6(v6) => v6.octets()[0] & 0xfe == 0xfc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// Mint and open, against a pinned clock.
    fn opened(book: &mut Book, now: i64) -> Opened {
        let code = book.mint(now).unwrap();
        book.open(&code, "iPhone", now).unwrap()
    }

    #[test]
    fn an_invite_is_long_random_and_opens_once() {
        let mut book = Book::default();
        let code = book.mint(1000).unwrap();
        assert_eq!(code.len(), 64);
        assert!(code.bytes().all(|b| b.is_ascii_hexdigit()), "URL-safe: {code}");
        assert!(book.open(&code, "iPhone", 1000).is_some());
        assert!(
            book.open(&code, "iPhone", 1000).is_none(),
            "single use: a second browser gets nothing"
        );
        assert!(book.open("never-minted", "iPhone", 1000).is_none());
    }

    #[test]
    fn an_invite_and_a_pairing_live_five_minutes() {
        let mut book = Book::default();
        let code = book.mint(1000).unwrap();
        assert!(
            book.open(&code, "iPhone", 1000 + TTL_SECS).is_none(),
            "expired at the boundary"
        );
        assert!(book.open(&code, "iPhone", 1000).is_none(), "and gone for good, even rewound");

        let pairing = opened(&mut book, 2000);
        assert_eq!(book.claim(&pairing.id, &pairing.secret, 2000 + TTL_SECS - 1), Claim::Pending);
        assert_eq!(
            book.claim(&pairing.id, &pairing.secret, 2000 + TTL_SECS),
            Claim::Gone,
            "expired"
        );
        assert!(
            book.confirm(&pairing.confirm_code, 2000 + TTL_SECS).is_none(),
            "nothing to confirm"
        );
    }

    #[test]
    fn nothing_is_handed_over_until_the_cockpit_confirms() {
        let mut book = Book::default();
        let pairing = opened(&mut book, 1000);
        assert_eq!(pairing.confirm_code.len(), 7, "`123 456`: {}", pairing.confirm_code);
        for _ in 0..3 {
            assert_eq!(book.claim(&pairing.id, &pairing.secret, 1001), Claim::Pending);
        }
        let typed = normalize_confirm(&pairing.confirm_code);
        let wrong = if typed == "000000" { "111111" } else { "000000" };
        assert!(book.confirm(wrong, 1002).is_none(), "a wrong code approves nothing");
        assert!(book.confirm("12345", 1002).is_none(), "nor does a short one");
        assert_eq!(book.claim(&pairing.id, &pairing.secret, 1002), Claim::Pending);

        assert_eq!(
            book.confirm(&typed, 1003).as_deref(),
            Some("iPhone"),
            "typed without the space"
        );
        assert!(book.confirm(&typed, 1003).is_none(), "a confirm code approves once");
        assert_eq!(
            book.claim(&pairing.id, &pairing.secret, 1004),
            Claim::Approved("iPhone".into())
        );
        assert_eq!(book.claim(&pairing.id, &pairing.secret, 1004), Claim::Gone, "claimed once");
    }

    /// The pairing is bound to the browser that spent the invite: its id with a guessed secret, or
    /// with another pairing's secret, claims nothing.
    #[test]
    fn a_claim_needs_the_device_secret() {
        let mut book = Book::default();
        let mine = opened(&mut book, 1000);
        let theirs = opened(&mut book, 1000);
        book.confirm(&mine.confirm_code, 1001).unwrap();
        assert_eq!(book.claim(&mine.id, "guess", 1002), Claim::Gone);
        assert_eq!(book.claim(&mine.id, &theirs.secret, 1002), Claim::Gone);
        assert_eq!(book.claim(&mine.id, &mine.secret, 1002), Claim::Approved("iPhone".into()));
    }

    #[test]
    fn a_rejected_pairing_is_gone() {
        let mut book = Book::default();
        let pairing = opened(&mut book, 1000);
        assert!(book.reject(&pairing.id, 1001));
        assert!(!book.reject(&pairing.id, 1001));
        assert_eq!(book.claim(&pairing.id, &pairing.secret, 1002), Claim::Gone);
        assert!(book.confirm(&pairing.confirm_code, 1002).is_none());
    }

    #[test]
    fn open_invites_and_pairings_are_capped_not_evicted() {
        let mut book = Book::default();
        let codes: Vec<_> = (0..MAX_OPEN).map(|_| book.mint(1000).unwrap()).collect();
        assert!(book.mint(1000).is_none(), "at the cap, refuse");
        for code in &codes {
            book.open(code, "Phone", 1000).unwrap();
        }
        let extra = book.mint(1000).unwrap();
        assert!(book.open(&extra, "Phone", 1000).is_none(), "pairings are capped too");
        assert!(book.mint(1000 + TTL_SECS).is_some(), "expiry frees the slots");
    }

    #[test]
    fn failed_pairing_attempts_are_rate_limited_per_window() {
        let mut book = Book::default();
        for _ in 0..MAX_FAILURES {
            assert!(!book.limited(1000));
            book.failed(1000);
        }
        assert!(book.limited(1000), "the window is full");
        assert!(book.limited(1000 + ATTEMPT_WINDOW_SECS - 1));
        assert!(!book.limited(1000 + ATTEMPT_WINDOW_SECS), "the window slid past");
    }

    /// Through the store: guessing invites fills the window, and then even a real invite is
    /// refused (not spent) until it slides.
    #[test]
    fn guessing_invites_shuts_pairing_for_a_while() {
        let root = crate::tests::temp_root();
        let store = PhoneStore::load(&root);
        let real = store.book().mint(now_secs()).unwrap();
        for guess in 0..MAX_FAILURES {
            assert!(matches!(store.open(&format!("guess-{guess}"), "", false), Ok(None)));
        }
        assert!(
            store.open(&real, "iPhone", false).is_err(),
            "limited: refused before it is looked at"
        );
        assert!(store.book().invites.len() == 1, "and the real invite was not spent");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_phone_credential_is_its_own_and_revocable_per_device() {
        let root = crate::tests::temp_root();
        let store = PhoneStore::load(&root);
        let first = store.add("iPhone").unwrap();
        let second = store.add("Android phone").unwrap();
        assert!(first.starts_with(TOKEN_PREFIX));
        assert_ne!(first, second);
        assert!(store.authenticate(&first).is_some() && store.authenticate(&second).is_some());
        assert!(store.authenticate("cph_forged").is_none());
        assert!(store.authenticate("not-a-phone-token").is_none());
        let view = store.view();
        let one = view["devices"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(view["devices"][0]["label"], "iPhone");
        assert!(view["devices"][0].get("token_hash").is_none(), "the view carries no hash");

        let saved = std::fs::read_to_string(root.join("phones.json")).unwrap();
        assert!(!saved.contains(&first), "only hashes are stored");

        assert_eq!(store.revoke(&one).as_deref(), Some("iPhone"));
        assert!(store.revoke(&one).is_none(), "revoked once");
        assert!(store.authenticate(&first).is_none(), "the revoked phone is signed out");
        assert!(store.authenticate(&second).is_some(), "the other phone is not");
        let reloaded = PhoneStore::load(&root);
        assert!(
            reloaded.authenticate(&first).is_none(),
            "and it stays revoked across a restart"
        );
        assert!(reloaded.authenticate(&second).is_some());
        assert_eq!(reloaded.view()["devices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_list_view_carries_the_ranked_origins_and_no_secret() {
        let root = crate::tests::temp_root();
        let store = PhoneStore::load(&root);
        store.add("iPhone").unwrap();
        let list_origins = origins(
            Some(("abc123.my.colonizer.dev", true)),
            Some(ip("100.72.1.4")),
            None,
            "0.0.0.0:7878",
            &["100.72.1.4".to_string()],
        );
        let view = view_with_origins(store.view(), list_origins);
        let kinds: Vec<_> = view["origins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["relay", "tailnet"], "the same ranking the invite route answers");
        assert_eq!(view["origins"][1]["url"], "http://100.72.1.4:7878");
        assert_eq!(view["devices"][0]["label"], "iPhone", "the devices are still there");
        assert!(view["devices"][0].get("token_hash").is_none());
        // Bare origins only: no invite code, no credential rides along with the list.
        let text = view.to_string();
        assert!(!text.contains("\"code\""), "no invite code in the list");
        assert!(!text.contains("cph_"), "no credential in the list");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_phone_cannot_manage_credentials_or_the_remote_link() {
        for path in [
            "/api/phone/invites",
            "/api/phone/pairings/confirm",
            "/api/tokens",
            "/api/remote",
            "/api/remote/pairing/confirm",
            "/api/fleet/invites",
        ] {
            assert!(!phone_may(&Method::POST, path), "{path}");
        }
        assert!(!phone_may(&Method::DELETE, "/api/phone/devices/dev_1"));
        assert!(phone_may(&Method::GET, "/api/phone"), "reading is fine");
        assert!(phone_may(&Method::POST, "/api/sessions/abc/answer"));
        assert!(
            phone_may(&Method::POST, "/api/phones-lookalike"),
            "prefixes match whole segments"
        );
    }

    #[test]
    fn relay_on_comes_first_then_tailnet_then_lan() {
        let hosts = origins(
            Some(("abc123.my.colonizer.dev", true)),
            Some(ip("100.72.1.4")),
            Some(ip("192.168.1.5")),
            "0.0.0.0:7878",
            &["100.72.1.4".to_string(), "192.168.1.5".to_string()],
        );
        let kinds: Vec<_> = hosts.iter().map(|o| o.kind).collect();
        assert_eq!(kinds, ["relay", "tailnet", "lan"]);
        assert_eq!(hosts[0].url, "https://abc123.my.colonizer.dev");
        assert!(hosts[0].reachable && hosts[0].secure && hosts[0].note.is_none());
        assert_eq!(hosts[1].url, "http://100.72.1.4:7878");
        assert_eq!(hosts[2].url, "http://192.168.1.5:7878");
        for plain in &hosts[1..] {
            assert!(plain.reachable && !plain.secure);
            assert!(plain.note.as_deref().unwrap().contains("HTTPS"), "{:?}", plain.note);
        }
        let down = origins(Some(("abc123.my.colonizer.dev", false)), None, None, "127.0.0.1:7878", &[]);
        assert!(!down[0].reachable, "a relay link that is not connected is not reachable");
    }

    #[test]
    fn tailnet_only() {
        assert!(origins(None, None, None, "127.0.0.1:7878", &[]).is_empty());
        let hosts = origins(None, Some(ip("100.72.1.4")), None, "100.72.1.4:7878", &[]);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].kind, "tailnet");
        assert!(hosts[0].reachable, "the bind names the tailnet ip itself");
    }

    #[test]
    fn lan_only_says_what_to_fix() {
        let hosts = origins(None, None, Some(ip("192.168.1.5")), "127.0.0.1:7878", &[]);
        assert!(!hosts[0].reachable);
        assert!(hosts[0].note.as_deref().unwrap().contains("COLONIZER_BIND=0.0.0.0:7878"));
        let hosts = origins(None, None, Some(ip("192.168.1.5")), "0.0.0.0:7878", &[]);
        assert!(!hosts[0].reachable, "a wildcard bind still needs the Host allowlist");
        assert!(
            hosts[0]
                .note
                .as_deref()
                .unwrap()
                .contains("add 192.168.1.5 to COLONIZER_ALLOWED_HOSTS")
        );
        let hosts = origins(None, None, Some(ip("192.168.1.5")), "0.0.0.0:7878", &["192.168.1.5".into()]);
        assert!(hosts[0].reachable);
        let hosts = origins(None, None, Some(ip("fd7a::5")), "[::]:7878", &["[fd7a::5]".into()]);
        assert_eq!(hosts[0].url, "http://[fd7a::5]:7878");
        assert!(hosts[0].reachable);
    }

    #[test]
    fn the_address_filters_know_their_ranges() {
        assert!(is_cg_nat(&ip("100.64.0.1")) && is_cg_nat(&ip("100.127.255.254")));
        assert!(!is_cg_nat(&ip("100.128.0.1")) && !is_cg_nat(&ip("100.63.255.254")));
        for lan in ["10.0.0.5", "172.16.0.5", "172.31.255.5", "192.168.1.5", "fd7a::5"] {
            assert!(is_site_local(&ip(lan)), "{lan}");
        }
        for not_lan in ["172.32.0.5", "8.8.8.8", "100.72.1.4", "2001:db8::5"] {
            assert!(!is_site_local(&ip(not_lan)), "{not_lan}");
        }
    }
}
