//! Where the app is installed, and how a new version is installed beside it.
//!
//! The release layout (scripts/install-release.sh) gives every version its own directory and points
//! one symlink at the live one:
//!
//! ```text
//! ~/.local/share/colonizer/
//!   versions/v0.1.3/   bin/ vendor/ plugins/ modules/ web/ VERSION LICENSE NOTICE
//!   versions/v0.1.4/
//!   app -> versions/v0.1.4
//! ```
//!
//! A mothership resolves its own binary to the real `versions/<v>` path, and colonies mount the
//! agent module, plugins and vendored tools read-only straight out of it (sessions.rs, sandbox.rs).
//! That is what makes an update safe: installing a new version never touches the directory anything
//! is still reading, and switching versions is one atomic symlink rename. This module installs a
//! release beside the running one, moves the link, and prunes the versions nothing can reach.

use crate::Shared;
use crate::config::Settings;
use crate::util::exec;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use std::{
    collections::BTreeSet,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::{io::AsyncWriteExt, process::Command};

/// SHA256SUMS is a handful of lines; anything claiming to be bigger is not one.
const MAX_SUMS: usize = 64 * 1024;
/// Progress is reported about every megabyte, not on every chunk (headroom.rs does the same).
const PROGRESS_STEP: u64 = 1 << 20;

// ---------------------------------------------------------------------------
// What shape this install is
// ---------------------------------------------------------------------------

/// The shape of the install the mothership runs from, as `update.rs` needs to see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The release layout: `<root>/versions/<version>`, with `<root>/app` a symlink to it.
    Versioned { root: PathBuf, version: String },
    /// A real `app` directory: an install made before versions/, which only the release installer
    /// can move to the current layout.
    Legacy { root: PathBuf },
    /// A source checkout: the app lives in `dist/` beside the built binary.
    Source,
    /// Nothing the updater recognises.
    Unknown,
}

/// Which [`Layout`] a shape is, without the paths — everything the update view's decisions need,
/// and cheap to name in a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutKind {
    Versioned,
    Legacy,
    Source,
    Unknown,
}

impl Layout {
    pub fn kind(&self) -> LayoutKind {
        match self {
            Layout::Versioned { .. } => LayoutKind::Versioned,
            Layout::Legacy { .. } => LayoutKind::Legacy,
            Layout::Source => LayoutKind::Source,
            Layout::Unknown => LayoutKind::Unknown,
        }
    }
}

/// The shape of the install behind the process's settings.
pub fn layout(cfg: &Settings) -> Layout {
    layout_for(cfg.assets.as_deref())
}

/// The shape behind an assets directory. `COLONIZER_HOME` may name a symlink or a relative path, so
/// the real path decides: a symlinked `app` canonicalizes to its `versions/<v>` target, while the
/// legacy layout's `app` is a directory in its own right and keeps its name.
pub fn layout_for(assets: Option<&Path>) -> Layout {
    let Some(assets) = assets else {
        return Layout::Unknown;
    };
    let Ok(real) = std::fs::canonicalize(assets) else {
        return Layout::Unknown;
    };
    let Some(name) = real.file_name().and_then(|n| n.to_str()) else {
        return Layout::Unknown;
    };
    if name == "dist" {
        return Layout::Source;
    }
    let Some(parent) = real.parent() else {
        return Layout::Unknown;
    };
    if name == "app" {
        // A real directory named app: the layout the first installers used.
        return if real.is_dir() {
            Layout::Legacy {
                root: parent.to_path_buf(),
            }
        } else {
            Layout::Unknown
        };
    }
    if parent.file_name().and_then(|n| n.to_str()) == Some("versions") {
        let root = parent.parent().unwrap_or(parent).to_path_buf();
        // Only when the root's own `app` link really names this directory — that link is what an
        // update repoints, so its absence is not a layout to update in.
        if app_resolves_to(&root, &real) {
            return Layout::Versioned {
                root,
                version: name.to_string(),
            };
        }
    }
    Layout::Unknown
}

/// Whether `<root>/app` is a symlink resolving to `real`. Read, never assumed.
fn app_resolves_to(root: &Path, real: &Path) -> bool {
    let app = root.join("app");
    std::fs::symlink_metadata(&app).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::canonicalize(&app).is_ok_and(|target| target == real)
}

/// The platform slice the release workflow names bundles for: `colonizer-<platform>.tar.gz`. Two
/// bundles are published, and nothing pretends to work where neither is.
pub fn platform() -> Option<&'static str> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// The platform a machine's OS and architecture make, so the update view can answer for the
/// machine it runs on without trusting `platform()`'s compile-time constants in tests.
pub fn platform_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("macos", "aarch64") => Some("darwin-arm64"),
        _ => None,
    }
}

/// Only a plain `v…`/`dev-…` token may name a directory under `versions/`: no separators to escape
/// it, no leading dot to hide among the staging names. Colonies mount read-only out of the directory
/// this names, so it is worth being strict about it; the installer enforces the same rule.
pub fn check_version_token(version: &str) -> Result<()> {
    let plain = version.starts_with('v')
        && version
            .as_bytes()
            .get(1)
            .is_some_and(|b| b.is_ascii_digit())
        || version.starts_with("dev-");
    if plain
        && !version.starts_with('.')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        Ok(())
    } else {
        bail!(
            "'{version}' is not a plain v…/dev-… version; refusing to use it as an install directory"
        )
    }
}

// ---------------------------------------------------------------------------
// Installing a release beside the running one
// ---------------------------------------------------------------------------

/// What [`install`] tells the caller while it works, so the UI can follow along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Report {
    Downloading,
    Verifying,
    Unpacking,
    Installing,
    /// Download progress; `total` comes from Content-Length when the server sends one.
    Bytes {
        bytes: u64,
        total: Option<u64>,
    },
}

