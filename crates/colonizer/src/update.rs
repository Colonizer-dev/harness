//! Applying an update without losing colonies (#45).
//!
//! The check lives in `version.rs`; this is what happens when the operator asks
//! for the newer release to be installed.
//!
//! It runs the same installer a person would run — `scripts/install-release.sh`,
//! shipped inside the app — rather than reimplementing the download, the
//! checksum and the swap. That installer already unpacks beside the running app
//! and moves a symlink with one rename, so a failure part-way leaves the running
//! version exactly as it was.
//!
//! Two things make it safe for running colonies:
//!
//! * `COLONIZER_KEEP_PREVIOUS=1`. Colonies mount vendored plugins straight out
//!   of the slot the mothership was started from, so the installer must not take
//!   that slot away while they are reading it. It is cleaned up later, by
//!   [`sweep_slots`], once no live colony still points at it.
//! * Nothing is applied while a colony is publishing. The microVM is already
//!   gone at that point and the host is committing and pushing; interrupting it
//!   leaves the colony `failed` with its pull request unopened.
//!
//! The restart is an `exec` of the app symlink's binary, which now points at the
//! new slot. Colonies are detached microVMs, so `sessions::recover` reconnects
//! to each one and carries its event stream on from the sequence number it had.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use axum::{Json, body::Bytes, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{process::Command, sync::Mutex};

use crate::{
    Shared, auth,
    config::Settings,
    sessions::{Session, SessionStatus},
    util, version,
};

/// How long the installer gets before it is given up on. A slow connection
/// downloading a release is normal; twenty minutes of it is not.
const INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Installing,
    /// Installed; the process is about to be replaced by the new one.
    Restarting,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub version: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    /// The installer's own output, so a failure can be read without finding a log.
    pub log: String,
    /// What each colony was doing when the update was applied.
    pub colonies: Vec<ColonyNote>,
    /// Where `sessions.json` was backed up before the install, when there was one to back up.
    pub backup: Option<String>,
}

/// What happened to one colony, reported per the issue's last acceptance line.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ColonyNote {
    pub id: String,
    pub repo: String,
    pub outcome: String,
}

#[derive(Default)]
pub struct Updater {
    progress: Mutex<Progress>,
}

impl Updater {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn progress(&self) -> Progress {
        self.progress.lock().await.clone()
    }
}

/* ------------------------------------------------------------------ the app */

