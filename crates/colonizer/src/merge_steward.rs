//! The merge steward (issue #1172): an opt-in, per-org loop that gets Colonizer's own pull requests
//! merged without a person.
//!
//! **Off by default.** An org's `auto_merge` setting is `off`, `green` or `green+rebase` (`orgs.rs`),
//! and the steward never calls GitHub for an org that has not opted in. It looks only at pull requests
//! this mothership's colonies opened (sessions with a `pr_url`), never at anyone else's.
//!
//! **One read per org per cycle.** Every few minutes, one GraphQL query per org returns each pull
//! request's state, mergeability, labels and checks. The decision is a pure function
//! ([`decide`]) of those facts, so every rule is tested without a network:
//!
//! - green, clean, not a draft, no `hold` / `do-not-merge` / `needs-human` label: merge, pinned to the
//!   head that was read (`--match-head-commit`), by the org's merge method. GitHub's own
//!   `mergeStateStatus` is the branch-protection gate: only `CLEAN` merges, so a pending or failing
//!   required check, a missing review or a merge queue is never overridden;
//! - a pull request that adds a `changelog.d/` fragment waits while a `release: vX.Y.Z` pull request is
//!   open in its repository (the release train, issue #1193): the release's changelog check would fail
//!   on a fragment that lands after it was assembled;
//! - behind or conflicting, in `green+rebase`: GitHub's update-branch first; when that conflicts, the
//!   colony is resumed with a rebase task (the watcher's `needs_rebase` flag says its own host rebase
//!   already failed);
//! - a real failing check: the colony is resumed with the failing job's name and log tail, at most
//!   [`MAX_ROUNDS`] times, and never twice for the same head;
//! - every failed job ended in under [`FAST_FAIL_SECS`] seconds with no steps, or GitHub's annotation
//!   names billing or a spending limit: the pull request is marked `ci_blocked` and no colony is
//!   spent on it. One banner per org says GitHub Actions is blocked there (`/api/status`).
//!
//! The steward waits out an open GitHub circuit breaker (`github_breaker.rs`), merges at most one pull
//! request per repository per cycle, and leaves a repository the merge train or its loop drives to
//! them. What it saw and did is served by `GET /api/merge-steward`; `POST /api/merge-steward/merge` is
//! the cockpit's **Merge now**, which asks GitHub again and refuses anything but a clean pull request.

use crate::{
    ApiResult, App, Shared, authority, client_error,
    github::{self, Mergeability},
    orgs::{self, AutoMerge},
    sessions::{Session, SessionStatus},
    util::{exec_capture, exec_within, truncate, valid_repo, write_atomic},
};
use anyhow::{Context, Result, anyhow};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};
use tokio::sync::Mutex;

/// The saved state, in the config directory.
const FILE: &str = "merge-steward.json";
/// How far apart the cycles sit.
const TICK: Duration = Duration::from_secs(5 * 60);
/// The first cycle waits, so a restart does not open with a burst of GitHub calls.
const FIRST_DELAY: Duration = Duration::from_secs(90);
/// How many times the steward resumes one colony for a failing check, and again for a rebase.
pub(crate) const MAX_ROUNDS: u32 = 2;
/// A job that failed in less than this many seconds with no steps never ran anything.
pub(crate) const FAST_FAIL_SECS: i64 = 10;
/// Pull requests per GraphQL query; an org with more is read in more than one.
const QUERY_CHUNK: usize = 40;
/// How much of a failing job's log the colony is shown.
const LOG_TAIL: usize = 3000;
const GH_LIMIT: Duration = Duration::from_secs(60);
/// Labels that keep the steward's hands off a pull request, compared case-insensitively.
const HOLD_LABELS: &[&str] = &["hold", "do-not-merge", "do not merge", "needs-human"];

// ---------------------------------------------------------------------------------------------
// The facts, and the pure decision.
// ---------------------------------------------------------------------------------------------

/// How one check or status ended, from the steward's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pass,
    Pending,
    Fail,
}

/// One check run or commit status on a pull request's head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Check {
    pub name: String,
    pub outcome: Outcome,
    /// Seconds from start to finish, for a completed check run.
    pub duration_secs: Option<i64>,
    /// How many steps the job ran; `None` for a status, or when GitHub did not say.
    pub steps: Option<u64>,
    /// The annotations GitHub attached (a billing lock says so here).
    pub annotations: Vec<String>,
    /// Where the job's page is; carries the Actions job id.
    pub url: Option<String>,
}

/// What a pull request's checks add up to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChecksVerdict {
    /// Everything finished and passed.
    Green,
    /// Something is still running.
    Pending,
    /// No check has reported on this head.
    Nothing,
    /// At least one real failure, and the failed checks.
    Failed(Vec<Check>),
    /// The failures never ran anything: Actions is blocked, not the code broken. Carries why.
    CiBlocked(String),
}

/// Whether a failed check never got to run: it ended in under [`FAST_FAIL_SECS`] seconds and ran no
/// steps. A status (no step count) is never read this way.
pub(crate) fn never_ran(c: &Check) -> bool {
    c.outcome == Outcome::Fail && c.duration_secs.is_some_and(|d| d < FAST_FAIL_SECS) && c.steps == Some(0)
}

/// Whether a check's annotations say GitHub refused to start the job over money.
pub(crate) fn billing_annotation(c: &Check) -> Option<&str> {
    const WORDS: &[&str] = &["spending limit", "payments have failed", "billing"];
    c.annotations.iter().map(String::as_str).find(|a| {
        let a = a.to_ascii_lowercase();
        WORDS.iter().any(|w| a.contains(w))
    })
}

