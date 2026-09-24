//! Cross-mothership duplicate-colony guard (issue #454).
//!
//! The local guard in [`crate::sessions`] refuses a second colony on an issue another local colony
//! still holds, but a second mothership shares no memory with this one: two motherships watching
//! the same repository can each launch a colony on the same issue seconds apart, and both will do
//! the whole job. So a colony claims its issue on GitHub itself — a `colonizer:claimed` label plus
//! a comment carrying a machine-readable marker — and a launch checks for a competing claim before
//! starting: a live branch or pull request with the colony prefix, or the label.
//!
//! Everything here degrades to the local guard: any `gh` failure falls back instead of refusing a
//! launch, and publishing or releasing a claim never fails one. Claim closures passed to the
//! admission lock never await, so the async checks below always run outside it.

use crate::{
    App, Shared,
    sessions::{Session, SessionStatus},
    util::exec_within,
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// The label a running colony leaves on its issue, so a second mothership sees the claim.
pub const CLAIM_LABEL: &str = "colonizer:claimed";
/// Purple, beside findings' light blue: a claim is not a finding.
pub const CLAIM_LABEL_COLOR: &str = "5319E7";
pub const CLAIM_LABEL_DESCRIPTION: &str = "A Colonizer colony on some mothership is working this issue";

/// One remote call's deadline. A wedged `gh` must not park a launch: like `pr_info`, twenty
/// seconds, then the caller falls back to the local guard.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);

/// The branch prefix a colony on `issue` boots from: `colonizer/issue-{N}-`, with the colony id
/// after the dash. The trailing dash matters: without it issue 7 would match issue 77's branch.
pub fn branch_prefix(issue: u64) -> String {
    format!("colonizer/issue-{issue}-")
}

/// The first live branch with the colony prefix for `issue`, if any.
pub fn branch_claimed(branches: &[&str], issue: u64) -> Option<String> {
    let prefix = branch_prefix(issue);
    branches
        .iter()
        .find(|b| b.starts_with(prefix.as_str()))
        .map(|b| (*b).to_string())
}

/// Whether a pull request state blocks a new colony: open work and merged work both do (a merged
/// PR means the issue is done), while a closed-unmerged one leaves the issue free for a retry.
/// Case is tolerated, like `pr_state_from`.
fn pr_state_blocks(state: &str) -> bool {
    matches!(state.trim().to_ascii_uppercase().as_str(), "OPEN" | "MERGED")
}

/// The first pull request on `issue` that blocks a new colony: the head has the colony prefix for
/// the issue and the state is open or merged. Each entry is `(head, state, url)`.
pub fn pr_claimed<'a>(prs: &[(&'a str, &'a str, &'a str)], issue: u64) -> Option<(&'a str, &'a str, &'a str)> {
    let prefix = branch_prefix(issue);
    prs.iter()
        .find(|(head, state, _)| head.starts_with(prefix.as_str()) && pr_state_blocks(state))
        .copied()
}

/// Whether the issue carries the claim label. Exact and case-sensitive: a human label like
/// `Colonizer:Claimed` is not this mothership's claim.
pub fn label_claimed(labels: &[&str]) -> bool {
    labels.contains(&CLAIM_LABEL)
}

/// A parsed claim comment: who left it and on what issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteClaim {
    pub host: String,
    pub colony: String,
    pub issue: u64,
}

/// Escapes a marker value so a host or colony id with spaces or quotes round-trips through the
/// `"..."` quoting.
fn escape_marker_value(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The claim comment body: a machine-parseable marker line plus a human line saying the same.
pub fn format_claim(host: &str, colony: &str, issue: u64) -> String {
    let marker = format!(
        "<!-- colonizer:claim host=\"{}\" colony=\"{}\" issue=\"{issue}\" -->",
        escape_marker_value(host),
        escape_marker_value(colony),
    );
    format!(
        "{marker}\nColonizer colony `{colony}` on host `{host}` claimed issue #{issue}: starting a second colony on it duplicates its work."
    )
}

/// Every `key="value"` pair in `text`, honouring `\"` and `\\` escapes inside the quotes. Split
/// from `parse_claim` so the marker-anchored parse and the lenient fallback share it.
fn parse_pairs(text: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        if i > start && i + 1 < chars.len() && chars[i] == '=' && chars[i + 1] == '"' {
            let key: String = chars[start..i].iter().collect();
            i += 2;
            let mut value = String::new();
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    value.push(chars[i + 1]);
                    i += 2;
                } else if chars[i] == '"' {
                    closed = true;
                    i += 1;
                    break;
                } else {
                    value.push(chars[i]);
                    i += 1;
                }
            }
            if closed {
                pairs.push((key, value));
            }
        } else if i == start {
            i += 1;
        }
    }
    pairs
}

