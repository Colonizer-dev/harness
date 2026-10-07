use super::*;
use std::{
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
};

/// A Stripe-shaped value built at runtime, so no such literal sits in this file.
fn stripe() -> String {
    format!("{}{}", "sk_", "live_4eC39HqLyjWDarjtT1zdp7dc")
}

fn diff_with(added: &str) -> String {
    format!(
        "diff --git a/crates/m/tests/schema.rs b/crates/m/tests/schema.rs\n\
         index 1..2 100644\n--- a/crates/m/tests/schema.rs\n+++ b/crates/m/tests/schema.rs\n\
         @@ -40,3 +40,5 @@ fn t() {{\n let a = 1;\n-let old = 2;\n+let fine = 3;\n+{added}\n let b = 4;\n"
    )
}

#[test]
fn a_secret_on_an_added_line_is_reported_by_path_line_and_kind() {
    let spots = scan_diff(&diff_with(&format!("let key = \"{}\";", stripe())));
    assert_eq!(
        spots,
        vec![SecretSpot {
            path: "crates/m/tests/schema.rs".into(),
            line: 42,
            kind: "stripe_key".into()
        }],
        "line 40 context, the removed line does not count, 41 is the fine line, 42 the key"
    );
    let note = secrets_note(&spots, "main");
    assert!(note.contains("`crates/m/tests/schema.rs:42` contains a stripe key-shaped literal"));
    assert!(note.contains("at runtime"), "{note}");
    assert!(!note.contains(&stripe()), "the value never rides the note");
}

#[test]
fn context_and_removed_lines_and_lockfiles_are_not_scanned() {
    let key = stripe();
    let context = format!(
        "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n let k = \"{key}\";\n-let k2 = \"{key}\";\n+let ok = 1;\n"
    );
    assert!(scan_diff(&context).is_empty());
    let lock = format!("diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -0,0 +1 @@\n+{key}\n");
    assert!(scan_diff(&lock).is_empty());
    assert!(scan_diff("").is_empty());
}

#[test]
fn a_second_hunk_restarts_the_line_count() {
    let key = stripe();
    let diff =
        format!("diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,1 +1,2 @@\n a\n+b\n@@ -50,1 +51,2 @@\n c\n+\"{key}\"\n");
    let spots = scan_diff(&diff);
    assert_eq!(spots.len(), 1);
    assert_eq!((spots[0].path.as_str(), spots[0].line), ("x.rs", 52));
}

const GH013: &str = "remote: error: GH013: Repository rule violations found for refs/heads/colonizer/issue-692-a.\n\
remote: \n\
remote: - GITHUB PUSH PROTECTION\n\
remote:   ———————————————————————————————————————————\n\
remote:     Resolve the following violations before pushing again\n\
remote: \n\
remote:     - Push cannot contain secrets\n\
remote: \n\
remote:      \n\
remote:      (?) Learn how to resolve a blocked push\n\
remote:      \n\
remote:       —— Stripe API Key ————————————————————————————————\n\
remote:        locations:\n\
remote:          - commit: 0123abc\n\
remote:            path: crates/module-error-reporting/tests/schema.rs:46\n\
remote:          - commit: 0123abc\n\
remote:            path: crates/module-error-reporting/tests/schema.rs:189\n\
remote:      \n\
remote:       —— GitHub Personal Access Token ——————————————————\n\
remote:        locations:\n\
remote:          - commit: 0123abc\n\
remote:            path: docs/example.md:3\n\
 ! [remote rejected] colonizer/issue-692-a -> colonizer/issue-692-a (push declined due to repository rule violations)";

#[test]
fn a_gh013_rejection_gives_every_path_line_and_kind() {
    assert!(is_secret_rejection(GH013));
    let spots = parse_gh013(GH013);
    let got: Vec<_> = spots.iter().map(|s| (s.path.as_str(), s.line, s.kind.as_str())).collect();
    assert_eq!(
        got,
        vec![
            ("crates/module-error-reporting/tests/schema.rs", 46, "stripe_api_key"),
            ("crates/module-error-reporting/tests/schema.rs", 189, "stripe_api_key"),
            ("docs/example.md", 3, "github_personal_access_token"),
        ]
    );
    let note = secrets_note(&spots, "main");
    assert!(note.contains("`crates/module-error-reporting/tests/schema.rs:46` contains a stripe api key-shaped literal"));
}