/// The symlink the installer maintains: `~/.local/share/colonizer/app`, or
/// wherever `COLONIZER_APP` moved it.
///
/// Not the same as the assets directory. `resolve_assets` canonicalises, so a
/// running mothership holds the *slot* it started from; this is the name that
/// follows the newest install.
pub fn app_link() -> Option<PathBuf> {
    if let Some(app) = util::env_nonempty("COLONIZER_APP") {
        return Some(PathBuf::from(app));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/colonizer/app"))
}

/// Why an update cannot be applied from here, if it cannot.
///
/// A source checkout has no installer and no symlink, and should keep using
/// `scripts/install.sh`; saying so is better than half-applying something.
pub fn blocker(assets: Option<&Path>) -> Option<String> {
    let Some(assets) = assets else {
        return Some("this mothership is running without an installed app directory".into());
    };
    if !installer(assets).is_file() {
        return Some("this install has no scripts/install-release.sh, so it was not installed from a release".into());
    }
    match app_link() {
        Some(app) if app.is_symlink() => None,
        Some(app) => Some(format!("{} is not the symlink the installer maintains", app.display())),
        None => Some("HOME is not set, so the app directory cannot be found".into()),
    }
}

/// The one refusal decision, shared by the route and `colonizer update` alike:
/// why this build must not be replaced by `latest`, if it must not.
///
/// A development build — commits after its last tag, a modified tree, or no tag
/// at all — holds work the newest release does not, however much newer that
/// release's number is. And a release newer than the latest one would be a
/// downgrade, not an update. Both refuse unless `force` says the operator has
/// read the reason and means it anyway; anything else is the caller's
/// newer-or-nothing decision to make, not a refusal.
///
/// The refusal names both versions, so either side can print it as is: the
/// route only has the stamped build, and the command only has the JSON.
pub fn refusal(build: &version::Build, latest: Option<&str>, force: bool) -> Option<String> {
    if force {
        return None;
    }
    if build.development {
        return Some(dev_refusal(&build.version, build.release.as_deref(), latest));
    }
    let running = build.release.as_deref().and_then(version::Semver::parse);
    let newest = latest.and_then(version::Semver::parse);
    match (running, newest, latest) {
        (Some(running), Some(newest), Some(latest)) if running > newest => Some(format!(
            "running `{}`, newer than the latest release `{latest}` — refusing to downgrade; pass `--force` to install `{latest}` anyway",
            build.version
        )),
        _ => None,
    }
}

/// How many commits a `git describe` build sits ahead of its release, if it says so.
///
/// `v0.1.5-60-gd62bfb2` is 60 commits ahead of `v0.1.5`; anything that does not
/// start with the tag is not counted rather than guessed at.
fn commits_ahead(version: &str, release: &str) -> Option<u64> {
    version
        .strip_prefix(release)?
        .strip_prefix('-')?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn dev_refusal(version: &str, release: Option<&str>, latest: Option<&str>) -> String {
    let mut running = "development build".to_string();
    if let Some(release) = release
        && let Some(ahead) = commits_ahead(version, release)
    {
        running.push_str(&format!(", {ahead} commits ahead of release `{release}`"));
    }
    let known = latest
        .map(|latest| format!(" and the latest release is `{latest}`"))
        .unwrap_or_default();
    let force = latest
        .map(|latest| format!(", or pass `--force` to install `{latest}` anyway"))
        .unwrap_or_default();
    format!(
        "running `{version}` ({running}){known} — refusing to replace a source build with a release; update it from its checkout with `git pull && scripts/install.sh --install`{force}"
    )
}

fn installer(assets: &Path) -> PathBuf {
    assets.join("scripts/install-release.sh")
}

/// Whether a colony in this state must finish before an update is applied.
///
/// Only publishing. A colony that is merely working is detached, and reconnects
/// after the restart with its transcript intact; waiting for one would mean
/// never being able to update while anything is running.
pub fn holds_update(status: SessionStatus) -> bool {
    status == SessionStatus::Publishing
}

/// Whether a colony in this state will be reconnected by `recover` afterwards.
pub fn will_reconnect(status: SessionStatus) -> bool {
    status.is_live()
}

pub fn publishing(sessions: &[Session]) -> Vec<&Session> {
    sessions.iter().filter(|s| holds_update(s.status)).collect()
}

/// What each colony will experience, decided before anything is installed.
pub fn notes(sessions: &[Session]) -> Vec<ColonyNote> {
    sessions
        .iter()
        .filter(|s| will_reconnect(s.status) || holds_update(s.status))
        .map(|s| ColonyNote {
            id: s.id.clone(),
            repo: s.repo.clone(),
            outcome: match s.status {
                // Should never reach here: publishing blocks the update.
                SessionStatus::Publishing => "was publishing".into(),
                _ => "reconnected after the restart".into(),
            },
        })
        .collect()
}

/* --------------------------------------------------------------- the sweep */

/// Removes app slots that nothing is using any more.
///
/// An update keeps the slot it replaced, because colonies boot with plugins
/// mounted out of it. This runs at start, once `recover` has settled, and takes
/// away any slot that is neither the one this process is running from nor
/// referenced by a live colony.
pub fn sweep_slots(assets: Option<&Path>, live_slots: &[PathBuf]) -> Vec<PathBuf> {
    let Some(current) = assets else { return Vec::new() };
    let Some(app) = app_link() else { return Vec::new() };
    let Some(dir) = app.parent() else { return Vec::new() };
    let Some(stem) = app.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };

    let mut removed = Vec::new();
    for slot in ["a", "b"] {
        let candidate = dir.join(format!("{stem}-{slot}"));
        if !candidate.is_dir() {
            continue;
        }
        let same = |other: &Path| {
            // Compare resolved paths: `current` is canonical, the candidate is not.
            candidate
                .canonicalize()
                .ok()
                .zip(other.canonicalize().ok())
                .map(|(a, b)| a == b)
                .unwrap_or(false)
        };
        if same(current) || live_slots.iter().any(|used| same(used)) {
            continue;
        }
        if std::fs::remove_dir_all(&candidate).is_ok() {
            removed.push(candidate);
        }
    }
    removed
}

/* ---------------------------------------------------------------- applying */

/// Copies `sessions.json` to `sessions.json.pre-update-<unix-timestamp>` beside it.
///
/// The new binary reads the colony list back at start, and a release that
/// misreads it would otherwise take the only copy with it. No file yet is a
/// first run with nothing to lose, not an error. The copy is a write of the
/// backup, so it goes through the fault seam as one, like `copy_corrupt_aside`.
fn backup_sessions(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let saved = path.with_file_name(format!("{name}.pre-update-{stamp}"));
    let context = || format!("could not back {} up to {}", path.display(), saved.display());
    util::faults::check(path, util::faults::Op::Write).with_context(context)?;
    std::fs::copy(path, &saved).with_context(context)?;
    Ok(Some(saved))
}

/// The backup, taken under the lock every save of the list holds, so it is
/// never a copy of a file that is being replaced.
async fn back_up_sessions(app: &Shared) -> Result<Option<PathBuf>> {
    let _guard = app.session_persist.lock().await;
    backup_sessions(&app.sessions_file())
}

/// The backup, then the installer — and no installer if the backup failed.
///
/// Both run in the spawned task rather than in the handler: a handler dropped
/// while it waits for the save lock (the client went away) would otherwise
/// leave the phase at `Installing` for good, and every retry refused. Here a
/// failed backup takes the same `Failed` path a failed install does.
///
/// Returns the installer's output and where the backup went, and records the
/// path on the progress first, so `GET /api/update` names it while the
/// installer is still running rather than only once it finishes.
async fn back_up_and_install(app: &Shared, version: &str) -> Result<(String, Option<PathBuf>)> {
    let saved = back_up_sessions(app).await?;
    {
        let mut progress = app.updater.progress.lock().await;
        progress.backup = saved.as_ref().map(|path| path.display().to_string());
    }
    // The progress is in memory and gone after the restart, so the path is
    // logged where the mothership's own output goes — a Cockpit-initiated
    // update leaves it behind there.
    if let Some(path) = &saved {
        println!("update: sessions.json backed up to {}", path.display());
    }
    let log = install(app, version).await?;
    Ok((log, saved))
}

async fn install(app: &Shared, version: &str) -> Result<String> {
    let assets = app.cfg.assets.clone().context("no app assets")?;
    let script = installer(&assets);
    let mut command = Command::new("sh");
    command
        .arg(&script)
        .env("COLONIZER_VERSION", version)
        // The slot this mothership is running from stays until the sweep.
        .env("COLONIZER_KEEP_PREVIOUS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(app_dir) = app_link() {
        command.env("COLONIZER_APP", app_dir);
    }

    let output = tokio::time::timeout(INSTALL_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the installer did not finish within {} minutes",
                INSTALL_TIMEOUT.as_secs() / 60
            )
        })?
        .with_context(|| format!("running {}", script.display()))?;

    let mut log = String::from_utf8_lossy(&output.stdout).into_owned();
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    let log = util::truncate(log.trim(), 8000);
    if !output.status.success() {
        bail!("the installer exited with {}:\n{log}", output.status);
    }
    Ok(log)
}

