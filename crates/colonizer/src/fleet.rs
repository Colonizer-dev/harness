//! Fleet visibility (issue #231): `GET /api/hosts` turns this mothership's own `/api/status`
//! numbers, plus a poll-on-request fan-out to any configured peers, into one list an operator with
//! several machines can look at together. There is no second machine reaching in: this host only
//! ever dials *out* to peers named in `COLONIZER_FLEET_PEERS`, over whatever private network the
//! operator already has. `COLONIZER_BIND` stays loopback-only on every host by default; an operator
//! who wants a given host to answer these polls sets that host's own `COLONIZER_BIND` to a private
//! interface IP (never `0.0.0.0`). Nothing here changes that default or opens anything new.

use crate::{Shared, orgs, runtime, sessions::SessionStatus};
use axum::{Json, extract::State};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};
use tokio::sync::Mutex;

/// How long a peer poll waits before that peer is counted unreachable. Short on purpose: the fleet
/// view is one page load, not a background job, and a wedged peer must not hold the whole list up.
const PEER_POLL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HostHealth {
    Online,
    Unreachable,
}

/// One row of the fleet view: this host's own numbers, or a peer's, told from `/api/status`'s
/// `host`, `runtime`, `version` and `queue_depth` fields.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct HostSummary {
    /// This host's stable `host_id` (see `runtime::host_id`), or — for a peer never yet reached —
    /// the peer's configured base URL, so it still has *some* stable key to be listed under.
    pub id: String,
    /// Display name: the host's hostname, or (never-reached peer) its base URL again.
    pub name: String,
    pub platform: String,
    pub os: String,
    pub version: Option<String>,
    pub slots_in_use: usize,
    pub slots_ceiling: usize,
    pub queue_depth: usize,
    pub disk_free_bytes: Option<u64>,
    /// RFC 3339, `None` for a peer that has never once answered.
    pub last_heartbeat: Option<String>,
    pub health: HostHealth,
}

/// Last-known `HostSummary` per configured peer base URL (not per host id: an unreachable peer we
/// have never successfully reached has no id yet beyond its own URL). This machine's own entry is
/// never stored here — `self_summary` always computes it live, so it can never go stale.
#[derive(Default)]
pub struct FleetCache {
    last_known: Mutex<HashMap<String, HostSummary>>,
}

impl FleetCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// This machine's own row, built from the same probes `/api/status` reports — never cached in
/// `FleetCache`, always fresh (though `runtime`/`host` still share their own short-lived probe
/// cache, same as a direct `/api/status` call would).
pub async fn self_summary(app: &Shared) -> HostSummary {
    let modules = app.modules.read().await.clone();
    let (runtime, host) = tokio::join!(runtime::status_runtime(app, false), runtime::status_host(app, false),);
    let sessions = app.sessions.read().await;
    let slots_in_use = sessions.iter().filter(|s| s.status.busy()).count();
    let queue_depth = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
    drop(sessions);
    let slots_ceiling = orgs::global_max_parallel(&modules) as usize;
    HostSummary {
        id: host.id.clone(),
        name: host.hostname.clone().unwrap_or_else(|| host.id.clone()),
        platform: runtime.platform.to_string(),
        os: runtime.os.name.clone(),
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        slots_in_use,
        slots_ceiling,
        queue_depth,
        disk_free_bytes: host.disk_free_bytes,
        last_heartbeat: Some(chrono::Utc::now().to_rfc3339()),
        health: HostHealth::Online,
    }
}

/// One peer's `GET /api/status`, turned into a `HostSummary`. `None` on any failure — connection
/// refused, non-2xx, a body that is not the JSON `/api/status` shape, or a timeout — never a panic;
/// the caller decides what an unreachable peer looks like in the list.
async fn poll_peer(client: &reqwest::Client, base_url: &str, timeout: Duration) -> Option<HostSummary> {
    let url = format!("{}/api/status", base_url.trim_end_matches('/'));
    let response = client.get(&url).timeout(timeout).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    summary_from_status_json(&body)
}

