//! The update check and the self-update: while the mothership runs it asks GitHub, every few hours,
//! whether a newer release exists than the one this build contains, and remembers the answer for
//! Settings and the web UI. The request carries no token, no query and nothing about the install —
//! GitHub sees one anonymous `GET /releases/latest` per check, and issue #14 covers sending anything
//! more. It is on by default. Applying a release lives here too: install::install puts it beside this
//! one, `app` is repointed, and this process re-execs through the link, so the mothership and every
//! colony come back on the new version with their state intact.

use crate::{
    App, Shared,
    install::{self, LayoutKind},
    sessions::{Session, SessionStatus},
    util, version,
};
use anyhow::{Context, Result, bail};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{Mutex, Notify};

pub const DEFAULT_API: &str = "https://api.github.com";
pub const DEFAULT_REPO: &str = "Colonizer-dev/harness";
/// The first check happens this soon after start, so a running mothership hears about a release
/// without the check delaying startup.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
/// How often a checked-in mothership asks again.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// A check that failed (or found the switch off) is retried after this long instead of a full interval.
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);
/// Release notes longer than this are cut: the UI shows a paragraph, not the whole changelog.
const MAX_NOTES: usize = 16 * 1024;

/// The user's switch, kept in `<config>/update.json`. Unlike the live map there is no question to
/// answer first: checking is on until someone turns it off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for Choice {
    fn default() -> Self {
        Choice { enabled: true }
    }
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

/// The newest release the last check saw, and how that check went.
#[derive(Clone, Debug, Default)]
struct Found {
    last_checked_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    latest: Option<Release>,
}

/// One GitHub release, as the API reports it to the UI.
#[derive(Clone, Debug, Serialize)]
pub struct Release {
    /// The tag, e.g. `v0.2.0`; this is what is compared against the running build.
    pub version: String,
    pub name: Option<String>,
    pub notes: Option<String>,
    pub url: String,
    pub published_at: Option<String>,
}

/// Where a self-update is in its walk from download to restart. The vocabulary is the UI's
/// (`web/src/types.ts`); `idle` and `failed` are the resting states, everything else is under way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyState {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Unpacking,
    Installing,
    /// The new release is in place and this process is about to replace itself.
    Restarting,
    Failed,
}

/// The progress of a self-update: reported while the work runs, and kept afterwards so the UI can
/// say what happened.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ApplyStatus {
    pub state: ApplyState,
    /// The release being applied, once one is.
    pub version: Option<String>,
    pub bytes: u64,
    pub total: Option<u64>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    /// Bumped for every apply started, so a stale one cannot overwrite a newer status.
    #[serde(skip)]
    pub generation: u64,
}

impl ApplyStatus {
    /// Whether an apply is under way: anything between starting and restarting counts, and a failed
    /// one does not — a failed update may simply be tried again.
    pub(crate) fn running(&self) -> bool {
        !matches!(self.state, ApplyState::Idle | ApplyState::Failed)
    }
}

pub struct Updates {
    /// Where the switch is kept: `<config_dir>/update.json`.
    path: PathBuf,
    /// The GitHub API root, overridable so tests (and air-gapped setups) can point it elsewhere.
    api: String,
    /// The repository whose releases are watched.
    repo: String,
    /// When set, the environment keeps the check off and nothing may override it.
    blocked: Option<&'static str>,
    client: reqwest::Client,
    choice: Mutex<Choice>,
    found: Mutex<Found>,
    /// The self-update's progress while one runs, `App.headroom`'s pattern. `pub(crate)` because
    /// install.rs's startup prune also reads it, to stay out of a running apply's way.
    pub(crate) apply: Mutex<ApplyStatus>,
    /// Wakes the check loop when the switch changes or a check is asked for.
    wake: Notify,
}