/// Replaces this process with the newly installed one.
///
/// Never returns on success. The mesh children are killed first so the new
/// process can take their ports, and the live map is told this mothership is
/// going away rather than leaving it lit.
async fn restart(app: &Shared) -> anyhow::Error {
    let binary = match app_link().map(|a| a.join("bin/colonizer")) {
        Some(binary) if binary.exists() => binary,
        Some(binary) => return anyhow::anyhow!("{} is missing after the install", binary.display()),
        None => return anyhow::anyhow!("no app directory to restart from"),
    };

    app.telemetry.goodbye().await;
    let mesh = app.mesh.lock().await.clone();
    if let Some(mesh) = mesh {
        mesh.shutdown().await;
    }

    // Same arguments, so a mothership started with flags keeps them.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // Only returns if exec failed: on success this process is already gone.
    let error = exec(&binary, &args);
    anyhow::anyhow!("could not start {}: {error}", binary.display())
}

#[cfg(unix)]
fn exec(binary: &Path, args: &[std::ffi::OsString]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(binary).args(args).exec()
}

#[cfg(not(unix))]
fn exec(_binary: &Path, _args: &[std::ffi::OsString]) -> std::io::Error {
    std::io::Error::other("updating in place needs a Unix process")
}

/* --------------------------------------------------------------------- cli */

/// Just what the update decision reads from the mothership's `installed`
/// object. Read tolerantly: the CLI on disk may be newer or older than the
/// running mothership, so unknown fields are ignored and a missing one falls
/// back rather than failing the whole command.
struct Installed {
    version: String,
    release: Option<String>,
    development: bool,
}

impl Installed {
    fn from_status(status: &Value) -> Self {
        let installed = &status["installed"];
        Self {
            version: installed["version"].as_str().unwrap_or("this build").to_string(),
            release: installed["release"].as_str().map(str::to_string),
            development: installed["development"].as_bool().unwrap_or(false),
        }
    }