/// The parse half of [`poll_peer`], sealed from the network call so it is testable on a literal
/// JSON value. Pulls exactly the fields `/api/status` adds for this purpose; anything missing or
/// the wrong shape is `None`.
fn summary_from_status_json(body: &Value) -> Option<HostSummary> {
    let host = body.get("host")?;
    let runtime = body.get("runtime")?;
    let id = host.get("id")?.as_str()?.to_string();
    let name = host.get("hostname").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
    let platform = runtime
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let os = runtime
        .get("os")
        .and_then(|o| o.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let version = body.get("version").and_then(|v| v.as_str()).map(str::to_string);
    let slots_in_use = host.get("microvms_live")?.as_u64()? as usize;
    let slots_ceiling = host.get("microvms_ceiling")?.as_u64()? as usize;
    let queue_depth = body.get("queue_depth").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let disk_free_bytes = host.get("disk_free_bytes").and_then(|v| v.as_u64());
    Some(HostSummary {
        id,
        name,
        platform,
        os,
        version,
        slots_in_use,
        slots_ceiling,
        queue_depth,
        disk_free_bytes,
        last_heartbeat: Some(chrono::Utc::now().to_rfc3339()),
        health: HostHealth::Online,
    })
}

/// A peer that just failed to answer: the last-known row with `health` flipped to `Unreachable` if
/// one exists (so a stalled peer keeps showing its last real numbers, not nulls), else a placeholder
/// keyed on the URL itself, cached so later calls have something to fall back to.
async fn unreachable_summary(app: &Shared, base_url: &str) -> HostSummary {
    let mut cache = app.fleet_cache.last_known.lock().await;
    if let Some(prior) = cache.get(base_url) {
        let mut stale = prior.clone();
        stale.health = HostHealth::Unreachable;
        return stale;
    }
    let placeholder = HostSummary {
        id: base_url.to_string(),
        name: base_url.to_string(),
        platform: String::new(),
        os: String::new(),
        version: None,
        slots_in_use: 0,
        slots_ceiling: 0,
        queue_depth: 0,
        disk_free_bytes: None,
        last_heartbeat: None,
        health: HostHealth::Unreachable,
    };
    cache.insert(base_url.to_string(), placeholder.clone());
    placeholder
}

/// The whole fleet: this host first, then every configured peer, polled concurrently. A peer that
/// answers updates `fleet_cache`; one that does not falls back to [`unreachable_summary`]. Nobody
/// vanishes from the list just because a poll failed.
pub async fn list_hosts(app: &Shared) -> Vec<HostSummary> {
    let mut hosts = vec![self_summary(app).await];
    let peers = app.cfg.fleet_peers.clone();
    if peers.is_empty() {
        return hosts;
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build();
    let client = match client {
        Ok(client) => client,
        Err(e) => {
            eprintln!("fleet: could not build an HTTP client, every peer reads as unreachable: {e:#}");
            for url in &peers {
                hosts.push(unreachable_summary(app, url).await);
            }
            return hosts;
        }
    };
    let polls = peers
        .iter()
        .map(|url| async { (url.as_str(), poll_peer(&client, url, PEER_POLL_TIMEOUT).await) });
    for (url, result) in futures_util::future::join_all(polls).await {
        match result {
            Some(summary) => {
                app.fleet_cache
                    .last_known
                    .lock()
                    .await
                    .insert(url.to_string(), summary.clone());
                hosts.push(summary);
            }
            None => hosts.push(unreachable_summary(app, url).await),
        }
    }
    hosts
}

/// `GET /api/hosts`: `{"hosts": [HostSummary, ...]}`, self first.
pub async fn list_hosts_handler(State(app): State<Shared>) -> Json<Value> {
    Json(json!({"hosts": list_hosts(&app).await}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_app_with;
    use axum::{Router, routing::get};
    use std::net::TcpListener as StdTcpListener;

    #[tokio::test]
    async fn one_host_with_no_peers_configured_lists_only_itself() {
        let root = temp_root();
        let app = test_app_with(&root, |_| {});
        let hosts = list_hosts(&app).await;
        assert_eq!(hosts.len(), 1, "no peers configured: just this machine");
        assert_eq!(hosts[0].health, HostHealth::Online);
        assert_eq!(hosts[0].id, runtime::host_id(&app));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A stand-in peer mothership: one route, `/api/status`, answering a fixed body shaped like the
    /// real handler's — just the keys `summary_from_status_json` reads.
    async fn fake_peer(body: Value) -> String {
        let router = Router::new().route(
            "/api/status",
            get(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        url
    }

    fn peer_status_body(id: &str, hostname: &str) -> Value {
        json!({
            "version": "9.9.9",
            "queue_depth": 2,
            "host": {
                "id": id,
                "hostname": hostname,
                "disk_free_bytes": 123456,
                "microvms_live": 1,
                "microvms_ceiling": 4,
            },
            "runtime": {
                "platform": "linux-x86_64",
                "os": {"vendor": "debian", "name": "Debian", "version": "13"},
            },
        })
    }

    #[tokio::test]
    async fn two_hosts_when_one_peer_is_configured_and_reachable() {
        let peer_url = fake_peer(peer_status_body("peer-123", "peer-box")).await;
        let root = temp_root();
        let app = test_app_with(&root, move |cfg| cfg.fleet_peers = vec![peer_url]);
        let hosts = list_hosts(&app).await;
        assert_eq!(hosts.len(), 2, "self plus the one configured peer");
        assert_eq!(hosts[0].health, HostHealth::Online, "self is always online");
        let peer = &hosts[1];
        assert_eq!(peer.health, HostHealth::Online);
        assert_eq!(peer.id, "peer-123");
        assert_eq!(peer.name, "peer-box");
        assert_eq!(peer.platform, "linux-x86_64");
        assert_eq!(peer.os, "Debian");
        assert_eq!(peer.version.as_deref(), Some("9.9.9"));
        assert_eq!(peer.slots_in_use, 1);
        assert_eq!(peer.slots_ceiling, 4);
        assert_eq!(peer.queue_depth, 2);
        assert_eq!(peer.disk_free_bytes, Some(123456));
        assert!(peer.last_heartbeat.is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreachable_peer_never_seen_before_still_appears_as_a_placeholder() {
        // A bound-then-dropped listener: its port is free again, but nothing answers on it, so a
        // connection to it is refused rather than accepted.
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let peer_url = format!("http://127.0.0.1:{port}");

        let root = temp_root();
        let peer_url_clone = peer_url.clone();
        let app = test_app_with(&root, move |cfg| cfg.fleet_peers = vec![peer_url_clone.clone()]);
        let hosts = list_hosts(&app).await;
        assert_eq!(hosts.len(), 2);
        let peer = &hosts[1];
        assert_eq!(peer.health, HostHealth::Unreachable);
        assert_eq!(peer.id, peer_url, "never reached: the url stands in for an id");
        assert_eq!(peer.name, peer_url);
        assert_eq!(peer.slots_in_use, 0);
        assert_eq!(peer.disk_free_bytes, None);
        assert_eq!(peer.last_heartbeat, None, "never once answered");

        // Seed the cache as if this peer had answered before, then poll again: the down peer keeps
        // its last-known numbers instead of being nulled out.
        let prior = HostSummary {
            id: "peer-was-here".into(),
            name: "peer-was-here".into(),
            platform: "linux-x86_64".into(),
            os: "Debian".into(),
            version: Some("9.9.8".into()),
            slots_in_use: 3,
            slots_ceiling: 4,
            queue_depth: 1,
            disk_free_bytes: Some(999),
            last_heartbeat: Some("2026-09-20T12:00:00+00:00".into()),
            health: HostHealth::Online,
        };
        app.fleet_cache
            .last_known
            .lock()
            .await
            .insert(peer_url.clone(), prior.clone());
        let hosts_again = list_hosts(&app).await;
        let peer_again = &hosts_again[1];
        assert_eq!(peer_again.health, HostHealth::Unreachable, "still down");
        assert_eq!(peer_again.id, prior.id, "last-known stats are kept, not nulled out");
        assert_eq!(peer_again.slots_in_use, prior.slots_in_use);
        assert_eq!(peer_again.disk_free_bytes, prior.disk_free_bytes);
        assert_eq!(peer_again.last_heartbeat, prior.last_heartbeat);
        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_root() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-fleet-{}", crate::util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        dir
    }
}
