//! Headroom (docs/protocol.md, "Token savings"): the bundle colonies run Headroom from, downloaded only when
//! someone switches Headroom on — it is a standalone CPython with headroom-ai[proxy], 220–245 MB depending on
//! the architecture, which nobody who leaves the switch off should carry. Pinned per architecture in
//! headroom.lock beside this crate's Cargo.toml, and published by .github/workflows/headroom-bundle.yml.
//! Colonies mount the unpacked bundle read-only at /opt/colonizer/headroom, whatever stack they use.

use crate::{App, Shared, util::exec};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../headroom.lock");

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

/// The bundle for this machine's colonies. A colony is a Linux microVM on the host's own architecture, so a
/// Mac on Apple Silicon takes the linux aarch64 bundle.
pub fn pin() -> Option<Pin> {
    pin_for(LOCK, std::env::consts::ARCH)
}

fn pin_for(lock: &str, arch: &str) -> Option<Pin> {
    let platform = match arch {
        "x86_64" => "linux-x86_64",
        "aarch64" => "linux-aarch64",
        _ => return None,
    };
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["headroom", release, p, "bundle", sha256, url] if *p == platform => Some(Pin {
                    release: release.to_string(),
                    sha256: sha256.to_string(),
                    url: url.to_string(),
                }),
                _ => None,
            }
        })
}

fn root(app: &App) -> PathBuf {
    app.cfg.data_dir.join("headroom")
}

/// The unpacked bundle for the pinned release, when it has been downloaded.
pub fn installed(app: &App) -> Option<PathBuf> {
    installed_in(&root(app), &pin()?)
}

fn installed_in(root: &Path, pin: &Pin) -> Option<PathBuf> {
    let dir = root.join(&pin.release);
    dir.join("python/bin/python3").is_file().then_some(dir)
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not downloaded, and no download running.
    #[default]
    Idle,
    /// The pinned release is unpacked and ready.
    Installed,
    Downloading,
    Unpacking,
    Failed,
    /// No bundle is pinned for this machine's architecture.
    Unavailable,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub release: Option<String>,
    pub state: State,
    pub bytes: u64,
    pub total: Option<u64>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    /// Bumped for every download started, so a stale one cannot overwrite a newer status.
    #[serde(skip)]
    pub generation: u64,
}

/// What Settings shows: the running download if there is one, otherwise what is on disk.
async fn current(app: &App) -> Status {
    let status = app.headroom.lock().await.clone();
    if matches!(status.state, State::Downloading | State::Unpacking | State::Failed) {
        return status;
    }
    let Some(pin) = pin() else {
        return Status {
            state: State::Unavailable,
            ..status
        };
    };
    let state = if installed(app).is_some() {
        State::Installed
    } else {
        State::Idle
    };
    Status {
        release: Some(pin.release),
        state,
        ..status
    }
}

/// `GET /api/headroom`
pub async fn status(axum::extract::State(app): axum::extract::State<Shared>) -> axum::Json<Status> {
    axum::Json(current(&app).await)
}

