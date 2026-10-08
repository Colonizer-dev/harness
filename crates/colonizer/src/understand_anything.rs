//! The understand-anything skillset (docs/skill-packs.md, "Downloadable skillsets"): a Claude Code plugin
//! (github.com/Egonex-AI/Understand-Anything, MIT) that analyses the repository a colony is working in and
//! keeps a knowledge graph of it. Not shipped with the app — the mothership downloads it when someone asks
//! for it in Settings, pinned in understand-anything.lock, and unpacks it to `<data>/plugins/understand-anything`.
//! From there it is an ordinary local skillset: `plugins::resolve` finds it and boot mounts it read-only like
//! any other, off by default like every skillset but archify. Unlike graft there is no bundle to build: the
//! upstream repository *is* the tarball, so the install is the one directory the plugin lives in, taken out
//! of the marketplace repository and given the repository's root LICENSE.
//!
//! The state machine, the API shape and the two-rename swap are graft's (see `graft.rs`), sharing its
//! [`State`] and [`Status`]; only the pin, what an install finds in the archive, and the marker differ.

use crate::{
    App, Shared,
    graft::{State, Status},
    util::exec,
};
use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// The skillset's name: its directory under `<data>/plugins` and what the `plugins` setting names.
pub const NAME: &str = "understand-anything";

/// Compiled in, so the pin always matches the harness that was built.
const LOCK: &str = include_str!("../understand-anything.lock");

/// The plugin directory inside upstream's marketplace repository, and the manifest it must carry.
const PLUGIN_DIR: &str = "understand-anything-plugin";

/// Where an install records what it unpacked, so the release on disk can be read back. The plugin's own
/// files are upstream's; this is the one thing here that is ours.
const PIN_FILE: &str = ".colonizer-pin";

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

/// The skillset is plain source, so one row pins every colony architecture.
pub fn pin() -> Option<Pin> {
    pin_for(LOCK)
}

fn pin_for(lock: &str) -> Option<Pin> {
    lock.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                [NAME, release, "any", "source", sha256, url]
                    if sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()) && url.starts_with("https://") =>
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

/// The release a download recorded in its PIN file, when `dir` holds one with the manifest it validates.
/// None for anything else — including an operator's own `plugins/understand-anything`, which a download
/// never overwrites.
fn installed_release(dir: &Path) -> Option<String> {
    if !dir.join(".claude-plugin/plugin.json").is_file() {
        return None;
    }
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(PIN_FILE)).ok()?).ok()?;
    meta["release"].as_str().map(String::from)
}

