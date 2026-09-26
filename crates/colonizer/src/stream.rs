//! The Cockpit push channel: `GET /api/stream`, one WebSocket per open tab. A single shared
//! hub task diffs the four dashboard sources and broadcasts only what changed; each connection
//! gets the current full values first, then the deltas. Client messages are ignored.

use crate::{Shared, fleet, orgs, reclaim, sessions};
use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, broadcast, futures::Notified};

/// Sessions are re-diffed this often. Cheap (locks plus `with_activity`), and a 1 s cadence
/// guarantees running cost/token counters reach clients within ~2 s without touching mutation sites.
const SESSIONS_INTERVAL: Duration = Duration::from_secs(1);
/// Orgs assemble from memory — the GitHub refresh inside is throttled to 5 min — so every 2nd tick.
const ORGS_EVERY_TICKS: u64 = 2;
/// Hosts poll every peer over the network (3 s timeout each, concurrently), so 5 s and off the
/// sessions path: one slow peer must never stall session deltas.
const HOSTS_EVERY_TICKS: u64 = 5;
/// Storage walks the data dir on every call, so 20 s, also off the sessions path.
const STORAGE_EVERY_TICKS: u64 = 20;
/// A dead peer is noticed via the ping below, not a write timeout.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// While nobody subscribes the hub sleeps this long between checks, waking early the moment a
/// tab subscribes.
const IDLE_INTERVAL: Duration = Duration::from_secs(30);

/// The latest slow-source frames, so a new tab gets hosts/storage immediately and a lagged one
/// can resync them. Cleared whenever the hub goes idle, so nothing stale survives.
#[derive(Debug, Default)]
struct SlowCache {
    hosts: Option<Arc<str>>,
    storage: Option<Arc<str>>,
}

#[derive(Debug)]
struct Inner {
    tx: broadcast::Sender<Arc<str>>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    slow: std::sync::Mutex<SlowCache>,
    wake: tokio::sync::Notify,
}

