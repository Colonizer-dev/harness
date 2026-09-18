//! The live map (docs/telemetry.md): a mothership whose user switched it on sends a small heartbeat to
//! Colonizer's telemetry service every few minutes, and https://colonizer.dev/live shows a dot for its area,
//! about 25 km across, that lights up while colonies run. It is off until the user switches it on; the web
//! UI asks once. `Heartbeat` below is everything that is sent. The receiving end is services/telemetry.

use crate::{Shared, util};
use anyhow::{Context, Result, bail};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify};

pub const DEFAULT_ENDPOINT: &str = "https://telemetry.colonizer.dev";
pub const MAP_URL: &str = "https://colonizer.dev/live";

/// How often the loop looks at the colony count; a change is sent then, instead of waiting for the interval.
const CHECK_EVERY: Duration = Duration::from_secs(60);
/// The heartbeat interval until the service names one, and the bounds on what it may name.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const MAX_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// The service counts at most this many colonies per mothership.
const MAX_COLONIES: usize = 64;

/// The user's answer, kept in `<config>/telemetry.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// `None` until the user has answered.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Random, created when the live map is switched on and forgotten when it is switched off, so the service
    /// cannot tell that two periods on the map were the same mothership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
}

impl Choice {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        util::write_private(path, &serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// Everything a heartbeat carries. No repository, issue, user name, path or IP address: the service sees the
/// connecting IP like any web server, turns it into a 25 km cell and keeps only the cell.
#[derive(Debug, Serialize, PartialEq)]
pub struct Heartbeat<'a> {
    pub install_id: &'a str,
    pub version: &'static str,
    pub platform: &'static str,
    /// Colonies with a running microVM.
    pub colonies: usize,
}

pub fn platform() -> &'static str {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("linux", "x86_64") => "linux-x86_64",
        ("macos", "aarch64") => "darwin-arm64",
        _ => "other",
    }
}

/// `DO_NOT_TRACK` (https://consoledonottrack.com) or `COLONIZER_TELEMETRY=off` keep the live map off whatever
/// Settings says, for machines where nobody should have to remember to answer.
fn blocked_by(
    do_not_track: Option<&str>,
    colonizer_telemetry: Option<&str>,
) -> Option<&'static str> {
    if do_not_track.is_some_and(|v| !matches!(v.trim(), "" | "0" | "false")) {
        return Some("DO_NOT_TRACK");
    }
    if colonizer_telemetry.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    }) {
        return Some("COLONIZER_TELEMETRY");
    }
    None
}

fn interval_from(next_in: Option<u64>) -> Duration {
    next_in.map_or(DEFAULT_INTERVAL, |s| {
        Duration::from_secs(s).clamp(MIN_INTERVAL, MAX_INTERVAL)
    })
}

#[derive(Clone, Debug, Default)]
struct Report {
    last_sent_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

pub struct Telemetry {
    path: PathBuf,
    endpoint: String,
    blocked: Option<&'static str>,
    client: reqwest::Client,
    choice: Mutex<Choice>,
    report: Mutex<Report>,
    /// Wakes the heartbeat loop when the switch changes.
    wake: Notify,
}

impl Telemetry {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let blocked = blocked_by(
            std::env::var("DO_NOT_TRACK").ok().as_deref(),
            std::env::var("COLONIZER_TELEMETRY").ok().as_deref(),
        );
        let endpoint = util::env_nonempty("COLONIZER_TELEMETRY_URL")
            .unwrap_or_else(|| DEFAULT_ENDPOINT.into());
        Self::with(config_dir.join("telemetry.json"), endpoint, blocked)
    }