/// Sums checks up: a billing annotation on any failure, or every failure being a job that never ran,
/// is [`ChecksVerdict::CiBlocked`]; any other failure is [`ChecksVerdict::Failed`].
pub(crate) fn judge_checks(checks: &[Check]) -> ChecksVerdict {
    if checks.is_empty() {
        return ChecksVerdict::Nothing;
    }
    let failed: Vec<Check> = checks.iter().filter(|c| c.outcome == Outcome::Fail).cloned().collect();
    if !failed.is_empty() {
        if let Some(note) = failed.iter().find_map(billing_annotation) {
            return ChecksVerdict::CiBlocked(format!("GitHub says: {}", truncate(note, 160)));
        }
        if failed.iter().all(never_ran) {
            return ChecksVerdict::CiBlocked(format!(
                "every failed job ended in under {FAST_FAIL_SECS}s with no steps ({})",
                failed.iter().map(|c| c.name.as_str()).take(3).collect::<Vec<_>>().join(", ")
            ));
        }
        return ChecksVerdict::Failed(failed);
    }
    if checks.iter().any(|c| c.outcome == Outcome::Pending) {
        ChecksVerdict::Pending
    } else {
        ChecksVerdict::Green
    }
}

/// What one GraphQL read says about one pull request.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub url: String,
    pub title: String,
    pub open: bool,
    pub draft: bool,
    pub cross_repository: bool,
    pub labels: Vec<String>,
    pub mergeability: Mergeability,
    /// GitHub's `mergeStateStatus`, uppercased: `CLEAN` is the only one that merges.
    pub merge_state: String,
    /// GitHub's `reviewDecision`, uppercased, when the repository asks for reviews.
    pub review: Option<String>,
    pub head: String,
    /// The pull request adds or edits a fragment under `changelog.d/` (the first 100 files it touches).
    pub adds_fragment: bool,
    pub checks: Vec<Check>,
}

/// What the steward remembers about a colony and its pull request, handed to [`decide`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Ctx {
    pub mode: AutoMerge,
    /// The colony is finished with its run (`pr_opened`), so it may be resumed or merged.
    pub colony_idle: bool,
    /// The colony's record says a person must decide (an open question, or a superseding merge).
    pub needs_human: bool,
    /// The watcher's own rebase failed and flagged the colony (`needs_rebase`).
    pub rebase_flagged: bool,
    pub fix_rounds: u32,
    pub rebase_rounds: u32,
    /// The head a fix colony was last sent for.
    pub fixed_head: Option<String>,
    /// The head update-branch was last tried on.
    pub update_tried_head: Option<String>,
    /// A `release: vX.Y.Z` pull request is open in this repository (the release train's freeze).
    pub release_open: bool,
}

/// What the steward does about one pull request this cycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Not the steward's to touch (off, closed).
    Skip(String),
    /// Nothing to do yet, and why.
    Wait(String),
    Merge,
    UpdateBranch,
    ResumeRebase,
    ResumeFix(Vec<Check>),
    CiBlocked(String),
    /// A person has to look: bounded rounds spent, or a colony that fixed nothing.
    NeedsAttention(String),
}

/// Whether a label keeps the steward away.
pub(crate) fn is_hold_label(label: &str) -> bool {
    let label = label.trim();
    HOLD_LABELS.iter().any(|h| label.eq_ignore_ascii_case(h))
}

/// The decision: pull request facts and the steward's memory in, one action out. Every rule that
/// keeps a merge from happening comes before the one that allows it, and a merge needs GitHub itself
/// to say the pull request is `CLEAN` — branch protection is never second-guessed.
pub(crate) fn decide(f: &Facts, ctx: &Ctx) -> Action {
    if ctx.mode == AutoMerge::Off {
        return Action::Skip("auto_merge is off for this org".into());
    }
    if !f.open {
        return Action::Skip("the pull request is not open".into());
    }
    if f.cross_repository {
        return Action::Wait("it comes from a fork, which the steward never merges".into());
    }
    if f.draft {
        return Action::Wait("it is a draft".into());
    }
    if let Some(label) = f.labels.iter().find(|l| is_hold_label(l)) {
        return Action::Wait(format!("it carries the {label:?} label"));
    }
    if ctx.release_open && f.adds_fragment {
        return Action::Wait("a release pull request is open, and this one adds a changelog fragment".into());
    }
    if ctx.needs_human {
        return Action::Wait("the colony marked it as needing a person's decision".into());
    }
    if !ctx.colony_idle {
        return Action::Wait("its colony is working on it".into());
    }
    match f.mergeability {
        Mergeability::Conflicted | Mergeability::Behind => return stale_branch(f, ctx),
        Mergeability::Unknown => return Action::Wait("GitHub is still working out whether it merges".into()),
        Mergeability::Clean => {}
    }
    match judge_checks(&f.checks) {
        ChecksVerdict::Failed(failed) => {
            if ctx.fixed_head.as_deref() == Some(f.head.as_str()) {
                return Action::NeedsAttention("the colony was resumed for a failing check and pushed nothing".into());
            }
            if ctx.fix_rounds >= MAX_ROUNDS {
                return Action::NeedsAttention(format!("{MAX_ROUNDS} rounds of fixing a failing check did not turn it green"));
            }
            Action::ResumeFix(failed)
        }
        ChecksVerdict::CiBlocked(why) => Action::CiBlocked(why),
        ChecksVerdict::Pending => Action::Wait("its checks are still running".into()),
        ChecksVerdict::Nothing => Action::Wait("no check has reported on its head yet".into()),
        ChecksVerdict::Green => {
            if f.review.as_deref() == Some("CHANGES_REQUESTED") {
                return Action::Wait("a reviewer requested changes".into());
            }
            match f.merge_state.as_str() {
                "CLEAN" | "HAS_HOOKS" => Action::Merge,
                other => Action::Wait(format!(
                    "GitHub reports {other}: branch protection still wants a required check or review"
                )),
            }
        }
    }
}

