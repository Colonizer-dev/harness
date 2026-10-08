//! Supersession (issue #673): when a colony's pull request merges, other open colonies of the same
//! repository whose work it covers are marked `superseded` — kept out of the queue and the resume
//! route until the operator keeps them. Refusing a second live colony for one supply-chain target
//! at launch is the shared duplicates service's job (duplicates.rs, issue #832).

use crate::sessions::{Session, SessionStatus};
use crate::util::exec_within;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

/// How much of either colony's changed files must sit in the other's set before file overlap alone
/// marks a colony superseded: 80% of one side's files covered by the other's, and at least
/// [`MIN_SHARED_FILES`] of them — two dependency bumps that each touch the manifest and its
/// lockfile are two pieces of work, not one.
pub(crate) const FILE_OVERLAP: f64 = 0.8;

/// The fewest shared files a file overlap stands on: under that, a couple of shared paths is a
/// coincidence, not coverage.
const MIN_SHARED_FILES: usize = 3;

/// Lockfiles and vendored dependency sums, matched by file name: churn every dependency bump
/// shares, so they say nothing about what a colony actually worked on. Ignored when judging a file
/// overlap — a merge that touched a package's source covers a colony that touched the same source,
/// not one that bumped a version somewhere else in the same repository.
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "go.sum",
    "poetry.lock",
    "uv.lock",
    "Gemfile.lock",
    "composer.lock",
];

fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.contains(&name)
}

/// Same budget a GitHub claim or release gets: one `gh` call, best effort.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);

/// The supply-chain target a colony was launched against (issue #673): a package and the advisory
/// it was launched to fix. Two targets are the same whatever case they were written in — `PKG` and
/// `pkg` are one package, `Ghsa-…` and `ghsa-…` one advisory — and both sides are trimmed before
/// the compare, so a stray space cannot split one target in two.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupplyChainTarget {
    pub package: String,
    pub advisory: String,
}

impl SupplyChainTarget {
    /// A normalized target: trimmed and lowercased, the form the session record keeps.
    pub fn new(package: &str, advisory: &str) -> Self {
        Self {
            package: package.trim().to_lowercase(),
            advisory: advisory.trim().to_lowercase(),
        }
    }
}

impl PartialEq for SupplyChainTarget {
    fn eq(&self, other: &Self) -> bool {
        // Written by hand rather than derived, so a target that reached the record without going
        // through [`SupplyChainTarget::new`] still compares the way it is documented to.
        self.package.trim().eq_ignore_ascii_case(other.package.trim())
            && self.advisory.trim().eq_ignore_ascii_case(other.advisory.trim())
    }
}

impl Eq for SupplyChainTarget {}

/// Why a merge superseded a colony, most specific first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapReason {
    /// Both colonies carry the same supply-chain target (package + advisory).
    SupplyChain,
    /// Both colonies are on the same issue.
    Issue,
    /// Both colonies changed files, and at least [`FILE_OVERLAP`] of either side's files are in the
    /// other's set.
    Files,
}

impl OverlapReason {
    /// The phrase colony logs and refusals name the reason by.
    fn as_str(self) -> &'static str {
        match self {
            Self::SupplyChain => "the same supply-chain target",
            Self::Issue => "the same issue",
            Self::Files => "overlapping files",
        }
    }
}

impl std::fmt::Display for OverlapReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one colony's merge covers in another's: the supply-chain target first, then the issue, then
/// the files. `None` when the colonies are unrelated — one and the same, or different repositories.
pub fn overlap(merged: &Session, other: &Session) -> Option<OverlapReason> {
    if merged.id == other.id || merged.org != other.org || merged.repo != other.repo {
        return None;
    }
    // The supply-chain rule is the launch refusal's (duplicates.rs, issue #832): the same package and
    // advisory, a side naming no advisory covering every advisory of its package.
    if crate::duplicates::supply_chain_overlap(merged, other) {
        return Some(OverlapReason::SupplyChain);
    }
    if merged.issue.is_some() && merged.issue == other.issue {
        return Some(OverlapReason::Issue);
    }
    files_overlap(&merged.changed_paths, &other.changed_paths).then_some(OverlapReason::Files)
}

