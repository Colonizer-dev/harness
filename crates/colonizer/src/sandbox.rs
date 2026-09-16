//! Sandbox module: microsandbox provider.

use axum::extract::State;

use crate::util::{exec, mount_spec};
use anyhow::{Context, Result};
use std::{collections::HashSet, path::PathBuf};
use tokio::process::Command;

pub struct Mount {
    pub source: PathBuf,
    pub target: String,
    pub read_only: bool,
}

pub struct Secret {
    pub env: String,
    pub value: String,
    pub hosts: Vec<String>,
}

pub struct BootSpec {
    pub name: String,
    pub image: String,
    pub cpus: u64,
    pub memory: String,
    pub root_disk: String,
    pub max_duration: String,
    pub workdir: String,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub secrets: Vec<Secret>,
    pub net_profiles: Vec<String>,
    pub net_rules: Vec<String>,
    /// `(host_port, guest_port)` published on 127.0.0.1.
    pub publish: Option<(u16, u16)>,
    pub command: Vec<String>,
}

/// Boots a detached microVM running `spec.command` as its main process.
pub async fn boot(msb: &str, spec: &BootSpec) -> Result<()> {
    let mut cmd = Command::new(msb);
    cmd.args(["run", "--detach", "--replace", "--quiet", "--name", spec.name.as_str()])
        .arg("--cpus")
        .arg(spec.cpus.to_string())
        .args(["--memory", spec.memory.as_str()])
        .args(["--root-disk", spec.root_disk.as_str()])
        .args(["--max-duration", spec.max_duration.as_str()])
        .args(["--workdir", spec.workdir.as_str()]);
    for mount in &spec.mounts {
        cmd.arg("-v").arg(mount_spec(&mount.source, &mount.target, mount.read_only)?);
    }
    for (key, value) in &spec.env {
        cmd.arg("-e").arg(format!("{key}={value}"));
    }
    for secret in &spec.secrets {
        // The value stays in msb's host process; the guest env only holds a placeholder.
        cmd.arg("--secret").arg(format!("{}@{}", secret.env, secret.hosts.join(","))).env(&secret.env, &secret.value);
    }
    if !spec.net_profiles.is_empty() {
        cmd.arg("--net").arg(spec.net_profiles.join(","));
    }
    for rule in &spec.net_rules {
        cmd.arg("--net-rule").arg(rule);
    }
    if let Some((host, guest)) = spec.publish {
        cmd.arg("-p").arg(format!("127.0.0.1:{host}:{guest}"));
    }
    cmd.arg(&spec.image).arg("--").args(&spec.command);
    exec(&mut cmd).await.with_context(|| format!("microVM {} failed to boot", spec.name))?;
    Ok(())
}

pub async fn remove(msb: &str, name: &str) {
    let _ = exec(Command::new(msb).args(["rm", "--force", "--quiet", name])).await;
}

pub async fn running(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["ls", "--running", "--quiet"])).await?;
    Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Images already in the local cache, as `msb image list` reports them.