/// Behind or conflicting: only `green+rebase` brings it up to date, GitHub first, then the colony.
fn stale_branch(f: &Facts, ctx: &Ctx) -> Action {
    let what = if f.mergeability == Mergeability::Conflicted {
        "conflicts with its base"
    } else {
        "is behind its base"
    };
    if ctx.mode != AutoMerge::GreenRebase {
        return Action::Wait(format!("it {what}; auto_merge is `green`, so a person rebases it"));
    }
    if ctx.update_tried_head.as_deref() != Some(f.head.as_str()) {
        return Action::UpdateBranch;
    }
    if ctx.rebase_rounds >= MAX_ROUNDS {
        return Action::NeedsAttention(format!("{MAX_ROUNDS} rounds of rebasing did not bring it up to date"));
    }
    if !ctx.rebase_flagged {
        return Action::Wait(format!("it {what}; the watcher's own rebase goes first"));
    }
    Action::ResumeRebase
}

// ---------------------------------------------------------------------------------------------
// Reading GitHub: one GraphQL query per org.
// ---------------------------------------------------------------------------------------------

/// `(owner/name, number)` from a pull request URL.
pub(crate) fn parse_pr_url(url: &str) -> Option<(String, u64)> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (owner, name, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if kind != "pull" || parts.next().is_some() {
        return None;
    }
    let repo = format!("{owner}/{name}");
    valid_repo(&repo).then_some((repo, number.parse().ok()?))
}

const PR_FRAGMENT: &str = "fragment F on PullRequest{number url title state isDraft isCrossRepository mergeable mergeStateStatus \
    reviewDecision headRefOid labels(first:20){nodes{name}} files(first:100){nodes{path}} \
    commits(last:1){nodes{commit{statusCheckRollup{contexts(first:60){nodes{__typename \
    ...on CheckRun{name status conclusion startedAt completedAt detailsUrl steps(first:1){totalCount} \
    annotations(first:5){nodes{title message}}} \
    ...on StatusContext{context state description targetUrl}}}}}}}}";