/// Builds a claim from parsed pairs: all three keys present, a numeric issue, and a non-empty
/// host and colony. The last occurrence of a repeated key wins.
fn claim_from_pairs(pairs: &[(String, String)]) -> Option<RemoteClaim> {
    let get = |key: &str| {
        pairs
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    Some(RemoteClaim {
        host: get("host")?,
        colony: get("colony")?,
        issue: get("issue")?.parse::<u64>().ok()?,
    })
}

/// Reads a claim comment back. The marker-anchored parse runs first; the lenient fallback reads
/// the same `host=`/`colony=` pairs anywhere in the body, so a hand-written comment quoting them
/// still names its colony. Anything else is `None`.
pub fn parse_claim(body: &str) -> Option<RemoteClaim> {
    if let Some(at) = body.find("colonizer:claim")
        && let Some(claim) = claim_from_pairs(&parse_pairs(&body[at..]))
    {
        return Some(claim);
    }
    claim_from_pairs(&parse_pairs(body))
}

/// Where the competing claim was found: the strongest signal first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimKind {
    PullRequest,
    Branch,
    Label,
}

/// A competing remote claim: where it was found and what names it. `host`/`colony` come from
/// parsing the claim comments, so a branch or PR match usually leaves them unknown. `merged` says
/// a PullRequest claim's pull request has landed: open work and landed work block a waiter's turn
/// differently (see [`claim_wait_conflict`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteClaimInfo {
    pub kind: ClaimKind,
    pub detail: String,
    pub host: Option<String>,
    pub colony: Option<String>,
    pub merged: bool,
}

/// The 409 text for a refused launch: the holding host and colony when the claim names them, else
/// the branch, pull request or label that blocked. Always names `allow_duplicate`, the way out.
pub fn remote_conflict_message(info: &RemoteClaimInfo, issue: u64) -> String {
    let what = match info.kind {
        ClaimKind::PullRequest => format!("its pull request is still live at {}", info.detail),
        ClaimKind::Branch => format!("its branch {} is still live", info.detail),
        ClaimKind::Label => format!("it left the `{CLAIM_LABEL}` claim on the issue"),
    };
    match (&info.host, &info.colony) {
        (Some(host), Some(colony)) => format!(
            "colony {colony} on host {host} already claimed #{issue} ({what}): starting a second colony duplicates its work. \
             Read that colony's pull request or branch first, or pass allow_duplicate to start another anyway."
        ),
        _ => format!(
            "issue #{issue} is already claimed on GitHub ({what}): a colony on another mothership is working it, \
             so starting one here duplicates its work. Pass allow_duplicate to start another anyway."
        ),
    }
}

/// The colony id a colony's branch name carries: the tail after `colonizer/issue-{N}-`.
fn branch_colony(branch: &str, issue: u64) -> Option<&str> {
    branch
        .strip_prefix(branch_prefix(issue).as_str())
        .filter(|tail| !tail.is_empty())
}

/// Issue #321: what stands between a claim_wait waiter whose turn has come and taking the issue
/// over. `None` promotes it; `Some(reason)` retires it instead, in the words a fresh launch would
/// have been refused with. A merged pull request on the issue is never tolerated — the work has
/// landed, and starting would redo it — and neither is a claim another mothership holds; a live
/// branch, open pull request or stale label of one of `our_colonies` on the issue is, since the
/// waiter is that colony's successor in its own queue.
pub fn claim_wait_conflict(info: Option<&RemoteClaimInfo>, issue: u64, our_colonies: &[&str]) -> Option<String> {
    let info = info?;
    let merged_pr = info.kind == ClaimKind::PullRequest && info.merged;
    let ours = info.colony.as_deref().is_some_and(|colony| our_colonies.contains(&colony))
        || matches!(info.kind, ClaimKind::Branch if branch_colony(&info.detail, issue).is_some_and(|c| our_colonies.contains(&c)));
    if merged_pr || !ours {
        return Some(if merged_pr {
            "the holder's pull request was merged; the issue is done".to_string()
        } else {
            remote_conflict_message(info, issue)
        });
    }
    None
}

/// Whether a launch checks GitHub for a competing claim. `allow_duplicate` skips the remote check
/// entirely, exactly like it skips the local guard.
pub fn should_check_remote(issue: Option<u64>, allow_duplicate: bool) -> bool {
    issue.is_some() && !allow_duplicate
}

/// Whether a colony that just ended should release its GitHub claim: one that finished without a
/// pull request (failed, stopped, or nothing to push, and no `pr_url`), or whose pull request
/// closed unmerged (`Closed`, even with a `pr_url` — the work is not landing). A merge leaves the
/// claim: the issue is done. Anything still running keeps it.
pub fn should_release(status: &SessionStatus, pr_url: Option<&str>) -> bool {
    match status {
        SessionStatus::Closed => true,
        SessionStatus::Failed | SessionStatus::Stopped | SessionStatus::NoChanges => pr_url.is_none(),
        _ => false,
    }
}