/// Whether the two colonies changed substantially the same files: at least [`MIN_SHARED_FILES`]
/// shared paths, making up at least [`FILE_OVERLAP`] of the smaller side. Lockfiles are ignored, so
/// two unrelated dependency bumps sharing a manifest and its lockfile do not read as one piece of
/// work; a side with no real file left cannot overlap, because a colony whose pull request never
/// listed one cannot be said to cover a colony that did.
fn files_overlap(a: &[String], b: &[String]) -> bool {
    let other: std::collections::HashSet<&String> = b.iter().filter(|p| !is_lockfile(p)).collect();
    let shared = a.iter().filter(|p| !is_lockfile(p) && other.contains(*p)).count();
    let smaller = a.iter().filter(|p| !is_lockfile(p)).count().min(other.len());
    shared >= MIN_SHARED_FILES && shared as f64 / smaller as f64 >= FILE_OVERLAP
}

/// Why a colony stands superseded, kept on the record so the marker survives restarts and the UI can
/// say what covered this work without loading the colony that did it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Supersession {
    /// The colony whose merged pull request covered this one's work.
    pub by: String,
    /// The merged pull request's URL.
    pub pr_url: String,
    /// The merged pull request's number, parsed from the URL when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    /// The merged colony's title.
    pub title: String,
    pub reason: OverlapReason,
    pub at: DateTime<Utc>,
    /// The operator read the supersession and wants this colony to run anyway: the queue and the
    /// resume route start it once this is true.
    pub kept: bool,
}

impl Supersession {
    /// Marks the record kept unless it already was, saying whether that changed — the keep route's
    /// authority on "there was something to keep".
    pub fn mark_kept(&mut self) -> bool {
        !std::mem::replace(&mut self.kept, true)
    }
}

/// Whether a merge can still supersede this colony: anything not already finished. `pr_opened`
/// counts — its pull request is exactly what may need closing — while merged, closed, no-changes,
/// stopped and failed colonies are done, with nothing left to mark. A `claim_wait` waiter is
/// excluded too: it never started, and the queue already retires the waiters of a holder whose
/// pull request merged — a hold here would only demand a pointless Keep.
fn supersedable(s: &Session) -> bool {
    !s.claim_wait
        && !matches!(
            s.status,
            SessionStatus::Merged
                | SessionStatus::Closed
                | SessionStatus::NoChanges
                | SessionStatus::Stopped
                | SessionStatus::Failed
        )
}

/// Whether this colony is kept out of the queue and the resume route: superseded, and not kept.
pub fn blocks_start(s: &Session) -> bool {
    s.superseded.as_ref().is_some_and(|superseded| !superseded.kept)
}

/// The 409 message a blocked resume gets, saying what covered the work and the way out.
pub fn blocked_message(superseded: &Supersession) -> String {
    format!(
        "superseded by {}: {} — this colony's work was covered; Keep it first to start it anyway",
        superseded.pr_url, superseded.reason
    )
}

/// The merged colony's title, as the supersession record keeps it.
fn colony_title(s: &Session) -> String {
    let title = s.issue_title.trim();
    if title.is_empty() {
        s.summary.clone().unwrap_or_default()
    } else {
        title.to_string()
    }
}

/// The pull request number at the end of a GitHub pull request URL (`…/pull/123`), suffix and all
/// (`…/pull/123/files` is the same pull request).
fn pr_number(url: &str) -> Option<u64> {
    url.rsplit_once("/pull/")?.1.split('/').next()?.parse().ok()
}

