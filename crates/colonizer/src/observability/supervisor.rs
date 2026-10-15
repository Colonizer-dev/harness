//! The mothership's half of the exporter (#850, #854): it writes the contract
//! (`<data>/observability/exporter.json`), spawns the `colonizer-observability` add-on with the
//! headers on its stdin, restarts it with backoff, stops it when export is switched off, and serves
//! `GET /api/observability/status`, `POST /api/observability/test`, and the history backfill's
//! start/cancel routes (#858).
//!
//! Off by default: with no saved+enabled module and no `COLONIZER_OBSERVABILITY=on`, nothing is
//! written and nothing is spawned. The add-on is a separate binary (docs/design/observability.md):
//! the mothership links none of OpenTelemetry, protobuf or gzip, and a crash on that side cannot
//! take a colony down. Nothing in a colony's path calls into this module.

use super::env::{self, Resolved};
use crate::{ApiResult, App, Shared, client_error, util};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// The contract version this mothership writes (the add-on's `contract::CONTRACT`).
pub const CONTRACT: u32 = 1;
/// The add-on's exit code for a refused contract or version: never restarted.
const EXIT_REFUSED: i32 = 78;
/// The add-on binary's name.
pub const BINARY: &str = "colonizer-observability";
/// The variable naming a locally built add-on binary.
pub const BINARY_ENV: &str = "COLONIZER_OBSERVABILITY_BIN";
/// How often the supervisor re-reads the config and the colony list.
const RECONCILE: Duration = Duration::from_secs(5);
const RESTART_CAP: Duration = Duration::from_secs(60);
/// Running this long without exiting resets the restart backoff.
const HEALTHY: Duration = Duration::from_secs(600);
/// The child's environment is cleared to these, so it never reads the service's own `OTEL_*`.
const CHILD_ENV: [&str; 4] = ["PATH", "HOME", "TZ", "LANG"];

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The effective config for this mothership right now.
pub(crate) async fn resolve(app: &App) -> Resolved {
    let module = app.modules.read().await.observability.clone();
    let secret = util::read_secret(&app.cfg.config_dir.join("observability-headers"));
    let id = env::Identity {
        host_id: crate::runtime::host_id(app),
        host_name: crate::runtime::probe_hostname().await.unwrap_or_default(),
        fleet_role: fleet_role(app).await,
    };
    env::resolve(module.as_ref(), &process_env, secret, &id)
}

/// `owner` when this mothership has members, `member` when it has joined one, empty when it is
/// alone — the same three states `fleet_policy.rs` reads, and the only two it can see.
async fn fleet_role(app: &App) -> String {
    if app.fleet_members.membership().await.is_some() {
        "member".into()
    } else if app.fleet_members.has_members().await {
        "owner".into()
    } else {
        String::new()
    }
}

/// `<data>/observability/exporter.json`.
pub(crate) fn contract_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("exporter.json")
}

fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("status.json")
}

/// The contract for `settings`: install ids, the data dir, and every colony's org, repo,
/// sensitivity and status, and the agent, creation time, origin and pull request its trace's root
/// span carries. Content and thinking are false for every colony: per-colony content
/// export (#848) is not built, and the add-on's content gate stays closed regardless.
pub(crate) async fn contract(app: &App, settings: &Map<String, Value>) -> Value {
    let sessions = app.sessions.read().await;
    let policy: Map<String, Value> = sessions
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                json!({
                    "org": s.org,
                    "repo": s.repo,
                    "sensitivity": s.sensitivity,
                    "content": false,
                    "thinking": false,
                    "status": serde_json::to_value(s.status).unwrap_or(Value::Null),
                    // The colony's trace (#846): its root span's agent, start, launcher and PR.
                    "agent": s.agent,
                    "created_at": s.created_at.to_rfc3339(),
                    "origin": s.origin,
                    "pr_url": s.pr_url,
                }),
            )
        })
        .collect();
    json!({
        "contract": CONTRACT,
        "mothership_version": env!("CARGO_PKG_VERSION"),
        "host_id": crate::runtime::host_id(app),
        "fleet_id": "",
        "data_dir": app.cfg.data_dir,
        "settings": settings,
        "policy": policy,
    })
}