#[test]
fn text_that_is_no_secret_rejection_parses_to_nothing() {
    for text in ["! [rejected] a -> a (non-fast-forward)", "fatal: could not read Username", ""] {
        assert!(!is_secret_rejection(text));
        assert!(parse_gh013(text).is_empty());
    }
    // A rejection that names no place is still a rejection: the colony is told to look.
    assert!(is_secret_rejection("remote: error: GH013: Repository rule violations found"));
    assert!(parse_gh013("remote: error: GH013: Repository rule violations found").is_empty());
    assert!(secrets_note(&[], "main").contains("did not say where"));
}

#[test]
fn only_a_rejected_push_that_lacks_remote_commits_is_a_non_fast_forward() {
    assert!(is_non_fast_forward(" ! [rejected]  a -> a (non-fast-forward)"));
    assert!(is_non_fast_forward(
        "hint: Updates were rejected ... (e.g., 'git pull ...') ! [rejected] a -> a (fetch first)"
    ));
    assert!(!is_non_fast_forward("! [rejected] a -> a (stale info)"));
    assert!(!is_non_fast_forward("fatal: unable to access: Could not resolve host"));
}

#[tokio::test]
async fn push_syncing_folds_in_the_remote_and_pushes_again_at_most_twice() {
    use std::cell::Cell;
    let (pushes, syncs) = (Cell::new(0u32), Cell::new(0u32));
    // Rejected once, lands after one sync.
    let n = push_syncing(
        || async {
            pushes.set(pushes.get() + 1);
            if pushes.get() == 1 {
                anyhow::bail!("! [rejected] a -> a (non-fast-forward)");
            }
            Ok(())
        },
        || async {
            syncs.set(syncs.get() + 1);
            Ok(())
        },
        MAX_SYNCS,
    )
    .await
    .unwrap();
    assert_eq!((n, pushes.get(), syncs.get()), (1, 2, 1));

    // Rejected forever: two syncs, three pushes, then the push's own error.
    let (pushes, syncs) = (Cell::new(0u32), Cell::new(0u32));
    let err = push_syncing(
        || async {
            pushes.set(pushes.get() + 1);
            anyhow::bail!("! [rejected] a -> a (fetch first)")
        },
        || async {
            syncs.set(syncs.get() + 1);
            Ok(())
        },
        MAX_SYNCS,
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("fetch first"));
    assert_eq!((pushes.get(), syncs.get()), (3, 2));

    // Any other failure is not retried.
    let pushes = Cell::new(0u32);
    let err = push_syncing(
        || async {
            pushes.set(pushes.get() + 1);
            anyhow::bail!("fatal: permission denied")
        },
        || async { Ok(()) },
        MAX_SYNCS,
    )
    .await
    .unwrap_err();
    assert_eq!(pushes.get(), 1);
    assert!(format!("{err:#}").contains("permission denied"));

    // A sync that conflicts ends the loop with its hold.
    let err = push_syncing(
        || async { anyhow::bail!("! [rejected] a -> a (non-fast-forward)") },
        || async {
            Err(PublishHold::Conflict {
                files: vec!["a.md".into()],
            }
            .into())
        },
        MAX_SYNCS,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<PublishHold>(),
        Some(PublishHold::Conflict { .. })
    ));
}

#[test]
fn the_conflict_note_names_the_branch_and_files() {
    let note = conflict_note("colonizer/issue-45-a", &["CHANGELOG.md".to_string(), "src/a.rs".to_string()]);
    assert!(
        note.contains("`colonizer/issue-45-a`") && note.contains("CHANGELOG.md, src/a.rs"),
        "{note}"
    );
    assert!(note.contains("rebase"));
}

// ── real git ──