/// Installs release `tag` from `base_url` into `<root>/versions/<tag>` and returns that path. It
/// never points `app` anywhere — [`point_app_at`] does that once the caller is ready — and nothing
/// lands at the final path until the archive has been verified, unpacked and reassembled: the
/// download lands in `.<tag>.part`, the unpack in `.<tag>.unpack`, both removed on every way out.
pub async fn install(
    root: &Path,
    tag: &str,
    base_url: &str,
    client: &reqwest::Client,
    mut report: impl FnMut(Report),
) -> Result<PathBuf> {
    let Some(platform) = platform() else {
        bail!("there are no release bundles for this platform");
    };
    check_version_token(tag)?;
    let base = base_url.trim_end_matches('/');
    let archive = format!("colonizer-{platform}.tar.gz");
    let versions = root.join("versions");
    tokio::fs::create_dir_all(&versions)
        .await
        .with_context(|| format!("creating {}", versions.display()))?;
    let target = versions.join(tag);

    // The release's checksums, and the line for this platform's bundle.
    report(Report::Downloading);
    let sums = fetch_capped(client, &format!("{base}/SHA256SUMS"), MAX_SUMS)
        .await
        .context("fetching the release's SHA256SUMS")?;
    let Some(expected) = sha256sums_entry(&sums, &archive) else {
        bail!("{archive} is not listed in the release's SHA256SUMS");
    };

    // The bundle itself, hashed as it arrives.
    let part = versions.join(format!(".{tag}.part"));
    let downloaded = async {
        let response = client
            .get(format!("{base}/{archive}"))
            .send()
            .await
            .with_context(|| format!("downloading {archive}"))?
            .error_for_status()?;
        let total = response.content_length();
        let mut file = tokio::fs::File::create(&part)
            .await
            .with_context(|| format!("creating {}", part.display()))?;
        // stream_to reports about every megabyte, which is the progress the UI sees.
        let (bytes, got) = stream_to(response, &mut file, |bytes| {
            report(Report::Bytes { bytes, total })
        })
        .await?;
        report(Report::Bytes { bytes, total });
        report(Report::Verifying);
        if got != expected {
            bail!("checksum mismatch for {archive}: expected {expected}, got {got}");
        }
        Ok(bytes)
    }
    .await;
    if downloaded.is_err() {
        let _ = tokio::fs::remove_file(&part).await;
    }
    downloaded?;

    // Unpack into scratch and reassemble the pieces the release deliberately left out. Both scratch
    // directories go on every way out; the final move is the only thing that touches `versions/<tag>`.
    report(Report::Unpacking);
    let unpack = versions.join(format!(".{tag}.unpack"));
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    tokio::fs::create_dir_all(&unpack).await?;
    let outcome = async {
        exec(
            Command::new("tar")
                .arg("-xzf")
                .arg(&part)
                .arg("-C")
                .arg(&unpack),
        )
        .await?;
        let unpacked = unpack.join("colonizer");
        check_unpacked(&unpacked)?;
        report(Report::Installing);
        // The Anthropic SDK the agent module runs is not in the release (not ours to redistribute);
        // without it every colony fails, so a fetch that cannot be reassembled fails the update.
        fetch_at_install(client, &unpack.join(".fetch"), &unpacked).await?;
        copy_guest_claude(&unpacked).await?;
        move_into_place(&unpacked, &target).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    let _ = tokio::fs::remove_file(&part).await;
    outcome?;
    Ok(target)
}

/// The two things an unpacked bundle must have before anything moves it into place.
fn check_unpacked(unpacked: &Path) -> Result<()> {
    if !unpacked.is_dir() {
        bail!("the archive has no colonizer/ directory at its top level");
    }
    let bin = unpacked.join("bin/colonizer");
    if !is_executable(&bin) {
        bail!("the archive has no executable colonizer/bin/colonizer");
    }
    if !unpacked.join("VERSION").is_file() {
        bail!("the archive has no colonizer/VERSION");
    }
    Ok(())
}

/// Moves the staged copy to `versions/<tag>`. A copy already there is replaced — except the one this
/// very process is running from, which nothing may pull out from under it.
async fn move_into_place(unpacked: &Path, target: &Path) -> Result<()> {
    let mut aside: Option<PathBuf> = None;
    if let Ok(meta) = tokio::fs::symlink_metadata(target).await {
        if meta.is_dir() {
            if running_from(target) {
                bail!(
                    "refusing to replace {}: this mothership is running from it",
                    target.display()
                );
            }
            // Deleting the old copy outright would empty any colony still mounting out of it, so
            // it is renamed aside — a rename keeps every mount and open file alive. It goes away
            // only once the new copy holds the name, and then only when nothing can reach it; a
            // deferred leftover is reclaimed by [`reclaim_leftovers`] at the next startup, or by
            // the next installer run.
            let aside_path = aside_name(target);
            tokio::fs::rename(target, &aside_path)
                .await
                .with_context(|| format!("moving the old {} aside", target.display()))?;
            aside = Some(aside_path);
        } else {
            // A leftover symlink or file is simply unlinked, never followed.
            tokio::fs::remove_file(target)
                .await
                .with_context(|| format!("removing the old {}", target.display()))?;
        }
    }
    if let Err(e) = tokio::fs::rename(unpacked, target).await {
        // The staged copy could not take the name: put the old copy straight back, the way the
        // installers do. Left where it was, `app` would dangle — and the only remaining copy
        // would sit under a leftover name that a later pass's reclaim deletes.
        if let Some(aside_path) = &aside {
            let _ = tokio::fs::rename(aside_path, target).await;
        }
        return Err(e).with_context(|| format!("moving the new release into {}", target.display()));
    }
    // The new copy holds the name, so the old one is only history — unless something still holds
    // it, in which case it stays as a leftover for a later pass to reclaim.
    if let Some(aside_path) = &aside {
        if !dir_in_use(aside_path) {
            let _ = tokio::fs::remove_dir_all(aside_path).await;
        }
    }
    Ok(())
}

/// The name to stage a replaced version aside to: `.<version>.old`, or `.old-2`, `.old-3`, … when
/// a leftover still holds the canonical name. The same convention the installers use, so every
/// side's reclaim recognises the leftovers.
fn aside_name(target: &Path) -> PathBuf {
    let dir = target.parent().unwrap_or(target);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut aside = dir.join(format!(".{name}.old"));
    let mut n = 2;
    while std::fs::symlink_metadata(&aside).is_ok() {
        aside = dir.join(format!(".{name}.old-{n}"));
        n += 1;
    }
    aside
}

/// Whether the running binary lives inside `dir`: the one version directory an update must never
/// replace, because this process's own image and the colonies' mounts follow it.
fn running_from(dir: &Path) -> bool {
    let Ok(exe) = std::env::current_exe().and_then(|exe| exe.canonicalize()) else {
        return false;
    };
    let Ok(real) = dir.canonicalize() else {
        return false;
    };
    exe.starts_with(real)
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Streams a response body into `file`, hashing it as it goes and reporting progress about every
/// megabyte. Returns the byte count and the sha256 of what arrived.
async fn stream_to(
    response: reqwest::Response,
    file: &mut tokio::fs::File,
    mut on_progress: impl FnMut(u64),
) -> Result<(u64, String)> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut stream = response.bytes_stream();
    let mut bytes = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was interrupted")?;
        digest.update(&chunk);
        file.write_all(&chunk).await?;
        bytes += chunk.len() as u64;
        if bytes - reported >= PROGRESS_STEP {
            reported = bytes;
            on_progress(bytes);
        }
    }
    file.flush().await?;
    Ok((bytes, hex(digest.finish().as_ref())))
}

/// Fetches a small text file, refusing anything past `cap`: a check that is supposed to cost a
/// request or two must not be turned into a way to fill the disk.
async fn fetch_capped(client: &reqwest::Client, url: &str, cap: usize) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|len| len as usize > cap)
    {
        bail!("{url} is larger than {cap} bytes; that is not the file this request was for");
    }
    let body = response.bytes().await?;
    if body.len() > cap {
        bail!("{url} is larger than {cap} bytes; that is not the file this request was for");
    }
    String::from_utf8(body.to_vec()).with_context(|| format!("{url} is not UTF-8"))
}

/// The hash a SHA256SUMS names for `name`. Lines are `<hex>  <name>` in the text form and
/// `<hex> *<name>` in the binary form; only a 64-hexdigest line counts (the same match
/// `install-release.sh` makes with its awk).
pub fn sha256sums_entry(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let Some((hash, rest)) = line.split_once(' ') else {
            continue;
        };
        let file = match rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*')) {
            Some(file) => file,
            None => continue,
        };
        if file == name && is_sha256(hash) {
            return Some(hash.to_string());
        }
    }
    None
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// One line of a `fetch-at-install` file: `<path> <tarball url> <sha256>` — a package the release
/// left out because it is not ours to redistribute (scripts/record-fetch-at-install.mjs wrote it).
/// Blank and `#` lines are skipped; anything else malformed is dropped.
pub fn parse_fetch_records(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.to_string(),
                fields.next()?.to_string(),
                fields.next()?.to_string(),
            ))
        })
        .collect()
}