impl Updates {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let blocked = blocked_by(std::env::var("COLONIZER_UPDATE_CHECK").ok().as_deref());
        let api = util::env_nonempty("COLONIZER_UPDATE_API").unwrap_or_else(|| DEFAULT_API.into());
        let repo =
            util::env_nonempty("COLONIZER_UPDATE_REPO").unwrap_or_else(|| DEFAULT_REPO.into());
        Self::with(config_dir.join("update.json"), api, repo, blocked)
    }

    fn with(
        path: PathBuf,
        api: String,
        repo: String,
        blocked: Option<&'static str>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            choice: Mutex::new(Choice::load(&path)),
            path,
            api: api.trim_end_matches('/').to_string(),
            repo,
            blocked,
            client,
            found: Mutex::new(Found::default()),
            apply: Mutex::new(ApplyStatus::default()),
            wake: Notify::new(),
        })
    }

    /// Switches the check on or off and saves the answer.
    async fn set_enabled(&self, enabled: bool) -> Result<()> {
        if let Some(variable) = self.blocked {
            bail!("the update check is kept off by {variable} in the mothership's environment");
        }
        {
            let mut choice = self.choice.lock().await;
            choice.enabled = enabled;
            choice.save(&self.path)?;
        }
        self.wake.notify_one();
        Ok(())
    }

    /// Asks GitHub for the latest release of the watched repository.
    async fn fetch_latest(&self) -> Result<Release> {
        let response = self
            .client
            .get(format!("{}/repos/{}/releases/latest", self.api, self.repo))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?
            .error_for_status()?;
        let body: Value = response.json().await?;
        let tag = body["tag_name"]
            .as_str()
            .context("the release has no tag_name")?
            .trim()
            .to_string();
        Ok(Release {
            version: tag,
            name: body["name"].as_str().map(str::to_string),
            notes: body["body"].as_str().map(|b| util::truncate(b, MAX_NOTES)),
            url: body["html_url"].as_str().unwrap_or_default().to_string(),
            published_at: body["published_at"].as_str().map(str::to_string),
        })
    }

    /// One turn of the loop: runs a check when the switch and the environment allow it, records when
    /// it happened and what it found, and says whether it succeeded (a skipped or failed check is
    /// retried sooner than a successful one).
    async fn poll(&self) -> bool {
        if self.blocked.is_some() || !self.choice.lock().await.enabled {
            return false;
        }
        let result = self.fetch_latest().await;
        let mut found = self.found.lock().await;
        // A failed check keeps the last good answer: better a release the UI can name than a blank.
        match result {
            Ok(release) => {
                found.latest = Some(release);
                found.last_error = None;
                found.last_checked_at = Some(Utc::now());
                true
            }
            Err(e) => {
                found.last_error = Some(format!("{e:#}"));
                found.last_checked_at = Some(Utc::now());
                false
            }
        }
    }

    /// Folds one install progress report into the apply status. The generation is the caller's, so a
    /// straggler from an earlier apply cannot overwrite a newer one's status.
    async fn note(&self, report: install::Report, generation: u64) {
        let mut apply = self.apply.lock().await;
        if apply.generation != generation {
            return;
        }
        match report {
            install::Report::Downloading => apply.state = ApplyState::Downloading,
            install::Report::Verifying => apply.state = ApplyState::Verifying,
            install::Report::Unpacking => apply.state = ApplyState::Unpacking,
            install::Report::Installing => apply.state = ApplyState::Installing,
            install::Report::Bytes { bytes, total } => {
                apply.bytes = bytes;
                apply.total = total;
            }
        }
    }

    /// The view with the locks taken, factored out so tests can pin its exact shape without an `App`.
    fn view_with(&self, choice: &Choice, found: &Found, now: Now<'_>) -> Value {
        let available = found
            .latest
            .as_ref()
            .is_some_and(|r| newer(&r.version, version::build().release.as_deref()));
        let blocked_reason = blocked_reason_for(now.kind, now.platform);
        json!({
            "enabled": self.blocked.is_none() && choice.enabled,
            "blocked_by": self.blocked,
            "repo": self.repo,
            "installed": version::value(version::build()),
            "last_checked_at": found.last_checked_at,
            "last_error": found.last_error,
            "latest": found.latest.as_ref(),
            "available": available,
            "can_apply": available && blocked_reason.is_none(),
            "blocked_reason": blocked_reason,
            "busy": busy_reasons(now.sessions, now.apply),
            "apply": now.apply,
        })
    }
}

/// Everything beyond the check itself that decides whether an update can be applied right now: what
/// shape this install is, what machine it is on, what the colonies are doing, and whether an apply
/// is already on its way.
pub struct Now<'a> {
    pub kind: LayoutKind,
    pub platform: Option<&'static str>,
    pub sessions: &'a [Session],
    pub apply: &'a ApplyStatus,
}

/// Why this install cannot apply a release even when one is out. `None` when it can: a versioned
/// layout with a published bundle for this platform.
fn blocked_reason_for(kind: LayoutKind, platform: Option<&str>) -> Option<&'static str> {
    match kind {
        LayoutKind::Versioned => match platform {
            Some(_) => None,
            None => Some("there are no release bundles for this platform"),
        },
        LayoutKind::Source => Some("this mothership runs from a source checkout"),
        LayoutKind::Legacy => Some(
            "this install predates versioned updates; run the release installer once to move to it",
        ),
        LayoutKind::Unknown => Some(
            "this install is not laid out for in-place updates; run the release installer once",
        ),
    }
}

/// Why an apply would be refused right now: a colony mid-publish files its pull request from this
/// very process, which a re-exec would replace, and two applies at once would trample each other.
fn busy_reasons(sessions: &[Session], apply: &ApplyStatus) -> Vec<String> {
    let mut reasons = Vec::new();
    if sessions
        .iter()
        .any(|s| s.status == SessionStatus::Publishing)
    {
        reasons.push("a colony is publishing; its pull request would not open".to_string());
    }
    if apply.running() {
        reasons.push("an update is already being applied".to_string());
    }
    reasons
}

