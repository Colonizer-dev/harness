//! Colony-to-commit links that survive rebase, amend, squash and force-push (issue #765).
//!
//! Until this module the only trace of which commits a colony wrote was the publish log line
//! `committed <short sha> as …` in the colony's `harness.jsonl`: a sha that stops meaning anything
//! the moment the merge train, an update-branch, a redo colony or the auto-rebase rewrites the
//! branch. Now every colony commit is recorded in `<data>/sessions/<id>/commits.json` next to its
//! `git patch-id --stable` — the id of the change itself, which a rebase, a cherry-pick or a
//! message-only amend keeps — plus the colony and the agent session that wrote it.
//!
//! [`reconcile`] re-points the links after the branch moved: a recorded commit that is no longer
//! reachable from the tip is matched to the one reachable commit carrying the same patch-id. It
//! never guesses. No match (a squash or a content amend changed the patch) or more than one
//! candidate orphans the link — kept, flagged `orphaned`, its sha untouched — and any git failure
//! leaves every link exactly as it was. While a rebase is in progress (`rebase-merge` or
//! `rebase-apply` in the git dir) it does nothing at all: the branch is mid-rewrite, and matching
//! against half a history would orphan commits that are about to come back.
//!
//! The matching is pure over a git builder, so the tests drive it against throwaway repositories;
//! [`record_for_session`] and [`reconcile_session`] are the production wrappers over the harness's
//! hardened host git ([`App::git`]).

use crate::{App, util::write_atomic};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

/// The per-session file the links live in, beside `harness.jsonl` and `events.jsonl`.
pub const FILE: &str = "commits.json";

/// Every git call here runs under this deadline; a timeout is a failure like any other, so it
/// leaves the links alone.
const GIT_LIMIT: Duration = Duration::from_secs(30);

/// One colony's recorded commits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitLinks {
    #[serde(default)]
    pub links: Vec<CommitLink>,
    /// The branch tip the links were last reconciled against. A head the mothership sees again
    /// (the PR watcher, the merge train, a `sync_repo` fetch) that matches it is a no-op.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
}

/// One commit a colony wrote, and where it lives now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitLink {
    /// The commit's current full sha: where the link points after every re-point so far.
    pub sha: String,
    /// `git patch-id --stable` of the commit as recorded; `None` for a commit with an empty diff,
    /// which has none (such a link can only ever be orphaned once its sha is gone).
    pub patch_id: Option<String>,
    /// The colony (session id) that wrote it.
    pub colony: String,
    /// The agent runner's session inside that colony, when it had reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
    pub recorded_at: DateTime<Utc>,
    /// The shas this link pointed at before, oldest first: the rewrite history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous: Vec<String>,
    /// The commit is gone from the branch and no single commit with its patch-id replaced it (a
    /// squash, a content amend, or an ambiguous match). The link is kept, never guessed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orphaned: bool,
}

/// What a reconcile did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciled {
    /// A rebase is in progress in the git dir; nothing was read or changed.
    RebaseInProgress,
    /// The links were checked against the tip.
    Done { repointed: usize, orphaned: usize },
}

/// Builds a fresh git command with the git dir already pinned: [`App::git`] in production.
pub type GitBuilder<'a> = &'a (dyn Fn() -> Command + Sync);

/// Whether git is in the middle of a rebase in `git_dir` (for a linked worktree, its admin dir).
pub fn rebase_in_progress(git_dir: &Path) -> bool {
    git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
}

/// Runs git to completion under the deadline: `(exit code, stdout)`. `Err` when it could not start,
/// timed out or died on a signal — every one of which a caller treats as "no answer".
async fn run(git: GitBuilder<'_>, args: &[&str], stdin: Option<&[u8]>) -> Result<(i32, String)> {
    let mut cmd = git();
    cmd.args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let what = format!("git {}", args.join(" "));
    let work = async {
        let mut child = cmd.spawn().with_context(|| format!("failed to start `{what}`"))?;
        if let Some(input) = stdin {
            let mut pipe = child.stdin.take().context("git stdin")?;
            pipe.write_all(input).await?;
            drop(pipe);
        }
        let out = child.wait_with_output().await?;
        let code = out
            .status
            .code()
            .with_context(|| format!("`{what}` was killed by a signal"))?;
        anyhow::Ok((code, String::from_utf8_lossy(&out.stdout).into_owned()))
    };
    tokio::time::timeout(GIT_LIMIT, work)
        .await
        .with_context(|| format!("`{what}` timed out after {GIT_LIMIT:?}"))?
}

/// [`run`] that insists on exit 0.
async fn ok(git: GitBuilder<'_>, args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    let (code, out) = run(git, args, stdin).await?;
    if code != 0 {
        bail!("`git {}` exited {code}", args.join(" "));
    }
    Ok(out)
}

