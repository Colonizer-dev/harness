//! Self-repair for a colony repository's bare git mirror (`App::bare_repo`). A crash or a full
//! disk mid-write can leave a zero-byte or half-written object in the mirror's store; from then on
//! every `git fetch` dies on it, and every colony of that repository spends its whole boot retry
//! budget failing to sync. This module is the ladder `github::sync_repo` climbs when a fetch fails
//! on what looks like corruption: first delete the corrupt loose objects (a fetch re-downloads
//! anything missing), then, if the store is damaged beyond that, re-clone from origin and swap the
//! fresh mirror in atomically, keeping the colony branches and the worktrees attached.

use crate::{
    App,
    sessions::{SessionLogger, SessionStatus},
    util::{exec, exec_capture},
};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long one `git fsck` over a mirror may take before the cleanup gives up on it.
const FSCK_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The fetch refspec `sync_repo` sets on every mirror, re-applied to a re-cloned one.
const ORIGIN_FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";

/// Whether a git error reads like local object-store corruption rather than a network, auth or
/// GitHub-side failure. Calibrated on what git actually prints: `git cat-file`/`git fetch` on a
/// zero-byte loose object say `error: object file <path> is empty` and `fatal: loose object <sha>
/// (stored in <path>) is corrupt`; garbage bytes say `… object corrupt or missing: <path>` and
/// `fatal: packed object <sha> (stored in <pack>) is corrupt`; a ref naming a lost object says
/// `fatal: bad object refs/heads/<branch>`; damaged packs say `… pack checksum mismatch`, `index
/// CRC mismatch …` and `fatal: pack has bad object at offset N`. None of that vocabulary appears
/// in ordinary fetch failures (`Could not resolve host`, `Authentication failed`, `remote: Invalid
/// username or token`), and a bare `did not send all necessary objects` is deliberately not enough
/// on its own — it can follow a rejected credential too.
pub(crate) fn is_corruption_error(text: &str) -> bool {
    let t = text.to_lowercase();
    (t.contains("object file") && t.contains("is empty"))
        || (t.contains("object") && t.contains("is corrupt"))
        || t.contains("object corrupt or missing")
        || t.contains("bad object")
        || t.contains("checksum mismatch")
        || t.contains("crc mismatch")
}

/// Deletes the corrupt loose objects from the mirror's store and returns how many went. Two
/// detectors run: a scan for zero-byte files under `objects/<xx>/<38 hex>` (the exact shape a
/// crash mid-write leaves), then up to two `git fsck --no-dangling` passes — deleting what the
/// first names and re-running to catch cascades, stopping when a pass names nothing new. Only
/// objects fsck names as corrupt or empty are deleted; a report of "missing" or "unreachable"
/// names no file and deletes nothing. fsck's nonzero exit code is fine — it is the detector here,
/// its findings are the value — so only a failure to run it at all propagates.
pub(crate) async fn clean_corrupt_loose_objects(app: &App, bare: &Path, log: &SessionLogger) -> Result<usize> {
    let objects = bare.join("objects");
    ensure!(objects.is_dir(), "the mirror {} has no objects directory", bare.display());
    let mut removed = remove_empty_loose_objects(&objects).await?;
    for _ in 0..2 {
        let named = fsck_named_loose_objects(app, bare).await?;
        let mut newly_removed = 0;
        for path in named {
            if !looks_like_loose_object(bare, &path) || !path.is_file() {
                continue;
            }
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("removing the corrupt loose object {}", path.display()))?;
            newly_removed += 1;
        }
        removed += newly_removed;
        if newly_removed == 0 {
            break;
        }
    }
    if removed > 0 {
        log.warn(format!(
            "removed {removed} corrupt loose object{} from the mirror {}; the next fetch re-downloads them",
            if removed == 1 { "" } else { "s" },
            bare.display()
        ))
        .await;
    }
    Ok(removed)
}

