//! Overlap-aware queueing and automatic rebases (issue #453): two colonies developing against the
//! same repository at once can edit the same files, and a pull request left behind its base rots.
//! This module holds the pure decisions both features rest on — when a rebase is due, who a fresh
//! colony queues behind, which files overlap — plus the best-effort worktree reads behind them.
//! The watch loop (`publish.rs`) and admission (`sessions.rs`) own the side effects.

use crate::github::Mergeability;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

/// Whether a still-open pull request should be rebased onto its base right now: only `Behind` and
/// `Conflicted` ever qualify, and each main commit is tried at most once — the caller records the
/// sha it acted on and passes it back as `last_rebased_sha`, so a main that has not moved never
/// re-triggers. An empty or missing sha refuses, so nothing rebases onto an unknown base.
pub fn should_auto_rebase(current: Mergeability, main_sha: Option<&str>, last_rebased_sha: Option<&str>) -> bool {
    if !matches!(current, Mergeability::Behind | Mergeability::Conflicted) {
        return false;
    }
    let Some(sha) = main_sha else { return false };
    if sha.is_empty() {
        return false;
    }
    last_rebased_sha != Some(sha)
}

/// Records the main sha an attempt acted on, arming the once-per-sha guard above.
pub fn record_rebase_attempt(last_rebased: &mut Option<String>, main_sha: &str) {
    *last_rebased = Some(main_sha.to_string());
}

/// Whether an auto-rebase may run now: the time backoff from the last failure has expired, or the
/// main sha that failure was recorded against has since moved. A time-only backoff would make a
/// failure against sha A hold back an attempt against a fresh sha B for no reason — main moving on
/// is exactly the news the once-per-sha guard above exists to act on, so a moved sha is due
/// immediately rather than waiting out the clock. `current_base` empty means the caller could not
/// read the current sha (no news): the time backoff decides alone rather than treating that as a
/// moved sha and retrying every tick.
pub fn rebase_due(failed_base: Option<&str>, until: Option<Instant>, current_base: &str, now: Instant) -> bool {
    let Some(until) = until else { return true };
    if now >= until {
        return true;
    }
    if current_base.is_empty() {
        return false;
    }
    failed_base != Some(current_base)
}

/// What a colony whose branch just failed to auto-rebase is told: the files that conflicted (up
/// to ten, then a count) and the main commit it tripped on, with what to do about it.
pub fn conflict_wake_text(files: &[String], main_sha: &str, branch: &str) -> String {
    const SHOWN: usize = 10;
    let mut text = if files.is_empty() {
        format!("your branch `{branch}` could not be auto-rebased onto `{main_sha}`: it conflicts")
    } else {
        let listed: Vec<&str> = files.iter().take(SHOWN).map(String::as_str).collect();
        format!(
            "your branch `{branch}` could not be auto-rebased onto `{main_sha}`: it conflicts in {listed}",
            listed = listed.join(", ")
        )
    };
    if files.len() > SHOWN {
        text.push_str(&format!(" (and {} more)", files.len() - SHOWN));
    }
    text.push_str(&format!(". Resolve the conflicts, re-run the gates, and push `{branch}`."));
    text
}

/// What a *live* colony is told instead of the host rebasing for it (issue #453's security fix):
/// its branch fell behind or conflicts, and — since the colony's own branch and gates are not
/// something the host may run unsandboxed — it is asked to do the rebase itself, inside its own
/// microVM, where its own gates are the right sandbox for its own content.
pub fn rebase_request_text(branch: &str, base: &str, base_sha: &str) -> String {
    format!(
        "your branch `{branch}` is behind or conflicts with `{base}` (now at `{base_sha}`): please rebase \
         onto `origin/{base}`, re-run this repo's gates, and force-push `{branch}` yourself."
    )
}

/// The files two sets share, sorted: exact-path match, so a rename or a move never counts.
pub fn files_overlap(a: &[String], b: &[String]) -> Vec<String> {
    let in_b: BTreeSet<&str> = b.iter().map(String::as_str).collect();
    let shared: BTreeSet<&str> = a.iter().map(String::as_str).filter(|f| in_b.contains(f)).collect();
    shared.into_iter().map(str::to_string).collect()
}

/// Whether a fresh colony queues behind a live same-repo colony: this is not a real overlap check —
/// the newcomer's own file set is unknown until it boots, so there is nothing yet to compare `live_files`
/// against — it queues behind any live colony that has touched files at all, the conservative
/// reading. A true overlap check would need the newcomer's predicted files, which nothing in this
/// codebase produces for a colony that has not started.
pub fn should_queue_behind_live_colony(live_files: &[String]) -> bool {
    !live_files.is_empty()
}

