//! What is installed, and whether a newer release exists.
//!
//! The check is **on by default**, with a switch in Settings and
//! `COLONIZER_UPDATE_CHECK=0` to keep it off from the environment. It asks
//! GitHub for the latest release of this repository and compares it with the
//! version stamped into the binary by `build.rs`. The request carries nothing
//! about the install beyond a user agent; the live map is separate and off by
//! default (`telemetry.rs`).
//!
//! Applying an update is not here: `update.rs` installs a release into the
//! versioned app directory and restarts into it (`colonizer update`,
//! `POST /api/update/apply`).

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{Json, extract::State};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{Shared, util};

/// Where the check looks. Overridable so a fork, or a test, does not ask about this repository.
const RELEASES_URL: &str = "https://api.github.com/repos/Colonizer-dev/harness/releases/latest";

/// A few hours, as the issue puts it. Well inside GitHub's unauthenticated rate limit.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Long enough after start that it does not compete with booting colonies.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);

/// How stale the cache may be before a `/api/update` read forces a fresh
/// check, rather than waiting for the next [`CHECK_EVERY`] tick. Short enough
/// that a release published while the mothership was already running — the
/// issue's "a few minutes" — is seen the next time anything asks; long enough
/// that the Cockpit polling apply progress every few seconds does not turn
/// into a burst of requests to GitHub.
const STALE_AFTER: Duration = Duration::from_secs(5 * 60);

/* ----------------------------------------------------------------- the build */

/// What this binary was built from.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Build {
    /// `v0.1.4`, or `v0.1.4-12-gabc1234` for a build after that tag.
    pub version: String,
    /// `None` when built from a source package with no git history.
    pub commit: Option<String>,
    /// The tree had uncommitted changes at build time.
    pub dirty: bool,
    pub built_at: DateTime<Utc>,
    /// The last release tag this build contains, if any: what an update compares against.
    pub release: Option<String>,
    /// Not a release: built after a tag, from a modified tree, or with no tag at all.
    /// The UI needs this to avoid offering an update to someone running their own build.
    pub development: bool,
}

impl Build {
    /// One line, for `colonizer version` and anywhere a log wants it.
    ///
    /// `v0.1.4 (1367191, built 2026-09-17T17:21:32Z)`, with `development` said
    /// plainly rather than left for the reader to infer from the shape of a tag.
    pub fn line(&self) -> String {
        let mut line = self.version.clone();
        if let Some(commit) = &self.commit {
            line.push_str(&format!(" ({}", &commit[..commit.len().min(7)]));
            if self.dirty {
                line.push_str(", modified tree");
            }
            line.push_str(&format!(", built {})", self.built_at.format("%Y-%m-%dT%H:%M:%SZ")));
        }
        if self.development {
            line.push_str(" — development build");
        }
        line
    }
}

pub fn build() -> &'static Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    BUILD.get_or_init(|| {
        let describe = env!("COLONIZER_DESCRIBE").trim();
        let commit = env!("COLONIZER_COMMIT").trim();
        let built_at = env!("COLONIZER_BUILT_AT").trim().parse::<i64>().unwrap_or_default();
        Build {
            // A describe that names no tag (`abc1234`) is not a version; fall back to the
            // crate's own, which is what a release bumps.
            version: match Semver::parse(describe) {
                Some(_) => describe.to_string(),
                None => format!("v{}", env!("CARGO_PKG_VERSION")),
            },
            commit: (!commit.is_empty()).then(|| commit.to_string()),
            dirty: describe.ends_with("-dirty"),
            built_at: Utc.timestamp_opt(built_at, 0).single().unwrap_or_else(Utc::now),
            // A describe that is exactly a tag is a release; anything else — a tag plus
            // commits, a dirty tree, or no tag at all — is not.
            development: Semver::parse(describe).is_none() || describe.contains("-g") || describe.ends_with("-dirty"),
            release: Semver::parse(describe).map(|v| v.to_tag()).or_else(|| {
                // No git: the crate version is the release this was cut from.
                Semver::parse(env!("CARGO_PKG_VERSION")).map(|v| v.to_tag())
            }),
        }
    })
}

/* --------------------------------------------------------------- comparison */