/// `COLONIZER_UPDATE_CHECK=off` keeps the check off whatever the switch says, the same escape hatch
/// `DO_NOT_TRACK` is for the live map: a machine that must not contact GitHub at all.
fn blocked_by(value: Option<&str>) -> Option<&'static str> {
    value
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        })
        .then_some("COLONIZER_UPDATE_CHECK")
}

/// Whether a release tag is newer than the release this build contains — the tag, not the describe
/// string, so a dev build `v0.1.3-12-gabc` is only told about releases past `v0.1.3`.
fn newer(latest: &str, installed: Option<&str>) -> bool {
    let Some(latest) = version::parse(latest) else {
        return false;
    };
    match installed.and_then(version::parse) {
        Some(installed) => latest.is_newer_than(&installed),
        // A build with no tag at all takes any tagged release as newer.
        None => true,
    }
}

/// The check loop: once shortly after start, then every interval; a failed (or skipped) check is
/// retried sooner; flipping the switch wakes the loop at once. When the check is off, no request is
/// made at all and the switch is the only way back in.
pub async fn run(app: Shared) {
    let updates = &app.updates;
    let mut wait = FIRST_CHECK_AFTER;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                wait = if updates.poll().await { CHECK_EVERY } else { RETRY_AFTER };
            }
            _ = updates.wake.notified() => wait = Duration::ZERO,
        }
    }
}

/// The full view as the handlers serve it: the check's answer plus everything about this install.
async fn view(app: &App) -> Value {
    let choice = app.updates.choice.lock().await.clone();
    let found = app.updates.found.lock().await.clone();
    let apply = app.updates.apply.lock().await.clone();
    let sessions = app.sessions.read().await.clone();
    let now = Now {
        kind: install::layout(&app.cfg).kind(),
        platform: install::platform(),
        sessions: &sessions,
        apply: &apply,
    };
    app.updates.view_with(&choice, &found, now)
}