/// The shared broadcast hub. One per `App`, created with it; the diff task starts on the first
/// connection and then runs only while a tab is subscribed.
#[derive(Debug, Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(Inner {
                tx,
                task: std::sync::Mutex::new(None),
                slow: std::sync::Mutex::new(SlowCache::default()),
                wake: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Starts the single shared diff task, or restarts it if the previous one died: a finished
    /// handle is replaced, a live one left alone.
    pub fn ensure_running(&self, app: &Shared) {
        let mut task = self.inner.task.lock().unwrap();
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
        *task = Some(tokio::spawn(run_hub(app.clone(), self.clone())));
    }

    fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        // Wake the hub out of its idle sleep, so the new tab's deltas start immediately.
        self.inner.wake.notify_one();
        self.inner.tx.subscribe()
    }

    fn wake(&self) -> Notified<'_> {
        self.inner.wake.notified()
    }

    fn send(&self, frame: String) {
        let _ = self.inner.tx.send(Arc::from(frame));
    }

    /// Caches the frame for late joiners and lagged resyncs, and sends it to live subscribers.
    fn publish_slow(&self, hosts: bool, frame: String) {
        let frame: Arc<str> = Arc::from(frame);
        let mut slow = self.inner.slow.lock().unwrap();
        if hosts {
            slow.hosts = Some(frame.clone());
        } else {
            slow.storage = Some(frame.clone());
        }
        let _ = self.inner.tx.send(frame);
    }

    fn cached_slow(&self) -> Vec<Arc<str>> {
        let slow = self.inner.slow.lock().unwrap();
        slow.hosts.iter().chain(slow.storage.iter()).cloned().collect()
    }

    fn clear_slow(&self) {
        *self.inner.slow.lock().unwrap() = SlowCache::default();
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// The last value sent per source, so only changes go out. Reset when nobody is subscribed, so a
/// tab opened later re-diffs from scratch and gets full values even if nothing changed meanwhile.
#[derive(Default)]
struct LastSent {
    sessions: BTreeMap<String, Value>,
    orgs: Option<Value>,
    hosts: Option<Value>,
    storage: Option<Value>,
}

/// The sources exactly as their GET handlers answer them: the hub calls the handlers directly and
/// unwraps the `Json`, so hub and HTTP can never drift apart. No handler changes were needed.
async fn sessions_snapshot(app: &Shared) -> Vec<sessions::Session> {
    sessions::list(State(app.clone()), None).await.0
}

async fn orgs_snapshot(app: &Shared) -> Vec<Value> {
    orgs::list(State(app.clone())).await.0
}

async fn storage_snapshot(app: &Shared) -> Value {
    reclaim::storage(State(app.clone())).await.0
}

/// The per-session frames for one diff: an upsert per new or changed session, a removal per
/// vanished id, nothing for the unchanged. Pure so tests can drive it without an `App`.
pub(crate) fn diff_sessions(old: &BTreeMap<String, Value>, new: &BTreeMap<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    for (id, session) in new {
        if old.get(id) != Some(session) {
            out.push(json!({"type": "session", "session": session}).to_string());
        }
    }
    for id in old.keys() {
        if !new.contains_key(id) {
            out.push(json!({"type": "session_removed", "id": id}).to_string());
        }
    }
    out
}

fn sessions_map(list: Vec<sessions::Session>) -> BTreeMap<String, Value> {
    list.into_iter()
        .map(|s| {
            let value = serde_json::to_value(&s).unwrap_or(Value::Null);
            (s.id.clone(), value)
        })
        .collect()
}

async fn run_hub(app: Shared, hub: Hub) {
    let last = Arc::new(Mutex::new(LastSent::default()));
    let hosts_busy = Arc::new(AtomicBool::new(false));
    let storage_busy = Arc::new(AtomicBool::new(false));
    let mut tick = tokio::time::interval(SESSIONS_INTERVAL);
    let mut n: u64 = 0;
    // The first active iteration fetches everything, so a tab opened after idle gets full values
    // without waiting for each source's cadence.
    let mut catch_up = true;
    loop {
        if hub.inner.tx.receiver_count() == 0 {
            *last.lock().await = LastSent::default();
            hub.clear_slow();
            catch_up = true;
            tokio::select! {
                _ = tokio::time::sleep(IDLE_INTERVAL) => {}
                _ = hub.wake() => {}
            }
            tick.reset();
            continue;
        }
        tick.tick().await;
        n += 1;
        let map = sessions_map(sessions_snapshot(&app).await);
        {
            let mut last = last.lock().await;
            for frame in diff_sessions(&last.sessions, &map) {
                hub.send(frame);
            }
            last.sessions = map;
        }
        if catch_up || n.is_multiple_of(ORGS_EVERY_TICKS) {
            let orgs = Value::Array(orgs_snapshot(&app).await);
            let mut last = last.lock().await;
            if last.orgs.as_ref() != Some(&orgs) {
                hub.send(json!({"type": "orgs", "orgs": orgs}).to_string());
                last.orgs = Some(orgs);
            }
        }
        // The slow sources run in their own spawned fetch, caching and broadcasting against the
        // same shared last-sent state when they land, so a wedged peer or disk walk only delays
        // its own rows.
        if (catch_up || n.is_multiple_of(HOSTS_EVERY_TICKS)) && !hosts_busy.swap(true, Ordering::SeqCst) {
            let (app, hub, last, busy) = (app.clone(), hub.clone(), last.clone(), hosts_busy.clone());
            tokio::spawn(async move {
                if let Ok(hosts) = tokio::time::timeout(Duration::from_secs(10), fleet::list_hosts(&app)).await {
                    let hosts = serde_json::to_value(&hosts).unwrap_or(Value::Null);
                    let mut last = last.lock().await;
                    if last.hosts.as_ref() != Some(&hosts) {
                        hub.publish_slow(true, json!({"type": "hosts", "hosts": hosts}).to_string());
                        last.hosts = Some(hosts);
                    }
                }
                busy.store(false, Ordering::SeqCst);
            });
        }
        if (catch_up || n.is_multiple_of(STORAGE_EVERY_TICKS)) && !storage_busy.swap(true, Ordering::SeqCst) {
            let (app, hub, last, busy) = (app.clone(), hub.clone(), last.clone(), storage_busy.clone());
            tokio::spawn(async move {
                let storage = storage_snapshot(&app).await;
                let mut last = last.lock().await;
                if last.storage.as_ref() != Some(&storage) {
                    hub.publish_slow(false, json!({"type": "storage", "storage": storage}).to_string());
                    last.storage = Some(storage);
                }
                busy.store(false, Ordering::SeqCst);
            });
        }
        catch_up = false;
    }
}

/// `GET /api/stream`: authenticated by `host_guard` like every other `/api/` route, then upgraded.
pub async fn handler(State(app): State<Shared>, ws: WebSocketUpgrade) -> Response {
    app.stream.ensure_running(&app);
    ws.on_upgrade(move |socket| serve(app, socket))
}

fn text(frame: &str) -> Message {
    Message::Text(frame.to_string().into())
}

/// The two fast full snapshots — the `sessions` list exactly as `GET /api/sessions` answers it,
/// then the `orgs` list as `GET /api/orgs` does — sent first on connect and again after a lag.
/// Built from the snapshots directly, never from the id-keyed diff map, so the sessions frame
/// keeps the GET body's newest-first order.
async fn full_frames(app: &Shared) -> [String; 2] {
    let sessions = serde_json::to_value(sessions_snapshot(app).await).unwrap_or(Value::Null);
    let orgs = Value::Array(orgs_snapshot(app).await);
    [
        json!({"type": "sessions", "sessions": sessions}).to_string(),
        json!({"type": "orgs", "orgs": orgs}).to_string(),
    ]
}

async fn serve(app: Shared, socket: WebSocket) {
    let (mut sink, mut incoming) = socket.split();
    // Subscribe before snapshotting, so nothing the hub sends in between is missed; a duplicate
    // of a snapshot frame is just an idempotent upsert client-side. The cached slow frames mean
    // no per-connection fetch is needed: hosts/storage arrive with the next hub broadcast when
    // the cache is empty.
    let mut sub = app.stream.subscribe();
    let slow: Vec<String> = app.stream.cached_slow().iter().map(|frame| frame.to_string()).collect();
    for frame in full_frames(&app).await.into_iter().chain(slow) {
        if sink.send(text(&frame)).await.is_err() {
            return;
        }
    }
    // A single select loop over the broadcast receiver, the socket and the ping interval: no
    // spawned forwarder, so nothing outlives the connection.
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        tokio::select! {
            msg = sub.recv() => match msg {
                Ok(frame) => {
                    if sink.send(text(&frame)).await.is_err() {
                        return;
                    }
                }
                // A lagged receiver re-sends the full lists instead of the deltas it missed.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    for frame in full_frames(&app).await {
                        if sink.send(text(&frame)).await.is_err() {
                            return;
                        }
                    }
                    for frame in app.stream.cached_slow() {
                        if sink.send(text(&frame)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            msg = incoming.next() => {
                // Client messages are ignored; only the end of the stream matters.
                match msg {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/stream", routing::get(handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use crate::{sessions::SessionStatus, tests::test_app};
    use serde_json::json;

    fn session_value(id: &str, status: &str) -> Value {
        json!({"id": id, "status": status})
    }

    #[test]
    fn new_changed_and_removed_sessions_produce_the_right_frames() {
        let old: BTreeMap<String, Value> = [
            ("a".to_string(), session_value("a", "running")),
            ("b".to_string(), session_value("b", "running")),
            ("gone".to_string(), session_value("gone", "stopped")),
        ]
        .into_iter()
        .collect();
        let new: BTreeMap<String, Value> = [
            ("a".to_string(), session_value("a", "running")),
            ("b".to_string(), session_value("b", "stopped")),
            ("c".to_string(), session_value("c", "running")),
        ]
        .into_iter()
        .collect();
        let frames: Vec<Value> = diff_sessions(&old, &new)
            .iter()
            .map(|f| serde_json::from_str(f).unwrap())
            .collect();
        assert_eq!(
            frames.len(),
            3,
            "changed + new + removed, nothing for the unchanged: {frames:?}"
        );
        assert!(frames.contains(&json!({"type": "session", "session": session_value("b", "stopped")})));
        assert!(frames.contains(&json!({"type": "session", "session": session_value("c", "running")})));
        assert!(frames.contains(&json!({"type": "session_removed", "id": "gone"})));
        assert!(
            !frames
                .iter()
                .any(|f| f.get("session").and_then(|s| s.get("id")) == Some(&json!("a"))),
            "the unchanged session sends nothing: {frames:?}"
        );
    }

    #[test]
    fn identical_snapshots_produce_no_frames() {
        let map: BTreeMap<String, Value> = [("a".to_string(), session_value("a", "running"))].into_iter().collect();
        assert!(diff_sessions(&map, &map).is_empty());
        assert!(diff_sessions(&BTreeMap::new(), &BTreeMap::new()).is_empty());
    }

    #[tokio::test]
    async fn the_full_sessions_frame_matches_the_get_body_newest_first() {
        let root = std::env::temp_dir().join(format!("colonizer-stream-{}", crate::util::short_id()));
        let app = test_app(&root);
        for id in ["first", "second"] {
            let mut session = colony("acme", SessionStatus::Stopped);
            session.id = id.to_string();
            app.sessions.write().await.push(session);
        }
        let frames = full_frames(&app).await;
        let body = sessions::list(State(app.clone()), None).await.0;
        let parsed: Value = serde_json::from_str(&frames[0]).unwrap();
        let frame_ids: Vec<&str> = parsed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        let body_ids: Vec<&str> = body.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(frame_ids, body_ids, "the frame must carry the GET body verbatim");
        assert_eq!(frame_ids, ["second", "first"], "newest-created first");
        let _ = std::fs::remove_dir_all(root);
    }
}
