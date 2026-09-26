//! The graft skillset (docs/skill-packs.md, "Downloadable skillsets"): a code map colonies query with
//! `graft ask` / `callers` / `skeleton`. Not shipped with the app — the bundle carries its own Node 22 and native
//! tree-sitter grammars, around 80 MB per architecture — so the mothership downloads it when someone asks for
//! it in Settings, pinned per architecture in graft.lock, and unpacks it to `<data>/plugins/graft`. From there
//! it is an ordinary local skillset: `plugins::resolve` finds it and boot mounts it read-only like any other,
//! and each colony builds its own graph of its own worktree on first use (the bundle's bin/graft).

use crate::{App, Shared, util::exec};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// The skillset's name: its directory under `<data>/plugins` and what the `plugins` setting names.
pub const NAME: &str = "graft";

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../graft.lock");

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

/// The bundle for this machine's colonies: a Linux microVM on the host's own architecture.
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
                [NAME, release, p, "bundle", sha256, url]
                    if *p == platform && sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    Some(Pin {
                        release: release.to_string(),
                        sha256: sha256.to_ascii_lowercase(),
                        url: url.to_string(),
                    })
                }
                _ => None,
            }
        })
}

fn dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("plugins").join(NAME)
}

/// The release a downloaded bundle records in its BUNDLE.json, when `dir` holds one with its wrapper.
/// None for anything else — including an operator's own `plugins/graft`, which a download never overwrites.
fn bundle_release(dir: &Path) -> Option<String> {
    if !dir.join("bin/graft").is_file() || !dir.join("node/bin/node").is_file() {
        return None;
    }
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("BUNDLE.json")).ok()?).ok()?;
    meta["release"].as_str().map(String::from)
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Not downloaded, and no download running.
    #[default]
    Idle,
    /// The pinned release is unpacked and ready to switch on.
    Installed,
    Downloading,
    Unpacking,
    Failed,
    /// This build pins no bundle for this machine's architecture (none published yet, or an unsupported arch).
    Unavailable,
    /// `<data>/plugins/graft` is the operator's own directory, not a downloaded bundle: it is used as is.
    Local,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub name: &'static str,
    pub release: Option<String>,
    /// The release on disk when it differs from the pinned one (an update is a new download).
    pub installed_release: Option<String>,
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

/// What is on disk against what is pinned, for a status with no download running.
fn settled(dir: &Path, pin: Option<&Pin>) -> (State, Option<String>) {
    let on_disk = bundle_release(dir);
    if on_disk.is_none() && dir.is_dir() {
        return (State::Local, None);
    }
    match (pin, on_disk) {
        (_, Some(release)) if pin.is_none_or(|p| p.release == release) => (State::Installed, Some(release)),
        (None, _) => (State::Unavailable, None),
        (Some(_), on_disk) => (State::Idle, on_disk),
    }
}

/// What Settings shows: the running download if there is one, otherwise what is on disk.
pub async fn current(app: &App) -> Status {
    let status = app.graft.lock().await.clone();
    if matches!(status.state, State::Downloading | State::Unpacking | State::Failed) {
        return Status { name: NAME, ..status };
    }
    let pin = pin();
    let (state, installed_release) = settled(&dir(app), pin.as_ref());
    let error = (state == State::Unavailable).then(|| {
        "no graft bundle is pinned in this build for this machine's architecture yet (crates/colonizer/graft.lock)".to_string()
    });
    Status {
        name: NAME,
        release: pin.map(|p| p.release),
        installed_release,
        state,
        error,
        ..status
    }
}

/// `GET /api/plugins/graft`
pub async fn status(axum::extract::State(app): axum::extract::State<Shared>) -> axum::Json<Status> {
    axum::Json(current(&app).await)
}

