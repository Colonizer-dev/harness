//! `colonizer setup`: install a release over a `cargo install` build (#905).
//!
//! The crate is published as `colonizer-harness` (`colonizer` on crates.io is another project), and
//! `cargo install colonizer-harness` builds the `colonizer` binary alone: no `scripts/` beside it, no
//! vendored microsandbox, no agent modules, no web UI. That binary cannot run the mothership, and
//! `colonizer update` refuses to fix it ([`crate::update::blocker`]). `setup` closes the gap: it
//! fetches the installer for *this* version from the release, checks it against the release's
//! `SHA256SUMS`, and runs it. The installer does the rest — the tarball checksum, the Sigstore
//! attestation, the atomic swap — exactly as for `curl -fsSL https://colonizer.dev/install.sh | sh`.
//!
//! The download is `curl`, the same tool the installer requires and uses, so a `file://` release URL
//! (the installer test, a local mirror) works without a second code path.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{config::Settings, update, util};

/// The release tag for this binary's own version: the workflow tags each release `v<crate version>`
/// (`.github/workflows/release.yml` checks `$TAG = v$crate`), so a build's version names its release.
pub fn tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Where this release's assets live: `COLONIZER_RELEASE_URL` when it names a mirror (or a `file://`
/// directory), else the release's own GitHub download directory.
pub fn base_url(release_url: Option<&str>, tag: &str) -> String {
    match release_url {
        Some(url) => url.trim_end_matches('/').to_string(),
        None => format!("https://github.com/Colonizer-dev/harness/releases/download/{tag}"),
    }
}

/// The digest recorded for `name` in a `sha256sum`-style `SHA256SUMS`, if there is one.
///
/// The row is `<hex>  <name>`, or `<hex> *<name>` in binary mode — the same two spellings the
/// installer's own `awk` accepts. `None` when no row names the file.
pub fn sums_row(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let (Some(digest), Some(file)) = (fields.next(), fields.next()) else {
            continue;
        };
        if file == name || file.strip_prefix('*') == Some(name) {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

/// Why `install.sh` must not be run, if it must not: no row, a malformed one, or a digest that does
/// not match the bytes fetched. `None` means the bytes are exactly what the release published.
pub fn checksum_refusal(bytes: &[u8], sums: &str, name: &str) -> Option<String> {
    let expected = match sums_row(sums, name) {
        Some(digest) if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) => digest,
        Some(digest) => {
            return Some(format!(
                "the release's SHA256SUMS lists a malformed digest for {name}: {digest}"
            ));
        }
        None => return Some(format!("{name} is not listed in the release's SHA256SUMS")),
    };
    let got = util::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());
    (got != expected).then(|| format!("checksum mismatch for {name}: expected {expected}, got {got}"))
}

/// `colonizer setup` — install a release of this version over a cargo-built binary.
///
/// The installer runs with the terminal inherited, so its progress and its own refusals read as they
/// do when a person curls it. Its exit status is this command's: a failed install is a failed setup.
pub async fn command() -> Result<()> {
    let cfg = Settings::from_env()?;
    // Only a directory that really looks like an installed app short-circuits: a stray
    // `COLONIZER_HOME` naming anything else must not make `setup` a no-op.
    if let Some(assets) = &cfg.assets
        && looks_installed(assets)
    {
        println!("colonizer is already installed at {}", assets.display());
        return Ok(());
    }

    let tag = tag();
    let release_url = util::env_nonempty("COLONIZER_RELEASE_URL");
    let base = base_url(release_url.as_deref(), &tag);
    let scratch = Scratch::new()?;
    let installer = scratch.path("install.sh");
    let sums = scratch.path("SHA256SUMS");

    println!("fetching the installer for {tag} from {base}");
    fetch(&format!("{base}/install.sh"), &installer).await?;
    fetch(&format!("{base}/SHA256SUMS"), &sums).await?;

    let bytes = std::fs::read(&installer).with_context(|| format!("reading {}", installer.display()))?;
    let sums_text = std::fs::read_to_string(&sums).with_context(|| format!("reading {}", sums.display()))?;
    // Refuse before anything runs: the installer checks the tarball, but an installer is itself code,
    // and the release's checksums file is what says which code this tag published.
    if let Some(reason) = checksum_refusal(&bytes, &sums_text, "install.sh") {
        bail!("{reason}; nothing was installed");
    }

    // `sh -s` reads the script from stdin, and the bytes written are the very ones that were just
    // verified — never the file on disk, which another process could swap between the check and the
    // run. The installer references neither `$0` nor its own path, so nothing is lost by piping it.
    let mut run = Command::new("sh");
    run.arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // The tag travels only when the release URL was not spelled out: an override is whoever's mirror,
    // and what it calls its versions is its own to say.
    if release_url.is_none() {
        run.env("COLONIZER_VERSION", &tag);
    }
    if let Some(app) = update::app_link() {
        run.env("COLONIZER_APP", app);
    }
    let mut child = run.spawn().context("running the installer")?;
    {
        let mut stdin = child.stdin.take().context("opening the installer's standard input")?;
        // The installer may exit before reading all of the script — a platform refusal does — which
        // closes the pipe. Its own exit status is what decides the outcome, not this write.
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.shutdown().await;
    }
    let status = child.wait().await.context("running the installer")?;
    if !status.success() {
        bail!("the installer exited with {status}; nothing was installed");
    }

    let app = update::app_link().context("HOME is not set, so the app directory cannot be found")?;
    println!("colonizer is now installed at {}", app.display());
    println!("`colonizer` starts it; restart the mothership if one is already running");
    Ok(())
}