/// Reassembles what `install-release.sh`'s `fetch_at_install` does at first install: for every
/// `modules/agents/*/fetch-at-install`, download each record's tarball, check it, and move its
/// `package/` directory to `<module>/<path>`. The `fetch-at-install` file itself stays in place,
/// the way the script leaves it. `scratch` is a directory inside the unpack stage, so whatever it
/// leaves behind is removed with the stage.
async fn fetch_at_install(client: &reqwest::Client, scratch: &Path, unpacked: &Path) -> Result<()> {
    let agents = unpacked.join("modules/agents");
    if !agents.is_dir() {
        return Ok(());
    }
    tokio::fs::create_dir_all(scratch)
        .await
        .with_context(|| format!("creating {}", scratch.display()))?;
    let mut modules = tokio::fs::read_dir(&agents)
        .await
        .with_context(|| format!("reading {}", agents.display()))?;
    while let Some(module) = modules.next_entry().await? {
        let record = module.path().join("fetch-at-install");
        if !record.is_file() {
            continue;
        }
        let text = tokio::fs::read_to_string(&record)
            .await
            .with_context(|| format!("reading {}", record.display()))?;
        for (path, url, sha) in parse_fetch_records(&text) {
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            let tgz = scratch.join(format!("{name}.tgz"));
            let into = scratch.join(&name);
            if let Err(e) = fetch_package(client, &url, &sha, &tgz, &into).await {
                bail!(
                    "the {} module needs {} and it would not install: {e:#}",
                    module.path().display(),
                    url
                );
            }
            let dest = module.path().join(&path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let _ = tokio::fs::remove_dir_all(&dest).await;
            tokio::fs::rename(into.join("package"), &dest)
                .await
                .with_context(|| format!("moving the downloaded package to {}", dest.display()))?;
        }
    }
    Ok(())
}

/// Downloads one non-redistributable package, checks its sha256, and unpacks it into `into`.
async fn fetch_package(
    client: &reqwest::Client,
    url: &str,
    sha: &str,
    tgz: &Path,
    into: &Path,
) -> Result<()> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()?;
    let mut file = tokio::fs::File::create(tgz)
        .await
        .with_context(|| format!("creating {}", tgz.display()))?;
    let (_, got) = stream_to(response, &mut file, |_| {}).await?;
    drop(file);
    if got != sha {
        bail!("checksum mismatch for {url}: expected {sha}, got {got}");
    }
    let _ = tokio::fs::remove_dir_all(into).await;
    tokio::fs::create_dir_all(into).await?;
    exec(Command::new("tar").arg("-xzf").arg(tgz).arg("-C").arg(into)).await?;
    if !into.join("package").is_dir() {
        bail!("{url} has no package/ directory");
    }
    Ok(())
}

/// On a Mac, colonies run a Linux build of Claude Code that the release does not ship (not ours to
/// redistribute). `install-release.sh` fetches it from Anthropic's own channel against their
/// manifest; an in-app update deliberately does not reimplement that: it copies the running
/// install's binary forward — the same reuse the script makes when the hash matches — and refuses
/// the update when there is no copy to carry, telling the operator to run the release installer
/// once. The cost of copying forward is that the guest's Claude Code may lag the pinned one until
/// the next installer run; the alternative was reimplementing Anthropic's manifest plumbing (its
/// channel endpoint and plutil) inside the mothership, for a download that happens once per machine.
async fn copy_guest_claude(unpacked: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let out = unpacked.join("bin/claude-guest");
    if out.exists() {
        return Ok(());
    }
    // The running install's copy: this binary is <version>/bin/colonizer, so its neighbour is the
    // <version>/bin/claude-guest the previous install verified.
    let previous = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .and_then(|bin| bin.parent().map(|bin| bin.join("claude-guest")))
        .filter(|guest| guest.is_file());
    let Some(previous) = previous else {
        bail!(
            "the new release needs bin/claude-guest for the colonies and there is no previous copy to carry forward; \
             run the release installer once (curl -fsSL https://colonizer.dev/install.sh | sh)"
        );
    };
    tokio::fs::copy(&previous, &out)
        .await
        .with_context(|| format!("carrying {} forward", previous.display()))?;
    tokio::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755))
        .await
        .with_context(|| format!("making {} executable", out.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pointing the app at a version, and pruning the rest
// ---------------------------------------------------------------------------

/// Points `<root>/app` at `versions/<version>`. The new link is staged as `.app.new` — a name that
/// is only ever a link — and renamed over `app`: a rename over an existing symlink is atomic on
/// POSIX, so nothing that reads through `app` (this process's own path, a colony's mount table, the
/// `~/.local/bin` link) ever sees it missing or dangling.
pub fn point_app_at(root: &Path, version: &str) -> Result<()> {
    check_version_token(version)?;
    let app = root.join("app");
    if std::fs::symlink_metadata(&app)
        .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.is_dir())
    {
        bail!(
            "{} is a real directory; run the release installer once to move to the versioned layout",
            app.display()
        );
    }
    if !root.join("versions").join(version).is_dir() {
        bail!(
            "{} is not installed; there is nothing to point {} at",
            version,
            app.display()
        );
    }
    let staged = root.join(".app.new");
    let _ = std::fs::remove_file(&staged); // a leftover from an interrupted run; this name is only ever a link
    std::os::unix::fs::symlink(format!("versions/{version}"), &staged)
        .with_context(|| format!("staging {}", staged.display()))?;
    std::fs::rename(&staged, &app)
        .with_context(|| format!("pointing {} at versions/{version}", app.display()))
}

/// Which names under `versions/` survive a prune: the version this process runs from, whatever `app`
/// names now, and the version directory of every colony still live (its read-only mounts follow that
/// directory, so deleting it would reach into a running colony). Everything else is only disk.
pub fn keep_versions(
    running: &str,
    app_target: Option<&str>,
    live_app_dirs: &[String],
) -> BTreeSet<String> {
    let mut keep = BTreeSet::from([running.to_string()]);
    if let Some(target) = app_target {
        keep.insert(target.to_string());
    }
    keep.extend(live_app_dirs.iter().cloned());
    keep
}

/// The `versions/<name>` entry a session's recorded `app_dir` names, when it names one. Recordings
/// from an older build, or from a legacy install, name no version and are ignored by pruning.
pub fn version_of_app_dir(app_dir: &str) -> Option<String> {
    let dir = std::fs::canonicalize(app_dir).ok()?;
    if dir.parent()?.file_name()?.to_str()? != "versions" {
        return None;
    }
    dir.file_name()?.to_str().map(str::to_string)
}

/// Removes every `versions/` entry not in `keep`, returning their names. Entries are never followed
/// out of `versions/`: an entry that is a symlink is unlinked, not descended into. The keep-set is
/// a recording of who can still reach a version, and a recording can be stale — a session gone
/// non-live, a record lost — so the same test the installers defer their deletes under gets a veto
/// too: whatever [`dir_in_use`] still sees held stays, whatever the keep-set says. The dot-prefixed
/// staging names belong to an install in progress and are none of this pass's business; the
/// dot-prefixed leftovers a deferred delete left behind are [`reclaim_leftovers`]'s.
pub fn prune(root: &Path, keep: &BTreeSet<String>) -> Result<Vec<String>> {
    let versions = root.join("versions");
    let mut removed = Vec::new();
    let entries =
        std::fs::read_dir(&versions).with_context(|| format!("reading {}", versions.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || keep.contains(&name) {
            continue;
        }
        if dir_in_use(&entry.path()) {
            continue; // still held, whatever the keep-set says
        }
        // file_type() reads the directory entry itself, so a symlink is seen as a symlink: removing
        // it cannot follow it anywhere.
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        } else {
            std::fs::remove_file(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        }
        removed.push(name);
    }
    Ok(removed)
}

/// Decodes the escapes `/proc/self/mountinfo` writes into its path fields — `\040` space, `\011`
/// tab, `\012` newline, `\134` backslash. Compared raw, every mount under a path holding any of
/// those four characters would silently miss, and a directory a colony still holds would be
/// reported free. Decoded in one pass, so the `\134` that stands for a literal backslash can
/// never be decoded again out of what it decodes to: a file literally named `\040` arrives as
/// `\134040` and must still be `\040` afterwards.
fn unescape_mountinfo(path: &str) -> String {
    if !path.contains('\\') {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let decoded = if tail.starts_with("040") {
            Some((' ', 4))
        } else if tail.starts_with("011") {
            Some(('\t', 4))
        } else if tail.starts_with("012") {
            Some(('\n', 4))
        } else if tail.starts_with("134") {
            Some(('\\', 4))
        } else {
            None
        };
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[i + len..];
            }
            None => {
                out.push('\\');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether `whole` is `prefix` itself or a path under it: a bare string prefix would answer yes
/// for `/home/userX` when asked about `/home/user`, and for `v0.1.10` when asked about `v0.1.1`,
/// so the next character has to be the path separator — the same boundary the shell draws with
/// `case $dir in "$mp"/*)`.
fn under_or_equal(whole: &str, prefix: &str) -> bool {
    whole == prefix
        || (whole.len() > prefix.len()
            && whole.starts_with(prefix)
            && whole.as_bytes()[prefix.len()] == b'/')
}

/// Whether anything still holds `dir`: a path under it is a mount point, or a running process's own
/// executable resolves into it. Colonies bind-mount their agent, plugins and vendored tools
/// read-only out of their version directory, and a mothership executes out of the one it booted
/// from — and renaming a directory keeps all of that alive, since a mount follows the inode. Only
/// ever asked before a delete. Where there is no /proc to ask (macOS ships none), the answer is
/// yes: an old version kept a while longer costs disk, a colony whose mounts went empty costs a
/// restart.
fn dir_in_use(dir: &Path) -> bool {
    if !Path::new("/proc/self").is_dir() {
        return true;
    }
    let Ok(real) = std::fs::canonicalize(dir) else {
        return false; // nothing resolves there any more, so nothing holds it
    };
    let real = real.to_string_lossy().into_owned();
    let real_under = format!("{real}/");
    // /proc/self/mountinfo, not /proc/self/mounts: only mountinfo says where a bind mount's tree
    // is rooted in its filesystem (field 4), which is the path a colony's read-only mounts were
    // made from — and that path follows a rename, because a mount follows the inode. It is
    // relative to whatever filesystem the directory sits on, so it is compared with the directory
    // relative to the deepest mount point over it. Both path fields are decoded first
    // ([`unescape_mountinfo`]) and the mount point is matched with a path boundary
    // ([`under_or_equal`]): compared raw, a path holding a space never matches and a sibling name
    // that extends another matches too much — either way a directory a colony still holds is
    // reported free.
    let info = match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(info) => info,
        Err(_) => return true, // no way to tell after all, so never delete
    };
    let mounts: Vec<(String, String)> = info
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let rooted_at = fields.nth(3)?;
            let mounted_at = fields.next()?;
            Some((unescape_mountinfo(rooted_at), unescape_mountinfo(mounted_at)))
        })
        .collect();
    let mut base = "";
    for (_, mounted_at) in &mounts {
        if under_or_equal(&real, mounted_at) && mounted_at.len() > base.len() {
            base = mounted_at;
        }
    }
    let relative = if base.is_empty() || base == "/" {
        real.clone()
    } else {
        real[base.len()..].to_string()
    };
    let relative_under = format!("{relative}/");
    for (rooted_at, mounted_at) in &mounts {
        if *mounted_at == real || mounted_at.starts_with(&real_under) {
            return true; // something is mounted on or under the directory
        }
        if *rooted_at == relative || rooted_at.starts_with(&relative_under) {
            return true; // a colony bind-mounts out of it
        }
    }
    // /proc/<pid>/exe is the kernel's own resolution of the process's binary, symlinks included;
    // an entry that will not read is a process that is already gone.
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return true;
    };
    for process in processes.flatten() {
        if !process
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if let Ok(exe) = std::fs::read_link(process.path().join("exe")) {
            let exe = exe.to_string_lossy();
            if exe == real || exe.starts_with(&real_under) {
                return true;
            }
        }
    }
    false
}

