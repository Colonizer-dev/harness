//! The repository's own scripts, as the base commit has them (issue #1239): the list the guest's
//! exec policy reads so a colony can run the check scripts the repository ships (`python3
//! tools/sync-nav.py --check`) without `script-egress` refusing them.
//!
//! The boot writes it into the colony's `vm_dir`, which the guest sees read-only at `/colonizer`,
//! before the VM starts. It is read on the host from the colony's git admin dir at the merge-base of
//! the branch and `origin/<base>`: the guest cannot see that commit by itself (the agent can stage,
//! commit and move refs in its worktree, so `git ls-files` inside the VM would answer with whatever
//! the colony made of it), and it cannot write the list. Each line is the blob's git object id and
//! its path, so the policy exempts a script only while its bytes are still the committed ones: a
//! tracked script the colony edits, and any script it adds, is scanned like before.

use std::{path::Path, time::Duration};

/// The file in `vm_dir` (guest `/colonizer/tracked-scripts`).
pub(crate) const TRACKED_SCRIPTS_FILE: &str = "tracked-scripts";
/// The most entries written: the list is for check scripts, not a repository index.
const MAX_ENTRIES: usize = 20_000;
/// How long the host-side git reads may take before the list is left empty.
const GIT_LIMIT: Duration = Duration::from_secs(30);
/// The extensions an interpreter the exec policy knows runs (execpolicy.mjs `INTERPRETERS` and its
/// direct `./x.sh` form).
const SCRIPT_EXTENSIONS: [&str; 11] = ["sh", "bash", "zsh", "py", "js", "mjs", "cjs", "ts", "mts", "rb", "pl"];

/// The list's text from `git ls-tree -r -z` output: `<object id> <path>` per regular-file blob with
/// a script extension, newline-terminated. Symlinks, submodules and paths holding a newline are
/// left out, so a line is always one real file.
pub(crate) fn list_text(ls_tree: &[u8]) -> String {
    let mut out = String::new();
    let mut count = 0;
    for entry in ls_tree.split(|b| *b == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else { continue };
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split(' ');
        let (Some(mode), Some("blob"), Some(id)) = (meta.next(), meta.next(), meta.next()) else {
            continue;
        };
        let script = path.rsplit_once('.').is_some_and(|(_, ext)| SCRIPT_EXTENSIONS.contains(&ext));
        if !matches!(mode, "100644" | "100755") || !script || path.contains('\n') {
            continue;
        }
        if count == MAX_ENTRIES {
            break;
        }
        out.push_str(&format!("{id} {path}\n"));
        count += 1;
    }
    out
}

/// Writes the list for a colony's worktree. Best effort: when git cannot say, the file is written
/// empty, which exempts nothing and leaves the policy as strict as before. Returns the entry count.
pub(crate) async fn write(app: &crate::App, admin: &Path, base: &str, vm_dir: &Path) -> usize {
    let base_ref = format!("origin/{base}");
    let mut merge_base = app.git(admin);
    merge_base.args(["merge-base", "HEAD", &base_ref]);
    let commit = match crate::util::exec_within(GIT_LIMIT, &mut merge_base).await {
        Ok(sha) if !sha.trim().is_empty() => sha.trim().to_string(),
        _ => base_ref,
    };
    let mut ls = app.git(admin);
    ls.args(["ls-tree", "-r", "-z", "--full-tree", &commit]);
    let text = match crate::util::exec_within(GIT_LIMIT, &mut ls).await {
        Ok(listing) => list_text(listing.as_bytes()),
        Err(_) => String::new(),
    };
    let count = text.lines().count();
    let _ = std::fs::write(vm_dir.join(TRACKED_SCRIPTS_FILE), text);
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_regular_script_blobs_are_listed() {
        let listing = [
            "100644 blob 1111111111111111111111111111111111111111\ttools/sync-nav.py",
            "100755 blob 2222222222222222222222222222222222222222\ttools/deploy.sh",
            "100644 blob 3333333333333333333333333333333333333333\tREADME.md",
            "120000 blob 4444444444444444444444444444444444444444\ttools/link.sh",
            "160000 commit 5555555555555555555555555555555555555555\tvendor/sub.py",
            "100644 blob 6666666666666666666666666666666666666666\tscripts/x.mjs",
            "100644 blob 7777777777777777777777777777777777777777\tMakefile",
        ]
        .join("\0");
        assert_eq!(
            list_text(listing.as_bytes()),
            "1111111111111111111111111111111111111111 tools/sync-nav.py\n\
             2222222222222222222222222222222222222222 tools/deploy.sh\n\
             6666666666666666666666666666666666666666 scripts/x.mjs\n"
        );
        assert_eq!(list_text(b""), "");
    }

    /// The real thing: a committed script is listed at the merge-base, a commit on the branch after
    /// it is not, and a worktree with no base answers an empty list.
    #[tokio::test]
    async fn the_list_is_the_merge_base_tree() {
        let (app, root) = crate::sessions::tests::app_with_colony("abc", crate::sessions::SessionStatus::Running).await;
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("tools")).unwrap();
        let git = |args: &[&str]| crate::verify::tests::git(&repo, args);
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("tools/check.py"), "print('ok')\n").unwrap();
        git(&["add", "-A"]);
        crate::verify::tests::git_commit(&repo, "base");
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        std::fs::write(repo.join("tools/new.sh"), "curl https://x\n").unwrap();
        git(&["add", "-A"]);
        crate::verify::tests::git_commit(&repo, "colony");
        let vm_dir = root.join("vm");
        std::fs::create_dir_all(&vm_dir).unwrap();
        assert_eq!(write(&app, &repo.join(".git"), "main", &vm_dir).await, 1);
        let text = std::fs::read_to_string(vm_dir.join(TRACKED_SCRIPTS_FILE)).unwrap();
        let blob = git(&["rev-parse", "HEAD~1:tools/check.py"]);
        assert_eq!(text, format!("{blob} tools/check.py\n"));
        assert_eq!(write(&app, &repo.join(".git"), "absent", &vm_dir).await, 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
