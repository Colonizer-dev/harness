//! Build metadata: where this build came from, compiled in as `COLONIZER_BUILD_*` so the harness can
//! say what it is (`colonizer version`, the startup banner) and check for newer releases. Git is
//! asked politely and may decline: the crate is published to crates.io, where there is no `.git` to
//! ask, so a missing answer is an empty string, never a build failure.

use chrono::{DateTime, SecondsFormat, Utc};
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=COLONIZER_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=COLONIZER_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // The build is rerun when the commit or the staged tree changes, so a new checkout of the same
    // source does not silently keep the old metadata.
    if let Some(git) = git_dir() {
        for file in ["HEAD", "index"] {
            let path = git.join(file);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    // The release workflow sets the version itself, because its checkout is shallow and has no tags
    // for `git describe` to name; an ordinary build asks git, and a crates.io build gets nothing.
    let version = std::env::var("COLONIZER_BUILD_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| git(&["describe", "--tags", "--always", "--match", "v[0-9]*"]))
        .unwrap_or_default();
    println!("cargo:rustc-env=COLONIZER_BUILD_VERSION={version}");

    // The commit comes from git first: the env var is only there for builds where git cannot answer,
    // the opposite order of the version above.
    let commit = git(&["rev-parse", "HEAD"])
        .or_else(|| {
            std::env::var("COLONIZER_BUILD_COMMIT")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=COLONIZER_BUILD_COMMIT={commit}");

    // Anything uncommitted counts as dirty; not being able to ask is not evidence of dirt.
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    println!("cargo:rustc-env=COLONIZER_BUILD_DIRTY={}", u8::from(dirty));
    println!("cargo:rustc-env=COLONIZER_BUILD_TIME={}", build_time());
}

/// Runs git for its stdout, or nothing when git is missing, there is no repository, or it fails:
/// metadata must never fail a build that does not need it.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The `.git` directory governing this crate, if any. The build script runs in the crate directory,
/// and the repository root may sit any number of levels above it (`.git` is a file in a worktree).
fn git_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// RFC3339 UTC. `SOURCE_DATE_EPOCH` (seconds since the epoch) is honoured so release builds are
/// reproducible: the same source stamps the same time whatever the building machine's clock said.
fn build_time() -> String {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .and_then(|secs| DateTime::from_timestamp(secs, 0));
    epoch
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}