/// The one query for a set of pull requests, an alias each. Repository names come from
/// [`parse_pr_url`], which only lets `valid_repo` characters through, so nothing here needs escaping.
pub(crate) fn build_query(prs: &[(String, u64)]) -> String {
    let mut q = String::from("query{");
    for (i, (repo, number)) in prs.iter().enumerate() {
        let (owner, name) = repo.split_once('/').unwrap_or_default();
        q.push_str(&format!(
            "p{i}:repository(owner:\"{owner}\",name:\"{name}\"){{pullRequest(number:{number}){{...F}}}} "
        ));
    }
    q.push('}');
    q.push_str(PR_FRAGMENT);
    q
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

fn secs_between(start: &Value, end: &Value) -> Option<i64> {
    let parse = |v: &Value| DateTime::parse_from_rfc3339(v.as_str()?).ok();
    Some((parse(end)? - parse(start)?).num_seconds())
}

/// One entry of `contexts` as a [`Check`]; `None` for a kind the steward does not read.
pub(crate) fn parse_check(node: &Value) -> Option<Check> {
    match node["__typename"].as_str()? {
        "CheckRun" => {
            let status = text(&node["status"]).to_ascii_uppercase();
            let conclusion = text(&node["conclusion"]).to_ascii_uppercase();
            let outcome = if status != "COMPLETED" || conclusion.is_empty() {
                Outcome::Pending
            } else {
                match conclusion.as_str() {
                    "SUCCESS" | "NEUTRAL" | "SKIPPED" | "STALE" => Outcome::Pass,
                    _ => Outcome::Fail,
                }
            };
            Some(Check {
                name: text(&node["name"]),
                outcome,
                duration_secs: secs_between(&node["startedAt"], &node["completedAt"]),
                steps: node["steps"]["totalCount"].as_u64(),
                annotations: node["annotations"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| format!("{} {}", text(&a["title"]), text(&a["message"])).trim().to_string())
                    .filter(|a| !a.is_empty())
                    .collect(),
                url: Some(text(&node["detailsUrl"])).filter(|u| !u.is_empty()),
            })
        }
        "StatusContext" => {
            let outcome = match text(&node["state"]).to_ascii_uppercase().as_str() {
                "SUCCESS" => Outcome::Pass,
                "FAILURE" | "ERROR" => Outcome::Fail,
                _ => Outcome::Pending,
            };
            Some(Check {
                name: text(&node["context"]),
                outcome,
                duration_secs: None,
                steps: None,
                annotations: Vec::new(),
                url: Some(text(&node["targetUrl"])).filter(|u| !u.is_empty()),
            })
        }
        _ => None,
    }
}

/// Whether a changed path is a changelog fragment (`changelog.d/` holds a README too).
pub(crate) fn is_fragment_path(path: &str) -> bool {
    path.strip_prefix("changelog.d/")
        .is_some_and(|rest| !rest.contains('/') && rest != "README.md" && rest != ".gitkeep")
}

/// Whether a release pull request (`release: vX.Y.Z`) is open in `repo`. A failed read says yes: a
/// pull request held a cycle too long costs nothing, one merged under a release breaks its check.
async fn release_pr_open(app: &App, repo: &str) -> bool {
    let mut cmd = app.gh([
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--search",
        "release: in:title",
        "--json",
        "title",
        "--limit",
        "30",
    ]);
    match exec_capture(GH_LIMIT, &mut cmd).await {
        Ok((stdout, _)) => serde_json::from_str::<Value>(&stdout)
            .map(|v| {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .any(|p| is_release_title(&text(&p["title"])))
            })
            .unwrap_or(true),
        Err(e) => {
            eprintln!("merge steward: could not look for an open release pull request in {repo}: {e:#}");
            true
        }
    }
}

/// `release: vX.Y.Z`, the title the release train gives its pull request.
pub(crate) fn is_release_title(title: &str) -> bool {
    let Some(v) = title.trim().strip_prefix("release: v") else {
        return false;
    };
    let v = v.split_whitespace().next().unwrap_or_default();
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// One pull request node as [`Facts`]; `None` when GitHub returned no such pull request.
pub(crate) fn parse_pr(node: &Value) -> Option<Facts> {
    let url = text(&node["url"]);
    if url.is_empty() {
        return None;
    }
    let contexts = &node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"];
    let review = text(&node["reviewDecision"]).to_ascii_uppercase();
    let mergeable = node["mergeable"].as_str();
    let merge_state = text(&node["mergeStateStatus"]).to_ascii_uppercase();
    Some(Facts {
        url,
        title: text(&node["title"]),
        open: text(&node["state"]).eq_ignore_ascii_case("OPEN"),
        draft: node["isDraft"].as_bool().unwrap_or(false),
        // Fails closed: a field GitHub leaves out reads as a fork.
        cross_repository: node["isCrossRepository"].as_bool().unwrap_or(true),
        labels: node["labels"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| text(&l["name"]))
            .collect(),
        mergeability: github::mergeability_from(mergeable, Some(&merge_state)),
        merge_state: if merge_state.is_empty() {
            "UNKNOWN".into()
        } else {
            merge_state
        },
        review: Some(review).filter(|r| !r.is_empty()),
        head: text(&node["headRefOid"]),
        adds_fragment: node["files"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|n| is_fragment_path(&text(&n["path"]))),
        checks: contexts.as_array().into_iter().flatten().filter_map(parse_check).collect(),
    })
}

/// Reads a set of pull requests with one query (chunked past [`QUERY_CHUNK`]), `url → facts`. A pull
/// request GitHub does not return is simply absent.
async fn read_prs(app: &App, urls: &[String]) -> Result<HashMap<String, Facts>> {
    let mut out = HashMap::new();
    for chunk in urls.chunks(QUERY_CHUNK) {
        let parsed: Vec<(String, u64)> = chunk.iter().filter_map(|u| parse_pr_url(u)).collect();
        if parsed.is_empty() {
            continue;
        }
        let mut cmd = app.gh(["api", "graphql"]);
        cmd.args(["-f", &format!("query={}", build_query(&parsed))]);
        let (stdout, stderr) = exec_capture(GH_LIMIT, &mut cmd).await?;
        let body: Value = serde_json::from_str(&stdout)
            .map_err(|_| anyhow!("GitHub's GraphQL answer did not parse: {}", truncate(&stderr, 200)))?;
        let Some(data) = body["data"].as_object() else {
            return Err(anyhow!(
                "GitHub's GraphQL answer carried no data: {}",
                truncate(&body["errors"].to_string(), 200)
            ));
        };
        for node in data.values() {
            if let Some(facts) = parse_pr(&node["pullRequest"]) {
                out.insert(facts.url.clone(), facts);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// What it remembers, and what the cockpit is shown.
// ---------------------------------------------------------------------------------------------

/// A pull request's state as the cockpit names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    #[default]
    Waiting,
    Merging,
    Rebasing,
    Fixing,
    CiBlocked,
    NeedsAttention,
}

/// What the last cycle concluded about one pull request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PrMemory {
    pub session: String,
    pub org: String,
    pub repo: String,
    pub title: String,
    pub state: Phase,
    pub reason: String,
    pub head: String,
    pub at: DateTime<Utc>,
    /// The head update-branch was last tried on.
    #[serde(default)]
    pub update_tried_head: Option<String>,
}

/// The bounded rounds spent on one colony.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Rounds {
    pub fix: u32,
    pub rebase: u32,
    /// The head the last fix round was sent for.
    #[serde(default)]
    pub fix_head: Option<String>,
}

/// One org's GitHub Actions lock.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CiBlock {
    pub since: DateTime<Utc>,
    pub reason: String,
    pub prs: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Memory {
    /// By pull request URL.
    pub prs: BTreeMap<String, PrMemory>,
    /// By colony id.
    pub rounds: BTreeMap<String, Rounds>,
    /// By org.
    pub ci_blocked: BTreeMap<String, CiBlock>,
    pub last_cycle: Option<DateTime<Utc>>,
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The saved memory; a missing or unreadable file reads as empty.
pub(crate) async fn load(dir: &Path) -> Memory {
    match tokio::fs::read(file(dir)).await {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
            eprintln!("merge steward: could not parse {}: {e}; starting empty", file(dir).display());
            Memory::default()
        }),
        Err(_) => Memory::default(),
    }
}

/// Serialises every read-modify-write of the file.
static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

async fn update<R>(dir: &Path, f: impl FnOnce(&mut Memory) -> R) -> Result<R> {
    let _guard = LOCK.lock().await;
    let mut memory = load(dir).await;
    let r = f(&mut memory);
    write_atomic(&file(dir), &serde_json::to_vec_pretty(&memory)?).await?;
    Ok(r)
}

/// The sentence the org banner shows.
pub(crate) fn banner_message(org: &str) -> String {
    format!("GitHub Actions is blocked for {org} (billing or spending limit); its checks fail without running.")
}

/// The `merge_steward` key of `/api/status`: one entry per org whose Actions are blocked.
pub(crate) async fn status_json(app: &App) -> Value {
    let memory = load(&app.cfg.config_dir).await;
    json!({
        "ci_blocked": memory.ci_blocked.iter().map(|(org, b)| json!({
            "org": org,
            "since": b.since,
            "prs": b.prs.len(),
            "reason": b.reason,
            "message": banner_message(org),
        })).collect::<Vec<_>>(),
    })
}

/// A session whose pull request the cockpit lists: one a colony opened that is not yet merged or closed.
fn listed(s: &Session) -> bool {
    s.pr_url.is_some()
        && !matches!(
            s.status,
            SessionStatus::Merged | SessionStatus::Closed | SessionStatus::NoChanges | SessionStatus::Failed
        )
}

/// One row of the cockpit's Pull requests list.
pub(crate) fn row(s: &Session, mode: AutoMerge, memory: Option<&PrMemory>) -> Value {
    let (state, reason, since) = match memory {
        Some(m) => (m.state, m.reason.clone(), Some(m.at)),
        None if mode == AutoMerge::Off => (Phase::Waiting, "auto_merge is off for this org".to_string(), None),
        None => (Phase::Waiting, "not read yet".to_string(), None),
    };
    json!({
        "session": s.id,
        "repo": s.repo,
        "url": s.pr_url,
        "title": if s.issue_title.is_empty() { s.summary.clone().unwrap_or_default() } else { s.issue_title.clone() },
        "colony_status": s.status.as_str(),
        "state": state,
        "reason": reason,
        "since": since,
    })
}

async fn get_status(State(app): State<Shared>) -> ApiResult<Value> {
    let memory = load(&app.cfg.config_dir).await;
    let sessions = app.sessions.read().await.clone();
    let mut orgs: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut modes: BTreeMap<String, AutoMerge> = BTreeMap::new();
    for s in sessions.iter().filter(|s| listed(s)) {
        let mode = *modes
            .entry(s.org.clone())
            .or_insert_with(|| orgs::auto_merge_mode(&app.org_settings(&s.org)));
        let mem = s.pr_url.as_ref().and_then(|u| memory.prs.get(u));
        orgs.entry(s.org.clone()).or_default().push(row(s, mode, mem));
    }
    Ok(Json(json!({
        "orgs": orgs.into_iter().map(|(org, prs)| json!({
            "org": org,
            "auto_merge": modes.get(&org).copied().unwrap_or_default(),
            "ci_blocked": memory.ci_blocked.get(&org).map(|b| json!({
                "since": b.since,
                "reason": b.reason,
                "message": banner_message(&org),
            })),
            "prs": prs,
        })).collect::<Vec<_>>(),
        "last_cycle": memory.last_cycle,
    })))
}

// ---------------------------------------------------------------------------------------------
// Acting.
// ---------------------------------------------------------------------------------------------

/// The `gh pr merge` invocation: the org's method, pinned to the head that was read so a push landing
/// in between cannot ride through, and `--delete-branch` only when asked for and nothing is stacked on
/// the branch. `auto` asks GitHub to merge when its own requirements are met (a merge queue).
pub(crate) fn merge_args(url: &str, head: &str, method: orgs::MergeMethod, delete_branch: bool, auto: bool) -> Vec<String> {
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        url.to_string(),
        method.flag().to_string(),
        "--match-head-commit".to_string(),
        head.to_string(),
    ];
    if delete_branch {
        args.push("--delete-branch".to_string());
    }
    if auto {
        args.push("--auto".to_string());
    }
    args
}

/// Whether a failed merge says the repository wants GitHub's own auto-merge (a merge queue).
pub(crate) fn wants_auto_merge(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("merge queue") || e.contains("auto-merge") || e.contains("auto merge")
}

/// Whether a failed update-branch is a conflict (the colony has to resolve it) rather than a
/// transient failure worth another cycle.
pub(crate) fn is_conflict(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("merge conflict") || e.contains("conflict")
}

/// The Actions job id in a check's URL (`…/actions/runs/<run>/job/<job>`).
pub(crate) fn job_id(url: &str) -> Option<u64> {
    url.split("/job/").nth(1)?.split(['/', '?', '#']).next()?.parse().ok()
}

/// The last `max` characters of a log.
pub(crate) fn log_tail(log: &str, max: usize) -> String {
    let chars: Vec<char> = log.trim_end().chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// The note a colony is resumed with to fix a failing check: which jobs failed, and the end of each log.
pub(crate) fn fix_note(url: &str, jobs: &[(String, String)], round: u32) -> String {
    let mut note = format!(
        "Your pull request {url} has a failing check, found by the merge steward (round {round} of {MAX_ROUNDS}). \
         Fix the cause on your branch, run this repo's gates, and push; do not skip or weaken the check.\n"
    );
    for (name, tail) in jobs {
        note.push_str(&format!("\n### Failing job: {name}\n"));
        if tail.trim().is_empty() {
            note.push_str("(no log could be read; open the job on the pull request)\n");
        } else {
            note.push_str(&format!("```\n{tail}\n```\n"));
        }
    }
    note
}

/// The note a colony is resumed with to rebase.
pub(crate) fn rebase_note(url: &str, base: &str, round: u32) -> String {
    format!(
        "Your pull request {url} no longer merges cleanly into `{base}` and GitHub's update-branch could not fix it \
         (the merge steward, round {round} of {MAX_ROUNDS}). Fetch, rebase onto `origin/{base}`, resolve the conflicts \
         (changelog fragments and generated files are mechanical; take both sides), re-run this repo's gates, and \
         force-push your branch."
    )
}

/// Resumes a finished colony with a one-shot note, the way the merge-train loop resolves a conflict:
/// `pr_opened` becomes `stopped` (the resumable shape) with the note on its record and its autopilot
/// on, so the run publishes to the same pull request itself; a refused resume puts the status back.
async fn resume_with_note(app: &Shared, id: &str, note: String) -> Result<()> {
    let flipped = app
        .update_session(id, |x| {
            if x.status != SessionStatus::PrOpened {
                return false;
            }
            x.status = SessionStatus::Stopped;
            x.autopilot = true;
            x.resume_note = Some(note.clone());
            true
        })
        .await
        .is_some_and(|(_, ok)| ok);
    if !flipped {
        return Err(anyhow!("the colony moved on before it could be resumed"));
    }
    if let Err(e) = crate::lifecycle::resume(State(app.clone()), axum::extract::Path(id.to_string()), None).await {
        app.update_session(id, |x| {
            if x.status == SessionStatus::Stopped && x.pr_url.is_some() {
                x.status = SessionStatus::PrOpened;
                x.resume_note = None;
            }
        })
        .await;
        return Err(anyhow!("the colony could not be resumed ({})", e.message()));
    }
    Ok(())
}

/// The tail of one failing job's log, best effort.
async fn job_log_tail(app: &App, repo: &str, check: &Check) -> String {
    let Some(job) = check.url.as_deref().and_then(job_id) else {
        return check.annotations.first().cloned().unwrap_or_default();
    };
    let mut cmd = app.gh(["api", &format!("repos/{repo}/actions/jobs/{job}/logs")]);
    match exec_within(GH_LIMIT, &mut cmd).await {
        Ok(log) => crate::redact::redact_text(&log_tail(&log, LOG_TAIL)).into_owned(),
        Err(_) => check.annotations.first().cloned().unwrap_or_default(),
    }
}

/// What one merge attempt came to.
enum Merged {
    Yes,
    /// GitHub accepted it as auto-merge (a merge queue); it lands when its own requirements do.
    Queued,
}

async fn merge(app: &App, s: &Session, f: &Facts, settings: &orgs::OrgSettings, keep_branch: bool) -> Result<Merged> {
    let delete = orgs::deletes_merged_branch(settings) && !keep_branch;
    let method = orgs::merge_method(settings);
    match exec_within(GH_LIMIT, &mut app.gh(merge_args(&f.url, &f.head, method, delete, false))).await {
        Ok(_) => Ok(Merged::Yes),
        Err(e) if wants_auto_merge(&format!("{e:#}")) => {
            exec_within(GH_LIMIT, &mut app.gh(merge_args(&f.url, &f.head, method, delete, true)))
                .await
                .map(|_| Merged::Queued)
                .with_context(|| format!("auto-merge for {}", s.repo))
        }
        Err(e) => Err(e),
    }
}

/// GitHub's update-branch, pinned to the head that was read.
async fn update_branch(app: &App, repo: &str, number: u64, head: &str) -> Result<()> {
    let mut cmd = app.gh([
        "api".to_string(),
        "-X".into(),
        "PUT".into(),
        format!("repos/{repo}/pulls/{number}/update-branch"),
        "-f".into(),
        format!("expected_head_sha={head}"),
    ]);
    exec_within(GH_LIMIT, &mut cmd).await.map(|_| ())
}

/// The activity line for something the steward did to a colony's pull request.
async fn record(app: &App, s: &Session, what: &str) {
    let mut entry = crate::activity::Entry::new("publish.merge_steward", "colony").colony(s);
    entry.detail = Some(what.to_string());
    crate::activity::record(app, entry).await;
}

fn pr_memory(s: &Session, f: &Facts, state: Phase, reason: String, update_tried: Option<String>) -> PrMemory {
    PrMemory {
        session: s.id.clone(),
        org: s.org.clone(),
        repo: s.repo.clone(),
        title: f.title.clone(),
        state,
        reason,
        head: f.head.clone(),
        at: Utc::now(),
        update_tried_head: update_tried,
    }
}

/// One org's pull requests for one cycle, read with one query and acted on.
async fn steward_org(app: &Shared, org: &str, settings: &orgs::OrgSettings, candidates: Vec<Session>, sessions: &[Session]) {
    let mode = orgs::auto_merge_mode(settings);
    let urls: Vec<String> = candidates.iter().filter_map(|s| s.pr_url.clone()).collect();
    let facts = match read_prs(app, &urls).await {
        Ok(facts) => facts,
        Err(e) => {
            eprintln!("merge steward: could not read {org}'s pull requests: {e:#}");
            return;
        }
    };
    let dir = app.cfg.config_dir.clone();
    let memory = load(&dir).await;
    let mut merged_repos: HashSet<String> = HashSet::new();
    // Per repository, looked up once and only for a pull request that adds a fragment.
    let mut release_open: HashMap<String, bool> = HashMap::new();
    let mut results: Vec<(String, PrMemory)> = Vec::new();
    let mut bumped: Vec<(String, Rounds)> = Vec::new();
    for s in &candidates {
        let Some(url) = s.pr_url.clone() else { continue };
        let Some(f) = facts.get(&url) else { continue };
        let saved = memory.prs.get(&url);
        let rounds = memory.rounds.get(&s.id).cloned().unwrap_or_default();
        if f.adds_fragment && !release_open.contains_key(&s.repo) {
            let open = release_pr_open(app, &s.repo).await;
            release_open.insert(s.repo.clone(), open);
        }
        let mut ctx = Ctx {
            mode,
            release_open: release_open.get(&s.repo).copied().unwrap_or(false),
            colony_idle: s.status == SessionStatus::PrOpened,
            needs_human: s.pending_answer.is_some() || s.attention.is_some() || s.superseded.as_ref().is_some_and(|x| !x.kept),
            rebase_flagged: s.needs_rebase,
            fix_rounds: rounds.fix,
            rebase_rounds: rounds.rebase,
            fixed_head: rounds.fix_head.clone(),
            update_tried_head: saved.and_then(|m| m.update_tried_head.clone()),
        };
        let mut update_tried = ctx.update_tried_head.clone();
        let mut updated_rounds = rounds.clone();
        let mut action = decide(f, &ctx);
        // Update-branch is tried inline, and a conflict decides again with that fact in hand.
        if action == Action::UpdateBranch {
            let number = parse_pr_url(&url).map(|(_, n)| n).unwrap_or_default();
            update_tried = Some(f.head.clone());
            match update_branch(app, &s.repo, number, &f.head).await {
                Ok(()) => {
                    app.session_log(&s.id, "info", "merge steward: asked GitHub to update the branch".into())
                        .await;
                    results.push((
                        url.clone(),
                        pr_memory(
                            s,
                            f,
                            Phase::Rebasing,
                            "GitHub is updating the branch from its base".into(),
                            update_tried,
                        ),
                    ));
                    continue;
                }
                Err(e) if is_conflict(&format!("{e:#}")) => {
                    ctx.update_tried_head = update_tried.clone();
                    action = decide(f, &ctx);
                }
                Err(e) => {
                    eprintln!("merge steward: update-branch on {url} failed: {e:#}");
                    results.push((
                        url.clone(),
                        pr_memory(
                            s,
                            f,
                            Phase::Waiting,
                            "update-branch failed; will try again".into(),
                            saved.and_then(|m| m.update_tried_head.clone()),
                        ),
                    ));
                    continue;
                }
            }
        }
        let (state, reason) = match action {
            Action::Skip(why) | Action::Wait(why) => (Phase::Waiting, why),
            Action::NeedsAttention(why) => (Phase::NeedsAttention, why),
            Action::CiBlocked(why) => {
                app.session_log(&s.id, "warn", format!("merge steward: GitHub Actions looks blocked: {why}"))
                    .await;
                (Phase::CiBlocked, why)
            }
            Action::Merge => {
                if merged_repos.contains(&s.repo) {
                    (
                        Phase::Waiting,
                        "another pull request of this repository merged this cycle".into(),
                    )
                } else if authority::external_writes_blocked() {
                    (Phase::Waiting, crate::publish::BLOCKED.to_string())
                } else {
                    let keep = crate::merge_train::has_stacked_child(sessions, s);
                    match merge(app, s, f, settings, keep).await {
                        Ok(done) => {
                            merged_repos.insert(s.repo.clone());
                            let what = match done {
                                Merged::Yes => "merged by the merge steward",
                                Merged::Queued => "queued for auto-merge by the merge steward",
                            };
                            app.session_log(&s.id, "info", format!("{what} ({})", f.head)).await;
                            record(app, s, what).await;
                            (Phase::Merging, what.to_string())
                        }
                        Err(e) => {
                            let reason = format!("the merge was refused: {}", truncate(&format!("{e:#}"), 200));
                            app.session_log(&s.id, "warn", format!("merge steward: {reason}")).await;
                            (Phase::Waiting, reason)
                        }
                    }
                }
            }
            Action::ResumeFix(failed) => {
                if authority::external_writes_blocked() {
                    (Phase::Waiting, crate::publish::BLOCKED.to_string())
                } else {
                    let mut jobs = Vec::new();
                    for c in failed.iter().take(3) {
                        jobs.push((c.name.clone(), job_log_tail(app, &s.repo, c).await));
                    }
                    let round = rounds.fix + 1;
                    match resume_with_note(app, &s.id, fix_note(&url, &jobs, round)).await {
                        Ok(()) => {
                            updated_rounds.fix = round;
                            updated_rounds.fix_head = Some(f.head.clone());
                            let names = failed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ");
                            app.session_log(
                                &s.id,
                                "info",
                                format!("merge steward: resumed to fix failing checks ({names}), round {round}"),
                            )
                            .await;
                            record(app, s, &format!("resumed to fix failing checks, round {round}")).await;
                            (Phase::Fixing, format!("round {round} of {MAX_ROUNDS}: {names}"))
                        }
                        Err(e) => (Phase::Waiting, format!("{e:#}")),
                    }
                }
            }
            Action::ResumeRebase => {
                if authority::external_writes_blocked() {
                    (Phase::Waiting, crate::publish::BLOCKED.to_string())
                } else {
                    let round = rounds.rebase + 1;
                    let base = s.base.clone().unwrap_or_else(|| "main".into());
                    match resume_with_note(app, &s.id, rebase_note(&url, &base, round)).await {
                        Ok(()) => {
                            updated_rounds.rebase = round;
                            app.session_log(
                                &s.id,
                                "info",
                                format!("merge steward: resumed to rebase onto {base}, round {round}"),
                            )
                            .await;
                            record(app, s, &format!("resumed to rebase, round {round}")).await;
                            (Phase::Rebasing, format!("round {round} of {MAX_ROUNDS}"))
                        }
                        Err(e) => (Phase::Waiting, format!("{e:#}")),
                    }
                }
            }
            Action::UpdateBranch => (Phase::Waiting, "update-branch is pending".into()),
        };
        if updated_rounds.fix != rounds.fix || updated_rounds.rebase != rounds.rebase {
            bumped.push((s.id.clone(), updated_rounds));
        }
        results.push((url, pr_memory(s, f, state, reason, update_tried)));
    }
    let blocked: Vec<String> = results
        .iter()
        .filter(|(_, m)| m.state == Phase::CiBlocked)
        .map(|(u, _)| u.clone())
        .collect();
    let blocked_reason = results
        .iter()
        .find(|(_, m)| m.state == Phase::CiBlocked)
        .map(|(_, m)| m.reason.clone());
    let org = org.to_string();
    let wrote = update(&dir, move |m| {
        for (url, mem) in results {
            m.prs.insert(url, mem);
        }
        for (id, r) in bumped {
            m.rounds.insert(id, r);
        }
        match blocked_reason {
            Some(reason) => {
                let since = m.ci_blocked.get(&org).map_or_else(Utc::now, |b| b.since);
                m.ci_blocked.insert(
                    org,
                    CiBlock {
                        since,
                        reason,
                        prs: blocked,
                    },
                );
            }
            None => {
                m.ci_blocked.remove(&org);
            }
        }
    })
    .await;
    if let Err(e) = wrote {
        eprintln!("merge steward: could not save its memory: {e:#}");
    }
}

/// One cycle: every opted-in org, one read each.
pub(crate) async fn tick_once(app: &Shared) {
    // Issue #1074: nothing while GitHub refuses the account; the cycle waits for the breaker.
    if crate::github_breaker::paused(app).is_some() {
        return;
    }
    let sessions = app.sessions.read().await.clone();
    let looped = crate::merge_loop::load(&app.cfg.config_dir).await.settings;
    let looped = crate::merge_loop::resolve_settings(app, looped).await;
    let train = crate::merge_train::train_settings(app).await;
    let mut by_org: BTreeMap<String, Vec<Session>> = BTreeMap::new();
    for s in sessions.iter().filter(|s| s.pr_url.is_some() && !s.org.is_empty()) {
        if s.status != SessionStatus::PrOpened || s.repo.is_empty() {
            continue;
        }
        by_org.entry(s.org.clone()).or_default().push(s.clone());
    }
    let mut on_orgs: HashSet<String> = HashSet::new();
    for (org, group) in by_org {
        let settings = app.org_settings(&org);
        // A hidden org is out of the steward's sight (issue #1213), as `off` is.
        if settings.hidden || orgs::auto_merge_mode(&settings) == AutoMerge::Off {
            continue;
        }
        on_orgs.insert(org.clone());
        // A repository the merge train or its loop drives is theirs: two drivers would each merge on
        // their own reading.
        let own: Vec<Session> = group
            .into_iter()
            .filter(|s| {
                !crate::merge_loop::drives(&looped, &s.repo)
                    && crate::merge_train::effective_state(&train, &s.repo) != crate::merge_train::TrainState::On
            })
            .collect();
        if own.is_empty() {
            continue;
        }
        steward_org(app, &org, &settings, own, &sessions).await;
    }
    // Forget what no longer applies: colonies that are gone or merged, and orgs switched off.
    let live: HashSet<String> = sessions
        .iter()
        .filter(|s| !matches!(s.status, SessionStatus::Merged | SessionStatus::Closed))
        .map(|s| s.id.clone())
        .collect();
    let _ = update(&app.cfg.config_dir, move |m| {
        m.prs.retain(|_, p| live.contains(&p.session));
        m.rounds.retain(|id, _| live.contains(id));
        m.ci_blocked.retain(|org, _| on_orgs.contains(org));
        m.last_cycle = Some(Utc::now());
    })
    .await;
}

/// `POST /api/merge-steward/merge`: the cockpit's **Merge now**. Asks GitHub again and merges only a
/// pull request GitHub itself calls clean — the same branch-protection gate the loop uses, without
/// waiting for green checks to be the steward's idea. Works whatever the org's `auto_merge` says.
async fn merge_now(State(app): State<Shared>, Json(body): Json<Value>) -> ApiResult<Value> {
    let url = body["url"].as_str().unwrap_or_default().to_string();
    let Some(s) = app
        .sessions
        .read()
        .await
        .iter()
        .find(|s| s.pr_url.as_deref() == Some(url.as_str()))
        .cloned()
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no colony opened that pull request"));
    };
    if authority::external_writes_blocked() {
        return Err(client_error(StatusCode::CONFLICT, crate::publish::BLOCKED));
    }
    if crate::github_breaker::paused(&app).is_some() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "GitHub is paused for this account; try again once it clears",
        ));
    }
    let facts = read_prs(&app, std::slice::from_ref(&url))
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))?;
    let Some(f) = facts.get(&url) else {
        return Err(client_error(StatusCode::NOT_FOUND, "GitHub has no such pull request"));
    };
    if let Some(why) = manual_refusal(f) {
        return Err(client_error(StatusCode::CONFLICT, &why));
    }
    let settings = app.org_settings(&s.org);
    let sessions = app.sessions.read().await.clone();
    let keep = crate::merge_train::has_stacked_child(&sessions, &s);
    let done = merge(&app, &s, f, &settings, keep)
        .await
        .map_err(|e| client_error(StatusCode::CONFLICT, &format!("GitHub refused the merge: {e:#}")))?;
    let what = match done {
        Merged::Yes => "merged from the cockpit",
        Merged::Queued => "queued for auto-merge from the cockpit",
    };
    app.session_log(&s.id, "info", format!("merge steward: {what} ({})", f.head))
        .await;
    record(&app, &s, what).await;
    let (dir, session, repo, org, title, head) = (
        app.cfg.config_dir.clone(),
        s.id.clone(),
        s.repo.clone(),
        s.org.clone(),
        f.title.clone(),
        f.head.clone(),
    );
    let _ = update(&dir, move |m| {
        m.prs.insert(
            url,
            PrMemory {
                session,
                org,
                repo,
                title,
                state: Phase::Merging,
                reason: what.to_string(),
                head,
                at: Utc::now(),
                update_tried_head: None,
            },
        );
    })
    .await;
    Ok(Json(json!({"merged": true, "message": what})))
}