/// The version token a staging name under `versions/` was made for, when it is one:
/// `.<version>.part`, `.<version>.unpack` or `.<version>.new` — the names an interrupted install
/// leaves behind. Anything else dot-prefixed is not a staging name and stays.
fn staging_of(name: &str) -> Option<String> {
    let stem = name.strip_prefix('.')?;
    for suffix in [".part", ".unpack", ".new"] {
        if let Some(version) = stem.strip_suffix(suffix) {
            return check_version_token(version).ok().map(|()| version.to_string());
        }
    }
    None
}

/// Removes what earlier passes could not safely delete: the copies an installer renamed aside
/// (`versions/.<version>.old`, or `.old-2`, `.old-3`, … when that name was still owed), the
/// migrated legacy app directory (`<root>/.app.legacy*`), and the staging names (`.<version>.new`,
/// `.part`, `.unpack`) of an install that died mid-flight. Each goes only once nothing mounts or
/// executes out of it any more — the same test the installers defer their deletes under — so a
/// colony still holding one keeps it for a later pass. This runs once at startup, with no update
/// in flight, so a staging name found here belongs to an install that is not coming back; the
/// shell installers, which can race a live one, leave staging names alone. A leftover that would
/// not go stays for the next startup. Returns the names removed.
fn reclaim_leftovers(root: &Path) -> Vec<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("versions")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_leftover =
                name.starts_with('.') && (name.ends_with(".old") || name.contains(".old-"));
            if is_leftover || staging_of(&name).is_some() {
                candidates.push(entry.path());
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".app.legacy" || name.starts_with(".app.legacy-") {
                candidates.push(entry.path());
            }
        }
    }
    let mut removed = Vec::new();
    for leftover in candidates {
        if dir_in_use(&leftover) {
            continue;
        }
        // A .part leftover is a file — half a bundle — the rest are directories.
        let gone = if leftover.is_dir() {
            std::fs::remove_dir_all(&leftover)
        } else {
            std::fs::remove_file(&leftover)
        };
        if gone.is_ok() {
            if let Some(name) = leftover.file_name() {
                removed.push(name.to_string_lossy().into_owned());
            }
        }
    }
    removed
}