/// One `git diff --name-only` in a worktree, best effort: `[]` on any error, timeout included.
pub(crate) async fn diff_names(worktree: &Path, args: &[&str]) -> Vec<String> {
    git_lines(worktree, args).await.unwrap_or_default()
}

/// Files with unmerged paths (`--diff-filter=U`), best effort: `[]` on any error.
pub(crate) async fn unmerged_files(worktree: &Path) -> Vec<String> {
    git_lines(worktree, &["diff", "--name-only", "--diff-filter=U"])
        .await
        .unwrap_or_default()
}

/// What a worktree is touching: committed-but-unpushed changes against `base_ref` plus uncommitted
/// ones (tracked and untracked alike). Best effort with a short timeout — `[]` on any error — so
/// callers treat an unreadable worktree as untouched, never as a reason to queue or rebase.
pub async fn touched_files(worktree: &Path, base_ref: &str) -> Vec<String> {
    let mut files = BTreeSet::new();
    let range = format!("{base_ref}...HEAD");
    files.extend(
        git_lines(worktree, &["diff", "--name-only", &range])
            .await
            .unwrap_or_default(),
    );
    for line in git_lines(worktree, &["status", "--porcelain"]).await.unwrap_or_default() {
        if let Some(path) = porcelain_path(&line) {
            files.insert(path.to_string());
        }
    }
    files.into_iter().collect()
}

/// Runs `git` in a worktree with a short deadline: trimmed non-empty output lines, or `None` when
/// git failed, timed out, or said nothing.
async fn git_lines(worktree: &Path, args: &[&str]) -> Option<Vec<String>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(worktree).args(crate::github::HOST_GIT_NO_EXEC).args(args);
    let out = crate::util::exec_within(Duration::from_secs(10), &mut cmd).await.ok()?;
    let lines: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    Some(lines)
}