/// Writes a contract file atomically, readable by its owner only.
async fn write_contract(path: &Path, contract: &Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    util::write_atomic(path, &serde_json::to_vec_pretty(contract)?).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Where the add-on binary is: `COLONIZER_OBSERVABILITY_BIN`, then
/// `<data>/addons/observability/<version>/colonizer-observability`, then beside the mothership's
/// own executable.
pub(crate) fn locate(data_dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{BINARY}.exe")
    } else {
        BINARY.to_string()
    };
    let mut candidates = Vec::new();
    if let Some(p) = process_env(BINARY_ENV).filter(|p| !p.is_empty()) {
        candidates.push(PathBuf::from(p));
    }
    candidates.push(
        data_dir
            .join("addons")
            .join("observability")
            .join(env!("CARGO_PKG_VERSION"))
            .join(&name),
    );
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(&name));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// A command for the add-on with the environment cleared to [`CHILD_ENV`].
fn command(binary: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.env_clear();
    for key in CHILD_ENV {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    cmd
}

/// The add-on's `--version --json`, checked against this mothership: the same version and contract.
async fn check_version(binary: &Path) -> Result<String, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        command(binary)
            .args(["--version", "--json"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "the add-on did not answer --version".to_string())?
    .map_err(|e| format!("cannot run {}: {e}", binary.display()))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "the add-on's --version is not JSON".to_string())?;
    let version = v["version"].as_str().unwrap_or("").to_string();
    if v["contract"] != json!(CONTRACT) || version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "the add-on is {version} (contract {}), this mothership is {} (contract {CONTRACT}); install the matching add-on",
            v["contract"],
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(version)
}

/// What the status API reports about the child, beside the add-on's own `status.json`.
#[derive(Clone, Debug, Default)]
struct Runtime {
    state: &'static str,
    error: Option<String>,
    binary: Option<PathBuf>,
    version: Option<String>,
    restarts: u32,
}

/// The supervisor's view, per data dir (one per mothership; tests run several).
fn runtimes() -> &'static Mutex<BTreeMap<PathBuf, Runtime>> {
    static R: OnceLock<Mutex<BTreeMap<PathBuf, Runtime>>> = OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// The backfills in flight, the same one-per-data-dir pattern (`runtimes()` above): one running
/// backfill per install. A slot holding `None` is a backfill between its cancel and its child's
/// exit — a second one may not start beside a child that is still stopping.
fn backfills() -> &'static Mutex<BTreeMap<PathBuf, Option<Child>>> {
    static B: OnceLock<Mutex<BTreeMap<PathBuf, Option<Child>>>> = OnceLock::new();
    B.get_or_init(Mutex::default)
}

fn set_runtime(data_dir: &Path, f: impl FnOnce(&mut Runtime)) {
    if let Ok(mut map) = runtimes().lock() {
        f(map.entry(data_dir.to_path_buf()).or_default());
    }
}

struct Child {
    child: tokio::process::Child,
    /// Held open: the add-on stops when it closes.
    stdin: Option<tokio::process::ChildStdin>,
    started: Instant,
}

