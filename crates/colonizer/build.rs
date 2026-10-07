//! Stamps the binary with the version it was built from.
//!
//! Without this, "is there an update?" has no answer: the crate version alone
//! is bumped by hand and says nothing about which commit an install came from.
//!
//! Everything here degrades rather than fails. A build from a crates.io source
//! package or an unpacked tarball has no `.git`, and must still compile — it
//! simply reports the crate version and no commit.

use std::{path::Path, process::Command, time::SystemTime};

fn main() {
    // A stamped binary must not claim a commit it no longer has. HEAD covers
    // checkouts and switching branches, but a commit, pull or reset on the
    // current branch moves only the branch's ref file and leaves HEAD alone, so
    // that file is watched too; packed-refs covers a fetch that moves a tag and
    // a branch whose ref has been packed. Git is asked for the paths rather than
    // assuming `../../.git`, which is a file in a worktree. A source package has
    // no repository, so none of them exist and nothing is watched.
    let mut watched = vec![git_path("HEAD"), git_path("packed-refs")];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(git_path(&branch));
    }
    for path in watched.into_iter().flatten() {
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=COLONIZER_DESCRIBE");
    println!("cargo:rerun-if-env-changed=COLONIZER_COMMIT");

    // A release is not always built where git is: the Linux harness is built
    // inside rust:1-alpine, which has none. Unstamped, it would read as a
    // development build and refuse to update itself, so the release workflow
    // passes the tag and commit in. The commit is only taken together with the
    // tag, so the two always come from the same place.
    //
    // Otherwise git is asked: `--tags` so a release build reports `v0.1.4`;
    // `--always` so an untagged one still reports its commit; `--dirty` so a
    // build from a modified tree can never be mistaken for the release it is
    // descended from.
    let (describe, commit) = match from_env("COLONIZER_DESCRIBE") {
        Some(describe) => (describe, from_env("COLONIZER_COMMIT").unwrap_or_default()),
        None => (
            git(&["describe", "--tags", "--always", "--dirty"]).unwrap_or_default(),
            git(&["rev-parse", "HEAD"]).unwrap_or_default(),
        ),
    };

    // Honour SOURCE_DATE_EPOCH so a release can still be built reproducibly:
    // an unconditional clock reading would give every rebuild a different
    // binary, which is exactly what attestation needs not to happen.
    let built_at = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default()
        });

    println!("cargo:rustc-env=COLONIZER_DESCRIBE={describe}");
    println!("cargo:rustc-env=COLONIZER_COMMIT={commit}");
    println!("cargo:rustc-env=COLONIZER_BUILT_AT={built_at}");
}

/// Where git keeps `name` for this checkout, as an absolute path.
fn git_path(name: &str) -> Option<String> {
    git(&["rev-parse", "--path-format=absolute", "--git-path", name])
}

/// A variable from the build's environment, when it is set to something.
fn from_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}
