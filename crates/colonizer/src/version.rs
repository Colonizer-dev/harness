//! What is installed, and whether a newer release exists.
//!
//! The check is **on by default**, with a switch in Settings and
//! `COLONIZER_UPDATE_CHECK=0` to keep it off from the environment. It asks
//! GitHub for the latest release of this repository and compares it with the
//! version stamped into the binary by `build.rs`. The request carries nothing
//! about the install beyond a user agent; the live map is separate and off by
//! default (`telemetry.rs`).
//!
//! Applying an update is not here. That needs the versioned app directory from
//! #90/#104 to land first, so an old release can stay readable while colonies
//! still mount plugins from it.

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
}

/// What the last check found. Named for the check, not the app: `State` is axum's extractor.
#[derive(Default)]
struct LastCheck {
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

impl Updates {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let blocked = matches!(
            std::env::var("COLONIZER_UPDATE_CHECK").as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
        .then_some("COLONIZER_UPDATE_CHECK");
        let url = crate::util::env_nonempty("COLONIZER_RELEASES_URL").unwrap_or_else(|| RELEASES_URL.to_string());
        Ok(Self {
            path: config_dir.join("updates.json"),
            url,
            blocked,
            choice: Mutex::new(Choice::load(&config_dir.join("updates.json"))),
            state: Mutex::new(LastCheck::default()),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15))
                .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    /// On unless the operator said otherwise, and never when the environment forbids it.
    pub async fn enabled(&self) -> bool {
        self.blocked.is_none() && self.choice.lock().await.enabled.unwrap_or(true)
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
        let response = self
            .client
            .get(&self.url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
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
        Ok(Latest {
            version,
            url: release["html_url"].as_str().unwrap_or_default().to_string(),
            notes: util::truncate(release["body"].as_str().unwrap_or_default(), 4000),
            published_at: release["published_at"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(Into::into),
        })
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

    /// The latest release tag, when it is newer than what is installed.
    pub async fn latest_release(&self) -> Option<String> {
        let state = self.state.lock().await;
        let build = build();
        state
            .latest
            .as_ref()
            .filter(|l| is_newer(build.release.as_deref(), &l.version))
            .map(|l| l.version.clone())
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
    let mut view = app.updates.view().await;
    view["apply"] = serde_json::to_value(app.updater.progress().await).unwrap_or(Value::Null);
    view["can_apply"] = match crate::update::blocker(app.cfg.assets.as_deref()) {
        Some(reason) => json!({ "ok": false, "reason": reason }),
        None => json!({ "ok": true, "reason": Value::Null }),
    };
    view
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/update` — `{"enabled": true|false}`
pub async fn put(State(app): State<Shared>, Json(body): Json<SetRequest>) -> crate::ApiResult<Value> {
    if let Some(blocked) = app.updates.blocked {
        return Err(crate::client_error(
            axum::http::StatusCode::CONFLICT,
            &format!("the update check is kept off by {blocked} in the mothership's environment"),
        ));
    }
    app.updates
        .set(body.enabled)
        .await
        .map_err(|e| crate::client_error(axum::http::StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    if body.enabled {
        app.updates.check().await;
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
        }
        tokio::time::sleep(CHECK_EVERY).await;
    }
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
        assert_eq!(Choice::load(&path), Choice { enabled: None });
        assert!(Choice::load(&path).enabled.unwrap_or(true));

        Choice { enabled: Some(false) }.save(&path).unwrap();
        assert_eq!(Choice::load(&path), Choice { enabled: Some(false) });
        assert!(!Choice::load(&path).enabled.unwrap_or(true), "off must survive a restart");

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
        let put = put(State(app.clone()), Json(SetRequest { enabled: false })).await.unwrap().0;
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
}