    /// The decision only reads these three fields; the rest of the stamped
    /// build — commit, build time — is filler it never looks at.
    fn as_build(&self) -> version::Build {
        version::Build {
            version: self.version.clone(),
            commit: None,
            dirty: false,
            built_at: Utc::now(),
            release: self.release.clone(),
            development: self.development,
        }
    }
}

/// What one `/api/update` read decides, before anything is posted.
#[derive(Debug, PartialEq)]
enum Preflight {
    /// Already there: print it and stop.
    NothingToDo { installed: String },
    /// POST (forced when forcing) and follow the progress.
    Proceed { installed: String, latest: String },
}

/// The CLI's go/no-go from a single status read: the refusal both sides share,
/// the nothing-to-install short-circuit, and — unless forced — the mothership's
/// own verdict.
///
/// With `--force` there is no `can_apply` pre-check: the route rechecks the
/// blocker and the publishing colonies anyway, and its 409 is printed as is.
fn preflight(status: &Value, force: bool) -> Result<Preflight> {
    let installed = Installed::from_status(status);
    let build = installed.as_build();
    let Some(latest) = status["latest"]["version"].as_str().map(str::to_string) else {
        // No answer is not the same as no update, and must never be reported as
        // one: the check may be off, or simply not have run yet. Forcing changes
        // nothing here: with no release known there is nothing to install.
        if let Some(blocked) = status["blocked_by"].as_str() {
            bail!(
                "the update check is kept off by {blocked}, so there is nothing to compare {} against",
                installed.version
            );
        }
        if !status["enabled"].as_bool().unwrap_or(false) {
            bail!("the update check is switched off, so this mothership does not know what the newest release is");
        }
        bail!("this mothership has not reached the release feed yet; try again shortly");
    };
    // One decision, the same one the route makes.
    if let Some(reason) = refusal(&build, Some(&latest), force) {
        bail!("{reason}");
    }
    if !status["available"].as_bool().unwrap_or(false) && !force {
        return Ok(Preflight::NothingToDo {
            installed: installed.version,
        });
    }
    if !force && let Some(reason) = status["can_apply"]["reason"].as_str() {
        bail!("{latest} is out, but it cannot be installed from here: {reason}");
    }
    Ok(Preflight::Proceed {
        installed: installed.version,
        latest,
    })
}

/// `colonizer update [--force]` — a thin client of a running mothership.
///
/// The work belongs to the mothership: it knows what is installed, which
/// colonies are publishing, and how to restart itself. This asks it to start,
/// then follows the progress it already reports, so the command and the button
/// in Settings cannot drift apart.
///
/// `force` installs the latest release over a development build, or over a
/// release newer than it, after saying what is at risk. It never skips the
/// wait for a publishing colony: that refusal comes from the mothership.
pub async fn command(force: bool) -> Result<()> {
    let cfg = Settings::from_env()?;
    let base = format!("http://{}", cfg.bind);
    // The mothership's API needs the per-install token; this CLI reads the file the server minted.
    let token = auth::load_or_create(&cfg.config_dir)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let status: Value = client
        .get(format!("{base}/api/update"))
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("no mothership answering on {}; start `colonizer` first", cfg.bind))?
        // A wrong token answers 401, which must surface as refused, not as unparsable JSON.
        .error_for_status()?
        .json()
        .await?;

    let flight = preflight(&status, force)?;
    let (installed, latest) = match flight {
        Preflight::NothingToDo { installed } => {
            println!("{installed} is the newest release.");
            return Ok(());
        }
        Preflight::Proceed { installed, latest } => (installed, latest),
    };
    if force {
        println!("{}", force_warning(&client, &base, &installed, &latest).await);
    }
    println!("updating from {installed} to {latest}");

    // No body, like the Settings button, unless forcing: the route reads a
    // missing body as "just install the newer release".
    let request = client.post(format!("{base}/api/update/apply")).bearer_auth(&token);
    let started = if force {
        request.json(&json!({ "force": true })).send().await?
    } else {
        request.send().await?
    };
    if !started.status().is_success() {
        let body = started.text().await.unwrap_or_default();
        bail!("{}", util::truncate(&server_error(&body), 500));
    }

    // The mothership replaces itself when it is done, so the connection drops
    // rather than reporting success. A drop after `restarting` is the success.
    let mut last = String::new();
    let mut backup_announced = false;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Ok(response) = client.get(format!("{base}/api/update")).bearer_auth(&token).send().await else {
            println!("the mothership is restarting into {latest}");
            return Ok(());
        };
        let status: Value = response.json().await?;
        if let Some(path) = status["apply"]["backup"].as_str()
            && !backup_announced
        {
            println!("sessions.json backed up to {path}");
            backup_announced = true;
        }
        let phase = status["apply"]["phase"].as_str().unwrap_or("idle").to_string();
        if phase != last {
            println!("  {phase}");
            last = phase.clone();
        }
        match phase.as_str() {
            "failed" => bail!("{}", status["apply"]["error"].as_str().unwrap_or("the update failed")),
            "idle" => return Ok(()),
            _ => {}
        }
    }
}

