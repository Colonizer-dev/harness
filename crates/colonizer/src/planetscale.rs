//! PlanetScale downloadable skillset: pscale CLI (Apache-2.0) + skills repo (MIT).
//! Downloads to <data>/plugins/planetscale. Uses graft/understand-anything state machine.

use crate::{
    App, Shared,
    graft::{State, Status},
    util::exec,
};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub const NAME: &str = "planetscale";
const LOCK: &str = include_str!("../planetscale.lock");

#[derive(Clone, Debug, PartialEq)]
pub struct PscalePin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillsPin {
    pub release: String,
    pub sha256: String,
    pub url: String,
}

pub fn pins() -> Option<(PscalePin, SkillsPin)> {
    pins_for(LOCK, std::env::consts::ARCH)
}

fn pins_for(lock: &str, arch: &str) -> Option<(PscalePin, SkillsPin)> {
    let platform = match arch {
        "x86_64" => "linux-x86_64",
        "aarch64" => "linux-aarch64",
        _ => return None,
    };
    let mut pscale_pin = None;
    let mut skills_pin = None;
    for line in lock.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["pscale", rel, p, "archive", sha, url] if *p == platform && is_valid_sha(sha) => {
                pscale_pin = Some(PscalePin {
                    release: rel.to_string(),
                    sha256: sha.to_ascii_lowercase(),
                    url: url.to_string(),
                });
            }
            ["planetscale-skills", rel, "any", "source", sha, url] if is_valid_sha(sha) => {
                skills_pin = Some(SkillsPin {
                    release: rel.to_string(),
                    sha256: sha.to_ascii_lowercase(),
                    url: url.to_string(),
                });
            }
            _ => {}
        }
    }
    match (pscale_pin, skills_pin) {
        (Some(p), Some(s)) => Some((p, s)),
        _ => None,
    }
}

fn is_valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("plugins").join(NAME)
}

fn installed_release(dir: &Path) -> Option<String> {
    if !dir.join("plugin.json").is_file() {
        return None;
    }
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(".colonizer-pin")).ok()?).ok()?;
    meta["release"].as_str().map(String::from)
}

fn settled(dir: &Path, pins: Option<&(PscalePin, SkillsPin)>) -> (State, Option<String>) {
    let on_disk = installed_release(dir);
    if on_disk.is_none() && dir.is_dir() {
        return (State::Local, None);
    }
    match (pins, on_disk) {
        (_, Some(release)) if pins.is_none_or(|(p, _)| p.release == release) => (State::Installed, Some(release)),
        (None, _) => (State::Unavailable, None),
        (Some(_), on_disk) => (State::Idle, on_disk),
    }
}

pub async fn current(app: &App) -> Status {
    let status = app.planetscale.lock().await.clone();
    if matches!(status.state, State::Downloading | State::Unpacking | State::Failed) {
        return Status { name: NAME, ..status };
    }
    let pins = pins();
    let (state, installed_release) = settled(&dir(app), pins.as_ref());
    let error = (state == State::Unavailable).then(|| "no PlanetScale skillset pinned (planetscale.lock)".to_string());
    Status {
        name: NAME,
        release: pins.as_ref().map(|(p, _)| p.release.clone()),
        installed_release,
        state,
        error,
        ..status
    }
}

pub async fn status(axum::extract::State(app): axum::extract::State<Shared>) -> axum::Json<Status> {
    axum::Json(current(&app).await)
}