///
/// Used to decide whether a launch is about to pay for a download. A failure
/// here is not fatal anywhere it is used: the worst case is pulling an image
/// that was already cached, which `msb pull` turns into a no-op.
pub async fn cached_images(msb: &str) -> Result<HashSet<String>> {
    let out = exec(Command::new(msb).args(["image", "list"])).await?;
    Ok(out
        .lines()
        .skip(1) // header
        .filter_map(|line| line.split_whitespace().next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect())
}

/// A bare `name` is cached as `name:latest`, so a lookup has to try both.
fn matches_cache(image: &str, cached: &HashSet<String>) -> bool {
    if cached.contains(image) {
        return true;
    }
    if image.contains(':') {
        false
    } else {
        cached.contains(&format!("{image}:latest"))
    }
}

pub async fn is_cached(msb: &str, image: &str) -> bool {
    // A cache check that fails is reported as "not cached": pulling an image
    // that was already there is a no-op, but skipping a pull that was needed
    // puts the download back on the launch path unannounced.
    match cached_images(msb).await {
        Ok(images) => matches_cache(image, &images),
        Err(_) => false,
    }
}

/// Downloads an image into the local cache. A no-op when it is already there.
pub async fn pull(msb: &str, image: &str) -> Result<()> {
    exec(Command::new(msb).args(["pull", "--quiet", image])).await?;
    Ok(())
}

/// Where a background image pull has got to.
///
/// There is no percentage here on purpose. `msb pull` draws its progress bar
/// only on a terminal; piped, it prints a single line when it is finished, and
/// `--info` adds nothing but migration logs. Scraping the bar through a pty
/// would parse an undocumented format that can change with any msb release, so
/// the UI shows what is actually known: which image, since when, and the
/// outcome.
#[derive(Clone, Debug, Default, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PullState {
    #[default]
    Idle,
    /// Already in the local cache; nothing to do.
    Cached,
    Pulling,
    Done,
    Failed,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct PullStatus {
    pub image: String,
    pub state: PullState,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    /// Bumped for every pull started, so a slow pull that finishes after a newer
    /// one was requested cannot overwrite the newer one's status.
    #[serde(skip)]
    pub generation: u64,
}

/// The image the sandbox module is configured to boot, after the stack preset.
fn configured_image(app: &crate::App, modules: &crate::config::ModulesConfig) -> String {
    let schema = crate::modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let preset = crate::config::setting_str(&modules.sandbox, &schema, "preset");
    let settings = crate::config::with_preset(&modules.sandbox, &crate::presets::defaults(&preset));
    crate::config::setting_str(&settings, &schema, "image")
}

/// `POST /api/sandbox/pull` — start downloading the configured colony image.
///
/// Returns at once. A cold pull of the default image measured 108 s, which is
/// too long to hold a request open, so the download runs in the background and
/// `GET /api/sandbox/pull` reports on it. Calling this again while the same
/// image is already pulling returns the running pull rather than starting a
/// second one.
pub async fn pull_configured(State(app): State<crate::Shared>) -> crate::ApiResult<PullStatus> {
    let modules = app.modules.read().await.clone();
    let image = configured_image(&app, &modules);
    if image.is_empty() {
        return Err(crate::client_error(axum::http::StatusCode::BAD_REQUEST, "no colony image is configured"));
    }

    {
        let status = app.pull.lock().await;
        if status.state == PullState::Pulling && status.image == image {
            return Ok(axum::Json(status.clone()));
        }
    }

    if is_cached(&app.cfg.msb, &image).await {
        let mut status = app.pull.lock().await;
        *status = PullStatus { image, state: PullState::Cached, generation: status.generation, ..Default::default() };
        return Ok(axum::Json(status.clone()));
    }

    let started = {
        let mut status = app.pull.lock().await;
        let generation = status.generation + 1;
        *status = PullStatus {
            image: image.clone(),
            state: PullState::Pulling,
            started_at: Some(chrono::Utc::now()),
            finished_at: None,
            error: None,
            generation,
        };
        status.clone()
    };

    let background = app.clone();
    tokio::spawn(async move {
        let result = pull(&background.cfg.msb, &image).await;
        let mut status = background.pull.lock().await;
        if status.generation != started.generation {
            return; // superseded by a pull for a different image
        }
        status.finished_at = Some(chrono::Utc::now());
        match result {
            Ok(()) => status.state = PullState::Done,
            Err(e) => {
                status.state = PullState::Failed;
                status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });

    Ok(axum::Json(started))
}

/// `GET /api/sandbox/pull` — the status of the most recent pull.
pub async fn pull_status(State(app): State<crate::Shared>) -> axum::Json<PullStatus> {
    axum::Json(app.pull.lock().await.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_states_serialise_as_the_ui_expects() {
        // web/src/types.ts spells these out as a string union. A rename here
        // without that file following would leave Settings stuck on a state it
        // does not recognise, with no compile error on either side.
        let spelled: Vec<String> = [PullState::Idle, PullState::Cached, PullState::Pulling, PullState::Done, PullState::Failed]
            .iter()
            .map(|s| serde_json::to_value(s).unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(spelled, ["idle", "cached", "pulling", "done", "failed"]);
    }

    #[test]
    fn the_generation_guard_is_not_part_of_the_api() {
        // It exists so a slow pull cannot overwrite a newer one's status; it is
        // bookkeeping, and the UI has no use for it.
        let v = serde_json::to_value(PullStatus { generation: 7, ..Default::default() }).unwrap();
        assert!(v.get("generation").is_none(), "{v}");
        assert_eq!(v["state"], "idle");
    }

    fn cache(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_exact_reference_matches() {
        assert!(matches_cache("node:24-bookworm", &cache(&["node:24-bookworm"])));
    }

    #[test]
    fn a_bare_name_matches_the_latest_tag() {
        assert!(matches_cache("python", &cache(&["python:latest"])));
    }

    #[test]
    fn a_tagged_reference_does_not_fall_back_to_latest() {
        // Asking for :24-bookworm and finding :latest is a different image.
        assert!(!matches_cache("node:24-bookworm", &cache(&["node:latest"])));
    }

    #[test]
    fn an_empty_cache_matches_nothing() {
        assert!(!matches_cache("node:24-bookworm", &cache(&[])));
    }
}