/// The non-merge commits `rev-list` names for `range`, newest first, each with its stable
/// patch-id (`None` for an empty diff).
async fn commits_with_patch_ids(git: GitBuilder<'_>, range: &[&str]) -> Result<Vec<(String, Option<String>)>> {
    let mut args = vec!["rev-list", "--no-merges"];
    args.extend_from_slice(range);
    let shas: Vec<String> = ok(git, &args, None).await?.lines().map(str::to_string).collect();
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    // `git log -p` prefixed with `commit <sha>` is exactly what `patch-id` reads. Diff settings a
    // repository's config could vary are pinned, so one commit always hashes the same.
    let mut log = vec![
        "log",
        "-p",
        "--no-merges",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--format=commit %H",
    ];
    log.extend_from_slice(range);
    let patches = ok(git, &log, None).await?;
    let ids = ok(git, &["patch-id", "--stable"], Some(patches.as_bytes())).await?;
    let by_sha: HashMap<&str, &str> = ids
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(pid, sha)| (sha.trim(), pid.trim()))
        .collect();
    Ok(shas
        .into_iter()
        .map(|sha| {
            let pid = by_sha.get(sha.as_str()).map(|p| p.to_string());
            (sha, pid)
        })
        .collect())
}

/// Resolves `rev` to a full commit sha; a failure is an error, never "absent".
async fn resolve(git: GitBuilder<'_>, rev: &str) -> Result<String> {
    let sha = ok(
        git,
        &["rev-parse", "--verify", "--end-of-options", &format!("{rev}^{{commit}}")],
        None,
    )
    .await?;
    Ok(sha.trim().to_string())
}

/// Whether `sha` is reachable from `tip`. A commit object the repository no longer has is
/// unreachable; any other failure is an error.
async fn reachable(git: GitBuilder<'_>, sha: &str, tip: &str) -> Result<bool> {
    match run(git, &["merge-base", "--is-ancestor", sha, tip], None).await?.0 {
        0 => Ok(true),
        1 => Ok(false),
        code => {
            // 128 is also what a missing object reads as: tell the two apart.
            let (exists, _) = run(git, &["cat-file", "-e", &format!("{sha}^{{commit}}")], None).await?;
            if exists != 0 {
                return Ok(false);
            }
            bail!("`git merge-base --is-ancestor {sha} {tip}` exited {code}")
        }
    }
}

/// The range argument list for a tip and an optional base to exclude.
fn range<'a>(tip: &'a str, exclude: Option<&'a str>) -> Vec<String> {
    let mut r = vec![tip.to_string()];
    if let Some(base) = exclude {
        r.push(format!("^{base}"));
    }
    r
}

/// Re-points `links` at the commits now reachable from `tip`, in place. `base` (a resolved or
/// resolvable rev) bounds the candidate commits to the colony's own — what `tip` has and `base`
/// does not — so a long history is never walked. A rebase in progress changes nothing; so does any
/// git failure, which is returned as `Err` with `links` exactly as they came in.
pub async fn reconcile(
    git: GitBuilder<'_>,
    git_dir: &Path,
    links: &mut CommitLinks,
    tip: &str,
    base: Option<&str>,
) -> Result<Reconciled> {
    if rebase_in_progress(git_dir) {
        return Ok(Reconciled::RebaseInProgress);
    }
    if links.links.is_empty() {
        return Ok(Reconciled::Done {
            repointed: 0,
            orphaned: 0,
        });
    }
    let tip = resolve(git, tip).await?;
    let base = match base {
        Some(b) => Some(resolve(git, b).await?),
        None => None,
    };
    let r = range(&tip, base.as_deref());
    let r: Vec<&str> = r.iter().map(String::as_str).collect();
    let candidates = commits_with_patch_ids(git, &r).await?;

    // Every read happens before anything is written, so a failure part-way leaves `links` alone.
    let mut stale = Vec::new();
    let mut claimed = HashSet::new();
    for (i, link) in links.links.iter().enumerate() {
        if candidates.iter().any(|(sha, _)| *sha == link.sha) || reachable(git, &link.sha, &tip).await? {
            claimed.insert(link.sha.clone());
        } else {
            stale.push(i);
        }
    }
    // Each stale link's possible targets: unclaimed candidates with its patch-id.
    let targets: Vec<(usize, Vec<&str>)> = stale
        .iter()
        .map(|&i| {
            let pid = links.links[i].patch_id.as_deref();
            let found = candidates
                .iter()
                .filter(|(sha, cand)| pid.is_some() && cand.as_deref() == pid && !claimed.contains(sha))
                .map(|(sha, _)| sha.as_str())
                .collect();
            (i, found)
        })
        .collect();
    // A target two stale links both want is ambiguous for both.
    let mut wanted: HashMap<&str, usize> = HashMap::new();
    for (_, found) in &targets {
        if let [one] = found.as_slice() {
            *wanted.entry(one).or_default() += 1;
        }
    }

    let mut next = links.clone();
    for link in next.links.iter_mut().filter(|l| claimed.contains(&l.sha)) {
        link.orphaned = false;
    }
    let (mut repointed, mut orphaned) = (0, 0);
    for (i, found) in &targets {
        let link = &mut next.links[*i];
        match found.as_slice() {
            [one] if wanted.get(one) == Some(&1) => {
                let old = std::mem::replace(&mut link.sha, one.to_string());
                link.previous.push(old);
                link.orphaned = false;
                repointed += 1;
            }
            _ if !link.orphaned => {
                link.orphaned = true;
                orphaned += 1;
            }
            _ => {}
        }
    }
    *links = next;
    Ok(Reconciled::Done { repointed, orphaned })
}