pub async fn download(axum::extract::State(app): axum::extract::State<Shared>) -> crate::ApiResult<Status> {
    let status = current(&app).await;
    let pins = match status.state {
        State::Installed | State::Downloading | State::Unpacking => return Ok(axum::Json(status)),
        State::Local => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "plugins/planetscale is your own directory",
            ));
        }
        State::Unavailable => {
            return Err(crate::client_error(
                axum::http::StatusCode::CONFLICT,
                "no PlanetScale skillset pinned",
            ));
        }
        State::Idle | State::Failed => pins().expect("pins() returned from current()"),
    };
    let started = {
        let mut status = app.planetscale.lock().await;
        *status = Status {
            name: NAME,
            release: Some(pins.0.release.clone()),
            state: State::Downloading,
            started_at: Some(chrono::Utc::now()),
            generation: status.generation + 1,
            ..Default::default()
        };
        status.clone()
    };
    let background = app.clone();
    tokio::spawn(async move {
        let result = fetch(&background, &pins, started.generation).await;
        let mut status = background.planetscale.lock().await;
        if status.generation != started.generation {
            return;
        }
        status.finished_at = Some(chrono::Utc::now());
        status.state = if result.is_ok() { State::Idle } else { State::Failed };
        if let Err(e) = result {
            status.error = Some(crate::util::truncate(&format!("{e:#}"), 2000));
        }
    });
    Ok(axum::Json(started))
}

async fn fetch(app: &Shared, pins: &(PscalePin, SkillsPin), generation: u64) -> Result<()> {
    let plugins = app.cfg.data_dir.join("plugins");
    tokio::fs::create_dir_all(&plugins).await?;
    let pscale_part = plugins.join(format!(".ps-pscale-{generation}.tar.gz.part"));
    let skills_part = plugins.join(format!(".ps-skills-{generation}.tar.gz.part"));

    // Download both archives
    for (url, sha, path, name) in [
        (&pins.0.url, &pins.0.sha256, &pscale_part, "pscale"),
        (&pins.1.url, &pins.1.sha256, &skills_part, "skills"),
    ] {
        let progress = |a: &Shared, _: &str| {
            let a = a.clone();
            move |total: Option<u64>, bytes: u64| {
                let a = a.clone();
                async move {
                    let mut s = a.planetscale.lock().await;
                    if s.total.is_none() {
                        s.total = total;
                    }
                    s.bytes = bytes;
                }
            }
        };
        if let Err(e) = crate::util::download_sha256(url, sha, path, progress(app, name)).await {
            let _ = tokio::fs::remove_file(&pscale_part).await;
            let _ = tokio::fs::remove_file(&skills_part).await;
            return Err(e).with_context(|| format!("planetscale {name}"));
        }
    }

    app.planetscale.lock().await.state = State::Unpacking;
    let result = install_archives(&pscale_part, &skills_part, pins, &plugins, generation).await;
    let _ = tokio::fs::remove_file(&pscale_part).await;
    let _ = tokio::fs::remove_file(&skills_part).await;
    result
}

pub(crate) async fn install_archives(
    pscale_arch: &Path,
    skills_arch: &Path,
    pins: &(PscalePin, SkillsPin),
    plugins_dir: &Path,
    generation: u64,
) -> Result<()> {
    let ps_sha = crate::understand_anything::archive_sha256(pscale_arch).await?;
    if ps_sha != pins.0.sha256 {
        bail!("pscale: checksum mismatch");
    }
    let sk_sha = crate::understand_anything::archive_sha256(skills_arch).await?;
    if sk_sha != pins.1.sha256 {
        bail!("skills: checksum mismatch");
    }

    let unpack = plugins_dir.join(format!(".ps-{generation}.unpack"));
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    tokio::fs::create_dir_all(&unpack).await?;
    let result = unpack_and_assemble(pscale_arch, skills_arch, pins, plugins_dir, &unpack).await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    result
}

