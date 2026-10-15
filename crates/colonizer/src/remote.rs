//! Remote access: drive this mothership's cockpit from outside the machine by keeping one
//! outbound WebSocket to a relay and serving tunnelled cockpit traffic over it (#533). Nothing
//! is ever listened on: the mothership dials out, and a tunnelled request lands on the same
//! router as localhost with the same token check — admitted only while the switch is on, only
//! for its own tunnel host, with the Origin fence pinned to `https://<host>`.
//!
//! The identity is an Ed25519 key pair under `<config>/remote/` (key material is never logged);
//! the relay binds it to `<install_id>.…` at registration, and the mothership proves itself by
//! signing the relay's handshake challenge. The wire frames, caps and encodings are written down
//! in docs/protocol.md §6.10.

use crate::{ApiResult, App, AppError, Shared, activity, client_error, util};
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::{Method, StatusCode, header},
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt as _;
use ring::{
    rand::SecureRandom as _,
    signature::{Ed25519KeyPair, KeyPair as _},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::DuplexStream,
    net::TcpStream,
    sync::{RwLock, mpsc, watch},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{self, client::IntoClientRequest, protocol::CloseFrame},
};
use tower::ServiceExt as _;

/// The relay base; registration and the tunnel URL derive from it (`COLONIZER_REMOTE_URL`).
const DEFAULT_RELAY: &str = "wss://my.colonizer.dev";
/// The Ed25519 key under `<config>/remote/`, PKCS#8, 0600.
const KEY_FILE: &str = "key";
/// The switch and identity, `<config>/remote/state.json`.
const STATE_FILE: &str = "state.json";
/// The link credentials, hashed, `<config>/remote/links.json` (review finding R3).
const LINKS_FILE: &str = "links.json";
/// A link credential's prefix, telling it apart from the install token, `col_` and `cph_` at a glance.
const LINK_PREFIX: &str = "clk_";
/// The most devices signed in to the link at once.
const MAX_LINK_DEVICES: usize = 32;
/// The tunnel protocol version this build speaks; sent in the hello.
const VERSION: u64 = 1;
/// The most raw bytes one body chunk carries.
const CHUNK: usize = 48 * 1024;
/// The most concurrent streams per tunnel — requests and tunnelled websockets together.
const MAX_STREAMS: usize = 32;
/// The most bytes a tunnelled request body may buffer before it is refused.
const MAX_BODY: usize = 10 * 1024 * 1024;
/// Redial backoff bounds; reset by every successful handshake.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);
/// The relay's challenge, the registration POST, and the handshake, each bounded by these.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);
const REGISTER_WAIT: Duration = Duration::from_secs(10);
/// How long a tunnelled request body may take to fully arrive, per frame.
const BODY_WAIT: Duration = Duration::from_secs(60);
/// How long one response body frame may take to arrive before the stream is ended.
const STREAM_IDLE: Duration = Duration::from_secs(300);
/// The in-memory websocket server's per-connection buffer.
const DUPLEX_BYTES: usize = 64 * 1024;
/// Both sides ping every 20 s, and about a minute of silence ends the tunnel.
const PING_EVERY: Duration = Duration::from_secs(20);
const DEAD_AFTER: Duration = Duration::from_secs(60);
/// The longest challenge nonce the hello will sign.
const NONCE_CAP: usize = 1024;
/// Frames waiting to go to the relay at once; beyond this, stream tasks wait, so a relay that
/// stops reading slows the answer instead of growing the queue without limit.
const OUT_QUEUE: usize = 256;
/// The largest single message or frame the tunnel socket takes from the relay (review finding R5;
/// tungstenite's default is 64 MiB). The relay's biggest legitimate frame is a 48 KiB body chunk
/// as base64, or a browser websocket message it caps at 128 KiB (`WS_MSG_MAX`, tunnel.js) — at
/// most about 768 KiB once JSON-escaped — so a relay sending more ends the tunnel instead.
const TUNNEL_MESSAGE_MAX: usize = 1024 * 1024;
/// Request body frames waiting for their stream at once. The reader waits for room rather than
/// dropping one, and a stream reads its body until the end, so this only paces a burst.
const BODY_QUEUE: usize = 16;
/// Relay-to-cockpit websocket frames waiting for one tunnelled socket. A cockpit handler that falls
/// this far behind gets its socket closed (1008) rather than a queue that grows without limit.
const WS_QUEUE: usize = 64;
/// How long a tunnelled websocket's cockpit side may take to close once its relay side is gone,
/// before the stream is ended anyway and its slot freed.
const WS_CLOSE_GRACE: Duration = Duration::from_secs(5);
/// The longest base64 `chunk` a body frame may carry: [`CHUNK`] raw bytes, checked before any
/// decoding.
const CHUNK_B64_MAX: usize = CHUNK.div_ceil(3) * 4;

/// The switch and identity as persisted (`<config>/remote/state.json`). No file means off.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    enabled: bool,
    install_id: Option<String>,
    host: Option<String>,
    /// Whether the relay sends every browser through GitHub sign-in before forwarding anything
    /// (#534), on top of this machine's own pairing (#1086). `None` is a state file from before the
    /// setting: an install registered then keeps the gate it was built with, a fresh one starts
    /// without it. [`Saved::require_github`] reads it.
    #[serde(default)]
    require_github: Option<bool>,
}

impl Saved {
    /// The GitHub gate as it stands: what was set, else on for an install already registered (the
    /// relay keeps the gate on for those until told otherwise) and off for a new one.
    fn require_github(&self) -> bool {
        self.require_github.unwrap_or(self.install_id.is_some())
    }
}

/// The header the cockpit answers a tunnelled request with when the invite, pairing secret or
/// link/phone credential it carried did not authenticate (#1086). Its only value is `rejected`; the
/// relay counts it toward its throttle and strips it, so it never reaches a browser.
pub(crate) const CREDENTIAL_VERDICT: &str = "x-colonizer-credential";
/// The close code of a tunnelled websocket whose credential did not authenticate, for the same count.
const REJECTED_CLOSE: u16 = 4401;

/// Marks an answer to a tunnelled request as a rejected credential ([`CREDENTIAL_VERDICT`]).
pub(crate) fn mark_rejected(mut res: axum::response::Response) -> axum::response::Response {
    res.headers_mut()
        .insert(CREDENTIAL_VERDICT, axum::http::HeaderValue::from_static("rejected"));
    res
}

/// Whether a request presents a link (`clk_…`) or phone (`cph_…`) credential, as a bearer or as the
/// cockpit cookie: what the relay forwards without a GitHub session, and so what the cockpit must
/// report back when it does not authenticate.
pub(crate) fn presents_credential(headers: &axum::http::HeaderMap) -> bool {
    let credential = |token: &str| token.starts_with(LINK_PREFIX) || token.starts_with("cph_");
    crate::auth::bearer_token(headers).is_some_and(|t| credential(&t))
        || crate::auth::cookie_token(headers).is_some_and(|t| credential(&t))
}

/// Marks a request that arrived through the tunnel. Only in-process code can create one —
/// the supervisor inserts it after the relay frames are decoded — so `host_guard` can admit
/// requests no network peer could forge.
#[derive(Clone)]
pub struct Tunnelled {
    pub host: String,
}

/// One browser signed in to the link (review finding R3): what it is called and the hash of its
/// credential. The plaintext is handed over once, as that browser's cookie on the link's origin.
#[derive(Clone, Serialize, Deserialize)]
struct LinkDevice {
    id: String,
    label: String,
    token_hash: String,
    paired_at: chrono::DateTime<Utc>,
}

/// Marks a request the owner made through the link with a link credential; `host_guard` attaches it.
#[derive(Clone, Debug)]
pub(crate) struct LinkSession {
    pub(crate) id: String,
}

impl LinkSession {
    /// The key its revocation fires under (`auth::Revocation`): revoking the device, or a reset.
    pub(crate) fn revocation_key(&self) -> String {
        link_key(&self.id)
    }
}

fn link_key(id: &str) -> String {
    format!("link:{id}")
}

fn link_digest(secret: &str) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, secret.as_bytes()).as_ref())
}

/// Settings, identity and live link status, held on `App`.
pub struct Remote {
    dir: PathBuf,
    /// The link credentials (hashed), read once and saved on every change.
    links: std::sync::RwLock<Vec<LinkDevice>>,
    relay: RwLock<String>,
    saved: RwLock<Saved>,
    status: RwLock<Status>,
    /// Wakes the supervisor: the switch moved, or a reset wants an immediate redial. Only the
    /// change matters — every handler re-reads the state it just persisted.
    signal: watch::Sender<()>,
}

#[derive(Default)]
struct Status {
    connected: bool,
    since: Option<String>,
    /// The relay closed the tunnel because a newer one took this install over; the supervisor
    /// parks until the operator re-enables or resets instead of dialing back into a replacement
    /// war. Cleared by a fresh successful connect, a re-enable, or a reset.
    replaced: bool,
}