/// Marks every colony of the same repository whose work the merge covers, judging the file list the
/// merged colony carries right now. Returns the colonies it just marked, so their follow-ups can
/// run off the caller's path; a colony already marked comes back untouched, so a later pass over
/// the same merge is a no-op for it. The PR watcher calls this at the merge edge itself — no remote
/// read first, or a queue tick in between could start a colony this merge covers.
pub async fn mark_superseded(app: &crate::Shared, merged_id: &str) -> Vec<Session> {
    let Some(merged) = app.session(merged_id).await else {
        return Vec::new();
    };
    let targets: Vec<(Session, OverlapReason)> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|other| other.id != merged.id)
        // A stacked child is not covered by this merge: it was held waiting for it, and this is
        // the very merge that releases it.
        .filter(|other| other.parent.as_deref() != Some(merged.id.as_str()))
        .filter_map(|other| overlap(&merged, other).map(|reason| (other.clone(), reason)))
        .collect();
    let merged_pr_url = merged.pr_url.clone().unwrap_or_default();
    let supersession_for = |reason: OverlapReason| Supersession {
        by: merged.id.clone(),
        pr_url: merged_pr_url.clone(),
        pr: pr_number(&merged_pr_url),
        title: colony_title(&merged),
        reason,
        at: Utc::now(),
        kept: false,
    };
    let mut marked = Vec::new();
    for (other, reason) in targets {
        let supersession = supersession_for(reason);
        let Some((session, applied)) = app
            .update_session(&other.id, |x| {
                // Conditional on purpose: a finish between the snapshot and this write decides
                // against. An existing record gives way only to a different merge, and only a kept
                // one — an unkept hold is never traded away, and the same merge never marks twice.
                let apply = supersedable(x)
                    && x.superseded
                        .as_ref()
                        .is_none_or(|existing| existing.kept && existing.by != merged.id);
                if apply {
                    x.superseded = Some(supersession.clone());
                }
                apply
            })
            .await
        else {
            continue;
        };
        if applied {
            marked.push(session);
        }
    }
    marked
}

/// The follow-ups for the colonies a pass just marked, best effort and off the watcher's tick:
/// close a published colony's pull request when the org's `close_superseded_prs` list allows it
/// and external writes are not blocked, tell a live colony to rebase onto main or finish with
/// no changes, and note every marking in the colony's log. No lock is ever held across the `gh`
/// call, and a failure only prints — the marker stands either way.
pub async fn side_effects(app: &crate::Shared, marked: Vec<Session>) {
    for session in marked {
        // The record the pass just wrote carries the merge's coordinates.
        let Some(superseded) = session.superseded.as_ref() else {
            continue;
        };
        let note = format!(
            "superseded by colony {} ({}): {}",
            superseded.by, superseded.pr_url, superseded.reason
        );
        if session.status == SessionStatus::PrOpened {
            let org = app.org_settings(&session.org);
            let closed = match session.pr_url.as_deref() {
                Some(url)
                    if crate::orgs::closes_superseded_prs(&org, &session.repo)
                        && !crate::authority::external_writes_blocked() =>
                {
                    close_superseded_pr(app, &session.id, url, &superseded.pr_url, superseded.reason).await
                }
                _ => false,
            };
            let outcome = if closed {
                "its pull request was closed with a note"
            } else {
                "its pull request is left open"
            };
            app.session_log(&session.id, "info", format!("{note}; {outcome}")).await;
        } else if session.suspended.is_some() || matches!(session.status, SessionStatus::Queued | SessionStatus::Blocked) {
            app.session_log(&session.id, "info", format!("{note}; it will not start until it is kept"))
                .await;
        } else if session.status.is_live() && session.status != SessionStatus::Starting {
            // A suspended colony is live too (waiting_for_answer), but its runner is gone: handled
            // above. The rest get the word, exactly like a watchdog nudge.
            if let Some(rt) = app.runtimes.lock().await.get(&session.id).cloned() {
                rt.send_command(json!({
                    "type": "user_message",
                    "id": format!("supersede-{}", crate::util::short_id()),
                    "text": format!(
                        "The changes from {} (\"{}\") were just merged to main, and they overlap \
                         this colony's work — {}. Fetch and rebase onto main and continue, or \
                         finish with no_changes if main already covers the task.",
                        superseded.pr_url, superseded.title, superseded.reason
                    ),
                }));
            }
            app.session_log(
                &session.id,
                "info",
                format!("{note}; the colony was told to rebase onto main or finish"),
            )
            .await;
        }
    }
}