/// Just enough of a semantic version to order releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Semver {
    /// Reads the release a tag or `git describe` names.
    ///
    /// `v0.1.4`, `0.1.4`, `v0.1.4-12-gabc1234` and `v0.1.4-dirty` all read as
    /// 0.1.4: a build after a tag is measured against the tag it contains, so
    /// it is only told about releases newer than that. Anything else — a bare
    /// commit, a prerelease, an empty string — is not a release.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
        let core = text.split('-').next()?;
        let mut parts = core.split('.');
        let number = |part: Option<&str>| -> Option<u64> {
            let part = part?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let major = number(parts.next())?;
        let minor = number(parts.next())?;
        let patch = number(parts.next())?;
        // `0.1.4.1` is not a version this understands; refusing beats guessing.
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch })
    }

    pub fn to_tag(self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Whether `latest` is worth telling the operator about.
///
/// A dirty build is still compared on the tag it descends from: someone running
/// a modified checkout still wants to know a release happened.
pub fn is_newer(installed: Option<&str>, latest: &str) -> bool {
    match (installed.and_then(Semver::parse), Semver::parse(latest)) {
        (Some(installed), Some(latest)) => latest > installed,
        // Nothing to compare against: say nothing rather than cry wolf.
        _ => false,
    }
}

/* ------------------------------------------------------------------ setting */

/// The operator's answer, in `<config>/updates.json`. Absent means on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Choice {
    #[serde(default)]
    enabled: Option<bool>,
    /// `updates.auto_apply` (issue #1191); absent means off.
    #[serde(default)]
    auto_apply: Option<crate::update::AutoApply>,
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
        util::write_private(path, &serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }
}

/// A release as the check found it.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Latest {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub published_at: Option<DateTime<Utc>>,
    /// What the release says about itself to the mothership that would install it (issue #1097):
    /// read from the block in its body, which `notes` leaves out. Answered filtered, as
    /// `/api/update`'s `notices`, so not serialized here.
    #[serde(skip)]
    pub notices: Vec<crate::update_notices::Notice>,
}

/// What the last check found. Named for the check, not the app: `State` is axum's extractor.
#[derive(Default)]
struct LastCheck {
    /// What the last auto-apply poll did or is waiting for (issue #1191), and when it changed.
    auto_apply_last: Option<(DateTime<Utc>, String)>,
    last_checked: Option<DateTime<Utc>>,
    latest: Option<Latest>,
    error: Option<String>,
}

pub struct Updates {
    path: PathBuf,
    url: String,
    /// Set when the environment keeps the check off, so the UI can say why.
    pub blocked: Option<&'static str>,
    choice: Mutex<Choice>,
    state: Mutex<LastCheck>,
    client: reqwest::Client,
}

/// The release feed to ask: `COLONIZER_RELEASES_URL` when set, so a fork or a test does not ask
/// about this repository.
fn releases_url() -> String {
    crate::util::env_nonempty("COLONIZER_RELEASES_URL").unwrap_or_else(|| RELEASES_URL.to_string())
}

/// The client every release request uses: short timeouts, and a user agent naming this build.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// The one GitHub request: the latest release, with drafts and prereleases refused.
///
/// Shared by the background check and `colonizer update --check`, so a one-off check makes the
/// same request the mothership would.
async fn fetch_release(client: &reqwest::Client, url: &str) -> Result<Latest> {
    let response = client.get(url).header("Accept", "application/vnd.github+json").send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("GitHub answered {status}");
    }
    let release: Value = serde_json::from_str(&body).context("the release feed was not JSON")?;
    // A draft or prerelease is not something to nudge an operator towards.
    if release["draft"].as_bool().unwrap_or(false) || release["prerelease"].as_bool().unwrap_or(false) {
        anyhow::bail!("the latest release is a draft or prerelease");
    }
    let version = release["tag_name"].as_str().context("the release has no tag")?.to_string();
    let body = release["body"].as_str().unwrap_or_default();
    Ok(Latest {
        version,
        url: release["html_url"].as_str().unwrap_or_default().to_string(),
        // The notices are read from the whole body, before the notes are clipped for display.
        notices: crate::update_notices::parse(body),
        notes: util::truncate(&crate::update_notices::strip(body), 4000),
        published_at: release["published_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(Into::into),
    })
}