/// What is on disk against what is pinned, for a status with no download running.
fn settled(dir: &Path, pin: Option<&Pin>) -> (State, Option<String>) {
    let on_disk = installed_release(dir);
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
    let status = app.understand_anything.lock().await.clone();
    if matches!(status.state, State::Downloading | State::Unpacking | State::Failed) {
        return Status { name: NAME, ..status };
    }
    let pin = pin();
    let (state, installed_release) = settled(&dir(app), pin.as_ref());
    let error = (state == State::Unavailable).then(|| {
        "no understand-anything skillset is pinned in this build (crates/colonizer/understand-anything.lock)".to_string()
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

/// `GET /api/plugins/understand-anything`
pub async fn status(axum::extract::State(app): axum::extract::State<Shared>) -> axum::Json<Status> {
    axum::Json(current(&app).await)
}

/// `POST /api/plugins/understand-anything/download` — start downloading the pinned archive, or report that
/// it is already there or already coming. Returns at once; `GET /api/plugins/understand-anything` follows.
pub async fn download(axum::extract::State(app): axum::extract::State<Shared>) -> crate::ApiResult<Status> {
    let status = current(&app).await;
    let pin = match status.state {
        State::Installed | State::Downloading | State::Unpacking => return Ok(axum::Json(status)),
        State::Local => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "plugins/understand-anything is your own directory, not a downloaded skillset; remove it to download it",
            ));
        }
        State::Unavailable => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "no understand-anything skillset is pinned in this build",
            ));
        }
        State::Idle | State::Failed => pin().expect("current() returned a pinned state"),
    };
    let started = {
        let mut status = app.understand_anything.lock().await;
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
        let mut status = background.understand_anything.lock().await;
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

/// Downloads the archive, checks its sha256 against the pin, and installs it as `<data>/plugins/understand-anything`.
///
/// The generation is in the temporary names as well as in the status guard: a second download started
/// while the first is still running gets its own generation, so it must not share a `.part` file (two
/// writers streaming into one file, one of them deleting it) or a `.unpack` directory (one deleting the
/// other's tree). The status guard below keeps the newer download's progress the one being reported.
async fn fetch(app: &Shared, pin: &Pin, generation: u64) -> Result<()> {
    let plugins = app.cfg.data_dir.join("plugins");
    tokio::fs::create_dir_all(&plugins).await?;
    let part = plugins.join(format!(".understand-anything-{generation}.tar.gz.part"));
    let progress = |total: Option<u64>, bytes: u64| {
        let app = app.clone();
        async move {
            let mut status = app.understand_anything.lock().await;
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
        return Err(e).with_context(|| format!("understand-anything {}", pin.release));
    }

    app.understand_anything.lock().await.state = State::Unpacking;
    let result = install_archive(&part, pin, &plugins, generation).await;
    let _ = tokio::fs::remove_file(&part).await;
    result
}

/// Installs a downloaded archive. The archive is re-hashed here — the download already hashed what it
/// streamed, but the file on disk is what gets unpacked, so that is what is checked, and a mismatch stops
/// before anything is extracted (fail closed). Nothing lands in `plugins_dir` until the archive is
/// verified and fully unpacked, and the swap is two renames: a colony booting meanwhile mounts either
/// the whole old skillset or the whole new one.
///
/// Split out of [`fetch`] so the archive handling is testable without a download.
pub(crate) async fn install_archive(archive: &Path, pin: &Pin, plugins_dir: &Path, generation: u64) -> Result<()> {
    let got = archive_sha256(archive).await?;
    if got != pin.sha256 {
        bail!("checksum mismatch: expected {}, got {got}", pin.sha256);
    }
    let unpack = plugins_dir.join(format!(".understand-anything-{generation}.unpack"));
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    tokio::fs::create_dir_all(&unpack).await?;
    let installed = unpack_skillset(archive, pin, plugins_dir, &unpack).await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    installed
}

/// The archive is a repository: one top-level directory, the marketplace's plugin directory inside it, and
/// the repository's root LICENSE — the only copy of it, and the one that has to travel with the plugin.
///
/// `--no-same-owner` and `--no-same-permissions` keep the archive's own uid/gid and mode bits out of the
/// installed tree: a colony's files belong to the mothership's user with mothership permissions, whatever
/// the tarball claims. The LICENSE is copied through `fs::copy`, which follows symlinks — a LICENSE that
/// is one would put whatever it points at in the plugin directory, so a symlink is refused outright.
async fn unpack_skillset(archive: &Path, pin: &Pin, plugins_dir: &Path, unpack: &Path) -> Result<()> {
    exec(
        Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("--no-same-owner")
            .arg("--no-same-permissions")
            .arg("-C")
            .arg(unpack),
    )
    .await?;
    let top = top_level(unpack)?;
    let skillset = top.join(PLUGIN_DIR);
    if !skillset.is_dir() {
        bail!("the archive has no {PLUGIN_DIR}/ directory to install");
    }
    let license = top.join("LICENSE");
    match tokio::fs::symlink_metadata(&license).await {
        Ok(meta) if meta.is_symlink() => bail!("the repository LICENSE is a symlink; refusing to follow it"),
        Ok(_) if license.is_file() => {
            tokio::fs::copy(&license, skillset.join("LICENSE"))
                .await
                .with_context(|| format!("copying the repository LICENSE into {PLUGIN_DIR}/"))?;
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("reading the repository LICENSE"),
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(skillset.join(".claude-plugin/plugin.json")).await?)
            .context("the skillset manifest is not JSON")?;
    if manifest["name"].as_str() != Some(NAME) {
        bail!(
            "{} declares plugin {}, not {NAME}",
            PLUGIN_DIR,
            manifest["name"].as_str().unwrap_or("nothing")
        );
    }
    crate::plugins::validate(&skillset).context("the understand-anything skillset is not a valid skillset")?;
    tokio::fs::write(
        skillset.join(PIN_FILE),
        serde_json::to_vec(&serde_json::json!({"release": pin.release, "sha256": pin.sha256}))
            .expect("a two-field object serialises"),
    )
    .await?;
    if installed_release(&skillset).as_deref() != Some(pin.release.as_str()) {
        bail!("the install did not record {PIN_FILE} for {}", pin.release);
    }

    let dest = plugins_dir.join(NAME);
    let old = plugins_dir.join(format!(".understand-anything-replaced-{}", crate::util::short_id()));
    if dest.exists() {
        if installed_release(&dest).is_none() {
            bail!("plugins/{NAME} appeared as your own directory during the download; left untouched");
        }
        tokio::fs::rename(&dest, &old)
            .await
            .context("moving the old skillset aside")?;
    }
    tokio::fs::rename(&skillset, &dest)
        .await
        .context("moving the unpacked skillset into place")?;
    let _ = tokio::fs::remove_dir_all(&old).await;
    Ok(())
}

/// The one directory the archive unpacks into. Exactly one: a tarball with a sibling top-level file is not
/// the shape this pin promises.
fn top_level(unpack: &Path) -> Result<PathBuf> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(unpack)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    match dirs.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(anyhow!("the archive unpacked to nothing")),
        _ => bail!("the archive unpacks to {} top-level directories, not one", dirs.len()),
    }
}

/// `archive`'s sha256 in lowercase hex, read back off disk rather than from the stream that wrote it.
async fn archive_sha256(archive: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(archive).await?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            break;
        }
        digest.update(&buf[..read]);
    }
    Ok(crate::util::hex(digest.finish().as_ref()))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/plugins/understand-anything", routing::get(status))
        .route("/api/plugins/understand-anything/download", routing::post(download))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "c05c150f21d13e98cdbdf4e8e520b2101975751b031c0a2e8b013f41953f5e40";
    const COMMIT: &str = "f08763d11d0202a8a8f52b5dedda6d1b2e2ebac8";

    fn pin(release: &str) -> Pin {
        Pin {
            release: release.into(),
            sha256: SHA_A.into(),
            url: format!("https://codeload.github.com/Egonex-AI/Understand-Anything/tar.gz/{COMMIT}"),
        }
    }

    #[test]
    fn the_real_lock_pins_one_release_by_commit() {
        let pin = pin_for(LOCK).expect("understand-anything is not pinned in understand-anything.lock");
        assert_eq!(pin.release, "v2.9.0");
        assert_eq!(pin.sha256, SHA_A);
        assert_eq!(
            pin.url,
            format!("https://codeload.github.com/Egonex-AI/Understand-Anything/tar.gz/{COMMIT}"),
            "pinned by commit, not by a tag that can move"
        );
    }

    #[test]
    fn only_a_real_checksum_and_an_https_url_pin() {
        let lock = format!(
            "# a comment, not an entry\n\
             understand-anything v2.9.0 any source {SHA_A} https://example.com/ua.tar.gz\n"
        );
        assert_eq!(pin_for(&lock).unwrap().release, "v2.9.0");
        assert_eq!(pin_for(""), None);
        for bad in [
            // Not 64 hex.
            format!(
                "understand-anything v2.9.0 any source {} https://example.com/ua.tar.gz",
                &SHA_A[..63]
            ),
            // Not hex.
            format!(
                "understand-anything v2.9.0 any source {} https://example.com/ua.tar.gz",
                "z".repeat(64)
            ),
            // Not https.
            format!("understand-anything v2.9.0 any source {SHA_A} http://example.com/ua.tar.gz"),
            // The skillset is one row for every colony architecture, so another platform is not a row.
            format!("understand-anything v2.9.0 linux-x86_64 source {SHA_A} https://example.com/ua.tar.gz"),
            // Another skillset's row.
            format!("graft 0.19.0-1 linux-x86_64 bundle {SHA_A} https://example.com/graft.tar.gz"),
            // A template row is not a pin.
            "understand-anything v2.9.0 any source <sha256> https://example.com/ua.tar.gz".to_string(),
        ] {
            assert_eq!(pin_for(&bad), None, "pinned: {bad}");
        }
    }

    /// Builds an archive shaped like upstream's: one top-level directory, the marketplace's plugin
    /// directory inside it with its manifest and a skill, and the repository's root LICENSE.
    fn build_archive(root: &Path, name: &str) -> PathBuf {
        let top = root.join(format!("Understand-Anything-{COMMIT}"));
        let skillset = top.join(PLUGIN_DIR);
        std::fs::create_dir_all(skillset.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(skillset.join("skills/map")).unwrap();
        std::fs::write(
            skillset.join(".claude-plugin/plugin.json"),
            serde_json::json!({
                "name": NAME,
                "version": "2.9.0",
                "description": "understand the repo",
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(skillset.join("skills/map/SKILL.md"), "# map\n").unwrap();
        std::fs::write(top.join("LICENSE"), "MIT License\n").unwrap();
        let archive = root.join(name);
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root)
            .arg(format!("Understand-Anything-{COMMIT}"))
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "tar -czf failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        archive
    }

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-ua-test-{}-{name}", crate::util::short_id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    async fn sha256_of(path: &Path) -> String {
        archive_sha256(path).await.unwrap()
    }

    /// Whatever is left in `plugins` that an install put there and did not take away.
    fn temporaries(plugins: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(plugins)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".understand-anything-"))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_verified_archive_installs_and_reads_back_as_installed() {
        let root = temp("install");
        let pin = pin("v2.9.0");
        let archive = build_archive(&root, "ua.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut install_pin = pin.clone();
        install_pin.sha256 = sha256_of(&archive).await;

        install_archive(&archive, &install_pin, &plugins, 1).await.unwrap();

        let dir = plugins.join(NAME);
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
        assert_eq!(manifest["name"], NAME);
        assert!(dir.join("skills/map/SKILL.md").is_file(), "the skill came across");
        assert_eq!(
            std::fs::read_to_string(dir.join("LICENSE")).unwrap(),
            "MIT License\n",
            "the repository's LICENSE is kept with the plugin"
        );
        assert_eq!(installed_release(&dir).as_deref(), Some("v2.9.0"), "the release reads back");
        assert_eq!(settled(&dir, Some(&pin)), (State::Installed, Some("v2.9.0".into())));
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "no unpack or replaced directory is left behind"
        );
        assert_eq!(
            std::fs::read_dir(&plugins).unwrap().count(),
            1,
            "nothing but the install is left behind: {:?}",
            std::fs::read_dir(&plugins).unwrap().collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A second download started while the first is still running unpacks into its own directory.
    /// Before the generation reached the names, both used `.understand-anything-<release>.unpack`
    /// and the second one's `remove_dir_all` deleted the tree the first was extracting into.
    #[tokio::test]
    async fn a_second_generation_does_not_touch_the_first_ones_unpack_directory() {
        let root = temp("concurrent");
        let pin = pin("v2.9.0");
        let archive = build_archive(&root, "ua.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut install_pin = pin.clone();
        install_pin.sha256 = sha256_of(&archive).await;

        // What generation 1 is unpacking into while generation 2 starts.
        let first_unpack = plugins.join(".understand-anything-1.unpack");
        std::fs::create_dir_all(first_unpack.join("Understand-Anything")).unwrap();
        let second = install_archive(&archive, &install_pin, &plugins, 2).await;

        assert!(
            first_unpack.join("Understand-Anything").is_dir(),
            "generation 2 left generation 1's tree alone: {second:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Two downloads at once: both are allowed to end in a clean failure (one finds the directory the
    /// other installed and refuses to touch it), but neither may leave a temporary behind and the
    /// install that wins must be complete.
    #[tokio::test]
    async fn two_generations_at_once_leave_no_temporaries() {
        let root = temp("both-at-once");
        let pin = pin("v2.9.0");
        let archive = build_archive(&root, "ua.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut install_pin = pin.clone();
        install_pin.sha256 = sha256_of(&archive).await;

        let _ = tokio::join!(
            install_archive(&archive, &install_pin, &plugins, 1),
            install_archive(&archive, &install_pin, &plugins, 2),
        );

        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "both unpack directories are cleaned up"
        );
        if plugins.join(NAME).exists() {
            assert_eq!(
                installed_release(&plugins.join(NAME)).as_deref(),
                Some("v2.9.0"),
                "a winning install is complete"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_tampered_archive_installs_nothing_and_leaves_no_temporaries() {
        let root = temp("tampered");
        let pin = pin("v2.9.0");
        let archive = build_archive(&root, "ua.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        // The pin is the real archive's digest; the file on disk is one byte different.
        let mut extra = std::fs::read(&archive).unwrap();
        extra.push(0);
        std::fs::write(&archive, &extra).unwrap();
        assert_ne!(sha256_of(&archive).await, pin.sha256);

        let err = install_archive(&archive, &pin, &plugins, 1).await.unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!plugins.join(NAME).exists(), "nothing installed");
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "the mismatch stops before the unpack directory is made"
        );
        assert_eq!(
            std::fs::read_dir(&plugins).unwrap().count(),
            0,
            "no unpack directory is left behind: {:?}",
            std::fs::read_dir(&plugins).unwrap().collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn an_archive_without_the_plugin_directory_installs_nothing() {
        let root = temp("wrong-shape");
        let top = root.join("Understand-Anything-x");
        std::fs::create_dir_all(&top).unwrap();
        std::fs::write(top.join("README.md"), "x").unwrap();
        let archive = root.join("ua.tar.gz");
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&root)
            .arg("Understand-Anything-x")
            .output()
            .unwrap();
        assert!(made.status.success());
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut install_pin = pin("v2.9.0");
        install_pin.sha256 = sha256_of(&archive).await;

        let err = install_archive(&archive, &install_pin, &plugins, 1).await.unwrap_err();
        assert!(format!("{err:#}").contains("understand-anything-plugin"), "{err:#}");
        assert!(!plugins.join(NAME).exists(), "nothing installed");
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "the unpack directory is cleaned up"
        );
        assert_eq!(
            std::fs::read_dir(&plugins).unwrap().count(),
            0,
            "the unpack directory is cleaned up"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A LICENSE that is a symlink is refused rather than followed: `fs::copy` reads what it points
    /// at, which would put a file of the archive author's choosing in the plugin directory.
    #[tokio::test]
    async fn a_license_that_is_a_symlink_is_refused() {
        let root = temp("symlinked-license");
        let top = root.join(format!("Understand-Anything-{COMMIT}"));
        let skillset = top.join(PLUGIN_DIR);
        std::fs::create_dir_all(skillset.join(".claude-plugin")).unwrap();
        std::fs::write(
            skillset.join(".claude-plugin/plugin.json"),
            serde_json::json!({"name": NAME, "version": "2.9.0", "description": "understand the repo"}).to_string(),
        )
        .unwrap();
        std::fs::write(top.join("NOTICE"), "not the licence\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("NOTICE", top.join("LICENSE")).unwrap();
        let archive = root.join("ua.tar.gz");
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&root)
            .arg(format!("Understand-Anything-{COMMIT}"))
            .output()
            .unwrap();
        assert!(made.status.success());
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut install_pin = pin("v2.9.0");
        install_pin.sha256 = sha256_of(&archive).await;

        let err = install_archive(&archive, &install_pin, &plugins, 1).await.unwrap_err();
        assert!(format!("{err:#}").contains("LICENSE is a symlink"), "{err:#}");
        assert!(!plugins.join(NAME).exists(), "nothing installed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_on_disk_settles_the_state() {
        let root = temp("settled");
        let dir = root.join(NAME);
        let pin = pin("v2.9.0");
        assert_eq!(settled(&dir, Some(&pin)), (State::Idle, None), "nothing downloaded yet");
        assert_eq!(
            settled(&dir, None),
            (State::Unavailable, None),
            "nothing pinned, nothing there"
        );

        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(dir.join(".claude-plugin/plugin.json"), "{}").unwrap();
        assert_eq!(
            settled(&dir, Some(&pin)),
            (State::Local, None),
            "an operator's own plugins/understand-anything is theirs"
        );
        std::fs::write(dir.join(PIN_FILE), r#"{"release":"v2.9.0"}"#).unwrap();
        assert_eq!(settled(&dir, Some(&pin)), (State::Installed, Some("v2.9.0".into())));
        assert_eq!(
            settled(&dir, None),
            (State::Installed, Some("v2.9.0".into())),
            "an installed skillset outlives its pin"
        );
        assert_eq!(
            settled(
                &dir,
                Some(&Pin {
                    release: "v3.0.0".into(),
                    ..pin
                })
            ),
            (State::Idle, Some("v2.9.0".into())),
            "a new pin is a new download, and says what is there now"
        );
        std::fs::remove_dir_all(&root).unwrap();
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