/// What `--force` is about to risk, in one line: the release going over the
/// running build, and how many sessions were written by that build.
///
/// Queued colonies hold nothing yet, so an update does not interrupt them — but
/// `--force` is the explicit acknowledgement of the queue, and the count
/// belongs in the warning next to the total.
async fn force_warning(client: &reqwest::Client, base: &str, installed: &str, latest: &str) -> String {
    let sessions = session_counts(client, base).await;
    let at_risk = match sessions {
        Ok((total, queued)) => {
            let noun = if total == 1 {
                "1 session".to_string()
            } else {
                format!("{total} sessions")
            };
            format!("the {noun} in sessions.json ({queued} queued)")
        }
        Err(_) => "the sessions in sessions.json".to_string(),
    };
    format!(
        "warning: --force installs `{latest}` over `{installed}`; {at_risk} were written by the running build and are backed up first"
    )
}

async fn session_counts(client: &reqwest::Client, base: &str) -> Result<(usize, usize)> {
    let sessions: Vec<Value> = client.get(format!("{base}/api/sessions")).send().await?.json().await?;
    let queued = sessions.iter().filter(|s| s["status"].as_str() == Some("queued")).count();
    Ok((sessions.len(), queued))
}

/// A refused POST answers `{"error": …}`; print the message, not the JSON.
/// Anything else — a proxy's HTML, an empty body — prints as it arrived.
fn server_error(body: &str) -> String {
    let body = body.trim();
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|json| json["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

/* ------------------------------------------------------------------ routes */

/// The optional body of `POST /api/update/apply`. Absent, blank or `{}` all
/// mean the same as the Settings button sending no body at all; anything else
/// that is not JSON is a 400 rather than a silent no-force.
#[derive(Default, Deserialize)]
struct ApplyRequest {
    #[serde(default)]
    force: bool,
}

/// `POST /api/update/apply` — install the latest release and restart into it.
///
/// Answers as soon as the work starts; `GET /api/update` carries the progress.
/// Takes an optional `{"force": true}` body, which installs the latest release
/// over a development build or over a newer release. No body — what the
/// Settings button sends — means "just install the newer release": the body is
/// read as bytes so a missing one is `force: false`, not a 422. Anything else
/// that is not JSON is a 400.
pub async fn apply(State(app): State<Shared>, body: Bytes) -> crate::ApiResult<Value> {
    // No body is the Cockpit button: `fetch` sends no bytes, and that has
    // always meant "just install the newer release".
    let force = if body.iter().all(|b| b.is_ascii_whitespace()) {
        false
    } else {
        match serde_json::from_slice::<ApplyRequest>(&body) {
            Ok(request) => request.force,
            Err(e) => {
                return Err(crate::client_error(
                    StatusCode::BAD_REQUEST,
                    &format!("the apply body must be JSON like {{\"force\": true}}: {e}"),
                ));
            }
        }
    };
    if let Some(reason) = blocker(app.cfg.assets.as_deref()) {
        return Err(crate::client_error(StatusCode::CONFLICT, &reason));
    }
    let running = version::build();
    let Some(latest) = app.updates.latest_known().await else {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "the latest release is not known yet; the check may be switched off or not have run",
        ));
    };
    // The one decision, the same one `colonizer update` makes before posting.
    if let Some(reason) = refusal(running, Some(&latest), force) {
        return Err(crate::client_error(StatusCode::CONFLICT, &reason));
    }
    if !force && !version::is_newer(running.release.as_deref(), &latest) {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            &format!("{} is already the newest release", running.version),
        ));
    }

    let version = latest;
    let sessions = app.sessions.read().await.clone();
    let busy = publishing(&sessions);
    if !busy.is_empty() {
        let names: Vec<String> = busy.iter().map(|s| format!("{} ({})", s.repo, s.id)).collect();
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            &format!(
                "a colony is publishing: {}. Updating now would leave its pull request unopened.",
                names.join(", ")
            ),
        ));
    }

    {
        let mut progress = app.updater.progress.lock().await;
        if progress.phase == Phase::Installing || progress.phase == Phase::Restarting {
            return Err(crate::client_error(
                StatusCode::CONFLICT,
                "an update is already being applied",
            ));
        }
        *progress = Progress {
            phase: Phase::Installing,
            version: Some(version.clone()),
            started_at: Some(Utc::now()),
            error: None,
            log: String::new(),
            colonies: notes(&sessions),
            backup: None,
        };
    }

    let background = app.clone();
    tokio::spawn(async move {
        match back_up_and_install(&background, &version).await {
            Ok((log, saved)) => {
                {
                    let mut progress = background.updater.progress.lock().await;
                    progress.phase = Phase::Restarting;
                    // The path went onto the progress when the backup was taken;
                    // the log keeps it for anyone reading after the restart.
                    progress.log = match saved.as_ref().map(|path| path.display().to_string()) {
                        Some(path) => format!("sessions.json backed up to {path}\n{log}"),
                        None => log,
                    };
                }
                // Give the answer above a moment to reach the browser before the
                // process is replaced underneath it.
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                let e = restart(&background).await;
                let mut progress = background.updater.progress.lock().await;
                progress.phase = Phase::Failed;
                progress.error = Some(util::truncate(&format!("{e:#}"), 1000));
            }
            Err(e) => {
                // The running version is untouched: the installer swaps the
                // symlink last, and only after everything is unpacked.
                let mut progress = background.updater.progress.lock().await;
                progress.phase = Phase::Failed;
                progress.error = Some(util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });

    Ok(Json(json!({ "started": true })))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/update/apply", routing::post(apply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_publishing_colony_holds_the_update() {
        use SessionStatus::*;
        assert!(holds_update(Publishing));
        // Detached; the restart reconnects them.
        for status in [Running, WaitingForAnswer, Idle, Starting, Queued, Stopped, PrOpened] {
            assert!(!holds_update(status), "{status:?} should not hold an update");
        }
    }

    #[test]
    fn live_colonies_are_the_ones_reported_on() {
        use SessionStatus::*;
        for status in [Starting, Running, WaitingForAnswer, Idle] {
            assert!(will_reconnect(status), "{status:?} should be reported as reconnecting");
        }
        for status in [Stopped, Failed, NoChanges, PrOpened, Queued] {
            assert!(!will_reconnect(status), "{status:?} has nothing to reconnect");
        }
    }

    #[test]
    fn a_source_checkout_says_why_it_cannot_update() {
        // No assets at all: running from a build tree.
        let reason = blocker(None).expect("should refuse");
        assert!(reason.contains("installed app directory"), "{reason}");
    }

    #[test]
    fn an_install_without_the_installer_refuses() {
        let dir = std::env::temp_dir().join(format!("colonizer-update-{}", util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let reason = blocker(Some(&dir)).expect("should refuse");
        assert!(reason.contains("install-release.sh"), "{reason}");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn a_build(version: &str, release: &str, development: bool) -> version::Build {
        version::Build {
            version: version.into(),
            commit: Some("d62bfb2".into()),
            dirty: false,
            built_at: Utc::now(),
            release: Some(release.into()),
            development,
        }
    }

    #[test]
    fn a_development_build_refuses_and_names_both_versions() {
        let reason = refusal(&a_build("v0.1.5-60-gd62bfb2", "v0.1.5", true), Some("v0.1.5"), false).expect("should refuse");
        assert!(reason.contains("v0.1.5-60-gd62bfb2"), "{reason}");
        assert!(reason.contains("60 commits ahead of release `v0.1.5`"), "{reason}");
        assert!(reason.contains("the latest release is `v0.1.5`"), "{reason}");
        assert!(reason.contains("git pull && scripts/install.sh --install"), "{reason}");
        assert!(reason.contains("`--force` to install `v0.1.5` anyway"), "{reason}");
    }

    #[test]
    fn a_development_build_refuses_even_when_the_latest_release_is_unknown() {
        // The command bails before reading the feed when the check is off; the
        // route refuses before asking for it. Either way the build is named.
        let reason = refusal(&a_build("v0.1.5-60-gd62bfb2", "v0.1.5", true), None, false).expect("should refuse");
        assert!(reason.contains("v0.1.5-60-gd62bfb2"), "{reason}");
        assert!(reason.contains("git pull && scripts/install.sh --install"), "{reason}");
        assert!(!reason.contains("--force"), "{reason}");
    }

    #[test]
    fn a_release_newer_than_the_latest_release_refuses_the_downgrade() {
        let reason = refusal(&a_build("v0.1.6", "v0.1.6", false), Some("v0.1.5"), false).expect("should refuse");
        assert!(reason.contains("`v0.1.6`"), "{reason}");
        assert!(reason.contains("newer than the latest release `v0.1.5`"), "{reason}");
        assert!(reason.contains("`--force` to install `v0.1.5` anyway"), "{reason}");
    }

    #[test]
    fn a_release_at_or_behind_the_latest_release_is_no_refusal() {
        // Behind: proceeds to the newer-or-nothing decision. Equal: the caller
        // reports nothing to install. Neither is a refusal.
        assert_eq!(refusal(&a_build("v0.1.4", "v0.1.4", false), Some("v0.1.5"), false), None);
        assert_eq!(refusal(&a_build("v0.1.5", "v0.1.5", false), Some("v0.1.5"), false), None);
    }

    #[test]
    fn force_proceeds_past_either_refusal() {
        assert_eq!(
            refusal(&a_build("v0.1.5-60-gd62bfb2", "v0.1.5", true), Some("v0.1.5"), true),
            None
        );
        assert_eq!(refusal(&a_build("v0.1.6", "v0.1.6", false), Some("v0.1.5"), true), None);
    }

    /// One `/api/update` read, as the command sees it.
    fn status(
        version: &str,
        release: Option<&str>,
        development: bool,
        latest: Option<&str>,
        available: bool,
        reason: Option<String>,
    ) -> Value {
        let mut installed = serde_json::Map::new();
        installed.insert("version".into(), Value::String(version.into()));
        if let Some(release) = release {
            installed.insert("release".into(), Value::String(release.into()));
        }
        if development {
            installed.insert("development".into(), Value::Bool(true));
        }
        json!({
            "installed": installed,
            "latest": latest.map(|v| json!({"version": v})).unwrap_or(Value::Null),
            "available": available,
            "can_apply": {"ok": reason.is_none(), "reason": reason},
            "enabled": true,
            "blocked_by": Value::Null,
        })
    }

    #[test]
    fn a_forced_update_proceeds_past_the_refusal_in_can_apply() {
        // With force there is no `can_apply` pre-check — the route enforces the
        // blocker itself — so the refusal sitting in the read is not even looked at.
        let reason = refusal(&a_build("v0.1.5-60-gd62bfb2", "v0.1.5", true), Some("v0.1.5"), false);
        let flight = preflight(
            &status("v0.1.5-60-gd62bfb2", Some("v0.1.5"), true, Some("v0.1.5"), false, reason),
            true,
        )
        .expect("force proceeds");
        assert_eq!(
            flight,
            Preflight::Proceed {
                installed: "v0.1.5-60-gd62bfb2".into(),
                latest: "v0.1.5".into()
            }
        );
    }

    #[test]
    fn without_force_the_refusal_stops_before_any_post() {
        let reason = refusal(&a_build("v0.1.6", "v0.1.6", false), Some("v0.1.5"), false);
        let error = format!(
            "{:#}",
            preflight(&status("v0.1.6", Some("v0.1.6"), false, Some("v0.1.5"), false, reason), false)
                .expect_err("the downgrade refusal stops the command")
        );
        assert!(error.contains("refusing to downgrade"), "{error}");
    }

    #[test]
    fn an_up_to_date_release_has_nothing_to_do_unless_forced() {
        let read = status("v0.1.5", Some("v0.1.5"), false, Some("v0.1.5"), false, None);
        assert_eq!(
            preflight(&read, false).expect("no refusal, nothing newer"),
            Preflight::NothingToDo {
                installed: "v0.1.5".into()
            }
        );
        // Forced, the same read reinstalls rather than stopping.
        assert!(matches!(preflight(&read, true), Ok(Preflight::Proceed { .. })));
    }

    #[test]
    fn a_newer_or_older_motherships_installed_object_still_decides() {
        // Fields the decision never reads are ignored, and missing ones fall
        // back: this read names no release and no development flag.
        let read = status("v0.1.4", None, false, Some("v0.1.5"), true, None);
        assert!(matches!(preflight(&read, false), Ok(Preflight::Proceed { .. })));
    }

    #[test]
    fn a_refused_post_prints_the_message_not_the_json() {
        assert_eq!(
            server_error(r#"{"error":"a colony is publishing: o/r (abc)"}"#),
            "a colony is publishing: o/r (abc)"
        );
        assert_eq!(server_error("  Bad Gateway  "), "Bad Gateway");
    }

    #[test]
    fn a_first_run_with_no_session_list_backs_nothing_up() {
        let dir = std::env::temp_dir().join(format!("colonizer-update-nolist-{}", util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(backup_sessions(&dir.join("sessions.json")).unwrap(), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing should be written");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An app whose `install-release.sh` is `script`, with a `sessions.json` of `list`.
    fn app_with_installer(root: &Path, script: &str, list: &[u8]) -> Shared {
        let assets = root.join("assets");
        std::fs::create_dir_all(assets.join("scripts")).unwrap();
        std::fs::write(installer(&assets), script).unwrap();
        let app = crate::tests::test_app_with(root, |s| s.assets = Some(assets.clone()));
        std::fs::write(app.sessions_file(), list).unwrap();
        app
    }

    #[tokio::test]
    async fn the_session_list_is_backed_up_before_the_installer_swaps_anything() {
        let root = std::env::temp_dir().join(format!("colonizer-update-backup-{}", util::short_id()));
        // Stands in for install-release.sh: it refuses to "swap" unless the
        // backup is already on disk when it runs.
        let fake = format!(
            "ls {}/sessions.json.pre-update-* >/dev/null 2>&1 || {{ echo 'no backup before the swap' >&2; exit 1; }}\necho swapped\n",
            root.join("data").display()
        );
        let original = br#"[{"id":"abc","repo":"o/r"}]"#;
        let app = app_with_installer(&root, &fake, original);

        let (log, saved) = back_up_and_install(&app, "v9.9.9")
            .await
            .expect("the installer should find the backup");
        assert!(log.contains("swapped"), "{log}");
        let saved = saved.expect("the backup path is returned");
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        assert_eq!(
            app.updater.progress().await.backup,
            Some(saved.display().to_string()),
            "the progress names the backup while the installer runs"
        );
        let saved: Vec<PathBuf> = std::fs::read_dir(root.join("data"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("sessions.json.pre-update-")
            })
            .collect();
        assert_eq!(saved.len(), 1, "exactly one backup: {saved:?}");
        assert_eq!(std::fs::read(&saved[0]).unwrap(), original);

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_failed_backup_installs_nothing() {
        let root = std::env::temp_dir().join(format!("colonizer-update-nobackup-{}", util::short_id()));
        let marker = root.join("installed");
        let app = app_with_installer(&root, &format!("touch {}\n", marker.display()), b"[]");

        let _fault = util::faults::inject(&app.sessions_file().display().to_string(), util::faults::Op::Write, || {
            std::io::Error::other("disk full")
        });
        let error = back_up_and_install(&app, "v9.9.9")
            .await
            .expect_err("a failed backup must stop the update");
        assert!(format!("{error:#}").contains("could not back"), "{error:#}");
        assert!(!marker.exists(), "the installer must not run without a backup");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_sweep_keeps_the_slot_this_process_runs_from() {
        let root = std::env::temp_dir().join(format!("colonizer-sweep-{}", util::short_id()));
        let share = root.join(".local/share/colonizer");
        std::fs::create_dir_all(share.join("app-a")).unwrap();
        std::fs::create_dir_all(share.join("app-b")).unwrap();
        std::os::unix::fs::symlink("app-b", share.join("app")).unwrap();

        // SAFETY: single-threaded test process, and the value is restored below.
        let previous = std::env::var_os("COLONIZER_APP");
        unsafe { std::env::set_var("COLONIZER_APP", share.join("app")) };

        // Running from app-b, with a colony still mounted on app-a.
        let removed = sweep_slots(Some(&share.join("app-b")), &[share.join("app-a")]);
        assert!(removed.is_empty(), "a slot a colony still uses must survive: {removed:?}");
        assert!(share.join("app-a").is_dir());

        // Once nothing points at app-a, it goes.
        let removed = sweep_slots(Some(&share.join("app-b")), &[]);
        assert_eq!(removed.len(), 1, "the unused slot should be swept");
        assert!(!share.join("app-a").exists());
        assert!(share.join("app-b").is_dir(), "the running slot must never be swept");

        match previous {
            Some(v) => unsafe { std::env::set_var("COLONIZER_APP", v) },
            None => unsafe { std::env::remove_var("COLONIZER_APP") },
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