impl Child {
    /// Closes stdin (the add-on commits its cursors and exits), then kills it if it lingers.
    async fn stop(mut self) {
        drop(self.stdin.take());
        if tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

/// The supervision loop's state.
#[derive(Default)]
struct Supervisor {
    child: Option<Child>,
    /// A digest of the settings and headers the child was started with.
    fingerprint: Option<String>,
    /// The policy last written, to rewrite the contract only when colonies change.
    policy: Option<Value>,
    failures: u32,
    not_before: Option<Instant>,
    /// The add-on refused this fingerprint (exit 78): wait for the config to change.
    refused: Option<String>,
}

fn fingerprint(e: &env::Effective) -> String {
    let text = format!("{}|{}", Value::Object(e.settings.clone()), e.headers);
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    digest.as_ref().iter().take(12).map(|b| format!("{b:02x}")).collect()
}

impl Supervisor {
    async fn reconcile(&mut self, app: &App) {
        let data_dir = app.cfg.data_dir.clone();
        // A child that exited: note it, and back off before the next start.
        if let Some(child) = self.child.as_mut()
            && let Ok(Some(status)) = child.child.try_wait()
        {
            let ran = child.started.elapsed();
            self.child = None;
            if status.code() == Some(EXIT_REFUSED) {
                self.refused = self.fingerprint.clone();
                set_runtime(&data_dir, |r| {
                    r.state = "refused";
                    r.error = Some("the add-on refused this mothership's contract or version".into());
                });
            } else {
                self.failures = if ran > HEALTHY { 1 } else { self.failures + 1 };
                let delay = Duration::from_secs(1u64 << self.failures.min(6).saturating_sub(1)).min(RESTART_CAP);
                self.not_before = Some(Instant::now() + delay);
                set_runtime(&data_dir, |r| {
                    r.state = "restarting";
                    r.restarts += 1;
                    r.error = Some(format!("the add-on exited ({status}); restarting in {}s", delay.as_secs()));
                });
            }
        }

        // A backfill that finished or failed leaves no stale slot behind to refuse the next one.
        if let Ok(mut jobs) = backfills().lock() {
            let done = jobs
                .get_mut(&data_dir)
                .and_then(|slot| slot.as_mut())
                .is_some_and(|job| job.child.try_wait().is_ok_and(|s| s.is_some()));
            if done {
                jobs.remove(&data_dir);
            }
        }

        let effective = match resolve(app).await {
            Resolved::On(e) => e,
            Resolved::Off(why) => return self.stop(&data_dir, "off", Some(why)).await,
            Resolved::Invalid(why) => return self.stop(&data_dir, "invalid", Some(why)).await,
        };
        let print = fingerprint(&effective);
        let contract = contract(app, &effective.settings).await;

        if self.fingerprint.as_deref() != Some(print.as_str()) {
            self.stop(&data_dir, "starting", None).await;
            self.fingerprint = Some(print.clone());
            self.refused = None;
            self.failures = 0;
            self.not_before = None;
        }
        if self.refused.as_deref() == Some(print.as_str()) {
            return;
        }
        if self.child.is_some() {
            // Running: keep the colony list current; the add-on re-reads it.
            if self.policy.as_ref() != Some(&contract["policy"]) {
                if let Err(e) = write_contract(&contract_path(&data_dir), &contract).await {
                    eprintln!(
                        "observability: could not refresh {}: {e:#}",
                        contract_path(&data_dir).display()
                    );
                }
                self.policy = Some(contract["policy"].clone());
            }
            return;
        }
        if self.not_before.is_some_and(|t| Instant::now() < t) {
            return;
        }

        let Some(binary) = locate(&data_dir) else {
            set_runtime(&data_dir, |r| {
                r.state = "no_addon";
                r.binary = None;
                r.error = Some(format!(
                    "the {BINARY} add-on is not installed: build it (`cargo build --release -p colonizer-observability`) \
                     and put it beside the colonizer binary, or name it with {BINARY_ENV}"
                ));
            });
            return;
        };
        let version = match check_version(&binary).await {
            Ok(v) => v,
            Err(e) => {
                self.refused = Some(print);
                set_runtime(&data_dir, |r| {
                    r.state = "refused";
                    r.binary = Some(binary.clone());
                    r.error = Some(e);
                });
                return;
            }
        };
        if let Err(e) = write_contract(&contract_path(&data_dir), &contract).await {
            set_runtime(&data_dir, |r| {
                r.state = "failed";
                r.error = Some(format!("could not write the contract: {e:#}"));
            });
            return;
        }
        self.policy = Some(contract["policy"].clone());
        match spawn(&binary, &data_dir, &effective.headers).await {
            Ok(child) => {
                self.child = Some(child);
                set_runtime(&data_dir, |r| {
                    r.state = "running";
                    r.error = None;
                    r.binary = Some(binary);
                    r.version = Some(version);
                });
            }
            Err(e) => {
                self.failures += 1;
                self.not_before = Some(Instant::now() + RESTART_CAP.min(Duration::from_secs(1 << self.failures.min(6))));
                set_runtime(&data_dir, |r| {
                    r.state = "failed";
                    r.error = Some(e);
                });
            }
        }
    }

    async fn stop(&mut self, data_dir: &Path, state: &'static str, why: Option<String>) {
        if let Some(child) = self.child.take() {
            child.stop().await;
        }
        if state != "starting" {
            self.fingerprint = None;
            self.policy = None;
        }
        set_runtime(data_dir, |r| {
            r.state = state;
            r.error = why;
        });
    }
}

/// Starts `run`, writes the secrets line, and forwards the add-on's stderr to ours.
async fn spawn(binary: &Path, data_dir: &Path, headers: &str) -> Result<Child, String> {
    let mut cmd = command(binary);
    cmd.arg("run")
        .arg("--contract")
        .arg(contract_path(data_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
    let mut stdin = child.stdin.take();
    if let Some(pipe) = stdin.as_mut() {
        let line = format!("{}\n", json!({"headers": headers}));
        pipe.write_all(line.as_bytes())
            .await
            .map_err(|e| format!("cannot hand the add-on its headers: {e}"))?;
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // The add-on never prints a header value; redact anyway, as every log line is.
                eprintln!("{}", crate::redact::redact_text(&line));
            }
        });
    }
    Ok(Child {
        child,
        stdin,
        started: Instant::now(),
    })
}

/// The supervision loop, started once by `server::start_tasks`.
pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut supervisor = Supervisor::default();
        loop {
            supervisor.reconcile(&app).await;
            tokio::time::sleep(RECONCILE).await;
        }
    });
}