    fn with(path: PathBuf, endpoint: String, blocked: Option<&'static str>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            choice: Mutex::new(Choice::load(&path)),
            path,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            blocked,
            client,
            report: Mutex::new(Report::default()),
            wake: Notify::new(),
        })
    }

    /// The install id to send heartbeats with, when the live map is on.
    async fn active_id(&self) -> Option<String> {
        if self.blocked.is_some() {
            return None;
        }
        let choice = self.choice.lock().await;
        if choice.enabled == Some(true) {
            choice.install_id.clone()
        } else {
            None
        }
    }

    /// Switches the live map on or off and saves the answer. Switching off takes the mothership off the map
    /// at once and forgets its install id.
    async fn set(&self, enabled: bool) -> Result<()> {
        if let Some(variable) = self.blocked {
            bail!("the live map is kept off by {variable} in the mothership's environment");
        }
        let forgotten = {
            let mut choice = self.choice.lock().await;
            let forgotten = if enabled {
                None
            } else {
                choice.install_id.take()
            };
            if enabled && choice.install_id.is_none() {
                choice.install_id = Some(uuid::Uuid::new_v4().to_string());
            }
            choice.enabled = Some(enabled);
            choice.save(&self.path)?;
            forgotten
        };
        if let Some(id) = forgotten
            && let Err(e) = self.send_off(&id).await
        {
            // The service forgets it anyway once its heartbeats stop: off the map within 12 minutes,
            // deleted within the hour.
            self.report.lock().await.last_error = Some(format!("{e:#}"));
        }
        self.wake.notify_one();
        Ok(())
    }

    async fn post(&self, body: &Value) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/v1/heartbeat", self.endpoint))
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let reply: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "the telemetry service answered {status}: {}",
                reply["error"].as_str().unwrap_or("no reason given")
            );
        }
        Ok(reply)
    }

    /// Sends one heartbeat and returns how long to wait before the next.
    async fn send(&self, beat: &Heartbeat<'_>) -> Result<Duration> {
        let reply = self.post(&serde_json::to_value(beat)?).await?;
        Ok(interval_from(reply["next_in"].as_u64()))
    }

    async fn send_off(&self, install_id: &str) -> Result<()> {
        self.post(&json!({"install_id": install_id, "online": false}))
            .await
            .map(drop)
    }

    /// On shutdown: off the map now rather than when the heartbeats time out. The id is kept, so the next start
    /// is the same dot again.
    pub async fn goodbye(&self) {
        if let Some(id) = self.active_id().await {
            let _ = tokio::time::timeout(Duration::from_secs(3), self.send_off(&id)).await;
        }
    }
}

async fn live_colonies(app: &Shared) -> usize {
    app.sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status.is_live())
        .count()
        .min(MAX_COLONIES)
}