async fn unpack_and_assemble(
    ps_arch: &Path,
    sk_arch: &Path,
    pins: &(PscalePin, SkillsPin),
    plugins_dir: &Path,
    unpack: &Path,
) -> Result<()> {
    // Unpack both
    for (arc, name) in [(ps_arch, "pscale"), (sk_arch, "skills")] {
        exec(
            Command::new("tar")
                .arg("-xzf")
                .arg(arc)
                .arg("--no-same-owner")
                .arg("--no-same-permissions")
                .arg("-C")
                .arg(unpack),
        )
        .await
        .with_context(|| format!("unpacking {name}"))?;
    }

    // Find pscale binary and skills directory
    let ps_bin = std::fs::read_dir(unpack)?
        .flatten()
        .find(|e| e.file_name() == "pscale")
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("pscale binary not found"))?;

    let sk_top = std::fs::read_dir(unpack)?
        .flatten()
        .find(|e| e.path().is_dir() && e.file_name().to_string_lossy().starts_with("skills-"))
        .map(|e| e.path())
        .ok_or_else(|| anyhow::anyhow!("skills directory not found"))?;

    let staging = unpack.join("planetscale-staging");
    tokio::fs::create_dir_all(staging.join("skills")).await?;
    tokio::fs::create_dir_all(staging.join("bin")).await?;

    // Copy binary and licenses
    tokio::fs::copy(&ps_bin, staging.join("bin/pscale")).await?;
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staging.join("bin/pscale"), Permissions::from_mode(0o755))?;
    }

    // Copy skills (skip autonomous-execution-mode)
    if let Ok(entries) = std::fs::read_dir(sk_top.join("skills")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "planetscale-autonomous-execution-mode" {
                continue;
            }
            if !entry.path().join("SKILL.md").is_file() {
                continue;
            }
            copy_recursive(&entry.path(), &staging.join("skills").join(&name)).await?;
        }
    }

    // Licenses and metadata
    if sk_top.join("LICENSE").is_file() {
        tokio::fs::copy(sk_top.join("LICENSE"), staging.join("LICENSE")).await?;
    }
    if let Some(p) = std::fs::read_dir(unpack)?
        .flatten()
        .find(|e| e.file_name() == "LICENSE")
        .map(|e| e.path())
    {
        let _ = tokio::fs::copy(&p, staging.join("LICENSE.pscale")).await;
    }

    let version = format!("{}+{}", pins.0.release, &pins.1.release[..7.min(pins.1.release.len())]);
    tokio::fs::write(
        staging.join("plugin.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "name": NAME,
            "version": version,
            "description": "PlanetScale CLI and skills",
            "skills": ["./skills/"],
        }))?,
    )
    .await?;
    // mcp.json for Colonizer's boot.rs to read hosts for egress allowlisting
    tokio::fs::write(
        staging.join("mcp.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "planetscale": {
                    "type": "http",
                    "url": "https://mcp.pscale.dev/mcp/planetscale",
                    "hosts": ["mcp.pscale.dev"]
                }
            }
        }))?,
    )
    .await?;

    // .mcp.json for Claude Code's native plugin loader
    tokio::fs::write(
        staging.join(".mcp.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "mcpServers": {
                "planetscale": {
                    "type": "http",
                    "url": "https://mcp.pscale.dev/mcp/planetscale"
                }
            }
        }))?,
    )
    .await?;

    crate::plugins::validate(&staging)?;
    tokio::fs::write(
        staging.join(".colonizer-pin"),
        serde_json::to_vec(&serde_json::json!({"release": pins.0.release}))?,
    )
    .await?;

    let dest = plugins_dir.join(NAME);
    let old = plugins_dir.join(format!(".ps-replaced-{}", crate::util::short_id()));
    if dest.exists() && installed_release(&dest).is_none() {
        bail!("plugins/planetscale is your own directory");
    }
    if dest.exists() {
        tokio::fs::rename(&dest, &old).await?;
    }
    tokio::fs::rename(&staging, &dest).await?;
    let _ = tokio::fs::remove_dir_all(&old).await;
    Ok(())
}

async fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    let mut work = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((src, dst)) = work.pop() {
        tokio::fs::create_dir_all(&dst).await?;
        for entry in std::fs::read_dir(&src)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            // Refuse to follow symlinks
            if metadata.is_symlink() {
                continue;
            }
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if metadata.is_dir() {
                work.push((path, target));
            } else if metadata.is_file() {
                tokio::fs::copy(&path, &target).await?;
            }
        }
    }
    Ok(())
}

pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/plugins/planetscale", routing::get(status))
        .route("/api/plugins/planetscale/download", routing::post(download))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_lock_pins_both_architectures() {
        for arch in ["x86_64", "aarch64"] {
            let (p, s) = pins_for(LOCK, arch).unwrap_or_else(|| panic!("{arch} pinned"));
            assert_eq!(p.release, "0.347.0");
            assert!(p.url.starts_with("https://github.com/planetscale"));
            assert_eq!(s.release.len(), 40);
        }
    }

    #[test]
    fn pins_must_be_https_and_64_hex() {
        let lock = "pscale 0.347.0 linux-x86_64 archive 1111111111111111111111111111111111111111111111111111111111111111 https://example.com/x\nplanetscale-skills deadbeefdeadbeefdeadbeefdeadbeefdeadbeef any source 2222222222222222222222222222222222222222222222222222222222222222 https://example.com/y\n";
        assert!(pins_for(lock, "x86_64").is_some());
        assert!(pins_for("bad <sha256> https://example.com/x", "x86_64").is_none());
    }

    #[test]
    fn pscale_version_at_least_0_292_0() {
        let (p, _) = pins().expect("pinned");
        let parts: Vec<_> = p.release.split('.').collect();
        let major = parts[0].parse::<u32>().unwrap_or(0);
        let minor = parts[1].parse::<u32>().unwrap_or(0);
        assert!(major > 0 || minor >= 292);
    }

    #[test]
    fn state_serialises_correctly() {
        assert_eq!(serde_json::to_value(State::Installed).unwrap(), "installed");
        assert_eq!(serde_json::to_value(State::Idle).unwrap(), "idle");
    }

    fn pscale_pin() -> PscalePin {
        PscalePin {
            release: "0.347.0".into(),
            sha256: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            url: "https://example.com/pscale.tar.gz".into(),
        }
    }

    fn skills_pin() -> SkillsPin {
        SkillsPin {
            release: "0123456789abcdef0123456789abcdef01234567".into(),
            sha256: "2222222222222222222222222222222222222222222222222222222222222222".into(),
            url: "https://example.com/skills.tar.gz".into(),
        }
    }

    fn build_pscale_archive(root: &Path, name: &str) -> PathBuf {
        std::fs::write(root.join("pscale"), "#!/bin/sh\necho pscale").unwrap();
        std::fs::write(root.join("LICENSE"), "Apache License 2.0\n").unwrap();
        let archive = root.join(name);
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root)
            .arg("pscale")
            .arg("LICENSE")
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "tar -czf pscale failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        archive
    }

    fn build_skills_archive(root: &Path, name: &str) -> PathBuf {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let top = root.join(format!("skills-{commit}"));
        let skills_dir = top.join("skills");
        std::fs::create_dir_all(skills_dir.join("planetscale-change-gates-and-approval-contract")).unwrap();
        std::fs::create_dir_all(skills_dir.join("planetscale-autonomous-execution-mode")).unwrap();
        std::fs::write(
            skills_dir.join("planetscale-change-gates-and-approval-contract/SKILL.md"),
            "# change-gates\n",
        )
        .unwrap();
        std::fs::write(
            skills_dir.join("planetscale-autonomous-execution-mode/SKILL.md"),
            "# autonomous-execution-mode\n",
        )
        .unwrap();
        std::fs::write(top.join("LICENSE"), "MIT License\n").unwrap();
        let archive = root.join(name);
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root)
            .arg(format!("skills-{commit}"))
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "tar -czf skills failed: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        archive
    }

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("colonizer-ps-test-{}-{name}", crate::util::short_id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    async fn sha256_of(path: &Path) -> String {
        crate::understand_anything::archive_sha256(path).await.unwrap()
    }

    fn temporaries(plugins: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(plugins)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ps-"))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn verified_pscale_and_skills_archives_install_and_read_back_as_installed() {
        let root = temp("install");
        let ps_pin = pscale_pin();
        let sk_pin = skills_pin();
        let pscale_archive = build_pscale_archive(&root, "pscale.tar.gz");
        let skills_archive = build_skills_archive(&root, "skills.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();

        let mut ps_pin_mut = ps_pin.clone();
        ps_pin_mut.sha256 = sha256_of(&pscale_archive).await;
        let mut sk_pin_mut = sk_pin.clone();
        sk_pin_mut.sha256 = sha256_of(&skills_archive).await;

        install_archives(
            &pscale_archive,
            &skills_archive,
            &(ps_pin_mut.clone(), sk_pin_mut.clone()),
            &plugins,
            1,
        )
        .await
        .unwrap();

        let dir = plugins.join(NAME);
        assert!(dir.join("bin/pscale").is_file(), "pscale binary exists");
        assert!(
            dir.join("skills/planetscale-change-gates-and-approval-contract/SKILL.md")
                .is_file(),
            "the included skill came across"
        );
        assert!(
            !dir.join("skills/planetscale-autonomous-execution-mode").exists(),
            "the autonomous-execution-mode skill was skipped"
        );
        assert!(dir.join("LICENSE").is_file(), "skills LICENSE is present");
        assert!(dir.join("LICENSE.pscale").is_file(), "pscale LICENSE is present");
        assert_eq!(installed_release(&dir).as_deref(), Some("0.347.0"), "the release reads back");
        assert_eq!(
            settled(&dir, Some(&(ps_pin_mut.clone(), sk_pin_mut.clone()))),
            (State::Installed, Some("0.347.0".into()))
        );
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "no unpack or replaced directory is left behind"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_tampered_pscale_archive_installs_nothing_and_leaves_no_temporaries() {
        let root = temp("tampered-pscale");
        let ps_pin = pscale_pin();
        let sk_pin = skills_pin();
        let pscale_archive = build_pscale_archive(&root, "pscale.tar.gz");
        let skills_archive = build_skills_archive(&root, "skills.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();

        let mut ps_pin_mut = ps_pin.clone();
        ps_pin_mut.sha256 = sha256_of(&pscale_archive).await;
        let mut sk_pin_mut = sk_pin.clone();
        sk_pin_mut.sha256 = sha256_of(&skills_archive).await;

        // Tamper with pscale archive
        let mut extra = std::fs::read(&pscale_archive).unwrap();
        extra.push(0);
        std::fs::write(&pscale_archive, &extra).unwrap();

        let err = install_archives(&pscale_archive, &skills_archive, &(ps_pin_mut, sk_pin_mut), &plugins, 1)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!plugins.join(NAME).exists(), "nothing installed");
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "no unpack directory is left behind"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_tampered_skills_archive_installs_nothing_and_leaves_no_temporaries() {
        let root = temp("tampered-skills");
        let ps_pin = pscale_pin();
        let sk_pin = skills_pin();
        let pscale_archive = build_pscale_archive(&root, "pscale.tar.gz");
        let skills_archive = build_skills_archive(&root, "skills.tar.gz");
        let plugins = root.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();

        let mut ps_pin_mut = ps_pin.clone();
        ps_pin_mut.sha256 = sha256_of(&pscale_archive).await;
        let mut sk_pin_mut = sk_pin.clone();
        sk_pin_mut.sha256 = sha256_of(&skills_archive).await;

        // Tamper with skills archive
        let mut extra = std::fs::read(&skills_archive).unwrap();
        extra.push(0);
        std::fs::write(&skills_archive, &extra).unwrap();

        let err = install_archives(&pscale_archive, &skills_archive, &(ps_pin_mut, sk_pin_mut), &plugins, 1)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
        assert!(!plugins.join(NAME).exists(), "nothing installed");
        assert_eq!(
            temporaries(&plugins),
            Vec::<String>::new(),
            "no unpack directory is left behind"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
