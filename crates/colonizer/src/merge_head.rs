//! Quiet-head merges (issue #1075): the one way the merge train (`merge_train.rs`) and its loop
//! (`merge_loop.rs`) merge a pull request.
//!
//! A pull request whose first CI run went green could be squash-merged while its colony was still
//! pushing, and a second commit landing after that run was lost. So a merge here:
//!
//! 1. **Needs CI on that exact head.** The check runs and statuses of the head commit itself must
//!    all be green; a green run on an earlier head does not count, and a head nothing has run on
//!    yet waits.
//! 2. **Needs a quiet head.** The head must have been unchanged for the quiet period (the publish
//!    module's `merge_train_quiet_minutes`, ten minutes unless changed), measured from the later of
//!    the head commit's committer date and the first check run started on it — the run starts when
//!    GitHub sees the push, so a commit written long before it was pushed still waits.
//! 3. **Is pinned to that head.** The merge passes the head as the merge API's `sha`, so a push
//!    landing during the merge makes GitHub refuse it instead of merging the older head; that
//!    refusal reads as "the head moved, wait", never as a failure.
//! 4. **Checks the branch afterwards.** The branch's tip is read again after the merge. When it is
//!    not the merged head, commits arrived that the squash does not hold: the branch is kept and the
//!    pull request is raised as "commits not merged" (the decisions inbox shows it until dismissed).
//!    Only a branch whose tip is the merged head is deleted.
//!
//! GitHub sits behind [`HeadOps`], so both drivers and the tests share every step.

use crate::{App, authority, github, publish, util::truncate};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// The quiet period when the setting is absent or unreadable.
pub(crate) const DEFAULT_QUIET_MINUTES: u64 = 10;
/// A merge, a tip read or a branch delete gets a deadline like every other `gh` call.
const GH_LIMIT: Duration = Duration::from_secs(60);

/// The checks on one exact commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeadCi {
    Green,
    Pending,
    Failing,
    /// No check run and no status on this commit (yet).
    NotRun,
}

/// One head commit as the merge needs it: its own checks, and when it was pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadReading {
    pub ci: HeadCi,
    /// The later of the commit's committer date and its first check run's start; `None` when
    /// neither could be read, which never merges.
    pub pushed_at: Option<DateTime<Utc>>,
}

const FAILING: &[&str] = &[
    "FAILURE",
    "CANCELLED",
    "TIMED_OUT",
    "ACTION_REQUIRED",
    "STARTUP_FAILURE",
    "ERROR",
];
const PASSING: &[&str] = &["SUCCESS", "NEUTRAL", "SKIPPED"];

fn word(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().trim().to_ascii_uppercase()
}

/// `GET /repos/{repo}/commits/{sha}/check-runs` plus `…/status` for one commit: anything failed
/// fails, anything unfinished (or finished without a passing conclusion) is pending, and a commit
/// with neither check runs nor statuses has not been checked at all.
pub(crate) fn head_ci_from(check_runs: Option<&Value>, status: Option<&Value>) -> HeadCi {
    let mut seen = false;
    let mut pending = false;
    for run in check_runs.and_then(|v| v["check_runs"].as_array()).into_iter().flatten() {
        seen = true;
        let (state, conclusion) = (word(run, "status"), word(run, "conclusion"));
        if FAILING.contains(&conclusion.as_str()) {
            return HeadCi::Failing;
        }
        pending |= state != "COMPLETED" || !PASSING.contains(&conclusion.as_str());
    }
    // The combined status of a commit with no statuses says `pending`: only real ones count.
    for status in status.and_then(|v| v["statuses"].as_array()).into_iter().flatten() {
        seen = true;
        match word(status, "state").as_str() {
            "FAILURE" | "ERROR" => return HeadCi::Failing,
            "SUCCESS" => {}
            _ => pending = true,
        }
    }
    match (seen, pending) {
        (false, _) => HeadCi::NotRun,
        (true, true) => HeadCi::Pending,
        (true, false) => HeadCi::Green,
    }
}