/// Records every commit `tip` has and `base` does not that no link points at yet, with its
/// patch-id. Run after [`reconcile`], so a commit a rewrite merely moved is re-pointed rather than
/// recorded twice. Returns how many were added; `Ok(0)` while a rebase is in progress.
pub async fn record(
    git: GitBuilder<'_>,
    git_dir: &Path,
    links: &mut CommitLinks,
    tip: &str,
    base: &str,
    colony: &str,
    agent_session: Option<&str>,
) -> Result<usize> {
    if rebase_in_progress(git_dir) {
        return Ok(0);
    }
    let tip = resolve(git, tip).await?;
    let base = resolve(git, base).await?;
    let r = range(&tip, Some(&base));
    let r: Vec<&str> = r.iter().map(String::as_str).collect();
    let known: HashSet<String> = links.links.iter().map(|l| l.sha.clone()).collect();
    let now = Utc::now();
    // Oldest first, so the file reads in the order the commits were written.
    let fresh: Vec<CommitLink> = commits_with_patch_ids(git, &r)
        .await?
        .into_iter()
        .rev()
        .filter(|(sha, _)| !known.contains(sha))
        .map(|(sha, patch_id)| CommitLink {
            sha,
            patch_id,
            colony: colony.to_string(),
            agent_session: agent_session.map(str::to_string),
            recorded_at: now,
            previous: Vec::new(),
            orphaned: false,
        })
        .collect();
    let added = fresh.len();
    links.links.extend(fresh);
    Ok(added)
}