struct Repo(PathBuf);

impl GitRun for Repo {
    fn run_git<'a>(&'a mut self, args: Vec<String>) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            let out = Command::new("git").arg("-C").arg(&self.0).args(&args).output()?;
            if !out.status.success() {
                anyhow::bail!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        })
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// An origin with `main`, plus a colony clone and a second clone that moves the colony's branch on
/// the "remote" first. Returns (origin, colony, other).
fn diverged(colony_file: &str, other_file: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-push-guard-{}", crate::util::short_id()));
    let origin = root.join("origin.git");
    let (colony, other) = (root.join("colony"), root.join("other"));
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
    git(&root, &["clone", "-q", origin.to_str().unwrap(), colony.to_str().unwrap()]);
    std::fs::write(colony.join("base.txt"), "base\n").unwrap();
    git(&colony, &["add", "-A"]);
    git(&colony, &["commit", "-q", "-m", "base"]);
    git(&colony, &["push", "-q", "origin", "HEAD:main"]);
    git(&colony, &["checkout", "-q", "-b", "work"]);
    std::fs::write(colony.join("first.txt"), "first\n").unwrap();
    git(&colony, &["add", "-A"]);
    git(&colony, &["commit", "-q", "-m", "first"]);
    git(&colony, &["push", "-q", "origin", "work"]);
    git(
        &root,
        &["clone", "-q", "-b", "work", origin.to_str().unwrap(), other.to_str().unwrap()],
    );
    std::fs::write(other.join(other_file), "from elsewhere\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "-q", "-m", "elsewhere"]);
    git(&other, &["push", "-q", "origin", "work"]);
    std::fs::write(colony.join(colony_file), "second\n").unwrap();
    git(&colony, &["add", "-A"]);
    git(&colony, &["commit", "-q", "-m", "second"]);
    (origin, colony, other)
}

#[tokio::test]
async fn a_moved_remote_branch_is_rebased_onto_and_then_pushes() {
    let (_origin, colony, _other) = diverged("second.txt", "elsewhere.txt");
    // The plain push is rejected: the remote has a commit the colony lacks.
    let rejected = Command::new("git")
        .arg("-C")
        .arg(&colony)
        .args(["push", "-q", "origin", "work"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(is_non_fast_forward(&String::from_utf8_lossy(&rejected.stderr)));

    git(
        &colony,
        &["fetch", "-q", "origin", "+refs/heads/work:refs/remotes/origin/work"],
    );
    let how = integrate(&mut Repo(colony.clone()), "refs/remotes/origin/work")
        .await
        .unwrap();
    assert_eq!(how, Integrated::Rebased);
    git(&colony, &["push", "-q", "origin", "work"]);
    let log = git(&colony, &["log", "--format=%s", "origin/work"]);
    assert_eq!(log.lines().collect::<Vec<_>>(), vec!["second", "elsewhere", "first", "base"]);
    let _ = std::fs::remove_dir_all(colony.parent().unwrap());
}

#[tokio::test]
async fn a_conflict_is_aborted_and_reported_as_a_hold_with_the_files() {
    // Both sides add the same file with different content: rebase and merge both conflict.
    let (_origin, colony, other) = diverged("clash.txt", "clash.txt");
    git(
        &colony,
        &["fetch", "-q", "origin", "+refs/heads/work:refs/remotes/origin/work"],
    );
    let before = git(&colony, &["rev-parse", "HEAD"]);
    let err = integrate(&mut Repo(colony.clone()), "refs/remotes/origin/work")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        PublishHold::Conflict {
            files: vec!["clash.txt".into()]
        }
    );
    assert_eq!(git(&colony, &["rev-parse", "HEAD"]), before, "the branch is left as it was");
    assert!(
        git(&colony, &["status", "--porcelain"]).trim().is_empty(),
        "nothing is left mid-operation"
    );
    assert!(!colony.join(".git/rebase-merge").exists() && !colony.join(".git/MERGE_HEAD").exists());
    let _ = std::fs::remove_dir_all(other.parent().unwrap());
}
