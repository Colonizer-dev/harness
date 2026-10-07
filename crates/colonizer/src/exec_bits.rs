//! Restores executable bits the colony's file tools silently drop, before the colony commit.
//!
//! Agent file tools rewrite files without their mode, so a script that was `100755` at the base
//! becomes `100644` in the worktree; left alone, the colony commit would bake the drop in and CI
//! would fail with exit 126. [`restore_dropped_exec_bits`] runs after `git add -A` and before the
//! commit: it diffs the staged tree against the colony's base, re-adds `+x` to the index entries
//! that lost it without being asked to, and `chmod`s the worktree files to match (an
//! `update-index --chmod` alone would be undone by the next `git add`).
//!
//! The decision itself ([`should_restore`]) is pure and unit-tested; [`WorktreeGit`] is the thin
//! production [`GitRun`] over the harness's hardened git builder.

use crate::{App, util::exec};
use anyhow::{Context, Result};
use std::{future::Future, os::unix::fs::PermissionsExt, path::Path, pin::Pin};

/// One staged entry whose git mode differs from the base. Only `100755` → `100644` entries are
/// ever restore candidates; everything else is carried along and ignored.
pub struct StagedModeChange {
    pub path: String,
    pub old_mode: String,
    pub new_mode: String,
    /// Whether the content blob is identical (a mode-only diff).
    pub blob_unchanged: bool,
}

/// Runs `git <args>` with the worktree pinned. One method (not `async-trait`) so tests can drive
/// the restore against a throwaway repo without the harness.
pub trait GitRun {
    fn run_git<'a>(&'a mut self, args: Vec<String>) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

/// Production [`GitRun`]: the harness's hardened git builder with `--git-dir` pinned to the
/// worktree's admin dir and `--work-tree` to the worktree, the same pinning `publish` uses.
pub struct WorktreeGit<'a> {
    app: &'a App,
    admin: &'a Path,
    work_tree: &'a Path,
}

impl<'a> WorktreeGit<'a> {
    pub fn new(app: &'a App, admin: &'a Path, work_tree: &'a Path) -> Self {
        Self { app, admin, work_tree }
    }
}

impl GitRun for WorktreeGit<'_> {
    fn run_git<'a>(&'a mut self, args: Vec<String>) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let mut cmd = self.app.git(self.admin);
            cmd.arg("--work-tree").arg(self.work_tree).args(&args);
            exec(&mut cmd).await
        })
    }
}

/// The commit the colony started from, as a sha for the staged diff.
///
/// `create_worktree` cuts the colony branch at `origin/<base>`, so in the common case HEAD still
/// is that commit — but the agent may commit inside its VM, and a previous publish attempt
/// certainly may have (a retry after a failed push finds the colony's own commit in HEAD).
/// Diffing the staged tree against HEAD would miss a bit the colony dropped in one of those
/// earlier commits. The merge-base of HEAD with `origin/<base>` is the fork point either way:
/// exactly the tree the colony started from, and blind to anything upstream did to `<base>`
/// since. A stacked colony's base may have no `origin/` ref until it is pushed (see
/// `count_behind`), so a missing `origin/<base>` falls back to the local `<base>`; when neither
/// resolves, HEAD is the last resort — the common-case answer anyway.
async fn colony_base(git: &mut impl GitRun, base: &str) -> String {
    for candidate in [format!("origin/{base}"), base.to_string()] {
        let args = vec!["merge-base".to_string(), "HEAD".to_string(), candidate];
        if let Ok(sha) = git.run_git(args).await {
            let sha = sha.trim().to_string();
            if !sha.is_empty() {
                return sha;
            }
        }
    }
    "HEAD".to_string()
}