impl Remote {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let dir = config_dir.join("remote");
        let saved = match std::fs::read(dir.join(STATE_FILE)) {
            Ok(bytes) => serde_json::from_slice::<Saved>(&bytes)
                .with_context(|| format!("could not read {}", dir.join(STATE_FILE).display()))?,
            Err(_) => Saved::default(),
        };
        let relay = util::env_nonempty("COLONIZER_REMOTE_URL").unwrap_or_else(|| DEFAULT_RELAY.into());
        // An unreadable file signs every link device out: the safe direction.
        let links = match std::fs::read(dir.join(LINKS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!(
                    "remote: {} does not parse ({e}); no device is signed in to the link",
                    dir.join(LINKS_FILE).display()
                );
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Ok(Self {
            links: std::sync::RwLock::new(links),
            dir,
            relay: RwLock::new(relay.trim_end_matches('/').to_string()),
            saved: RwLock::new(saved),
            status: RwLock::new(Status::default()),
            signal: watch::channel(()).0,
        })
    }

    pub(crate) async fn enabled(&self) -> bool {
        self.saved.read().await.enabled
    }

    /// The install identity the relay registered, when remote access was ever enabled: what the
    /// signed install calls are addressed to (cratefield_push.rs rides them too).
    pub(crate) async fn install_id(&self) -> Option<String> {
        self.saved.read().await.install_id.clone()
    }

    /// The tunnel host (`<install_id>.my.colonizer.dev`) while remote access is on, and whether the
    /// link is connected right now — the `https://` origin a phone is offered first (phone.rs).
    pub(crate) async fn link(&self) -> Option<(String, bool)> {
        let saved = self.saved.read().await;
        let host = saved.host.clone().filter(|_| saved.enabled)?;
        Some((host, self.status.read().await.connected))
    }

    async fn saved(&self) -> Saved {
        self.saved.read().await.clone()
    }

    async fn relay(&self) -> String {
        self.relay.read().await.clone()
    }

    /// What `GET /api/remote` answers. A tunnel is only ever "connected" while the switch is on:
    /// the supervisor's status can lag behind a disable, and must not read as a live link then.
    /// The same gating keeps a stale "replaced" from outliving its switch.
    async fn view(&self) -> Value {
        let saved = self.saved.read().await;
        let status = self.status.read().await;
        let connected = saved.enabled && status.connected;
        json!({
            "enabled": saved.enabled,
            "host": saved.host,
            "connected": connected,
            "since": connected.then(|| status.since.clone()).flatten(),
            "replaced": saved.enabled && status.replaced,
            "require_github": saved.require_github(),
        })
    }

    async fn set_connected(&self, on: bool) {
        let mut status = self.status.write().await;
        status.connected = on;
        status.since = on.then(|| Utc::now().to_rfc3339());
        if on {
            status.replaced = false; // a fresh connect ends any parked-for-replaced state
        }
    }

    /// Flags the link as taken over by a newer tunnel; cleared by a fresh connect, a re-enable
    /// or a reset.
    async fn set_replaced(&self, on: bool) {
        self.status.write().await.replaced = on;
    }

    /// Makes `saved` the state, here and on disk. Callers hand over the mutated copy.
    async fn persist(&self, saved: Saved) -> Result<()> {
        *self.saved.write().await = saved.clone();
        std::fs::create_dir_all(&self.dir)?;
        util::write_atomic(&self.dir.join(STATE_FILE), &serde_json::to_vec(&saved)?).await
    }

    fn save_links(&self, links: &[LinkDevice]) {
        let saved = std::fs::create_dir_all(&self.dir)
            .map_err(anyhow::Error::from)
            .and_then(|()| Ok(serde_json::to_vec_pretty(links)?))
            .and_then(|bytes| util::write_private(&self.dir.join(LINKS_FILE), &bytes));
        if let Err(e) = saved {
            eprintln!("remote: could not save {}: {e:#}", self.dir.join(LINKS_FILE).display());
        }
    }

    /// The link device a presented credential belongs to, if it is a live link credential.
    pub(crate) fn authenticate_link(&self, token: &str) -> Option<LinkSession> {
        if !token.starts_with(LINK_PREFIX) {
            return None;
        }
        let hash = link_digest(token);
        let links = self.links.read().unwrap_or_else(|p| p.into_inner());
        links
            .iter()
            .find(|d| crate::gateway::constant_time_eq(d.token_hash.as_bytes(), hash.as_bytes()))
            .map(|d| LinkSession { id: d.id.clone() })
    }

    /// Mints a link credential for a newly confirmed device; the plaintext is returned once, to be
    /// set as that browser's cookie, and never stored. `None` past [`MAX_LINK_DEVICES`].
    pub(crate) fn add_link(&self, label: &str) -> Option<String> {
        let mut links = self.links.write().unwrap_or_else(|p| p.into_inner());
        if links.len() >= MAX_LINK_DEVICES {
            return None;
        }
        let token = format!("{LINK_PREFIX}{}", util::random_token());
        links.push(LinkDevice {
            id: format!("lnk_{}", util::short_id()),
            label: label.to_string(),
            token_hash: link_digest(&token),
            paired_at: Utc::now(),
        });
        self.save_links(&links);
        Some(token)
    }

    /// Signs one device out of the link: its credential stops working, and its in-flight requests
    /// and open sockets end now. `false` for an unknown device.
    fn revoke_link(&self, id: &str) -> bool {
        let mut links = self.links.write().unwrap_or_else(|p| p.into_inner());
        let before = links.len();
        links.retain(|d| d.id != id);
        if links.len() == before {
            return false;
        }
        self.save_links(&links);
        drop(links);
        crate::auth::Revocation::fire(&link_key(id));
        true
    }

    /// Rotates the link credentials (Reset link): every one is forgotten and revoked at once, so a
    /// copy the relay — or anyone — saw stops working, sockets included.
    fn rotate_links(&self) {
        let mut links = self.links.write().unwrap_or_else(|p| p.into_inner());
        let gone: Vec<String> = links.drain(..).map(|d| d.id).collect();
        self.save_links(&links);
        drop(links);
        for id in gone {
            crate::auth::Revocation::fire(&link_key(&id));
        }
    }

    fn links_view(&self) -> Vec<Value> {
        let links = self.links.read().unwrap_or_else(|p| p.into_inner());
        links
            .iter()
            .map(|d| json!({"id": d.id, "label": d.label, "paired_at": d.paired_at}))
            .collect()
    }

    /// The test relay's base URL (`COLONIZER_REMOTE_URL` is read once at construction, and
    /// ambient env vars must not leak into a parallel test run).
    #[cfg(test)]
    pub(crate) async fn set_relay(&self, url: String) {
        *self.relay.write().await = url.trim_end_matches('/').to_string();
    }

    /// A test's install identity, as a registration would have left it: the signed install calls
    /// are addressed to it (cratefield_push.rs's flush test).
    #[cfg(test)]
    pub(crate) async fn set_install_id(&self, install_id: &str) {
        self.saved.write().await.install_id = Some(install_id.to_string());
    }
}

// ---------------------------------------------------------------------------
// The API: `GET`/`PUT /api/remote`, `POST /api/remote/reset`; the pairing routes follow.
// ---------------------------------------------------------------------------

/// `GET /api/remote`
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(app.remote.view().await)
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/remote {"enabled": true|false}`. Enabling mints the key if this install has none,
/// registers it with the relay if needed, and wakes the supervisor. Disabling drops the tunnel
/// and every in-flight stream, keeping the key so the host comes back the same.
pub async fn put(
    State(app): State<Shared>,
    via: Option<Extension<crate::auth::Via>>,
    Json(body): Json<SetRequest>,
) -> ApiResult<Value> {
    if body.enabled == app.remote.enabled().await {
        return Ok(Json(app.remote.view().await)); // no change: nothing to do, nothing to record
    }
    let view = set_enabled(&app, body.enabled).await?;
    record_change(&app, if body.enabled { "remote.enable" } else { "remote.disable" }, via).await;
    Ok(Json(view))
}

/// `POST /api/remote/reset`: a fresh key and install identity, persisted in place of the old
/// ones; reconnects at once if the switch was on. The way to retire a key that leaked.
pub async fn reset(State(app): State<Shared>, via: Option<Extension<crate::auth::Via>>) -> ApiResult<Value> {
    let view = reset_identity(&app).await?;
    record_change(&app, "remote.reset", via).await;
    // Every link credential goes with the old link (R3): signed out at once, sockets included.
    // Last, because a reset made through the link revokes its own request here.
    app.remote.rotate_links();
    Ok(Json(view))
}

#[derive(Deserialize)]
pub struct RequireGithubRequest {
    require_github: bool,
}

/// `PUT /api/remote/require-github {"require_github": bool}` (#1086): whether the relay sends every
/// browser through GitHub sign-in before it forwards anything. Off, a device pairs with this
/// machine's pair code alone — the single-use invite and the six digits confirmed here — and the
/// relay forwards only invites and link/phone credentials, which this cockpit checks. On, the
/// GitHub owner gate of #534 comes first, as before. Local only: loosening or tightening who may
/// reach the link is this machine's call. With a link registered, the relay is told first (a
/// signed `PUT …/settings`), and nothing is saved unless it agreed; before the first enable the
/// choice is only saved, and the registration carries it. Records `remote.require_github`.
pub async fn put_require_github(
    State(app): State<Shared>,
    tunnelled: Option<Extension<Tunnelled>>,
    via: Option<Extension<crate::auth::Via>>,
    Json(body): Json<RequireGithubRequest>,
) -> ApiResult<Value> {
    local_only(tunnelled.as_ref(), "change the GitHub sign-in requirement")?;
    let mut saved = app.remote.saved().await;
    let changed = saved.require_github() != body.require_github;
    if !changed && saved.install_id.is_none() {
        return Ok(Json(app.remote.view().await)); // no change: nothing to do, nothing to record
    }
    // With a link registered the relay is always told, even when this machine already reads the
    // same value: the relay holds the gate that counts, and a PUT is how the two are put back in
    // step if they ever drifted (an install registered against a relay older than the setting).
    if saved.install_id.is_some() {
        let request = json!({ "require_github": body.require_github });
        match install_call(&app, Method::PUT, "/settings", Some(&request)).await? {
            (StatusCode::OK, answer) if answer["require_github"] == body.require_github => {}
            // A relay deployed before #1086 has no settings endpoint: its catch-all answers 404
            // `not found`, and it keeps the GitHub gate on whatever this machine says.
            (StatusCode::NOT_FOUND, answer) if answer["error"] == "not found" => {
                return Err(client_error(
                    StatusCode::BAD_GATEWAY,
                    "the relay does not support pairing without GitHub yet (it needs a redeploy); nothing was changed",
                ));
            }
            (status, answer) => return Err(relay_refused(status, &answer)),
        }
    }
    saved.require_github = Some(body.require_github);
    app.remote.persist(saved).await?;
    if !changed {
        return Ok(Json(app.remote.view().await)); // re-sent to the relay, but nothing to record
    }
    let mut entry = activity::Entry::new("remote.require_github", "you");
    entry.via = activity::via_name(via.map(|Extension(v)| v));
    entry.target = Some(if body.require_github { "on" } else { "off" }.into());
    entry.section = Some("remote".into());
    activity::record(&app, entry).await;
    Ok(Json(app.remote.view().await))
}

async fn set_enabled(app: &Shared, on: bool) -> Result<Value, AppError> {
    let mut saved = app.remote.saved().await;
    if on {
        let key = load_key(&app.remote.dir).map_err(|e| {
            client_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not load or mint the access key: {e:#}"),
            )
        })?;
        if saved.install_id.is_none() {
            let (install_id, host, require_github) = register(&app.remote.relay().await, &key, saved.require_github()).await?;
            saved.install_id = Some(install_id);
            saved.host = Some(host);
            saved.require_github = Some(require_github);
        }
        saved.enabled = true;
        app.remote.persist(saved).await?;
        app.remote.set_replaced(false).await; // a fresh enable dials from scratch
        app.remote.signal.send_replace(());
    } else {
        saved.enabled = false;
        app.remote.persist(saved).await?;
        app.remote.signal.send_replace(());
        app.remote.set_connected(false).await;
    }
    Ok(app.remote.view().await)
}

async fn reset_identity(app: &Shared) -> Result<Value, AppError> {
    // Mint and register first, so a failed registration leaves the old key and link in place.
    let doc = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not mint a key: {e}")))?;
    let key = Ed25519KeyPair::from_pkcs8(doc.as_ref()).map_err(|e| {
        client_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not read the minted key: {e}"),
        )
    })?;
    let relay = app.remote.relay().await;
    // The new install is registered with the gate the old one had: a reset is not a way to drop it.
    let (install_id, host, require_github) = register(&relay, &key, app.remote.saved().await.require_github()).await?;
    // The old install is retired at the relay (review finding R2): its row, owner and pairings are
    // deleted and its tunnel closed, signed with the old key before it is overwritten, so a leaked
    // copy of that key reaches nothing afterwards. If the relay cannot be told, the reset stops
    // here and the old link stays in place — a reset that left the old install live would be the
    // finding itself — and the install just registered is withdrawn again, best effort. Only an
    // unreadable old key goes on regardless: nothing can sign for that install any more.
    if let Some(old) = app.remote.saved().await.install_id {
        match read_key(&app.remote.dir) {
            Ok(old_key) => {
                if let Err(e) = retire_install(&relay, &old, &old_key).await {
                    if let Err(undo) = signed_call(&relay, &install_id, &key, Method::DELETE, "", None).await {
                        eprintln!(
                            "remote: could not withdraw the unused new install at the relay: {}",
                            undo.message()
                        );
                    }
                    return Err(client_error(
                        StatusCode::BAD_GATEWAY,
                        &format!("could not retire the old link at the relay, so it was kept: {}", e.message()),
                    ));
                }
            }
            Err(e) => eprintln!("remote: the old link cannot be retired at the relay, its key is unreadable: {e:#}"),
        }
    }
    util::write_private(&app.remote.dir.join(KEY_FILE), doc.as_ref())?;
    let mut saved = app.remote.saved().await;
    saved.install_id = Some(install_id);
    saved.host = Some(host);
    saved.require_github = Some(require_github);
    app.remote.persist(saved).await?;
    app.remote.set_replaced(false).await; // a reset is the way out of a taken-over link
    app.remote.signal.send_replace(()); // an immediate redial under the new identity
    Ok(app.remote.view().await)
}

/// Retires `install_id` at the relay: a signed `DELETE /api/installs/<id>` deletes the install, its
/// owner and its pairings, and closes its tunnel. An install the relay no longer knows is already
/// retired. A relay older than that endpoint answers its catch-all `404 not found`; there the old
/// owner is at least unbound (`DELETE …/owner`, #663), which is all such a relay can do.
async fn retire_install(relay: &str, install_id: &str, key: &Ed25519KeyPair) -> Result<(), AppError> {
    match signed_call(relay, install_id, key, Method::DELETE, "", None).await? {
        (StatusCode::NO_CONTENT | StatusCode::OK, _) => Ok(()),
        (StatusCode::NOT_FOUND, answer) if answer["error"] == "unknown install" => Ok(()),
        (StatusCode::NOT_FOUND, _) => match signed_call(relay, install_id, key, Method::DELETE, "/owner", None).await? {
            (StatusCode::NO_CONTENT | StatusCode::OK, _) => Ok(()),
            (status, answer) => Err(relay_refused(status, &answer)),
        },
        (status, answer) => Err(relay_refused(status, &answer)),
    }
}

/// The key pair to sign with, from the stored file — minted only when there is no file yet. Key
/// material lives only here and in the 0600 file; it is never logged. A file that exists but
/// cannot be read or parsed is an error, never a silent replacement: the registered install_id
/// still names the old key, so a new one would fail every handshake — `POST /api/remote/reset`
/// is the way out of a broken key file.
fn load_key(dir: &Path) -> Result<Ed25519KeyPair> {
    let path = dir.join(KEY_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
            let doc = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).map_err(|e| anyhow!("{e}"))?;
            util::write_private(&path, doc.as_ref())?;
            doc.as_ref().to_vec()
        }
        Err(e) => return Err(anyhow!("could not read {}: {e}", path.display())),
    };
    Ed25519KeyPair::from_pkcs8(&bytes)
        .map_err(|e| anyhow!("could not parse {} ({e}); POST /api/remote/reset replaces it", path.display()))
}

/// The stored key pair, never minted: the signed relay calls must sign with the key the relay
/// registered, and a fresh key would only ever earn a `401 bad signature`.
fn read_key(dir: &Path) -> Result<Ed25519KeyPair> {
    let path = dir.join(KEY_FILE);
    let bytes = std::fs::read(&path).map_err(|e| anyhow!("could not read {}: {e}", path.display()))?;
    Ed25519KeyPair::from_pkcs8(&bytes)
        .map_err(|e| anyhow!("could not parse {} ({e}); POST /api/remote/reset replaces it", path.display()))
}

/// Registers the public key with the relay: `POST /api/installs` answers `{"install_id", "host"}`,
/// and the tunnel URL and admitted `Host` are both derived from that host. `require_github` asks
/// the relay for its GitHub sign-in gate on the new install (#1086); without it the install pairs
/// with this machine's pair code alone. Answers the gate the relay actually gave the install, from
/// its answer's `require_github`; a relay older than the setting names none and always gates, so a
/// missing field reads as on.
async fn register(relay: &str, key: &Ed25519KeyPair, require_github: bool) -> Result<(String, String, bool), AppError> {
    let bad = |message: String| client_error(StatusCode::BAD_GATEWAY, &message);
    let url = format!("{}/api/installs", http_base(relay)?);
    let answer = tokio::time::timeout(
        REGISTER_WAIT,
        reqwest::Client::new()
            .post(&url)
            .json(&json!({
                "public_key": util::b64_encode(key.public_key().as_ref()),
                "require_github": require_github,
            }))
            .send(),
    )
    .await
    .map_err(|_| bad("the relay did not answer the registration in time".into()))?
    .map_err(|e| bad(format!("could not reach the relay: {e}")))?;
    if !answer.status().is_success() {
        return Err(bad(format!("the relay refused the registration ({})", answer.status())));
    }
    let answer: Value = answer
        .json()
        .await
        .map_err(|e| bad(format!("the relay's registration answer was not JSON: {e}")))?;
    let field = |name| {
        answer[name]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| bad(format!("the relay's registration answer named no {name}")))
    };
    let granted = answer["require_github"].as_bool().unwrap_or(true);
    Ok((field("install_id")?, field("host")?, granted))
}

/// `wss://my.colonizer.dev` → `https://my.colonizer.dev` (and ws → http, for a local test relay).
/// Plaintext `ws://` is accepted only for a loopback host (review finding R3): everything the
/// tunnel and the signed calls carry — cookies, tokens, bodies — would otherwise cross the network
/// in the clear, so a relay anywhere else must be `wss://`.
fn http_base(relay: &str) -> Result<String> {
    if let Some(rest) = relay.strip_prefix("wss://") {
        Ok(format!("https://{rest}"))
    } else if let Some(rest) = relay.strip_prefix("ws://") {
        let base = format!("http://{rest}");
        let host = reqwest::Url::parse(&base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
        let loopback = host.is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !loopback {
            bail!("COLONIZER_REMOTE_URL may be plaintext ws:// only for a loopback relay; use wss://, got {relay:?}")
        }
        Ok(base)
    } else {
        bail!("COLONIZER_REMOTE_URL must be a ws:// or wss:// URL, got {relay:?}")
    }
}

/// The activity log line for a switch change, recorded by the handler (not the route layer, which
/// has no rule for these routes) and only after the state actually changed.
async fn record_change(app: &Shared, kind: &str, via: Option<Extension<crate::auth::Via>>) {
    let mut entry = activity::Entry::new(kind, "you");
    entry.via = activity::via_name(via.map(|Extension(v)| v));
    entry.target = Some("remote access".into());
    entry.section = Some("remote".into());
    activity::record(app, entry).await;
}

// ---------------------------------------------------------------------------
// Pairing (#534, #599): the relay parks a GitHub sign-in behind a six-digit code, and the local
// cockpit decides who owns the link. Every route here is a signed call to the relay's install
// endpoints (services/relay/src/worker.js `signed`): headers `x-colonizer-ts` (unix seconds) and
// `x-colonizer-sig`, standard base64 of Ed25519 over `METHOD\npath\nts\nbody`, with the key the
// relay registered. The relay holds the codes, their expiry and single use; the mothership holds
// the key that may confirm one, so a browser can neither mint nor confirm a pairing on its own.
// ---------------------------------------------------------------------------

/// What a pairing code must look like before it is worth a relay round trip; the relay applies
/// the same rule (400).
fn is_pairing_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

/// One signed call to `<relay>/api/installs/<install_id><suffix>`: the relay's status and its JSON
/// answer (`Value::Null` for an empty one). Failing to reach the relay at all is a 502.
async fn signed_call(
    relay: &str,
    install_id: &str,
    key: &Ed25519KeyPair,
    method: Method,
    suffix: &str,
    body: Option<&Value>,
) -> Result<(StatusCode, Value), AppError> {
    let bad = |message: String| client_error(StatusCode::BAD_GATEWAY, &message);
    let url = reqwest::Url::parse(&format!("{}/api/installs/{install_id}{suffix}", http_base(relay)?))
        .map_err(|e| bad(format!("bad relay URL: {e}")))?;
    let raw = body.map(Value::to_string).unwrap_or_default();
    let ts = Utc::now().timestamp();
    // The relay rebuilds this from the URL it received, so it signs the path as sent: a relay base
    // with a path prefix is signed with the prefix.
    let message = format!("{method}\n{}\n{ts}\n{raw}", url.path());
    let sig = util::b64_encode(key.sign(message.as_bytes()).as_ref());
    let mut request = reqwest::Client::new()
        .request(method, url)
        .header("x-colonizer-ts", ts.to_string())
        .header("x-colonizer-sig", sig);
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json").body(raw);
    }
    let answer = tokio::time::timeout(REGISTER_WAIT, request.send())
        .await
        .map_err(|_| bad("the relay did not answer in time".into()))?
        .map_err(|e| bad(format!("could not reach the relay: {e}")))?;
    let status = StatusCode::from_u16(answer.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = answer
        .bytes()
        .await
        .map_err(|e| bad(format!("the relay's answer broke off: {e}")))?;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Ok((status, value))
}

/// The same signed call, for this install: its identity and key, or a 409 when remote access has
/// never been switched on (no link, so nothing to pair). `pub(crate)` because the Cratefield
/// delivery channel (cratefield_push.rs, issue #1085) rides the same install endpoints.
pub(crate) async fn install_call(
    app: &App,
    method: Method,
    suffix: &str,
    body: Option<&Value>,
) -> Result<(StatusCode, Value), AppError> {
    let Some(install_id) = app.remote.saved().await.install_id else {
        return Err(client_error(
            StatusCode::CONFLICT,
            "remote access has no link yet; switch it on first",
        ));
    };
    let key = read_key(&app.remote.dir).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    signed_call(&app.remote.relay().await, &install_id, &key, method, suffix, body).await
}

/// A relay answer the cockpit is not meant to see as-is: a 401 means the relay no longer knows this
/// key (or the clock is off by minutes), a 404 on the install that the relay forgot it. Both are a
/// broken link, so a 502 naming what happened, never a status the cockpit would read as its own.
fn relay_refused(status: StatusCode, answer: &Value) -> AppError {
    let said = answer["error"].as_str().unwrap_or("no reason given");
    let hint = match status {
        StatusCode::UNAUTHORIZED => {
            "; the relay does not accept this install's key — is the clock right? A reset registers a new one"
        }
        StatusCode::NOT_FOUND => "; the relay does not know this install — a reset registers it again",
        _ => "",
    };
    client_error(
        StatusCode::BAD_GATEWAY,
        &format!("the relay refused ({status}: {said}){hint}"),
    )
}

/// The activity line for a pairing decision; the target is the GitHub account it was about.
async fn record_pairing(app: &Shared, kind: &str, login: Option<&str>, via: Option<Extension<crate::auth::Via>>) {
    let mut entry = activity::Entry::new(kind, "you");
    entry.via = activity::via_name(via.map(|Extension(v)| v));
    entry.target = Some(login.map_or_else(|| "remote access".to_string(), |login| format!("@{login}")));
    entry.section = Some("remote".into());
    activity::record(app, entry).await;
}

/// `GET /api/remote/pairing`: the relay's view, as is —
/// `{"owner": {"github_login"} | null, "pending": [{"code", "github_login", "expires_at"}]}`.
pub async fn pairing(State(app): State<Shared>) -> ApiResult<Value> {
    match install_call(&app, Method::GET, "/pairing", None).await? {
        (StatusCode::OK, view) => Ok(Json(view)),
        (status, answer) => Err(relay_refused(status, &answer)),
    }
}

#[derive(Deserialize)]
pub struct CodeRequest {
    code: String,
}

/// Refuses a request that came through the tunnel. Binding an owner decides who may reach the
/// link at all, so it happens only on this machine: were it reachable through the tunnel, anyone
/// who got a request through could pair themselves. The marker is an extension only the tunnel
/// client sets (see [`Tunnelled`]), never a header a peer could send.
pub(crate) fn local_only(tunnelled: Option<&Extension<Tunnelled>>, what: &str) -> Result<(), AppError> {
    match tunnelled {
        Some(_) => Err(client_error(
            StatusCode::FORBIDDEN,
            &format!("{what} only from the cockpit on this machine, not through the remote link"),
        )),
        None => Ok(()),
    }
}

/// `POST /api/remote/pairing/confirm {"code"}`: binds the GitHub account waiting behind that code
/// as the link's owner. Local only. 400 for a code that is not six digits, 404 for an unknown,
/// expired or already-used code, 409 when an owner is already bound — the relay's answers.
pub async fn confirm_pairing(
    State(app): State<Shared>,
    tunnelled: Option<Extension<Tunnelled>>,
    via: Option<Extension<crate::auth::Via>>,
    Json(body): Json<CodeRequest>,
) -> ApiResult<Value> {
    local_only(tunnelled.as_ref(), "confirm a pairing code")?;
    if !is_pairing_code(&body.code) {
        return Err(client_error(StatusCode::BAD_REQUEST, "code must be 6 digits"));
    }
    let request = json!({ "code": body.code });
    match install_call(&app, Method::POST, "/pairing/confirm", Some(&request)).await? {
        (StatusCode::OK, answer) => {
            record_pairing(&app, "remote.pair", answer["owner"]["github_login"].as_str(), via).await;
            Ok(Json(answer))
        }
        (status @ (StatusCode::BAD_REQUEST | StatusCode::CONFLICT), answer) => Err(client_error(
            status,
            answer["error"].as_str().unwrap_or("the relay refused the code"),
        )),
        // The relay's 404 is ambiguous: an unknown install says `unknown install`, a code that is
        // gone says `no such pairing`. Only the second is the cockpit's 404.
        (StatusCode::NOT_FOUND, answer) if answer["error"] == "no such pairing" => Err(client_error(
            StatusCode::NOT_FOUND,
            "no such pairing: the code is wrong, expired or already used",
        )),
        (status, answer) => Err(relay_refused(status, &answer)),
    }
}

/// `POST /api/remote/pairing/reject {"code"}`: drops one pending pairing, so that sign-in never
/// becomes the owner. Local only, like confirming. 404 for a code that is not pending.
pub async fn reject_pairing(
    State(app): State<Shared>,
    tunnelled: Option<Extension<Tunnelled>>,
    via: Option<Extension<crate::auth::Via>>,
    Json(body): Json<CodeRequest>,
) -> ApiResult<Value> {
    local_only(tunnelled.as_ref(), "reject a pairing code")?;
    if !is_pairing_code(&body.code) {
        return Err(client_error(StatusCode::BAD_REQUEST, "code must be 6 digits"));
    }
    let request = json!({ "code": body.code });
    match install_call(&app, Method::POST, "/pairing/reject", Some(&request)).await? {
        (StatusCode::OK, answer) => {
            record_pairing(&app, "remote.pair_reject", answer["github_login"].as_str(), via).await;
            Ok(Json(answer))
        }
        (StatusCode::BAD_REQUEST, answer) => Err(client_error(
            StatusCode::BAD_REQUEST,
            answer["error"].as_str().unwrap_or("the relay refused the code"),
        )),
        (StatusCode::NOT_FOUND, answer) if answer["error"] == "no such pairing" => Err(client_error(
            StatusCode::NOT_FOUND,
            "no such pairing: the code is wrong, expired or already used",
        )),
        (status, answer) => Err(relay_refused(status, &answer)),
    }
}

/// `DELETE /api/remote/owner`: unbinds the link's owner and drops every pending pairing; the
/// owner's sessions at the relay stop working on their next request. The next sign-in pairs anew.
/// Local only: an unbind that cut the link's owner off mid-session is this machine's call.
pub async fn unbind_owner(
    State(app): State<Shared>,
    tunnelled: Option<Extension<Tunnelled>>,
    via: Option<Extension<crate::auth::Via>>,
) -> Result<StatusCode, AppError> {
    local_only(tunnelled.as_ref(), "unbind the owner")?;
    match install_call(&app, Method::DELETE, "/owner", None).await? {
        (StatusCode::NO_CONTENT | StatusCode::OK, _) => {
            record_pairing(&app, "remote.unpair", None, via).await;
            Ok(StatusCode::NO_CONTENT)
        }
        (status, answer) => Err(relay_refused(status, &answer)),
    }
}

// ---------------------------------------------------------------------------
// Signing a browser in on the link (review finding R3). The install token is never accepted
// through the tunnel; the owner's browser elsewhere gets a link credential of its own instead,
// handed over the way a phone is paired (phone.rs): a single-use invite opened on the link, a
// six-digit code confirmed here, then a claim that sets the credential as that browser's cookie on
// the link's origin. Reset link rotates them all.
// ---------------------------------------------------------------------------

/// `GET /api/remote/devices`: the browsers signed in to the link and those waiting for their code.
pub async fn devices(State(app): State<Shared>) -> Json<Value> {
    let pending = app.phones.book().pending_link_view(crate::phone::now_secs());
    Json(json!({"devices": app.remote.links_view(), "pending": pending}))
}

/// `POST /api/remote/devices/invites`: a single-use, five-minute invite, as the link URL to open on
/// the other device. It is a ticket to ask, never a credential, and it opens only on the link.
pub async fn device_invite(State(app): State<Shared>) -> ApiResult<Value> {
    let Some((host, _)) = app.remote.link().await else {
        return Err(client_error(StatusCode::CONFLICT, "switch remote access on first"));
    };
    let now = crate::phone::now_secs();
    let Some(code) = app.phones.book().mint_link(now) else {
        return Err(client_error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many open invites; use one or wait a few minutes for them to expire",
        ));
    };
    Ok(Json(json!({
        "url": format!("https://{host}/?pair={code}"),
        "expires_at": crate::phone::rfc3339(now + crate::phone::TTL_SECS),
        "ttl_secs": crate::phone::TTL_SECS,
    })))
}

/// `POST /api/remote/devices/confirm {"code"}`: approves the browser showing this code. Local only:
/// approving through the link would let whoever got a request through approve themselves.
pub async fn confirm_device(
    State(app): State<Shared>,
    tunnelled: Option<Extension<Tunnelled>>,
    via: Option<Extension<crate::auth::Via>>,
    Json(body): Json<CodeRequest>,
) -> ApiResult<Value> {
    local_only(tunnelled.as_ref(), "approve a device")?;
    let now = crate::phone::now_secs();
    let label = {
        let mut book = app.phones.book();
        if book.limited(now) {
            return Err(client_error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many failed pairing attempts; wait a minute",
            ));
        }
        match book.confirm_link(&body.code, now) {
            Some(label) => label,
            None => {
                book.failed(now);
                return Err(client_error(
                    StatusCode::NOT_FOUND,
                    "no device is waiting with that code: it is wrong, expired or already used",
                ));
            }
        }
    };
    record_change(&app, "remote.device_approve", via).await;
    Ok(Json(json!({"label": label})))
}