/// Asks GitHub for the latest release right now, for `colonizer update --check`.
///
/// A fresh client and no stored state, so a one-off check needs no running mothership and
/// nothing written to `<config>`. The same request and the same draft/prerelease rules as the
/// mothership's own `fetch`.
pub async fn latest_release() -> Result<Latest> {
    fetch_release(&client()?, &releases_url()).await
}

impl Updates {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let blocked = matches!(
            std::env::var("COLONIZER_UPDATE_CHECK").as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
        .then_some("COLONIZER_UPDATE_CHECK");
        Ok(Self {
            path: config_dir.join("updates.json"),
            url: releases_url(),
            blocked,
            choice: Mutex::new(Choice::load(&config_dir.join("updates.json"))),
            state: Mutex::new(LastCheck::default()),
            client: client()?,
        })
    }

    /// On unless the operator said otherwise, and never when the environment forbids it.
    pub async fn enabled(&self) -> bool {
        self.blocked.is_none() && self.choice.lock().await.enabled.unwrap_or(true)
    }

    /// The auto-apply mode; off unless the operator chose otherwise.
    pub async fn auto_apply(&self) -> crate::update::AutoApply {
        self.choice.lock().await.auto_apply.unwrap_or_default()
    }

    async fn set_auto_apply(&self, mode: crate::update::AutoApply) -> Result<()> {
        let mut choice = self.choice.lock().await;
        choice.auto_apply = Some(mode);
        choice.save(&self.path)
    }

    /// Records what the auto-apply poll did. A wait is logged only when its text changes, so a
    /// colony holding the spare slot for hours is one line, not one per check.
    pub(crate) async fn note_auto_apply(&self, message: String, waiting: bool) {
        let mut state = self.state.lock().await;
        let same = waiting && state.auto_apply_last.as_ref().is_some_and(|(_, last)| *last == message);
        if !same {
            eprintln!("update: auto-apply: {message}");
            state.auto_apply_last = Some((Utc::now(), message));
        }
    }

    async fn set(&self, enabled: bool) -> Result<()> {
        let mut choice = self.choice.lock().await;
        choice.enabled = Some(enabled);
        choice.save(&self.path)?;
        if !enabled {
            // Stop showing a banner the moment it is switched off.
            *self.state.lock().await = LastCheck::default();
        }
        Ok(())
    }

    /// Asks GitHub for the latest release. Only ever called behind [`Updates::enabled`].
    async fn fetch(&self) -> Result<Latest> {
        fetch_release(&self.client, &self.url).await
    }

    async fn check(&self) {
        let result = self.fetch().await;
        let mut state = self.state.lock().await;
        state.last_checked = Some(Utc::now());
        match result {
            Ok(latest) => {
                state.latest = Some(latest);
                state.error = None;
            }
            // Keep the last good answer: a flaky network should not erase a
            // release the operator has already been told about.
            Err(e) => state.error = Some(util::truncate(&format!("{e:#}"), 300)),
        }
    }

    /// Refreshes the cache when it is older than [`STALE_AFTER`], or has never
    /// run. Called from every `/api/update` read, so `colonizer update` — and
    /// the Cockpit — see a release published after the mothership started
    /// without waiting for the periodic [`CHECK_EVERY`] tick (issue #820).
    async fn refresh_if_stale(&self) {
        if !self.enabled().await {
            return;
        }
        let stale = match self.state.lock().await.last_checked {
            Some(t) => Utc::now().signed_duration_since(t).num_seconds() >= STALE_AFTER.as_secs() as i64,
            None => true,
        };
        if stale {
            self.check().await;
        }
    }

    /// The latest release tag the check has seen, whether or not it is newer
    /// than what is installed. Applying needs it even when it is not newer:
    /// `--force` installs it anyway, and the refusal names it either way.
    pub async fn latest_known(&self) -> Option<String> {
        self.state.lock().await.latest.as_ref().map(|l| l.version.clone())
    }

    /// The whole latest release the check has seen, notices included.
    pub async fn latest(&self) -> Option<Latest> {
        self.state.lock().await.latest.clone()
    }