fn parse_time(raw: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw?.trim()).ok().map(|t| t.with_timezone(&Utc))
}

/// When a head was pushed, as closely as GitHub lets it be read: the later of the commit's
/// committer date (`GET /repos/{repo}/commits/{sha}`) and the earliest `started_at` of its check
/// runs, which start when GitHub sees the push. Later is the careful side: it only lengthens the wait.
pub(crate) fn pushed_at_from(commit: Option<&Value>, check_runs: Option<&Value>) -> Option<DateTime<Utc>> {
    let committed = commit.and_then(|c| parse_time(c["commit"]["committer"]["date"].as_str()));
    let started = check_runs
        .and_then(|v| v["check_runs"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| parse_time(r["started_at"].as_str()))
        .min();
    committed.into_iter().chain(started).max()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Whether a head may merge now, and if not why — with how long until it may, when only time is
/// missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Ready,
    NotYet { reason: String, retry_in: Option<Duration> },
}

/// The quiet-head gate. `require_ci` is off only for a merge on local checks (issue #969), which
/// ran on this head already and GitHub CI could not.
pub(crate) fn gate(head: &str, r: &HeadReading, now: DateTime<Utc>, quiet: Duration, require_ci: bool) -> Gate {
    let not_yet = |reason: String| Gate::NotYet { reason, retry_in: None };
    if require_ci {
        match r.ci {
            HeadCi::Green => {}
            HeadCi::Pending => return not_yet(format!("the checks on its head {} are still running", short(head))),
            HeadCi::Failing => return not_yet(format!("the checks on its head {} failed", short(head))),
            HeadCi::NotRun => {
                return not_yet(format!(
                    "no checks have run on its head {} yet; a green run on an earlier head does not count",
                    short(head)
                ));
            }
        }
    }
    let Some(at) = r.pushed_at else {
        return not_yet(format!("when its head {} was pushed could not be read", short(head)));
    };
    let ready = at + ChronoDuration::from_std(quiet).unwrap_or_default();
    if now < ready {
        let ago = (now - at).num_minutes().max(0);
        return Gate::NotYet {
            reason: format!(
                "its head {} was pushed {ago} min ago; it merges once the head has been quiet for {} min",
                short(head),
                quiet.as_secs() / 60
            ),
            retry_in: (ready - now).to_std().ok(),
        };
    }
    Gate::Ready
}

/// Whether a refused merge was GitHub saying the head is no longer the `sha` the merge was pinned to.
pub(crate) fn head_moved(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("head branch was modified") || e.contains("http 409") || e.contains("expected head sha")
}

/// The pull request a merge is for.
#[derive(Clone, Debug)]
pub(crate) struct Candidate<'a> {
    pub repo: &'a str,
    pub number: u64,
    /// The branch the pull request merges from.
    pub branch: &'a str,
    /// The head this driver read and checked: the merge is pinned to it.
    pub head: &'a str,
    /// The squash commit's title.
    pub title: &'a str,
    /// Delete the branch after a clean merge (nothing is stacked on it).
    pub delete_branch: bool,
    pub require_ci: bool,
}

/// A merge that happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Merged {
    /// The head commit that was merged.
    pub head: String,
    /// The squash commit on the base, when GitHub said.
    pub squash: Option<String>,
    /// The branch's tip after the merge, when it is not the merged head: commits not merged.
    pub drift: Option<String>,
    /// What happened to the branch, when it was not simply deleted or kept for a stacked child.
    pub branch_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Not merged: not quiet yet, or its checks are not green on this head.
    NotYet {
        reason: String,
        retry_in: Option<Duration>,
    },
    /// GitHub refused the pinned merge: the head moved. Wait for CI on the new head.
    HeadMoved(String),
    Failed(String),
    Merged(Merged),
}