/// The heartbeat loop: every interval while the live map is on, within a minute when the colony count changes,
/// and at once when the switch is flipped.
pub async fn run(app: Shared) {
    let telemetry = &app.telemetry;
    let mut interval = DEFAULT_INTERVAL;
    // When the last heartbeat was attempted, and with how many colonies.
    let mut last: Option<(Instant, usize)> = None;
    loop {
        match telemetry.active_id().await {
            None => last = None,
            Some(id) => {
                let colonies = live_colonies(&app).await;
                let due =
                    last.is_none_or(|(at, sent)| at.elapsed() >= interval || sent != colonies);
                if due {
                    let beat = Heartbeat {
                        install_id: &id,
                        version: env!("CARGO_PKG_VERSION"),
                        platform: platform(),
                        colonies,
                    };
                    let result = telemetry.send(&beat).await;
                    let mut report = telemetry.report.lock().await;
                    match result {
                        Ok(next) => {
                            interval = next;
                            *report = Report {
                                last_sent_at: Some(Utc::now()),
                                last_error: None,
                            };
                        }
                        Err(e) => report.last_error = Some(format!("{e:#}")),
                    }
                    last = Some((Instant::now(), colonies));
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(CHECK_EVERY) => {}
            _ = telemetry.wake.notified() => last = None,
        }
    }
}

async fn view(app: &Shared) -> Value {
    let telemetry = &app.telemetry;
    let choice = telemetry.choice.lock().await.clone();
    let report = telemetry.report.lock().await.clone();
    json!({
        "enabled": if telemetry.blocked.is_some() { Some(false) } else { choice.enabled },
        "blocked_by": telemetry.blocked,
        "endpoint": telemetry.endpoint,
        "map_url": MAP_URL,
        "last_sent_at": report.last_sent_at,
        "last_error": report.last_error,
        // Exactly what the next heartbeat will be. The id is null until the live map is first switched on.
        "heartbeat": {
            "install_id": choice.install_id,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": platform(),
            "colonies": live_colonies(app).await,
        },
    })
}

/// `GET /api/telemetry`
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/telemetry` — `{"enabled": true|false}`
pub async fn put(
    State(app): State<Shared>,
    Json(body): Json<SetRequest>,
) -> crate::ApiResult<Value> {
    if app.telemetry.blocked.is_some() {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "the live map is kept off by the mothership's environment",
        ));
    }
    app.telemetry.set(body.enabled).await?;
    Ok(Json(view(&app).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::post};
    use std::sync::Arc;

    #[test]
    fn platforms_are_the_released_ones_or_other() {
        assert_eq!(platform_for("linux", "x86_64"), "linux-x86_64");
        assert_eq!(platform_for("macos", "aarch64"), "darwin-arm64");
        assert_eq!(platform_for("linux", "aarch64"), "other");
        assert_eq!(platform_for("macos", "x86_64"), "other");
    }

    #[test]
    fn a_heartbeat_is_exactly_four_fields() {
        let beat = Heartbeat {
            install_id: "0b0c9a8e-4f7d-4a51-9b2e-3c1d5e6f7a8b",
            version: "0.1.3",
            platform: "darwin-arm64",
            colonies: 2,
        };
        let value = serde_json::to_value(&beat).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["colonies", "install_id", "platform", "version"]);
    }

    #[test]
    fn the_environment_can_keep_it_off() {
        assert_eq!(blocked_by(None, None), None);
        assert_eq!(blocked_by(Some("1"), None), Some("DO_NOT_TRACK"));
        assert_eq!(blocked_by(Some("true"), None), Some("DO_NOT_TRACK"));
        assert_eq!(blocked_by(Some("0"), None), None);
        assert_eq!(blocked_by(Some(""), None), None);
        assert_eq!(blocked_by(None, Some("off")), Some("COLONIZER_TELEMETRY"));
        assert_eq!(blocked_by(None, Some("OFF")), Some("COLONIZER_TELEMETRY"));
        assert_eq!(blocked_by(None, Some("on")), None);
    }

    #[test]
    fn the_service_cannot_make_heartbeats_too_frequent_or_too_rare() {
        assert_eq!(interval_from(None), DEFAULT_INTERVAL);
        assert_eq!(interval_from(Some(300)), Duration::from_secs(300));
        assert_eq!(interval_from(Some(1)), MIN_INTERVAL);
        assert_eq!(interval_from(Some(86_400)), MAX_INTERVAL);
    }

    #[test]
    fn a_missing_or_broken_file_means_not_asked_yet() {
        let dir =
            std::env::temp_dir().join(format!("colonizer-telemetry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("telemetry.json");
        assert_eq!(Choice::load(&path), Choice::default());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Choice::load(&path).enabled, None);
        let saved = Choice {
            enabled: Some(true),
            install_id: Some("x".into()),
        };
        saved.save(&path).unwrap();
        assert_eq!(Choice::load(&path), saved);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A stand-in telemetry service that records what it receives.
    async fn receiver() -> (String, Arc<Mutex<Vec<Value>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let log = received.clone();
        let router = Router::new().route(
            "/v1/heartbeat",
            post(move |Json(body): Json<Value>| {
                let log = log.clone();
                async move {
                    log.lock().await.push(body);
                    Json(json!({"ok": true, "next_in": 300}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, received)
    }

    #[tokio::test]
    async fn switching_on_and_off_takes_the_mothership_off_the_map_and_forgets_its_id() {
        let (url, received) = receiver().await;
        let dir =
            std::env::temp_dir().join(format!("colonizer-telemetry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("telemetry.json");
        let telemetry = Telemetry::with(path.clone(), url, None).unwrap();
        assert_eq!(
            telemetry.active_id().await,
            None,
            "off until the user answers"
        );

        telemetry.set(true).await.unwrap();
        let id = telemetry.active_id().await.expect("an id once switched on");
        assert!(uuid::Uuid::parse_str(&id).is_ok_and(|u| u.get_version_num() == 4));
        let beat = Heartbeat {
            install_id: &id,
            version: "0.1.3",
            platform: "darwin-arm64",
            colonies: 1,
        };
        assert_eq!(
            telemetry.send(&beat).await.unwrap(),
            Duration::from_secs(300)
        );

        telemetry.set(false).await.unwrap();
        assert_eq!(telemetry.active_id().await, None);
        assert_eq!(
            Choice::load(&path),
            Choice {
                enabled: Some(false),
                install_id: None
            }
        );
        let bodies = received.lock().await.clone();
        assert_eq!(
            bodies[0],
            json!({"install_id": id, "version": "0.1.3", "platform": "darwin-arm64", "colonies": 1})
        );
        assert_eq!(bodies[1], json!({"install_id": id, "online": false}));

        telemetry.set(true).await.unwrap();
        assert_ne!(
            telemetry.active_id().await.unwrap(),
            id,
            "a new period on the map gets a new id"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_environment_overrides_the_saved_answer() {
        let dir =
            std::env::temp_dir().join(format!("colonizer-telemetry-{}", uuid::Uuid::new_v4()));
        let path = dir.join("telemetry.json");
        Choice {
            enabled: Some(true),
            install_id: Some(uuid::Uuid::new_v4().to_string()),
        }
        .save(&path)
        .unwrap();
        let telemetry =
            Telemetry::with(path, "http://127.0.0.1:9".into(), Some("DO_NOT_TRACK")).unwrap();
        assert_eq!(telemetry.active_id().await, None);
        assert!(telemetry.set(true).await.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