/// `DELETE /api/remote/devices/{id}`: signs one browser out of the link, at once.
pub async fn revoke_device(
    State(app): State<Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
    via: Option<Extension<crate::auth::Via>>,
) -> Result<StatusCode, AppError> {
    if !app.remote.revoke_link(&id) {
        return Err(client_error(StatusCode::NOT_FOUND, "no such device"));
    }
    record_change(&app, "remote.device_revoke", via).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// The supervisor.
// ---------------------------------------------------------------------------

/// Spawned by `serve()` with the finished router: while remote access is enabled, keeps exactly
/// one tunnel to the relay open, serving frames until the relay or the operator drops it, and
/// redialing with exponential backoff (1 s doubling to 60 s, jittered, reset once the relay has
/// accepted the hello — not by a bare TCP/WebSocket connect). A close that says this tunnel was
/// replaced — another mothership dialed on the same install key — is never redialed: the two
/// motherships would keep replacing each other, so the supervisor parks instead, waiting on the
/// switch/reset signal like the disabled state, until the operator re-enables or resets. A
/// disable or reset tears the connection and every in-flight stream down at once.
pub async fn run(app: Shared, router: Router) {
    let mut signal = app.remote.signal.subscribe();
    loop {
        while !app.remote.enabled().await {
            if signal.changed().await.is_err() {
                return; // the Remote is gone; so is the job
            }
        }
        let mut delay = RECONNECT_MIN;
        loop {
            let mut redial = false;
            let mut replaced = false;
            tokio::select! {
                (accepted, was_replaced) = connect_and_serve(&app, &router) => {
                    if accepted {
                        delay = RECONNECT_MIN;
                    }
                    replaced = was_replaced;
                }
                _ = signal.changed() => redial = true,
            }
            if !app.remote.enabled().await {
                break;
            }
            if redial {
                continue; // a reset wants the new identity live now, not after the backoff
            }
            if replaced {
                // The relay handed this install to a newer tunnel: dialing again would take the
                // link straight back, and the two motherships would replace each other forever.
                // Park until the operator acts — a re-enable or reset signals, which dials again
                // (a fresh successful connect clears the status), a disable waits upstairs.
                eprintln!(
                    "remote: the relay gave this link to a newer tunnel; parked until remote access is re-enabled or reset"
                );
                app.remote.set_replaced(true).await;
                if signal.changed().await.is_err() {
                    return; // the Remote is gone; so is the job
                }
                if !app.remote.enabled().await {
                    break; // switched off while parked: the outer loop waits for the re-enable
                }
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(delay + jitter()) => {}
                _ = signal.changed() => {
                    if !app.remote.enabled().await {
                        break;
                    }
                    delay = RECONNECT_MIN;
                    continue;
                }
            }
            delay = (delay * 2).min(RECONNECT_MAX);
        }
    }
}

/// The jitter of the redial backoff, so a relay blip does not align every mothership's retries.
fn jitter() -> Duration {
    let mut byte = [0u8; 1];
    let _ = ring::rand::SystemRandom::new().fill(&mut byte);
    Duration::from_millis(u64::from(byte[0] % 128) * 4)
}

/// One live tunnel: dial, answer the challenge, serve frames. Answers `(accepted, replaced)`:
/// `accepted` only once the relay has shown it took the hello — its first frame on the
/// connection — so a relay that hangs up on a bad signature is not mistaken for a working tunnel
/// (that would reset the backoff into a silent one-second redial loop); `replaced` when the close
/// that ended it named a newer tunnel. Everything spawned here is aborted when the future is
/// dropped — relay drop, disable, or reset. It watches the signal through its own subscription,
/// so the caller's stays free.
async fn connect_and_serve(app: &Shared, router: &Router) -> (bool, bool) {
    let saved = app.remote.saved().await;
    let (Some(install_id), Some(host)) = (saved.install_id, saved.host) else {
        return (false, false); // enabled without an identity cannot happen: enabling registers first
    };
    let Ok(key) = load_key(&app.remote.dir) else {
        eprintln!("remote: cannot load the access key from {}", app.remote.dir.display());
        return (false, false);
    };
    let relay = app.remote.relay().await;
    if let Err(e) = http_base(&relay) {
        eprintln!("remote: not dialing: {e:#}");
        return (false, false);
    }
    let Ok(request) = format!("{relay}/tunnel/{install_id}").into_client_request() else {
        eprintln!("remote: bad relay URL {relay:?}");
        return (false, false);
    };
    let mut signal = app.remote.signal.subscribe();
    let ws = tokio::select! {
        ws = handshake(request, &key, &install_id) => match ws {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("remote: dialing {relay} failed: {e:#}");
                return (false, false);
            }
        },
        _ = signal.changed() => return (false, false),
    };
    let (accepted, replaced) = serve_connection(app, router, ws, host).await;
    app.remote.set_connected(false).await;
    if !accepted && !replaced {
        eprintln!("remote: the relay hung up before answering the hello (bad signature, or wrong install?); keeping the backoff");
    }
    (accepted, replaced)
}

/// `true` for the close codes that mean "another tunnel took this install over": `4000`, which
/// the relay sends today (`tunnel.js:114`), and `4409`, which docs/remote-tunnel.md pins for it.
/// Either parks the client instead of redialing into a replacement war.
fn is_replaced_close(code: u16) -> bool {
    matches!(code, 4000 | 4409)
}

/// Dials the relay, waits for `{"t":"challenge","nonce"}` and answers
/// `{"t":"hello","sig","ts","version":1}` with Ed25519 over `nonce ‖ install_id ‖ ts` (UTF-8,
/// ts decimal unix seconds, also a JSON number).
async fn handshake(
    request: tungstenite::handshake::client::Request,
    key: &Ed25519KeyPair,
    install_id: &str,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let (mut ws, _) = tokio::time::timeout(
        HANDSHAKE_WAIT,
        tokio_tungstenite::connect_async_tls_with_config(request, Some(tunnel_config()), false, None),
    )
    .await
    .context("dialing the relay timed out")?
    .context("the relay refused the tunnel websocket")?;
    let challenge = tokio::time::timeout(HANDSHAKE_WAIT, ws.next())
        .await
        .context("the relay never sent its challenge")?;
    let nonce = match challenge {
        Some(Ok(tungstenite::Message::Text(text))) => match serde_json::from_str::<Frame>(&text) {
            Ok(Frame::Challenge { nonce }) => nonce,
            _ => bail!("the relay's first frame was not a challenge"),
        },
        _ => bail!("the relay closed before challenging"),
    };
    if nonce.len() > NONCE_CAP {
        bail!("the relay's challenge nonce is longer than {NONCE_CAP} bytes");
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.as_secs());
    let sig = key.sign(format!("{nonce}{install_id}{ts}").as_bytes());
    let hello = json!({"t": "hello", "sig": util::b64_encode(sig.as_ref()), "ts": ts, "version": VERSION});
    ws.send(tungstenite::Message::Text(hello.to_string().into()))
        .await
        .context("could not answer the relay's challenge")?;
    Ok(ws)
}

/// The tunnel socket's limits: no message or frame from the relay past [`TUNNEL_MESSAGE_MAX`].
fn tunnel_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(TUNNEL_MESSAGE_MAX))
        .max_frame_size(Some(TUNNEL_MESSAGE_MAX))
}

/// What a relay frame carries for one open tunnelled websocket.
enum WsIn {
    Msg(tungstenite::Message),
    Close(u16),
}

/// Request bodies waiting for their remaining `body` frames, keyed by stream id; a sender's
/// presence is also the stream's claim on the id.
type Bodies = Arc<Mutex<HashMap<String, mpsc::Sender<(String, bool)>>>>;

/// The state of one live connection: the frame queue every task sends through, the routing
/// tables for open streams, the stream budget, and the spawned tasks torn down on exit.
struct Conn {
    host: String,
    out: mpsc::Sender<String>,
    /// Feeds one side of a duplex pair to the in-memory websocket server per `ws_open`.
    conns: mpsc::Sender<DuplexStream>,
    ws_in: Arc<Mutex<HashMap<String, mpsc::Sender<WsIn>>>>,
    bodies: Bodies,
    live: Arc<AtomicUsize>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// Aborts everything the connection spawned when the supervisor leaves it, whether because the
/// relay dropped or because the operator disabled or reset remote access mid-flight.
struct Cleanup(Arc<Conn>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for task in self.0.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

impl Conn {
    /// Takes one of the [`MAX_STREAMS`] slots, or `None` when they are all busy.
    // `fetch_update` is deprecated as `try_update` on the newest stable Rust; the old name still
    // builds on every toolchain we support, the new one only on the newest.
    #[allow(deprecated)]
    fn admit(&self) -> Option<Slot> {
        self.live
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < MAX_STREAMS).then_some(n + 1))
            .ok()
            .map(|_| Slot(self.live.clone()))
    }

    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|task| !task.is_finished()); // reaped here, so the vec stays as small as the tunnel
        tasks.push(tokio::spawn(task));
    }

    /// Queues `frame` without waiting — the reader loop's and refusals' small replies. A full
    /// queue means the relay stopped reading; the dead-link timer ends the tunnel soon enough.
    fn nudge(&self, frame: String) {
        let _ = self.out.try_send(frame);
    }

    /// Queues `frame`, waiting for room: what the streaming paths use, so backpressure reaches
    /// the answer instead of the queue growing without limit.
    async fn emit(&self, frame: String) {
        let _ = self.out.send(frame).await;
    }

    /// One body chunk of a streaming answer.
    async fn send_body(&self, id: &Value, bytes: &[u8], end: bool) {
        self.emit(json!({"t": "body", "id": id, "chunk": util::b64_encode(bytes), "end": end}).to_string())
            .await;
    }

    /// The `res` plus empty end-of-body frame that refuses a request.
    fn refuse_res(&self, id: &Value, status: StatusCode) {
        self.nudge(json!({"t": "res", "id": id, "status": status.as_u16(), "headers": []}).to_string());
        self.nudge(json!({"t": "body", "id": id, "chunk": "", "end": true}).to_string());
    }

    fn close_ws(&self, id: &Value, code: u16) {
        self.nudge(json!({"t": "ws_close", "id": id, "code": code}).to_string());
    }
}

/// One occupied stream slot; freed when the serving task ends, however it ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Removes a stream's routing entry when its task ends — however it ends, including a refusal
/// mid-setup. Compares channels, so an entry can only be removed while it is still this
/// stream's own. It holds its channel only weakly: the map's sender is the only strong one, so
/// removing the entry (an overflowing websocket) ends the stream's receiver.
struct Routing<T> {
    map: Arc<Mutex<HashMap<String, mpsc::Sender<T>>>>,
    id: String,
    mine: mpsc::WeakSender<T>,
}

impl<T> Drop for Routing<T> {
    fn drop(&mut self) {
        let mut map = self.map.lock().unwrap();
        if map
            .get(&self.id)
            .is_some_and(|tx| self.mine.upgrade().is_some_and(|mine| tx.same_channel(&mine)))
        {
            map.remove(&self.id);
        }
    }
}

/// A tunnel path worth serving: it starts with `/` and stays within a URL's sensible length.
fn plausible_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= 2048
}

/// Sets the connection up and pumps it until the socket dies, pinging every [`PING_EVERY`] and
/// giving up after [`DEAD_AFTER`] of silence. The connection only counts as accepted — the live
/// link, and a backoff reset — once the relay's first frame arrives, proving it took the hello.
/// Answers `(accepted, replaced)`: a close frame whose code says a newer tunnel took over sets
/// `replaced` for the supervisor to park on, not redial.
async fn serve_connection(
    app: &Shared,
    router: &Router,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    host: String,
) -> (bool, bool) {
    let (out, mut out_rx) = mpsc::channel(OUT_QUEUE);
    let (conns, conns_rx) = mpsc::channel(8);
    let conn = Arc::new(Conn {
        host,
        out,
        conns,
        ws_in: Arc::new(Mutex::new(HashMap::new())),
        bodies: Arc::new(Mutex::new(HashMap::new())),
        live: Arc::new(AtomicUsize::new(0)),
        tasks: Mutex::new(Vec::new()),
    });
    let cleanup = Cleanup(conn.clone());
    // The in-memory server tunnelled websocket upgrades land on: axum over duplex streams, with
    // the tunnelled marker layered onto the real router.
    let ws_router = router.clone().layer(Extension(Tunnelled { host: conn.host.clone() }));
    conn.spawn(async move {
        if let Err(e) = axum::serve(InMemory { rx: conns_rx }, ws_router).await {
            eprintln!("remote: tunnel websocket server stopped: {e}");
        }
    });
    // One writer owns the socket's sending half; everything sends frames through `conn.out`.
    let (mut sink, mut stream) = ws.split();
    conn.spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if sink.send(tungstenite::Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
    });
    let (mut accepted, mut last_frame) = (false, Instant::now());
    let mut replaced = false;
    loop {
        tokio::select! {
            frame = stream.next() => {
                match frame {
                    Some(Ok(tungstenite::Message::Text(text))) => {
                        if !accepted {
                            accepted = true;
                            app.remote.set_connected(true).await;
                        }
                        last_frame = Instant::now();
                        dispatch(&conn, router, &text).await;
                    }
                    Some(Ok(tungstenite::Message::Close(frame))) => {
                        replaced = frame.is_some_and(|f| is_replaced_close(u16::from(f.code)));
                        break;
                    }
                    Some(Err(_)) | None => break,
                    // Any other frame — a ping above all — still proves the relay took the hello.
                    Some(Ok(_)) => {
                        if !accepted {
                            accepted = true;
                            app.remote.set_connected(true).await;
                        }
                        last_frame = Instant::now();
                    }
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(last_frame + PING_EVERY)) => {
                if last_frame.elapsed() >= DEAD_AFTER {
                    eprintln!("remote: the relay went quiet; redialing");
                    break;
                }
                conn.nudge(json!({"t": "ping"}).to_string());
            }
        }
    }
    drop(cleanup);
    (accepted, replaced)
}

/// One frame from the relay: a ping, a new stream, or traffic for an open one. Frames outside the
/// v1 set are ignored rather than allowed to kill the tunnel.
async fn dispatch(conn: &Arc<Conn>, router: &Router, frame: &str) {
    let Ok(frame) = serde_json::from_str::<Frame>(frame) else {
        return;
    };
    match frame {
        Frame::Ping => conn.nudge(json!({"t": "pong"}).to_string()),
        Frame::Pong | Frame::Challenge { .. } => {}
        Frame::Req {
            id,
            method,
            path,
            headers,
        } => start_req(conn, router.clone(), id, method, path, headers),
        Frame::Body { id, chunk, end } => {
            let sink = conn.bodies.lock().unwrap().get(&id.to_string()).cloned();
            if let Some(sink) = sink {
                // Waits for room: the stream is reading its body, so this is a pause, not a stall.
                // Once it has its body it drops the receiver, and late frames fail here at once.
                let _ = sink.send((chunk, end)).await;
            }
        }
        Frame::WsOpen { id, path, headers } => start_ws(conn, id, path, headers),
        Frame::WsMsg { id, data, binary } => {
            let message = if binary {
                b64_empty_ok(&data).map(|bytes| tungstenite::Message::Binary(bytes.into()))
            } else {
                Some(tungstenite::Message::Text(data.into()))
            };
            let Some(message) = message else { return };
            ws_deliver(conn, &id, WsIn::Msg(message));
        }
        Frame::WsClose { id, code } => ws_deliver(conn, &id, WsIn::Close(code)),
    }
}

/// Hands one relay frame to its tunnelled websocket without waiting: a socket whose cockpit handler
/// has [`WS_QUEUE`] frames unread is closed (1008) and forgotten, so neither the tunnel's reader
/// nor the mothership's memory waits on one slow socket (review finding R5).
fn ws_deliver(conn: &Conn, id: &Value, frame: WsIn) {
    let key = id.to_string();
    let sink = conn.ws_in.lock().unwrap().get(&key).cloned();
    let Some(sink) = sink else { return };
    if let Err(mpsc::error::TrySendError::Full(_)) = sink.try_send(frame) {
        let mut routes = conn.ws_in.lock().unwrap();
        if routes.get(&key).is_some_and(|tx| tx.same_channel(&sink)) {
            routes.remove(&key); // the last strong sender: the socket's pump sees the end
        }
        drop(routes);
        conn.close_ws(id, 1008);
    }
}