/// Deletes every zero-byte file shaped `objects/<2 hex>/<38 hex>`; a loose object is written in
/// one go, so an empty file is always a write that never finished, never a real object.
async fn remove_empty_loose_objects(objects: &Path) -> Result<usize> {
    let mut removed = 0;
    let mut fanout = tokio::fs::read_dir(objects).await?;
    while let Some(dir) = fanout.next_entry().await? {
        if !is_hex_name(&dir.file_name().to_string_lossy(), 2) {
            continue;
        }
        let mut entries = match tokio::fs::read_dir(dir.path()).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Some(entry) = entries.next_entry().await? {
            if !is_hex_name(&entry.file_name().to_string_lossy(), 38) {
                continue;
            }
            let empty = entry.metadata().await.map(|m| m.is_file() && m.len() == 0).unwrap_or(false);
            if empty && tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Runs `git fsck --no-dangling` and pulls the loose object paths its corruption reports name:
/// `error: object file <path> is empty`, `<sha>: object corrupt or missing: <path>` and
/// `fatal: loose object <sha> (stored in <path>) is corrupt`. Anything else fsck says — missing,
/// unreachable, dangling — names no file here and deletes nothing.
async fn fsck_named_loose_objects(app: &App, bare: &Path) -> Result<Vec<PathBuf>> {
    let mut cmd = app.git(bare);
    cmd.args(["fsck", "--no-dangling"]);
    let (stdout, stderr) = exec_capture(FSCK_LIMIT, &mut cmd)
        .await
        .with_context(|| format!("running `git fsck` on the mirror {}", bare.display()))?;
    Ok(format!("{stdout}\n{stderr}")
        .lines()
        .filter_map(|line| loose_object_path_from_line(line, bare))
        .collect())
}

/// The path a single fsck error line names, if it names a loose object file at all.
fn loose_object_path_from_line(line: &str, bare: &Path) -> Option<PathBuf> {
    let line = line.trim();
    let named = if let Some(rest) = line.strip_prefix("error: object file ") {
        rest.strip_suffix(" is empty")?
    } else if let Some(at) = line.find("object corrupt or missing: ") {
        &line[at + "object corrupt or missing: ".len()..]
    } else {
        let at = line.find("(stored in ")?;
        let rest = &line[at + "(stored in ".len()..];
        &rest[..rest.find(')')?]
    };
    let named = named.trim();
    // fsck prints paths relative to the current directory when it is run from inside the mirror
    // (`./objects/…`) and absolute otherwise; only paths under this mirror's store are ours.
    let path = if named.starts_with('/') {
        PathBuf::from(named)
    } else {
        let stripped = named.trim_start_matches("./");
        if !stripped.starts_with("objects/") {
            return None;
        }
        bare.join(stripped)
    };
    looks_like_loose_object(bare, &path).then_some(path)
}

/// Whether `path` is shaped like this mirror's loose objects: `…/objects/<2 hex>/<38 hex>`.
fn looks_like_loose_object(bare: &Path, path: &Path) -> bool {
    if !path.starts_with(bare) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(dir) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) else {
        return false;
    };
    is_hex_name(dir, 2) && is_hex_name(name, 38)
}

/// Whether `name` is exactly `len` hex digits — the shape of object ids split into fan-out parts.
fn is_hex_name(name: &str, len: usize) -> bool {
    name.len() == len && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Replaces a mirror too damaged for [clean_corrupt_loose_objects] to fix: clones origin afresh
/// into a sibling directory, restores what only the old mirror had, and swaps the two. The colony
/// branches under `refs/heads/` (a worktree's `HEAD` points at one) and the linked-worktree admin
/// dirs under `worktrees/` are the only things a fresh clone lacks; the objects they need that the
/// fresh clone did not get are fetched out of the dying mirror best-effort. The swap is two
/// renames on one filesystem — the old store is kept beside the mirror as `<name>.corrupt-<ts>`
/// for inspection, since colony-only commits that the recovery fetch could not salvage exist
/// nowhere else — and never runs `git worktree prune`, which would detach exactly the worktrees
/// being preserved. Relative files inside the admin dirs (`commondir` and friends) stay valid
/// because the new mirror lands at the same path. Returns the quarantine path the old store was
/// moved to, so the caller can name it to the operator.
pub(crate) async fn reclone_mirror(app: &App, repo: &str, bare: &Path, log: &SessionLogger) -> Result<PathBuf> {
    let mut cmd = app.git(bare);
    cmd.args(["config", "--get", "remote.origin.url"]);
    let url = exec(&mut cmd)
        .await
        .with_context(|| format!("reading the origin of the mirror {}", bare.display()))?
        .trim()
        .to_string();
    ensure!(
        !url.is_empty(),
        "the mirror {} has no remote.origin.url to re-clone from",
        bare.display()
    );
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let repair = unused_sibling(bare, "repair", stamp);
    log.info(format!(
        "re-cloning the mirror for {repo} from {url} into {}",
        repair.display()
    ))
    .await;
    exec(app.git_remote().args(["clone", "--bare", "--quiet"]).arg(&url).arg(&repair))
        .await
        .with_context(|| format!("re-cloning the mirror for {repo} from {url}"))?;
    exec(app.git(&repair).args(["config", "remote.origin.fetch", ORIGIN_FETCH_REFSPEC])).await?;

    // Colony branches: refs the fresh clone has no reason to carry. for-each-ref reads only refs,
    // not objects, so it works even on a corrupt store; a fresh clone's version of a shared
    // branch wins. A loose ref file is written directly — `git update-ref` would refuse while the
    // branch's commit is not yet in the new store.
    let old_heads = heads(app, bare).await?;
    let new_heads: Vec<String> = heads(app, &repair).await?.into_iter().map(|(name, _)| name).collect();
    for (refname, sha) in &old_heads {
        // A branch the fresh clone already has — main, say — keeps the fresh clone's version.
        if new_heads.iter().any(|name| name == refname) {
            continue;
        }
        let Some(suffix) = refname.strip_prefix("refs/heads/") else {
            continue;
        };
        let path = repair.join("refs/heads").join(suffix);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, format!("{sha}\n"))
            .await
            .with_context(|| format!("restoring the colony branch {refname}"))?;
    }

    // Objects reachable only from those branches, out of the dying mirror while it still exists.
    let mut recovery = app.git(&repair);
    recovery
        .args(["fetch", "--quiet"])
        .arg(bare)
        .arg("+refs/heads/*:refs/heads/*");
    if let Err(err) = exec(&mut recovery).await {
        log.warn(format!(
            "could not recover colony-only objects from the corrupt mirror {}: {err:#}",
            bare.display()
        ))
        .await;
    }

    // Linked worktrees: their admin dirs move with the swap, their absolute `.git` pointers do
    // not change, because the new mirror takes the old one's place.
    let worktrees = bare.join("worktrees");
    if worktrees.is_dir() {
        tokio::fs::create_dir_all(repair.join("worktrees")).await?;
        let mut entries = tokio::fs::read_dir(&worktrees).await?;
        while let Some(entry) = entries.next_entry().await? {
            tokio::fs::rename(entry.path(), repair.join("worktrees").join(entry.file_name()))
                .await
                .with_context(|| format!("moving the worktree admin dir {}", entry.path().display()))?;
        }
    }

    let quarantine = unused_sibling(bare, "corrupt", stamp);
    tokio::fs::rename(bare, &quarantine)
        .await
        .with_context(|| format!("moving the corrupt mirror {} aside", bare.display()))?;
    tokio::fs::rename(&repair, bare)
        .await
        .with_context(|| format!("moving the fresh mirror {} into place", repair.display()))?;
    log.warn(format!(
        "the mirror for {repo} was corrupt and has been re-cloned from {url}; the old store is kept at {} for inspection",
        quarantine.display()
    ))
    .await;
    Ok(quarantine)
}

/// `heads` lists the mirror's branch refs as `(refname, sha)`, broken refs skipped.
async fn heads(app: &App, git_dir: &Path) -> Result<Vec<(String, String)>> {
    let mut cmd = app.git(git_dir);
    cmd.args(["for-each-ref", "--format=%(refname) %(objectname)", "refs/heads"]);
    let out = exec(&mut cmd)
        .await
        .with_context(|| format!("listing the branches of {}", git_dir.display()))?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let (refname, sha) = line.trim().split_once(' ')?;
            (!sha.is_empty()).then(|| (refname.to_string(), sha.to_string()))
        })
        .collect())
}