/// Startup housekeeping, run once recovery has said which colonies are still live: remove the
/// versions nothing can reach any more. This process's own directory, the one `app` names, and every
/// live colony's recorded version are kept; an update already on its way keeps its hands off.
pub async fn prune_unused(app: &Shared) {
    let Layout::Versioned { root, version } = layout(&app.cfg) else {
        return;
    };
    if app.updates.apply.lock().await.running() {
        return; // an apply may have staged a version nothing points at yet
    }
    let sessions = app.sessions.read().await.clone();
    let live: Vec<String> = sessions
        .iter()
        .filter(|s| s.status.is_live())
        .filter_map(|s| s.app_dir.as_deref())
        .filter_map(version_of_app_dir)
        .collect();
    let app_target = std::fs::read_link(root.join("app"))
        .ok()
        .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string));
    let keep = keep_versions(&version, app_target.as_deref(), &live);
    let mut removed = match prune(&root, &keep) {
        Ok(removed) => removed,
        Err(e) => {
            eprintln!("pruning old versions: {e:#}");
            return;
        }
    };
    // The shell installers defer exactly these deletes for a colony still holding them; this is the
    // pass that reclaims them when no installer will run again.
    removed.extend(reclaim_leftovers(&root));
    if !removed.is_empty() {
        println!("pruned old versions: {}", removed.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "colonizer-install-{name}-{}",
            crate::util::short_id()
        ))
    }

    /// A versioned farm: `versions/v0.1.3` with the `app` link pointing at it, like an installed
    /// mothership; `bin/` and `vendor/` are there so it also passes for a real one.
    fn versioned_farm(root: &Path, version: &str) {
        std::fs::create_dir_all(root.join("versions").join(version).join("bin")).unwrap();
        std::os::unix::fs::symlink(format!("versions/{version}"), root.join("app")).unwrap();
    }

    #[test]
    fn layout_detects_versioned_legacy_source_and_nothing_else() {
        let root = tempdir("layout-versioned");
        versioned_farm(&root, "v0.1.3");
        assert_eq!(
            layout_for(Some(&root.join("app"))),
            Layout::Versioned {
                root: root.clone(),
                version: "v0.1.3".into()
            },
            "through the link, like resolve_assets() hands them over"
        );
        assert_eq!(
            layout_for(Some(&root.join("versions/v0.1.3"))),
            Layout::Versioned {
                root: root.clone(),
                version: "v0.1.3".into()
            },
            "or named directly, like a COLONIZER_HOME that follows the link"
        );
        // An `app` link naming a different version than the assets is not a layout to update in.
        std::os::unix::fs::symlink("versions/v0.1.3", root.join("app.other")).unwrap();
        assert_eq!(
            layout_for(Some(&root.join("versions/v0.1.3").join("bin"))),
            Layout::Unknown
        );
        std::fs::remove_dir_all(&root).unwrap();

        let root = tempdir("layout-legacy");
        std::fs::create_dir_all(root.join("app/bin")).unwrap();
        assert_eq!(
            layout_for(Some(&root.join("app"))),
            Layout::Legacy { root: root.clone() }
        );
        std::fs::remove_dir_all(&root).unwrap();

        let root = tempdir("layout-source");
        std::fs::create_dir_all(root.join("dist/vendor")).unwrap();
        assert_eq!(layout_for(Some(&root.join("dist"))), Layout::Source);
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(layout_for(None), Layout::Unknown);
        let root = tempdir("layout-unknown");
        std::fs::create_dir_all(root.join("somewhere")).unwrap();
        assert_eq!(layout_for(Some(&root.join("somewhere"))), Layout::Unknown);
        assert_eq!(layout_for(Some(&root.join("missing"))), Layout::Unknown);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_version_must_be_a_plain_v_or_dev_token() {
        for good in ["v0.1.3", "v0.2.0-rc.1", "dev-20260917"] {
            assert!(check_version_token(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "v",
            "0.1.3",
            "latest",
            "../v0.1.3",
            "v0.1.3 x",
            ".hidden",
            "v0.1.3/../../x",
            "dev",
        ] {
            assert!(check_version_token(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn checksum_lists_are_read_in_both_name_forms_and_missing_entries_bail() {
        let sums = "\
1111111111111111111111111111111111111111111111111111111111111111  colonizer-linux-x86_64.tar.gz
2222222222222222222222222222222222222222222222222222222222222222 *colonizer-darwin-arm64.tar.gz
notahash  colonizer-fake.tar.gz
3333333333333333333333333333333333333333333333333333333333333333  other-file.txt
";
        assert_eq!(
            sha256sums_entry(sums, "colonizer-linux-x86_64.tar.gz"),
            Some("1".repeat(64))
        );
        assert_eq!(
            sha256sums_entry(sums, "colonizer-darwin-arm64.tar.gz"),
            Some("2".repeat(64))
        );
        assert_eq!(
            sha256sums_entry(sums, "other-file.txt"),
            Some("3".repeat(64))
        );
        assert_eq!(
            sha256sums_entry(sums, "colonizer-fake.tar.gz"),
            None,
            "a line that is not a sha256 is not an entry"
        );
        assert_eq!(sha256sums_entry(sums, "colonizer-not-there.tar.gz"), None);
    }

    #[test]
    fn fetch_records_are_path_url_and_hash() {
        let records = "\
sdk/anthropic  https://registry.npmjs.org/@anthropic-ai/sdk/-/sdk-0.60.0.tgz  abc
# a comment
sdk  https://registry.npmjs.org/x/-/x-1.0.0.tgz  def

only-two-fields
";
        let parsed = parse_fetch_records(records);
        assert_eq!(
            parsed,
            [
                (
                    "sdk/anthropic".into(),
                    "https://registry.npmjs.org/@anthropic-ai/sdk/-/sdk-0.60.0.tgz".into(),
                    "abc".into()
                ),
                (
                    "sdk".into(),
                    "https://registry.npmjs.org/x/-/x-1.0.0.tgz".into(),
                    "def".into()
                ),
            ]
        );
        assert_eq!(parse_fetch_records(""), Vec::new());
    }

    #[test]
    fn this_process_is_never_running_from_an_empty_temp_dir() {
        assert!(!running_from(&tempdir("running-from")));
        assert!(!running_from(&PathBuf::from("/definitely/not/here")));
    }

    #[test]
    fn point_app_at_swaps_the_link_and_refuses_a_real_directory() {
        let root = tempdir("point-app");
        versioned_farm(&root, "v0.1.3");
        std::fs::create_dir_all(root.join("versions/v0.1.4")).unwrap();

        point_app_at(&root, "v0.1.4").unwrap();
        assert_eq!(
            std::fs::read_link(root.join("app")).unwrap(),
            PathBuf::from("versions/v0.1.4"),
            "the target is relative"
        );
        assert_eq!(
            std::fs::canonicalize(root.join("app")).unwrap(),
            root.join("versions/v0.1.4")
        );

        // Swapping again must work, and `app` is never left missing between the two.
        point_app_at(&root, "v0.1.3").unwrap();
        assert_eq!(
            std::fs::read_link(root.join("app")).unwrap(),
            PathBuf::from("versions/v0.1.3")
        );
        assert!(
            root.join(".app.new").symlink_metadata().is_err(),
            "no staging link left behind"
        );

        assert!(
            point_app_at(&root, "v0.1.9").is_err(),
            "a version that is not installed points nowhere"
        );
        assert!(
            point_app_at(&root, "../versions/v0.1.4").is_err(),
            "an escaping token is refused"
        );
        assert_eq!(
            std::fs::read_link(root.join("app")).unwrap(),
            PathBuf::from("versions/v0.1.3"),
            "the refusals changed nothing"
        );

        let real = tempdir("point-app-legacy");
        std::fs::create_dir_all(real.join("app/bin")).unwrap();
        assert!(
            point_app_at(&real, "v0.1.4").is_err(),
            "a real app directory is the installer's to migrate, not ours"
        );
        assert!(real.join("app/bin").is_dir(), "the refusal changed nothing");

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&real).unwrap();
    }

    #[test]
    fn pruning_keeps_what_anything_still_runs_from() {
        let keep = keep_versions("v0.1.4", Some("v0.1.3"), &["v0.1.2".into()]);
        assert_eq!(
            keep,
            BTreeSet::from(["v0.1.2".into(), "v0.1.3".into(), "v0.1.4".into()])
        );
        let keep = keep_versions("v0.1.4", None, &[]);
        assert_eq!(keep, BTreeSet::from(["v0.1.4".into()]));
        let keep = keep_versions("v0.1.4", Some("v0.1.4"), &[]);
        assert_eq!(
            keep,
            BTreeSet::from(["v0.1.4".into()]),
            "the running version and the app target are one"
        );
    }

    #[test]
    fn pruning_removes_unkept_entries_without_following_symlinks_out() {
        let root = tempdir("prune");
        versioned_farm(&root, "v0.1.4");
        for name in ["v0.1.3", "v0.1.2"] {
            std::fs::create_dir_all(root.join("versions").join(name)).unwrap();
        }
        // A symlink under versions/ pointing at a file well outside it: pruning must unlink the
        // link, never the thing it points at.
        let outside = tempdir("prune-outside");
        std::fs::write(&outside, "precious").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("versions/stray")).unwrap();
        std::fs::write(root.join("versions/.v0.1.5.new"), "staging").unwrap();

        let keep = keep_versions("v0.1.4", Some("v0.1.3"), &[]);
        let mut removed = prune(&root, &keep).unwrap();
        removed.sort();
        assert_eq!(
            removed,
            vec!["stray".to_string(), "v0.1.2".to_string()],
            "read_dir order is not ours to pick"
        );
        assert!(root.join("versions/v0.1.4").is_dir());
        assert!(root.join("versions/v0.1.3").is_dir());
        assert!(!root.join("versions/v0.1.2").exists());
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"precious",
            "the symlink was unlinked, not followed"
        );
        assert!(
            root.join("versions/.v0.1.5.new").exists(),
            "staging names are not this pass's business"
        );

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_file(&outside).unwrap();
    }

    #[test]
    fn a_recorded_app_dir_names_a_version_only_from_a_versions_directory() {
        let root = tempdir("app-dir");
        versioned_farm(&root, "v0.1.3");
        let recorded = root.join("versions/v0.1.3").display().to_string();
        assert_eq!(version_of_app_dir(&recorded).as_deref(), Some("v0.1.3"));
        let legacy = tempdir("app-dir-legacy");
        std::fs::create_dir_all(legacy.join("app")).unwrap();
        assert_eq!(
            version_of_app_dir(&legacy.join("app").display().to_string()),
            None,
            "a legacy dir names no version"
        );
        assert_eq!(version_of_app_dir("/definitely/not/here"), None);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&legacy).unwrap();
    }

    /// /proc is the only witness for what still holds a directory, so these say nothing on a system
    /// without one — where the answer is always "in use" and nothing is ever reclaimed.
    #[cfg(target_os = "linux")]
    #[test]
    fn deferred_leftovers_are_reclaimed_once_nothing_holds_them() {
        let root = tempdir("reclaim");
        for leftover in ["versions/.v0.1.2.old", "versions/.v0.1.2.old-2", ".app.legacy"] {
            std::fs::create_dir_all(root.join(leftover).join("bin")).unwrap();
        }
        // The staging names of an install that died mid-flight are abandoned the same way, so this
        // pass takes them too — a half-downloaded .part is a file, not a directory. A dot name
        // that is neither a leftover nor a staging name is none of this pass's business.
        std::fs::create_dir_all(root.join("versions/.v0.9.9.new")).unwrap();
        std::fs::write(root.join("versions/.v0.9.9.part"), "half a bundle").unwrap();
        std::fs::create_dir_all(root.join("versions/.v0.9.9.unpack/colonizer")).unwrap();
        std::fs::create_dir_all(root.join("versions/.stray")).unwrap();
        std::fs::create_dir_all(root.join("versions/.NotAToken.new")).unwrap();

        let mut removed = reclaim_leftovers(&root);
        removed.sort();
        assert_eq!(
            removed,
            vec![
                ".app.legacy",
                ".v0.1.2.old",
                ".v0.1.2.old-2",
                ".v0.9.9.new",
                ".v0.9.9.part",
                ".v0.9.9.unpack",
            ],
            "read_dir order is not ours to pick"
        );
        assert!(!root.join(".app.legacy").exists());
        assert!(!root.join("versions/.v0.1.2.old").exists());
        assert!(!root.join("versions/.v0.1.2.old-2").exists());
        assert!(!root.join("versions/.v0.9.9.new").exists(), "staging goes too");
        assert!(!root.join("versions/.v0.9.9.part").exists());
        assert!(!root.join("versions/.v0.9.9.unpack").exists());
        assert!(
            root.join("versions/.stray").is_dir(),
            "not a leftover or staging name"
        );
        assert!(
            root.join("versions/.NotAToken.new").is_dir(),
            "not a version's staging name"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// mountinfo cannot write four characters raw into a path field, and a field compared against
    /// a real path before its `\040`/`\011`/`\012`/`\134` are decoded never matches: every mount
    /// under a `$HOME` holding a space would answer "free" and the delete would empty it.
    #[test]
    fn mountinfo_escapes_are_decoded_before_any_comparison() {
        assert_eq!(unescape_mountinfo("/tmp/sp\\040ace"), "/tmp/sp ace");
        assert_eq!(unescape_mountinfo("/a\\040b\\011c"), "/a b\tc");
        assert_eq!(unescape_mountinfo("/plain/path"), "/plain/path");
        assert_eq!(unescape_mountinfo("/no/escape\\x"), "/no/escape\\x");
        // A literal \040 in a name arrives as \134040 and must still be \040 afterwards: the
        // backslash's own escape is decoded last, in the same pass, never re-read.
        assert_eq!(unescape_mountinfo("/a\\134040b"), "/a\\040b");
        assert_eq!(unescape_mountinfo("/a\\134b"), "/a\\b");
        assert_eq!(unescape_mountinfo("trailing\\"), "trailing\\");
        // And the boundary: a prefix that only extends to the middle of a name is not a mount
        // point over it, or /home/user would answer for /home/userX.
        assert!(under_or_equal("/home/user", "/home/user"));
        assert!(under_or_equal("/home/user/x", "/home/user"));
        assert!(!under_or_equal("/home/userX", "/home/user"));
        assert!(!under_or_equal("/home/userx", "/home/user"));
        assert!(!under_or_equal("/home/u", "/home/user"));
        // "/" matches only itself: with no deeper mount point the directory is compared as the
        // whole path whether the base is "/" or empty, so the missing prefix costs nothing.
        assert!(under_or_equal("/", "/"));
        assert!(!under_or_equal("/anywhere", "/"));
    }

    /// A leftover a process still executes out of is left for the next pass, and goes once the
    /// process is gone: the same holder the installers defer for, reproduced with a shell instead
    /// of a colony.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_leftover_a_process_still_executes_out_of_is_left_for_the_next_pass() {
        let root = tempdir("reclaim-in-use");
        let leftover = root.join("versions/.v0.1.2.old");
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::copy("/bin/sh", leftover.join("sh")).unwrap();
        std::fs::set_permissions(leftover.join("sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(!dir_in_use(&leftover));

        // The trailing builtin keeps dash from execing sleep in its own image: the process that
        // must be seen here is the copy inside the leftover.
        let mut child = std::process::Command::new(leftover.join("sh"))
            .arg("-c")
            .arg("sleep 30; :")
            .spawn()
            .unwrap();
        assert!(dir_in_use(&leftover), "the shell runs out of the leftover");
        assert!(
            reclaim_leftovers(&root).is_empty(),
            "a process still holds it, so nothing goes"
        );
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(!dir_in_use(&leftover), "the process is gone");
        assert_eq!(reclaim_leftovers(&root), vec![".v0.1.2.old"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The executable test above proves the plumbing; this one reproduces what a colony actually
    /// does — bind-mount read-only out of its version directory — when the machine lets us make
    /// such a mount, which it needs privileges for.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_bind_mount_out_of_a_directory_counts_as_in_use() {
        let dir = tempdir("in-use-mount");
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(dir.join("plugins/file"), "mounted").unwrap();
        let at = tempdir("in-use-mount-target");
        std::fs::create_dir_all(&at).unwrap();
        let mounted = std::process::Command::new("mount")
            .arg("--bind")
            .arg(dir.join("plugins"))
            .arg(&at)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !mounted {
            eprintln!("skipping: this machine will not make a bind mount");
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::remove_dir_all(&at).unwrap();
            return;
        }
        // Renaming does not disturb the mount, and the check still sees it through the new name.
        let aside = tempdir("in-use-mount-aside");
        std::fs::rename(&dir, &aside).unwrap();
        assert!(dir_in_use(&aside), "the mount follows the rename");
        let unmounted = std::process::Command::new("umount")
            .arg(&at)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(unmounted, "umount failed");
        assert!(!dir_in_use(&aside), "the mount is gone");
        std::fs::remove_dir_all(&aside).unwrap();
        std::fs::remove_dir_all(&at).unwrap();
    }

    /// The same mount, with a space somewhere on the path: mountinfo cannot write one raw, and a
    /// field compared against the escaped form never matched, so a directory still mounted
    /// answered "free" and the delete emptied it. Reproduced with a real mount, which needs the
    /// machine's privileges.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_mount_on_a_path_with_a_space_still_counts_as_in_use() {
        let mount = |from: &Path, to: &Path| {
            std::process::Command::new("mount")
                .arg("--bind")
                .arg(from)
                .arg(to)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let umount = |to: &Path| {
            std::process::Command::new("umount")
                .arg(to)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let base = tempdir("escaped");
        let dir = base.join("sp ace/versions/v1");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/file"), "mounted").unwrap();
        let at = tempdir("escaped-target");
        std::fs::create_dir_all(&at).unwrap();
        if !mount(&dir.join("bin"), &at) {
            eprintln!("skipping: this machine will not make a bind mount");
            std::fs::remove_dir_all(&base).unwrap();
            std::fs::remove_dir_all(&at).unwrap();
            return;
        }
        // The colony mounts out of a path whose mountinfo form holds a \040; the copy is renamed
        // aside as a deferred delete would rename it, and the check must still see the mount
        // through the escaped names.
        let aside = base.join("sp ace/versions/.v1.old");
        std::fs::rename(&dir, &aside).unwrap();
        assert!(
            dir_in_use(&aside),
            "the mount follows the rename, escapes decoded"
        );

        // And the other way round: a mount point itself under the spaced path is escaped too.
        let src = tempdir("escaped-source");
        std::fs::create_dir_all(&src).unwrap();
        let onto = aside.join("mp x");
        std::fs::create_dir_all(&onto).unwrap();
        let mounted_too = mount(&src, &onto);
        if mounted_too {
            assert!(
                dir_in_use(&aside),
                "a mount point under the spaced path is seen too"
            );
            assert!(umount(&onto), "umount of the spaced mount point failed");
        } else {
            eprintln!("skipping: the second bind mount was refused");
        }
        assert!(umount(&at), "umount failed");
        assert!(
            !dir_in_use(&aside),
            "the mounts are gone, so the directory is free"
        );
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&at).unwrap();
        std::fs::remove_dir_all(&src).unwrap();
    }

    /// A sibling whose name extends another's (`pfx` vs `pfxLONG`, `v0.1.1` vs `v0.1.10`): a bare
    /// string prefix took the decoy mount at `pfx` for a mount at `pfxLONG/…` and computed the
    /// mount-root arithmetic from the wrong base, so a directory still mounted answered "free".
    /// Reproduced with real mounts, which need the machine's privileges.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_decoy_mount_on_a_shorter_sibling_name_misses_nothing() {
        let mount = |from: &Path, to: &Path| {
            std::process::Command::new("mount")
                .arg("--bind")
                .arg(from)
                .arg(to)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let umount = |to: &Path| {
            std::process::Command::new("umount")
                .arg(to)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        let base = tempdir("prefix");
        let decoy_src = base.join("decoy-src");
        let decoy = base.join("pfx");
        std::fs::create_dir_all(&decoy_src).unwrap();
        std::fs::create_dir_all(&decoy).unwrap();
        // The live mount, under a name the decoy's name extends — and renamed aside, the way a
        // deferred delete renames the copy it could not remove yet.
        let aside = base.join("pfxLONG/versions/.v1.old");
        std::fs::create_dir_all(aside.join("bin")).unwrap();
        std::fs::write(aside.join("bin/file"), "mounted").unwrap();
        let at = tempdir("prefix-target");
        std::fs::create_dir_all(&at).unwrap();
        if !mount(&decoy_src, &decoy) {
            eprintln!("skipping: this machine will not make a bind mount");
            std::fs::remove_dir_all(&base).unwrap();
            std::fs::remove_dir_all(&at).unwrap();
            return;
        }
        if !mount(&aside.join("bin"), &at) {
            eprintln!("skipping: the second bind mount was refused");
            umount(&decoy);
            std::fs::remove_dir_all(&base).unwrap();
            std::fs::remove_dir_all(&at).unwrap();
            return;
        }
        assert!(
            dir_in_use(&aside),
            "the mount under pfxLONG is seen despite the mount at pfx"
        );
        assert!(umount(&at), "umount failed");
        assert!(umount(&decoy), "umount of the decoy failed");
        assert!(
            !dir_in_use(&aside),
            "the mounts are gone, so the directory is free"
        );
        std::fs::remove_dir_all(&base).unwrap();
        std::fs::remove_dir_all(&at).unwrap();
    }

    /// A second rename that fails puts the old copy straight back, the way the installers do:
    /// left aside, `app` would dangle — and the only remaining copy would sit under a name a
    /// later pass's reclaim deletes.
    #[tokio::test]
    async fn a_failed_move_in_puts_the_old_copy_straight_back() {
        let root = tempdir("rollback");
        let target = root.join("versions/v1");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::write(target.join("bin/file"), "old").unwrap();
        let unpacked = root.join("nothing/staged/here");

        assert!(
            move_into_place(&unpacked, &target).await.is_err(),
            "the staged copy is not there to move in"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("bin/file")).unwrap(),
            "old",
            "the old copy is back under its own name"
        );
        assert!(
            !root.join("versions/.v1.old").exists(),
            "nothing was left under an aside name"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The keep-set is only a recording of who can still reach a version, so a version it fails
    /// to name — a session gone non-live, a record lost — is spared by the in-use test instead:
    /// pruning keeps whatever is still mounted, then takes it once the mount is gone. Needs the
    /// machine's privileges, like every test with a real mount.
    #[cfg(target_os = "linux")]
    #[test]
    fn pruning_spares_a_version_still_mounted_even_when_the_keep_set_misses_it() {
        let root = tempdir("prune-in-use");
        versioned_farm(&root, "v0.1.4");
        std::fs::create_dir_all(root.join("versions/v0.1.2/plugins")).unwrap();
        std::fs::write(root.join("versions/v0.1.2/plugins/file"), "mounted").unwrap();
        let at = tempdir("prune-in-use-target");
        std::fs::create_dir_all(&at).unwrap();
        let mounted = std::process::Command::new("mount")
            .arg("--bind")
            .arg(root.join("versions/v0.1.2/plugins"))
            .arg(&at)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !mounted {
            eprintln!("skipping: this machine will not make a bind mount");
            std::fs::remove_dir_all(&root).unwrap();
            std::fs::remove_dir_all(&at).unwrap();
            return;
        }
        // v0.1.2 is in no session's recording any more — the keep-set names only v0.1.4.
        let keep = keep_versions("v0.1.4", Some("v0.1.4"), &[]);
        assert!(prune(&root, &keep).unwrap().is_empty(), "still mounted");
        assert!(
            root.join("versions/v0.1.2").is_dir(),
            "the keep-set missed it, but a colony still mounts out of it"
        );
        let unmounted = std::process::Command::new("umount")
            .arg(&at)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(unmounted, "umount failed");
        assert_eq!(prune(&root, &keep).unwrap(), vec!["v0.1.2".to_string()]);
        assert!(!root.join("versions/v0.1.2").exists());
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&at).unwrap();
    }

    #[test]
    fn an_aside_name_skips_names_already_taken() {
        let root = tempdir("aside-name");
        let target = root.join("versions/v0.1.3");
        std::fs::create_dir_all(root.join("versions/.v0.1.3.old")).unwrap();
        assert_eq!(
            aside_name(&target),
            root.join("versions/.v0.1.3.old-2"),
            "the canonical name may still be owed to a deferred delete"
        );
        std::fs::create_dir_all(root.join("versions/.v0.1.3.old-2")).unwrap();
        assert_eq!(aside_name(&target), root.join("versions/.v0.1.3.old-3"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    // -- The end-to-end install: a fake release served from a local server. ----------------------

    fn sha256_of(path: &Path) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, &std::fs::read(path).unwrap()).as_ref())
    }

    fn tar_gz(dir: &Path, name: &str, out: &Path) {
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(out)
            .arg("-C")
            .arg(dir)
            .arg(name)
            .status()
            .unwrap();
        assert!(status.success(), "tar failed");
    }

    fn write_executable(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Serves named files out of `dir`, reading them at request time so a test can build its
    /// release after the server is already up (the fetch-at-install record names the server's URL).
    async fn serve(dir: PathBuf, names: &[String]) -> String {
        let mut router = axum::Router::new();
        for name in names {
            let dir = dir.clone();
            let name = name.clone();
            router = router.route(
                &format!("/{name}"),
                axum::routing::get(move || {
                    let dir = dir.clone();
                    async move { std::fs::read(dir.join(&name)).unwrap_or_default() }
                }),
            );
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        url
    }

    #[tokio::test]
    async fn a_good_bundle_installs_and_a_corrupt_one_leaves_nothing_behind() {
        let files = tempdir("release");
        let archive = format!("colonizer-{}.tar.gz", platform().unwrap());
        let names = [
            "SHA256SUMS".to_string(),
            archive.clone(),
            "sdk.tgz".to_string(),
        ];

        // The SDK tarball the release's fetch-at-install record names.
        let pkg = files.join("pkg/package");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("index.js"), "module.exports = 1;\n").unwrap();
        tar_gz(&files.join("pkg"), "package", &files.join("sdk.tgz"));
        let sdk_sha = sha256_of(&files.join("sdk.tgz"));

        let url = serve(files.clone(), &names).await;

        // The bundle: the two files install() checks for, and one fetch-at-install record.
        let bundle = files.join("colonizer");
        write_executable(&bundle.join("bin/colonizer"), "#!/bin/sh\necho colonizer\n");
        std::fs::write(bundle.join("VERSION"), "v0.9.9").unwrap();
        std::fs::create_dir_all(bundle.join("modules/agents/claude-code")).unwrap();
        std::fs::write(
            bundle.join("modules/agents/claude-code/fetch-at-install"),
            format!("sdk {url}/sdk.tgz {sdk_sha}\n"),
        )
        .unwrap();
        tar_gz(&files, "colonizer", &files.join(&archive));
        std::fs::write(
            files.join("SHA256SUMS"),
            format!(
                "{}  {archive}\n{sdk_sha}  sdk.tgz\n",
                sha256_of(&files.join(&archive))
            ),
        )
        .unwrap();

        let root = tempdir("e2e");
        versioned_farm(&root, "v0.1.3");
        let client = reqwest::Client::new();
        let target = install(&root, "v0.9.9", &url, &client, |_| {})
            .await
            .unwrap();
        assert_eq!(target, root.join("versions/v0.9.9"));
        assert!(is_executable(&target.join("bin/colonizer")));
        assert_eq!(
            std::fs::read_to_string(target.join("VERSION")).unwrap(),
            "v0.9.9"
        );
        // The fetch-at-install package landed where the record names it, the file itself left in place.
        assert_eq!(
            std::fs::read_to_string(target.join("modules/agents/claude-code/sdk/index.js"))
                .unwrap(),
            "module.exports = 1;\n"
        );
        assert!(
            target
                .join("modules/agents/claude-code/fetch-at-install")
                .is_file()
        );
        // No scratch survived, and the app link is exactly where it was.
        assert!(
            root.join("versions/.v0.9.9.part")
                .symlink_metadata()
                .is_err()
        );
        assert!(
            root.join("versions/.v0.9.9.unpack")
                .symlink_metadata()
                .is_err()
        );
        assert_eq!(
            std::fs::read_link(root.join("app")).unwrap(),
            PathBuf::from("versions/v0.1.3")
        );
        std::fs::remove_dir_all(&root).unwrap();

        // A bundle whose hash does not match SHA256SUMS leaves nothing anywhere.
        let bad_root = tempdir("e2e-bad");
        std::fs::create_dir_all(bad_root.join("versions")).unwrap();
        std::fs::write(
            files.join("SHA256SUMS"),
            format!("{}  {archive}\n", "0".repeat(64)),
        )
        .unwrap();
        let result = install(&bad_root, "v0.9.9", &url, &client, |_| {}).await;
        assert!(result.is_err(), "a mismatched bundle must not install");
        assert!(!bad_root.join("versions/v0.9.9").exists());
        assert_eq!(
            std::fs::read_dir(bad_root.join("versions"))
                .unwrap()
                .count(),
            0,
            "no .part or .unpack scratch may survive a failed install"
        );

        // A release whose archive is not listed at all is refused before anything downloads.
        std::fs::write(
            files.join("SHA256SUMS"),
            format!("{}  some-other-file\n", "0".repeat(64)),
        )
        .unwrap();
        let result = install(&bad_root, "v0.9.9", &url, &client, |_| {}).await;
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(bad_root.join("versions"))
                .unwrap()
                .count(),
            0
        );

        std::fs::remove_dir_all(&bad_root).unwrap();
        std::fs::remove_dir_all(&files).unwrap();
    }

    #[tokio::test]
    async fn replacing_an_installed_version_renames_the_old_copy_aside() {
        let files = tempdir("replace");
        let archive = format!("colonizer-{}.tar.gz", platform().unwrap());
        let names = ["SHA256SUMS".to_string(), archive.clone()];
        let url = serve(files.clone(), &names).await;
        let bundle = files.join("colonizer");
        write_executable(&bundle.join("bin/colonizer"), "#!/bin/sh\necho one\n");
        std::fs::write(bundle.join("VERSION"), "v0.9.9").unwrap();
        let rebuild = |marker: &str| {
            write_executable(&bundle.join("bin/colonizer"), &format!("#!/bin/sh\necho {marker}\n"));
            tar_gz(&files, "colonizer", &files.join(&archive));
            std::fs::write(
                files.join("SHA256SUMS"),
                format!("{}  {archive}\n", sha256_of(&files.join(&archive))),
            )
            .unwrap();
        };
        rebuild("one");

        let root = tempdir("replace-root");
        std::fs::create_dir_all(&root).unwrap();
        let client = reqwest::Client::new();
        let target = install(&root, "v0.9.9", &url, &client, |_| {})
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("bin/colonizer")).unwrap(),
            "#!/bin/sh\necho one\n"
        );

        // The same tag again, which an update to a release the machine already fetched can do:
        // the old copy goes aside, and with nothing holding it, it goes away for good at once.
        rebuild("two");
        install(&root, "v0.9.9", &url, &client, |_| {})
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("bin/colonizer")).unwrap(),
            "#!/bin/sh\necho two\n",
            "the new copy is in place"
        );
        assert!(
            root.join("versions/.v0.9.9.old")
                .symlink_metadata()
                .is_err(),
            "nothing held the old copy, so it was reclaimed immediately"
        );
        assert!(
            root.join("versions/.v0.9.9.part")
                .symlink_metadata()
                .is_err(),
        );

        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&files).unwrap();
    }
}