/// Whether `assets` is an installed app: it has the `bin/` and `vendor/` directories
/// `resolve_assets` looks for, so a `COLONIZER_HOME` naming something else is not mistaken for one.
pub fn looks_installed(assets: &Path) -> bool {
    assets.join("bin").is_dir() && assets.join("vendor").is_dir()
}

/// `curl -fsSL --retry 3 -o <to> <url>` — the installer's own fetch, so a `file://` base works too.
async fn fetch(url: &str, to: &Path) -> Result<()> {
    let output = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| "running curl (install it, or use the curl|sh installer instead)")?;
    if !output.status.success() {
        bail!("could not download {url}: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// A scratch directory that removes itself, so a refused or failed setup leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("colonizer-setup-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
        Ok(Self(dir))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_is_the_release_tag_for_this_version() {
        assert_eq!(tag(), concat!("v", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_base_is_this_versions_release_unless_a_url_is_given() {
        assert_eq!(
            base_url(None, "v1.2.3"),
            "https://github.com/Colonizer-dev/harness/releases/download/v1.2.3"
        );
        // A trailing slash on the override must not double up in `<base>/install.sh`.
        assert_eq!(base_url(Some("file:///tmp/rel/"), "v1.2.3"), "file:///tmp/rel");
        assert_eq!(
            base_url(Some("https://mirror.example/c"), "v1.2.3"),
            "https://mirror.example/c"
        );
    }

    #[test]
    fn only_a_directory_with_bin_and_vendor_counts_as_installed() {
        let dir = std::env::temp_dir().join(format!("colonizer-setup-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        assert!(!looks_installed(&dir), "bin alone is not an app");
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        assert!(looks_installed(&dir), "bin and vendor together are");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_row_lookup_reads_both_sha256sum_spellings() {
        assert_eq!(sums_row("abc  install.sh\n", "install.sh").as_deref(), Some("abc"));
        assert_eq!(sums_row("DEF\t*install.sh\n", "install.sh").as_deref(), Some("def"));
        assert_eq!(sums_row("abc  other.tar.gz\n", "install.sh"), None);
    }

    #[test]
    fn a_mismatch_or_a_missing_or_malformed_row_is_refused() {
        let bytes = b"#!/bin/sh\necho hi\n";
        let got = util::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref());

        assert_eq!(checksum_refusal(bytes, &format!("{got}  install.sh\n"), "install.sh"), None);

        let missing = checksum_refusal(bytes, &format!("{got}  colonizer-linux-x86_64.tar.gz\n"), "install.sh");
        assert!(missing.unwrap().contains("is not listed"), "a missing row is refused");

        let wrong = checksum_refusal(bytes, &format!("{}  install.sh\n", "0".repeat(64)), "install.sh");
        assert!(wrong.unwrap().contains("checksum mismatch"), "a wrong digest is refused");

        let malformed = checksum_refusal(bytes, "not-a-digest  install.sh\n", "install.sh");
        assert!(malformed.unwrap().contains("malformed"), "a malformed row is refused");
    }
}