/// The release guard: only our own claim — or one that no longer parses — may be released, never
/// a different live colony's.
pub fn ours_or_stale(claim: Option<&RemoteClaim>, colony: &str) -> bool {
    claim.is_none_or(|c| c.colony == colony)
}

/// Maps a remote-check outcome to the claim that blocks the launch: `None` on any error, so a
/// failed lookup falls back to the local guard instead of refusing the launch. The caller logs
/// the error itself, which is why this is a separate function rather than a `?`.
pub fn remote_result_or_fallback<T, E>(result: Result<Option<T>, E>) -> Option<T> {
    result.ok().flatten()
}

/// The `comments` array of a `gh issue view --json` answer, as bodies. Unparseable output reads
/// as no comments rather than an error: the label check below still runs on the labels half.
fn comment_bodies(value: &Value) -> Vec<String> {
    value
        .get("comments")
        .and_then(Value::as_array)
        .map(|comments| {
            comments
                .iter()
                .filter_map(|c| c.get("body").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `labels` array of a `gh issue view --json` answer, as names. `gh` reports objects with a
/// `name` key; plain strings are tolerated too.
fn label_names(value: &Value) -> Vec<String> {
    value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|l| {
                    l.get("name")
                        .and_then(Value::as_str)
                        .or_else(|| l.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `gh pr list --json headRefName,state,url` answer as `(head, state, url)` triples. Rows with
/// a missing field are skipped, not fatal: one odd row must not hide a real claim.
fn pr_tuples(value: &Value) -> Vec<(String, String, String)> {
    value
        .as_array()
        .map(|prs| {
            prs.iter()
                .filter_map(|p| {
                    Some((
                        p.get("headRefName")?.as_str()?.to_string(),
                        p.get("state")?.as_str()?.to_string(),
                        p.get("url")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One line of the `gh api .../branches --jq '.[].name'` answer. The filter prints raw names, but
/// a quoted line is tolerated rather than compared with its quotes on.
fn clean_branch_line(line: &str) -> &str {
    let line = line.trim();
    line.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(line)
}

/// Looks for a competing claim on `issue`: a live pull request or branch with the colony prefix,
/// or the claim label. Any `gh` failure — no binary, no auth, a timeout, a network error — is an
/// `Err`, and the caller falls back to the local guard.
pub async fn check_remote_claim(app: &App, repo: &str, issue: u64) -> Result<Option<RemoteClaimInfo>> {
    let number = issue.to_string();
    let issue_out = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "view", number.as_str(), "-R", repo, "--json", "labels,comments"]),
    )
    .await
    .with_context(|| format!("could not read issue #{issue} on {repo}"))?;
    let issue_value: Value = serde_json::from_str(&issue_out).context("could not parse `gh issue view` output")?;

    let pr_out = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh([
            "pr",
            "list",
            "-R",
            repo,
            "--state",
            "all",
            "--limit",
            "100",
            "--json",
            "headRefName,state,url",
        ]),
    )
    .await
    .with_context(|| format!("could not list pull requests on {repo}"))?;
    let pr_value: Value = serde_json::from_str(&pr_out).context("could not parse `gh pr list` output")?;

    let branches_path = format!("repos/{repo}/branches");
    let branch_out = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["api", branches_path.as_str(), "--paginate", "--jq", ".[].name"]),
    )
    .await
    .with_context(|| format!("could not list branches on {repo}"))?;

    // The claim comment, if any, names the colony that left it — whichever signal below actually
    // matches, so the refusal names host and colony whenever the issue carries one, not only when
    // the label is the strongest signal that fired.
    let bodies = comment_bodies(&issue_value);
    let claim = bodies.iter().rev().filter_map(|body| parse_claim(body)).next();
    let (host, colony) = (
        claim.as_ref().map(|c| c.host.clone()),
        claim.as_ref().map(|c| c.colony.clone()),
    );

    // A live pull request is the strongest claim: reviewable work, wherever it runs.
    let prs = pr_tuples(&pr_value);
    let pr_refs: Vec<(&str, &str, &str)> = prs
        .iter()
        .map(|(head, state, url)| (head.as_str(), state.as_str(), url.as_str()))
        .collect();
    if let Some((_, state, url)) = pr_claimed(&pr_refs, issue) {
        return Ok(Some(RemoteClaimInfo {
            kind: ClaimKind::PullRequest,
            detail: url.to_string(),
            host,
            colony,
            merged: state.eq_ignore_ascii_case("merged"),
        }));
    }
    // Then a live branch with the colony prefix.
    let branches: Vec<&str> = branch_out
        .lines()
        .map(clean_branch_line)
        .filter(|line| !line.is_empty())
        .collect();
    if let Some(branch) = branch_claimed(&branches, issue) {
        return Ok(Some(RemoteClaimInfo {
            kind: ClaimKind::Branch,
            detail: branch,
            host,
            colony,
            merged: false,
        }));
    }
    // Then the label itself.
    let labels = label_names(&issue_value);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    if label_claimed(&label_refs) {
        return Ok(Some(RemoteClaimInfo {
            kind: ClaimKind::Label,
            detail: CLAIM_LABEL.to_string(),
            host,
            colony,
            merged: false,
        }));
    }
    Ok(None)
}

/// This mothership's host, for the claim comment: the hostname plus the stable install id, or
/// just the id where no hostname probes.
async fn host_label(app: &App) -> String {
    let id = crate::runtime::host_id(app);
    match crate::runtime::probe_hostname().await {
        Some(name) if !name.trim().is_empty() => format!("{} ({id})", name.trim()),
        _ => id,
    }
}

/// Claims `issue` for `colony`: the label (created first, best effort, like findings) plus the
/// comment carrying the machine-readable marker. Best effort throughout and never an error: a
/// failed claim must not fail the launch it runs behind.
pub async fn publish_claim(app: &App, repo: &str, issue: u64, colony: &str) {
    // A kill-switch means no external writes at all, claims included.
    if crate::authority::external_writes_blocked() {
        return;
    }
    let number = issue.to_string();
    let host = host_label(app).await;
    let body = format_claim(&host, colony, issue);
    // Best effort: the label may exist already, or the token may not be allowed to create labels —
    // the comment below carries the machine-readable claim either way.
    let _ = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh([
            "label",
            "create",
            CLAIM_LABEL,
            "-R",
            repo,
            "--color",
            CLAIM_LABEL_COLOR,
            "--description",
            CLAIM_LABEL_DESCRIPTION,
        ]),
    )
    .await;
    // The comment goes up before the label: a release racing this claim (a waiter taking over
    // from a holder that just finished, issue #321) re-reads the comments after it removes the
    // label, so either it sees this claim and puts the label back, or this label lands after
    // its removal.
    if let Err(e) = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "comment", number.as_str(), "-R", repo, "--body", body.as_str()]),
    )
    .await
    {
        eprintln!("claims: could not leave the claim comment on {repo}#{issue} for colony {colony}: {e:#}");
        app.session_log(
            colony,
            "warn",
            format!("could not claim #{issue} on GitHub ({e:#}); the local guard still refuses duplicates on this mothership"),
        )
        .await;
    }
    if let Err(e) = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "edit", number.as_str(), "-R", repo, "--add-label", CLAIM_LABEL]),
    )
    .await
    {
        eprintln!("claims: could not add the {CLAIM_LABEL} label to {repo}#{issue} for colony {colony}: {e:#}");
    }
}

/// Releases `colony`'s claim on `issue`: the label off, plus a release note. Reads the latest
/// claim comment first and never clobbers a different live colony's claim. Best effort throughout.
pub async fn release_claim(app: &App, repo: &str, issue: u64, colony: &str) {
    if crate::authority::external_writes_blocked() {
        return;
    }
    let number = issue.to_string();
    let comments = match exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "view", number.as_str(), "-R", repo, "--json", "comments"]),
    )
    .await
    {
        Ok(out) => comment_bodies(&serde_json::from_str(&out).unwrap_or(Value::Null)),
        Err(e) => {
            eprintln!("claims: could not read the comments on {repo}#{issue} to release colony {colony}'s claim: {e:#}");
            return;
        }
    };
    let latest = comments.iter().rev().filter_map(|body| parse_claim(body)).next();
    if !ours_or_stale(latest.as_ref(), colony) {
        let other = latest
            .map(|claim| format!("colony {} on host {}", claim.colony, claim.host))
            .unwrap_or_else(|| "another colony".to_string());
        eprintln!("claims: leaving the claim on {repo}#{issue} alone: it now names {other}");
        return;
    }
    // Best effort: the label may already be gone, and the comment is a courtesy.
    let _ = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "edit", number.as_str(), "-R", repo, "--remove-label", CLAIM_LABEL]),
    )
    .await;
    // A successor may have claimed the issue while this release ran (issue #321: a waiter takes
    // over the moment its holder finishes). `publish_claim` comments before it labels, so a
    // re-read after the removal either sees that claim — and the label goes back — or the
    // successor's label lands after the removal anyway.
    let reread = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "view", number.as_str(), "-R", repo, "--json", "comments"]),
    )
    .await
    .map(|out| comment_bodies(&serde_json::from_str(&out).unwrap_or(Value::Null)));
    if let Ok(bodies) = reread
        && let Some(successor) = bodies.iter().rev().filter_map(|body| parse_claim(body)).next()
        && !ours_or_stale(Some(&successor), colony)
    {
        let _ = exec_within(
            REMOTE_TIMEOUT,
            &mut app.gh(["issue", "edit", number.as_str(), "-R", repo, "--add-label", CLAIM_LABEL]),
        )
        .await;
        eprintln!(
            "claims: colony {} claimed {repo}#{issue} while colony {colony} released it; its label stays",
            successor.colony
        );
        return;
    }
    let host = host_label(app).await;
    let body = format!(
        "Colonizer colony `{colony}` on host `{host}` released issue #{issue}: it finished without a merged pull request, so the issue is free for another colony."
    );
    if let Err(e) = exec_within(
        REMOTE_TIMEOUT,
        &mut app.gh(["issue", "comment", number.as_str(), "-R", repo, "--body", body.as_str()]),
    )
    .await
    {
        eprintln!("claims: could not leave the release comment on {repo}#{issue} for colony {colony}: {e:#}");
    }
}