    async fn view(&self) -> Value {
        let state = self.state.lock().await;
        let build = build();
        let available = state
            .latest
            .as_ref()
            .is_some_and(|l| is_newer(build.release.as_deref(), &l.version));
        json!({
            "enabled": self.enabled().await,
            "auto_apply": self.auto_apply().await.as_str(),
            "auto_apply_last": state.auto_apply_last.as_ref().map(|(at, what)| json!({"at": at, "what": what})),
            "blocked_by": self.blocked,
            "installed": build,
            "latest": state.latest,
            "available": available,
            "last_checked": state.last_checked,
            "error": state.error,
        })
    }
}

/* ------------------------------------------------------------------- routes */

/// `GET /api/version` — what is installed.
pub async fn version(State(app): State<Shared>) -> Json<&'static Build> {
    let _ = &app;
    Json(build())
}

/// `GET /api/update` — the installed version, the latest release, whether to act,
/// and how an update being applied is getting on.
pub async fn status(State(app): State<Shared>) -> Json<Value> {
    Json(full_status(&app).await)
}

/// The one `/api/update` shape, shared by the GET and the PUT: the base view
/// plus the apply progress and the can-apply verdict. The toggle used to answer
/// with the bare view, and the Settings pane dereferences `apply.colonies` and
/// `can_apply.ok` unconditionally — so flipping the switch blanked the screen
/// until a refresh re-fetched the full shape.
pub async fn full_status(app: &Shared) -> Value {
    app.updates.refresh_if_stale().await;
    let mut view = app.updates.view().await;
    // The refusal names the latest release when there is one; read it
    // regardless of newer-ness, so a development build hears both versions.
    let latest = app.updates.latest_known().await;
    view["apply"] = serde_json::to_value(app.updater.progress().await).unwrap_or(Value::Null);
    view["can_apply"] = match crate::update::blocker(app.cfg.assets.as_deref())
        .or_else(|| crate::update::refusal(build(), latest.as_deref(), false))
    {
        Some(reason) => json!({ "ok": false, "reason": reason }),
        None => json!({ "ok": true, "reason": Value::Null }),
    };
    // Issue #1097: the notices newer than this build, the colonies still on a previous version,
    // the restarts onto this one, and how a source build switches to releases.
    crate::update_notices::extend(app, &mut view).await;
    view
}

#[derive(Deserialize)]
pub struct SetRequest {
    #[serde(default)]
    enabled: Option<bool>,
    /// `off`, `when_idle` or `always` (issue #1191).
    #[serde(default)]
    auto_apply: Option<String>,
}

/// `PUT /api/update` — `{"enabled": true|false}`
pub async fn put(State(app): State<Shared>, Json(body): Json<SetRequest>) -> crate::ApiResult<Value> {
    if let Some(blocked) = app.updates.blocked {
        return Err(crate::client_error(
            axum::http::StatusCode::CONFLICT,
            &format!("the update check is kept off by {blocked} in the mothership's environment"),
        ));
    }
    let failed = |e: anyhow::Error| crate::client_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}"));
    if let Some(mode) = &body.auto_apply {
        let Some(mode) = crate::update::AutoApply::parse(mode) else {
            return Err(crate::client_error(
                axum::http::StatusCode::BAD_REQUEST,
                "auto_apply is one of off, when_idle or always",
            ));
        };
        app.updates.set_auto_apply(mode).await.map_err(failed)?;
    }
    if let Some(enabled) = body.enabled {
        app.updates.set(enabled).await.map_err(failed)?;
        if enabled {
            app.updates.check().await;
        }
    }
    Ok(Json(full_status(&app).await))
}