// ---------------------------------------------------------------------------
// Serving a tunnelled request.
// ---------------------------------------------------------------------------

fn start_req(conn: &Arc<Conn>, router: Router, id: Value, method: String, path: String, headers: Option<Value>) {
    // The routing entry is claimed under the lock, so a duplicate id is refused without touching
    // the open stream — and its guard is what removes the entry, on every exit below.
    let (tx, rx) = mpsc::channel(BODY_QUEUE);
    let routing = match conn.bodies.lock().unwrap().entry(id.to_string()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            conn.refuse_res(&id, StatusCode::SERVICE_UNAVAILABLE);
            return;
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            let mine = tx.downgrade();
            slot.insert(tx);
            Routing {
                map: conn.bodies.clone(),
                id: id.to_string(),
                mine,
            }
        }
    };
    let Some(slot) = conn.admit() else {
        conn.refuse_res(&id, StatusCode::SERVICE_UNAVAILABLE);
        return; // dropping `routing` releases the entry with it
    };
    let task_conn = conn.clone();
    conn.spawn(async move {
        serve_req(task_conn, router, id, method, path, headers, rx, slot, routing).await;
    });
}

/// Builds the request the router would have seen from a local caller, marks it tunnelled, and
/// answers it in-process; the answer goes back as `res` plus chunked `body` frames.
#[allow(clippy::too_many_arguments)]
async fn serve_req(
    conn: Arc<Conn>,
    router: Router,
    id: Value,
    method: String,
    path: String,
    headers: Option<Value>,
    mut chunks: mpsc::Receiver<(String, bool)>,
    _slot: Slot,
    // Pure drop guard: holds the stream's routing entry until the task ends, whichever way.
    _routing: Routing<(String, bool)>,
) {
    let refused = |status: StatusCode| conn.refuse_res(&id, status);
    let Ok(method) = Method::from_bytes(method.as_bytes()) else {
        refused(StatusCode::BAD_REQUEST);
        return;
    };
    // The body streams from `body` frames until `end`; GET and HEAD never carry one.
    let mut body = Vec::new();
    if !matches!(method, Method::GET | Method::HEAD) {
        loop {
            match tokio::time::timeout(BODY_WAIT, chunks.recv()).await {
                Ok(Some((chunk, end))) => {
                    // Sized before it is decoded: no chunk the relay may send is longer.
                    if chunk.len() > CHUNK_B64_MAX {
                        refused(StatusCode::PAYLOAD_TOO_LARGE);
                        return;
                    }
                    let Some(bytes) = b64_empty_ok(&chunk) else {
                        refused(StatusCode::BAD_REQUEST);
                        return;
                    };
                    if body.len() + bytes.len() > MAX_BODY {
                        refused(StatusCode::PAYLOAD_TOO_LARGE);
                        return;
                    }
                    body.extend_from_slice(&bytes);
                    if end {
                        break;
                    }
                }
                // The relay went away, or never ended the body: answer 408 either way, so the
                // stream is never left hanging without a status.
                Ok(None) | Err(_) => {
                    refused(StatusCode::REQUEST_TIMEOUT);
                    return;
                }
            }
        }
    }
    // The body is in: body frames still arriving for this stream are dropped from here on, never
    // queued (the routing entry stays, so the id stays taken).
    drop(chunks);
    if !plausible_path(&path) {
        refused(StatusCode::BAD_REQUEST);
        return;
    }
    let mut builder = Request::builder().method(method).uri(path.as_str());
    for (name, value) in clean_headers(headers, false) {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let Ok(mut request) = builder
        .header(header::HOST, conn.host.as_str())
        .body(axum::body::Body::from(body))
    else {
        refused(StatusCode::BAD_REQUEST);
        return;
    };
    request.extensions_mut().insert(Tunnelled { host: conn.host.clone() });
    let res = match router.oneshot(request).await {
        Ok(res) => res,
        Err(never) => match never {}, // the router's service cannot fail
    };
    let head: Vec<Value> = res
        .headers()
        .iter()
        // A header value that is not visible ASCII cannot ride a JSON frame; it is dropped.
        .filter(|(name, value)| !is_hop_by_hop(name.as_str()) && value.to_str().is_ok())
        .map(|(name, value)| {
            let value = value.to_str().unwrap_or_default();
            if name == header::SET_COOKIE {
                json!([name.as_str(), secure_cookie(value)])
            } else {
                json!([name.as_str(), value])
            }
        })
        .collect();
    conn.emit(json!({"t": "res", "id": &id, "status": res.status().as_u16(), "headers": head}).to_string())
        .await;
    // Stream the body in capped chunks. The last frame always carries `end` — a single empty one
    // when the body is empty — even when the body ends in an error, because the status is out.
    let mut body = res.into_body();
    let mut chunk: Vec<u8> = Vec::with_capacity(CHUNK);
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(STREAM_IDLE, body.frame()).await {
        let Ok(data) = frame.into_data() else { continue };
        let mut rest = data.as_ref();
        while !rest.is_empty() {
            let take = (CHUNK - chunk.len()).min(rest.len());
            chunk.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if chunk.len() == CHUNK {
                conn.send_body(&id, &chunk, false).await;
                chunk.clear();
            }
        }
    }
    conn.send_body(&id, &chunk, true).await;
}

// ---------------------------------------------------------------------------
// Serving a tunnelled websocket.
// ---------------------------------------------------------------------------

fn start_ws(conn: &Arc<Conn>, id: Value, path: String, headers: Option<Value>) {
    // The routing entry is claimed under the lock, so a duplicate id is refused without touching
    // the open stream — and its guard is what removes the entry, on every exit below.
    let (tx, from_relay) = mpsc::channel(WS_QUEUE);
    let routing = match conn.ws_in.lock().unwrap().entry(id.to_string()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            conn.close_ws(&id, 1008);
            return;
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            let mine = tx.downgrade();
            slot.insert(tx);
            Routing {
                map: conn.ws_in.clone(),
                id: id.to_string(),
                mine,
            }
        }
    };
    let Some(slot) = conn.admit() else {
        conn.close_ws(&id, 1013); // Try Again Later: the tunnel's streams are all busy
        return; // dropping `routing` releases the entry with it
    };
    let task_conn = conn.clone();
    conn.spawn(async move {
        serve_ws(task_conn, id, path, headers, from_relay, slot, routing).await;
    });
}

/// Hands one side of a duplex pair to the in-memory websocket server and handshakes over the
/// other (an upgrade needs a real connection for axum's `WebSocketUpgrade`), then pumps frames
/// both ways until either side closes.
async fn serve_ws(
    conn: Arc<Conn>,
    id: Value,
    path: String,
    headers: Option<Value>,
    mut from_relay: mpsc::Receiver<WsIn>,
    _slot: Slot,
    routing: Routing<WsIn>,
) {
    if !plausible_path(&path) {
        conn.close_ws(&id, 1008);
        return;
    }
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BYTES);
    if conn.conns.send(server_io).await.is_err() {
        return; // the connection is already gone
    }
    let Ok(mut request) = format!("ws://{}{}", conn.host, path).into_client_request() else {
        conn.close_ws(&id, 1014);
        return;
    };
    for (name, value) in clean_headers(headers, true) {
        if let (Ok(name), Ok(value)) = (name.parse::<header::HeaderName>(), value.parse::<header::HeaderValue>()) {
            request.headers_mut().insert(name, value);
        }
    }
    let inner = match tokio_tungstenite::client_async(request, client_io).await {
        Ok((inner, _)) => inner,
        // The guard turned the socket's credential down (#1086): a close the relay counts toward
        // its throttle, as it counts a rejected HTTP request by its verdict header.
        Err(tungstenite::Error::Http(res)) if res.headers().contains_key(CREDENTIAL_VERDICT) => {
            conn.close_ws(&id, REJECTED_CLOSE);
            return;
        }
        Err(_) => {
            conn.close_ws(&id, 1014); // Bad Gateway: the inner upgrade did not happen
            return;
        }
    };
    let (mut inner_out, mut inner_in) = inner.split();
    // Relay → cockpit. The channel ends when [`ws_deliver`] dropped an overflowing socket: the
    // cockpit side is closed too. Either way the pump's end is reported, so the loop below cannot
    // outlive it by more than [`WS_CLOSE_GRACE`] even when the cockpit never answers the close.
    let (pump_done, mut pumped) = tokio::sync::oneshot::channel::<()>();
    conn.spawn(async move {
        let _done = pump_done; // dropped, and so reported, however the pump ends
        loop {
            let Some(msg) = from_relay.recv().await else {
                let _ = inner_out
                    .send(tungstenite::Message::Close(Some(CloseFrame {
                        code: 1008.into(),
                        reason: Default::default(),
                    })))
                    .await;
                break;
            };
            let message = match msg {
                WsIn::Msg(message) => message,
                WsIn::Close(code) => {
                    let _ = inner_out
                        .send(tungstenite::Message::Close(Some(CloseFrame {
                            code: code.into(),
                            reason: Default::default(),
                        })))
                        .await;
                    break;
                }
            };
            if inner_out.send(message).await.is_err() {
                break;
            }
        }
    });
    // Cockpit → relay, until the cockpit closes or, once the pump has ended, the grace runs out.
    let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    loop {
        let next = tokio::select! {
            next = inner_in.next() => next,
            _ = &mut pumped, if grace.is_none() => {
                grace = Some(Box::pin(tokio::time::sleep(WS_CLOSE_GRACE)));
                continue;
            }
            () = async { grace.as_mut().expect("guarded").await }, if grace.is_some() => {
                conn.close_ws(&id, 1008);
                break;
            }
        };
        match next {
            Some(Ok(tungstenite::Message::Text(text))) => {
                conn.emit(json!({"t": "ws_msg", "id": &id, "data": text.as_str(), "binary": false}).to_string())
                    .await;
            }
            Some(Ok(tungstenite::Message::Binary(bytes))) => {
                conn.emit(json!({"t": "ws_msg", "id": &id, "data": util::b64_encode(&bytes), "binary": true}).to_string())
                    .await;
            }
            Some(Ok(tungstenite::Message::Close(frame))) => {
                conn.close_ws(&id, frame.map_or(1000, |f| u16::from(f.code)));
                break;
            }
            Some(Ok(_)) => {}
            Some(Err(_)) => {
                conn.close_ws(&id, 1011);
                break;
            }
            None => {
                conn.close_ws(&id, 1000);
                break;
            }
        }
    }
    drop(routing);
}

// ---------------------------------------------------------------------------
// An `axum::serve` listener over in-memory duplex streams.
// ---------------------------------------------------------------------------

/// The listening half of the tunnelled-websocket server: one queued duplex per `ws_open`. When
/// every connection is gone it parks forever rather than spins; the serve task is aborted with
/// the rest of the connection.
struct InMemory {
    rx: mpsc::Receiver<DuplexStream>,
}

impl axum::serve::Listener for InMemory {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(io) => (io, ()),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Frames and headers.
// ---------------------------------------------------------------------------

/// A frame the relay can send. `headers` on input is an array of `[name, value]` pairs or an
/// object of name → value; `id` is whatever the relay chose and is echoed back untouched.
#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Frame {
    Challenge {
        nonce: String,
    },
    Req {
        id: Value,
        method: String,
        path: String,
        #[serde(default)]
        headers: Option<Value>,
    },
    Body {
        id: Value,
        #[serde(default)]
        chunk: String,
        #[serde(default)]
        end: bool,
    },
    WsOpen {
        id: Value,
        path: String,
        #[serde(default)]
        headers: Option<Value>,
    },
    WsMsg {
        id: Value,
        data: String,
        #[serde(default)]
        binary: bool,
    },
    WsClose {
        id: Value,
        #[serde(default)]
        code: u16,
    },
    Ping,
    Pong,
}

/// `util::b64_decode`, with the empty string meaning empty bytes: the end-of-body frame's
/// `chunk` is exactly that, and so is a zero-length binary websocket message.
fn b64_empty_ok(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() { Some(Vec::new()) } else { util::b64_decode(s) }
}

/// Header pairs from either relay shape, with hop-by-hop headers (which cover `connection` and
/// `upgrade`) dropped; for a websocket handshake, also `host` and the `Sec-WebSocket-*` ones the
/// mothership's own handshake must set itself, and anything that cannot ride in a header value.
fn clean_headers(headers: Option<Value>, ws: bool) -> Vec<(String, String)> {
    let pairs = match headers {
        Some(Value::Array(pairs)) => pairs
            .into_iter()
            .filter_map(|pair| {
                let pair = pair.as_array()?;
                Some((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()))
            })
            .collect::<Vec<_>>(),
        Some(Value::Object(map)) => map
            .into_iter()
            .filter_map(|(name, value)| value.as_str().map(|v| (name, v.to_string())))
            .collect(),
        _ => Vec::new(),
    };
    pairs
        .into_iter()
        .filter(|(name, value)| {
            let lower = name.to_ascii_lowercase();
            !(is_hop_by_hop(&lower)
                || lower == "host"
                || (ws && lower.starts_with("sec-websocket-"))
                || value.contains(['\n', '\r', '\0']))
        })
        .collect()
}

/// A `Set-Cookie` value as it may leave through the tunnel (review finding R4): the browser reaches
/// the cockpit there only over `https://<host>`, so every cookie gets `Secure` if it lacks it. The
/// cockpit's own cookies stay without it on localhost and the LAN, which are plain `http://`.
fn secure_cookie(value: &str) -> String {
    let secure = value
        .split(';')
        .skip(1)
        .any(|attribute| attribute.trim().eq_ignore_ascii_case("secure"));
    if secure {
        value.to_string()
    } else {
        format!("{value}; Secure")
    }
}