/// Publishes the claim off the serving path: call after the colony is admitted, never before.
pub fn spawn_publish(app: Shared, repo: String, issue: u64, colony: String) {
    tokio::spawn(async move {
        publish_claim(&app, &repo, issue, &colony).await;
    });
}

/// Releases the claim off the serving path when the colony ended freeable: no issue, or a status
/// that keeps the claim, means nothing to do.
pub fn spawn_release_if_needed(app: Shared, session: &Session) {
    let Some(issue) = session.issue else { return };
    if !should_release(&session.status, session.pr_url.as_deref()) {
        return;
    }
    let (repo, colony) = (session.repo.clone(), session.id.clone());
    tokio::spawn(async move {
        release_claim(&app, &repo, issue, &colony).await;
    });
}

/// A claim this mothership owns that no session of ours holds any more: the mark a boot reconcile
/// removes. `colony` is the id the claim comment named, so the release stays identity-guarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanedClaim {
    pub repo: String,
    pub issue: u64,
    pub colony: String,
}

/// Whether a claim's host label is this mothership's. The label is the hostname plus the stable
/// install id (see `host_label`), so the comparison runs on the id — never on a hostname, which
/// can be renamed between boots. A bare id matches too, for hosts where no hostname probes.
fn claim_is_ours(host: &str, our_host_id: &str) -> bool {
    let host = host.trim();
    host == our_host_id || host.ends_with(&format!(" ({our_host_id})"))
}