/// `GET /api/observability/status`: whether export is on, why not, where each setting came from,
/// the header *names*, the add-on's own health, and any backfill's progress. Never a header value.
async fn status(State(app): State<Shared>) -> ApiResult<Value> {
    let data_dir = app.cfg.data_dir.clone();
    let runtime = runtimes()
        .lock()
        .ok()
        .and_then(|m| m.get(&data_dir).cloned())
        .unwrap_or_default();
    let exporter: Value = std::fs::read(status_path(&data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    // The backfill's own progress file, the same read: absent until one has run.
    let backfill: Value = std::fs::read(data_dir.join("observability").join("backfill.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let addon = locate(&data_dir);
    let mut out = json!({
        "state": if runtime.state.is_empty() { "off" } else { runtime.state },
        "error": runtime.error,
        "restarts": runtime.restarts,
        "addon": {"path": addon.or(runtime.binary), "version": runtime.version},
        "exporter": exporter,
        "backfill": backfill,
    });
    match resolve(&app).await {
        Resolved::On(e) => {
            out["configured"] = json!(true);
            out["endpoint"] = json!(e.str("endpoint"));
            out["protocol"] = json!(e.str("protocol"));
            out["service_name"] = json!(e.str("service_name"));
            out["provenance"] = json!(e.provenance);
            out["headers"] = json!({
                "source": e.headers_source,
                "names": e.parsed_headers.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>(),
            });
        }
        Resolved::Off(why) | Resolved::Invalid(why) => {
            out["configured"] = json!(false);
            out["reason"] = json!(why);
        }
    }
    Ok(Json(out))
}

/// `POST /api/observability/test`: one test log record and one metric point to the configured
/// backend, through the add-on's `send-test-event`, and what the backend answered.
async fn test(State(app): State<Shared>) -> ApiResult<Value> {
    let effective = match resolve(&app).await {
        Resolved::On(e) => e,
        Resolved::Off(why) | Resolved::Invalid(why) => return Err(client_error(StatusCode::CONFLICT, &why)),
    };
    let data_dir = app.cfg.data_dir.clone();
    let Some(binary) = locate(&data_dir) else {
        return Err(client_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!(
                "the {BINARY} add-on is not installed; build it and put it beside the colonizer binary, or name it with {BINARY_ENV}"
            ),
        ));
    };
    check_version(&binary)
        .await
        .map_err(|e| client_error(StatusCode::SERVICE_UNAVAILABLE, &e))?;
    let path = data_dir.join("observability").join(format!("test-{}.json", util::short_id()));
    let contract = contract(&app, &effective.settings).await;
    write_contract(&path, &contract).await?;
    let timeout = effective.settings.get("timeout_secs").and_then(Value::as_u64).unwrap_or(10) * 2 + 5;
    let result = run_test(&binary, &path, &effective.headers, Duration::from_secs(timeout)).await;
    let _ = tokio::fs::remove_file(&path).await;
    result.map(Json).map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))
}

async fn run_test(binary: &Path, contract: &Path, headers: &str, timeout: Duration) -> Result<Value, String> {
    let mut child = command(binary)
        .arg("send-test-event")
        .arg("--contract")
        .arg(contract)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let line = format!("{}\n", json!({"headers": headers}));
        let _ = stdin.write_all(line.as_bytes()).await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| "the test event timed out".to_string())?
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&out.stdout).map_err(|_| "the add-on gave no result".to_string())
}