/// What a merge asks of GitHub.
pub(crate) trait HeadOps {
    async fn read_head(&self, repo: &str, sha: &str) -> Result<HeadReading, String>;
    /// Squash-merges pinned to `head` (the merge API's `sha`); the squash commit's sha when GitHub says.
    async fn merge_pinned(&self, repo: &str, number: u64, head: &str, title: &str) -> Result<Option<String>, String>;
    /// The branch's tip; `None` when the branch is gone.
    async fn branch_tip(&self, repo: &str, branch: &str) -> Result<Option<String>, String>;
    async fn delete_branch(&self, repo: &str, branch: &str) -> Result<(), String>;
}

/// The shared merge: the quiet-head gate, the pinned squash, and the post-merge drift check.
pub(crate) async fn merge_quiet_head<O: HeadOps>(ops: &O, c: &Candidate<'_>, now: DateTime<Utc>, quiet: Duration) -> Outcome {
    let reading = match ops.read_head(c.repo, c.head).await {
        Ok(r) => r,
        Err(e) => {
            return Outcome::NotYet {
                reason: format!("the checks on its head {} could not be read ({e})", short(c.head)),
                retry_in: None,
            };
        }
    };
    if let Gate::NotYet { reason, retry_in } = gate(c.head, &reading, now, quiet, c.require_ci) {
        return Outcome::NotYet { reason, retry_in };
    }
    let squash = match ops.merge_pinned(c.repo, c.number, c.head, c.title).await {
        Ok(sha) => sha,
        Err(e) if head_moved(&e) => {
            return Outcome::HeadMoved(format!(
                "its head moved off {} while it was being merged, so GitHub refused the merge; it waits for checks on the new head",
                short(c.head)
            ));
        }
        Err(e) => return Outcome::Failed(e),
    };
    let (drift, branch_note) = match ops.branch_tip(c.repo, c.branch).await {
        Ok(Some(tip)) if tip != c.head => (
            Some(tip.clone()),
            Some(format!(
                "its branch is kept: its tip {} has commits after the merged head",
                short(&tip)
            )),
        ),
        Ok(Some(_)) if c.delete_branch => match ops.delete_branch(c.repo, c.branch).await {
            Ok(()) => (None, None),
            Err(e) => (
                None,
                Some(format!("its branch could not be deleted ({})", truncate(e.trim(), 200))),
            ),
        },
        Ok(_) => (None, None),
        Err(e) => (
            None,
            Some(format!(
                "its branch is kept: the tip could not be read after the merge ({})",
                truncate(e.trim(), 200)
            )),
        ),
    };
    Outcome::Merged(Merged {
        head: c.head.to_string(),
        squash,
        drift,
        branch_note,
    })
}

/// A pull request whose branch had commits after the head that was merged (issue #1075): an
/// attention item until a person dismisses it. Kept in `merge-train-loop.json`, keyed by URL.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Unmerged {
    pub pr_url: String,
    pub repo: String,
    pub title: String,
    pub colony: Option<String>,
    /// The head the squash holds.
    pub merged_head: String,
    /// The branch's tip after the merge.
    pub tip: String,
    pub at: Option<DateTime<Utc>>,
    /// Which driver merged it: `merge train` or `merge-train loop`.
    pub by: String,
}

impl Unmerged {
    /// The attention line, as the logs and the inbox say it.
    pub(crate) fn sentence(&self) -> String {
        format!(
            "commits not merged: the {} squash-merged head {}, but the branch's tip is now {}; those later commits are not on the base",
            self.by,
            short(&self.merged_head),
            short(&self.tip)
        )
    }
}

/// Records a drift raised by the merge train's tick, beside the loop's.
pub(crate) async fn record_unmerged(app: &App, u: Unmerged) {
    let dir = app.cfg.config_dir.clone();
    if let Err(e) = crate::merge_loop::update(&dir, move |s| {
        s.commits_not_merged.insert(u.pr_url.clone(), u);
    })
    .await
    {
        eprintln!("merge train: could not record unmerged commits: {e:#}");
    }
}

/// The pull request number at the end of its URL.
pub(crate) fn pr_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// The quiet period from the publish module's `merge_train_quiet_minutes`.
pub(crate) fn quiet_period(minutes: u64) -> Duration {
    Duration::from_secs(minutes.saturating_mul(60))
}