/// Whether a session of ours still keeps its claim through a restart: it holds the issue, or it
/// ended in a state whose terminal transition keeps the mark — a merged pull request (the record
/// of who did the work) or a stopped/failed colony whose pull request is still out. The boot
/// reconcile only finishes what a terminal transition would have done, never more.
fn keeps_claim(s: &Session) -> bool {
    crate::sessions::holds_issue(s) || !should_release(&s.status, s.pr_url.as_deref())
}

/// The claims in `marks` this mothership should drop: ours by host id, and no longer held — no
/// session is on that repo+issue under the claim's colony id in a state that holds the issue, so
/// a live holder keeps its mark through a restart while a stopped or failed one does not (issue
/// #321). Claims by another host are never in what this returns, whatever the sessions say.
pub fn orphaned_claims(our_host_id: &str, sessions: &[Session], marks: &[(String, RemoteClaim)]) -> Vec<OrphanedClaim> {
    marks
        .iter()
        .filter(|(_, claim)| claim_is_ours(&claim.host, our_host_id))
        .filter(|(repo, claim)| {
            !sessions
                .iter()
                .any(|s| s.id == claim.colony && s.repo == *repo && s.issue == Some(claim.issue) && keeps_claim(s))
        })
        .map(|(repo, claim)| OrphanedClaim {
            repo: repo.clone(),
            issue: claim.issue,
            colony: claim.colony.clone(),
        })
        .collect()
}