/// The `POST /api/observability/backfill` body: the window and the limits, passed through to the
/// add-on verbatim — it parses the dates and reports a bad one itself (`state: "failed"` with an
/// `error` in its progress file), so nothing is validated here.
#[derive(Deserialize)]
struct BackfillRequest {
    /// Only records written at or after this: RFC3339, `YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD`.
    since: Option<String>,
    /// Only records written before this, the same forms (a bare date means its end).
    until: Option<String>,
    /// Only colonies of this repository, `owner/name`.
    repo: Option<String>,
    /// Send at most this many records per second.
    rate: Option<u64>,
    #[serde(default)]
    dry_run: bool,
}

/// `backfill --contract …` with the request's flags, each only when it was given.
fn backfill_command(binary: &Path, contract: &Path, body: &BackfillRequest) -> tokio::process::Command {
    let mut cmd = command(binary);
    cmd.arg("backfill").arg("--contract").arg(contract);
    if let Some(v) = &body.since {
        cmd.arg("--since").arg(v);
    }
    if let Some(v) = &body.until {
        cmd.arg("--until").arg(v);
    }
    if let Some(v) = &body.repo {
        cmd.arg("--repo").arg(v);
    }
    if let Some(v) = body.rate {
        cmd.arg("--rate").arg(v.to_string());
    }
    if body.dry_run {
        cmd.arg("--dry-run");
    }
    cmd
}

/// A dry run, synchronously like [`run_test`]: counts only, sends nothing, writes no state — so
/// it registers no job — and answers the add-on's summary JSON from its stdout.
async fn run_backfill(
    binary: &Path,
    contract: &Path,
    headers: &str,
    body: &BackfillRequest,
    timeout: Duration,
) -> Result<Value, String> {
    let mut child = backfill_command(binary, contract, body)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let line = format!("{}\n", json!({"headers": headers}));
        let _ = stdin.write_all(line.as_bytes()).await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| "the dry run timed out".to_string())?
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&out.stdout).map_err(|_| "the add-on gave no result".to_string())
}