/// The real GitHub, through `gh`.
pub(crate) struct GhHead<'a> {
    pub app: &'a App,
}

impl GhHead<'_> {
    async fn get(&self, path: &str) -> Result<Option<Value>, String> {
        match github::gh_get(self.app, path, None).await {
            Ok((404, _)) => Ok(None),
            Ok((status, body)) if status >= 400 => Err(format!("HTTP {status}: {}", truncate(body.trim(), 300))),
            Ok((_, body)) if body.trim().is_empty() => Ok(Some(Value::Null)),
            Ok((_, body)) => serde_json::from_str(&body).map(Some).map_err(|e| format!("{e}")),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    async fn gh(&self, args: Vec<String>) -> Result<String, String> {
        crate::util::exec_within(GH_LIMIT, &mut self.app.gh(args))
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

impl HeadOps for GhHead<'_> {
    async fn read_head(&self, repo: &str, sha: &str) -> Result<HeadReading, String> {
        let runs = self
            .get(&format!("repos/{repo}/commits/{sha}/check-runs?per_page=100"))
            .await?
            .ok_or_else(|| format!("the head {} was not found", short(sha)))?;
        let status = self.get(&format!("repos/{repo}/commits/{sha}/status")).await.ok().flatten();
        let commit = self.get(&format!("repos/{repo}/commits/{sha}")).await.ok().flatten();
        Ok(HeadReading {
            ci: head_ci_from(Some(&runs), status.as_ref()),
            pushed_at: pushed_at_from(commit.as_ref(), Some(&runs)),
        })
    }

    async fn merge_pinned(&self, repo: &str, number: u64, head: &str, title: &str) -> Result<Option<String>, String> {
        if authority::external_writes_blocked() {
            return Err(publish::BLOCKED.to_string());
        }
        let out = self
            .gh(vec![
                "api".into(),
                "-X".into(),
                "PUT".into(),
                format!("repos/{repo}/pulls/{number}/merge"),
                "-f".into(),
                "merge_method=squash".into(),
                "-f".into(),
                format!("sha={head}"),
                "-f".into(),
                format!("commit_title={title}"),
            ])
            .await?;
        let v: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        if v["merged"].as_bool() == Some(false) {
            return Err(v["message"].as_str().unwrap_or("GitHub did not merge it").to_string());
        }
        Ok(v["sha"].as_str().map(str::to_string).filter(|s| !s.is_empty()))
    }

    async fn branch_tip(&self, repo: &str, branch: &str) -> Result<Option<String>, String> {
        let Some(v) = self.get(&format!("repos/{repo}/git/ref/heads/{branch}")).await? else {
            return Ok(None);
        };
        v["object"]["sha"]
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| "the branch's ref had no sha".to_string())
    }

    async fn delete_branch(&self, repo: &str, branch: &str) -> Result<(), String> {
        if authority::external_writes_blocked() {
            return Err(publish::BLOCKED.to_string());
        }
        self.gh(vec![
            "api".into(),
            "-X".into(),
            "DELETE".into(),
            format!("repos/{repo}/git/refs/heads/{branch}"),
        ])
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;
    use std::{cell::RefCell, collections::VecDeque};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap()
    }

    const QUIET: Duration = Duration::from_secs(600);

    /// A scripted GitHub: heads by sha, merge answers in order, the branch's tip after the merge.
    #[derive(Default)]
    struct FakeGh {
        heads: RefCell<Vec<(String, HeadReading)>>,
        merges: RefCell<VecDeque<Result<Option<String>, String>>>,
        tip: RefCell<Option<String>>,
        log: RefCell<Vec<String>>,
    }

    impl FakeGh {
        fn head(self, sha: &str, ci: HeadCi, pushed: DateTime<Utc>) -> Self {
            self.heads.borrow_mut().push((
                sha.into(),
                HeadReading {
                    ci,
                    pushed_at: Some(pushed),
                },
            ));
            self
        }
    }

    impl HeadOps for FakeGh {
        async fn read_head(&self, _repo: &str, sha: &str) -> Result<HeadReading, String> {
            self.log.borrow_mut().push(format!("read {sha}"));
            self.heads
                .borrow()
                .iter()
                .find(|(s, _)| s == sha)
                .map(|(_, r)| r.clone())
                .ok_or_else(|| "HTTP 422: No commit found".to_string())
        }
        async fn merge_pinned(&self, _repo: &str, number: u64, head: &str, _title: &str) -> Result<Option<String>, String> {
            self.log.borrow_mut().push(format!("merge #{number} sha={head}"));
            self.merges.borrow_mut().pop_front().unwrap_or(Ok(Some("squash1".into())))
        }
        async fn branch_tip(&self, _repo: &str, branch: &str) -> Result<Option<String>, String> {
            self.log.borrow_mut().push(format!("tip {branch}"));
            Ok(self.tip.borrow().clone())
        }
        async fn delete_branch(&self, _repo: &str, branch: &str) -> Result<(), String> {
            self.log.borrow_mut().push(format!("delete {branch}"));
            Ok(())
        }
    }

    fn candidate(head: &str) -> Candidate<'_> {
        Candidate {
            repo: "acme/web",
            number: 7,
            branch: "colonizer/issue-7",
            head,
            title: "Change 7 (#7)",
            delete_branch: true,
            require_ci: true,
        }
    }

    #[test]
    fn the_checks_must_be_green_on_the_exact_head() {
        let run = |status: &str, conclusion: Option<&str>| json!({"status": status, "conclusion": conclusion, "started_at": "2026-10-05T11:00:00Z"});
        let runs = |items: Vec<Value>| json!({"total_count": items.len(), "check_runs": items});
        let none = json!({"state": "pending", "statuses": []});
        assert_eq!(head_ci_from(Some(&runs(vec![])), Some(&none)), HeadCi::NotRun);
        assert_eq!(
            head_ci_from(Some(&runs(vec![run("completed", Some("success"))])), Some(&none)),
            HeadCi::Green
        );
        assert_eq!(
            head_ci_from(
                Some(&runs(vec![run("completed", Some("success")), run("in_progress", None)])),
                None
            ),
            HeadCi::Pending
        );
        assert_eq!(
            head_ci_from(Some(&runs(vec![run("completed", Some("failure"))])), None),
            HeadCi::Failing
        );
        let red_status = json!({"state": "failure", "statuses": [{"state": "failure"}]});
        assert_eq!(
            head_ci_from(Some(&runs(vec![run("completed", Some("success"))])), Some(&red_status)),
            HeadCi::Failing
        );
        let only_status = json!({"state": "success", "statuses": [{"state": "success"}]});
        assert_eq!(head_ci_from(None, Some(&only_status)), HeadCi::Green);
    }

    #[test]
    fn the_push_time_is_the_later_of_the_commit_and_its_first_check() {
        let commit = json!({"commit": {"committer": {"date": "2026-10-05T09:00:00Z"}}});
        let runs = json!({"check_runs": [
            {"started_at": "2026-10-05T11:58:00Z"},
            {"started_at": "2026-10-05T11:55:00Z"},
        ]});
        // Written at nine, pushed just before noon: the push counts.
        assert_eq!(
            pushed_at_from(Some(&commit), Some(&runs)),
            Some(Utc.with_ymd_and_hms(2026, 10, 5, 11, 55, 0).unwrap())
        );
        assert_eq!(
            pushed_at_from(Some(&commit), None),
            Some(Utc.with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap())
        );
        assert_eq!(pushed_at_from(None, None), None);
    }

    #[tokio::test]
    async fn a_push_inside_the_quiet_period_delays_the_merge() {
        let gh = FakeGh::default().head("new", HeadCi::Green, t0() - ChronoDuration::minutes(3));
        let out = merge_quiet_head(&gh, &candidate("new"), t0(), QUIET).await;
        let Outcome::NotYet { reason, retry_in } = out else {
            panic!("merged too early: {out:?}");
        };
        assert!(reason.contains("pushed 3 min ago"), "{reason}");
        assert_eq!(retry_in, Some(Duration::from_secs(7 * 60)));
        assert!(!gh.log.borrow().iter().any(|l| l.starts_with("merge")), "nothing merged");
        // Once quiet, it merges pinned to that head.
        let later = t0() + ChronoDuration::minutes(7);
        *gh.tip.borrow_mut() = Some("new".into());
        let Outcome::Merged(m) = merge_quiet_head(&gh, &candidate("new"), later, QUIET).await else {
            panic!("did not merge once quiet");
        };
        assert_eq!(
            (m.head.as_str(), m.squash.as_deref(), m.drift),
            ("new", Some("squash1"), None)
        );
        assert!(gh.log.borrow().contains(&"merge #7 sha=new".to_string()));
        assert!(gh.log.borrow().contains(&"delete colonizer/issue-7".to_string()));
    }

    #[tokio::test]
    async fn a_green_run_on_an_older_head_does_not_count() {
        // The pull request's rollup was green for `old`; the head is `new`, and nothing ran on it.
        let hour_ago = t0() - ChronoDuration::hours(1);
        let gh = FakeGh::default()
            .head("old", HeadCi::Green, hour_ago)
            .head("new", HeadCi::NotRun, hour_ago);
        let out = merge_quiet_head(&gh, &candidate("new"), t0(), QUIET).await;
        let Outcome::NotYet { reason, .. } = out else {
            panic!("merged on an older head's run: {out:?}");
        };
        assert!(reason.contains("earlier head does not count"), "{reason}");
        let gh = FakeGh::default().head("new", HeadCi::Pending, hour_ago);
        assert!(matches!(
            merge_quiet_head(&gh, &candidate("new"), t0(), QUIET).await,
            Outcome::NotYet { .. }
        ));
        // A merge on local checks (issue #969) needs the quiet head, not GitHub CI.
        let gh = FakeGh::default().head("new", HeadCi::Failing, hour_ago);
        *gh.tip.borrow_mut() = Some("new".into());
        let mut local = candidate("new");
        local.require_ci = false;
        assert!(matches!(merge_quiet_head(&gh, &local, t0(), QUIET).await, Outcome::Merged(_)));
    }

    #[tokio::test]
    async fn a_head_that_moves_during_the_merge_is_refused_and_waits() {
        let gh = FakeGh::default().head("old", HeadCi::Green, t0() - ChronoDuration::hours(1));
        gh.merges.borrow_mut().push_back(Err(
            "gh: Head branch was modified. Review and try the merge again. (HTTP 409)".into(),
        ));
        let out = merge_quiet_head(&gh, &candidate("old"), t0(), QUIET).await;
        let Outcome::HeadMoved(reason) = out else {
            panic!("expected a head-moved wait: {out:?}");
        };
        assert!(reason.contains("waits for checks on the new head"), "{reason}");
        assert_eq!(gh.log.borrow().as_slice(), ["read old", "merge #7 sha=old"]);
        assert!(head_moved("HTTP 409: Head branch was modified"));
        assert!(!head_moved("HTTP 405: Pull Request is not mergeable"));
    }

    #[tokio::test]
    async fn commits_after_the_merged_head_keep_the_branch_and_are_raised() {
        let gh = FakeGh::default().head("old", HeadCi::Green, t0() - ChronoDuration::hours(1));
        *gh.tip.borrow_mut() = Some("later".into());
        let Outcome::Merged(m) = merge_quiet_head(&gh, &candidate("old"), t0(), QUIET).await else {
            panic!("did not merge");
        };
        assert_eq!(m.drift.as_deref(), Some("later"));
        assert!(m.branch_note.unwrap().contains("kept"));
        assert!(
            !gh.log.borrow().iter().any(|l| l.starts_with("delete")),
            "the later commits keep their branch"
        );
        let u = Unmerged {
            merged_head: "old".into(),
            tip: "later".into(),
            by: "merge train".into(),
            ..Unmerged::default()
        };
        assert!(u.sentence().starts_with("commits not merged"), "{}", u.sentence());
    }
}