/// `POST /api/headroom/download` — start downloading the pinned bundle, or report that it is already
/// there or already coming. Returns at once; `GET /api/headroom` follows the download.
pub async fn download(axum::extract::State(app): axum::extract::State<Shared>) -> crate::ApiResult<Status> {
    let status = current(&app).await;
    let pin = match status.state {
        State::Installed | State::Downloading | State::Unpacking => return Ok(axum::Json(status)),
        State::Unavailable => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "no Headroom bundle is published for this machine's architecture",
            ));
        }
        State::Idle | State::Failed => pin().expect("current() returned a pinned state"),
    };

    let started = {
        let mut status = app.headroom.lock().await;
        *status = Status {
            release: Some(pin.release.clone()),
            state: State::Downloading,
            started_at: Some(chrono::Utc::now()),
            generation: status.generation + 1,
            ..Default::default()
        };
        status.clone()
    };

    let background = app.clone();
    tokio::spawn(async move {
        let result = fetch(&background, &pin, started.generation).await;
        let mut status = background.headroom.lock().await;
        if status.generation != started.generation {
            return;
        }
        status.finished_at = Some(chrono::Utc::now());
        match result {
            Ok(()) => status.state = State::Installed,
            Err(e) => {
                status.state = State::Failed;
                status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });
    Ok(axum::Json(started))
}

/// Downloads the archive, checks its sha256 against the pin, and unpacks it into `<data>/headroom/<release>`.
/// Nothing lands at the final path until the archive has been verified and fully unpacked.
async fn fetch(app: &Shared, pin: &Pin, generation: u64) -> Result<()> {
    let root = root(app);
    tokio::fs::create_dir_all(&root).await?;
    let part = root.join(format!(".{}.tar.gz.part", pin.release));
    let result = download_verified(app, pin, generation, &part).await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }

    app.headroom.lock().await.state = State::Unpacking;
    let unpack = root.join(format!(".{}.unpack", pin.release));
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    tokio::fs::create_dir_all(&unpack).await?;
    let unpacked = async {
        exec(Command::new("tar").arg("-xzf").arg(&part).arg("-C").arg(&unpack)).await?;
        let bundle = unpack.join("headroom");
        if !bundle.join("python/bin/python3").is_file() {
            bail!("the archive has no headroom/python/bin/python3");
        }
        let dest = root.join(&pin.release);
        let _ = tokio::fs::remove_dir_all(&dest).await;
        tokio::fs::rename(&bundle, &dest)
            .await
            .context("moving the unpacked bundle into place")?;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    let _ = tokio::fs::remove_file(&part).await;
    unpacked
}

async fn download_verified(app: &Shared, pin: &Pin, generation: u64, part: &Path) -> Result<()> {
    let progress = |total: Option<u64>, bytes: u64| {
        let app = app.clone();
        async move {
            let mut status = app.headroom.lock().await;
            if status.generation == generation {
                if total.is_some() {
                    status.total = total;
                }
                status.bytes = bytes;
            }
        }
    };
    crate::util::download_sha256(&pin.url, &pin.sha256, part, progress)
        .await
        .with_context(|| format!("Headroom {}", pin.release))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/headroom", routing::get(status))
        .route("/api/headroom/download", routing::post(download))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK_SAMPLE: &str = "\
# a comment naming headroom 0.37.0-1 linux-x86_64 bundle is not an entry
ecc          2.2.1     any           plugin  aaaa  https://example.com/ecc
headroom     0.37.0-1  linux-x86_64  bundle  1111  https://example.com/headroom-0.37.0-1-linux-x86_64.tar.gz
headroom     0.37.0-1  linux-aarch64 bundle  2222  https://example.com/headroom-0.37.0-1-linux-aarch64.tar.gz
";

    #[test]
    fn each_architecture_gets_its_own_bundle() {
        assert_eq!(pin_for(LOCK_SAMPLE, "x86_64").unwrap().sha256, "1111");
        let arm = pin_for(LOCK_SAMPLE, "aarch64").unwrap();
        assert_eq!((arm.release.as_str(), arm.sha256.as_str()), ("0.37.0-1", "2222"));
        assert!(arm.url.ends_with("linux-aarch64.tar.gz"));
        assert_eq!(pin_for(LOCK_SAMPLE, "riscv64"), None);
        assert_eq!(pin_for("ecc 2.2.1 any plugin aaaa https://x", "x86_64"), None);
    }

    #[test]
    fn the_real_lock_pins_both_architectures() {
        for arch in ["x86_64", "aarch64"] {
            let pin = pin_for(LOCK, arch).unwrap_or_else(|| panic!("headroom.lock pins no Headroom bundle for {arch}"));
            assert_eq!(pin.sha256.len(), 64, "{arch}: sha256 must be 64 hex characters");
            assert!(
                pin.url
                    .starts_with("https://github.com/Colonizer-dev/harness/releases/download/headroom-"),
                "{arch}: {}",
                pin.url
            );
        }
    }

    #[test]
    fn a_bundle_counts_as_installed_only_once_its_python_is_there() {
        let root = std::env::temp_dir().join(format!("colonizer-headroom-test-{}", crate::util::short_id()));
        let pin = Pin {
            release: "0.37.0-1".into(),
            sha256: "x".into(),
            url: "x".into(),
        };
        assert_eq!(installed_in(&root, &pin), None);
        std::fs::create_dir_all(root.join("0.37.0-1/python/bin")).unwrap();
        assert_eq!(installed_in(&root, &pin), None, "an empty directory is not a bundle");
        std::fs::write(root.join("0.37.0-1/python/bin/python3"), "").unwrap();
        assert_eq!(installed_in(&root, &pin), Some(root.join("0.37.0-1")));
        let other = Pin {
            release: "0.38.0-1".into(),
            ..pin
        };
        assert_eq!(installed_in(&root, &other), None, "a new pin needs its own download");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn states_serialise_as_the_ui_expects() {
        for (state, name) in [
            (State::Idle, "idle"),
            (State::Installed, "installed"),
            (State::Downloading, "downloading"),
            (State::Unpacking, "unpacking"),
            (State::Failed, "failed"),
            (State::Unavailable, "unavailable"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }
}