/// The real backfill: a long-running child like [`spawn`] — the headers line on a stdin held open
/// (closing it is the cancel signal), no stdout, stderr forwarded redacted.
async fn spawn_backfill(binary: &Path, contract: &Path, headers: &str, body: &BackfillRequest) -> Result<Child, String> {
    let mut cmd = backfill_command(binary, contract, body);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", binary.display()))?;
    let mut stdin = child.stdin.take();
    if let Some(pipe) = stdin.as_mut() {
        let line = format!("{}\n", json!({"headers": headers}));
        pipe.write_all(line.as_bytes())
            .await
            .map_err(|e| format!("cannot hand the add-on its headers: {e}"))?;
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // The add-on never prints a header value; redact anyway, as every log line is.
                eprintln!("{}", crate::redact::redact_text(&line));
            }
        });
    }
    Ok(Child {
        child,
        stdin,
        started: Instant::now(),
    })
}

/// `POST /api/observability/backfill` (#858): replay the history the live exporter never sent.
/// A dry run runs the one-shot count synchronously and answers its summary; otherwise the child
/// runs beside the exporter, at a bounded rate, and its progress — written by the add-on to
/// `<data>/observability/backfill.json` — is carried by `GET /api/observability/status`.
async fn backfill_start(State(app): State<Shared>, Json(body): Json<BackfillRequest>) -> ApiResult<Value> {
    let effective = match resolve(&app).await {
        Resolved::On(e) => e,
        Resolved::Off(why) | Resolved::Invalid(why) => return Err(client_error(StatusCode::CONFLICT, &why)),
    };
    let data_dir = app.cfg.data_dir.clone();
    let Some(binary) = locate(&data_dir) else {
        return Err(client_error(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!(
                "the {BINARY} add-on is not installed; build it and put it beside the colonizer binary, or name it with {BINARY_ENV}"
            ),
        ));
    };
    check_version(&binary)
        .await
        .map_err(|e| client_error(StatusCode::SERVICE_UNAVAILABLE, &e))?;
    let path = contract_path(&data_dir);
    write_contract(&path, &contract(&app, &effective.settings).await).await?;
    if body.dry_run {
        // A dry run is a count: it sends nothing and writes no state, so it registers no job.
        let answer = run_backfill(&binary, &path, &effective.headers, &body, Duration::from_secs(600))
            .await
            .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))?;
        if answer["ok"].as_bool() == Some(false) {
            let why = answer["error"].as_str().unwrap_or("the dry run failed");
            return Err(client_error(StatusCode::BAD_GATEWAY, why));
        }
        return Ok(Json(answer));
    }
    // One backfill per install: hold the slot before the child starts, so two posts cannot start
    // two children; a job whose child has already exited (reconcile reaps it within a tick) does
    // not refuse a restart.
    {
        let Ok(mut jobs) = backfills().lock() else {
            return Err(client_error(StatusCode::CONFLICT, "a backfill is already running"));
        };
        let finished = jobs
            .get_mut(&data_dir)
            .and_then(|slot| slot.as_mut())
            .is_some_and(|job| job.child.try_wait().is_ok_and(|s| s.is_some()));
        if finished {
            jobs.remove(&data_dir);
        }
        if jobs.contains_key(&data_dir) {
            return Err(client_error(StatusCode::CONFLICT, "a backfill is already running"));
        }
        jobs.insert(data_dir.clone(), None);
    }
    match spawn_backfill(&binary, &path, &effective.headers, &body).await {
        Ok(job) => {
            if let Ok(mut jobs) = backfills().lock() {
                jobs.insert(data_dir.clone(), Some(job));
            }
        }
        Err(e) => {
            if let Ok(mut jobs) = backfills().lock() {
                jobs.remove(&data_dir);
            }
            return Err(client_error(StatusCode::BAD_GATEWAY, &e));
        }
    }
    Ok(Json(json!({"started": true, "progress": "GET /api/observability/status"})))
}