/// Serializes the load-modify-save of every colony's links, so a publish and a reconcile racing on
/// one colony never drop each other's writes.
static FILE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Reads a colony's links: absent is empty, unparsable is an error (never overwritten blind).
pub async fn load(app: &App, id: &str) -> Result<CommitLinks> {
    let path = app.session_dir(id).join(FILE);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("could not parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CommitLinks::default()),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

async fn save(app: &App, id: &str, links: &CommitLinks) -> Result<()> {
    let dir = app.session_dir(id);
    tokio::fs::create_dir_all(&dir).await?;
    write_atomic(&dir.join(FILE), &serde_json::to_vec_pretty(links)?).await
}

/// Publish time: reconcile what the colony recorded before against the branch as it now stands (a
/// publish-time restack may have rewritten it), then record the commits it carries past
/// `origin/<base>`. Called after a successful push, with the worktree's admin dir as the git dir.
pub async fn record_for_session(app: &App, id: &str, admin: &Path, base: &str) -> Result<usize> {
    let _guard = FILE_LOCK.lock().await;
    let agent_session = app.session(id).await.and_then(|s| s.agent_session);
    let git = || app.git(admin);
    let base_ref = format!("refs/remotes/origin/{base}");
    let mut links = load(app, id).await?;
    reconcile(&git, admin, &mut links, "HEAD", Some(&base_ref)).await?;
    let added = record(&git, admin, &mut links, "HEAD", &base_ref, id, agent_session.as_deref()).await?;
    // The push just landed HEAD on origin: the PR watcher's next reading of this head is no news.
    links.tip = Some(resolve(&git, "HEAD").await?);
    save(app, id, &links).await?;
    Ok(added)
}

/// The mothership noticed a colony's branch moved (it rewrote it itself, or a fetch brought a
/// force-push): re-point that colony's links against its worktree's `HEAD`. Best effort and quiet
/// on success; what it changed and any failure are said in the colony's own log. A failure changes
/// nothing.
pub async fn reconcile_session(app: &App, id: &str) -> Option<Reconciled> {
    let s = app.session(id).await?;
    let admin = std::path::PathBuf::from(s.git_admin_dir.as_ref()?);
    let base_ref = format!("refs/remotes/origin/{}", s.base.as_deref().unwrap_or("main"));
    let git = || app.git(&admin);
    let result = reconcile_locked(app, id, &git, &admin, "HEAD", &base_ref, None).await;
    report(app, id, result).await
}

/// Load, reconcile against `tip`, stamp the resolved tip, save when anything changed — all under
/// the file lock. `skip_if_tip` short-circuits (`Ok(None)`) when the links were already reconciled
/// against exactly that sha, read under the same lock.
async fn reconcile_locked(
    app: &App,
    id: &str,
    git: GitBuilder<'_>,
    rebase_dir: &Path,
    tip: &str,
    base_ref: &str,
    skip_if_tip: Option<&str>,
) -> Result<Option<Reconciled>> {
    let _guard = FILE_LOCK.lock().await;
    let mut links = load(app, id).await?;
    if links.links.is_empty() || (skip_if_tip.is_some() && links.tip.as_deref() == skip_if_tip) {
        return Ok(None);
    }
    let before = links.clone();
    let resolved = resolve(git, tip).await?;
    let outcome = reconcile(git, rebase_dir, &mut links, &resolved, Some(base_ref)).await?;
    if outcome != Reconciled::RebaseInProgress {
        links.tip = Some(resolved);
    }
    if links != before {
        save(app, id, &links).await?;
    }
    Ok(Some(outcome))
}

/// Says what a reconcile did in the colony's own log: nothing when nothing moved.
async fn report(app: &App, id: &str, result: Result<Option<Reconciled>>) -> Option<Reconciled> {
    match result {
        Ok(Some(outcome @ Reconciled::Done { repointed, orphaned })) => {
            if repointed + orphaned > 0 {
                app.session_log(
                    id,
                    if orphaned > 0 { "warn" } else { "info" },
                    format!("commit links: re-pointed {repointed} rewritten commit(s), orphaned {orphaned} with no single match"),
                )
                .await;
            }
            Some(outcome)
        }
        Ok(outcome) => outcome,
        Err(e) => {
            app.session_log(
                id,
                "warn",
                format!("commit links: could not reconcile ({e:#}); links left untouched"),
            )
            .await;
            None
        }
    }
}

/// A branch name safe to splice into a fetch refspec: what `create_worktree` makes, never an
/// option, a second refspec or a revision expression.
fn plain_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with(['-', '/'])
        && !branch.ends_with(['/', '.'])
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.ends_with(".lock")
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Where a colony's links live on the host: its session, its mirror, the remote-tracking ref of
/// its branch and base, and the dir a rebase in progress would show up in.
struct Place {
    bare: std::path::PathBuf,
    branch_ref: String,
    base_ref: String,
    branch: String,
    rebase_dir: std::path::PathBuf,
}

async fn place(app: &App, id: &str) -> Option<Place> {
    let s = app.session(id).await?;
    if !plain_branch(&s.branch) {
        return None;
    }
    let bare = app.bare_repo(&s.repo);
    // The worktree's admin dir is where a rebase in progress shows; a cleaned-up colony has none,
    // and nothing can be rebasing it then, so the mirror's own git dir stands in.
    let rebase_dir = s
        .git_admin_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(|| bare.clone());
    Some(Place {
        branch_ref: format!("refs/remotes/origin/{}", s.branch),
        base_ref: format!("refs/remotes/origin/{}", s.base.as_deref().unwrap_or("main")),
        branch: s.branch,
        bare,
        rebase_dir,
    })
}

/// Whether a colony has any links on disk, without parsing them: the cheap filter every trigger
/// runs first.
fn has_links(app: &App, id: &str) -> bool {
    app.session_dir(id).join(FILE).is_file()
}

/// The PR's head moved to `head` (issue #765, the watcher's or the merge train's reading): fetch
/// the colony branch into the host mirror through the hardened host git, then re-point the links
/// against it. `None` when there was nothing to do — no links, the head is the one already
/// reconciled, or a fetch or git failure (said in the colony's log; the links stay untouched).
pub async fn on_pr_head(app: &App, id: &str, head: &str) -> Option<Reconciled> {
    if !has_links(app, id) {
        return None;
    }
    let p = place(app, id).await?;
    // Cheap first: the head already reconciled needs no fetch at all.
    if load(app, id).await.ok()?.tip.as_deref() == Some(head) {
        return None;
    }
    let refspec = format!("+refs/heads/{0}:refs/remotes/origin/{0}", p.branch);
    let fetched = crate::util::exec_within(
        GIT_LIMIT,
        app.git_authed(&p.bare)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "origin",
                "--end-of-options",
            ])
            .arg(&refspec),
    )
    .await;
    if let Err(e) = fetched {
        app.session_log(
            id,
            "warn",
            format!(
                "commit links: could not fetch {} after its head moved ({e:#}); links left untouched",
                p.branch
            ),
        )
        .await;
        return None;
    }
    let git = || app.git(&p.bare);
    let result = reconcile_locked(app, id, &git, &p.rebase_dir, &p.branch_ref, &p.base_ref, Some(head)).await;
    report(app, id, result).await
}

/// The last head each colony's PR was seen at, so a head read again every tick costs nothing.
static SEEN_HEADS: std::sync::Mutex<Option<HashMap<String, String>>> = std::sync::Mutex::new(None);

/// Whether `head` is news for colony `id`, remembering it either way.
fn head_is_news(id: &str, head: &str) -> bool {
    let mut seen = SEEN_HEADS.lock().unwrap_or_else(|e| e.into_inner());
    let map = seen.get_or_insert_with(HashMap::new);
    map.insert(id.to_string(), head.to_string()).as_deref() != Some(head)
}