/// The second marking pass, once the merged pull request's final file list has been read back: a
/// file overlap only shows itself there. Idempotent — the first pass's markings are skipped — so
/// it marks and follows up on exactly what the fuller list newly reveals.
pub async fn after_files(app: &crate::Shared, merged_id: &str) {
    side_effects(app, mark_superseded(app, merged_id).await).await;
}

/// Closes a superseded colony's pull request with the note, best effort, saying whether it closed: a
/// failure is logged and otherwise dropped — the supersession marker stands either way, and the
/// operator can close by hand.
async fn close_superseded_pr(app: &crate::Shared, id: &str, pr_url: &str, merged_pr_url: &str, reason: OverlapReason) -> bool {
    let comment = format!("Superseded by {merged_pr_url}: {reason}. Closed by Colonizer (the close_superseded_prs org setting).");
    let result = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["pr", "close", pr_url, "--comment", comment.as_str()]),
    )
    .await;
    if let Err(e) = &result {
        eprintln!("supersede: could not close the superseded pull request {pr_url} of colony {id}: {e:#}");
        app.session_log(
            id,
            "warn",
            format!("could not close the superseded pull request ({e:#}); close it by hand, or leave it open"),
        )
        .await;
    }
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::*;

    fn targeted(id: &str, repo: &str, status: SessionStatus) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.repo = repo.into();
        s
    }

    /// The launch-time answer for one target, through the shared duplicates service (issue #832).
    fn launch_refusal(sessions: &[Session], repo: &str, target: &SupplyChainTarget, allow_duplicate: bool) -> Option<String> {
        let work = crate::duplicates::Work {
            repo: repo.into(),
            supply_chain: vec![target.clone()],
            ..Default::default()
        };
        match crate::duplicates::check(sessions, &[], &work, allow_duplicate, false) {
            crate::duplicates::Verdict::Refuse(refusal) => Some(refusal.message),
            _ => None,
        }
    }

    fn with_target(s: &mut Session, package: &str, advisory: &str) {
        s.supply_chain = Some(SupplyChainTarget::new(package, advisory));
    }

    /// A supersession record standing in for one an earlier (or the same) merge wrote.
    fn a_record(by: &str, kept: bool) -> Supersession {
        Supersession {
            by: by.into(),
            pr_url: String::new(),
            pr: None,
            title: String::new(),
            reason: OverlapReason::Files,
            at: Utc::now(),
            kept,
        }
    }

    #[test]
    fn a_supply_chain_target_compares_case_insensitively_and_normalized() {
        assert_eq!(
            SupplyChainTarget::new(" Lodash ", "GHSA-xxxx-yyyy"),
            SupplyChainTarget::new("lodash", "ghsa-xxxx-yyyy")
        );
        assert_eq!(
            SupplyChainTarget::new("lodash", "ghsa-1"),
            SupplyChainTarget {
                package: "Lodash".into(),
                advisory: " GHSA-1 ".into()
            },
            "the compare trims and ignores case even without the constructor"
        );
        assert_ne!(
            SupplyChainTarget::new("lodash", "ghsa-1"),
            SupplyChainTarget::new("lodash", "ghsa-2")
        );
        assert_ne!(
            SupplyChainTarget::new("lodash", "ghsa-1"),
            SupplyChainTarget::new("left-pad", "ghsa-1")
        );
    }

    #[test]
    fn the_overlap_needs_one_repository_and_two_colonies() {
        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        merged.issue = Some(7);
        let mut other = targeted("other", "acme/repo", SessionStatus::Running);
        other.issue = Some(7);
        assert_eq!(overlap(&merged, &other), Some(OverlapReason::Issue));
        other.repo = "acme/other".into();
        assert_eq!(overlap(&merged, &other), None, "another repository is another world");
        other.repo = "acme/repo".into();
        other.org = "bea".into();
        assert_eq!(overlap(&merged, &other), None, "another org is another world");
        other.org = "acme".into();
        assert_eq!(overlap(&merged, &merged), None, "a colony does not supersede itself");
    }

    #[test]
    fn the_overlap_reads_target_first_then_issue_then_files() {
        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        merged.issue = Some(7);
        with_target(&mut merged, "lodash", "ghsa-1");
        merged.changed_paths = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let mut other = targeted("other", "acme/repo", SessionStatus::Running);
        other.issue = Some(7);
        with_target(&mut other, "Lodash", "GHSA-1");
        other.changed_paths = merged.changed_paths.clone();
        assert_eq!(overlap(&merged, &other), Some(OverlapReason::SupplyChain), "the target wins");

        other.supply_chain = Some(SupplyChainTarget::new("left-pad", "ghsa-2"));
        assert_eq!(overlap(&merged, &other), Some(OverlapReason::Issue), "then the issue");

        other.issue = Some(8);
        assert_eq!(overlap(&merged, &other), Some(OverlapReason::Files), "then the files");

        other.changed_paths = vec!["x".into()];
        assert_eq!(overlap(&merged, &other), None, "disjoint files overlap nothing");
        other.changed_paths = Vec::new();
        assert_eq!(overlap(&merged, &other), None, "no files, no file overlap");
    }

    #[test]
    fn the_file_overlap_takes_eighty_percent_of_either_side() {
        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        merged.changed_paths = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
        let mut other = targeted("other", "acme/repo", SessionStatus::Running);
        other.changed_paths = vec!["a".into(), "b".into(), "c".into(), "d".into(), "f".into()];
        assert_eq!(
            overlap(&merged, &other),
            Some(OverlapReason::Files),
            "4 of 5 shared: 80% of either side"
        );
        other.changed_paths = vec!["a".into(), "b".into(), "c".into(), "f".into(), "g".into()];
        assert_eq!(overlap(&merged, &other), None, "3 of 5 shared: 60% is short of the bar");
        // A colony that barely dips into a sweeping one is not covered by it: eight files of its
        // own dilute the two shared ones far under the bar.
        merged.changed_paths = (0..100).map(|i| format!("f{i}")).collect();
        other.changed_paths = (0..8).map(|i| format!("g{i}")).chain(["f1".into(), "f99".into()]).collect();
        assert_eq!(
            overlap(&merged, &other),
            None,
            "2 of 12 shared is nowhere near the bar, either way"
        );
    }

    #[test]
    fn a_file_overlap_needs_real_files_not_shared_lockfile_churn() {
        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        let mut other = targeted("other", "acme/repo", SessionStatus::Running);
        // Two unrelated dependency bumps: the manifest and the lockfiles are all they share.
        merged.changed_paths = vec![
            "Cargo.toml".into(),
            "Cargo.lock".into(),
            "src/lib.rs".into(),
            "crates/x/Cargo.lock".into(),
        ];
        other.changed_paths = vec!["Cargo.toml".into(), "package-lock.json".into(), "web/app.ts".into()];
        assert_eq!(
            overlap(&merged, &other),
            None,
            "a shared manifest plus lockfile churn is not the same work"
        );
        // And below the minimum count even real shared files are not coverage: this is the
        // two-files-entirely-in-common case, 100% of the smaller side and still a coincidence.
        merged.changed_paths = vec!["a".into(), "b".into()];
        other.changed_paths = vec!["a".into(), "b".into()];
        assert_eq!(
            overlap(&merged, &other),
            None,
            "two shared files is a coincidence, not coverage"
        );
    }

    #[test]
    fn a_live_colony_holding_the_target_refuses_a_second_one_and_allow_duplicate_wins() {
        let mut holder = targeted("holder", "acme/repo", SessionStatus::Running);
        with_target(&mut holder, "lodash", "ghsa-1");
        let sessions = vec![holder];
        let target = SupplyChainTarget::new("Lodash", "GHSA-1");
        let refused = launch_refusal(&sessions, "acme/repo", &target, false).expect("a live holder refuses");
        assert!(
            refused.contains("colony holder") && refused.contains("lodash / ghsa-1"),
            "{refused}"
        );
        assert!(refused.contains("allow_duplicate"), "{refused}");
        assert_eq!(
            launch_refusal(&sessions, "acme/repo", &target, true),
            None,
            "allow_duplicate starts a second colony anyway"
        );
        assert_eq!(
            launch_refusal(&sessions, "acme/other", &target, false),
            None,
            "the hold is per repository"
        );
        assert_eq!(
            launch_refusal(&sessions, "acme/repo", &SupplyChainTarget::new("left-pad", "ghsa-2"), false),
            None,
            "another target is free"
        );
    }

    #[test]
    fn a_finished_colony_leaves_its_target_free_to_try_again() {
        for status in [
            SessionStatus::Merged,
            SessionStatus::Closed,
            SessionStatus::NoChanges,
            SessionStatus::Stopped,
            SessionStatus::Failed,
        ] {
            let mut holder = targeted("holder", "acme/repo", status);
            with_target(&mut holder, "lodash", "ghsa-1");
            assert!(
                launch_refusal(&[holder], "acme/repo", &SupplyChainTarget::new("lodash", "ghsa-1"), false).is_none(),
                "{} is done, so a retry is not a duplicate",
                status.as_str()
            );
        }
    }

    #[test]
    fn only_a_superseded_and_unkempt_colony_is_blocked_from_starting() {
        let mut s = targeted("s", "acme/repo", SessionStatus::Queued);
        assert!(!blocks_start(&s), "never superseded, never blocked");
        s.superseded = Some(Supersession {
            by: "m".into(),
            pr_url: "https://github.com/acme/repo/pull/9".into(),
            pr: Some(9),
            title: "Fix the thing".into(),
            reason: OverlapReason::Issue,
            at: Utc::now(),
            kept: false,
        });
        assert!(blocks_start(&s), "superseded and not kept");
        s.superseded.as_mut().unwrap().kept = true;
        assert!(!blocks_start(&s), "kept by the operator");
    }

    #[test]
    fn a_pull_request_number_is_parsed_from_the_end_of_its_url() {
        assert_eq!(pr_number("https://github.com/acme/repo/pull/123"), Some(123));
        assert_eq!(pr_number("https://github.com/acme/repo/pull/1"), Some(1));
        assert_eq!(
            pr_number("https://github.com/acme/repo/pull/123/files"),
            Some(123),
            "a suffixed URL is still that pull request"
        );
        assert_eq!(pr_number("https://github.com/acme/repo/issue/9"), None);
        assert_eq!(pr_number("https://github.com/acme/repo/pull/"), None);
        assert_eq!(pr_number("not a url"), None);
    }

    /// A merge marks the overlapping colonies — by target, issue and file overlap alike — and
    /// leaves alone the finished, the unrelated, the stacked child this merge released, the
    /// `claim_wait` waiter (the queue retires that one itself), and any record that is not a kept
    /// one from a different merge.
    #[tokio::test]
    async fn a_merge_marks_the_overlapping_colonies_and_holds_them() {
        let root = std::env::temp_dir().join(format!("colonizer-supersede-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);

        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        merged.issue = Some(7);
        merged.issue_title = "Fix the login".into();
        // No pr_url here: the file list stands in for what record_changed_paths would have read
        // off the merged pull request before the second pass.
        merged.changed_paths = vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into(), "src/d.rs".into()];
        app.sessions.write().await.push(merged.clone());

        let files = merged.changed_paths.clone();
        let overlapping = |id: &str, status: SessionStatus| {
            let mut s = targeted(id, "acme/repo", status);
            s.changed_paths = files.clone();
            s
        };
        let mut by_issue = targeted("by-issue", "acme/repo", SessionStatus::Queued);
        by_issue.issue = Some(7);
        let mut elsewhere = targeted("elsewhere", "acme/other", SessionStatus::Running);
        elsewhere.issue = Some(7);
        elsewhere.changed_paths = files.clone();
        let mut child = overlapping("child", SessionStatus::Running);
        child.parent = Some("merged".into());
        let mut waiting = targeted("waiting", "acme/repo", SessionStatus::Queued);
        waiting.issue = Some(7);
        waiting.claim_wait = true;
        let mut kept_earlier = overlapping("kept-earlier", SessionStatus::Idle);
        kept_earlier.superseded = Some(a_record("an-earlier-merge", true));
        let mut blocked_earlier = overlapping("blocked-earlier", SessionStatus::Idle);
        blocked_earlier.superseded = Some(a_record("an-earlier-merge", false));
        let mut same_merge = overlapping("same-merge", SessionStatus::Idle);
        same_merge.superseded = Some(a_record("merged", true));
        for s in [
            overlapping("by-files", SessionStatus::Running),
            by_issue,
            overlapping("finished", SessionStatus::Stopped),
            elsewhere,
            child,
            waiting,
            kept_earlier,
            blocked_earlier,
            same_merge,
        ] {
            app.sessions.write().await.push(s);
        }
        // The colonies that get a log line need their on-disk directory, as the launch would
        // have made it (app_with_colony does the same for its single colony).
        for id in ["by-files", "by-issue", "kept-earlier"] {
            tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
        }
        side_effects(&app, mark_superseded(&app, "merged").await).await;

        let sessions = app.sessions.read().await;
        let superseded = |id: &str| sessions.iter().find(|s| s.id == id).unwrap().superseded.clone();
        let files = superseded("by-files").expect("the file overlap marks it");
        assert_eq!(files.by, "merged");
        assert_eq!(files.reason, OverlapReason::Files);
        assert_eq!(files.title, "Fix the login");
        assert!(!files.kept);
        assert_eq!(
            superseded("by-issue").unwrap().reason,
            OverlapReason::Issue,
            "the issue holds even without files"
        );
        assert!(superseded("finished").is_none(), "a stopped colony is done, nothing to mark");
        assert!(superseded("elsewhere").is_none(), "another repository is untouched");
        assert!(
            superseded("child").is_none(),
            "a stacked child is released by this merge, not covered"
        );
        assert!(
            superseded("waiting").is_none(),
            "a claim_wait waiter is the queue's to retire"
        );
        assert_eq!(
            superseded("kept-earlier").unwrap().by,
            "merged",
            "a kept record gives way to a different merge"
        );
        assert_eq!(
            superseded("blocked-earlier").unwrap().by,
            "an-earlier-merge",
            "an unkept hold is never traded away"
        );
        assert!(superseded("same-merge").unwrap().kept, "the same merge never marks twice");
        // The queue holds a superseded candidate in place instead of retiring it.
        let by_issue = sessions.iter().find(|s| s.id == "by-issue").unwrap();
        assert!(blocks_start(by_issue));
        drop(sessions);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A keep lets the colony start again: the marker stays for the record and `blocks_start`
    /// reads false, while a colony that was never superseded has nothing to keep.
    #[tokio::test]
    async fn keeping_a_superseded_colony_releases_it() {
        let root = std::env::temp_dir().join(format!("colonizer-supersede-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut merged = targeted("merged", "acme/repo", SessionStatus::Merged);
        merged.issue = Some(7);
        merged.pr_url = None;
        let mut queued = targeted("queued", "acme/repo", SessionStatus::Queued);
        queued.issue = Some(7);
        app.sessions.write().await.extend([merged, queued]);
        mark_superseded(&app, "merged").await;
        assert!(blocks_start(&app.session("queued").await.unwrap()));

        let (_, kept) = app
            .update_session("queued", |x| x.superseded.as_mut().is_some_and(Supersession::mark_kept))
            .await
            .unwrap();
        assert!(kept, "the keep released it");
        let s = app.session("queued").await.unwrap();
        assert!(!blocks_start(&s), "the queue starts it");
        assert!(s.superseded.is_some(), "the record stays for the history");

        // Not superseded: nothing to keep.
        let (_, applied) = app
            .update_session("merged", |x| x.superseded.as_mut().is_some_and(Supersession::mark_kept))
            .await
            .unwrap();
        assert!(!applied, "a colony that was never superseded has nothing to keep");
        let _ = std::fs::remove_dir_all(root);
    }
}