/// A sibling path `<name>.<kind>-<stamp>` no other directory holds; a stamp only ever moves
/// forward, so a previous repair's leftovers are never overwritten, just numbered past.
fn unused_sibling(path: &Path, kind: &str, mut stamp: u64) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mirror.git".into());
    loop {
        let candidate = path.with_file_name(format!("{name}.{kind}-{stamp}"));
        if !candidate.exists() {
            return candidate;
        }
        stamp += 1;
    }
}

/// The one operator line per repository about its mirror (issue #1072), down the same channels and
/// ledger as the other host announcements. The `mirror:<repo>` topic is the one-item-per-repo
/// mechanism: the ledger cools a topic down for ten minutes, so three colonies of one repository
/// climbing the ladder in the same minute announce once, not thrice. A no-op while the notify
/// module is off.
async fn announce(app: &App, repo: &str, event: &str, line: &str) {
    crate::notify::announce_line(app, event, format!("mirror:{repo}"), line).await;
}

/// The operator line once the cleaner's rung has made the fetch healthy again (issue #1072), and
/// the re-queue of the colonies the corruption had already failed. `removed` is what the cleaner
/// deleted; `None` when it could not run and the plain retry was healthy anyway.
pub(crate) async fn repaired_by_cleaning(app: &App, repo: &str, removed: Option<usize>) {
    let how = match removed {
        Some(1) => "removing 1 bad object".to_string(),
        Some(n) => format!("removing {n} bad objects"),
        None => "cleaning the mirror's object store".to_string(),
    };
    announce(
        app,
        repo,
        "mirror_repaired",
        &format!("the git mirror of {repo} was corrupt; repaired by {how}"),
    )
    .await;
    resume_sync_failed_colonies(app, repo).await;
}