/// A reading of a colony PR's head (the PR watcher, the merge train). When it differs from the
/// last one seen, and the colony has links, the fetch and reconcile run off the caller's tick.
pub fn head_seen(app: &crate::Shared, id: &str, head: Option<&str>) {
    let Some(head) = head.filter(|h| !h.is_empty()) else { return };
    if !head_is_news(id, head) || !has_links(app, id) {
        return;
    }
    let (app, id, head) = (app.clone(), id.to_string(), head.to_string());
    tokio::spawn(async move {
        on_pr_head(&app, &id, &head).await;
    });
}

/// After `sync_repo` fetched `repo` into its mirror: every colony there with links whose
/// remote-tracking branch moved past the tip last reconciled is re-pointed. One `rev-parse` per
/// colony with links; merged and closed colonies are left alone (their branches are done), and a
/// branch the fetch pruned is quietly skipped.
pub async fn after_sync(app: &App, repo: &str) {
    let sessions = app.sessions.read().await.clone();
    for s in sessions.iter().filter(|s| {
        s.repo == repo
            && !matches!(
                s.status,
                crate::sessions::SessionStatus::Merged | crate::sessions::SessionStatus::Closed
            )
            && has_links(app, &s.id)
    }) {
        let Some(p) = place(app, &s.id).await else { continue };
        let git = || app.git(&p.bare);
        let Ok(tip) = resolve(&git, &p.branch_ref).await else {
            continue;
        };
        let result = reconcile_locked(app, &s.id, &git, &p.rebase_dir, &tip, &p.base_ref, Some(&tip)).await;
        report(app, &s.id, result).await;
    }
}

/// One link as the API shows it: where it points, where it pointed, and whether it was kept
/// without a match.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CommitView {
    pub sha: String,
    pub previous: Vec<String>,
    pub orphaned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// `GET /api/sessions/{id}/commits` (issue #765): the colony's recorded commits, oldest first,