/// Parses `git diff --cached --raw -z --no-renames <base>`: NUL-separated `:old new osha nsha ST`
/// headers alternating with literal (never quoted, thanks to `-z`) paths. `--no-renames` keeps
/// every record to one header plus one path, so a rename surfaces as a delete plus an add — and
/// the add side (old mode `000000`) is never a restore candidate.
pub fn parse_raw_diff_z(out: &[u8]) -> Vec<StagedModeChange> {
    let mut changes = Vec::new();
    let mut chunks = out.split(|b| *b == 0);
    while let (Some(header), Some(raw_path)) = (chunks.next(), chunks.next()) {
        if header.first() != Some(&b':') {
            continue; // The trailing NUL, or anything that is not a raw header.
        }
        let mut fields = header[1..].split(|b| *b == b' ');
        let (Some(old_mode), Some(new_mode), Some(old_sha), Some(new_sha), Some(_status)) =
            (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Ok(path) = std::str::from_utf8(raw_path) else { continue };
        if path.is_empty() {
            continue;
        }
        changes.push(StagedModeChange {
            path: path.to_string(),
            old_mode: String::from_utf8_lossy(old_mode).into_owned(),
            new_mode: String::from_utf8_lossy(new_mode).into_owned(),
            blob_unchanged: old_sha == new_sha,
        });
    }
    changes
}

/// Whether a `100755` → `100644` staged change gets its executable bit back.
///
/// The exemption is checked first, before any other heuristic, and it wins outright: a change is
/// left dropped whenever the colony's task text names that exact path together with the word
/// "executable" or "chmod" — i.e. the issue actually asked for the bit to go, so the colony is
/// carrying out instructions, not suffering tooling fallout. This applies to every path,
/// including scripts, and regardless of whether the file's content also changed; checking it
/// first is what makes that true — checked after the heuristics below, it would never fire for a
/// script, since those heuristics already return `true` before the exemption is ever reached. The
/// exemption is deliberately narrow (exact path plus keyword) so a passing mention of "chmod" in
/// unrelated prose never suppresses a restore.
///
/// Absent that exemption, restored when the worktree file still looks executable — a `#!`
/// shebang, anything under a `scripts/` directory at any depth, or a `.sh` name — or when the
/// diff is mode-only (the blob is unchanged, so nobody edited the file on purpose; the loss is
/// pure tooling fallout).
pub fn should_restore(
    path: &str,
    old_mode: &str,
    new_mode: &str,
    blob_unchanged: bool,
    has_shebang: bool,
    task_text: &str,
) -> bool {
    if old_mode != "100755" || new_mode != "100644" {
        return false;
    }
    if task_text.contains(path) && (task_text.contains("executable") || task_text.contains("chmod")) {
        return false;
    }
    if has_shebang || under_scripts(path) || path.ends_with(".sh") {
        return true;
    }
    // A mode-only diff outside those heuristics: nobody edited the file on purpose, so the loss is
    // pure tooling fallout.
    blob_unchanged
}

/// `scripts/foo.sh` and `a/scripts/b`, but not `scriptsfoo/x` or `descriptions/x`.
fn under_scripts(path: &str) -> bool {
    path.starts_with("scripts/") || path.contains("/scripts/")
}

fn file_starts_with_shebang(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 2];
    f.read_exact(&mut head).is_ok() && head == *b"#!"
}

/// Whether `disk` is a regular file that really lives inside `work_tree`. The colony controls the
/// worktree's contents, so a symlinked file or a symlinked directory on the way could point the
/// host's `chmod` at a file outside it; either reads as "not ours" and the path is skipped.
fn regular_file_inside(work_tree: &Path, disk: &Path) -> bool {
    let is_file = std::fs::symlink_metadata(disk).is_ok_and(|m| m.file_type().is_file());
    let (Ok(root), Ok(real)) = (work_tree.canonicalize(), disk.canonicalize()) else {
        return false;
    };
    is_file && real.starts_with(&root)
}

fn chmod_plus_x(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).with_context(|| format!("could not stat {}", path.display()))?;
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms).with_context(|| format!("could not chmod +x {}", path.display()))?;
    Ok(())
}