/// The `number` of every entry in a `gh issue list --json number` answer. Rows without one are
/// skipped, not fatal: one odd row must not hide the marks behind it.
fn labelled_issues(value: &Value) -> Vec<u64> {
    value
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter_map(|i| i.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default()
}

/// Boot reconcile (issue #321): read every issue still labelled claimed in the repositories this
/// mothership's colonies touch, and release the marks we own but no longer hold — a colony that
/// died while the mothership was down never got to release its own, and the label would otherwise
/// refuse every future launch on the issue forever. Claims by another host are never touched.
/// Best effort throughout, and never on the serving path: `main` spawns it once recovery has
/// settled the session list. The kill switch means no external writes at all, this included.
pub async fn reconcile_orphaned_claims(app: Shared) {
    if crate::authority::external_writes_blocked() {
        return;
    }
    let repos: Vec<String> = {
        let sessions = app.sessions.read().await;
        let mut repos: Vec<String> = sessions.iter().map(|s| s.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        repos
    };
    let our_host_id = crate::runtime::host_id(&app);
    for repo in repos {
        let out = match exec_within(
            REMOTE_TIMEOUT,
            &mut app.gh([
                "issue",
                "list",
                "-R",
                repo.as_str(),
                "--label",
                CLAIM_LABEL,
                "--state",
                "open",
                "--limit",
                "1000",
                "--json",
                "number",
            ]),
        )
        .await
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("claims: could not list claimed issues on {repo} to reconcile: {e:#}");
                continue;
            }
        };
        let mut marks = Vec::new();
        for issue in labelled_issues(&serde_json::from_str(&out).unwrap_or(Value::Null)) {
            let number = issue.to_string();
            let Ok(out) = exec_within(
                REMOTE_TIMEOUT,
                &mut app.gh(["issue", "view", number.as_str(), "-R", repo.as_str(), "--json", "comments"]),
            )
            .await
            else {
                eprintln!("claims: could not read the claim on {repo}#{issue} to reconcile it");
                continue;
            };
            let bodies = comment_bodies(&serde_json::from_str(&out).unwrap_or(Value::Null));
            if let Some(claim) = bodies.iter().rev().filter_map(|body| parse_claim(body)).next() {
                marks.push((repo.clone(), claim));
            }
        }
        let sessions = app.sessions.read().await.clone();
        for orphan in orphaned_claims(&our_host_id, &sessions, &marks) {
            // Re-checked against the live list before the release: an operator resume during the
            // scan must not have its colony's fresh claim torn off.
            let held_again = app
                .sessions
                .read()
                .await
                .iter()
                .any(|s| s.id == orphan.colony && s.repo == orphan.repo && s.issue == Some(orphan.issue) && keeps_claim(s));
            if held_again {
                continue;
            }
            eprintln!(
                "claims: releasing the orphaned claim on {}#{}, colony {} no longer holds it",
                orphan.repo, orphan.issue, orphan.colony
            );
            release_claim(&app, &orphan.repo, orphan.issue, &orphan.colony).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_prefix_matches_only_its_issue() {
        assert_eq!(branch_prefix(7), "colonizer/issue-7-");
        let branches = ["colonizer/issue-7-abc123", "main"];
        assert_eq!(branch_claimed(&branches, 7).as_deref(), Some("colonizer/issue-7-abc123"));
    }

    #[test]
    fn branch_prefix_ignores_other_branches() {
        // A different issue's branch, even one whose number starts with ours, is not a claim.
        let branches = ["colonizer/issue-77-abc123", "colonizer/issue-8-xyz", "main"];
        assert_eq!(branch_claimed(&branches, 7), None);
        assert_eq!(branch_claimed(&[], 7), None);
    }

    #[test]
    fn open_and_merged_pull_requests_block() {
        let open = [("colonizer/issue-7-abc", "OPEN", "https://github.com/acme/app/pull/1")];
        assert_eq!(pr_claimed(&open, 7), Some(open[0]));
        let merged = [("colonizer/issue-7-abc", "merged", "https://github.com/acme/app/pull/2")];
        assert_eq!(pr_claimed(&merged, 7), Some(merged[0]));
    }

    #[test]
    fn closed_unmerged_and_foreign_pull_requests_do_not_block() {
        let closed = [("colonizer/issue-7-abc", "CLOSED", "https://github.com/acme/app/pull/3")];
        assert_eq!(pr_claimed(&closed, 7), None);
        let foreign = [("colonizer/issue-77-abc", "OPEN", "https://github.com/acme/app/pull/4")];
        assert_eq!(pr_claimed(&foreign, 7), None);
        let main = [("main", "OPEN", "https://github.com/acme/app/pull/5")];
        assert_eq!(pr_claimed(&main, 7), None);
    }

    #[test]
    fn label_detection_is_exact() {
        assert!(label_claimed(&["bug", "colonizer:claimed"]));
        assert!(!label_claimed(&["bug"]));
        assert!(!label_claimed(&[]));
        // A human label with different case is not this mothership's claim.
        assert!(!label_claimed(&["Colonizer:Claimed"]));
    }

    #[test]
    fn claim_format_and_parse_round_trip() {
        for (host, colony) in [
            ("mothership", "abc123"),
            ("my host \"prod\"", "colony one"),
            ("back\\slash", "quote\"and space"),
        ] {
            let body = format_claim(host, colony, 7);
            assert_eq!(
                parse_claim(&body),
                Some(RemoteClaim {
                    host: host.to_string(),
                    colony: colony.to_string(),
                    issue: 7,
                })
            );
        }
    }

    #[test]
    fn unparseable_bodies_parse_to_nothing() {
        assert_eq!(parse_claim("just a human comment"), None);
        assert_eq!(parse_claim(""), None);
        assert_eq!(
            parse_claim("<!-- colonizer:claim host=\"\" colony=\"\" issue=\"x\" -->"),
            None
        );
        assert_eq!(parse_claim("<!-- colonizer:claim host=\"h\" -->"), None);
    }

    fn info_with_identity() -> RemoteClaimInfo {
        RemoteClaimInfo {
            kind: ClaimKind::Label,
            detail: CLAIM_LABEL.to_string(),
            host: Some("mothership (id-1)".to_string()),
            colony: Some("abc123".to_string()),
            merged: false,
        }
    }

    #[test]
    fn conflict_message_names_host_and_colony_when_known() {
        let message = remote_conflict_message(&info_with_identity(), 7);
        assert!(message.contains("abc123"), "{message}");
        assert!(message.contains("mothership (id-1)"), "{message}");
        assert!(message.contains("allow_duplicate"), "{message}");
    }

    #[test]
    fn conflict_message_falls_back_to_branch_or_pr_detail() {
        let branch = RemoteClaimInfo {
            kind: ClaimKind::Branch,
            detail: "colonizer/issue-7-abc123".to_string(),
            host: None,
            colony: None,
            merged: false,
        };
        let message = remote_conflict_message(&branch, 7);
        assert!(message.contains("colonizer/issue-7-abc123"), "{message}");
        assert!(message.contains("allow_duplicate"), "{message}");
        let pr = RemoteClaimInfo {
            kind: ClaimKind::PullRequest,
            detail: "https://github.com/acme/app/pull/9".to_string(),
            host: None,
            colony: None,
            merged: false,
        };
        let message = remote_conflict_message(&pr, 7);
        assert!(message.contains("https://github.com/acme/app/pull/9"), "{message}");
        assert!(message.contains("allow_duplicate"), "{message}");
    }

    #[test]
    fn a_waiters_turn_ends_when_the_holders_pull_request_merged_but_ours_may_stand() {
        // The holder merged: its session reads done and the gate hands the turn to the waiter,
        // but the merged pull request still blocks the issue — promoting would redo landed work,
        // so the waiter is retired with the reason a fresh launch would be refused for.
        let pr = RemoteClaimInfo {
            kind: ClaimKind::PullRequest,
            detail: "https://github.com/acme/app/pull/9".to_string(),
            host: Some("box (host-id-1)".to_string()),
            colony: Some("holder".to_string()),
            merged: true,
        };
        let reason = claim_wait_conflict(Some(&pr), 7, &["holder"]).expect("a merged pull request retires the waiter");
        assert!(reason.contains("merged") && reason.contains("done"), "{reason}");
        // The same pull request still open is our colony's own business: the waiter is its
        // successor, so it takes the turn. A stranger's claim retires it.
        let mut open = pr.clone();
        open.merged = false;
        assert_eq!(claim_wait_conflict(Some(&open), 7, &["holder"]), None);
        assert!(claim_wait_conflict(Some(&open), 7, &["another"]).is_some());
        // A branch names its colony in its tail, so a comment-less branch still reads as ours;
        // a label of ours is a stale mark the promotion will take over. No claim, no conflict.
        let branch = RemoteClaimInfo {
            kind: ClaimKind::Branch,
            detail: "colonizer/issue-7-holder".to_string(),
            host: None,
            colony: None,
            merged: false,
        };
        assert_eq!(claim_wait_conflict(Some(&branch), 7, &["holder"]), None);
        assert!(claim_wait_conflict(Some(&branch), 7, &["another"]).is_some());
        let label = RemoteClaimInfo {
            kind: ClaimKind::Label,
            detail: CLAIM_LABEL.to_string(),
            host: None,
            colony: Some("holder".to_string()),
            merged: false,
        };
        assert_eq!(claim_wait_conflict(Some(&label), 7, &["holder"]), None);
        assert_eq!(claim_wait_conflict(None, 7, &["holder"]), None);
    }

    #[test]
    fn should_release_frees_only_finished_without_a_pr_or_closed() {
        use SessionStatus::*;
        assert!(should_release(&Failed, None));
        assert!(should_release(&Stopped, None));
        assert!(should_release(&NoChanges, None));
        assert!(should_release(&Closed, Some("https://github.com/acme/app/pull/9")));
        assert!(should_release(&Closed, None));
        assert!(!should_release(&Failed, Some("https://github.com/acme/app/pull/9")));
        assert!(!should_release(&Stopped, Some("https://github.com/acme/app/pull/9")));
        assert!(!should_release(&Merged, None));
        assert!(!should_release(&PrOpened, Some("https://github.com/acme/app/pull/9")));
        assert!(!should_release(&Running, None));
        assert!(!should_release(&Publishing, None));
        assert!(!should_release(&Starting, None));
        assert!(!should_release(&Queued, None));
    }

    #[test]
    fn allow_duplicate_skips_the_remote_check() {
        assert!(should_check_remote(Some(7), false));
        assert!(!should_check_remote(Some(7), true));
        assert!(!should_check_remote(None, false));
    }

    #[test]
    fn remote_errors_fall_back_to_no_claim() {
        let err: Result<Option<RemoteClaimInfo>, anyhow::Error> = Err(anyhow::anyhow!("no gh binary"));
        assert_eq!(remote_result_or_fallback(err), None);
        let blocked: Result<Option<RemoteClaimInfo>, anyhow::Error> = Ok(Some(info_with_identity()));
        assert_eq!(remote_result_or_fallback(blocked), Some(info_with_identity()));
        let free: Result<Option<RemoteClaimInfo>, anyhow::Error> = Ok(None);
        assert_eq!(remote_result_or_fallback(free), None);
    }

    #[test]
    fn release_guard_keeps_a_different_colonys_claim() {
        let ours = RemoteClaim {
            host: "mothership".to_string(),
            colony: "abc123".to_string(),
            issue: 7,
        };
        assert!(ours_or_stale(None, "abc123"));
        assert!(ours_or_stale(Some(&ours), "abc123"));
        let theirs = RemoteClaim {
            colony: "def456".to_string(),
            ..ours
        };
        assert!(!ours_or_stale(Some(&theirs), "abc123"));
    }

    /// A mark as the boot reconcile collects them: the repo plus the claim parsed from the issue's
    /// latest claim comment.
    fn mark(repo: &str, host: &str, colony: &str) -> (String, RemoteClaim) {
        (
            repo.to_string(),
            RemoteClaim {
                host: host.to_string(),
                colony: colony.to_string(),
                issue: 7,
            },
        )
    }

    /// A session of ours on issue 7, in whatever state.
    fn holding_session(id: &str, status: SessionStatus) -> Session {
        let mut s = crate::sessions::tests::colony("acme", status);
        s.id = id.into();
        s.repo = "acme/app".into();
        s.issue = Some(7);
        s
    }

    #[test]
    fn an_unheld_claim_of_ours_is_orphaned() {
        // The hostname-plus-id label matches on the stable id; a bare id matches too, for a host
        // where no hostname probes.
        for host in ["box (host-id-1)", "host-id-1"] {
            let orphans = orphaned_claims("host-id-1", &[], &[mark("acme/app", host, "abc123")]);
            assert_eq!(
                orphans,
                vec![OrphanedClaim {
                    repo: "acme/app".into(),
                    issue: 7,
                    colony: "abc123".into()
                }],
                "{host} is this mothership, and nothing holds the issue"
            );
        }
    }

    #[test]
    fn a_live_holders_mark_survives_the_restart() {
        let sessions = vec![holding_session("abc123", SessionStatus::Running)];
        assert!(
            orphaned_claims("host-id-1", &sessions, &[mark("acme/app", "box (host-id-1)", "abc123")]).is_empty(),
            "the colony is still on the issue, so its claim stands"
        );
    }

    #[test]
    fn another_hosts_claim_is_never_touched_even_unheld() {
        // A different install id is another mothership, whatever the hostname says: the id alone
        // decides, so a renamed host cannot have its claims stripped by its neighbour.
        for host in ["other-box (host-id-2)", "box (host-id-2)", "host-id-2"] {
            assert!(
                orphaned_claims("host-id-1", &[], &[mark("acme/app", host, "abc123")]).is_empty(),
                "{host} is not us"
            );
        }
    }

    #[test]
    fn a_mark_its_terminal_transition_keeps_survives_the_reconcile() {
        // A merged pull request keeps the mark as the record of who did the work, and a stopped
        // colony whose pull request is still out keeps it too: the reconcile never drops what the
        // colony's own terminal transition would have kept.
        let merged = holding_session("abc123", SessionStatus::Merged);
        let mut stopped_with_pr = holding_session("abc123", SessionStatus::Stopped);
        stopped_with_pr.pr_url = Some("https://github.com/acme/app/pull/9".into());
        for s in [merged, stopped_with_pr] {
            assert!(
                orphaned_claims(
                    "host-id-1",
                    std::slice::from_ref(&s),
                    &[mark("acme/app", "box (host-id-1)", "abc123")]
                )
                .is_empty(),
                "a {} colony keeps its mark",
                s.status.as_str()
            );
        }
    }

    #[test]
    fn a_finished_colonys_mark_is_released_on_boot() {
        // It died — or stopped — while the mothership was down, so it never released its own
        // claim; the boot reconcile finishes the job a terminal transition would have.
        for status in [SessionStatus::Stopped, SessionStatus::Failed] {
            let sessions = vec![holding_session("abc123", status)];
            assert_eq!(
                orphaned_claims("host-id-1", &sessions, &[mark("acme/app", "box (host-id-1)", "abc123")]),
                vec![OrphanedClaim {
                    repo: "acme/app".into(),
                    issue: 7,
                    colony: "abc123".into()
                }],
                "a {} colony no longer holds its issue",
                status.as_str()
            );
        }
    }
}