/// `POST /api/plugins/graft/download` — start downloading the pinned bundle, or report that it is already
/// there or already coming. Returns at once; `GET /api/plugins/graft` follows the download.
pub async fn download(axum::extract::State(app): axum::extract::State<Shared>) -> crate::ApiResult<Status> {
    let status = current(&app).await;
    let pin = match status.state {
        State::Installed | State::Downloading | State::Unpacking => return Ok(axum::Json(status)),
        State::Local => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "plugins/graft is your own directory, not a downloaded bundle; remove it to download graft",
            ));
        }
        State::Unavailable => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "no graft bundle is published for this machine's architecture yet",
            ));
        }
        State::Idle | State::Failed => pin().expect("current() returned a pinned state"),
    };
    let started = {
        let mut status = app.graft.lock().await;
        *status = Status {
            name: NAME,
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
        let mut status = background.graft.lock().await;
        if status.generation != started.generation {
            return;
        }
        status.finished_at = Some(chrono::Utc::now());
        match result {
            // Settled from disk on the next read, like every other idle state.
            Ok(()) => status.state = State::Idle,
            Err(e) => {
                status.state = State::Failed;
                status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
            }
        }
    });
    Ok(axum::Json(started))
}

/// Downloads the archive, checks its sha256 against the pin, and unpacks it to `<data>/plugins/graft`. Nothing
/// lands there until the archive is verified and fully unpacked, and the swap is two renames: a colony
/// booting meanwhile mounts either the whole old bundle or the whole new one.
async fn fetch(app: &Shared, pin: &Pin, generation: u64) -> Result<()> {
    let plugins = app.cfg.data_dir.join("plugins");
    tokio::fs::create_dir_all(&plugins).await?;
    let part = plugins.join(format!(".graft-{}.tar.gz.part", pin.release));
    let progress = |total: Option<u64>, bytes: u64| {
        let app = app.clone();
        async move {
            let mut status = app.graft.lock().await;
            if status.generation == generation {
                if total.is_some() {
                    status.total = total;
                }
                status.bytes = bytes;
            }
        }
    };
    if let Err(e) = crate::util::download_sha256(&pin.url, &pin.sha256, &part, progress).await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e).with_context(|| format!("graft {}", pin.release));
    }

    app.graft.lock().await.state = State::Unpacking;
    let unpack = plugins.join(format!(".graft-{}.unpack", pin.release));
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    tokio::fs::create_dir_all(&unpack).await?;
    let unpacked = async {
        exec(Command::new("tar").arg("-xzf").arg(&part).arg("-C").arg(&unpack)).await?;
        let bundle = unpack.join(NAME);
        if bundle_release(&bundle).as_deref() != Some(pin.release.as_str()) {
            bail!(
                "the archive is not the graft {} bundle (bin/graft, node/bin/node, BUNDLE.json)",
                pin.release
            );
        }
        crate::plugins::validate(&bundle).context("the graft bundle is not a valid skillset")?;
        let dest = dir(app);
        let old = plugins.join(format!(".graft-replaced-{}", crate::util::short_id()));
        if dest.exists() {
            if bundle_release(&dest).is_none() {
                bail!("plugins/graft appeared as your own directory during the download; left untouched");
            }
            tokio::fs::rename(&dest, &old).await.context("moving the old bundle aside")?;
        }
        tokio::fs::rename(&bundle, &dest)
            .await
            .context("moving the unpacked bundle into place")?;
        let _ = tokio::fs::remove_dir_all(&old).await;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    let _ = tokio::fs::remove_file(&part).await;
    unpacked
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/plugins/graft", routing::get(status))
        .route("/api/plugins/graft/download", routing::post(download))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SHA_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn sample() -> String {
        format!(
            "# graft 0.19.0-1 linux-x86_64 bundle {SHA_A} https://x (a comment, not an entry)\n\
             headroom 0.37.0-1 linux-x86_64 bundle {SHA_B} https://example.com/headroom\n\
             graft    0.19.0-1 linux-x86_64 bundle {SHA_A} https://example.com/graft-x64.tar.gz\n\
             graft    0.19.0-1 linux-aarch64 bundle {SHA_B} https://example.com/graft-arm.tar.gz\n"
        )
    }

    #[test]
    fn each_architecture_gets_its_own_bundle_and_only_a_real_checksum_pins() {
        let lock = sample();
        assert_eq!(pin_for(&lock, "x86_64").unwrap().sha256, SHA_A);
        let arm = pin_for(&lock, "aarch64").unwrap();
        assert_eq!(
            (arm.release.as_str(), arm.url.as_str()),
            ("0.19.0-1", "https://example.com/graft-arm.tar.gz")
        );
        assert_eq!(pin_for(&lock, "riscv64"), None);
        assert_eq!(
            pin_for("graft 0.19.0-1 linux-x86_64 bundle <sha256> https://x", "x86_64"),
            None,
            "a template row is not a pin"
        );
    }

    #[test]
    fn the_real_lock_pins_nothing_but_valid_rows() {
        for arch in ["x86_64", "aarch64"] {
            if let Some(pin) = pin_for(LOCK, arch) {
                assert!(
                    pin.url
                        .starts_with("https://github.com/Colonizer-dev/harness/releases/download/graft-"),
                    "{arch}: {}",
                    pin.url
                );
                assert!(pin.url.ends_with(&format!("-linux-{arch}.tar.gz")), "{arch}: {}", pin.url);
            }
        }
    }

    fn bundle(dir: &Path, release: &str) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("node/bin")).unwrap();
        std::fs::write(dir.join("bin/graft"), "").unwrap();
        std::fs::write(dir.join("node/bin/node"), "").unwrap();
        std::fs::write(dir.join("BUNDLE.json"), format!(r#"{{"release":"{release}"}}"#)).unwrap();
    }

    #[test]
    fn what_is_on_disk_settles_the_state() {
        let root = std::env::temp_dir().join(format!("colonizer-graft-test-{}", crate::util::short_id()));
        let dir = root.join("graft");
        let pin = Pin {
            release: "0.19.0-1".into(),
            sha256: SHA_A.into(),
            url: "x".into(),
        };
        assert_eq!(settled(&dir, Some(&pin)), (State::Idle, None), "nothing downloaded yet");
        assert_eq!(
            settled(&dir, None),
            (State::Unavailable, None),
            "nothing pinned, nothing there"
        );

        std::fs::create_dir_all(dir.join("skills")).unwrap();
        assert_eq!(
            settled(&dir, Some(&pin)),
            (State::Local, None),
            "an operator's own plugins/graft is theirs"
        );
        std::fs::remove_dir_all(&dir).unwrap();

        bundle(&dir, "0.19.0-1");
        assert_eq!(settled(&dir, Some(&pin)), (State::Installed, Some("0.19.0-1".into())));
        assert_eq!(
            settled(&dir, None),
            (State::Installed, Some("0.19.0-1".into())),
            "a bundle outlives its pin"
        );
        let newer = Pin {
            release: "0.20.0-1".into(),
            ..pin
        };
        assert_eq!(
            settled(&dir, Some(&newer)),
            (State::Idle, Some("0.19.0-1".into())),
            "a new pin is a new download, and says what is there now"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_staged_plugin_is_a_valid_skillset() {
        let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/graft-bundle/plugin");
        crate::plugins::validate(&plugin).unwrap();
        let skill = std::fs::read_to_string(plugin.join("skills/graft/SKILL.md")).unwrap();
        for rule in ["graft ask", "--source", "graft grep", "--depth all", "graft skeleton", "head"] {
            assert!(skill.contains(rule), "SKILL.md should teach {rule:?}");
        }
        let wrapper = std::fs::read_to_string(plugin.join("bin/graft")).unwrap();
        assert!(
            wrapper.contains("node/bin/node"),
            "the wrapper runs graft with the bundle's own node"
        );
        assert!(wrapper.contains("DO_NOT_TRACK=1"), "graft's telemetry stays closed");
        assert!(
            wrapper.contains("-u ANTHROPIC_API_KEY") && wrapper.contains("-u OPENAI_API_KEY"),
            "no model key reaches graft"
        );
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
            (State::Local, "local"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
    }
}