/// The RFC 9110 §7.6.1 hop-by-hop headers, stripped on both sides of the tunnel.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves:
/// the remote-access tunnel dials the relay while the switch is on. It gets a clone of the finished
/// router, so tunnelled requests land on exactly what localhost would.
pub(crate) fn start_tasks(app: &crate::Shared, router: &Router) {
    tokio::spawn(run(app.clone(), router.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/remote", routing::get(status).put(put))
        .route("/api/remote/reset", routing::post(reset))
        .route("/api/remote/require-github", routing::put(put_require_github))
        .route("/api/remote/pairing", routing::get(pairing))
        .route("/api/remote/pairing/confirm", routing::post(confirm_pairing))
        .route("/api/remote/pairing/reject", routing::post(reject_pairing))
        .route("/api/remote/owner", routing::delete(unbind_owner))
        .route("/api/remote/devices", routing::get(devices))
        .route("/api/remote/devices/invites", routing::post(device_invite))
        .route("/api/remote/devices/confirm", routing::post(confirm_device))
        .route("/api/remote/devices/{id}", routing::delete(revoke_device))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_app;
    use axum::{
        body::Body,
        middleware,
        response::Response,
        routing::{get, post},
    };
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    type RelayWs = WebSocketStream<tokio::net::TcpStream>;

    /// A temp dir removed when the test ends — even on a failed assert.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root() -> TempRoot {
        TempRoot(std::env::temp_dir().join(format!("colonizer-remote-{}", util::short_id())))
    }

    // -- The fake relay -----------------------------------------------------

    /// A fake relay on 127.0.0.1:0: `POST /api/installs` registers a public key, `DELETE
    /// /api/installs/<id>` retires one, and
    /// `GET /tunnel/<id>` speaks the challenge/hello handshake — verifying the signature against
    /// the registered key the way the real relay must — then hands the live socket to the test.
    async fn spawn_relay() -> (String, mpsc::UnboundedReceiver<RelayWs>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tunnels, tunnels_rx) = mpsc::unbounded_channel();
        let key: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let registered = key.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut io, _)) = listener.accept().await else { return };
                let registered = registered.clone();
                let tunnels = tunnels.clone();
                tokio::spawn(async move {
                    let head = read_head(&mut io).await;
                    let mut parts = head.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let path = parts.next().unwrap_or_default().to_string();
                    if method == "POST" && path == "/api/installs" {
                        let mut body = vec![0u8; header_of(&head, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0)];
                        io.read_exact(&mut body).await.unwrap();
                        let answer: Value = serde_json::from_slice(&body).unwrap();
                        let public = util::b64_decode(answer["public_key"].as_str().unwrap()).unwrap();
                        assert_eq!(public.len(), 32, "public_key is the raw key bytes");
                        *registered.lock().unwrap() = Some(public);
                        let install_id = util::short_id();
                        let body =
                            json!({"install_id": install_id, "host": format!("{install_id}.my.colonizer.dev")}).to_string();
                        io.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    } else if method == "DELETE" && path.starts_with("/api/installs/") {
                        // A reset retiring the old install (R2); this fake checks no signature.
                        io.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                    } else if let Some(install_id) = path.strip_prefix("/tunnel/") {
                        let ws_key = header_of(&head, "sec-websocket-key").unwrap();
                        let accept = util::b64_encode(
                            ring::digest::digest(
                                &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
                                format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
                            )
                            .as_ref(),
                        );
                        io.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
                        let mut ws = WebSocketStream::from_raw_socket(io, tungstenite::protocol::Role::Server, None).await;
                        ws.send(tungstenite::Message::Text(
                            json!({"t": "challenge", "nonce": "nonce-1"}).to_string().into(),
                        ))
                        .await
                        .unwrap();
                        let hello = tokio::time::timeout(Duration::from_secs(5), ws.next())
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap();
                        let tungstenite::Message::Text(hello) = hello else {
                            panic!("expected the hello, got {hello:?}")
                        };
                        let hello: Value = serde_json::from_str(&hello).unwrap();
                        assert_eq!(hello["t"], "hello");
                        assert_eq!(hello["version"], 1, "the tunnel speaks version 1");
                        let ts = hello["ts"].as_u64().expect("ts is a JSON number of unix seconds");
                        let sig = util::b64_decode(hello["sig"].as_str().unwrap()).unwrap();
                        let public = registered
                            .lock()
                            .unwrap()
                            .clone()
                            .expect("the install registered before dialing");
                        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &public)
                            .verify(format!("nonce-1{install_id}{ts}").as_bytes(), &sig)
                            .expect("the hello signature verifies against the registered key");
                        tunnels.send(ws).unwrap();
                    }
                });
            }
        });
        (format!("ws://127.0.0.1:{port}"), tunnels_rx)
    }

    async fn read_head(io: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            io.read_exact(&mut byte).await.unwrap();
            buf.push(byte[0]);
        }
        String::from_utf8(buf).unwrap()
    }

    fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines().find_map(|line| {
            let (field, value) = line.split_once(':')?;
            field.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    // -- The cockpit router, as serve() would hand the supervisor one -------

    /// A handler that parks every request until the test releases it, announcing each one: enough
    /// concurrent streams to hold the cap open.
    #[derive(Clone)]
    struct Park {
        opened: mpsc::UnboundedSender<()>,
        release: watch::Receiver<bool>,
    }

    fn idle_park() -> Park {
        let (opened, _) = mpsc::unbounded_channel();
        let (_, release) = watch::channel(false);
        Park { opened, release }
    }

    async fn park_handler(Extension(park): Extension<Park>) -> &'static str {
        let _ = park.opened.send(());
        let mut release = park.release.clone();
        release.changed().await.ok();
        "released"
    }

    /// A websocket whose handler never reads: what a stuck cockpit socket looks like to the tunnel.
    async fn deaf_socket(ws: axum::extract::WebSocketUpgrade) -> Response {
        ws.on_upgrade(|socket| async move {
            let _held = socket;
            std::future::pending::<()>().await
        })
    }

    async fn big_answer() -> Vec<u8> {
        vec![0xAB; 120_000] // 2.4 chunks at the 48 KiB cap
    }

    fn remote_router(app: &Shared, park: Park) -> Router {
        Router::new()
            .route("/api/remote", get(status).put(put))
            .route("/api/remote/reset", post(reset))
            .route("/api/remote/require-github", axum::routing::put(put_require_github))
            .route("/api/remote/pairing", get(pairing))
            .route("/api/remote/pairing/confirm", post(confirm_pairing))
            .route("/api/remote/pairing/reject", post(reject_pairing))
            .route("/api/remote/owner", axum::routing::delete(unbind_owner))
            .route("/api/remote/devices", get(devices))
            .route("/api/remote/devices/invites", post(device_invite))
            .route("/api/remote/devices/confirm", post(confirm_device))
            .route("/api/remote/devices/{id}", axum::routing::delete(revoke_device))
            .route("/", get(|| async { "the cockpit" }))
            .merge(crate::phone::routes())
            .route("/api/activity", get(crate::activity::list))
            .route("/api/stream", get(crate::stream::handler))
            .route("/api/big", get(big_answer))
            .route("/api/demo", get(demo))
            .route("/api/deaf", get(deaf_socket))
            .route("/api/park", get(park_handler))
            .layer(Extension(park))
            .layer(middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    /// The app with its supervisor running against a fresh fake relay, and remote access switched
    /// on through the real guard, the way the cockpit would. The router the supervisor holds
    /// carries the given park route, so a test can hold stream slots open inside it.
    async fn enabled_app_with(park: Park) -> (Shared, Router, mpsc::UnboundedReceiver<RelayWs>, TempRoot) {
        let root = temp_root();
        let app = test_app(root.path());
        let (relay, tunnels) = spawn_relay().await;
        app.remote.set_relay(relay).await;
        let router = remote_router(&app, park);
        tokio::spawn(run(app.clone(), router.clone()));
        switch(&app, &router, true).await;
        (app, router, tunnels, root)
    }

    async fn enabled_app() -> (Shared, Router, mpsc::UnboundedReceiver<RelayWs>, TempRoot) {
        enabled_app_with(idle_park()).await
    }

    /// `PUT /api/remote` through the real guard.
    async fn switch(app: &Shared, router: &Router, enabled: bool) -> Response {
        let request = Request::builder()
            .method(Method::PUT)
            .uri("/api/remote")
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "enabled": enabled }).to_string()))
            .unwrap();
        router.clone().oneshot(request).await.unwrap()
    }

    /// `Bearer <link credential>`: what authenticates the owner through the tunnel (R3), minted
    /// fresh for the test the way a confirmed hand-over would.
    fn link_bearer(app: &Shared) -> String {
        format!("Bearer {}", app.remote.add_link("test").unwrap())
    }

    // -- Driving the relay side of a live tunnel ----------------------------

    async fn next_tunnel(tunnels: &mut mpsc::UnboundedReceiver<RelayWs>) -> RelayWs {
        tokio::time::timeout(Duration::from_secs(10), tunnels.recv())
            .await
            .expect("timed out waiting for a tunnel to connect")
            .expect("the relay keeps handing out tunnels")
    }

    /// The next relay frame as JSON, with a timeout, so a hung tunnel fails the test, not the clock.
    async fn next_frame(ws: &mut RelayWs, what: &str) -> Value {
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("the tunnel socket stayed open")
            .expect("the tunnel socket stayed readable");
        let tungstenite::Message::Text(text) = frame else {
            panic!("expected a text frame for {what}")
        };
        serde_json::from_str(&text).expect("a JSON frame")
    }

    fn req_frame(id: &str, method: &str, path: &str, headers: Value) -> tungstenite::Message {
        json!({ "t": "req", "id": id, "method": method, "path": path, "headers": headers })
            .to_string()
            .into()
    }

    /// The single body frame that ends a request body, as the contract requires of the relay.
    fn body_frame(id: &str, bytes: &[u8]) -> tungstenite::Message {
        json!({ "t": "body", "id": id, "chunk": util::b64_encode(bytes), "end": true })
            .to_string()
            .into()
    }

    fn end_body(id: &str) -> tungstenite::Message {
        body_frame(id, b"")
    }

    /// Reads `res` plus the body frames for one request, checking the chunk cap and the closing
    /// `end` frame the way the real relay would.
    async fn read_response(ws: &mut RelayWs, id: &str, what: &str) -> (u16, Vec<Value>, Vec<u8>) {
        let res = next_frame(ws, what).await;
        assert_eq!(res["t"], "res");
        assert_eq!(res["id"], id);
        let status = res["status"].as_u64().unwrap() as u16;
        let mut body = Vec::new();
        loop {
            let frame = next_frame(ws, &format!("{what} body")).await;
            assert_eq!(frame["t"], "body");
            assert_eq!(frame["id"], id);
            let chunk = util::b64_decode(frame["chunk"].as_str().unwrap_or_default()).unwrap_or_default();
            assert!(chunk.len() <= CHUNK, "no chunk carries more than the cap");
            body.extend_from_slice(&chunk);
            if frame["end"].as_bool().unwrap_or(false) {
                break;
            }
        }
        (status, res["headers"].as_array().cloned().unwrap_or_default(), body)
    }

    // -- The tests ----------------------------------------------------------

    #[test]
    fn plaintext_ws_is_only_for_a_loopback_relay() {
        assert_eq!(http_base("wss://my.colonizer.dev").unwrap(), "https://my.colonizer.dev");
        assert_eq!(
            http_base("wss://relay.example.com:8443").unwrap(),
            "https://relay.example.com:8443"
        );
        for local in [
            "ws://127.0.0.1:7000",
            "ws://localhost:7000",
            "ws://LOCALHOST",
            "ws://[::1]:7000",
            "ws://127.8.9.10",
        ] {
            assert!(http_base(local).is_ok(), "{local} is loopback");
        }
        // Anything off this machine over plaintext would carry cookies, tokens and bodies in the clear.
        for remote in [
            "ws://my.colonizer.dev",
            "ws://relay.example.com:80",
            "ws://10.0.0.5:7000",
            "ws://[2001:db8::1]:7000",
            "ws://localhost.evil.example",
            "ws://127.0.0.1.evil.example",
            "ws://",
        ] {
            let refused = http_base(remote).expect_err(remote).to_string();
            assert!(refused.contains("wss://"), "{remote}: {refused}");
        }
        assert!(http_base("https://my.colonizer.dev").is_err());
    }

    #[tokio::test]
    async fn enabling_against_a_plaintext_remote_relay_is_refused() {
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay("ws://relay.example.com".into()).await;
        let Err(refused) = set_enabled(&app, true).await else {
            panic!("a plaintext relay off this machine was accepted");
        };
        assert!(refused.message().contains("wss://"), "{}", refused.message());
        assert!(!app.remote.enabled().await, "the switch stays off");
    }

    #[tokio::test]
    async fn a_tunnelled_request_round_trips() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let host = app.remote.saved().await.host.unwrap();
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(req_frame("1", "GET", "/api/remote", headers)).await.unwrap();
        let (status, head, body) = read_response(&mut ws, "1", "the tunnelled GET").await;
        assert_eq!(status, 200);
        assert!(
            head.iter()
                .any(|pair| pair[0] == "content-type" && pair[1].as_str().unwrap().contains("application/json"))
        );
        let view: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(view["enabled"], true);
        assert_eq!(view["host"], host.as_str());
        assert_eq!(view["connected"], true, "the supervisor knows its tunnel is live");
    }

    #[tokio::test]
    async fn cookies_leave_the_tunnel_secure() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(req_frame("c", "GET", "/api/demo", headers)).await.unwrap();
        let (status, head, _) = read_response(&mut ws, "c", "the cookie answer").await;
        assert_eq!(status, 201);
        let cookies: Vec<&str> = head
            .iter()
            .filter(|pair| pair[0] == "set-cookie")
            .map(|pair| pair[1].as_str().unwrap())
            .collect();
        assert_eq!(cookies, ["one=1; Path=/; HttpOnly; Secure", "two=2; Path=/; Secure"]);
        // The same handler on localhost keeps its cookies as they were: plain http has no Secure.
        assert_eq!(secure_cookie("a=1; secure"), "a=1; secure");
        assert_eq!(secure_cookie("a=1; Path=/"), "a=1; Path=/; Secure");
        assert_eq!(
            secure_cookie("secure=1"),
            "secure=1; Secure",
            "a cookie named secure is not the attribute"
        );
    }

    #[tokio::test]
    async fn the_tunnel_enforces_the_same_auth_as_the_lan() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let host = app.remote.saved().await.host.unwrap();
        let mut ws = next_tunnel(&mut tunnels).await;
        // No token: the same 401 as on the LAN.
        ws.send(req_frame("a", "GET", "/api/remote", json!([]))).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "a", "the unauthenticated request").await;
        assert_eq!(status, 401);
        // Cookie-authenticated writes keep the Origin fence, pinned to exactly https://<host>.
        // The refused ones never reach the handler, so the tunnel stays up under them.
        let cookie = format!("{}={}", crate::auth::COOKIE_NAME, app.remote.add_link("test").unwrap());
        for origin in [format!("http://{host}"), "https://other.example".into()] {
            let headers = json!([["Cookie", cookie], ["Origin", origin]]);
            ws.send(req_frame("b", "POST", "/api/remote/reset", headers)).await.unwrap();
            ws.send(end_body("b")).await.unwrap(); // a bodyless POST still ends its body
            let (status, _, _) = read_response(&mut ws, "b", "the cookie-authenticated write").await;
            assert_eq!(status, 403, "Origin {origin}");
        }
        // The one origin the fence admits: the identity is replaced, and the tunnel redials at
        // once under it — the relay verifies the fresh key in the new handshake.
        let was = app.remote.saved().await.install_id.unwrap();
        let headers = json!([["Cookie", cookie], ["Origin", format!("https://{host}")]]);
        ws.send(req_frame("b", "POST", "/api/remote/reset", headers)).await.unwrap();
        ws.send(end_body("b")).await.unwrap();
        // The reset tears its own tunnel down at once (the in-flight answer is lost with it) and
        // redials under the fresh identity — the relay verifies the new key in that handshake.
        let mut ws = next_tunnel(&mut tunnels).await;
        assert_ne!(
            app.remote.saved().await.install_id.unwrap(),
            was,
            "the admitted reset replaced the identity"
        );
        // A bearer token skips the fence, like on the LAN: any origin, and a no-op write passes.
        let headers = json!([
            ["Authorization", link_bearer(&app)],
            ["Origin", "http://evil.example"],
            ["Content-Type", "application/json"],
        ]);
        ws.send(req_frame("c", "PUT", "/api/remote", headers)).await.unwrap();
        ws.send(body_frame("c", br#"{"enabled":true}"#)).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "c", "the bearer write").await;
        assert_eq!(status, 200);
    }

    // -- The link credential (review finding R3) -----------------------------

    /// A request as the tunnel client would hand it to the router: marked, with the tunnel host.
    fn via_tunnel(app_host: &str, uri: &str, headers: &[(&str, String)]) -> Request {
        let mut builder = Request::builder().method(Method::GET).uri(uri).header(header::HOST, app_host);
        for (name, value) in headers {
            builder = builder.header(*name, value.as_str());
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(Tunnelled { host: app_host.into() });
        request
    }

    /// A router and app with remote access on under a fixed host, no relay needed.
    async fn linked_app() -> (Shared, Router, String, TempRoot) {
        let root = temp_root();
        let app = test_app(root.path());
        let host = "abcdefghijklmnopqrst.my.colonizer.dev".to_string();
        app.remote
            .persist(Saved {
                enabled: true,
                install_id: Some("abcdefghijklmnopqrst".into()),
                host: Some(host.clone()),
                require_github: None,
            })
            .await
            .unwrap();
        let router = remote_router(&app, idle_park());
        (app, router, host, root)
    }

    #[tokio::test]
    async fn the_install_token_is_refused_through_the_tunnel_and_the_link_credential_accepted() {
        let (app, router, host, _root) = linked_app().await;
        let install_cookie = format!("{}={}", crate::auth::COOKIE_NAME, app.api_token);
        for headers in [
            vec![("authorization", format!("Bearer {}", app.api_token))],
            vec![("cookie", install_cookie.clone())],
        ] {
            let res = router
                .clone()
                .oneshot(via_tunnel(&host, "/api/remote", &headers))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{headers:?}");
            let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
            assert!(String::from_utf8_lossy(&body).contains("Settings → Remote access"));
        }
        // The sign-in link's ?token= sets no cookie through the tunnel either.
        let res = router
            .clone()
            .oneshot(via_tunnel(&host, &format!("/?token={}", app.api_token), &[]))
            .await
            .unwrap();
        assert!(
            res.headers().get(header::SET_COOKIE).is_none(),
            "no install-token cookie through the link"
        );
        // A link credential works, by cookie or by header, with full owner reach (not a phone's).
        let token = app.remote.add_link("laptop").unwrap();
        let cookie = format!("{}={token}", crate::auth::COOKIE_NAME);
        for headers in [
            vec![("authorization", format!("Bearer {token}"))],
            vec![("cookie", cookie.clone())],
        ] {
            let res = router
                .clone()
                .oneshot(via_tunnel(&host, "/api/remote", &headers))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{headers:?}");
        }
        let mut write = via_tunnel(
            &host,
            "/api/remote",
            &[
                ("cookie", cookie.clone()),
                ("origin", format!("https://{host}")),
                ("content-type", "application/json".into()),
            ],
        );
        *write.method_mut() = Method::PUT;
        *write.body_mut() = Body::from(r#"{"enabled":true}"#);
        assert_eq!(router.clone().oneshot(write).await.unwrap().status(), StatusCode::OK);
        // A forged marker cannot be had: off the tunnel the link credential is worth nothing, Host
        // header or not, and the install token still works locally.
        for host_header in [host.as_str(), "127.0.0.1:7878"] {
            let request = Request::builder()
                .uri("/api/remote")
                .header(header::HOST, host_header)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("x-colonizer-tunnelled", "1")
                .body(Body::empty())
                .unwrap();
            let status = router.clone().oneshot(request).await.unwrap().status();
            assert!(
                matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
                "{host_header}: {status}"
            );
        }
        let (status, _) = local(&router, &app, Method::GET, "/api/remote", None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn revoking_a_link_device_ends_it_at_once_and_reset_rotates_them_all() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let token = app.remote.add_link("laptop").unwrap();
        let bearer = json!([["Authorization", format!("Bearer {token}")]]);
        // A live socket on the link credential.
        ws.send(
            json!({ "t": "ws_open", "id": "s", "path": "/api/stream", "headers": bearer })
                .to_string()
                .into(),
        )
        .await
        .unwrap();
        assert_eq!(next_frame(&mut ws, "the stream's first frame").await["t"], "ws_msg");
        let id = app.remote.links_view()[0]["id"].as_str().unwrap().to_string();
        let (status, _) = local(
            &remote_router(&app, idle_park()),
            &app,
            Method::DELETE,
            &format!("/api/remote/devices/{id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        // The socket closes now, on the live tunnel, and the credential answers 401 from here on.
        let close = loop {
            let frame = next_frame(&mut ws, "the revoked socket's close").await;
            if frame["t"] == "ws_close" {
                break frame;
            }
        };
        assert_eq!(close["id"], "s");
        ws.send(req_frame("r", "GET", "/api/remote", bearer.clone())).await.unwrap();
        assert_eq!(read_response(&mut ws, "r", "the revoked credential").await.0, 401);

        // Reset link rotates every link credential: a second device's socket and credential end.
        let second = app.remote.add_link("tablet").unwrap();
        let second_bearer = json!([["Authorization", format!("Bearer {second}")]]);
        ws.send(req_frame("ok", "GET", "/api/remote", second_bearer.clone()))
            .await
            .unwrap();
        assert_eq!(read_response(&mut ws, "ok", "the second device").await.0, 200);
        let key = link_key(app.remote.links_view()[0]["id"].as_str().unwrap());
        let (status, _) = local(
            &remote_router(&app, idle_park()),
            &app,
            Method::POST,
            "/api/remote/reset",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            crate::auth::Revocation::watch(&key).is_fired(),
            "its sockets and streams were told"
        );
        assert!(app.remote.links_view().is_empty());
        let mut ws = next_tunnel(&mut tunnels).await;
        ws.send(req_frame("old", "GET", "/api/remote", second_bearer)).await.unwrap();
        assert_eq!(read_response(&mut ws, "old", "the rotated credential").await.0, 401);
        assert!(std::fs::read_to_string(app.remote.dir.join(LINKS_FILE)).unwrap().trim() == "[]");
    }

    #[tokio::test]
    async fn a_link_invite_opens_only_through_the_tunnel_and_is_confirmed_locally() {
        let (app, router, host, _root) = linked_app().await;
        let (status, invite) = local(&router, &app, Method::POST, "/api/remote/devices/invites", None).await;
        assert_eq!(status, StatusCode::OK, "{invite}");
        let url = invite["url"].as_str().unwrap();
        let prefix = format!("https://{host}/?pair=");
        assert!(url.starts_with(&prefix), "{url}");
        let code = &url[prefix.len()..];
        assert!(!url.contains(&app.api_token));
        // Opened anywhere but the link, the invite is spent and opens nothing.
        let (status, again) = local(&router, &app, Method::POST, "/api/remote/devices/invites", None).await;
        assert_eq!(status, StatusCode::OK);
        let lan_code = again["url"].as_str().unwrap()[prefix.len()..].to_string();
        let lan = Request::builder()
            .uri(format!("/?pair={lan_code}"))
            .header(header::HOST, "127.0.0.1:7878")
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(lan).await.unwrap();
        assert!(res.headers().get(header::SET_COOKIE).is_none(), "no pairing off the link");
        // Through the link it opens a pairing: a code on the page, a device secret as a cookie.
        let res = router
            .clone()
            .oneshot(via_tunnel(&host, &format!("/?pair={code}"), &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let pair = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let page = String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/devices", None).await;
        assert_eq!(view["pending"].as_array().unwrap().len(), 1, "{view}");
        // The phone pane never sees it, and the phone confirm route cannot approve it.
        let (_, phones) = local(&router, &app, Method::GET, "/api/phone", None).await;
        assert!(
            phones
                .get("pending")
                .is_none_or(|p| p.as_array().is_none_or(|p| p.is_empty()))
        );
        let digits: String = page
            .split("aria-label=\"Confirmation code\">")
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .expect("the page shows the code")
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        assert_eq!(digits.len(), 6, "{page}");
        assert!(page.contains("Remote access"), "the page sends the owner to the right pane");
        // A confirm through the link is refused; the local one approves.
        let mut through = via_tunnel(
            &host,
            "/api/remote/devices/confirm",
            &[
                ("authorization", link_bearer(&app)),
                ("content-type", "application/json".into()),
            ],
        );
        *through.method_mut() = Method::POST;
        *through.body_mut() = Body::from(json!({ "code": digits }).to_string());
        assert_eq!(router.clone().oneshot(through).await.unwrap().status(), StatusCode::FORBIDDEN);
        let (status, answer) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/devices/confirm",
            Some(json!({ "code": digits })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        // The claim, through the link, sets the link credential as the browser's cookie.
        let mut claim = via_tunnel(&host, "/api/phone/claim", &[("cookie", pair)]);
        *claim.method_mut() = Method::POST;
        let res = router.clone().oneshot(claim).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cookie = res
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .find(|v| v.starts_with(&format!("{}=clk_", crate::auth::COOKIE_NAME)))
            .expect("a link credential cookie");
        let cookie = cookie.split(';').next().unwrap().to_string();
        let res = router
            .clone()
            .oneshot(via_tunnel(&host, "/api/remote", &[("cookie", cookie)]))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "the handed-over credential signs the browser in"
        );
        assert_eq!(app.remote.links_view().len(), 2, "the test's own credential and the new one");
    }

    #[tokio::test]
    async fn a_tunnelled_websocket_reaches_a_real_upgrade() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(
            json!({ "t": "ws_open", "id": "w", "path": "/api/stream", "headers": headers })
                .to_string()
                .into(),
        )
        .await
        .unwrap();
        let msg = next_frame(&mut ws, "the first stream frame").await;
        assert_eq!(msg["t"], "ws_msg");
        assert_eq!(msg["binary"], false);
        assert_eq!(msg["id"], "w");
        let frame: Value = serde_json::from_str(msg["data"].as_str().unwrap()).unwrap();
        assert_eq!(frame["type"], "sessions", "the cockpit stream's first frame");
        // Closing from the relay side closes the inner websocket and is reported back. The
        // cockpit's stream may push a periodic frame in between; only the close is the answer.
        ws.send(json!({ "t": "ws_close", "id": "w", "code": 1000 }).to_string().into())
            .await
            .unwrap();
        let close = loop {
            let frame = next_frame(&mut ws, "the close back").await;
            if frame["t"] == "ws_close" {
                break frame;
            }
            assert_eq!(frame["t"], "ws_msg", "only stream frames ride before the close");
        };
        assert_eq!(close["id"], "w");
    }

    #[tokio::test]
    async fn a_dropped_tunnel_is_redialed() {
        let (_app, _router, mut tunnels, _root) = enabled_app().await;
        drop(next_tunnel(&mut tunnels).await);
        // The supervisor redials after about a second: the backoff floor plus jitter.
        next_tunnel(&mut tunnels).await;
    }

    /// The relay-side close of a live tunnel with `code`, the way the real relay hangs up.
    async fn close_with(ws: &mut RelayWs, code: u16) {
        ws.close(Some(CloseFrame {
            code: code.into(),
            reason: Default::default(),
        }))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_replaced_tunnel_parks_until_the_operator_acts() {
        let (app, router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        // The relay gives the link to a newer tunnel: the code it really sends is 4000 'replaced'
        // (tunnel.js), the pinned one 4409. Either must park the supervisor, or two motherships
        // on one install would keep replacing each other forever.
        close_with(&mut ws, 4000).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(tunnels.try_recv().is_err(), "nothing redials a replaced tunnel");
        let view = app.remote.view().await;
        assert_eq!(view["enabled"], true);
        assert_eq!(view["connected"], false);
        assert_eq!(view["replaced"], true, "the status names the takeover");
        // The way out: a reset signals the supervisor, which dials at once under the fresh
        // identity, and the reset itself clears the status.
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/remote/reset")
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
        let mut ws = next_tunnel(&mut tunnels).await;
        assert_eq!(app.remote.view().await["replaced"], false, "the reset cleared the status");
        // Taken over again, the other exit: a disable ends the parked state, and the re-enable
        // dials again and clears the status.
        close_with(&mut ws, 4000).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(app.remote.view().await["replaced"], true, "parked again");
        switch(&app, &router, false).await;
        switch(&app, &router, true).await;
        let _ws = next_tunnel(&mut tunnels).await;
        assert_eq!(app.remote.view().await["replaced"], false, "the re-enable cleared the status");
    }

    #[tokio::test]
    async fn a_transient_close_still_redials() {
        let (_app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        // 1012 (Service Restart) is a transient close, unlike 4000: the supervisor reconnects
        // with the usual backoff.
        close_with(&mut ws, 1012).await;
        next_tunnel(&mut tunnels).await;
    }

    // -- The real relay, end to end -----------------------------------------
    //
    // `services/relay/scripts/local-relay.mjs` is the deployed relay for real: the real worker,
    // a real InstallTunnel DO over a real WebSocket, a real registration. Only the D1 and the DO
    // namespace are in-memory fakes, and the owner sign-in is bypassed with a request header.

    /// Spawns the harness and returns it with its port; it prints one line, `listening <port>`,
    /// to stdout, and exits when stdin closes or the returned `Child` is dropped. `None` skips:
    /// `node` is not on PATH, which only CI counts as a failure, or the repository is not at hand
    /// (see below), which skips even under CI.
    async fn spawn_local_relay() -> Option<(tokio::process::Child, u16)> {
        if tokio::process::Command::new("node").arg("--version").output().await.is_err() {
            if std::env::var_os("CI").is_some() {
                panic!("CI is set but node is not on PATH: the local-relay e2e cannot run");
            }
            eprintln!("skipping the local-relay e2e: node is not on PATH");
            return None;
        }
        // The relay harness lives outside this crate (services/relay). The repository root comes
        // from COLONIZER_REPO_ROOT, which the workspace .cargo/config.toml sets; the published
        // crate ships no such config, so there the variable is unset and this skips.
        let Some(root) = std::env::var_os("COLONIZER_REPO_ROOT") else {
            eprintln!("skipping the local-relay e2e: COLONIZER_REPO_ROOT is unset (run it from the repository)");
            return None;
        };
        let mut relay = Command::new("node")
            .arg("scripts/local-relay.mjs")
            .current_dir(Path::new(&root).join("services/relay"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("node is on PATH but the local-relay harness would not spawn");
        let mut stdout = tokio::io::BufReader::new(relay.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), stdout.read_line(&mut line))
            .await
            .expect("the local-relay harness never announced a port")
            .expect("reading the local-relay harness's stdout failed");
        let port = line
            .trim()
            .strip_prefix("listening ")
            .expect("the harness's one stdout line was not `listening <port>`")
            .parse()
            .expect("the harness's announced port was not a number");
        Some((relay, port))
    }

    /// Waits, bounded, until `what` holds on the app's remote view.
    async fn until(app: &Shared, what: impl Fn(&Value) -> bool, msg: &'static str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !what(&app.remote.view().await) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect(msg);
    }

    /// The answer a tunnelled request must carry back through the real relay unharmed: a 201 with
    /// a content-type, two cookies, and a body.
    async fn demo() -> Response {
        Response::builder()
            .status(StatusCode::CREATED)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::SET_COOKIE, "one=1; Path=/; HttpOnly")
            .header(header::SET_COOKIE, "two=2; Path=/")
            .body(Body::from(r#"{"ok":true}"#.as_bytes().to_vec()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_request_rides_the_real_relay_end_to_end() {
        let Some((_relay, port)) = spawn_local_relay().await else {
            return;
        };
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(format!("ws://127.0.0.1:{port}")).await;
        tokio::spawn(run(app.clone(), Router::new().route("/api/demo", get(demo))));
        // The real registration (the worker answers 201 from its D1) and the real dial; the DO's
        // first ping, 5 s in, is the first frame, and that is what flips the status to connected.
        set_enabled(&app, true).await.unwrap();
        until(
            &app,
            |v| v["connected"] == true,
            "the tunnel never connected to the real relay",
        )
        .await;
        let (install, host) = {
            let saved = app.remote.saved().await;
            (saved.install_id.unwrap(), saved.host.unwrap())
        };
        // A plain HTTP/1.1 request, the shape a signed-in owner's browser produces once the
        // harness's header stands in for the sign-in.
        let mut io = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        io.write_all(
            format!("GET /api/demo HTTP/1.1\r\nhost: {host}\r\nx-local-relay-install: {install}\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut raw = Vec::new();
        // Bounded, because before the relay's proxy fix this hung instead of answering.
        tokio::time::timeout(Duration::from_secs(10), io.read_to_end(&mut raw))
            .await
            .expect("the real relay never answered the tunnelled request")
            .unwrap();
        let raw = String::from_utf8(raw).unwrap();
        assert!(raw.starts_with("HTTP/1.1 201"), "the handler's status: {raw:?}");
        assert!(raw.to_ascii_lowercase().contains("content-type: application/json"), "{raw:?}");
        assert_eq!(
            raw.lines()
                .filter(|line| line.to_ascii_lowercase().starts_with("set-cookie: "))
                .count(),
            2,
            "both cookies as their own header lines: {raw:?}"
        );
        assert!(raw.trim_end().ends_with(r#"{"ok":true}"#), "the body intact: {raw:?}");
    }

    #[tokio::test]
    async fn a_real_replacement_parks_the_losing_client() {
        let Some((_relay, port)) = spawn_local_relay().await else {
            return;
        };
        let relay_url = format!("ws://127.0.0.1:{port}");
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(relay_url.clone()).await;
        tokio::spawn(run(app.clone(), Router::new()));
        set_enabled(&app, true).await.unwrap();
        until(&app, |v| v["connected"] == true, "the first tunnel never connected").await;
        // A second mothership that found the first's config dir: the same key and install id, the
        // accident the replaced close exists for. Same install, so enabling dials without
        // registering, and the relay hands the link over.
        let (install, host) = {
            let saved = app.remote.saved().await;
            (saved.install_id.unwrap(), saved.host.unwrap())
        };
        let rival_root = temp_root();
        let rival = test_app(rival_root.path());
        rival.remote.set_relay(relay_url).await;
        std::fs::create_dir_all(&rival.remote.dir).unwrap();
        util::write_private(
            &rival.remote.dir.join(KEY_FILE),
            &std::fs::read(app.remote.dir.join(KEY_FILE)).unwrap(),
        )
        .unwrap();
        rival
            .remote
            .persist(Saved {
                enabled: false,
                install_id: Some(install),
                host: Some(host),
                require_github: None,
            })
            .await
            .unwrap();
        tokio::spawn(run(rival.clone(), Router::new()));
        set_enabled(&rival, true).await.unwrap();
        // The relay closed the first tunnel with 4000 'replaced'; the winner takes the link (its
        // status flips when the DO's first ping arrives, up to 5 s later).
        until(
            &app,
            |v| v["replaced"] == true,
            "the replaced client never reported the takeover",
        )
        .await;
        until(&rival, |v| v["connected"] == true, "the winning client never connected").await;
        // The loser stays parked: a redial on the 1 s backoff would have taken the link back.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let first = app.remote.view().await;
        assert_eq!(first["replaced"], true, "still parked");
        assert_eq!(first["connected"], false);
        let second = rival.remote.view().await;
        assert_eq!(second["connected"], true, "the winner holds the link");
        assert_eq!(second["replaced"], false);
    }

    // -- Pairing (#599) ------------------------------------------------------

    /// A local cockpit call through the real guard: loopback Host, the owner's bearer token.
    async fn local(router: &Router, app: &Shared, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token));
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let res = router.clone().oneshot(request).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// One plain HTTP/1.1 GET with the local relay under `host`: status, headers, body.
    async fn relay_http(port: u16, path: &str, host: &str, cookie: Option<&str>) -> (u16, Vec<(String, String)>, String) {
        relay_request(port, "GET", path, host, cookie).await
    }

    /// [`relay_http`] with any method and no body, sent from the link's own https origin.
    async fn relay_request(
        port: u16,
        method: &str,
        path: &str,
        host: &str,
        cookie: Option<&str>,
    ) -> (u16, Vec<(String, String)>, String) {
        let mut io = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let cookie = cookie.map(|c| format!("cookie: {c}\r\n")).unwrap_or_default();
        let body = if method == "GET" { "" } else { "content-length: 0\r\n" };
        io.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nhost: {host}\r\norigin: https://{host}\r\n{cookie}{body}connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), io.read_to_end(&mut raw))
            .await
            .expect("the local relay answered in time")
            .unwrap();
        let raw = String::from_utf8(raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        // A streamed answer arrives chunked: undo the framing.
        let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            let mut out = String::new();
            let mut rest = body;
            while let Some((size, after)) = rest.split_once("\r\n") {
                let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                if size == 0 {
                    break;
                }
                out.push_str(&after[..size]);
                rest = &after[size + 2..];
            }
            out
        } else {
            body.to_string()
        };
        let body = body.as_str();
        let mut lines = head.lines();
        let status = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        (status, headers, body.to_string())
    }

    /// The relay's owner sign-in on `host` as GitHub account `account` (`<id>:<login>`, which the
    /// harness's GitHub stub answers): /_auth, then the callback with the state and the OAuth
    /// cookie it set. Answers the callback's status and body — 200 with the pairing page for an
    /// unowned install, 302 into the cockpit for its owner, 403 for anyone else.
    async fn sign_in(port: u16, host: &str, account: &str) -> (u16, String) {
        let (status, headers, _) = relay_http(port, "/_auth?next=/", host, None).await;
        assert_eq!(status, 302, "/_auth starts the GitHub sign-in");
        let location = &headers.iter().find(|(k, _)| k == "location").unwrap().1;
        let state = location
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();
        let cookie = headers
            .iter()
            .find(|(k, v)| k == "set-cookie" && v.starts_with("__Host-colonizer_oauth="))
            .map(|(_, v)| v.split(';').next().unwrap().to_string())
            .unwrap();
        let (status, _, body) = relay_http(
            port,
            &format!("/_auth/callback?state={state}&code={account}"),
            host,
            Some(&cookie),
        )
        .await;
        (status, body)
    }

    /// The six digits on the relay's pairing page.
    fn code_on(page: &str) -> String {
        let code = page
            .split("class=\"pairing-code\"")
            .nth(1)
            .and_then(|rest| rest.split('>').nth(1))
            .and_then(|rest| rest.split('<').next())
            .expect("the pairing page shows a code")
            .to_string();
        assert!(is_pairing_code(&code), "a six-digit code: {code:?}");
        code
    }

    /// A code that is well formed but not `code`.
    fn other_code(code: &str) -> String {
        if code == "000000" { "000001".into() } else { "000000".into() }
    }

    #[tokio::test]
    async fn pairing_binds_an_owner_through_the_real_relay() {
        let Some((_relay, port)) = spawn_local_relay().await else {
            return;
        };
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(format!("ws://127.0.0.1:{port}")).await;
        let router = remote_router(&app, idle_park());
        // The real registration; no tunnel is needed for pairing, which is all signed HTTP.
        set_enabled(&app, true).await.unwrap();
        let host = app.remote.saved().await.host.unwrap();

        let (status, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view, json!({"owner": null, "pending": [], "require_github": false}));

        // The first sign-in parks a code; the local cockpit sees it, with the account behind it.
        let (status, page) = sign_in(port, &host, "4242:alice").await;
        assert_eq!(status, 200, "an unowned install shows the pairing page: {page}");
        let code = code_on(&page);
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(view["owner"], Value::Null);
        assert_eq!(view["pending"][0]["code"], code.as_str());
        assert_eq!(view["pending"][0]["github_login"], "alice");

        // A wrong code is refused: malformed here (400), unknown at the relay (404).
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({"code": "12345"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let wrong = other_code(&code);
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": wrong })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The right code binds the owner, once.
        let (status, answer) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": code })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer, json!({"owner": {"github_login": "alice"}}));
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": code })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "a code is single-use");
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(
            view,
            json!({"owner": {"github_login": "alice"}, "pending": [], "require_github": false})
        );

        // The relay now lets the owner straight through and refuses anyone else.
        let (status, _) = sign_in(port, &host, "4242:alice").await;
        assert_eq!(status, 302, "the owner's sign-in goes straight through");
        let (status, _) = sign_in(port, &host, "5555:mallory").await;
        assert_eq!(status, 403, "another GitHub account is refused");

        // Unbind clears the binding: the next sign-in pairs anew.
        let (status, _) = local(&router, &app, Method::DELETE, "/api/remote/owner", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(view, json!({"owner": null, "pending": [], "require_github": false}));
        let (status, page) = sign_in(port, &host, "4242:alice").await;
        assert_eq!(status, 200, "unbound, the old owner pairs again: {page}");

        // A code expires: once the relay has moved it into the past, confirming it is a 404, and
        // the cockpit no longer lists it.
        let expiring = code_on(&page);
        let mut io = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let install = app.remote.saved().await.install_id.unwrap();
        io.write_all(
            format!("POST /_local/expire-pairings?install={install} HTTP/1.1\r\nhost: x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        io.read_to_end(&mut Vec::new()).await.unwrap();
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": expiring })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "an expired code is refused");
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(view["pending"], json!([]));

        // Reject drops a pending code, so it can no longer be confirmed.
        let (_, page) = sign_in(port, &host, "5555:mallory").await;
        let rejected = code_on(&page);
        let (status, answer) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/reject",
            Some(json!({ "code": rejected })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["github_login"], "mallory");
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": rejected })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Reset link retires the old install outright (review finding R2): its owner is gone with
        // it, and its host is unknown to the relay.
        let (_, page) = sign_in(port, &host, "4242:alice").await;
        let code = code_on(&page);
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": code })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = local(&router, &app, Method::POST, "/api/remote/reset", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, _) = relay_http(port, "/_auth?next=/", &host, None).await;
        assert_eq!(status, 404, "after a reset the relay no longer knows the old link");
        let (_, view) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(
            view,
            json!({"owner": null, "pending": [], "require_github": false}),
            "the new link starts unowned"
        );

        // Each decision is in the activity log, as its own kind.
        let kinds = activity_kinds(&app).await;
        assert_eq!(kinds.iter().filter(|k| *k == "remote.pair").count(), 2);
        assert_eq!(kinds.iter().filter(|k| *k == "remote.pair_reject").count(), 1);
        assert_eq!(kinds.iter().filter(|k| *k == "remote.unpair").count(), 1);
    }

    /// The owner's relay session cookie (`name=value`) for an install already bound to `account`.
    async fn owner_session(port: u16, host: &str, account: &str) -> String {
        let (status, headers, _) = relay_http(port, "/_auth?next=/", host, None).await;
        assert_eq!(status, 302);
        let location = &headers.iter().find(|(k, _)| k == "location").unwrap().1;
        let state = location
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();
        let oauth = headers
            .iter()
            .find(|(k, v)| k == "set-cookie" && v.starts_with("__Host-colonizer_oauth="))
            .map(|(_, v)| v.split(';').next().unwrap().to_string())
            .unwrap();
        let (status, headers, _) = relay_http(
            port,
            &format!("/_auth/callback?state={state}&code={account}"),
            host,
            Some(&oauth),
        )
        .await;
        assert_eq!(status, 302, "the bound owner's sign-in mints a session");
        headers
            .iter()
            .find(|(k, v)| k == "set-cookie" && v.starts_with("__Host-colonizer_session="))
            .map(|(_, v)| v.split(';').next().unwrap().to_string())
            .expect("a session cookie")
    }

    /// A browser websocket through the local relay to `path` on `host`, with `cookie` and the
    /// install's own https origin, as the cockpit page would open it.
    async fn relay_socket(
        port: u16,
        host: &str,
        path: &str,
        cookie: &str,
    ) -> Result<WebSocketStream<tokio::net::TcpStream>, tungstenite::Error> {
        let io = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut request = format!("ws://{host}{path}").into_client_request().unwrap();
        request.headers_mut().insert(header::COOKIE, cookie.parse().unwrap());
        request
            .headers_mut()
            .insert(header::ORIGIN, format!("https://{host}").parse().unwrap());
        tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::client_async(request, io))
            .await
            .expect("the relay answered the upgrade in time")
            .map(|(ws, _)| ws)
    }

    /// "Sign in on another device", through the local relay: an invite minted here, opened on the
    /// link with `cookie` (or none), its six digits confirmed here, and the claim — the answer is the
    /// link credential's `name=value` the claim set on the link's origin.
    async fn hand_over(port: u16, router: &Router, app: &Shared, host: &str, cookie: Option<&str>) -> String {
        let (status, invite) = local(router, app, Method::POST, "/api/remote/devices/invites", None).await;
        assert_eq!(status, StatusCode::OK, "{invite}");
        let url = invite["url"].as_str().unwrap();
        let path = url
            .strip_prefix(&format!("https://{host}"))
            .expect("the invite opens on the link");
        let (status, headers, page) = relay_http(port, path, host, cookie).await;
        assert_eq!(status, 200, "the invite opens the pairing page: {page}");
        let pair = headers
            .iter()
            .find(|(k, v)| k == "set-cookie" && v.starts_with(crate::phone::PAIR_COOKIE))
            .map(|(_, v)| v.clone())
            .expect("the device secret cookie");
        assert!(pair.ends_with("; Secure"), "{pair}");
        let digits: String = page
            .split("aria-label=\"Confirmation code\">")
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .unwrap()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        // The six digits are confirmed here, on this machine: never through the link.
        let (status, answer) = local(
            router,
            app,
            Method::POST,
            "/api/remote/devices/confirm",
            Some(json!({ "code": digits })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let pair = pair.split(';').next().unwrap();
        let claim_cookie = cookie.map_or_else(|| pair.to_string(), |c| format!("{c}; {pair}"));
        let (status, headers, body) = relay_request(port, "POST", "/api/phone/claim", host, Some(&claim_cookie)).await;
        assert_eq!(status, 200, "{body}");
        let link = headers
            .iter()
            .find(|(k, v)| k == "set-cookie" && v.starts_with(&format!("{}=clk_", crate::auth::COOKIE_NAME)))
            .map(|(_, v)| v.clone())
            .expect("the link credential cookie");
        assert!(link.contains("HttpOnly") && link.ends_with("; Secure"), "{link}");
        link.split(';').next().unwrap().to_string()
    }

    /// The relay's own "Pair this device" page (#1086): what a browser with no invite and no
    /// credential, or a rejected one, gets — never GitHub, never the cockpit.
    fn is_pair_page(status: u16, headers: &[(String, String)], body: &str) -> bool {
        status == 401 && body.contains("Pair this device") && !headers.iter().any(|(k, _)| k == "location")
    }

    /// Whether the relay cleared the cockpit cookie (a dead credential) on this answer.
    fn clears_token(headers: &[(String, String)]) -> bool {
        headers.iter().any(|(k, v)| {
            k == "set-cookie" && v.starts_with(&format!("{}=;", crate::auth::COOKIE_NAME)) && v.contains("Max-Age=0")
        })
    }

    /// Waits, bounded, for a held socket to end: any close or error, but no more text frames.
    async fn ends(
        socket: &mut WebSocketStream<tokio::net::TcpStream>,
    ) -> Option<Result<tungstenite::Message, tungstenite::Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match socket.next().await {
                    Some(Ok(tungstenite::Message::Text(_) | tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_))) => {}
                    other => return other,
                }
            }
        })
        .await
        .expect("the held socket closed")
    }

    /// The whole link, end to end through the real relay (worker, DO, D1 schema, tunnel) and the
    /// real tunnel client, with only GitHub stubbed: register, tunnel up, pair a fresh browser with
    /// the pair code alone (#1086) and load the cockpit and its websocket through the relay, see a
    /// rejected and a revoked credential land on the pair page, then switch the GitHub gate on and
    /// run the owner flow of #534 behind it, unpair and reset.
    #[tokio::test]
    async fn the_whole_link_round_trips_through_the_real_relay() {
        let Some((_relay, port)) = spawn_local_relay().await else {
            return;
        };
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(format!("ws://127.0.0.1:{port}")).await;
        let router = remote_router(&app, idle_park());
        tokio::spawn(run(app.clone(), router.clone()));

        // 1. Register and bring the tunnel up, through the cockpit's own switch. A new install pairs
        //    with the pair code alone: no GitHub gate at the relay.
        let (status, view) = local(&router, &app, Method::PUT, "/api/remote", Some(json!({"enabled": true}))).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["require_github"], false);
        until(&app, |v| v["connected"] == true, "the tunnel never came up").await;
        let host = app.remote.saved().await.host.unwrap();

        // 2. No invite, no credential: the relay's pair page, never GitHub and never the cockpit; an
        //    API call and a websocket are refused there too.
        let (status, headers, body) = relay_http(port, "/", &host, None).await;
        assert!(is_pair_page(status, &headers, &body), "{status} {body}");
        let (status, _, body) = relay_http(port, "/api/remote", &host, None).await;
        assert_eq!(status, 401, "{body}");
        assert!(body.contains("not paired"), "{body}");
        assert!(relay_socket(port, &host, "/api/stream", "a=b").await.is_err());
        // Not even this machine's install token gets past the relay now (R3): it is no credential.
        let install = format!("{}={}", crate::auth::COOKIE_NAME, app.api_token);
        let (status, headers, body) = relay_http(port, "/", &host, Some(&install)).await;
        assert!(is_pair_page(status, &headers, &body), "{status} {body}");

        // 3. A fresh browser pairs with the invite and the six digits alone — no GitHub session.
        let link = hand_over(port, &router, &app, &host, None).await;
        let link_token = link.split_once('=').unwrap().1.to_string();

        // 4. The cockpit through the relay on that link credential alone: a GET, the cookies it sets
        //    (host-only and Secure, R4), and its websocket.
        let (status, headers, body) = relay_http(port, "/api/remote", &host, Some(&link)).await;
        assert_eq!(status, 200, "{body}");
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "content-type" && v.contains("application/json"))
        );
        assert!(!headers.iter().any(|(k, _)| k == CREDENTIAL_VERDICT));
        let seen: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(seen["host"], host.as_str());
        assert_eq!(seen["connected"], true, "the cockpit answered over its own live tunnel");
        let (status, headers, _) = relay_http(port, "/api/demo", &host, Some(&link)).await;
        assert_eq!(status, 201);
        let cookies: Vec<&str> = headers
            .iter()
            .filter(|(k, _)| k == "set-cookie")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(cookies, ["one=1; Path=/; HttpOnly; Secure", "two=2; Path=/; Secure"]);
        let mut socket = relay_socket(port, &host, "/api/stream", &link)
            .await
            .expect("the upgrade went through");
        let first = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("the stream's first frame arrived")
            .unwrap()
            .unwrap();
        let tungstenite::Message::Text(first) = first else {
            panic!("expected a text frame, got {first:?}")
        };
        let first: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["type"], "sessions", "the cockpit stream's first frame");
        socket.close(None).await.unwrap();
        let mut held = relay_socket(port, &host, "/api/stream", &link).await.expect("a held socket");

        // 5. A credential of the right shape that the cockpit does not know: forwarded, rejected, and
        //    the browser lands on the pair page with the dead cookie cleared. A websocket on it is
        //    closed 4401, which the relay counts the same way.
        let forged = format!("{}=clk_{}", crate::auth::COOKIE_NAME, "0".repeat(64));
        let (status, headers, body) = relay_http(port, "/", &host, Some(&forged)).await;
        assert!(is_pair_page(status, &headers, &body), "{status} {body}");
        assert!(clears_token(&headers), "{headers:?}");
        assert!(!headers.iter().any(|(k, _)| k == CREDENTIAL_VERDICT));
        let mut rejected = relay_socket(port, &host, "/api/stream", &forged)
            .await
            .expect("the relay accepts the upgrade before the cockpit decides");
        match ends(&mut rejected).await {
            Some(Ok(tungstenite::Message::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 4401),
            other => panic!("expected a 4401 close, got {other:?}"),
        }

        // 6. Revoking the device signs it out at once: the held socket ends, and its very next page
        //    load is the pair page.
        let (_, devices) = local(&router, &app, Method::GET, "/api/remote/devices", None).await;
        let id = devices["devices"][0]["id"].as_str().unwrap().to_string();
        let (status, _) = local(&router, &app, Method::DELETE, &format!("/api/remote/devices/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let ended = ends(&mut held).await;
        assert!(!matches!(ended, Some(Ok(tungstenite::Message::Text(_)))), "{ended:?}");
        let (status, headers, body) = relay_http(port, "/", &host, Some(&link)).await;
        assert!(
            is_pair_page(status, &headers, &body),
            "a revoked clk_ gets the pair page: {status} {body}"
        );
        assert!(clears_token(&headers));
        assert!(app.remote.authenticate_link(&link_token).is_none());

        // A PUT of the value already set still reaches the relay (it puts the two back in step if
        // they ever drifted) and records nothing.
        let (status, view) = local(
            &router,
            &app,
            Method::PUT,
            "/api/remote/require-github",
            Some(json!({"require_github": false})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["require_github"], false);
        let (_, pairing) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(pairing["require_github"], false);

        // 7. The GitHub gate, switched on here (local only): the relay sends every browser to sign in
        //    first, invites and link credentials included, and the owner flow of #534 works as before.
        let (status, view) = local(
            &router,
            &app,
            Method::PUT,
            "/api/remote/require-github",
            Some(json!({"require_github": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["require_github"], true);
        let (status, headers, _) = relay_http(port, "/api/remote", &host, None).await;
        assert_eq!(status, 302);
        assert!(headers.iter().any(|(k, v)| k == "location" && v.starts_with("/_auth")));
        let (_, pairing) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(pairing["require_github"], true, "the relay holds the gate: {pairing}");
        let (status, page) = sign_in(port, &host, "4242:alice").await;
        assert_eq!(status, 200, "{page}");
        let code = code_on(&page);
        let (status, answer) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({ "code": code })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let session = owner_session(port, &host, "4242:alice").await;
        // Behind the gate the cockpit's own check still applies: neither the relay session alone
        // nor this machine's install token gets in (R3).
        let (status, _, body) = relay_http(port, "/api/remote", &host, Some(&session)).await;
        assert_eq!(status, 401, "the relay's sign-in alone is not the cockpit's: {body}");
        let with_install = format!("{session}; {install}");
        let (status, _, body) = relay_http(port, "/api/remote", &host, Some(&with_install)).await;
        assert_eq!(status, 401, "the install token is refused through the link: {body}");
        let link = hand_over(port, &router, &app, &host, Some(&session)).await;
        let both = format!("{session}; {link}");
        let (status, _, body) = relay_http(port, "/api/remote", &host, Some(&both)).await;
        assert_eq!(status, 200, "{body}");
        // With the gate on, the link credential alone opens nothing.
        let (status, _, _) = relay_http(port, "/api/remote", &host, Some(&link)).await;
        assert_eq!(status, 302);
        assert!(relay_socket(port, &host, "/api/stream", &link).await.is_err());
        let mut held = relay_socket(port, &host, "/api/stream", &both).await.expect("a held socket");

        // 8. Unpair: the owner's session dies on its next request, HTTP and websocket alike.
        let (status, _) = local(&router, &app, Method::DELETE, "/api/remote/owner", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = relay_http(port, "/api/remote", &host, Some(&both)).await;
        assert_eq!(status, 302, "back to sign-in");
        assert!(relay_socket(port, &host, "/api/stream", &both).await.is_err());

        // 9. Reset: the old link is retired at the relay (R2), the link credential is rotated (R3) —
        //    the held socket closes — and a new link comes up, keeping the gate it had.
        let (status, _) = local(&router, &app, Method::POST, "/api/remote/reset", None).await;
        assert_eq!(status, StatusCode::OK);
        let ended = ends(&mut held).await;
        assert!(!matches!(ended, Some(Ok(tungstenite::Message::Text(_)))), "{ended:?}");
        let link_token = link.split_once('=').unwrap().1;
        assert!(
            app.remote.authenticate_link(link_token).is_none(),
            "the old credential is dead"
        );
        let (status, _, _) = relay_http(port, "/", &host, None).await;
        assert_eq!(status, 404, "the old host is unknown");
        until(
            &app,
            |v| v["connected"] == true && v["host"] != host.as_str(),
            "the new link never came up",
        )
        .await;
        let new_host = app.remote.saved().await.host.unwrap();
        let (status, headers, _) = relay_http(port, "/", &new_host, None).await;
        assert_eq!(status, 302, "the new install was registered with the GitHub gate on");
        assert!(headers.iter().any(|(k, v)| k == "location" && v.starts_with("/_auth")));
        let kinds = activity_kinds(&app).await;
        assert_eq!(kinds.iter().filter(|k| *k == "remote.require_github").count(), 1);
    }

    /// A signed `GET …/pairing` for `install_id` with `key`, straight at the relay: its status.
    async fn relay_knows(relay: &str, install_id: &str, key: &Ed25519KeyPair) -> StatusCode {
        signed_call(relay, install_id, key, Method::GET, "/pairing", None)
            .await
            .expect("the local relay answers")
            .0
    }

    #[tokio::test]
    async fn a_reset_retires_the_old_install_at_the_real_relay() {
        let Some((_relay, port)) = spawn_local_relay().await else {
            return;
        };
        let relay = format!("ws://127.0.0.1:{port}");
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(relay.clone()).await;
        let router = remote_router(&app, idle_park());
        tokio::spawn(run(app.clone(), router.clone()));
        set_enabled(&app, true).await.unwrap();
        until(&app, |v| v["connected"] == true, "the tunnel never connected").await;
        // What a thief would have copied out of <config>/remote/ before the owner reset the link.
        let old = app.remote.saved().await.install_id.unwrap();
        let leaked = read_key(&app.remote.dir).unwrap();
        assert_eq!(relay_knows(&relay, &old, &leaked).await, StatusCode::OK);

        let (status, view) = local(&router, &app, Method::POST, "/api/remote/reset", None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        let new = app.remote.saved().await.install_id.unwrap();
        assert_ne!(new, old);

        // The old install is gone at the relay: the leaked key signs for nothing, its tunnel dial is
        // a 404, and its host shows the unknown-install page.
        assert_eq!(relay_knows(&relay, &old, &leaked).await, StatusCode::NOT_FOUND);
        let dial = format!("{relay}/tunnel/{old}").into_client_request().unwrap();
        let refused = tokio_tungstenite::connect_async(dial)
            .await
            .expect_err("the old install cannot dial");
        assert!(
            matches!(&refused, tungstenite::Error::Http(res) if res.status() == StatusCode::NOT_FOUND),
            "{refused:?}"
        );
        let old_host = format!("{old}.my.colonizer.dev");
        let (status, _, _) = relay_http(port, "/", &old_host, None).await;
        assert_eq!(status, 404);
        // The new link is live under the new key.
        let key = read_key(&app.remote.dir).unwrap();
        assert_eq!(relay_knows(&relay, &new, &key).await, StatusCode::OK);
        until(&app, |v| v["connected"] == true, "the new tunnel never connected").await;
    }

    #[tokio::test]
    async fn a_reset_the_relay_cannot_hear_keeps_the_old_link() {
        // A relay that registers but refuses everything signed (here: a fake that answers 503).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let Ok((mut io, _)) = listener.accept().await else { return };
                let head = read_head(&mut io).await;
                n += 1;
                let len: usize = header_of(&head, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                let mut body = vec![0u8; len];
                io.read_exact(&mut body).await.unwrap();
                let answer = if head.starts_with("POST /api/installs ") {
                    let body =
                        json!({"install_id": format!("install{n}"), "host": format!("install{n}.my.colonizer.dev")}).to_string();
                    format!(
                        "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                };
                io.write_all(answer.as_bytes()).await.unwrap();
            }
        });
        let root = temp_root();
        let app = test_app(root.path());
        app.remote.set_relay(format!("ws://127.0.0.1:{port}")).await;
        let router = remote_router(&app, idle_park());
        set_enabled(&app, true).await.unwrap();
        let before = app.remote.saved().await.install_id.unwrap();
        let key_before = std::fs::read(app.remote.dir.join(KEY_FILE)).unwrap();
        let (status, answer) = local(&router, &app, Method::POST, "/api/remote/reset", None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{answer}");
        assert!(answer.to_string().contains("could not retire the old link"), "{answer}");
        assert_eq!(app.remote.saved().await.install_id.unwrap(), before, "the old link is kept");
        assert_eq!(
            std::fs::read(app.remote.dir.join(KEY_FILE)).unwrap(),
            key_before,
            "and its key"
        );
    }

    #[tokio::test]
    async fn pairing_decisions_are_refused_through_the_tunnel() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        // The owner's own token, through the real tunnel: the guard admits it, the handler does not.
        let headers = json!([["Authorization", link_bearer(&app)], ["Content-Type", "application/json"]]);
        for (id, method, path) in [
            ("c", "POST", "/api/remote/pairing/confirm"),
            ("r", "POST", "/api/remote/pairing/reject"),
            ("u", "DELETE", "/api/remote/owner"),
        ] {
            ws.send(req_frame(id, method, path, headers.clone())).await.unwrap();
            ws.send(body_frame(id, br#"{"code":"123456"}"#)).await.unwrap();
            let (status, _, body) = read_response(&mut ws, id, path).await;
            assert_eq!(status, 403, "{path} through the tunnel: {}", String::from_utf8_lossy(&body));
            assert!(String::from_utf8_lossy(&body).contains("on this machine"));
        }
        let kinds = activity_kinds(&app).await;
        assert!(!kinds.iter().any(|k| k.starts_with("remote.pair") || k == "remote.unpair"));
    }

    /// Whether a tunnelled answer carries the rejected-credential verdict for the relay (#1086).
    fn verdict(headers: &[Value]) -> Option<String> {
        headers
            .iter()
            .find(|h| {
                h[0].as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(CREDENTIAL_VERDICT))
            })
            .and_then(|h| h[1].as_str().map(str::to_string))
    }

    #[tokio::test]
    async fn the_cockpit_tells_the_relay_which_tunnelled_credentials_it_rejected() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let live = app.remote.add_link("test").unwrap();
        let revoked = app.remote.add_link("gone").unwrap();
        let id = app.remote.links.read().unwrap().last().unwrap().id.clone();
        assert!(app.remote.revoke_link(&id));
        let forged = format!("clk_{}", "0".repeat(64));
        let phone = format!("cph_{}", "1".repeat(64));
        let cookie = |token: &str| format!("{}={token}", crate::auth::COOKIE_NAME);
        let invite = format!("/?pair={}", "f".repeat(64));
        let cases: Vec<(&str, &str, Value, u16, bool)> = vec![
            // A live link credential: through, and no verdict.
            ("a", "/api/remote", json!([["Cookie", cookie(&live)]]), 200, false),
            // Revoked, forged, an unknown phone: answered 401 as before, and marked for the relay —
            // on an API call and on a page load alike.
            ("b", "/api/remote", json!([["Cookie", cookie(&revoked)]]), 401, true),
            ("c", "/", json!([["Cookie", cookie(&forged)]]), 401, true),
            (
                "d",
                "/api/remote",
                json!([["Authorization", format!("Bearer {forged}")]]),
                401,
                true,
            ),
            ("e", "/", json!([["Cookie", cookie(&phone)]]), 401, true),
            // No credential at all is not a rejected one (the relay never forwards it anyway).
            ("f", "/api/remote", json!([]), 401, false),
            // An invite that opens nothing is marked too; an invite that opens shows its page, unmarked.
            ("g", &invite, json!([]), 401, true),
        ];
        for (id, path, headers, want, marked) in cases {
            ws.send(req_frame(id, "GET", path, headers)).await.unwrap();
            let (status, headers, _) = read_response(&mut ws, id, path).await;
            assert_eq!(status, want, "{id} {path}");
            assert_eq!(verdict(&headers).as_deref(), marked.then_some("rejected"), "{id} {path}");
        }
        let code = app.phones.book().mint_link(crate::phone::now_secs()).unwrap();
        let path = format!("/?pair={code}");
        ws.send(req_frame("h", "GET", &path, json!([]))).await.unwrap();
        let (status, headers, _) = read_response(&mut ws, "h", &path).await;
        assert_eq!(status, 200);
        assert_eq!(verdict(&headers), None);
        // A claim whose pairing secret finds nothing is marked; locally the same claim is not.
        ws.send(req_frame(
            "i",
            "POST",
            "/api/phone/claim",
            json!([["Cookie", "colonizer_pair=ph_00000000.nope"]]),
        ))
        .await
        .unwrap();
        ws.send(end_body("i")).await.unwrap();
        let (status, headers, _) = read_response(&mut ws, "i", "claim").await;
        assert_eq!(status, 404);
        assert_eq!(verdict(&headers).as_deref(), Some("rejected"));
    }

    #[tokio::test]
    async fn a_tunnelled_websocket_on_a_rejected_credential_closes_4401() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let forged = format!("{}=clk_{}", crate::auth::COOKIE_NAME, "0".repeat(64));
        let host = app.remote.saved().await.host.unwrap();
        let open = json!({"t": "ws_open", "id": "w", "path": "/api/stream", "headers": [["Cookie", forged], ["Origin", format!("https://{host}")]]});
        ws.send(open.to_string().into()).await.unwrap();
        let frame = next_frame(&mut ws, "the ws_close").await;
        assert_eq!(frame["t"], "ws_close");
        assert_eq!(frame["code"], 4401);
    }

    #[tokio::test]
    async fn a_relay_that_names_no_gate_at_registration_is_taken_as_gating() {
        // The fake relay answers like one deployed before #1086: no `require_github` in its answer,
        // and it gates every install with GitHub whatever was asked. The cockpit must read it so.
        let root = temp_root();
        let app = test_app(root.path());
        let (relay, _tunnels) = spawn_relay().await;
        app.remote.set_relay(relay).await;
        assert_eq!(
            app.remote.view().await["require_github"],
            false,
            "a new install asks for no gate"
        );
        set_enabled(&app, true).await.unwrap();
        assert_eq!(app.remote.saved().await.require_github, Some(true));
        assert_eq!(app.remote.view().await["require_github"], true);
    }

    #[test]
    fn the_github_gate_defaults_to_what_the_install_was_built_with() {
        // A state file from before the setting: a registered install keeps the gate, a fresh one has none.
        let old: Saved = serde_json::from_str(r#"{"enabled": true, "install_id": "i", "host": "h"}"#).unwrap();
        assert!(old.require_github());
        let fresh: Saved = serde_json::from_str(r#"{"enabled": false, "install_id": null, "host": null}"#).unwrap();
        assert!(!fresh.require_github());
        assert!(!Saved::default().require_github());
        let set: Saved =
            serde_json::from_str(r#"{"enabled": true, "install_id": "i", "host": "h", "require_github": false}"#).unwrap();
        assert!(!set.require_github());
    }

    #[tokio::test]
    async fn the_github_gate_is_switched_only_here_and_carried_into_the_registration() {
        let root = temp_root();
        let app = test_app(root.path());
        let router = remote_router(&app, idle_park());
        // Before any link exists the choice is only saved; nothing is called (the relay is unset here).
        let (status, view) = local(
            &router,
            &app,
            Method::PUT,
            "/api/remote/require-github",
            Some(json!({"require_github": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["require_github"], true);
        assert_eq!(app.remote.saved().await.require_github, Some(true));
        // Through the tunnel it is refused, like every decision about who may reach the link.
        let refused = local_only(
            Some(&Extension(Tunnelled { host: "h".into() })),
            "change the GitHub sign-in requirement",
        );
        assert!(refused.is_err());
        // Enabling registers with it (the real relay honours it: the whole-link test's reset step).
        let (relay, _tunnels) = spawn_relay().await;
        app.remote.set_relay(relay).await;
        set_enabled(&app, true).await.unwrap();
        assert_eq!(app.remote.view().await["require_github"], true);
        assert_eq!(app.remote.saved().await.require_github, Some(true));
        let kinds = activity_kinds(&app).await;
        assert_eq!(kinds.iter().filter(|k| *k == "remote.require_github").count(), 1);
    }

    #[tokio::test]
    async fn a_scoped_token_cannot_reach_the_pairing_routes() {
        let root = temp_root();
        std::fs::create_dir_all(root.path().join("config")).unwrap();
        let app = test_app(root.path());
        let router = remote_router(&app, idle_park());
        for scope in ["read", "operate", "launch"] {
            let made = app
                .api_tokens
                .create(crate::api_tokens::NewToken {
                    name: format!("t-{scope}"),
                    scope: scope.into(),
                    orgs: Vec::new(),
                    repos: Vec::new(),
                    max_concurrent: None,
                    budget_usd_per_day: None,
                })
                .await
                .unwrap();
            for (method, path) in [
                (Method::GET, "/api/remote/pairing"),
                (Method::POST, "/api/remote/pairing/confirm"),
                (Method::POST, "/api/remote/pairing/reject"),
                (Method::DELETE, "/api/remote/owner"),
            ] {
                let request = Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:7878")
                    .header(header::AUTHORIZATION, format!("Bearer {}", made.token))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"123456"}"#))
                    .unwrap();
                let status = router.clone().oneshot(request).await.unwrap().status();
                assert_eq!(status, StatusCode::FORBIDDEN, "{scope} token, {method} {path}");
            }
        }
    }

    #[tokio::test]
    async fn pairing_without_a_link_is_a_conflict_not_a_relay_call() {
        let root = temp_root();
        let app = test_app(root.path());
        let router = remote_router(&app, idle_park());
        let (status, answer) = local(&router, &app, Method::GET, "/api/remote/pairing", None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{answer}");
        let (status, _) = local(
            &router,
            &app,
            Method::POST,
            "/api/remote/pairing/confirm",
            Some(json!({"code": "123456"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = local(&router, &app, Method::DELETE, "/api/remote/owner", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn disabling_closes_the_tunnel_and_shuts_the_door() {
        let (app, router, mut tunnels, _root) = enabled_app().await;
        let host = app.remote.saved().await.host.unwrap();
        let mut ws = next_tunnel(&mut tunnels).await;
        let res = switch(&app, &router, false).await;
        assert_eq!(res.status(), StatusCode::OK);
        // The tunnel socket closes, and nothing redials while the switch is off.
        let closed = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("the tunnel closed in time");
        assert!(
            !matches!(closed, Some(Ok(tungstenite::Message::Text(_)))),
            "no frames ride a closed tunnel"
        );
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(tunnels.try_recv().is_err(), "nothing redials while disabled");
        let view = app.remote.view().await;
        assert_eq!(view["enabled"], false);
        assert_eq!(view["connected"], false);
        assert_eq!(view["host"], host.as_str(), "the identity is kept so the host is stable");
        // The tunnel host is not a LAN host: a plain request with it fails the allowlist.
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, &host)
            .body(Body::empty())
            .unwrap();
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
        // And a forged Tunnelled marker is refused while the switch is off.
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, &host)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Tunnelled { host: host.clone() });
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn the_tunnel_host_is_never_a_lan_host() {
        let root = temp_root();
        let app = test_app(root.path());
        let router = remote_router(&app, idle_park());
        let host = "09876543.my.colonizer.dev";
        // No marker, tunnel Host: the LAN allowlist refuses, bearer or not.
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::FORBIDDEN);
        // The marker alone is not enough while the switch is off...
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Tunnelled { host: host.into() });
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // ...nor while it is on, when the marker names some other host than the request's...
        app.remote
            .persist(Saved {
                enabled: true,
                install_id: Some("i".into()),
                host: Some(host.into()),
                require_github: None,
            })
            .await
            .unwrap();
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Tunnelled {
            host: "other.my.colonizer.dev".into(),
        });
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // ...and even the real thing still needs the token once the guard lets it through.
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Tunnelled { host: host.into() });
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        // The install token is refused through the tunnel (R3); a link credential is what works.
        let tunnelled = |auth: String| {
            let mut request = Request::builder()
                .method(Method::GET)
                .uri("/api/remote")
                .header(header::HOST, host)
                .header(header::AUTHORIZATION, auth)
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(Tunnelled { host: host.into() });
            request
        };
        let refused = router
            .clone()
            .oneshot(tunnelled(format!("Bearer {}", app.api_token)))
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            router.clone().oneshot(tunnelled(link_bearer(&app))).await.unwrap().status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn the_thirty_third_stream_is_refused() {
        let (opened, mut opened_rx) = mpsc::unbounded_channel();
        let (release, release_rx) = watch::channel(false);
        let (app, _router, mut tunnels, _root) = enabled_app_with(Park {
            opened,
            release: release_rx,
        })
        .await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let token = link_bearer(&app);
        for i in 1..=33 {
            ws.send(req_frame(
                &i.to_string(),
                "GET",
                "/api/park",
                json!([["Authorization", token]]),
            ))
            .await
            .unwrap();
        }
        for _ in 0..32 {
            tokio::time::timeout(Duration::from_secs(5), opened_rx.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(opened_rx.try_recv().is_err(), "only 32 of the 33 streams are served");
        // The 33rd is refused at once: a 503 res and the empty end-of-body frame.
        let refused = next_frame(&mut ws, "the refused stream").await;
        assert_eq!(refused["t"], "res", "the refusal is a res frame");
        assert_eq!(refused["status"], 503, "the 33rd stream is refused with 503");
        let end = next_frame(&mut ws, "the refused body").await;
        assert_eq!(end["t"], "body");
        assert_eq!(end["end"], true);
        // Let the parked 32 through: each answers 200 and ends cleanly.
        let _ = release.send(true);
        let (mut answered, mut ended) = (0u32, 0u32);
        while answered < 32 || ended < 32 {
            let frame = next_frame(&mut ws, "a park response").await;
            match frame["t"].as_str() {
                Some("res") => {
                    assert_eq!(frame["status"], 200, "the served streams answer");
                    answered += 1;
                }
                Some("body") => {
                    assert_eq!(frame["end"], true, "every served stream ends cleanly");
                    ended += 1;
                }
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_large_answer_arrives_in_capped_chunks() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(req_frame("big", "GET", "/api/big", headers)).await.unwrap();
        let res = next_frame(&mut ws, "the big res").await;
        assert_eq!(res["status"], 200);
        let mut body = Vec::new();
        let mut chunks = 0;
        loop {
            let frame = next_frame(&mut ws, "a big body frame").await;
            let chunk = util::b64_decode(frame["chunk"].as_str().unwrap()).unwrap();
            assert!(chunk.len() <= CHUNK, "every chunk is within the cap");
            body.extend_from_slice(&chunk);
            chunks += 1;
            if frame["end"].as_bool().unwrap_or(false) {
                break;
            }
        }
        assert_eq!(body.len(), 120_000);
        assert_eq!(chunks, 3, "49152 + 49152 + 21696");
        assert!(body.iter().all(|byte| *byte == 0xAB));
    }

    #[tokio::test]
    async fn a_duplicate_stream_id_is_refused() {
        let (opened, mut opened_rx) = mpsc::unbounded_channel();
        let (release, release_rx) = watch::channel(false);
        let (app, _router, mut tunnels, _root) = enabled_app_with(Park {
            opened,
            release: release_rx,
        })
        .await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        // Hold one stream open, then present the same id again while it is still live.
        ws.send(req_frame("d", "GET", "/api/park", headers.clone())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), opened_rx.recv())
            .await
            .unwrap()
            .unwrap();
        ws.send(req_frame("d", "GET", "/api/park", headers)).await.unwrap();
        let refused = next_frame(&mut ws, "the duplicate refusal").await;
        assert_eq!(refused["t"], "res");
        assert_eq!(refused["id"], "d");
        assert_eq!(refused["status"], 503, "a duplicate id is refused");
        let end = next_frame(&mut ws, "the refusal body").await;
        assert_eq!(end["t"], "body");
        assert_eq!(end["end"], true);
        // The open stream is untouched, and answers once released.
        let _ = release.send(true);
        let (status, _, _) = read_response(&mut ws, "d", "the held stream").await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_and_the_tunnel_lives_on() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(req_frame("big", "POST", "/api/remote", headers.clone()))
            .await
            .unwrap();
        // Full-size chunks, the way a relay streams an upload, until the body passes the cap.
        let chunk = util::b64_encode(&vec![0x41; CHUNK]);
        for _ in 0..=MAX_BODY / CHUNK {
            ws.send(
                json!({"t": "body", "id": "big", "chunk": chunk, "end": false})
                    .to_string()
                    .into(),
            )
            .await
            .unwrap();
        }
        let res = next_frame(&mut ws, "the oversized refusal").await;
        assert_eq!(res["t"], "res");
        assert_eq!(res["id"], "big");
        assert_eq!(res["status"], 413, "a body past the cap is refused");
        let end = next_frame(&mut ws, "the refusal body").await;
        assert_eq!(end["t"], "body");
        assert_eq!(end["end"], true);
        // Later frames for the dead stream are dropped, not buffered; the tunnel answers on.
        ws.send(end_body("big")).await.unwrap();
        ws.send(req_frame("after", "GET", "/api/remote", headers)).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "after", "the next request").await;
        assert_eq!(status, 200);
    }

    // -- Bounded queues (review finding R5) ---------------------------------

    #[tokio::test]
    async fn a_body_chunk_past_the_cap_is_refused_before_it_is_decoded() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(req_frame("fat", "POST", "/api/big", headers.clone())).await.unwrap();
        // One chunk three bytes past the 48 KiB a relay may send, ending the body.
        ws.send(body_frame("fat", &vec![0x42; CHUNK + 3])).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "fat", "the fat chunk's refusal").await;
        assert_eq!(status, 413, "refused on its size, not routed");
        // A chunk exactly at the cap is fine (the route then refuses the method itself).
        ws.send(req_frame("ok", "POST", "/api/big", headers)).await.unwrap();
        ws.send(body_frame("ok", &vec![0x42; CHUNK])).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "ok", "the full chunk").await;
        assert_eq!(status, 405);
    }

    #[tokio::test]
    async fn a_relay_message_past_the_cap_ends_the_tunnel() {
        let (_app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        // A well-formed ping, padded past the tunnel's message cap: the client must not take it in.
        let padded = json!({"t": "ping", "pad": "x".repeat(TUNNEL_MESSAGE_MAX)}).to_string();
        let _ = ws.send(padded.into()).await;
        let answer = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("the client reacted");
        assert!(
            !matches!(&answer, Some(Ok(tungstenite::Message::Text(text))) if text.contains("pong")),
            "the oversized frame was read and answered: {answer:?}"
        );
        // The supervisor redials a fresh tunnel.
        next_tunnel(&mut tunnels).await;
    }

    #[tokio::test]
    async fn a_socket_whose_handler_stops_reading_is_closed_not_queued() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", link_bearer(&app)]]);
        ws.send(
            json!({ "t": "ws_open", "id": "deaf", "path": "/api/deaf", "headers": headers })
                .to_string()
                .into(),
        )
        .await
        .unwrap();
        // Far more than the in-memory pipe and the per-socket queue can hold together.
        let data = "y".repeat(60 * 1024);
        for _ in 0..(WS_QUEUE * 2) {
            ws.send(
                json!({ "t": "ws_msg", "id": "deaf", "data": data, "binary": false })
                    .to_string()
                    .into(),
            )
            .await
            .unwrap();
        }
        let close = next_frame(&mut ws, "the overflow close").await;
        assert_eq!(close["t"], "ws_close");
        assert_eq!(close["id"], "deaf");
        assert_eq!(close["code"], 1008);
        // The tunnel itself is unharmed.
        ws.send(req_frame("after", "GET", "/api/big", headers)).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "after", "a request after the overflow").await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn remote_switches_are_recorded_once_each() {
        let (app, router, mut tunnels, _root) = enabled_app().await;
        let _ws = next_tunnel(&mut tunnels).await;
        // A second enable with nothing changed records nothing, nor does a second disable.
        assert_eq!(switch(&app, &router, true).await.status(), StatusCode::OK);
        assert_eq!(switch(&app, &router, false).await.status(), StatusCode::OK);
        assert_eq!(switch(&app, &router, false).await.status(), StatusCode::OK);
        // A reset changes the identity, so it records.
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/remote/reset")
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
        let kinds = activity_kinds(&app).await;
        assert_eq!(kinds.iter().filter(|k| *k == "remote.enable").count(), 1);
        assert_eq!(kinds.iter().filter(|k| *k == "remote.disable").count(), 1);
        assert_eq!(kinds.iter().filter(|k| *k == "remote.reset").count(), 1);
    }

    async fn activity_kinds(app: &Shared) -> Vec<String> {
        let request = Request::builder()
            .method(Method::GET)
            .uri("/api/activity")
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .body(Body::empty())
            .unwrap();
        let res = Router::new()
            .route("/api/activity", get(crate::activity::list))
            .layer(middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
            .oneshot(request)
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        let page: Value = serde_json::from_slice(&body).unwrap();
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    }
}