/// `GET /api/update`
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/update` — `{"enabled": true|false}`
pub async fn put(
    State(app): State<Shared>,
    Json(body): Json<SetRequest>,
) -> crate::ApiResult<Value> {
    if app.updates.blocked.is_some() {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "the update check is kept off by the mothership's environment",
        ));
    }
    app.updates.set_enabled(body.enabled).await?;
    Ok(Json(view(&app).await))
}

/// `POST /api/update/check` — run a check now and report what it found.
pub async fn check_now(State(app): State<Shared>) -> crate::ApiResult<Value> {
    if let Some(variable) = app.updates.blocked {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            &format!("the update check is kept off by {variable} in the mothership's environment"),
        ));
    }
    if !app.updates.choice.lock().await.enabled {
        return Err(crate::client_error(
            StatusCode::CONFLICT,
            "the update check is switched off; switch it on first",
        ));
    }
    app.updates.poll().await;
    Ok(Json(view(&app).await))
}

/// The base to download a release's bundle and SHA256SUMS from. `COLONIZER_UPDATE_DOWNLOAD_URL`
/// overrides it, the way `COLONIZER_UPDATE_API` stands in for the GitHub API in tests.
fn download_base(repo: &str, tag: &str, override_url: Option<String>) -> String {
    override_url
        .map(|url| url.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("https://github.com/{repo}/releases/download/{tag}"))
}

/// `POST /api/update/apply` — download the newer release, install it beside this one, repoint `app`
/// and re-exec. Returns at once; `GET /api/update` follows the work. Refused, with the reason and
/// nothing changed, when the check is off, nothing newer is known, this install cannot apply, or
/// something is busy.
pub async fn apply(State(app): State<Shared>) -> crate::ApiResult<Value> {
    let current = view(&app).await;
    let refused = |reason: String| crate::client_error(StatusCode::CONFLICT, &reason);
    if !current["enabled"].as_bool().unwrap_or(false) {
        return Err(refused(
            "the update check is switched off; switch it on and check first".into(),
        ));
    }
    if !current["available"].as_bool().unwrap_or(false) {
        return Err(refused(
            "there is no newer release to apply; check again first".into(),
        ));
    }
    if let Some(reason) = current["blocked_reason"].as_str() {
        return Err(refused(reason.to_string()));
    }
    let busy: Vec<String> = serde_json::from_value(current["busy"].clone()).unwrap_or_default();
    if !busy.is_empty() {
        return Err(refused(busy.join("; ")));
    }
    let Some(tag) = current["latest"]["version"].as_str().map(str::to_string) else {
        return Err(refused(
            "there is no newer release to apply; check again first".into(),
        ));
    };
    // The layout was already checked via can_apply; this match only gets at the root.
    let install::Layout::Versioned { root, .. } = install::layout(&app.cfg) else {
        return Err(refused(
            "this install is not laid out for in-place updates; run the release installer once"
                .into(),
        ));
    };

    let started = {
        let mut status = app.updates.apply.lock().await;
        if status.running() {
            return Err(refused("an update is already being applied".into()));
        }
        let started = ApplyStatus {
            state: ApplyState::Downloading,
            version: Some(tag.clone()),
            started_at: Some(Utc::now()),
            generation: status.generation + 1,
            ..Default::default()
        };
        *status = started.clone();
        started
    };

    let base = download_base(
        &app.updates.repo,
        &tag,
        util::env_nonempty("COLONIZER_UPDATE_DOWNLOAD_URL"),
    );
    let background = app.clone();
    tokio::spawn(async move {
        // Progress is reported from inside the download loop, which cannot wait on a lock; it lands
        // on a channel and is folded into the status while the install runs.
        let (events, mut progress) = tokio::sync::mpsc::unbounded_channel::<install::Report>();
        let outcome: Result<()> = async {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .build()
                .context("building the download client")?;
            let worker = install::install(&root, &tag, &base, &client, move |report| {
                let _ = events.send(report);
            });
            let drain = async {
                while let Some(report) = progress.recv().await {
                    background.updates.note(report, started.generation).await;
                }
            };
            // The install's path is `app`'s new target; nothing here needs to keep it.
            let _ = tokio::join!(worker, drain).0?;
            // The link switch is the point of no return; until it lands the state stays
            // `installing`, so a failure here reports `failed` with the old release still running.
            install::point_app_at(&root, &tag)?;
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => restart(&background, &root, &tag, started.generation).await,
            Err(e) => {
                let mut status = background.updates.apply.lock().await;
                if status.generation == started.generation {
                    status.state = ApplyState::Failed;
                    status.finished_at = Some(Utc::now());
                    status.error = Some(util::truncate(&format!("{e:#}"), 2000));
                }
            }
        }
    });
    Ok(Json(view(&app).await))
}

/// The last leg of an apply: with the new release installed and `app` pointed at it, replace this
/// process with the new binary. It only comes back from the exec when the exec failed, which is
/// recorded so the UI says so and this mothership keeps serving.
async fn restart(app: &Shared, root: &Path, tag: &str, generation: u64) {
    {
        let mut status = app.updates.apply.lock().await;
        if status.generation != generation {
            return;
        }
        status.state = ApplyState::Restarting;
    }
    println!("update: {tag} is installed; restarting the mothership onto it");
    // Sessions first: the next process must find every colony exactly where this one leaves it.
    app.persist_sessions().await;
    // The mesh's headscale and tailscaled are children with kill_on_drop(true), and `exec` replaces
    // the process image without running destructors — so they are killed here or they orphan and
    // hold their ports. The next start's kill_stale (mesh.rs) restarts them against the same state.
    if let Some(mesh) = app.running_mesh().await {
        mesh.shutdown().await;
    }
    app.telemetry.goodbye().await;
    // A moment, so the UI's poll (and `colonizer update`) sees `restarting` before the connection
    // drops — which it must, when this process is replaced.
    tokio::time::sleep(Duration::from_secs(2)).await;
    // The colonies' microVMs are deliberately left running: sessions::recover reconnects each one on
    // the next start — that is what makes replacing this process safe for work in flight.
    let program = root.join("app/bin/colonizer"); // through the link, so the new version runs
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(&program)
        .args(std::env::args_os().skip(1))
        .exec();
    let mut status = app.updates.apply.lock().await;
    if status.generation == generation {
        status.state = ApplyState::Failed;
        status.finished_at = Some(Utc::now());
        status.error = Some(format!("restarting onto {tag} failed: {error}"));
    }
}

// ---------------------------------------------------------------------------
// `colonizer update`: a thin client of the running mothership
// ---------------------------------------------------------------------------

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1 << 20) as f64)
}

/// `colonizer update` asks the running mothership to check for a release, to apply it, and then
/// follows the progress until the mothership restarts onto it, gives up, or stops answering. All the
/// work happens in the mothership; this is only its command line. A connection that drops while the
/// state is `restarting` is the success path: the process was replaced mid-answer.
pub async fn command() -> Result<()> {
    use std::io::Write as _;
    let bind = crate::config::Settings::from_env()?.bind;
    let base = format!("http://{bind}");
    let url = |path: &str| format!("{base}{path}");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()?;

    print!("asking {base} to check for a new release… ");
    std::io::stdout().flush()?;
    let response = match client.post(url("/api/update/check")).send().await {
        Ok(response) => response,
        Err(e) if e.is_connect() => {
            println!();
            eprintln!("no mothership is listening on {base}; start it first (colonizer)");
            std::process::exit(1);
        }
        Err(e) => return Err(e.into()),
    };
    if !response.status().is_success() {
        bail!("{}", error_message(response).await);
    }
    let found: Value = response.json().await?;
    if !found["available"].as_bool().unwrap_or(false) {
        println!(
            "already up to date ({}).",
            found["latest"]["version"]
                .as_str()
                .unwrap_or("no release found by the last check")
        );
        return Ok(());
    }
    let tag = found["latest"]["version"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    println!("found {tag}; applying it");
    let response = client.post(url("/api/update/apply")).send().await?;
    if !response.status().is_success() {
        bail!("{}", error_message(response).await);
    }

    let mut last_state = String::new();
    let mut printed_mib = u64::MAX;
    let mut failures = 0u32;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status: Value = match client.get(url("/api/update")).send().await {
            Ok(response) if response.status().is_success() => match response.json().await {
                Ok(json) => {
                    failures = 0;
                    json
                }
                Err(e) => {
                    failures += 1;
                    if last_state == "restarting" {
                        return restarted(&tag, &base);
                    }
                    if failures > 5 {
                        return Err(anyhow::anyhow!(
                            "the mothership stopped answering while the update was {last_state}: {e}"
                        ));
                    }
                    continue;
                }
            },
            // While `restarting`, losing the mothership is the update succeeding: the process that
            // was answering has been replaced by the new one.
            _ if last_state == "restarting" => return restarted(&tag, &base),
            Ok(response) => {
                failures += 1;
                if failures > 5 {
                    bail!("the mothership answered {}", response.status());
                }
                continue;
            }
            Err(e) => {
                failures += 1;
                if failures > 5 {
                    bail!(
                        "the mothership stopped answering while the update was {last_state}: {e}"
                    );
                }
                continue;
            }
        };
        let apply = &status["apply"];
        let state = apply["state"].as_str().unwrap_or_default().to_string();
        // Read the transition before `last_state` is moved on below.
        let came_back = last_state == "restarting" && state == "idle";
        if state != last_state {
            if !last_state.is_empty() {
                println!();
            }
            print!("update: {state}");
            std::io::stdout().flush()?;
            last_state = state.clone();
            printed_mib = u64::MAX;
        }
        if state == "downloading" {
            let bytes = apply["bytes"].as_u64().unwrap_or(0);
            let whole = bytes / (1 << 20);
            if whole != printed_mib {
                printed_mib = whole;
                let total = apply["total"]
                    .as_u64()
                    .map(mib)
                    .unwrap_or_else(|| "?".into());
                print!("\rupdate: downloading, {} of {}", mib(bytes), total);
                std::io::stdout().flush()?;
            }
        }
        if state == "failed" {
            println!();
            bail!(
                "the update failed: {}",
                apply["error"].as_str().unwrap_or("unknown error")
            );
        }
        // The new process can come up and answer before the old connection is noticed as dropped.
        if came_back {
            println!();
            println!(
                "the mothership is back, on {}.",
                status["installed"]["version"]
                    .as_str()
                    .unwrap_or(tag.as_str())
            );
            return Ok(());
        }
    }
}

/// The happy ending the poll loop looks for: the connection dropped while `restarting`.
fn restarted(tag: &str, base: &str) -> Result<()> {
    println!();
    println!(
        "{tag} is in place; the mothership on {base} is restarting onto it and will be back in a moment."
    );
    Ok(())
}

/// The `{"error": "…"}` body an axum handler answers a refusal with.
async fn error_message(response: reqwest::Response) -> String {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    body["error"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("the mothership answered {status}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stand-in GitHub API serving one release, counting every request it receives.
    async fn fake_github(tag: &str) -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let tag = tag.to_string();
        let router = Router::new().route(
            "/repos/{owner}/{repo}/releases/latest",
            get(move || {
                let seen = seen.clone();
                let tag = tag.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "tag_name": tag,
                        "name": format!("Colonizer {tag}"),
                        "body": "Fixes and a feature",
                        "html_url": format!("https://github.com/Colonizer-dev/harness/releases/tag/{tag}"),
                        "published_at": "2026-09-01T12:00:00Z",
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, hits)
    }

    fn updates(api: String, blocked: Option<&'static str>) -> Updates {
        let dir =
            std::env::temp_dir().join(format!("colonizer-update-{}", crate::util::short_id()));
        Updates::with(dir.join("update.json"), api, DEFAULT_REPO.into(), blocked).unwrap()
    }

    #[test]
    fn the_environment_can_keep_the_check_off() {
        assert_eq!(blocked_by(None), None);
        assert_eq!(blocked_by(Some("on")), None);
        assert_eq!(blocked_by(Some("1")), None);
        assert_eq!(blocked_by(Some("")), None);
        assert_eq!(blocked_by(Some("off")), Some("COLONIZER_UPDATE_CHECK"));
        assert_eq!(blocked_by(Some("OFF")), Some("COLONIZER_UPDATE_CHECK"));
        assert_eq!(blocked_by(Some("false")), Some("COLONIZER_UPDATE_CHECK"));
        assert_eq!(blocked_by(Some("no")), Some("COLONIZER_UPDATE_CHECK"));
    }

    #[test]
    fn a_missing_or_broken_file_leaves_checking_on() {
        let dir =
            std::env::temp_dir().join(format!("colonizer-update-{}", crate::util::short_id()));
        assert!(
            Choice::default().enabled,
            "checking is on until the user says otherwise"
        );
        assert_eq!(Choice::load(&dir.join("update.json")), Choice::default());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("update.json"), "not json").unwrap();
        assert!(Choice::load(&dir.join("update.json")).enabled);
        Choice { enabled: false }
            .save(&dir.join("update.json"))
            .unwrap();
        assert_eq!(
            Choice::load(&dir.join("update.json")),
            Choice { enabled: false }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn availability_compares_against_the_tag_this_build_contains() {
        assert!(newer("v0.2.0", Some("v0.1.3")));
        assert!(newer("v0.2.0-rc.1", Some("v0.1.3")));
        assert!(
            !newer("v0.1.3", Some("v0.1.3")),
            "the tag the build contains is not an update"
        );
        assert!(!newer("v0.1.2", Some("v0.1.3")));
        assert!(
            newer("v0.2.0", Some("v0.1.3-12-gabc1234")),
            "a dev build is told about releases past its tag"
        );
        assert!(!newer("v0.1.3", Some("v0.1.3-12-gabc1234")));
        assert!(newer("v0.2.0", Some("v0.1.3-rc.1-4-gdeadbee")));
        assert!(!newer("v0.1.3-rc.2", Some("v0.1.3")));
        assert!(
            newer("v0.2.0", None),
            "a build with no tag at all takes any tagged release"
        );
        assert!(
            !newer("not a version", Some("v0.1.3")),
            "an unparseable latest is never an update"
        );
    }

    #[tokio::test]
    async fn a_newer_release_is_recorded_and_reported_available() {
        let (url, hits) = fake_github("v0.2.0").await;
        let updates = updates(url, None);
        assert!(
            updates.poll().await,
            "the check runs while it is switched on"
        );
        let found = updates.found.lock().await.clone();
        let latest = found.latest.expect("the check recorded a release");
        assert_eq!(latest.version, "v0.2.0");
        assert_eq!(latest.name.as_deref(), Some("Colonizer v0.2.0"));
        assert_eq!(latest.notes.as_deref(), Some("Fixes and a feature"));
        assert_eq!(latest.published_at.as_deref(), Some("2026-09-01T12:00:00Z"));
        assert!(
            latest
                .url
                .starts_with("https://github.com/Colonizer-dev/harness/releases/tag/")
        );
        assert!(found.last_checked_at.is_some());
        assert_eq!(found.last_error, None);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(newer(&latest.version, Some("v0.1.3")));
    }

    #[tokio::test]
    async fn an_equal_or_older_release_is_not_an_update() {
        let (url, _) = fake_github("v0.1.3").await;
        let updates = updates(url, None);
        assert!(updates.poll().await);
        let found = updates.found.lock().await.clone();
        assert_eq!(found.latest.unwrap().version, "v0.1.3");
        assert!(
            !newer("v0.1.3", Some("v0.1.3")),
            "the build's own tag is not an update"
        );
    }

    #[tokio::test]
    async fn a_switched_off_or_blocked_check_never_reaches_the_network() {
        let (url, hits) = fake_github("v0.9.0").await;
        let blocked = updates(url.clone(), Some("COLONIZER_UPDATE_CHECK"));
        assert!(
            !blocked.poll().await,
            "an environment-blocked check does not run"
        );
        let off = updates(url, None);
        off.set_enabled(false).await.unwrap();
        assert_eq!(Choice::load(&off.path), Choice { enabled: false });
        assert!(!off.poll().await, "a switched-off check does not run");
        assert!(off.found.lock().await.last_checked_at.is_none());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "the fake GitHub must receive no request"
        );
    }

    #[tokio::test]
    async fn a_failed_check_is_recorded_not_thrown() {
        // Nothing listens here, so the check fails fast and says so instead of losing the answer.
        let updates = updates("http://127.0.0.1:9".into(), None);
        assert!(!updates.poll().await);
        let found = updates.found.lock().await.clone();
        assert!(
            found.last_checked_at.is_some(),
            "a failed check is still stamped"
        );
        assert!(found.last_error.is_some());
        assert!(found.latest.is_none());
    }

    /// A colony with only the fields the busy decision reads, as `sessions.rs`'s own tests build one.
    fn colony(status: SessionStatus) -> Session {
        Session {
            id: String::new(),
            repo: "acme/repo".into(),
            org: "acme".into(),
            issue: None,
            issue_title: String::new(),
            instructions: String::new(),
            status,
            branch: String::new(),
            base: None,
            worktree: String::new(),
            git_admin_dir: None,
            sandbox: String::new(),
            mesh: None,
            local_port: None,
            agent: String::new(),
            autopilot: false,
            pr_url: None,
            error: None,
            cost_usd: None,
            model_usage: None,
            cleaned_up: false,
            attention: None,
            last_activity_at: None,
            boot_timing: None,
            app_dir: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn now<'a>(kind: LayoutKind, sessions: &'a [Session], apply: &'a ApplyStatus) -> Now<'a> {
        Now {
            kind,
            platform: platform(),
            sessions,
            apply,
        }
    }

    fn platform() -> Option<&'static str> {
        crate::install::platform_for("linux", "x86_64")
    }

    #[test]
    fn whether_an_update_can_apply_is_a_decision_table() {
        // A versioned layout on a machine with bundles: nothing stands in the way.
        assert_eq!(
            blocked_reason_for(LayoutKind::Versioned, Some("linux-x86_64")),
            None
        );
        assert_eq!(
            blocked_reason_for(LayoutKind::Versioned, Some("darwin-arm64")),
            None
        );
        assert_eq!(
            blocked_reason_for(LayoutKind::Versioned, None),
            Some("there are no release bundles for this platform")
        );
        assert_eq!(
            blocked_reason_for(LayoutKind::Source, Some("linux-x86_64")),
            Some("this mothership runs from a source checkout")
        );
        assert_eq!(
            blocked_reason_for(LayoutKind::Legacy, Some("linux-x86_64")),
            Some(
                "this install predates versioned updates; run the release installer once to move to it"
            )
        );
        assert_eq!(
            blocked_reason_for(LayoutKind::Unknown, Some("linux-x86_64")),
            Some(
                "this install is not laid out for in-place updates; run the release installer once"
            )
        );
    }

    #[test]
    fn busy_names_what_would_break_right_now() {
        let quiet = ApplyStatus::default();
        let running = ApplyStatus {
            state: ApplyState::Downloading,
            ..Default::default()
        };
        let done = ApplyStatus {
            state: ApplyState::Failed,
            ..Default::default()
        };

        assert!(busy_reasons(&[], &quiet).is_empty());
        // Live colonies, queued ones and finished ones are no obstacle; publishing is.
        for status in [
            SessionStatus::Starting,
            SessionStatus::Running,
            SessionStatus::WaitingForAnswer,
            SessionStatus::Idle,
            SessionStatus::Queued,
            SessionStatus::Stopped,
            SessionStatus::Failed,
            SessionStatus::PrOpened,
            SessionStatus::NoChanges,
        ] {
            assert!(
                busy_reasons(&[colony(status)], &quiet).is_empty(),
                "{status:?} is not a reason to refuse"
            );
        }
        assert_eq!(
            busy_reasons(&[colony(SessionStatus::Publishing)], &quiet),
            ["a colony is publishing; its pull request would not open"]
        );
        // An apply under way is refused; a failed or resting one is not.
        assert_eq!(
            busy_reasons(&[], &running),
            ["an update is already being applied"]
        );
        assert!(busy_reasons(&[], &done).is_empty());
        for state in [
            ApplyState::Verifying,
            ApplyState::Unpacking,
            ApplyState::Installing,
            ApplyState::Restarting,
        ] {
            let running = ApplyStatus {
                state,
                ..Default::default()
            };
            assert_eq!(
                busy_reasons(&[], &running),
                ["an update is already being applied"],
                "{state:?}"
            );
        }
        assert_eq!(
            busy_reasons(&[colony(SessionStatus::Publishing)], &running).len(),
            2,
            "both reasons at once"
        );
    }

    #[test]
    fn the_view_has_the_exact_keys_the_ui_builds_on() {
        let updates = updates("http://127.0.0.1:9".into(), None);
        let value = updates.view_with(
            &Choice::default(),
            &Found::default(),
            now(LayoutKind::Unknown, &[], &ApplyStatus::default()),
        );
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "apply",
                "available",
                "blocked_by",
                "blocked_reason",
                "busy",
                "can_apply",
                "enabled",
                "installed",
                "last_checked_at",
                "last_error",
                "latest",
                "repo",
            ]
        );
        assert_eq!(value["enabled"], true, "checking is on by default");
        assert_eq!(value["blocked_by"], Value::Null);
        assert_eq!(value["repo"], DEFAULT_REPO);
        assert_eq!(value["installed"]["version"], version::build().version);
        assert_eq!(value["installed"].as_object().unwrap().len(), 6);
        assert_eq!(value["last_checked_at"], Value::Null);
        assert_eq!(value["latest"], Value::Null);
        assert_eq!(value["available"], false);
        assert_eq!(value["can_apply"], false);
        assert_eq!(
            value["blocked_reason"],
            "this install is not laid out for in-place updates; run the release installer once"
        );
        assert_eq!(value["busy"], json!([]));
        let apply = &value["apply"];
        assert_eq!(apply["state"], "idle");
        assert_eq!(apply["version"], Value::Null);
        assert_eq!(apply["bytes"], 0);
        assert_eq!(apply["total"], Value::Null);
        assert_eq!(apply["started_at"], Value::Null);
        assert_eq!(apply["finished_at"], Value::Null);
        assert_eq!(apply["error"], Value::Null);
        assert_eq!(
            apply.as_object().unwrap().len(),
            7,
            "the generation counter is not serialised"
        );
    }

    #[tokio::test]
    async fn a_versioned_install_with_a_newer_release_can_apply() {
        let (url, _) = fake_github("v9.0.0").await;
        let updates = updates(url, None);
        updates.poll().await;

        // What the mothership runs from decides the answer; here a versioned layout on a machine
        // with published bundles, nothing busy.
        let sessions = [
            colony(SessionStatus::Running),
            colony(SessionStatus::Publishing),
        ];
        let apply = ApplyStatus::default();
        let value = updates.view_with(
            &Choice::default(),
            &updates.found.lock().await.clone(),
            now(LayoutKind::Versioned, &sessions, &apply),
        );
        assert_eq!(value["available"], true);
        assert_eq!(value["can_apply"], true);
        assert_eq!(value["blocked_reason"], Value::Null);
        assert_eq!(
            value["busy"],
            json!(["a colony is publishing; its pull request would not open"])
        );

        let value = updates.view_with(
            &Choice::default(),
            &updates.found.lock().await.clone(),
            now(LayoutKind::Versioned, &[], &apply),
        );
        assert_eq!(
            value["busy"],
            json!([]),
            "the same install with nothing publishing is free to apply"
        );

        let value = updates.view_with(
            &Choice::default(),
            &updates.found.lock().await.clone(),
            now(LayoutKind::Source, &[], &apply),
        );
        assert_eq!(
            value["can_apply"], false,
            "a source checkout cannot apply even with a release waiting"
        );
    }

    #[test]
    fn apply_states_serialise_as_the_ui_builds_on_them() {
        for (state, name) in [
            (ApplyState::Idle, "idle"),
            (ApplyState::Downloading, "downloading"),
            (ApplyState::Verifying, "verifying"),
            (ApplyState::Unpacking, "unpacking"),
            (ApplyState::Installing, "installing"),
            (ApplyState::Restarting, "restarting"),
            (ApplyState::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }

    #[test]
    fn an_apply_under_way_is_running_and_a_resting_one_is_not() {
        assert!(!ApplyStatus::default().running());
        assert!(
            !ApplyStatus {
                state: ApplyState::Failed,
                ..Default::default()
            }
            .running()
        );
        for state in [
            ApplyState::Downloading,
            ApplyState::Verifying,
            ApplyState::Unpacking,
            ApplyState::Installing,
            ApplyState::Restarting,
        ] {
            assert!(
                ApplyStatus {
                    state,
                    ..Default::default()
                }
                .running(),
                "{state:?}"
            );
        }
    }

    #[tokio::test]
    async fn progress_notes_only_land_while_their_apply_is_the_current_one() {
        let updates = updates("http://127.0.0.1:9".into(), None);
        updates.note(install::Report::Downloading, 1).await;
        {
            let apply = updates.apply.lock().await;
            assert_eq!(
                apply.state,
                ApplyState::Idle,
                "generation 0 ignores generation 1's reports"
            );
        }
        updates
            .note(
                install::Report::Bytes {
                    bytes: 42,
                    total: Some(84),
                },
                0,
            )
            .await;
        updates.note(install::Report::Installing, 0).await;
        let apply = updates.apply.lock().await.clone();
        assert_eq!(apply.state, ApplyState::Installing);
        assert_eq!(apply.bytes, 42);
        assert_eq!(apply.total, Some(84));
    }

    #[test]
    fn the_download_base_is_the_release_tagged_url_or_the_override() {
        assert_eq!(
            download_base(DEFAULT_REPO, "v0.2.0", None),
            format!("https://github.com/{DEFAULT_REPO}/releases/download/v0.2.0")
        );
        // The env override, read by the handler, lands here as Some: what an install test points at
        // a local server with.
        assert_eq!(
            download_base(
                DEFAULT_REPO,
                "v0.2.0",
                Some("http://127.0.0.1:9/files/".into())
            ),
            "http://127.0.0.1:9/files"
        );
    }
}