/// Diffs the staged tree against the colony base and restores every dropped executable bit
/// [`should_restore`] claims, both in the index (`update-index --chmod=+x`, so the commit about
/// to happen carries `100755`) and on the worktree file (so a later `git add` cannot undo the
/// index fix). Returns the restored paths, so the caller can log them. Naturally idempotent: a
/// second run sees no `100755` → `100644` diff and restores nothing.
pub async fn restore_dropped_exec_bits(
    work_tree: &Path,
    base: &str,
    task_text: &str,
    git: &mut impl GitRun,
) -> Result<Vec<String>> {
    let base_sha = colony_base(git, base).await;
    let out = git
        .run_git(vec![
            "diff".to_string(),
            "--cached".to_string(),
            "--raw".to_string(),
            "-z".to_string(),
            "--no-renames".to_string(),
            base_sha,
        ])
        .await?;
    let mut restored = Vec::new();
    for change in parse_raw_diff_z(out.as_bytes()) {
        if change.old_mode != "100755" || change.new_mode != "100644" {
            continue;
        }
        let disk = work_tree.join(&change.path);
        if !regular_file_inside(work_tree, &disk) {
            continue;
        }
        let has_shebang = file_starts_with_shebang(&disk);
        if !should_restore(
            &change.path,
            &change.old_mode,
            &change.new_mode,
            change.blob_unchanged,
            has_shebang,
            task_text,
        ) {
            continue;
        }
        git.run_git(vec![
            "update-index".to_string(),
            "--chmod=+x".to_string(),
            "--".to_string(),
            change.path.clone(),
        ])
        .await
        .with_context(|| format!("could not re-add the executable bit on {}", change.path))?;
        chmod_plus_x(&disk)?;
        restored.push(change.path);
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlinks_and_paths_escaping_the_worktree_are_never_chmodded() {
        let base = std::env::temp_dir().join(format!("exec-bits-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (work, outside) = (base.join("work"), base.join("outside"));
        std::fs::create_dir_all(work.join("scripts")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(work.join("scripts/ok.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(outside.join("victim.sh"), "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(outside.join("victim.sh"), work.join("scripts/link.sh")).unwrap();
        std::os::unix::fs::symlink(&outside, work.join("escape")).unwrap();

        assert!(
            regular_file_inside(&work, &work.join("scripts/ok.sh")),
            "a plain file inside is ours"
        );
        assert!(
            !regular_file_inside(&work, &work.join("scripts/link.sh")),
            "a symlinked file is skipped"
        );
        assert!(
            !regular_file_inside(&work, &work.join("escape/victim.sh")),
            "a symlinked directory is skipped"
        );
        assert!(
            !regular_file_inside(&work, &work.join("../outside/victim.sh")),
            "a `..` path is skipped"
        );
        assert!(
            !regular_file_inside(&work, &work.join("scripts/missing.sh")),
            "a missing file is skipped"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn new_added_and_deleted_entries_are_never_candidates() {
        assert!(!should_restore("scripts/new.sh", "000000", "100644", false, true, ""));
        assert!(!should_restore("scripts/gone.sh", "100755", "000000", false, false, ""));
        assert!(!should_restore("scripts/ok.sh", "100755", "100755", true, true, ""));
        assert!(!should_restore("other/mode.sh", "100644", "100755", true, false, ""));
    }

    #[test]
    fn executable_looking_files_are_restored_even_with_content_changes() {
        // A shebang outside scripts/, a non-shebang file under scripts/, and a .sh elsewhere.
        assert!(should_restore("tool/run", "100755", "100644", false, true, ""));
        assert!(should_restore("scripts/helper.mjs", "100755", "100644", false, false, ""));
        assert!(should_restore("a/scripts/deep/x", "100755", "100644", false, false, ""));
        assert!(should_restore("docs/build.sh", "100755", "100644", false, false, ""));
    }

    #[test]
    fn scripts_prefix_matching_is_not_fooled_by_lookalikes() {
        assert!(!under_scripts("scriptsfoo/x"));
        assert!(!under_scripts("descriptions/x"));
        assert!(under_scripts("scripts/x"));
        assert!(under_scripts("a/scripts/b"));
    }

    #[test]
    fn mode_only_diffs_are_restored() {
        assert!(should_restore("tools/convert.py", "100755", "100644", true, false, ""));
    }

    #[test]
    fn content_changed_non_script_files_are_left_alone() {
        assert!(!should_restore("README.md", "100755", "100644", false, false, ""));
        assert!(!should_restore("src/main.rs", "100755", "100644", false, false, ""));
    }

    #[test]
    fn an_explicit_task_exempts_a_mode_only_diff() {
        let task = "remove the executable bit from tools/convert.py (chmod -x it)";
        assert!(!should_restore("tools/convert.py", "100755", "100644", true, false, task));
        let task = "tools/convert.py should no longer be executable, fix the docs too";
        assert!(!should_restore("tools/convert.py", "100755", "100644", true, false, task));
        // Keyword without the path, or the path without a keyword: still restored.
        assert!(should_restore(
            "tools/convert.py",
            "100755",
            "100644",
            true,
            false,
            "chmod the release script"
        ));
        assert!(should_restore(
            "tools/convert.py",
            "100755",
            "100644",
            true,
            false,
            "rework tools/convert.py output"
        ));
        // The exemption never covers files that still look executable.
        assert!(should_restore("scripts/convert.sh", "100755", "100644", true, false, task));
    }

    /// Regression for the review of issue #455: the exemption must win even for a path that also
    /// matches the `scripts/`/`.sh`/shebang heuristics, and even when the file's content also
    /// changed — the old code checked those heuristics first, so a script's own exemption was
    /// never reached.
    #[test]
    fn an_explicit_task_exempts_a_script_even_with_a_content_change() {
        let task = "remove the executable bit from scripts/deploy.sh";
        assert!(!should_restore("scripts/deploy.sh", "100755", "100644", false, false, task));
        // A shebang alone does not override the exemption either.
        assert!(!should_restore("scripts/deploy.sh", "100755", "100644", false, true, task));
    }

    #[test]
    fn raw_z_parsing_handles_spaces_and_skips_non_drops() {
        let sha_a = "a".repeat(40);
        let sha_b = "b".repeat(40);
        let mut out = Vec::new();
        // A mode drop on a path with a space, content changed.
        out.extend_from_slice(format!(":100755 100644 {sha_a} {sha_b} M\x00").as_bytes());
        out.extend_from_slice("scripts/my tool.sh\x00".as_bytes());
        // A mode-only drop elsewhere.
        out.extend_from_slice(format!(":100755 100644 {sha_a} {sha_a} M\x00").as_bytes());
        out.extend_from_slice("tools/x\x00".as_bytes());
        // An added file and a content-only edit: not candidates, but must parse.
        out.extend_from_slice(format!(":000000 100644 0000000000000000000000000000000000000000 {sha_b} A\x00").as_bytes());
        out.extend_from_slice("new.txt\x00".as_bytes());
        out.extend_from_slice(format!(":100644 100644 {sha_a} {sha_b} M\x00").as_bytes());
        out.extend_from_slice("plain.txt\x00".as_bytes());
        let changes = parse_raw_diff_z(&out);
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0].path, "scripts/my tool.sh");
        assert!(!changes[0].blob_unchanged);
        assert!(changes[1].blob_unchanged);
        assert_eq!(changes[2].old_mode, "000000");
    }

    /// `git` against a throwaway repo, without the harness: `GIT_CONFIG_NOSYSTEM` plus a local
    /// identity keep it hermetic (no global config needed). The `env_remove` calls drop the
    /// `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` a colony sandbox exports for its own worktree,
    /// so the test's git never touches anything outside the scratch repo.
    struct CliGit {
        dir: std::path::PathBuf,
    }

    impl GitRun for CliGit {
        fn run_git<'a>(&'a mut self, args: Vec<String>) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            Box::pin(async move {
                let out = tokio::process::Command::new("git")
                    .current_dir(&self.dir)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env_remove("GIT_DIR")
                    .env_remove("GIT_WORK_TREE")
                    .env_remove("GIT_INDEX_FILE")
                    .args(&args)
                    .output()
                    .await?;
                if !out.status.success() {
                    anyhow::bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
                }
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            })
        }
    }

    fn scratch_repo(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "colonizer-exec-bits-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct RemoveOnDrop(std::path::PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_sync(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn index_mode(dir: &Path, path: &str) -> String {
        git_sync(dir, &["ls-files", "-s", "--", path])[..6].to_string()
    }

    fn disk_executable(dir: &Path, path: &str) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(dir.join(path)).unwrap().permissions().mode() & 0o111 == 0o111
    }

    /// End to end against a real repo: a `100755` script rewritten as `100644` and staged comes
    /// back as `100755` in the index and `+x` on disk, survives a later `git add -A`, and a
    /// second restore is a no-op.
    #[tokio::test]
    async fn dropped_bits_come_back_in_the_index_and_on_disk() {
        let dir = scratch_repo("restore");
        let _cleanup = RemoveOnDrop(dir.clone());
        git_sync(&dir, &["init", "-q", "-b", "main"]);
        git_sync(&dir, &["config", "user.email", "exec-bits@test"]);
        git_sync(&dir, &["config", "user.name", "exec-bits test"]);
        git_sync(&dir, &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/foo.sh"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(dir.join("scripts/foo.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        git_sync(&dir, &["add", "-A"]);
        git_sync(&dir, &["commit", "-q", "-m", "base"]);
        assert_eq!(index_mode(&dir, "scripts/foo.sh"), "100755");

        // What the agent file tools do: rewrite the file, losing the mode (a fresh write is 0644).
        std::fs::remove_file(dir.join("scripts/foo.sh")).unwrap();
        std::fs::write(dir.join("scripts/foo.sh"), "#!/bin/sh\necho changed\n").unwrap();
        assert!(!disk_executable(&dir, "scripts/foo.sh"));
        git_sync(&dir, &["add", "-A"]);
        assert_eq!(index_mode(&dir, "scripts/foo.sh"), "100644");

        // No `origin/main` here, so the base resolution falls back to the local `main`.
        let mut git = CliGit { dir: dir.clone() };
        let restored = restore_dropped_exec_bits(&dir, "main", "", &mut git).await.unwrap();
        assert_eq!(restored, vec!["scripts/foo.sh".to_string()]);
        assert_eq!(index_mode(&dir, "scripts/foo.sh"), "100755");
        assert!(disk_executable(&dir, "scripts/foo.sh"));

        // The update-index + chmod pair: a later `git add -A` must not undo the index fix.
        git_sync(&dir, &["add", "-A"]);
        assert_eq!(index_mode(&dir, "scripts/foo.sh"), "100755");

        let restored = restore_dropped_exec_bits(&dir, "main", "", &mut git).await.unwrap();
        assert!(restored.is_empty(), "the restore is idempotent");
    }
}