/// Why **Merge now** refuses a pull request, or `None` when GitHub calls it mergeable. A person
/// pressing the button decides about unfinished or non-required checks, but never about GitHub's own
/// refusal: a failing or pending required check, a missing review or a conflict is `BLOCKED`, `DIRTY`
/// or `BEHIND`, and none of those merges.
pub(crate) fn manual_refusal(f: &Facts) -> Option<String> {
    if !f.open {
        return Some("the pull request is not open".into());
    }
    if f.draft {
        return Some("the pull request is a draft".into());
    }
    if f.cross_repository {
        return Some("the pull request comes from a fork".into());
    }
    match f.merge_state.as_str() {
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => None,
        "BEHIND" => Some("the branch is behind its base; update it first".into()),
        "DIRTY" => Some("the branch conflicts with its base".into()),
        "BLOCKED" => Some("branch protection is not satisfied: a required check or review is missing".into()),
        other => Some(format!("GitHub reports {other}, not mergeable yet")),
    }
}

fn routes() -> axum::Router<Shared> {
    axum::Router::new()
        .route("/api/merge-steward", axum::routing::get(get_status))
        .route("/api/merge-steward/merge", axum::routing::post(merge_now))
}

/// The activity line pressing **Merge now** records.
const ACTIVITY: &[crate::activity::Rule] = &[crate::activity::rule(
    "POST",
    "/api/merge-steward/merge",
    "publish.merge_steward",
    crate::activity::Target::Fixed("merge steward", "loops"),
)];

fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_DELAY).await;
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            // One panicked cycle must not end the steward silently.
            let worker = app.clone();
            if let Err(e) = tokio::spawn(async move { tick_once(&worker).await }).await
                && e.is_panic()
            {
                eprintln!("merge steward: a cycle panicked and was skipped: {e}");
            }
        }
    });
}

/// This module's feature descriptor (`features.rs`). Both routes are the owner's: one lists what the
/// steward did, the other merges.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "merge_steward",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &["publish.merge_steward"],
    start_tasks: Some(start_tasks),
};

#[cfg(test)]
mod tests;
