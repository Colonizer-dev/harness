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
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::Serialize;
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

/// Why this build must not be replaced by a release, if it must not.
///
/// A development build — commits after its last tag, a modified tree, or no tag
/// at all — holds work the newest release does not, however much newer that
/// release's number is. Installing the release would swap that work out without
/// a word, so the refusal names the way such a build is updated instead.
pub fn dev_refusal(build: &version::Build) -> Option<String> {
    build.development.then(|| dev_guidance(&build.version))
}

/// The one wording, shared with `colonizer update`, which only has the JSON.
fn dev_guidance(version: &str) -> String {
    format!(
        "this is a development build ({version}); update it from its checkout with `git pull && scripts/install.sh --install`"
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
async fn back_up_and_install(app: &Shared, version: &str) -> Result<String> {
    back_up_sessions(app).await?;
    install(app, version).await
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

/// `colonizer update` — a thin client of a running mothership.
///
/// The work belongs to the mothership: it knows what is installed, which
/// colonies are publishing, and how to restart itself. This asks it to start,
/// then follows the progress it already reports, so the command and the button
/// in Settings cannot drift apart.
pub async fn command() -> Result<()> {
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

    let installed = status["installed"]["version"].as_str().unwrap_or("this build").to_string();
    // Before anything about releases: whether one is newer or not, it would
    // replace work this build has and it does not.
    if status["installed"]["development"].as_bool().unwrap_or(false) {
        bail!("{}", dev_guidance(&installed));
    }
    let Some(latest) = status["latest"]["version"].as_str().map(str::to_string) else {
        // No answer is not the same as no update, and must never be reported as
        // one: the check may be off, or simply not have run yet.
        if let Some(blocked) = status["blocked_by"].as_str() {
            bail!("the update check is kept off by {blocked}, so there is nothing to compare {installed} against");
        }
        if !status["enabled"].as_bool().unwrap_or(false) {
            bail!("the update check is switched off, so this mothership does not know what the newest release is");
        }
        bail!("this mothership has not reached the release feed yet; try again shortly");
    };
    if !status["available"].as_bool().unwrap_or(false) {
        println!("{installed} is the newest release.");
        return Ok(());
    }
    if let Some(reason) = status["can_apply"]["reason"].as_str() {
        bail!("{latest} is out, but it cannot be installed from here: {reason}");
    }
    println!("updating from {installed} to {latest}");

    let started = client
        .post(format!("{base}/api/update/apply"))
        .bearer_auth(&token)
        .send()
        .await?;
    if !started.status().is_success() {
        let body = started.text().await.unwrap_or_default();
        bail!("{}", util::truncate(body.trim(), 500));
    }

    // The mothership replaces itself when it is done, so the connection drops
    // rather than reporting success. A drop after `restarting` is the success.
    let mut last = String::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let Ok(response) = client.get(format!("{base}/api/update")).bearer_auth(&token).send().await else {
            println!("the mothership is restarting into {latest}");
            return Ok(());
        };
        let status: Value = response.json().await?;
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

/* ------------------------------------------------------------------ routes */

/// `POST /api/update/apply` — install the latest release and restart into it.
///
/// Answers as soon as the work starts; `GET /api/update` carries the progress.
pub async fn apply(State(app): State<Shared>) -> crate::ApiResult<Value> {
    if let Some(reason) = blocker(app.cfg.assets.as_deref()) {
        return Err(crate::client_error(StatusCode::CONFLICT, &reason));
    }
    if let Some(reason) = dev_refusal(version::build()) {
        return Err(crate::client_error(StatusCode::CONFLICT, &reason));
    }

    let status = app.updates.latest_release().await;
    let Some(version) = status else {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "there is no newer release to install",
        ));
    };

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
        };
    }

    let background = app.clone();
    tokio::spawn(async move {
        match back_up_and_install(&background, &version).await {
            Ok(log) => {
                {
                    let mut progress = background.updater.progress.lock().await;
                    progress.phase = Phase::Restarting;
                    progress.log = log;
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

    fn a_build(version: &str, development: bool) -> version::Build {
        version::Build {
            version: version.into(),
            commit: Some("d62bfb2".into()),
            dirty: false,
            built_at: Utc::now(),
            release: Some("v0.1.5".into()),
            development,
        }
    }

    #[test]
    fn a_development_build_refuses_to_be_replaced_by_a_release() {
        let reason = dev_refusal(&a_build("v0.1.5-60-gd62bfb2", true)).expect("should refuse");
        assert!(reason.contains("v0.1.5-60-gd62bfb2"), "{reason}");
        assert!(reason.contains("git pull && scripts/install.sh --install"), "{reason}");
    }

    #[test]
    fn a_release_build_is_allowed_to_update() {
        assert_eq!(dev_refusal(&a_build("v0.1.5", false)), None);
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

        let log = back_up_and_install(&app, "v9.9.9")
            .await
            .expect("the installer should find the backup");
        assert!(log.contains("swapped"), "{log}");
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