/// The operator line once only the re-clone rung would do (issue #1072); `old` is where the dying
/// store was quarantined, kept for inspection because colony-only commits may exist nowhere else.
pub(crate) async fn repaired_by_recloning(app: &App, repo: &str, old: &Path) {
    let kept = old
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| old.display().to_string());
    announce(
        app,
        repo,
        "mirror_repaired",
        &format!(
            "the git mirror of {repo} was corrupt; repaired by re-cloning the mirror (the old copy is kept alongside as {kept})"
        ),
    )
    .await;
    resume_sync_failed_colonies(app, repo).await;
}

/// The operator line when even a fresh clone left the fetch unhealthy (issue #1072): the ladder is
/// out of rungs, a person has to look, and every colony of the repository keeps failing until one
/// does.
pub(crate) async fn needs_attention(app: &App, repo: &str, err: &str) {
    let err = err.lines().next().unwrap_or_default();
    announce(
        app,
        repo,
        "mirror_needs_attention",
        &format!(
            "the git mirror of {repo} could not be repaired automatically and needs operator attention; its colonies keep failing to sync until it is fixed (last error: {err})"
        ),
    )
    .await;
}

/// Colonies that died because the mirror was corrupt failed before their worktree was cut, so a
/// repaired mirror is all they need: send them back to the queue (the same flip restart recovery
/// uses, `lifecycle.rs`), and the ordinary boot path takes it from there. A colony that failed for
/// any other reason — or that already holds a worktree — is not touched. Never `resume_if`: these
/// colonies have nothing to resume, they only re-enter the queue.
pub(crate) async fn resume_sync_failed_colonies(app: &App, repo: &str) {
    let sessions = app.sessions.read().await.clone();
    let doomed: Vec<String> = sessions
        .iter()
        .filter(|s| {
            s.repo == repo
                && s.status == SessionStatus::Failed
                && s.git_admin_dir.is_none()
                && s.error.as_deref().is_some_and(is_corruption_error)
        })
        .map(|s| s.id.clone())
        .collect();
    let mut resumed = 0;
    for id in &doomed {
        if app
            .update_session(id, |s| {
                s.status = SessionStatus::Queued;
                s.error = None;
                s.retry_at = None;
                s.failure_class = None;
                s.attention = None;
                s.parked = None;
            })
            .await
            .is_some()
        {
            resumed += 1;
            app.session_log(
                id,
                "info",
                "the repository mirror was corrupt and has been repaired; retrying the colony".into(),
            )
            .await;
        }
    }
    if resumed > 0 {
        tracing::info!(
            "mirror: re-queued {resumed} colon{} of {repo} that the corrupt mirror had failed",
            if resumed == 1 { "y" } else { "ies" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::sync_repo;
    use crate::sessions::{Session, SessionStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    /// A local origin repository, the mothership's mirror of `acme/repo` cloned from it, and the
    /// throwaway App — never GitHub, never the network.
    struct Mirror {
        root: PathBuf,
        app: crate::Shared,
        origin: PathBuf,
    }

    impl Drop for Mirror {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Mirror {
        async fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("colonizer-mirror-health-{name}-{}", crate::util::short_id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let root = std::fs::canonicalize(&root).unwrap();
            let app = crate::tests::test_app(&root);
            let origin = root.join("origin");
            git(&root, &["init", "-q", "-b", "main", "origin"]);
            git(&origin, &["config", "user.name", "Test"]);
            git(&origin, &["config", "user.email", "test@example.com"]);
            git(&origin, &["config", "commit.gpgsign", "false"]);
            commit(&origin, "base.txt", "base\n", "base");
            let bare = app.bare_repo("acme/repo");
            std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
            git(
                &root,
                &[
                    "clone",
                    "-q",
                    "--bare",
                    "--no-hardlinks",
                    origin.to_str().unwrap(),
                    bare.to_str().unwrap(),
                ],
            );
            git(
                &bare,
                &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"],
            );
            git(&bare, &["fetch", "-q", "origin"]);
            Self { root, app, origin }
        }

        fn bare(&self) -> PathBuf {
            self.app.bare_repo("acme/repo")
        }

        fn log(&self) -> SessionLogger {
            self.app.logger("c1")
        }

        /// A commit on origin the mirror has not fetched yet, so every fetch below has work to do.
        fn commit_on_origin(&self, file: &str, body: &str, message: &str) -> String {
            commit(&self.origin, file, body, message)
        }

        /// The loose object file of a ref in the mirror; panics if git packed it, so a fixture
        /// drift is loud rather than a silently wrong test.
        fn loose_object_of(&self, rev: &str) -> PathBuf {
            let sha = git(&self.bare(), &["rev-parse", rev]);
            let objects = self.bare().join("objects");
            let file = objects.join(&sha[..2]).join(&sha[2..]);
            assert!(
                file.is_file(),
                "the fixture assumed {rev} ({sha}) is a loose object at {}, but it is not — adjust the fixture",
                file.display()
            );
            file
        }

        /// A branch only the mirror has, as `create_worktree` leaves one behind.
        fn colony_branch(&self, branch: &str) -> String {
            let base = git(&self.bare(), &["rev-parse", "refs/remotes/origin/main"]);
            git(&self.bare(), &["branch", branch, &base]);
            base
        }
    }

    /// Runs git in `dir`, failing the test on a nonzero exit.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Like [`git`], but returns whatever git said and its exit status: the corrupt-store side of
    /// a test expects failure.
    fn git_maybe(dir: &Path, args: &[&str]) -> (bool, String) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn commit(dir: &Path, file: &str, body: &str, message: &str) -> String {
        std::fs::write(dir.join(file), body).unwrap();
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
        git(dir, &["rev-parse", "HEAD"])
    }

    // -- Detection.

    #[test]
    fn a_corruption_error_is_told_from_a_network_or_auth_failure() {
        // The real strings, captured from git on a damaged store.
        for real in [
            "error: object file /data/repos/acme/repo.git/objects/f3/698d1e218e39e6e21d6ec2c1ff4a67cea954b73 is empty",
            "fatal: loose object f3698d1e218e39e6e21d6ec2c1ff4a67cea954b73 (stored in /data/repos/acme/repo.git/objects/f3/698d1e218e39e6e21d6ec2c1ff4a67cea954b73) is corrupt",
            "error: f3698d1e218e39e6e21d6ec2c1ff4a67cea954b73: object corrupt or missing: /data/repos/acme/repo.git/objects/f3/698d1e218e39e6e21d6ec2c1ff4a67cea954b73",
            "fatal: packed object f3698d1e218e39e6e21d6ec2c1ff4a67cea954b73 (stored in /data/repos/acme/repo.git/objects/pack/pack-1e202b8a97eb9bb7a4b2a6739d09e47417e6f510.pack) is corrupt",
            "error: /data/repos/acme/repo.git/objects/pack/pack-1e202b8a97eb9bb7a4b2a6739d09e47417e6f510.pack pack checksum mismatch",
            "error: index CRC mismatch for object f3698d1e218e39e6e21d6ec2c1ff4a67cea954b73 from /data/repos/acme/repo.git/objects/pack/pack-1e.pack at offset 269",
            "fatal: pack has bad object at offset 140: inflate returned -3",
            "fatal: bad object refs/heads/colonizer/issue-7-c1",
            "`git --git-dir /data/repos/acme/repo.git fetch --quiet --prune origin` failed (exit code: 1): fatal: loose object f3698d1e218e39e6e21d6ec2c1ff4a67cea954b73 (stored in ./objects/f3/698d1e218e39e6e21d6ec2c1ff4a67cea954b73) is corrupt",
        ] {
            assert!(is_corruption_error(real), "should match: {real}");
        }
        for other in [
            "",
            "fatal: Could not read from remote repository.",
            "ssh: Could not resolve host github.com: Name or service not known",
            "fatal: unable to access 'https://github.com/acme/repo.git/': Connection timed out",
            "fatal: Authentication failed for 'https://github.com/acme/repo.git/'",
            "remote: Invalid username or token. Password authentication is not supported for Git operations.",
            "fatal: the remote end hung up unexpectedly",
            "error: RPC failed; curl 56 OpenSSL SSL_read: Connection was reset",
            "fatal: could not read Username for 'https://github.com': No such device or address",
        ] {
            assert!(!is_corruption_error(other), "should not match: {other}");
        }
    }

    // -- Repair rung 1: corrupt loose objects.

    #[tokio::test]
    async fn an_empty_object_file_is_removed_and_the_fetch_succeeds() {
        let m = Mirror::new("empty-object").await;
        let second = m.commit_on_origin("second.txt", "second\n", "second");
        let empty = m.loose_object_of("refs/heads/main");
        std::fs::write(&empty, b"").unwrap();

        // The fetch really does die on it, the way the incident's mirrors died.
        let bare = m.bare();
        let (ok, stderr) = git_maybe(&bare, &["fetch", "--quiet", "--prune", "origin"]);
        assert!(!ok, "the fetch should fail on the empty object");
        assert!(crate::mirror_health::is_corruption_error(&stderr), "git said: {stderr}");

        // The incident's exact ladder through sync_repo: clean rung 1, retry, and the mirror is
        // never re-cloned.
        sync_repo(&m.app, "acme/repo", &bare, &m.log())
            .await
            .expect("the fetch recovers after the clean");
        let siblings: Vec<String> = std::fs::read_dir(bare.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !siblings.iter().any(|n| n.contains(".repair-") || n.contains(".corrupt-")),
            "a cleanable store is never re-cloned: {siblings:?}"
        );
        // The cleaned object is back as a real file: the retried fetch re-downloaded it.
        assert!(
            empty.metadata().map(|m| m.len() > 0).unwrap_or(false),
            "the empty file was removed and re-fetched, not left empty"
        );
        // And the store is healthy again: the fetch landed the commit it could not read before.
        assert_eq!(git(&bare, &["rev-parse", "refs/remotes/origin/main"]), second);
        let (ok, fsck) = git_maybe(&bare, &["fsck", "--no-dangling"]);
        assert!(ok, "the store is valid after the clean: {fsck}");
    }

    #[tokio::test]
    async fn garbage_objects_are_removed_by_fsck_and_counted() {
        let m = Mirror::new("garbage").await;
        // Both paths are resolved before anything is corrupted: rev-parse of `^{tree}` walks the
        // commit, and the emptied commit below would break that walk.
        let emptied = m.loose_object_of("refs/heads/main");
        let tree = m.loose_object_of("refs/heads/main^{tree}");
        std::fs::write(&emptied, b"").unwrap();
        std::fs::write(&tree, b"garbage that is not empty").unwrap();

        let bare = m.bare();
        let log = m.log();
        let removed = clean_corrupt_loose_objects(&m.app, &bare, &log).await.unwrap();
        assert_eq!(removed, 2, "the empty file and the fsck-named garbage both go");
        assert!(!emptied.exists() && !tree.exists());
        git(&bare, &["fetch", "--quiet", "--prune", "origin"]);
    }

    #[tokio::test]
    async fn a_healthy_mirror_cleans_to_zero_and_is_left_alone() {
        let m = Mirror::new("healthy").await;
        let bare = m.bare();
        let before = git(&bare, &["for-each-ref", "refs/heads"]);
        let log = m.log();
        assert_eq!(clean_corrupt_loose_objects(&m.app, &bare, &log).await.unwrap(), 0);
        assert_eq!(git(&bare, &["for-each-ref", "refs/heads"]), before);
    }

    // -- Repair rung 2: re-clone and swap.

    #[tokio::test]
    async fn a_corruption_the_cleaner_cannot_fix_reclones_the_mirror_and_keeps_the_colony_branch() {
        let m = Mirror::new("reclone").await;
        let colony_sha = m.colony_branch("colonizer/issue-7-c1");
        m.commit_on_origin("fresh.txt", "fresh\n", "work on origin");

        // A branch ref naming an object the store lost: the fetch dies on `bad object`, fsck has
        // no file to name, so the ladder's first rung cannot fix it.
        let bare = m.bare();
        std::fs::write(bare.join("refs/heads/main"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        let (ok, stderr) = git_maybe(&bare, &["fetch", "--quiet", "--prune", "origin"]);
        assert!(
            !ok && crate::mirror_health::is_corruption_error(&stderr),
            "git said: {stderr}"
        );

        sync_repo(&m.app, "acme/repo", &bare, &m.log())
            .await
            .expect("the ladder ends in a working mirror");
        let entries: Vec<String> = std::fs::read_dir(bare.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let quarantined = entries
            .iter()
            .find(|n| n.starts_with("repo.git.corrupt-"))
            .expect("the old store is kept beside the mirror");
        assert!(
            bare.parent().unwrap().join(quarantined).join("refs/heads/main").is_file(),
            "the quarantined store keeps its refs"
        );
        assert!(bare.is_dir(), "the live mirror is back in place");
        // A fresh, valid store that fetched origin, with the colony's branch restored into it.
        let (ok, stderr) = git_maybe(&bare, &["fsck", "--no-dangling"]);
        assert!(ok, "the new store is valid: {stderr}");
        assert_eq!(git(&bare, &["rev-parse", "refs/heads/colonizer/issue-7-c1"]), colony_sha);
        assert_eq!(
            git(&bare, &["rev-parse", "refs/remotes/origin/main"]),
            git(&m.origin, &["rev-parse", "HEAD"]),
            "the fetch after the re-clone landed the new commit"
        );
    }

    #[tokio::test]
    async fn worktrees_survive_a_reclone() {
        let m = Mirror::new("worktrees").await;
        let branch = "colonizer/issue-9-c1";
        let wt = m.root.join("worktrees/acme/repo/issue-9-c1");
        let admin = crate::github::create_worktree(&m.app, &m.bare(), &wt, branch, "main")
            .await
            .expect("the worktree is created");
        assert!(admin.starts_with(m.bare()));

        m.commit_on_origin("fresh.txt", "fresh\n", "work on origin");
        let bare = m.bare();
        std::fs::write(bare.join("refs/heads/main"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();

        sync_repo(&m.app, "acme/repo", &bare, &m.log())
            .await
            .expect("the ladder re-clones around the damage");
        assert!(admin.is_dir(), "the admin dir moved with the swap to the same path");
        assert_eq!(
            git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch,
            "still on its branch"
        );
        let (ok, stderr) = git_maybe(&wt, &["status", "--short"]);
        assert!(ok, "the worktree still works: {stderr}");
        let listed = git(&bare, &["worktree", "list", "--porcelain"]);
        assert!(
            listed.contains(wt.to_str().unwrap()),
            "the re-cloned mirror still knows the worktree: {listed}"
        );
    }

    // -- After the repair: the colonies the corruption had already failed.

    /// Issue #1072: a colony that failed booting against the corrupt mirror never got a worktree,
    /// so a repaired mirror is all it was missing — the successful repair sends it back to the
    /// queue with nothing held against it, while a colony that failed for another reason, and one
    /// that already holds a worktree, stay failed.
    #[tokio::test]
    async fn a_repaired_mirror_requeues_the_colonies_its_corruption_failed() {
        let m = Mirror::new("resume").await;
        let seed = |id: &str| Session {
            id: id.into(),
            repo: "acme/repo".into(),
            org: "acme".into(),
            status: SessionStatus::Failed,
            ..Session::default()
        };
        // The incident's colony: failed by the corrupt mirror, nothing to resume.
        let mut corrupt = seed("c1");
        corrupt.error = Some("syncing the local clone of acme/repo failed after 3 attempts over 20m (budget spent); last error: `git fetch` failed (exit status: 128): fatal: object file .git/objects/f3/698d1e218e39e6e21d6ec2c1ff4a67cea954b73 is empty".into());
        corrupt.failure_class = Some(crate::retry::FailureClass::Permanent);
        corrupt.retry_at = Some(Utc::now());
        // Failed by something else entirely.
        let mut other_failure = seed("c2");
        other_failure.error = Some("GitHub refused the credentials for acme/repo (HTTP 403)".into());
        // Corrupt-mirror failure, but with a worktree: a resume, not a re-queue.
        let mut with_worktree = seed("c3");
        with_worktree.error = corrupt.error.clone();
        with_worktree.git_admin_dir = Some("repos/acme/repo.git/worktrees/issue-1072-c3".into());
        *m.app.sessions.write().await = vec![corrupt, other_failure, with_worktree];

        // Driven end to end through the ladder: an emptied loose object dies on the cleaner's
        // rung, whose success announces the repair and re-queues the waiting colonies.
        m.commit_on_origin("second.txt", "second\n", "second");
        std::fs::write(m.loose_object_of("refs/heads/main"), b"").unwrap();
        sync_repo(&m.app, "acme/repo", &m.bare(), &m.log())
            .await
            .expect("the ladder repairs the mirror");

        let c1 = m.app.session("c1").await.expect("the session survives");
        assert_eq!(c1.status, SessionStatus::Queued, "back in the queue");
        assert_eq!(c1.error, None, "the corruption verdict is dropped");
        assert_eq!(c1.retry_at, None);
        assert_eq!(c1.failure_class, None, "no failure is held against it");
        let c2 = m.app.session("c2").await.expect("the session survives");
        assert_eq!(
            c2.status,
            SessionStatus::Failed,
            "a colony that failed for another reason is not re-queued"
        );
        let c3 = m.app.session("c3").await.expect("the session survives");
        assert_eq!(
            c3.status,
            SessionStatus::Failed,
            "a colony that already has a worktree is not re-queued"
        );
        assert!(c3.error.is_some(), "its verdict is untouched");
    }
}