/// Checks shortly after start, then every few hours, and only while switched on.
///
/// The gate is here, before anything touches the network, so "off" means no
/// request rather than a request whose answer is discarded.
pub async fn run(app: Shared) {
    tokio::time::sleep(FIRST_CHECK_AFTER).await;
    loop {
        if app.updates.enabled().await {
            app.updates.check().await;
            // Issue #1191: on the same cadence, install what the check found when the operator said so.
            crate::update::auto_apply_poll(&app).await;
        }
        tokio::time::sleep(CHECK_EVERY).await;
    }
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(run(app.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/version", routing::get(version))
        .route("/api/update", routing::get(status).put(put))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_reads_as_its_release() {
        assert_eq!(
            Semver::parse("v0.1.4"),
            Some(Semver {
                major: 0,
                minor: 1,
                patch: 4
            })
        );
        assert_eq!(Semver::parse("0.1.4"), Semver::parse("v0.1.4"));
        assert_eq!(
            Semver::parse(" v1.20.300 "),
            Some(Semver {
                major: 1,
                minor: 20,
                patch: 300
            })
        );
    }

    #[test]
    fn a_build_after_a_tag_reads_as_that_tag() {
        // The issue's rule: a development build is only told about releases
        // newer than the last tag it contains.
        assert_eq!(Semver::parse("v0.1.4-12-gabc1234"), Semver::parse("v0.1.4"));
        assert_eq!(Semver::parse("v0.1.4-12-gabc1234-dirty"), Semver::parse("v0.1.4"));
        assert_eq!(Semver::parse("v0.1.4-dirty"), Semver::parse("v0.1.4"));
    }

    #[test]
    fn what_is_not_a_release_is_not_guessed_at() {
        for text in ["", "abc1234", "gabc1234", "v0.1", "0.1.4.1", "v.1.4", "vx.y.z", "latest"] {
            assert_eq!(Semver::parse(text), None, "{text:?} should not parse as a release");
        }
    }

    #[test]
    fn newer_releases_are_offered_and_older_ones_are_not() {
        assert!(is_newer(Some("v0.1.4"), "v0.1.5"));
        assert!(is_newer(Some("v0.1.4"), "v0.2.0"));
        assert!(is_newer(Some("v0.9.9"), "v1.0.0"));
        assert!(!is_newer(Some("v0.1.4"), "v0.1.4"), "the installed release is not an update");
        assert!(!is_newer(Some("v0.2.0"), "v0.1.9"), "an older release is not an update");
        // 10 > 9: string ordering would get this wrong.
        assert!(is_newer(Some("v0.9.0"), "v0.10.0"));
    }

    #[test]
    fn a_dev_build_hears_about_the_release_after_its_tag() {
        assert!(
            is_newer(Some("v0.1.4"), "v0.1.5"),
            "a build after v0.1.4 wants to know about v0.1.5"
        );
        assert!(!is_newer(Some("v0.1.4"), "v0.1.4"), "its own tag is not news");
    }

    #[test]
    fn an_unknown_installed_version_is_never_told_it_is_behind() {
        // Better to say nothing than to nag someone whose build cannot be placed.
        assert!(!is_newer(None, "v9.9.9"));
        assert!(!is_newer(Some("abc1234"), "v9.9.9"));
        assert!(!is_newer(Some("v0.1.4"), "not-a-tag"));
    }

    #[test]
    fn a_release_build_is_not_a_development_build() {
        // These are the shapes `git describe --tags --always --dirty` produces.
        let dev = |d: &str| Semver::parse(d).is_none() || d.contains("-g") || d.ends_with("-dirty");
        assert!(!dev("v0.1.4"), "an exact tag is a release");
        assert!(dev("v0.1.4-12-gabc1234"), "commits after a tag");
        assert!(dev("v0.1.4-dirty"), "a modified tree");
        assert!(dev("abc1234"), "no tag at all");
        assert!(dev(""), "no git at all");
    }

    #[test]
    fn the_one_line_form_names_the_commit_and_says_when_it_is_a_development_build() {
        let b = build();
        let line = b.line();
        assert!(line.starts_with(&b.version), "{line}");
        if let Some(commit) = &b.commit {
            assert!(line.contains(&commit[..7]), "{line}");
        }
        assert_eq!(line.contains("development build"), b.development, "{line}");
    }

    #[test]
    fn the_stamped_build_describes_itself() {
        let b = build();
        assert!(
            b.version.starts_with('v'),
            "version should be tag-shaped, got {:?}",
            b.version
        );
        // Either a real release was parsed out of it, or there was no git and
        // the crate version stood in; both are a release to compare against.
        assert!(b.release.is_some(), "a build should know which release it descends from");
        assert!(Semver::parse(b.release.as_deref().unwrap()).is_some());
    }

    #[test]
    fn the_choice_round_trips_and_absent_means_on() {
        let dir = std::env::temp_dir().join(format!("colonizer-updates-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("updates.json");

        // Nothing written yet: the check is on.
        assert_eq!(Choice::load(&path), Choice::default());
        assert!(Choice::load(&path).enabled.unwrap_or(true));

        Choice {
            enabled: Some(false),
            auto_apply: None,
        }
        .save(&path)
        .unwrap();
        assert_eq!(
            Choice::load(&path),
            Choice {
                enabled: Some(false),
                auto_apply: None
            }
        );
        assert!(!Choice::load(&path).enabled.unwrap_or(true), "off must survive a restart");

        // Issue #1191: auto-apply is off unless chosen, and the choice survives a restart.
        assert_eq!(
            Choice::load(&path).auto_apply.unwrap_or_default(),
            crate::update::AutoApply::Off
        );
        Choice {
            enabled: None,
            auto_apply: Some(crate::update::AutoApply::WhenIdle),
        }
        .save(&path)
        .unwrap();
        assert_eq!(Choice::load(&path).auto_apply, Some(crate::update::AutoApply::WhenIdle));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"when_idle\""), "{text}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The toggle must answer the same shape as the status read: the Settings
    /// pane dereferences `apply.colonies` and `can_apply.ok` unconditionally,
    /// so a PUT that returned the bare view blanked the screen until refresh.
    #[tokio::test]
    async fn toggle_returns_the_full_update_shape() {
        let dir = std::env::temp_dir().join(format!("colonizer-updates-{}", crate::util::short_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let app = crate::tests::test_app(&dir);
        // Off, so no check call reaches the network.
        let put = put(
            State(app.clone()),
            Json(SetRequest {
                enabled: Some(false),
                auto_apply: None,
            }),
        )
        .await
        .unwrap()
        .0;
        let get = status(State(app.clone())).await.0;
        for key in [
            "enabled",
            "blocked_by",
            "installed",
            "latest",
            "available",
            "last_checked",
            "error",
            "apply",
            "can_apply",
        ] {
            assert!(put.get(key).is_some(), "PUT shape is missing {key}");
            assert!(get.get(key).is_some(), "GET shape is missing {key}");
        }
        assert_eq!(put["enabled"], false);
        assert!(put["can_apply"]["ok"].is_boolean());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_stale_cache_refreshes_on_the_next_read() {
        use axum::{Json, Router, routing::get};

        let mock = Router::new().route(
            "/release",
            get(|| async {
                Json(json!({
                    "tag_name": "v0.1.11",
                    "html_url": "https://example.com/v0.1.11",
                    "body": "notes",
                    "published_at": "2026-10-02T12:00:00Z",
                    "draft": false,
                    "prerelease": false,
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

        let dir = std::env::temp_dir().join(format!("colonizer-updates-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut updates = Updates::new(&dir).unwrap();
        updates.url = format!("http://{addr}/release");
        {
            // The mothership's own scenario from issue #820: it checked once,
            // a while ago, and only ever saw v0.1.10. A release published
            // since then must not wait for the next six-hour tick.
            let mut state = updates.state.lock().await;
            state.last_checked = Some(Utc::now() - chrono::Duration::hours(1));
            state.latest = Some(Latest {
                version: "v0.1.10".into(),
                ..Default::default()
            });
        }

        updates.refresh_if_stale().await;

        assert_eq!(updates.latest_known().await, Some("v0.1.11".into()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_fresh_cache_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("colonizer-updates-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut updates = Updates::new(&dir).unwrap();
        // Unreachable: if `refresh_if_stale` fetched anyway, this would
        // record an error, which the assertion below would catch.
        updates.url = "http://127.0.0.1:1/would-error".into();
        {
            let mut state = updates.state.lock().await;
            state.last_checked = Some(Utc::now());
            state.latest = Some(Latest {
                version: "v0.1.10".into(),
                ..Default::default()
            });
        }

        updates.refresh_if_stale().await;

        let state = updates.state.lock().await;
        assert_eq!(state.latest.as_ref().map(|l| l.version.as_str()), Some("v0.1.10"));
        assert!(state.error.is_none(), "a fresh cache must not trigger a fetch");
        std::fs::remove_dir_all(&dir).ok();
    }
}