/// with their rewrite history and the `orphaned` flag. An unknown colony is a 404; a colony that
/// never published reads as an empty list.
pub async fn api_commits(
    axum::extract::State(app): axum::extract::State<crate::Shared>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, crate::AppError> {
    if app.session(&id).await.is_none() {
        return Err(crate::client_error(axum::http::StatusCode::NOT_FOUND, "no such session"));
    }
    let links = load(&app, &id).await?;
    let commits: Vec<CommitView> = links
        .links
        .into_iter()
        .map(|l| CommitView {
            sha: l.sha,
            previous: l.previous,
            orphaned: l.orphaned,
            agent_session: l.agent_session,
            recorded_at: l.recorded_at,
        })
        .collect();
    Ok(axum::Json(serde_json::json!({ "commits": commits })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Repo(PathBuf);

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Repo {
        fn new(name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "colonizer-commit-links-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = Self(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            repo.git(&["config", "user.name", "Test"]);
            repo.git(&["config", "user.email", "test@example.com"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo.commit("base.txt", "base\n", "base");
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .current_dir(&self.0)
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
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// Like `git`, but a failure is fine (a conflicting rebase).
        fn git_may_fail(&self, args: &[&str]) {
            let _ = std::process::Command::new("git")
                .current_dir(&self.0)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .unwrap();
        }

        fn commit(&self, file: &str, body: &str, message: &str) -> String {
            std::fs::write(self.0.join(file), body).unwrap();
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }

        fn git_dir(&self) -> PathBuf {
            self.0.join(".git")
        }

        /// The hardened builder shape production uses, pinned to this repo's git dir.
        fn builder(&self) -> impl Fn() -> Command + Sync + use<> {
            let dir = self.git_dir();
            move || {
                let mut c = Command::new("git");
                c.args(crate::github::HOST_GIT_NO_EXEC)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env_remove("GIT_DIR")
                    .env_remove("GIT_WORK_TREE")
                    .env_remove("GIT_INDEX_FILE")
                    .arg("--git-dir")
                    .arg(&dir);
                c
            }
        }

        /// Records the colony's commits on `colony` (past `main`), the publish-time step.
        async fn recorded(&self) -> CommitLinks {
            let mut links = CommitLinks::default();
            let git = self.builder();
            let added = record(&git, &self.git_dir(), &mut links, "HEAD", "main", "colony-1", Some("agent-1"))
                .await
                .unwrap();
            assert!(added > 0);
            links
        }

        async fn reconcile(&self, links: &mut CommitLinks) -> Result<Reconciled> {
            let git = self.builder();
            reconcile(&git, &self.git_dir(), links, "HEAD", Some("main")).await
        }
    }

    #[tokio::test]
    async fn an_amend_keeps_the_link_when_only_the_message_changed_and_orphans_it_when_the_change_did() {
        let repo = Repo::new("amend");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        let first = repo.commit("a.txt", "a\n", "add a");
        let mut links = repo.recorded().await;
        assert_eq!(links.links[0].sha, first);
        assert_eq!(links.links[0].colony, "colony-1");
        assert_eq!(links.links[0].agent_session.as_deref(), Some("agent-1"));

        repo.git(&["commit", "-q", "--amend", "-m", "add a, reworded"]);
        let amended = repo.git(&["rev-parse", "HEAD"]);
        let out = repo.reconcile(&mut links).await.unwrap();
        assert_eq!(
            out,
            Reconciled::Done {
                repointed: 1,
                orphaned: 0
            }
        );
        assert_eq!(links.links[0].sha, amended);
        assert_eq!(links.links[0].previous, vec![first]);
        assert!(!links.links[0].orphaned);

        // An amend that changes the content changes the patch-id: no guess, the link is orphaned.
        std::fs::write(repo.0.join("a.txt"), "a, changed\n").unwrap();
        repo.git(&["commit", "-q", "-a", "--amend", "-m", "add a, changed"]);
        let out = repo.reconcile(&mut links).await.unwrap();
        assert_eq!(
            out,
            Reconciled::Done {
                repointed: 0,
                orphaned: 1
            }
        );
        assert_eq!(links.links[0].sha, amended, "an orphaned link keeps its last sha");
        assert!(links.links[0].orphaned);
    }

    #[tokio::test]
    async fn a_rebase_onto_a_moved_base_repoints_every_commit() {
        let repo = Repo::new("rebase");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        let a = repo.commit("a.txt", "a\n", "add a");
        let b = repo.commit("b.txt", "b\n", "add b");
        let mut links = repo.recorded().await;
        assert_eq!(links.links.iter().map(|l| l.sha.as_str()).collect::<Vec<_>>(), [&a, &b]);

        repo.git(&["checkout", "-q", "main"]);
        repo.commit("main.txt", "moved\n", "main moves on");
        repo.git(&["checkout", "-q", "colony"]);
        repo.git(&["rebase", "-q", "main"]);
        let new_a = repo.git(&["rev-parse", "HEAD~1"]);
        let new_b = repo.git(&["rev-parse", "HEAD"]);

        let out = repo.reconcile(&mut links).await.unwrap();
        assert_eq!(
            out,
            Reconciled::Done {
                repointed: 2,
                orphaned: 0
            }
        );
        assert_eq!(links.links[0].sha, new_a);
        assert_eq!(links.links[1].sha, new_b);
        assert_eq!(links.links[0].previous, vec![a]);

        // A second pass over the same history changes nothing.
        let again = links.clone();
        assert_eq!(
            repo.reconcile(&mut links).await.unwrap(),
            Reconciled::Done {
                repointed: 0,
                orphaned: 0
            }
        );
        assert_eq!(links, again);
    }

    #[tokio::test]
    async fn a_squash_of_three_into_one_orphans_all_three_instead_of_guessing() {
        let repo = Repo::new("squash");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        let shas = [
            repo.commit("a.txt", "a\n", "add a"),
            repo.commit("b.txt", "b\n", "add b"),
            repo.commit("c.txt", "c\n", "add c"),
        ];
        let mut links = repo.recorded().await;
        assert_eq!(links.links.len(), 3);

        repo.git(&["reset", "-q", "--soft", "main"]);
        repo.git(&["commit", "-q", "-m", "squashed"]);
        let out = repo.reconcile(&mut links).await.unwrap();
        assert_eq!(
            out,
            Reconciled::Done {
                repointed: 0,
                orphaned: 3
            }
        );
        for (link, sha) in links.links.iter().zip(&shas) {
            assert!(link.orphaned);
            assert_eq!(&link.sha, sha, "kept, not re-pointed");
        }
        // Still orphaned on the next pass, and not counted twice.
        assert_eq!(
            repo.reconcile(&mut links).await.unwrap(),
            Reconciled::Done {
                repointed: 0,
                orphaned: 0
            }
        );
    }

    #[tokio::test]
    async fn a_cherry_pick_onto_a_fresh_branch_repoints_the_link() {
        let repo = Repo::new("cherry");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        let a = repo.commit("a.txt", "a\n", "add a");
        let mut links = repo.recorded().await;

        // A redo: the branch is rebuilt from a moved main and the old commit cherry-picked across,
        // then the rebuilt branch replaces the old one (what a force-push looks like locally).
        repo.git(&["checkout", "-q", "main"]);
        repo.commit("main.txt", "moved\n", "main moves on");
        repo.git(&["checkout", "-q", "-b", "redo"]);
        repo.git(&["cherry-pick", &a]);
        let picked = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["branch", "-f", "colony", "redo"]);
        repo.git(&["checkout", "-q", "colony"]);

        let out = repo.reconcile(&mut links).await.unwrap();
        assert_eq!(
            out,
            Reconciled::Done {
                repointed: 1,
                orphaned: 0
            }
        );
        assert_eq!(links.links[0].sha, picked);
    }

    #[tokio::test]
    async fn a_failed_git_call_leaves_every_link_untouched() {
        let repo = Repo::new("failure");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        repo.commit("a.txt", "a\n", "add a");
        let mut links = repo.recorded().await;
        // A rewrite that would orphan the link if git answered...
        repo.git(&["reset", "-q", "--hard", "main"]);
        repo.commit("z.txt", "z\n", "something else");
        let before = links.clone();

        // ...but git fails: the builder points at a git dir that is not there.
        let broken = repo.0.join("no-such-git-dir");
        let git = move || {
            let mut c = Command::new("git");
            c.args(crate::github::HOST_GIT_NO_EXEC)
                .env_remove("GIT_DIR")
                .arg("--git-dir")
                .arg(&broken);
            c
        };
        assert!(
            reconcile(&git, &repo.git_dir(), &mut links, "HEAD", Some("main"))
                .await
                .is_err()
        );
        assert_eq!(links, before, "nothing orphaned, nothing re-pointed");

        // And a git that cannot even start is the same.
        let missing = || Command::new("/nonexistent/git");
        assert!(
            reconcile(&missing, &repo.git_dir(), &mut links, "HEAD", Some("main"))
                .await
                .is_err()
        );
        assert_eq!(links, before);
    }

    #[tokio::test]
    async fn a_rebase_in_progress_is_skipped() {
        let repo = Repo::new("midrebase");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        repo.commit("base.txt", "colony side\n", "colony edits base");
        let mut links = repo.recorded().await;
        repo.git(&["checkout", "-q", "main"]);
        repo.commit("base.txt", "main side\n", "main edits base");
        repo.git(&["checkout", "-q", "colony"]);
        repo.git_may_fail(&["rebase", "main"]);
        assert!(rebase_in_progress(&repo.git_dir()), "the conflict leaves the rebase open");
        let before = links.clone();

        assert_eq!(repo.reconcile(&mut links).await.unwrap(), Reconciled::RebaseInProgress);
        assert_eq!(links, before);
        let git = repo.builder();
        assert_eq!(
            record(&git, &repo.git_dir(), &mut links, "HEAD", "main", "colony-1", None)
                .await
                .unwrap(),
            0
        );
        assert_eq!(links, before);
    }

    #[tokio::test]
    async fn recording_twice_adds_only_new_commits() {
        let repo = Repo::new("record");
        repo.git(&["checkout", "-q", "-b", "colony"]);
        repo.commit("a.txt", "a\n", "add a");
        let mut links = repo.recorded().await;
        repo.commit("b.txt", "b\n", "add b");
        let git = repo.builder();
        let added = record(&git, &repo.git_dir(), &mut links, "HEAD", "main", "colony-1", None)
            .await
            .unwrap();
        assert_eq!(added, 1);
        assert_eq!(links.links.len(), 2);
        assert!(links.links.iter().all(|l| l.patch_id.is_some()));
    }

    // -- The triggers (issue #765): a PR head change, a sync_repo fetch, and the API.

    const BRANCH: &str = "colonizer/issue-1-c1";

    /// An App whose mirror of `acme/repo` is a bare clone of a local `origin` repository — never
    /// GitHub — with colony `c1` on [`BRANCH`] and its one commit recorded, as a publish leaves it.
    struct Mirror {
        root: PathBuf,
        app: crate::Shared,
        origin: Repo,
        first: String,
    }

    impl Drop for Mirror {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Mirror {
        async fn new(name: &str) -> Self {
            let origin = Repo::new(name);
            origin.git(&["checkout", "-q", "-b", BRANCH]);
            let first = origin.commit("a.txt", "a\n", "add a");
            let root = std::env::temp_dir().join(format!("colonizer-commit-links-app-{name}-{}", crate::util::short_id()));
            let app = crate::tests::test_app(&root);
            let bare = app.bare_repo("acme/repo");
            std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
            let clone = std::process::Command::new("git")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["clone", "-q", "--bare"])
                .arg(&origin.0)
                .arg(&bare)
                .status()
                .unwrap();
            assert!(clone.success());
            let mirror = Self {
                root,
                app,
                origin,
                first,
            };
            mirror.bare_git(&["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
            mirror.bare_git(&["fetch", "-q", "origin"]);
            let mut s = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::PrOpened);
            s.id = "c1".into();
            s.branch = BRANCH.into();
            s.base = Some("main".into());
            mirror.app.sessions.write().await.push(s);
            // What a publish records: the commit, and the head it was pushed as.
            let git = || mirror.app.git(&bare);
            let mut links = CommitLinks::default();
            let tip = format!("refs/remotes/origin/{BRANCH}");
            record(&git, &bare, &mut links, &tip, "refs/remotes/origin/main", "c1", None)
                .await
                .unwrap();
            links.tip = Some(mirror.first.clone());
            save(&mirror.app, "c1", &links).await.unwrap();
            mirror
        }

        fn bare_git(&self, args: &[&str]) {
            let out = std::process::Command::new("git")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("--git-dir")
                .arg(self.app.bare_repo("acme/repo"))
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }

        async fn links(&self) -> CommitLinks {
            load(&self.app, "c1").await.unwrap()
        }

        /// A message-only amend on origin, force-pushed as far as the mirror can tell.
        fn amend_on_origin(&self, message: &str) -> String {
            self.origin.git(&["commit", "-q", "--amend", "-m", message]);
            self.origin.git(&["rev-parse", "HEAD"])
        }
    }

    #[tokio::test]
    async fn a_pr_head_change_fetches_the_branch_and_repoints_an_amended_commit() {
        let m = Mirror::new("headmove").await;
        let amended = m.amend_on_origin("add a, reworded");
        assert_ne!(amended, m.first);

        let out = on_pr_head(&m.app, "c1", &amended).await;
        assert_eq!(
            out,
            Some(Reconciled::Done {
                repointed: 1,
                orphaned: 0
            })
        );
        let links = m.links().await;
        assert_eq!(links.links[0].sha, amended, "re-pointed at the amended commit");
        assert_eq!(links.links[0].previous, vec![m.first.clone()]);
        assert!(!links.links[0].orphaned);
        assert_eq!(links.tip.as_deref(), Some(amended.as_str()));
    }

    #[tokio::test]
    async fn an_unchanged_pr_head_is_a_no_op() {
        let m = Mirror::new("headsame").await;
        let before = std::fs::read(m.app.session_dir("c1").join(FILE)).unwrap();
        // Origin moves, but the reading is still the head already reconciled: nothing is fetched
        // or re-read, so the link is not even orphaned by the unseen rewrite.
        m.origin.commit("a.txt", "changed\n", "unrelated");
        assert_eq!(on_pr_head(&m.app, "c1", &m.first).await, None);
        assert_eq!(std::fs::read(m.app.session_dir("c1").join(FILE)).unwrap(), before);
        // And the in-memory dedup: the same head twice is news once.
        assert!(head_is_news("dedup-c1", "abc"));
        assert!(!head_is_news("dedup-c1", "abc"));
        assert!(head_is_news("dedup-c1", "def"));
    }

    #[tokio::test]
    async fn a_colony_without_links_is_never_fetched() {
        let m = Mirror::new("nolinks").await;
        std::fs::remove_file(m.app.session_dir("c1").join(FILE)).unwrap();
        let amended = m.amend_on_origin("reworded");
        assert_eq!(on_pr_head(&m.app, "c1", &amended).await, None);
        assert!(!m.app.session_dir("c1").join(FILE).exists());
    }

    #[tokio::test]
    async fn a_sync_fetch_that_moved_the_branch_repoints_and_a_second_pass_is_a_no_op() {
        let m = Mirror::new("sync").await;
        let amended = m.amend_on_origin("reworded on sync");
        m.bare_git(&["fetch", "-q", "--prune", "origin"]);
        after_sync(&m.app, "acme/repo").await;
        let links = m.links().await;
        assert_eq!(links.links[0].sha, amended);
        assert_eq!(links.tip.as_deref(), Some(amended.as_str()));
        let before = std::fs::read(m.app.session_dir("c1").join(FILE)).unwrap();
        after_sync(&m.app, "acme/repo").await;
        assert_eq!(std::fs::read(m.app.session_dir("c1").join(FILE)).unwrap(), before);
    }

    #[tokio::test]
    async fn the_api_lists_the_links_with_their_history_and_the_orphaned_flag() {
        use axum::extract::{Path as P, State};
        let m = Mirror::new("api").await;
        // A content amend: no commit carries the old patch-id any more, so the link is orphaned.
        m.origin.commit("a.txt", "a, but different\n", "tmp");
        m.origin.git(&["reset", "-q", "--soft", "HEAD~2"]);
        m.origin.git(&["commit", "-q", "-m", "squashed"]);
        let squashed = m.origin.git(&["rev-parse", "HEAD"]);
        assert_eq!(
            on_pr_head(&m.app, "c1", &squashed).await,
            Some(Reconciled::Done {
                repointed: 0,
                orphaned: 1
            })
        );
        let axum::Json(body) = api_commits(State(m.app.clone()), P("c1".into())).await.unwrap();
        let commits = body["commits"].as_array().unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0]["sha"], m.first.as_str(), "kept, not guessed");
        assert_eq!(commits[0]["orphaned"], true);
        assert_eq!(commits[0]["previous"], serde_json::json!([]));
        assert!(commits[0]["recorded_at"].is_string());

        let missing = api_commits(State(m.app.clone()), P("nope".into())).await.unwrap_err();
        assert_eq!(missing.0, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn only_plain_branch_names_reach_a_refspec() {
        assert!(plain_branch("colonizer/issue-1-c1"));
        for bad in ["", "-x", "a..b", "a:b", "a b", "a/", "/a", "a.lock", "a^", "a~1", "a*", "+a"] {
            assert!(!plain_branch(bad), "{bad:?}");
        }
    }
}