/// `DELETE /api/observability/backfill`: close the child's stdin — it finishes its current tick,
/// writes `state: "cancelled"` to its progress file and exits — and kill it if 15 seconds is not
/// enough. The wait runs as its own task so the slot is freed even if this request goes away
/// mid-wait.
async fn backfill_cancel(State(app): State<Shared>) -> ApiResult<Value> {
    let data_dir = app.cfg.data_dir.clone();
    let job = backfills()
        .lock()
        .ok()
        .and_then(|mut jobs| jobs.get_mut(&data_dir).and_then(|slot| slot.take()));
    let Some(job) = job else {
        return Err(client_error(StatusCode::CONFLICT, "no backfill is running"));
    };
    let done = tokio::spawn(async move {
        let mut job = job;
        drop(job.stdin.take());
        if tokio::time::timeout(Duration::from_secs(15), job.child.wait()).await.is_err() {
            let _ = job.child.kill().await;
        }
    });
    let _ = done.await;
    if let Ok(mut jobs) = backfills().lock() {
        jobs.remove(&data_dir);
    }
    Ok(Json(json!({"cancelled": true})))
}

pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/observability/status", routing::get(status))
        .route("/api/observability/test", routing::post(test))
        .route(
            "/api/observability/backfill",
            routing::post(backfill_start).delete(backfill_cancel),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_state(data_dir: &Path) -> &'static str {
        runtimes().lock().unwrap().get(data_dir).map(|r| r.state).unwrap_or("")
    }

    #[tokio::test]
    async fn off_by_default_writes_nothing_and_spawns_nothing() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        let mut supervisor = Supervisor::default();
        supervisor.reconcile(&app).await;
        assert!(supervisor.child.is_none());
        assert!(!contract_path(&app.cfg.data_dir).exists(), "no contract while export is off");
        assert!(
            !app.cfg.data_dir.join("observability").exists(),
            "nothing at all under observability/"
        );
        assert_eq!(runtime_state(&app.cfg.data_dir), "off");
        let Json(status) = status(State(app.clone())).await.unwrap();
        assert_eq!(status["configured"], json!(false));
        let err = test(State(app.clone())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT, "nothing to test while off");
    }

    #[tokio::test]
    async fn an_enabled_module_without_the_add_on_says_so_and_never_leaks_headers() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        let secret = format!("canary-{}", util::short_id());
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        util::write_private(
            &app.cfg.config_dir.join("observability-headers"),
            format!("x-honeycomb-team={secret}").as_bytes(),
        )
        .unwrap();
        app.modules.write().await.observability = Some(crate::config::ModuleChoice {
            provider: "otlp".into(),
            enabled: true,
            settings: serde_json::from_value(json!({"endpoint": "http://127.0.0.1:4318"})).unwrap(),
        });
        let mut supervisor = Supervisor::default();
        supervisor.reconcile(&app).await;
        // The test runs from target/<profile>/deps, where no add-on binary sits.
        if locate(&app.cfg.data_dir).is_none() {
            assert_eq!(runtime_state(&app.cfg.data_dir), "no_addon");
        }
        let Json(status) = status(State(app.clone())).await.unwrap();
        assert_eq!(status["configured"], json!(true));
        assert_eq!(status["headers"]["names"], json!(["x-honeycomb-team"]));
        assert!(!status.to_string().contains(&secret), "{status}");

        let contract = contract(&app, &Map::new()).await;
        assert!(!contract.to_string().contains(&secret), "headers never go in the contract");
        assert_eq!(contract["contract"], json!(CONTRACT));
    }

    #[tokio::test]
    async fn backfill_routes_refuse_an_unconfigured_backend_and_status_carries_the_slot() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        // No backfill has ever run: the status slot is there, and null.
        let Json(status) = status(State(app.clone())).await.unwrap();
        assert_eq!(status["backfill"], json!(null), "{status}");
        // Export off: a dry run is refused like the test event, and nothing is registered.
        let err = backfill_start(
            State(app.clone()),
            Json(BackfillRequest {
                since: None,
                until: None,
                repo: None,
                rate: None,
                dry_run: true,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT, "nothing to backfill while export is off");
        // And there is nothing to cancel.
        let err = backfill_cancel(State(app.clone())).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT, "no backfill is running");
        assert!(
            backfills().lock().unwrap().get(&app.cfg.data_dir).is_none(),
            "no job was registered"
        );
    }
}
