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

use crate::{ApiResult, AppError, Shared, activity, client_error, util};
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

/// The switch and identity as persisted (`<config>/remote/state.json`). No file means off.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    enabled: bool,
    install_id: Option<String>,
    host: Option<String>,
}

/// Marks a request that arrived through the tunnel. Only in-process code can create one —
/// the supervisor inserts it after the relay frames are decoded — so `host_guard` can admit
/// requests no network peer could forge.
#[derive(Clone)]
pub struct Tunnelled {
    pub host: String,
}

/// Settings, identity and live link status, held on `App`.
pub struct Remote {
    dir: PathBuf,
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
        Ok(Self {
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

    async fn saved(&self) -> Saved {
        self.saved.read().await.clone()
    }

    async fn relay(&self) -> String {
        self.relay.read().await.clone()
    }

    /// What `GET /api/remote` answers. A tunnel is only ever "connected" while the switch is on:
    /// the supervisor's status can lag behind a disable, and must not read as a live link then.
    async fn view(&self) -> Value {
        let saved = self.saved.read().await;
        let status = self.status.read().await;
        let connected = saved.enabled && status.connected;
        json!({
            "enabled": saved.enabled,
            "host": saved.host,
            "connected": connected,
            "since": connected.then(|| status.since.clone()).flatten(),
        })
    }

    async fn set_connected(&self, on: bool) {
        let mut status = self.status.write().await;
        status.connected = on;
        status.since = on.then(|| Utc::now().to_rfc3339());
    }

    /// Makes `saved` the state, here and on disk. Callers hand over the mutated copy.
    async fn persist(&self, saved: Saved) -> Result<()> {
        *self.saved.write().await = saved.clone();
        std::fs::create_dir_all(&self.dir)?;
        util::write_atomic(&self.dir.join(STATE_FILE), &serde_json::to_vec(&saved)?).await
    }

    /// The test relay's base URL (`COLONIZER_REMOTE_URL` is read once at construction, and
    /// ambient env vars must not leak into a parallel test run).
    #[cfg(test)]
    async fn set_relay(&self, url: String) {
        *self.relay.write().await = url.trim_end_matches('/').to_string();
    }
}

// ---------------------------------------------------------------------------
// The API: `GET`/`PUT /api/remote`, `POST /api/remote/reset`.
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
    Ok(Json(view))
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
            let (install_id, host) = register(&app.remote.relay().await, &key).await?;
            saved.install_id = Some(install_id);
            saved.host = Some(host);
        }
        saved.enabled = true;
        app.remote.persist(saved).await?;
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
    let (install_id, host) = register(&app.remote.relay().await, &key).await?;
    util::write_private(&app.remote.dir.join(KEY_FILE), doc.as_ref())?;
    let mut saved = app.remote.saved().await;
    saved.install_id = Some(install_id);
    saved.host = Some(host);
    app.remote.persist(saved).await?;
    app.remote.signal.send_replace(()); // an immediate redial under the new identity
    Ok(app.remote.view().await)
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

/// Registers the public key with the relay: `POST /api/installs` answers `{"install_id", "host"}`,
/// and the tunnel URL and admitted `Host` are both derived from that host.
async fn register(relay: &str, key: &Ed25519KeyPair) -> Result<(String, String), AppError> {
    let bad = |message: String| client_error(StatusCode::BAD_GATEWAY, &message);
    let url = format!("{}/api/installs", http_base(relay)?);
    let answer = tokio::time::timeout(
        REGISTER_WAIT,
        reqwest::Client::new()
            .post(&url)
            .json(&json!({"public_key": util::b64_encode(key.public_key().as_ref())}))
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
    Ok((field("install_id")?, field("host")?))
}

/// `wss://my.colonizer.dev` → `https://my.colonizer.dev` (and ws → http, for a local test relay).
fn http_base(relay: &str) -> Result<String> {
    if let Some(rest) = relay.strip_prefix("wss://") {
        Ok(format!("https://{rest}"))
    } else if let Some(rest) = relay.strip_prefix("ws://") {
        Ok(format!("http://{rest}"))
    } else {
        bail!("COLONIZER_REMOTE_URL must be a ws:// or wss:// URL, got {relay:?}")
    }
}

/// The activity log line for a switch change, recorded by the handler (not the route layer, which
/// has no rule for these routes) and only after the state actually changed.
async fn record_change(app: &Shared, kind: &str, via: Option<Extension<crate::auth::Via>>) {
    let mut entry = activity::Entry::new(kind, "you");
    entry.via = via.map(|Extension(v)| match v {
        crate::auth::Via::Cockpit => "cockpit".to_string(),
        crate::auth::Via::Api => "api".to_string(),
    });
    entry.target = Some("remote access".into());
    entry.section = Some("remote".into());
    activity::record(app, entry).await;
}

// ---------------------------------------------------------------------------
// The supervisor.
// ---------------------------------------------------------------------------

/// Spawned by `serve()` with the finished router: while remote access is enabled, keeps exactly
/// one tunnel to the relay open, serving frames until the relay or the operator drops it, and
/// redialing with exponential backoff (1 s doubling to 60 s, jittered, reset once the relay has
/// accepted the hello — not by a bare TCP/WebSocket connect). A disable or reset tears the
/// connection and every in-flight stream down at once.
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
            tokio::select! {
                accepted = connect_and_serve(&app, &router) => {
                    if accepted {
                        delay = RECONNECT_MIN;
                    }
                }
                _ = signal.changed() => redial = true,
            }
            if !app.remote.enabled().await {
                break;
            }
            if redial {
                continue; // a reset wants the new identity live now, not after the backoff
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

/// One live tunnel: dial, answer the challenge, serve frames. Answers `true` only once the relay
/// has shown it accepted the hello — its first frame on the connection — so a relay that hangs up
/// on a bad signature is not mistaken for a working tunnel (that would reset the backoff into a
/// silent one-second redial loop). Everything spawned here is aborted when the future is dropped
/// — relay drop, disable, or reset. It watches the signal through its own subscription, so the
/// caller's stays free.
async fn connect_and_serve(app: &Shared, router: &Router) -> bool {
    let saved = app.remote.saved().await;
    let (Some(install_id), Some(host)) = (saved.install_id, saved.host) else {
        return false; // enabled without an identity cannot happen: enabling registers first
    };
    let Ok(key) = load_key(&app.remote.dir) else {
        eprintln!("remote: cannot load the access key from {}", app.remote.dir.display());
        return false;
    };
    let relay = app.remote.relay().await;
    let Ok(request) = format!("{relay}/tunnel/{install_id}").into_client_request() else {
        eprintln!("remote: bad relay URL {relay:?}");
        return false;
    };
    let mut signal = app.remote.signal.subscribe();
    let ws = tokio::select! {
        ws = handshake(request, &key, &install_id) => match ws {
            Ok(ws) => ws,
            Err(e) => {
                eprintln!("remote: dialing {relay} failed: {e:#}");
                return false;
            }
        },
        _ = signal.changed() => return false,
    };
    let accepted = serve_connection(app, router, ws, host).await;
    app.remote.set_connected(false).await;
    if !accepted {
        eprintln!("remote: the relay hung up before answering the hello (bad signature, or wrong install?); keeping the backoff");
    }
    accepted
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
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, None),
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

/// What a relay frame carries for one open tunnelled websocket.
enum WsIn {
    Msg(tungstenite::Message),
    Close(u16),
}

/// Request bodies waiting for their remaining `body` frames, keyed by stream id; a sender's
/// presence is also the stream's claim on the id.
type Bodies = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<(String, bool)>>>>;

/// The state of one live connection: the frame queue every task sends through, the routing
/// tables for open streams, the stream budget, and the spawned tasks torn down on exit.
struct Conn {
    host: String,
    out: mpsc::Sender<String>,
    /// Feeds one side of a duplex pair to the in-memory websocket server per `ws_open`.
    conns: mpsc::Sender<DuplexStream>,
    ws_in: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsIn>>>>,
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
/// stream's own.
struct Routing<T> {
    map: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<T>>>>,
    id: String,
    mine: mpsc::UnboundedSender<T>,
}

impl<T> Drop for Routing<T> {
    fn drop(&mut self) {
        let mut map = self.map.lock().unwrap();
        if map.get(&self.id).is_some_and(|tx| tx.same_channel(&self.mine)) {
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
async fn serve_connection(app: &Shared, router: &Router, ws: WebSocketStream<MaybeTlsStream<TcpStream>>, host: String) -> bool {
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
                        dispatch(&conn, router, &text);
                    }
                    Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
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
    accepted
}

/// One frame from the relay: a ping, a new stream, or traffic for an open one. Frames outside the
/// v1 set are ignored rather than allowed to kill the tunnel.
fn dispatch(conn: &Arc<Conn>, router: &Router, frame: &str) {
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
                let _ = sink.send((chunk, end));
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
            let sink = conn.ws_in.lock().unwrap().get(&id.to_string()).cloned();
            if let Some(sink) = sink {
                let _ = sink.send(WsIn::Msg(message));
            }
        }
        Frame::WsClose { id, code } => {
            let sink = conn.ws_in.lock().unwrap().get(&id.to_string()).cloned();
            if let Some(sink) = sink {
                let _ = sink.send(WsIn::Close(code));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Serving a tunnelled request.
// ---------------------------------------------------------------------------

fn start_req(conn: &Arc<Conn>, router: Router, id: Value, method: String, path: String, headers: Option<Value>) {
    // The routing entry is claimed under the lock, so a duplicate id is refused without touching
    // the open stream — and its guard is what removes the entry, on every exit below.
    let (tx, rx) = mpsc::unbounded_channel();
    let routing = match conn.bodies.lock().unwrap().entry(id.to_string()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            conn.refuse_res(&id, StatusCode::SERVICE_UNAVAILABLE);
            return;
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(tx.clone());
            Routing {
                map: conn.bodies.clone(),
                id: id.to_string(),
                mine: tx,
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
    mut chunks: mpsc::UnboundedReceiver<(String, bool)>,
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
        .map(|(name, value)| json!([name.as_str(), value.to_str().unwrap_or_default()]))
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
    let (tx, from_relay) = mpsc::unbounded_channel();
    let routing = match conn.ws_in.lock().unwrap().entry(id.to_string()) {
        std::collections::hash_map::Entry::Occupied(_) => {
            conn.close_ws(&id, 1008);
            return;
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(tx.clone());
            Routing {
                map: conn.ws_in.clone(),
                id: id.to_string(),
                mine: tx,
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
    mut from_relay: mpsc::UnboundedReceiver<WsIn>,
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
        Err(_) => {
            conn.close_ws(&id, 1014); // Bad Gateway: the inner upgrade did not happen
            return;
        }
    };
    let (mut inner_out, mut inner_in) = inner.split();
    // Relay → cockpit.
    conn.spawn(async move {
        while let Some(msg) = from_relay.recv().await {
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
    // Cockpit → relay.
    loop {
        match inner_in.next().await {
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// A fake relay on 127.0.0.1:0: `POST /api/installs` registers a public key, and
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

    async fn big_answer() -> Vec<u8> {
        vec![0xAB; 120_000] // 2.4 chunks at the 48 KiB cap
    }

    fn remote_router(app: &Shared, park: Park) -> Router {
        Router::new()
            .route("/api/remote", get(status).put(put))
            .route("/api/remote/reset", post(reset))
            .route("/api/activity", get(crate::activity::list))
            .route("/api/stream", get(crate::stream::handler))
            .route("/api/big", get(big_answer))
            .route("/api/park", get(park_handler))
            .layer(Extension(park))
            .layer(middleware::from_fn_with_state(app.clone(), crate::host_guard))
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

    #[tokio::test]
    async fn a_tunnelled_request_round_trips() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let host = app.remote.saved().await.host.unwrap();
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", format!("Bearer {}", app.api_token)]]);
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
        let cookie = format!("{}={}", crate::auth::COOKIE_NAME, app.api_token);
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
            ["Authorization", format!("Bearer {}", app.api_token)],
            ["Origin", "http://evil.example"],
            ["Content-Type", "application/json"],
        ]);
        ws.send(req_frame("c", "PUT", "/api/remote", headers)).await.unwrap();
        ws.send(body_frame("c", br#"{"enabled":true}"#)).await.unwrap();
        let (status, _, _) = read_response(&mut ws, "c", "the bearer write").await;
        assert_eq!(status, 200);
    }

    #[tokio::test]
    async fn a_tunnelled_websocket_reaches_a_real_upgrade() {
        let (app, _router, mut tunnels, _root) = enabled_app().await;
        let mut ws = next_tunnel(&mut tunnels).await;
        let headers = json!([["Authorization", format!("Bearer {}", app.api_token)]]);
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
        let mut request = Request::builder()
            .method(Method::GET)
            .uri("/api/remote")
            .header(header::HOST, host)
            .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(Tunnelled { host: host.into() });
        assert_eq!(router.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
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
        let token = format!("Bearer {}", app.api_token);
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
        let headers = json!([["Authorization", format!("Bearer {}", app.api_token)]]);
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
        let headers = json!([["Authorization", format!("Bearer {}", app.api_token)]]);
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
        let headers = json!([["Authorization", format!("Bearer {}", app.api_token)]]);
        ws.send(req_frame("big", "POST", "/api/remote", headers.clone()))
            .await
            .unwrap();
        let oversized = util::b64_encode(&vec![0x41; MAX_BODY + 1]);
        ws.send(
            json!({"t": "body", "id": "big", "chunk": oversized, "end": false})
                .to_string()
                .into(),
        )
        .await
        .unwrap();
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
            .layer(middleware::from_fn_with_state(app.clone(), crate::host_guard))
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