/// The path a `--porcelain` v1 line names: two status bytes, a space, then the path — the new path
/// after ` -> ` for a rename. `None` for a line too short or empty to name anything.
fn porcelain_path(line: &str) -> Option<&str> {
    let path = line.get(3..)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(match path.split_once(" -> ") {
        Some((_, new)) => new.trim(),
        None => path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::mergeability_from;

    #[test]
    fn only_behind_and_conflicted_rebase_onto_a_new_main() {
        for current in [Mergeability::Behind, Mergeability::Conflicted] {
            assert!(
                should_auto_rebase(current, Some("abc"), None),
                "{current:?} with no guard rebases"
            );
            assert!(
                should_auto_rebase(current, Some("abc"), Some("old")),
                "{current:?} onto a new main rebases"
            );
            assert!(
                !should_auto_rebase(current, Some("abc"), Some("abc")),
                "the same main never re-triggers"
            );
            assert!(!should_auto_rebase(current, None, None), "no main sha refuses");
            assert!(!should_auto_rebase(current, Some(""), None), "an empty main sha refuses");
        }
        for current in [Mergeability::Clean, Mergeability::Unknown] {
            assert!(!should_auto_rebase(current, Some("abc"), None), "{current:?} never rebases");
        }
    }

    #[test]
    fn a_dirty_reading_rebases_as_conflicted_and_arms_the_guard() {
        let conflicted = mergeability_from(Some("MERGEABLE"), Some("DIRTY"));
        assert_eq!(conflicted, Mergeability::Conflicted, "DIRTY reads as conflicted");
        assert!(should_auto_rebase(conflicted, Some("abc"), None));
        let mut guard = None;
        record_rebase_attempt(&mut guard, "abc");
        assert_eq!(guard.as_deref(), Some("abc"));
        assert!(
            !should_auto_rebase(conflicted, Some("abc"), guard.as_deref()),
            "recorded once, not retried"
        );
        assert!(
            should_auto_rebase(conflicted, Some("def"), guard.as_deref()),
            "a moved main re-triggers"
        );
    }

    #[test]
    fn the_conflict_wake_names_files_and_the_main_sha() {
        let text = conflict_wake_text(&["src/a.rs".to_string(), "src/b.rs".to_string()], "deadbee", "colonizer/x");
        assert!(text.contains("src/a.rs") && text.contains("src/b.rs"), "{text}");
        assert!(text.contains("deadbee") && text.contains("colonizer/x"), "{text}");
        assert!(
            text.contains("Resolve") && text.contains("gates") && text.contains("push"),
            "{text}"
        );
        // No files: still names the sha and the branch, with no empty list.
        let bare = conflict_wake_text(&[], "deadbee", "colonizer/x");
        assert!(bare.contains("deadbee") && bare.contains("colonizer/x"), "{bare}");
    }

    #[test]
    fn the_conflict_wake_lists_ten_files_then_counts_the_rest() {
        let files: Vec<String> = (0..12).map(|i| format!("src/f{i}.rs")).collect();
        let text = conflict_wake_text(&files, "deadbee", "colonizer/x");
        assert!(text.contains("src/f9.rs") && !text.contains("src/f10.rs"), "{text}");
        assert!(text.contains("and 2 more"), "{text}");
    }

    #[test]
    fn the_rebase_request_names_the_branch_base_and_sha_and_asks_the_colony_to_do_it_itself() {
        let text = rebase_request_text("colonizer/x", "main", "deadbee");
        assert!(
            text.contains("colonizer/x") && text.contains("main") && text.contains("deadbee"),
            "{text}"
        );
        assert!(
            text.contains("rebase") && text.contains("gates") && text.contains("force-push"),
            "{text}"
        );
    }

    #[test]
    fn overlap_is_the_sorted_exact_path_intersection() {
        let a = ["src/b.rs".to_string(), "src/a.rs".to_string(), "only-a.rs".to_string()];
        let b = ["src/a.rs".to_string(), "src/b.rs".to_string(), "only-b.rs".to_string()];
        assert_eq!(files_overlap(&a, &b), vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
        assert!(files_overlap(&a, &[]).is_empty(), "nothing overlaps nothing");
        assert!(files_overlap(&[], &b).is_empty(), "nothing overlaps nothing");
        assert!(files_overlap(&["src/a.rs".to_string()], &["src/b.rs".to_string()]).is_empty());
        // Near misses are not matches: renames and case differences never count.
        assert!(files_overlap(&["src/A.rs".to_string()], &["src/a.rs".to_string()]).is_empty());
    }

    #[test]
    fn any_live_touched_files_queue_the_newcomer() {
        assert!(!should_queue_behind_live_colony(&[]), "a quiet repository admits at once");
        assert!(
            should_queue_behind_live_colony(&["src/a.rs".to_string()]),
            "any live files queue behind, whether or not they overlap the newcomer's own"
        );
    }

    #[test]
    fn porcelain_lines_yield_their_paths() {
        assert_eq!(porcelain_path(" M src/a.rs"), Some("src/a.rs"));
        assert_eq!(porcelain_path("?? new file.rs"), Some("new file.rs"));
        assert_eq!(porcelain_path("UU src/both.rs"), Some("src/both.rs"));
        assert_eq!(porcelain_path("R  old.rs -> new.rs"), Some("new.rs"));
        assert_eq!(porcelain_path(""), None);
        assert_eq!(porcelain_path("M"), None);
    }

    #[test]
    fn a_rebase_is_due_once_its_time_backoff_expires_or_main_moves_on() {
        let now = Instant::now();
        assert!(rebase_due(None, None, "abc", now), "no failure recorded: due");
        let until = now + Duration::from_secs(60);
        assert!(
            !rebase_due(Some("abc"), Some(until), "abc", now),
            "a live backoff against the same sha holds"
        );
        assert!(
            rebase_due(Some("abc"), Some(until), "def", now),
            "main moved on: due immediately, backoff or not"
        );
        assert!(
            rebase_due(Some("abc"), Some(until), "abc", until),
            "an expired backoff is due even against the same sha"
        );
        assert!(
            !rebase_due(Some("abc"), Some(until), "", now),
            "an unreadable current sha holds the time backoff rather than looking moved"
        );
    }

    /// Security fix for issue #453's review: the host must never execute a colony branch's own
    /// `ci.yml` `run:` steps — that content is attacker/agent-controlled, and running it via
    /// `sh -c` on the host is unsandboxed arbitrary code execution with the host's real
    /// credentials. A live colony re-runs its own gates inside its own microVM instead (see
    /// `publish::attempt_auto_rebase`), and a gone colony's conflict-free host rebase skips
    /// gates and relies on GitHub's own CI. This reads the module's own source so a
    /// reintroduction of either function, or of a host `sh -c` invocation, fails the build rather
    /// than relying on a reviewer noticing.
    #[test]
    fn the_host_never_shells_out_to_run_repo_controlled_ci_steps() {
        // Scan only the production code above this test module: the banned substrings below
        // necessarily appear, verbatim, in this very assertion, so scanning the whole file
        // (test module included) would always fail against itself.
        let source = include_str!("rebase.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for banned in ["Command::new(\"sh\")", ".arg(\"-c\")", "fn run_gates", "fn extract_ci_gates"] {
            assert!(!production.contains(banned), "{banned} must not reappear in rebase.rs");
        }
    }
}
