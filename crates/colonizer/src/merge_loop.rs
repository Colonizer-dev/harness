//! The merge-train loop (issue #754): a built-in, opt-in loop that drives the merge train
//! (`merge_train.rs`, issue #671/#720) the careful way an operator runs it by hand. It is off by
//! default, runs on a cadence (hourly unless changed), and only in repositories the operator opted
//! in. Each run:
//!
//! 1. **Keeps main green.** Nothing merges unless the default branch's latest CI on its tip is
//!    completed and successful. A run still going is a wait, and a cancelled run (re-queued or not)
//!    is a wait too, never red.
//! 2. **Merges only on fresh CI.** A pull request merges only when it is behind its base by 0 and
//!    every check on that exact head is green; GitHub's CLEAN alone is not enough.
//! 3. **Goes one at a time.** After a merge the next candidate is updated onto the new base and the
//!    run waits (bounded) for its CI before re-checking. A per-run merge cap, a per-repository
//!    cooldown between merges, a budget and a minimum gap for GitHub calls keep it gentle, and any
//!    403/429, abuse or secondary-rate-limit answer stops the run on the spot — no retry.
//! 4. **Merges only what it may:** colony pull requests (`colonizer/*` branches the mothership
//!    published), never drafts or HOLD/WIP/do-not-merge, never a held or superseded colony, never a
//!    repository that is not on the allowlist, and never one on the `never` list.
//! 5. **Rebases only mechanically.** A conflicted pull request gets the host's mechanical rebase; if
//!    that conflicts it is marked `needs_redo` and — only when the operator enabled it — a redo
//!    colony is dispatched once, with the pull request as its reference.
//! 6. **Self-heals main, only when enabled.** Main red right after the train's own merge pauses the
//!    repository; with `self_heal` on, the failed jobs are re-run once, then a fix colony is sent;
//!    with `revert_on_red` it is a revert of the train's own last merge instead. Never anything else.
//! 7. **Re-runs a red pull request once** when every failing check is on the known-flaky list.
//! 8. **Adds no attribution.** Squash merges titled after the pull request, and pull requests whose
//!    commits carry AI attribution are refused, on top of the train's own author/forbid guards.
//! 9. **Reports every run** — merged, updated (CI running), red, redo dispatched, skipped, each with
//!    its reason — into the loop's history, the activity log and each colony's own log.
//!
//! A dry run reads everything and writes nothing, and `COLONIZER_NO_EXTERNAL_EFFECTS` turns every
//! run into one. GitHub sits behind [`Ops`], so the whole engine runs against fakes in the tests.
//! The settings, what the loop remembers per repository and the run history live in
//! `<config_dir>/merge-train-loop.json`.

use crate::{
    ApiResult, App, Shared,
    activity::Entry,
    authority, client_error,
    github::{self, CiState, Mergeability},
    merge_train::{self, CHECKS_FAILING, Decision, Guards, PrFacts, TrainState},
    plugins::parse_list,
    schedule::{Cadence, next_run_after},
    sessions::{self, NewSession, Session, SessionStatus},
    util::{truncate, valid_repo, write_atomic},
};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    routing,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

/// The origin a redo colony carries: `merge-train:redo:<the colony it redoes>`. Its existence is
/// what supersedes the original.
pub(crate) const ORIGIN_REDO: &str = "merge-train:redo:";
/// The origin of a colony sent to fix (or revert onto) a red main: `merge-train:fix:<owner/repo>`.
pub(crate) const ORIGIN_FIX: &str = "merge-train:fix:";
const FILE: &str = "merge-train-loop.json";
/// Runs kept in the history.
const HISTORY: usize = 20;
/// How much of a failing job's log a fix colony is handed.
const LOG_TAIL: usize = 6000;
/// Attribution no merge may carry, whatever `merge_train_forbid` says (rule 8).
const AI_ATTRIBUTION: &[&str] = &[
    "co-authored-by: claude",
    "noreply@anthropic.com",
    "generated with [claude code]",
];
const GH_LIMIT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------------------------
// Settings, memory and reports.
// ---------------------------------------------------------------------------------------------

/// The operator's settings. Everything defaults to the careful side: off, no repository opted in,
/// no self-heal, no revert, no redo colonies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub cadence: Cadence,
    /// Opted-in `owner` or `owner/repo` entries. Empty: the loop merges nowhere.
    pub allow: Vec<String>,
    /// `owner` or `owner/repo` entries the loop never merges in (upstream-review-only forks, say),
    /// whatever `allow` says.
    pub never: Vec<String>,
    /// Merges per repository per run.
    pub max_merges: u32,
    /// Per-repository caps (`owner/repo` → merges per run) that replace `max_merges` there.
    pub repo_max_merges: BTreeMap<String, u32>,
    /// The least time between two merges in one repository.
    pub cooldown_secs: u64,
    /// How long a run waits for an updated pull request's CI before moving on.
    pub ci_wait_minutes: u64,
    /// How far apart those waits re-read GitHub.
    pub ci_poll_secs: u64,
    /// Check names (a trailing `*` matches a prefix) re-run once when they are all that fails.
    pub flaky_checks: Vec<String>,
    /// Rule 6: re-run main's failed jobs once, then send a fix colony, when main goes red right
    /// after the train's own merge.
    pub self_heal: bool,
    /// With `self_heal`: send a revert of the train's own last merge instead of a fix colony.
    pub revert_on_red: bool,
    /// Dispatch a redo colony (once per pull request) for one whose mechanical rebase conflicted.
    pub redo_on_conflict: bool,
    /// GitHub calls one run may make; past it the run stops and the rest waits for the next.
    pub max_api_calls: u32,
    /// The least time between two GitHub calls.
    pub min_call_gap_ms: u64,
    /// Colony ids the operator holds out of the loop.
    pub held: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            cadence: Cadence::Interval { minutes: 60 },
            allow: Vec::new(),
            never: Vec::new(),
            max_merges: 4,
            repo_max_merges: BTreeMap::new(),
            cooldown_secs: 120,
            ci_wait_minutes: 20,
            ci_poll_secs: 120,
            flaky_checks: Vec::new(),
            self_heal: false,
            revert_on_red: false,
            redo_on_conflict: false,
            max_api_calls: 400,
            min_call_gap_ms: 1000,
            held: Vec::new(),
        }
    }
}

/// An `owner` or `owner/repo` entry.
fn valid_target(t: &str) -> bool {
    valid_repo(t) || (!t.contains('/') && valid_repo(&format!("{t}/x")))
}

/// Checks and tidies a settings body before it is saved: entries trimmed, lowercased and
/// deduplicated, every bound enforced so a hand-made body cannot switch the brakes off.
pub(crate) fn normalize(mut s: Settings) -> Result<Settings, String> {
    s.cadence.check()?;
    let tidy = |list: &mut Vec<String>, what: &str| -> Result<(), String> {
        let mut out: Vec<String> = Vec::new();
        for entry in list.iter().map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()) {
            if !valid_target(&entry) {
                return Err(format!("{what} entry {entry:?} is not an owner or owner/repo"));
            }
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
        *list = out;
        Ok(())
    };
    tidy(&mut s.allow, "allow")?;
    tidy(&mut s.never, "never")?;
    let bound = |value: u64, lo: u64, hi: u64, what: &str| -> Result<(), String> {
        if (lo..=hi).contains(&value) {
            Ok(())
        } else {
            Err(format!("{what} must be {lo} to {hi}, got {value}"))
        }
    };
    bound(u64::from(s.max_merges), 1, 20, "max_merges")?;
    let mut caps = BTreeMap::new();
    for (repo, cap) in &s.repo_max_merges {
        let repo = repo.trim().to_ascii_lowercase();
        if !valid_repo(&repo) {
            return Err(format!("repo_max_merges key {repo:?} is not owner/repo"));
        }
        bound(u64::from(*cap), 1, 20, "a repository's merge cap")?;
        caps.insert(repo, *cap);
    }
    s.repo_max_merges = caps;
    bound(s.cooldown_secs, 30, 3600, "cooldown_secs")?;
    bound(s.ci_wait_minutes, 1, 120, "ci_wait_minutes")?;
    bound(s.ci_poll_secs, 30, 600, "ci_poll_secs")?;
    bound(u64::from(s.max_api_calls), 20, 2000, "max_api_calls")?;
    bound(s.min_call_gap_ms, 200, 10_000, "min_call_gap_ms")?;
    s.flaky_checks = parse_list(&s.flaky_checks.join(","));
    s.held = parse_list(&s.held.join(","));
    if s.revert_on_red && !s.self_heal {
        return Err("revert_on_red only acts as part of self_heal; switch self_heal on too".to_string());
    }
    Ok(s)
}

fn listed(list: &[String], repo: &str) -> bool {
    let repo = repo.to_ascii_lowercase();
    let owner = repo.split('/').next().unwrap_or_default();
    list.iter().any(|t| {
        let t = t.trim().to_ascii_lowercase();
        t == repo || t == owner
    })
}

/// Whether the loop merges in a repository, and why not when it does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OptIn {
    In,
    NotOptedIn,
    Never(&'static str),
}

/// The `never` list and the train's `merge_train_deny_orgs` beat the allowlist, which is empty by
/// default.
fn opt_in(s: &Settings, repo: &str, denied_org: bool) -> OptIn {
    if listed(&s.never, repo) {
        return OptIn::Never("this repository is marked never (upstream review only)");
    }
    if denied_org {
        return OptIn::Never("its org is on merge_train_deny_orgs");
    }
    if listed(&s.allow, repo) {
        OptIn::In
    } else {
        OptIn::NotOptedIn
    }
}

/// Whether the loop is the one driving a repository, so the train's own two-minute tick keeps out.
pub(crate) fn drives(s: &Settings, repo: &str) -> bool {
    s.enabled && opt_in(s, repo, false) == OptIn::In
}

/// The train's own merge, as the loop remembers it: what a red main is checked against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrainMerge {
    pub pr_url: String,
    pub title: String,
    /// The squash commit on the base; `None` when GitHub did not say.
    pub sha: Option<String>,
    pub at: DateTime<Utc>,
}

/// What the loop remembers about one repository between runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RepoMemory {
    /// Why the train is paused here; cleared when main is green again.
    pub paused: Option<String>,
    pub last_merge_at: Option<DateTime<Utc>>,
    pub last_train_merge: Option<TrainMerge>,
    /// The red main sha whose failed jobs were already re-run once.
    pub reran_main: Option<String>,
    /// The red main sha a fix (or revert) colony was already sent for.
    pub healed: Option<String>,
    /// Pull requests whose mechanical rebase conflicted, with why.
    pub needs_redo: BTreeMap<String, String>,
    /// Pull requests a redo colony was already dispatched for: at most once each.
    pub redo_dispatched: BTreeSet<String>,
    /// `pr_url@head` pairs whose known-flaky checks were already re-run once.
    pub flaky_reruns: BTreeSet<String>,
}

/// What a run did (or, in a dry run, would do) with one pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Merged,
    Updated,
    Rebased,
    Red,
    Rerun,
    NeedsRedo,
    RedoDispatched,
    Waiting,
    Skipped,
}

impl Action {
    fn word(self, dry: bool) -> &'static str {
        match (self, dry) {
            (Action::Merged, false) => "merged",
            (Action::Merged, true) => "would merge",
            (Action::Updated, false) => "updated (CI running)",
            (Action::Updated, true) => "would update",
            (Action::Rebased, false) => "rebased (CI running)",
            (Action::Rebased, true) => "would rebase",
            (Action::Red, _) => "red",
            (Action::Rerun, false) => "re-ran flaky checks",
            (Action::Rerun, true) => "would re-run flaky checks",
            (Action::NeedsRedo, _) => "needs redo",
            (Action::RedoDispatched, false) => "redo dispatched",
            (Action::RedoDispatched, true) => "would dispatch a redo",
            (Action::Waiting, _) => "waiting",
            (Action::Skipped, _) => "skipped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Item {
    pub session: String,
    pub pr_url: String,
    pub title: String,
    pub action: Action,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct RepoReport {
    pub repo: String,
    /// Main's CI as the run last read it, in words.
    pub main: String,
    pub paused: Option<String>,
    /// What the run did about a red main (rule 6), in words.
    pub heal: Vec<String>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Report {
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub dry_run: bool,
    /// The kill switch turned a real run into this dry run.
    pub forced_dry_run: bool,
    /// Why the run stopped early: GitHub pushed back, or the call budget ran out.
    pub stopped: Option<String>,
    pub api_calls: u32,
    /// One line: the counts.
    pub summary: String,
    /// The report in lines, as the CLI prints it.
    pub lines: Vec<String>,
    pub repos: Vec<RepoReport>,
}

/// The counts line: merged, updated (CI running), red, redo dispatched, skipped — every run says
/// all five, and the other actions when they happened.
pub(crate) fn summary(r: &Report) -> String {
    let count = |a: Action| r.repos.iter().flat_map(|x| &x.items).filter(|i| i.action == a).count();
    let dry = r.dry_run;
    let mut parts = vec![
        format!("{} {}", if dry { "would merge" } else { "merged" }, count(Action::Merged)),
        format!(
            "{} {}",
            if dry { "would update" } else { "updated (CI running)" },
            count(Action::Updated)
        ),
        format!("red {}", count(Action::Red)),
        format!(
            "{} {}",
            if dry { "would dispatch redo" } else { "redo dispatched" },
            count(Action::RedoDispatched)
        ),
        format!("skipped {}", count(Action::Skipped)),
    ];
    for (action, word) in [
        (Action::Rebased, if dry { "would rebase" } else { "rebased" }),
        (Action::Rerun, if dry { "would re-run" } else { "re-ran flaky" }),
        (Action::NeedsRedo, "needs redo"),
        (Action::Waiting, "waiting"),
    ] {
        let n = count(action);
        if n > 0 {
            parts.push(format!("{word} {n}"));
        }
    }
    let mut out = format!("{}{}", if dry { "dry run: " } else { "" }, parts.join(" · "));
    if let Some(why) = &r.stopped {
        out.push_str(&format!(" — stopped: {why}"));
    }
    out
}

/// The whole report as lines: the summary, then each repository with main's state, anything done
/// about it, and every pull request with its action and reason.
pub(crate) fn lines(r: &Report) -> Vec<String> {
    let mut out = vec![summary(r)];
    if r.forced_dry_run {
        out.push("external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS), so this was a dry run".to_string());
    }
    for repo in &r.repos {
        out.push(format!("{} — main: {}", repo.repo, repo.main));
        if let Some(p) = &repo.paused {
            out.push(format!("  paused: {p}"));
        }
        for h in &repo.heal {
            out.push(format!("  {h}"));
        }
        for i in &repo.items {
            let pr = pr_number(&i.pr_url)
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| i.pr_url.clone());
            out.push(format!("  {pr} {}: {} — {}", i.title, i.action.word(r.dry_run), i.reason));
        }
    }
    out
}

/// Everything in `merge-train-loop.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LoopState {
    pub settings: Settings,
    pub next_run_at: Option<DateTime<Utc>>,
    pub repos: BTreeMap<String, RepoMemory>,
    /// Oldest first, at most [`HISTORY`].
    pub history: Vec<Report>,
}

fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The saved state; a missing or unreadable file reads as the defaults — the loop off.
pub(crate) async fn load(dir: &Path) -> LoopState {
    match tokio::fs::read(file(dir)).await {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|e| {
            eprintln!(
                "merge-train loop: could not parse {}: {e}; the loop reads as off",
                file(dir).display()
            );
            LoopState::default()
        }),
        Err(_) => LoopState::default(),
    }
}

/// Serialises every read-modify-write of the file.
static STATE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

async fn update<R>(dir: &Path, f: impl FnOnce(&mut LoopState) -> R) -> anyhow::Result<(LoopState, R)> {
    let _guard = STATE_LOCK.lock().await;
    let mut state = load(dir).await;
    let r = f(&mut state);
    write_atomic(&file(dir), &serde_json::to_vec_pretty(&state)?).await?;
    Ok((state, r))
}

// ---------------------------------------------------------------------------------------------
// Main's CI, pull request plans, and the other pure readings.
// ---------------------------------------------------------------------------------------------

/// Main's CI on its tip, as rule 1 needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MainCi {
    Green {
        sha: String,
    },
    /// Running, cancelled (re-queued or not), not started or unreadable: wait, never red.
    Pending {
        reason: String,
    },
    Red {
        sha: String,
        run_ids: Vec<u64>,
        detail: String,
    },
}

impl MainCi {
    fn describe(&self) -> String {
        match self {
            MainCi::Green { sha } => format!("green at {}", short(sha)),
            MainCi::Pending { reason } => format!("not green yet ({reason}); merging nothing"),
            MainCi::Red { sha, detail, .. } => format!("red at {} ({detail})", short(sha)),
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Reads `GET /repos/{repo}/actions/runs?head_sha=<tip>` for the tip: the newest run of each
/// workflow decides. Any failure is red; anything unfinished, and any cancellation, is a wait;
/// no run at all is a wait too — nothing merges on CI that has not run.
pub(crate) fn main_ci_from(tip: &str, runs: &Value) -> MainCi {
    let word = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().trim().to_ascii_lowercase();
    let all: Vec<&Value> = runs["workflow_runs"]
        .as_array()
        .map(|a| a.iter().filter(|r| r["head_sha"].as_str() == Some(tip)).collect())
        .unwrap_or_default();
    if all.is_empty() {
        return MainCi::Pending {
            reason: format!("no CI run has started on {} yet", short(tip)),
        };
    }
    // Newest first, as GitHub lists them: the first run seen of a workflow is its latest, so a
    // cancelled run that was re-queued is judged by the re-queued one.
    let mut seen = HashSet::new();
    let (mut running, mut cancelled, mut red, mut run_ids) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut ordered = all.clone();
    ordered.sort_by(|a, b| {
        let key = |r: &Value| {
            (
                r["created_at"].as_str().unwrap_or_default().to_string(),
                r["run_attempt"].as_u64().unwrap_or(0),
            )
        };
        key(b).cmp(&key(a))
    });
    for run in ordered {
        let key = run["workflow_id"]
            .as_u64()
            .map(|id| id.to_string())
            .unwrap_or_else(|| word(run, "name"));
        if !seen.insert(key) {
            continue;
        }
        let name = run["name"].as_str().unwrap_or("a workflow").to_string();
        if word(run, "status") != "completed" {
            running.push(name);
            continue;
        }
        match word(run, "conclusion").as_str() {
            "success" | "neutral" | "skipped" => {}
            "cancelled" | "stale" => cancelled.push(name),
            "" => running.push(name),
            _ => {
                red.push(name);
                if let Some(id) = run["id"].as_u64() {
                    run_ids.push(id);
                }
            }
        }
    }
    if !red.is_empty() {
        return MainCi::Red {
            sha: tip.to_string(),
            run_ids,
            detail: format!("failed: {}", red.join(", ")),
        };
    }
    if !running.is_empty() {
        return MainCi::Pending {
            reason: format!("still running: {}", running.join(", ")),
        };
    }
    if !cancelled.is_empty() {
        return MainCi::Pending {
            reason: format!("cancelled, not red: waiting for {} to run again", cancelled.join(", ")),
        };
    }
    MainCi::Green { sha: tip.to_string() }
}

/// One failing check on a pull request's head, with the Actions run that produced it when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailingCheck {
    pub name: String,
    pub run_id: Option<u64>,
}

/// The failing checks in a `statusCheckRollup`, named as GitHub shows them.
pub(crate) fn failing_checks_from(rollup: &Value) -> Vec<FailingCheck> {
    const FAILING: &[&str] = &[
        "FAILURE",
        "CANCELLED",
        "TIMED_OUT",
        "ACTION_REQUIRED",
        "STARTUP_FAILURE",
        "ERROR",
    ];
    let word = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().trim().to_ascii_uppercase();
    rollup
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| {
                    let verdict = if word(i, "conclusion").is_empty() {
                        word(i, "state")
                    } else {
                        word(i, "conclusion")
                    };
                    FAILING.contains(&verdict.as_str())
                })
                .map(|i| FailingCheck {
                    name: i["name"]
                        .as_str()
                        .or_else(|| i["context"].as_str())
                        .unwrap_or("a check")
                        .to_string(),
                    run_id: i["detailsUrl"]
                        .as_str()
                        .or_else(|| i["targetUrl"].as_str())
                        .and_then(run_id_from_url),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `…/actions/runs/<id>/job/<job>` → `<id>`.
fn run_id_from_url(url: &str) -> Option<u64> {
    let rest = url.split("/actions/runs/").nth(1)?;
    rest.split(['/', '?', '#']).next()?.parse().ok()
}

fn pr_number(url: &str) -> Option<u64> {
    url.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

fn is_flaky(name: &str, flaky: &[String]) -> bool {
    let name = name.trim().to_ascii_lowercase();
    flaky.iter().any(|p| {
        let p = p.trim().to_ascii_lowercase();
        match p.strip_suffix('*') {
            Some(prefix) => !prefix.is_empty() && name.starts_with(prefix),
            None => name == p,
        }
    })
}

/// Whether GitHub is pushing back: a 403 or 429, abuse detection or a secondary rate limit. The
/// run stops on the first one and does not retry — burst automation is how an account gets suspended.
pub(crate) fn is_throttle(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "http 403",
        "http 429",
        "status 403",
        "status 429",
        "403 forbidden",
        "429 too many",
        "(403)",
        "(429)",
        "rate limit",
        "abuse",
        "secondary rate",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// The colony whose redo, or whose newer pull request on the same issue, replaces this one.
fn superseded_by<'a>(sessions: &'a [Session], s: &Session) -> Option<&'a str> {
    let redo = format!("{ORIGIN_REDO}{}", s.id);
    sessions
        .iter()
        .find(|t| {
            t.id != s.id
                && t.repo == s.repo
                && (t.origin.as_deref() == Some(redo.as_str())
                    || (s.issue.is_some()
                        && t.issue == s.issue
                        && t.created_at > s.created_at
                        && matches!(t.status, SessionStatus::PrOpened | SessionStatus::Merged)))
        })
        .map(|t| t.id.as_str())
}

/// Rule 4's colony-level half; the pull-request half (draft, HOLD/WIP, authors) is [`plan_pr`]'s.
fn eligibility(sessions: &[Session], s: &Session, settings: &Settings) -> Result<(), String> {
    if !s.branch.starts_with("colonizer/") {
        return Err("not a colony branch (colonizer/*) the mothership published".to_string());
    }
    if settings.held.iter().any(|h| h == &s.id) {
        return Err("the colony is held out of the merge train".to_string());
    }
    if let Some(by) = superseded_by(sessions, s) {
        return Err(format!("the colony is superseded by {by}"));
    }
    Ok(())
}

/// What the loop does with one pull request, from the train's own [`merge_train::decide`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Behind by 0, every check on the head green: merge (the head of the queue only).
    Merge,
    /// Behind the base: update the branch, then wait for fresh CI.
    Update(String),
    /// Its checks are still running.
    WaitCi,
    /// Conflicted: the host's mechanical rebase, or `needs_redo`.
    Rebase,
    Red(String),
    Wait(String),
    Skip(String),
}

pub(crate) fn plan_pr(facts: &PrFacts, guards: &Guards) -> Plan {
    match merge_train::decide(facts, guards) {
        Decision::Merge => Plan::Merge,
        Decision::WaitingCi => Plan::WaitCi,
        Decision::NeedsRebase(nr) => {
            if facts.info.mergeability == Mergeability::Conflicted {
                Plan::Rebase
            } else {
                Plan::Update(nr.detail)
            }
        }
        Decision::Skipped(r) if r == CHECKS_FAILING => Plan::Red(r),
        Decision::Skipped(r) => Plan::Skip(r),
        Decision::Waiting(r) => Plan::Wait(r),
    }
}

// ---------------------------------------------------------------------------------------------
// The GitHub seam.
// ---------------------------------------------------------------------------------------------

/// One pull request as the loop reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reading {
    pub facts: PrFacts,
    pub failing: Vec<FailingCheck>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RebaseResult {
    Rebased(String),
    Conflicted,
    Failed(String),
}

/// A colony the loop sends.
#[derive(Clone, Debug)]
pub(crate) enum Dispatch {
    Redo {
        session: Box<Session>,
        pr_url: String,
        base: String,
        why: String,
    },
    Fix {
        repo: String,
        base: String,
        merged: TrainMerge,
        detail: String,
        log: String,
    },
    Revert {
        repo: String,
        base: String,
        merged: TrainMerge,
        detail: String,
    },
}

/// Everything the loop asks of GitHub and of the host, so the tests stand in for both. Module
/// private, like `github.rs`' `PublishOps`: its futures stay concrete, so the real one is `Send`.
trait Ops {
    fn now(&self) -> DateTime<Utc>;
    async fn sleep(&self, d: Duration);
    async fn guards(&self) -> Result<Guards, String>;
    async fn default_branch(&self, repo: &str) -> Result<String, String>;
    async fn main_ci(&self, repo: &str, branch: &str) -> Result<MainCi, String>;
    async fn read_pr(&self, s: &Session, base: &str) -> Result<Reading, String>;
    async fn update_branch(&self, s: &Session, head: &str) -> Result<(), String>;
    async fn rebase(&self, s: &Session) -> RebaseResult;
    /// Squash-merges pinned to `head`; the squash commit's sha when GitHub says.
    async fn merge(&self, s: &Session, head: &str, subject: &str, delete_branch: bool) -> Result<Option<String>, String>;
    async fn rerun(&self, repo: &str, run_id: u64) -> Result<(), String>;
    async fn failure_log(&self, repo: &str, run_id: u64) -> Result<String, String>;
    async fn dispatch(&self, d: Dispatch) -> Result<String, String>;
}

// ---------------------------------------------------------------------------------------------
// The engine.
// ---------------------------------------------------------------------------------------------

struct Stop(String);

enum Waited {
    Settled,
    MainRed,
    Timeout,
}

/// The items one repository's run settles, in the order they settled; a colony settles once.
#[derive(Default)]
struct Items {
    list: Vec<Item>,
    done: HashSet<String>,
}

impl Items {
    fn add(&mut self, s: &Session, title: &str, action: Action, reason: impl Into<String>) {
        if self.done.insert(s.id.clone()) {
            self.list.push(Item {
                session: s.id.clone(),
                pr_url: s.pr_url.clone().unwrap_or_default(),
                title: title.to_string(),
                action,
                reason: reason.into(),
            });
        }
    }
}

fn title_of(s: &Session, reading: Option<&Reading>) -> String {
    reading
        .map(|r| r.facts.info.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| s.issue_title.clone())
}

struct Engine<'a, O: Ops> {
    ops: &'a O,
    cfg: &'a Settings,
    sessions: &'a [Session],
    guards: Guards,
    dry: bool,
    calls: u32,
}

/// One paced GitHub call: counts against the budget, keeps the minimum gap, and turns a push-back
/// answer into the run's stop.
macro_rules! gh {
    ($self:ident, $call:expr) => {{
        $self.pace().await?;
        let result = $call.await;
        $self.vet(result)?
    }};
}

impl<'a, O: Ops> Engine<'a, O> {
    async fn pace(&mut self) -> Result<(), Stop> {
        if self.calls >= self.cfg.max_api_calls {
            return Err(Stop(format!(
                "this run's GitHub budget ({} calls) is spent; the rest waits for the next run",
                self.cfg.max_api_calls
            )));
        }
        if self.calls > 0 && self.cfg.min_call_gap_ms > 0 {
            self.ops.sleep(Duration::from_millis(self.cfg.min_call_gap_ms)).await;
        }
        self.calls += 1;
        Ok(())
    }

    fn vet<T>(&self, result: Result<T, String>) -> Result<Result<T, String>, Stop> {
        match result {
            Err(e) if is_throttle(&e) => Err(Stop(format!(
                "GitHub pushed back ({}); the run stopped and does not retry",
                truncate(e.trim(), 200)
            ))),
            other => Ok(other),
        }
    }

    fn cap(&self, repo: &str) -> u32 {
        self.cfg
            .repo_max_merges
            .get(&repo.to_ascii_lowercase())
            .copied()
            .unwrap_or(self.cfg.max_merges)
    }

    fn wait_minutes(&self) -> u64 {
        self.cfg.ci_wait_minutes
    }

    /// One repository's run: main's gate, the queue oldest first, at most one pull request moved
    /// at a time, and every colony's line in the report.
    async fn repo(&mut self, repo: &str, group: &[&Session], mem: &mut RepoMemory, out: &mut RepoReport) -> Result<(), Stop> {
        let mut items = Items::default();
        let result = self.repo_inner(repo, group, mem, out, &mut items).await;
        // Whatever stopped the run, every colony still gets a line.
        let mut ordered = group.to_vec();
        ordered.sort_by(|a, b| a.pr_opened_at.cmp(&b.pr_opened_at).then(a.id.cmp(&b.id)));
        let fallback = match &result {
            Err(Stop(why)) => format!("not reached: {why}"),
            Ok(Some(why)) => why.clone(),
            Ok(None) => "waits its turn: the train moves one pull request at a time".to_string(),
        };
        for s in ordered {
            items.add(s, &s.issue_title, Action::Waiting, fallback.clone());
        }
        out.items = items.list;
        out.paused = mem.paused.clone();
        result.map(|_| ())
    }

    /// `Ok(Some(why))` when the run held the whole repository (main not green, say).
    async fn repo_inner(
        &mut self,
        repo: &str,
        group: &[&Session],
        mem: &mut RepoMemory,
        out: &mut RepoReport,
        items: &mut Items,
    ) -> Result<Option<String>, Stop> {
        let base = match gh!(self, self.ops.default_branch(repo)) {
            Ok(base) => base,
            Err(e) => return Ok(Some(format!("the default branch could not be read ({e})"))),
        };
        let cap = self.cap(repo);
        let mut queue: Vec<&Session> = group.to_vec();
        queue.sort_by(|a, b| a.pr_opened_at.cmp(&b.pr_opened_at).then(a.id.cmp(&b.id)));
        queue.retain(|s| match eligibility(self.sessions, s, self.cfg) {
            Ok(()) => true,
            Err(why) => {
                items.add(s, &s.issue_title, Action::Skipped, why);
                false
            }
        });
        let mut merges = 0u32;
        let mut updated: HashSet<String> = HashSet::new();
        let mut rounds = 0u32;
        loop {
            rounds += 1;
            if rounds > cap * 3 + 4 {
                return Ok(None);
            }
            if queue.iter().all(|s| items.done.contains(&s.id)) {
                return Ok(None);
            }
            // Rule 1, before anything else — and again before every later merge.
            let main = match gh!(self, self.ops.main_ci(repo, &base)) {
                Ok(main) => main,
                Err(e) => MainCi::Pending {
                    reason: format!("its CI could not be read ({e})"),
                },
            };
            out.main = main.describe();
            match &main {
                MainCi::Green { .. } => {
                    if let Some(was) = mem.paused.take() {
                        out.heal
                            .push(format!("{base} is green again; the train resumes (it was paused: {was})"));
                    }
                    mem.reran_main = None;
                    mem.healed = None;
                }
                MainCi::Red { .. } => {
                    self.heal(&base, &main, mem, out).await?;
                    return Ok(Some(format!("{base} is red: the train merges nothing until it is green")));
                }
                // Right after this run's own merge, main's CI on the new tip is naturally still
                // running: the next candidate is updated meanwhile, and nothing merges until both
                // are green (`wait_ci`). Otherwise a main that is not green holds everything.
                MainCi::Pending { reason } if merges == 0 => {
                    return Ok(Some(format!("{base} is not green yet ({reason}): merging nothing")));
                }
                MainCi::Pending { .. } => {}
            }
            let main_green = matches!(main, MainCi::Green { .. });
            // Read the queue; the first mergeable-or-almost pull request is the head of the train,
            // everything else is settled on its own reading or waits its turn behind the head.
            let mut head: Option<(&Session, Reading, Plan)> = None;
            let mut behind_head: Vec<&Session> = Vec::new();
            for s in queue.clone() {
                if items.done.contains(&s.id) {
                    continue;
                }
                let reading = match gh!(self, self.ops.read_pr(s, &base)) {
                    Ok(r) => r,
                    Err(e) => {
                        items.add(s, &s.issue_title, Action::Waiting, e);
                        continue;
                    }
                };
                let title = title_of(s, Some(&reading));
                match plan_pr(&reading.facts, &self.guards) {
                    Plan::Skip(why) => items.add(s, &title, Action::Skipped, why),
                    Plan::Wait(why) => items.add(s, &title, Action::Waiting, why),
                    Plan::Red(_) => {
                        let (action, why) = self.red(s, &reading, mem).await?;
                        items.add(s, &title, action, why);
                    }
                    Plan::Rebase => {
                        let (action, why) = self.conflict(s, &base, mem).await?;
                        items.add(s, &title, action, why);
                    }
                    plan @ (Plan::Merge | Plan::Update(_) | Plan::WaitCi) => {
                        if head.is_none() {
                            head = Some((s, reading, plan));
                        } else {
                            behind_head.push(s);
                        }
                    }
                }
            }
            let Some((s, reading, plan)) = head else {
                return Ok(None);
            };
            let title = title_of(s, Some(&reading));
            let head_oid = reading.facts.info.head_ref_oid.clone().unwrap_or_default();
            let wait = self.wait_minutes();
            // Rule 1 again: a candidate that reads mergeable while main is still running merges
            // only once main is green — it waits for both first.
            let plan = if plan == Plan::Merge && !main_green {
                Plan::WaitCi
            } else {
                plan
            };
            match plan {
                Plan::Merge => {
                    if merges >= cap {
                        items.add(
                            s,
                            &title,
                            Action::Waiting,
                            format!("green and current, but this run's merge cap ({cap}) is reached; it merges on the next run"),
                        );
                        return Ok(None);
                    }
                    if self.dry {
                        items.add(
                            s,
                            &title,
                            Action::Merged,
                            "would squash-merge: behind its base by 0 and every check on its head green",
                        );
                        merges += 1;
                        for t in behind_head {
                            if merges < cap {
                                merges += 1;
                                items.add(
                                    t,
                                    &t.issue_title,
                                    Action::Updated,
                                    format!(
                                        "would update onto the new {base} after the merge before it, wait up to {wait} min for fresh CI, then merge if green"
                                    ),
                                );
                            } else {
                                items.add(
                                    t,
                                    &t.issue_title,
                                    Action::Waiting,
                                    format!("this run's merge cap ({cap}) is reached"),
                                );
                            }
                        }
                        return Ok(None);
                    }
                    if head_oid.is_empty() {
                        items.add(
                            s,
                            &title,
                            Action::Waiting,
                            "its head commit is unknown; merges are pinned to it",
                        );
                        return Ok(None);
                    }
                    // The cooldown between two merges in one repository, across runs too.
                    if let Some(at) = mem.last_merge_at {
                        let ready = at + ChronoDuration::seconds(self.cfg.cooldown_secs as i64);
                        let now = self.ops.now();
                        if now < ready {
                            self.ops.sleep((ready - now).to_std().unwrap_or_default()).await;
                        }
                    }
                    let n = pr_number(s.pr_url.as_deref().unwrap_or_default()).unwrap_or_default();
                    let subject = format!("{title} (#{n})");
                    let keep = merge_train::has_stacked_child(self.sessions, s);
                    match gh!(self, self.ops.merge(s, &head_oid, &subject, !keep)) {
                        Ok(sha) => {
                            merges += 1;
                            let at = self.ops.now();
                            mem.last_merge_at = Some(at);
                            mem.last_train_merge = Some(TrainMerge {
                                pr_url: s.pr_url.clone().unwrap_or_default(),
                                title: title.clone(),
                                sha,
                                at,
                            });
                            items.add(
                                s,
                                &title,
                                Action::Merged,
                                format!("squash-merged: behind {base} by 0, every check on its head green, {base} green"),
                            );
                        }
                        Err(e) => {
                            items.add(s, &title, Action::Waiting, format!("the merge failed ({e})"));
                            return Ok(None);
                        }
                    }
                }
                Plan::Update(detail) => {
                    if merges >= cap {
                        items.add(s, &title, Action::Waiting, format!("this run's merge cap ({cap}) is reached"));
                        return Ok(None);
                    }
                    if self.dry {
                        items.add(
                            s,
                            &title,
                            Action::Updated,
                            format!("would update the branch onto {base} ({detail}), then wait up to {wait} min for fresh CI"),
                        );
                        return Ok(None);
                    }
                    if head_oid.is_empty() {
                        items.add(s, &title, Action::Waiting, "its head commit is unknown");
                        return Ok(None);
                    }
                    // One update per pull request per run: still behind after it means the base
                    // moved again, and chasing it is the burst this loop exists to avoid.
                    if updated.contains(&s.id) {
                        items.add(
                            s,
                            &title,
                            Action::Updated,
                            format!("updated onto {base}, but it reads behind again ({detail}); it is caught up on a later run"),
                        );
                        return Ok(None);
                    }
                    if let Err(e) = gh!(self, self.ops.update_branch(s, &head_oid)) {
                        items.add(s, &title, Action::Waiting, format!("updating the branch failed ({e})"));
                        return Ok(None);
                    }
                    updated.insert(s.id.clone());
                    match self.wait_ci(s, repo, &base).await? {
                        Waited::Settled | Waited::MainRed => continue,
                        Waited::Timeout => {
                            items.add(
                                s,
                                &title,
                                Action::Updated,
                                format!(
                                    "updated onto {base}; its CI was still running after {wait} min, so it merges on a later run"
                                ),
                            );
                            return Ok(None);
                        }
                    }
                }
                Plan::WaitCi => {
                    let action = if updated.contains(&s.id) {
                        Action::Updated
                    } else {
                        Action::Waiting
                    };
                    if self.dry {
                        items.add(
                            s,
                            &title,
                            action,
                            format!("its checks are running; would wait up to {wait} min for them"),
                        );
                        return Ok(None);
                    }
                    match self.wait_ci(s, repo, &base).await? {
                        Waited::Settled | Waited::MainRed => continue,
                        Waited::Timeout => {
                            items.add(s, &title, action, format!("its checks were still running after {wait} min"));
                            return Ok(None);
                        }
                    }
                }
                _ => unreachable!("only mergeable-or-almost plans head the train"),
            }
        }
    }

    /// Waits (bounded) for a pull request's CI to settle on a green main.
    async fn wait_ci(&mut self, s: &Session, repo: &str, base: &str) -> Result<Waited, Stop> {
        let deadline = self.ops.now() + ChronoDuration::minutes(self.wait_minutes() as i64);
        loop {
            self.ops.sleep(Duration::from_secs(self.cfg.ci_poll_secs)).await;
            let main = gh!(self, self.ops.main_ci(repo, base));
            if matches!(main, Ok(MainCi::Red { .. })) {
                return Ok(Waited::MainRed);
            }
            if matches!(main, Ok(MainCi::Green { .. })) {
                let reading = gh!(self, self.ops.read_pr(s, base));
                // Settled: the checks on a current head finished, or it conflicts now. Still
                // behind is GitHub catching up with the update: keep waiting.
                if let Ok(r) = reading
                    && ((r.facts.info.ci.settled()
                        && r.facts.info.mergeability != Mergeability::Behind
                        && r.facts.behind_base.is_none_or(|n| n == 0))
                        || r.facts.info.mergeability == Mergeability::Conflicted)
                {
                    return Ok(Waited::Settled);
                }
            }
            if self.ops.now() >= deadline {
                return Ok(Waited::Timeout);
            }
        }
    }

    /// Rule 7: a red pull request is re-run once when everything failing is known-flaky.
    async fn red(&mut self, s: &Session, reading: &Reading, mem: &mut RepoMemory) -> Result<(Action, String), Stop> {
        let names = reading.failing.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ");
        let failing = if names.is_empty() {
            "its checks are failing".to_string()
        } else {
            format!("failing: {names}")
        };
        let flaky = !reading.failing.is_empty() && reading.failing.iter().all(|c| is_flaky(&c.name, &self.cfg.flaky_checks));
        if !flaky {
            return Ok((Action::Red, failing));
        }
        let head = reading.facts.info.head_ref_oid.clone().unwrap_or_default();
        let key = format!("{}@{head}", s.pr_url.as_deref().unwrap_or_default());
        if mem.flaky_reruns.contains(&key) {
            return Ok((Action::Red, format!("{failing} — again, after its one re-run")));
        }
        let runs: BTreeSet<u64> = reading.failing.iter().filter_map(|c| c.run_id).collect();
        if runs.is_empty() {
            return Ok((Action::Red, format!("{failing} — known-flaky, but no Actions run to re-run")));
        }
        if self.dry {
            return Ok((Action::Rerun, format!("would re-run the known-flaky checks once: {names}")));
        }
        for id in runs {
            if let Err(e) = gh!(self, self.ops.rerun(&s.repo, id)) {
                return Ok((Action::Red, format!("{failing} — re-running them failed ({e})")));
            }
        }
        mem.flaky_reruns.insert(key);
        Ok((
            Action::Rerun,
            format!("re-ran the known-flaky checks once: {names} (CI running)"),
        ))
    }

    /// Rule 5: a conflicted pull request gets the host's mechanical rebase; a conflicting rebase is
    /// never resolved by guessing — it becomes `needs_redo`.
    async fn conflict(&mut self, s: &Session, base: &str, mem: &mut RepoMemory) -> Result<(Action, String), Stop> {
        let url = s.pr_url.clone().unwrap_or_default();
        if let Some(why) = mem.needs_redo.get(&url).cloned() {
            return self.redo(s, &url, base, mem, why).await;
        }
        if self.dry {
            return Ok((
                Action::Rebased,
                format!(
                    "conflicts with {base}; would try the host's mechanical rebase, and mark it needs_redo if that conflicts"
                ),
            ));
        }
        // The rebase pushes: it counts against the pace like a call.
        self.pace().await?;
        match self.ops.rebase(s).await {
            RebaseResult::Rebased(sha) => Ok((
                Action::Rebased,
                format!(
                    "mechanically rebased onto {base} ({}); it merges only after fresh CI on the new head",
                    short(&sha)
                ),
            )),
            RebaseResult::Conflicted => {
                let why = format!("the mechanical rebase onto {base} conflicted");
                mem.needs_redo.insert(url.clone(), why.clone());
                self.redo(s, &url, base, mem, why).await
            }
            RebaseResult::Failed(e) => Ok((
                Action::Waiting,
                format!("conflicts with {base}; the host rebase could not run ({e}), tried again next run"),
            )),
        }
    }

    async fn redo(
        &mut self,
        s: &Session,
        url: &str,
        base: &str,
        mem: &mut RepoMemory,
        why: String,
    ) -> Result<(Action, String), Stop> {
        if mem.redo_dispatched.contains(url) {
            return Ok((
                Action::NeedsRedo,
                format!("needs_redo ({why}); its redo colony was already dispatched"),
            ));
        }
        if !self.cfg.redo_on_conflict {
            return Ok((
                Action::NeedsRedo,
                format!("needs_redo ({why}); redo colonies are off, so it waits for a person"),
            ));
        }
        if self.dry {
            return Ok((
                Action::RedoDispatched,
                format!("needs_redo ({why}); would dispatch one redo colony with the pull request as its reference"),
            ));
        }
        self.pace().await?;
        let d = Dispatch::Redo {
            session: Box::new(s.clone()),
            pr_url: url.to_string(),
            base: base.to_string(),
            why: why.clone(),
        };
        match self.ops.dispatch(d).await {
            Ok(id) => {
                mem.redo_dispatched.insert(url.to_string());
                Ok((
                    Action::RedoDispatched,
                    format!("needs_redo ({why}); dispatched redo colony {id} with the pull request as its reference"),
                ))
            }
            Err(e) => Ok((
                Action::NeedsRedo,
                format!("needs_redo ({why}); dispatching its redo colony failed ({e})"),
            )),
        }
    }

    /// Rule 6: main red. Only a red tip that is the train's own merge is the train's to act on, and
    /// then only as far as the operator allowed.
    async fn heal(&mut self, base: &str, main: &MainCi, mem: &mut RepoMemory, out: &mut RepoReport) -> Result<(), Stop> {
        let MainCi::Red { sha, run_ids, detail } = main else {
            return Ok(());
        };
        let ours = mem
            .last_train_merge
            .clone()
            .filter(|m| m.sha.as_deref() == Some(sha.as_str()));
        let Some(merged) = ours else {
            out.heal.push(format!(
                "{base} is red ({detail}) and its tip is not the train's merge: merging nothing, touching nothing"
            ));
            return Ok(());
        };
        let repo = out.repo.clone();
        let why = format!("{base} went red after the train merged {}", merged.pr_url);
        if mem.paused.is_none() {
            mem.paused = Some(why.clone());
        }
        if !self.cfg.self_heal {
            out.heal.push(format!(
                "{why}: the train is paused here until {base} is green; self-heal is off, so a person fixes it"
            ));
            return Ok(());
        }
        if mem.reran_main.as_deref() != Some(sha.as_str()) {
            if self.dry {
                out.heal
                    .push(format!("{why}: would re-run its failed jobs once, in case they are flaky"));
                return Ok(());
            }
            let mut rerun = 0;
            for id in run_ids {
                match gh!(self, self.ops.rerun(&repo, *id)) {
                    Ok(()) => rerun += 1,
                    Err(e) => out.heal.push(format!("re-running run {id} failed ({e})")),
                }
            }
            mem.reran_main = Some(sha.clone());
            out.heal.push(format!(
                "{why}: re-ran its failed jobs once ({rerun} run{}); paused until {base} is green",
                if rerun == 1 { "" } else { "s" }
            ));
            return Ok(());
        }
        if mem.healed.as_deref() == Some(sha.as_str()) {
            out.heal.push(format!(
                "{why}: still red; its {} colony is already on it, and the train stays paused",
                if self.cfg.revert_on_red { "revert" } else { "fix" }
            ));
            return Ok(());
        }
        if self.cfg.revert_on_red {
            if self.dry {
                out.heal.push(format!(
                    "{why}: still red after a re-run; would dispatch a colony to revert the train's own merge"
                ));
                return Ok(());
            }
            self.pace().await?;
            let d = Dispatch::Revert {
                repo: repo.clone(),
                base: base.to_string(),
                merged: merged.clone(),
                detail: detail.clone(),
            };
            match self.ops.dispatch(d).await {
                Ok(id) => {
                    mem.healed = Some(sha.clone());
                    out.heal.push(format!(
                        "{why}: still red after a re-run; dispatched revert colony {id} for the train's own merge"
                    ));
                }
                Err(e) => out.heal.push(format!("{why}: dispatching the revert colony failed ({e})")),
            }
            return Ok(());
        }
        if self.dry {
            out.heal.push(format!(
                "{why}: still red after a re-run; would dispatch a colony to fix {base} minimally"
            ));
            return Ok(());
        }
        let log = match run_ids.first() {
            Some(id) => match gh!(self, self.ops.failure_log(&repo, *id)) {
                Ok(log) => log,
                Err(e) => format!("(the failure log could not be read: {e})"),
            },
            None => "(no Actions run to read a log from)".to_string(),
        };
        self.pace().await?;
        let d = Dispatch::Fix {
            repo: repo.clone(),
            base: base.to_string(),
            merged: merged.clone(),
            detail: detail.clone(),
            log,
        };
        match self.ops.dispatch(d).await {
            Ok(id) => {
                mem.healed = Some(sha.clone());
                out.heal.push(format!(
                    "{why}: still red after a re-run; dispatched fix colony {id} to fix {base} minimally"
                ));
            }
            Err(e) => out.heal.push(format!("{why}: dispatching the fix colony failed ({e})")),
        }
        Ok(())
    }
}

/// One whole run over every repository with open colony pull requests. `denied_org` reads the
/// train's `merge_train_deny_orgs`. `memory` is written to only by a real run.
async fn run_all<O: Ops>(
    ops: &O,
    cfg: &Settings,
    denied_org: impl Fn(&str) -> bool,
    sessions: &[Session],
    memory: &mut BTreeMap<String, RepoMemory>,
    dry: bool,
) -> Report {
    let mut report = Report {
        started_at: Some(ops.now()),
        dry_run: dry,
        ..Report::default()
    };
    let mut by_repo: BTreeMap<String, Vec<&Session>> = BTreeMap::new();
    for s in sessions
        .iter()
        .filter(|s| s.status == SessionStatus::PrOpened && s.pr_url.is_some())
    {
        by_repo.entry(s.repo.to_ascii_lowercase()).or_default().push(s);
    }
    let mut engine = Engine {
        ops,
        cfg,
        sessions,
        guards: Guards::default(),
        dry,
        calls: 0,
    };
    let mut guards_read: Option<Result<(), String>> = None;
    for (repo, group) in by_repo {
        let mut out = RepoReport {
            repo: repo.clone(),
            ..RepoReport::default()
        };
        let skip_all = |out: &mut RepoReport, action: Action, why: &str| {
            for s in &group {
                out.items.push(Item {
                    session: s.id.clone(),
                    pr_url: s.pr_url.clone().unwrap_or_default(),
                    title: s.issue_title.clone(),
                    action,
                    reason: why.to_string(),
                });
            }
        };
        if report.stopped.is_some() {
            out.main = "not read".to_string();
            let why = format!("not reached: {}", report.stopped.clone().unwrap_or_default());
            skip_all(&mut out, Action::Waiting, &why);
            report.repos.push(out);
            continue;
        }
        match opt_in(cfg, &repo, denied_org(&repo)) {
            OptIn::In => {}
            OptIn::NotOptedIn => {
                out.main = "not read".to_string();
                skip_all(
                    &mut out,
                    Action::Skipped,
                    "this repository is not opted in to the merge-train loop",
                );
                report.repos.push(out);
                continue;
            }
            OptIn::Never(why) => {
                out.main = "not read".to_string();
                skip_all(&mut out, Action::Skipped, why);
                report.repos.push(out);
                continue;
            }
        }
        // The identity guards are read once, and only when something could merge.
        if guards_read.is_none() {
            let read = match engine.pace().await {
                Err(Stop(why)) => {
                    report.stopped = Some(why);
                    Err("not read".to_string())
                }
                Ok(()) => match engine.vet(ops.guards().await) {
                    Err(Stop(why)) => {
                        report.stopped = Some(why);
                        Err("not read".to_string())
                    }
                    Ok(Ok(mut g)) => {
                        g.forbidden.extend(AI_ATTRIBUTION.iter().map(|s| s.to_string()));
                        engine.guards = g;
                        Ok(())
                    }
                    Ok(Err(e)) => Err(e),
                },
            };
            guards_read = Some(read);
        }
        if let Some(Err(e)) = &guards_read {
            out.main = "not read".to_string();
            skip_all(
                &mut out,
                Action::Waiting,
                &format!("the mothership's identity could not be read ({e})"),
            );
            report.repos.push(out);
            continue;
        }
        let mem = memory.entry(repo.clone()).or_default();
        if let Err(Stop(why)) = engine.repo(&repo, &group, mem, &mut out).await {
            report.stopped = Some(why);
        }
        report.repos.push(out);
    }
    report.api_calls = engine.calls;
    report.finished_at = Some(ops.now());
    report.summary = summary(&report);
    report.lines = lines(&report);
    report
}

// ---------------------------------------------------------------------------------------------
// The real GitHub, through `gh` and the host.
// ---------------------------------------------------------------------------------------------

struct GhOps<'a> {
    app: &'a Shared,
}

impl GhOps<'_> {
    async fn get_json(&self, path: &str) -> Result<Value, String> {
        match github::gh_get(self.app, path, None).await {
            Ok((status, body)) if status >= 400 => Err(format!("HTTP {status}: {}", truncate(body.trim(), 300))),
            Ok((_, body)) if body.trim().is_empty() => Ok(Value::Null),
            Ok((_, body)) => serde_json::from_str(&body).map_err(|e| format!("{e}")),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    async fn gh(&self, args: Vec<String>) -> Result<String, String> {
        crate::util::exec_within(GH_LIMIT, &mut self.app.gh(args))
            .await
            .map_err(|e| format!("{e:#}"))
    }
}

impl Ops for GhOps<'_> {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    async fn sleep(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }

    async fn guards(&self) -> Result<Guards, String> {
        let t = merge_train::train_settings(self.app).await;
        let configured: Vec<String> = parse_list(&t.authors).iter().map(|a| a.to_ascii_lowercase()).collect();
        let allowed_authors = if configured.is_empty() {
            merge_train::default_authors(self.app)
                .await
                .ok_or_else(|| "gh could not say who the mothership publishes as".to_string())?
        } else {
            configured
        };
        Ok(Guards {
            allowed_authors,
            forbidden: parse_list(&t.forbid),
        })
    }

    async fn default_branch(&self, repo: &str) -> Result<String, String> {
        github::default_branch(self.app, repo).await.map_err(|e| format!("{e:#}"))
    }

    async fn main_ci(&self, repo: &str, branch: &str) -> Result<MainCi, String> {
        let tip = self.get_json(&format!("repos/{repo}/commits/{branch}")).await?;
        let sha = tip["sha"].as_str().ok_or("the branch tip had no sha")?.to_string();
        let runs = self
            .get_json(&format!("repos/{repo}/actions/runs?head_sha={sha}&per_page=50"))
            .await?;
        Ok(main_ci_from(&sha, &runs))
    }

    async fn read_pr(&self, s: &Session, base: &str) -> Result<Reading, String> {
        let facts = merge_train::read_pr(self.app, s, base).await?;
        let failing = if facts.info.ci == CiState::Failure {
            let url = s.pr_url.clone().unwrap_or_default();
            let out = self
                .gh(vec![
                    "pr".into(),
                    "view".into(),
                    url,
                    "--json".into(),
                    "statusCheckRollup".into(),
                ])
                .await?;
            let v: Value = serde_json::from_str(&out).map_err(|e| format!("{e}"))?;
            failing_checks_from(&v["statusCheckRollup"])
        } else {
            Vec::new()
        };
        Ok(Reading { facts, failing })
    }

    async fn update_branch(&self, s: &Session, head: &str) -> Result<(), String> {
        if authority::external_writes_blocked() {
            return Err(crate::publish::BLOCKED.to_string());
        }
        let n = pr_number(s.pr_url.as_deref().unwrap_or_default()).ok_or("no pull request number")?;
        self.gh(vec![
            "api".into(),
            "-X".into(),
            "PUT".into(),
            format!("repos/{}/pulls/{n}/update-branch", s.repo),
            "-f".into(),
            format!("expected_head_sha={head}"),
        ])
        .await
        .map(|_| ())
    }

    async fn rebase(&self, s: &Session) -> RebaseResult {
        if authority::external_writes_blocked() {
            return RebaseResult::Failed(crate::publish::BLOCKED.to_string());
        }
        let lock = self.app.repo_lock(&s.repo).await;
        let _worktree = lock.lock().await;
        match crate::publish::attempt_auto_rebase(self.app, &s.id, Mergeability::Conflicted, None).await {
            crate::publish::RebaseOutcome::Rebased(sha) | crate::publish::RebaseOutcome::Woke(sha) => RebaseResult::Rebased(sha),
            crate::publish::RebaseOutcome::Conflicted => RebaseResult::Conflicted,
            crate::publish::RebaseOutcome::Failed => RebaseResult::Failed("the host rebase failed; see the colony's log".into()),
            crate::publish::RebaseOutcome::Gone => RebaseResult::Failed("the colony's worktree is gone".into()),
            crate::publish::RebaseOutcome::Skipped => RebaseResult::Failed("already tried onto this base".into()),
        }
    }

    async fn merge(&self, s: &Session, head: &str, subject: &str, delete_branch: bool) -> Result<Option<String>, String> {
        if authority::external_writes_blocked() {
            return Err(crate::publish::BLOCKED.to_string());
        }
        let url = s.pr_url.clone().unwrap_or_default();
        let mut args = merge_train::merge_args(&url, head, delete_branch);
        args.extend(["--subject".to_string(), subject.to_string()]);
        self.gh(args).await?;
        let sha = self
            .gh(vec![
                "pr".into(),
                "view".into(),
                url.clone(),
                "--json".into(),
                "mergeCommit".into(),
                "--jq".into(),
                ".mergeCommit.oid".into(),
            ])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.app
            .session_log(&s.id, "info", format!("the merge-train loop squash-merged {url}"))
            .await;
        let mut entry = Entry::new("publish.merge_train", "colony").colony(s);
        entry.detail = Some("squash-merged by the merge-train loop".to_string());
        crate::activity::record(self.app, entry).await;
        Ok(sha)
    }

    async fn rerun(&self, repo: &str, run_id: u64) -> Result<(), String> {
        if authority::external_writes_blocked() {
            return Err(crate::publish::BLOCKED.to_string());
        }
        self.gh(vec![
            "run".into(),
            "rerun".into(),
            run_id.to_string(),
            "--failed".into(),
            "-R".into(),
            repo.to_string(),
        ])
        .await
        .map(|_| ())
    }

    async fn failure_log(&self, repo: &str, run_id: u64) -> Result<String, String> {
        let log = self
            .gh(vec![
                "run".into(),
                "view".into(),
                run_id.to_string(),
                "--log-failed".into(),
                "-R".into(),
                repo.to_string(),
            ])
            .await?;
        let tail: String = log
            .chars()
            .rev()
            .take(LOG_TAIL)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(tail)
    }

    async fn dispatch(&self, d: Dispatch) -> Result<String, String> {
        if authority::external_writes_blocked() {
            return Err(crate::publish::BLOCKED.to_string());
        }
        let body = dispatch_body(&d);
        let req: NewSession = serde_json::from_value(body).map_err(|e| format!("{e}"))?;
        match sessions::create(State(self.app.clone()), None, Json(req)).await {
            Ok(Json(session)) => {
                if let Dispatch::Redo { session: old, .. } = &d {
                    self.app
                        .session_log(
                            &old.id,
                            "warn",
                            format!("merge-train loop: needs_redo; redo colony {} dispatched", session.id),
                        )
                        .await;
                }
                Ok(session.id)
            }
            Err(e) => Err(e.message().to_string()),
        }
    }
}

/// The launch body for a colony the loop sends: autopilot on, so it publishes its pull request.
fn dispatch_body(d: &Dispatch) -> Value {
    match d {
        Dispatch::Redo {
            session,
            pr_url,
            base,
            why,
        } => {
            let n = pr_number(pr_url).unwrap_or_default();
            json!({
                "repo": session.repo,
                "issue": session.issue,
                "title": format!("Redo #{n}: {}", session.issue_title),
                "instructions": format!(
                    "Pull request {pr_url} (#{n}) can no longer merge: {why}. Redo the same change on top of the current \
                     `{base}` in a fresh branch. Use the pull request as your reference — `git fetch origin pull/{n}/head:pr-{n}` \
                     and read `git diff origin/{base}...pr-{n}` — and re-apply its intent to today's code; do not merge or \
                     cherry-pick its branch blindly, and do not widen the change. The original task was:\n\n{}",
                    truncate(session.instructions.trim(), 8000)
                ),
                "autopilot": true,
                "allow_duplicate": true,
                "origin": format!("{ORIGIN_REDO}{}", session.id),
            })
        }
        Dispatch::Fix {
            repo,
            base,
            merged,
            detail,
            log,
        } => json!({
            "repo": repo,
            "title": format!("Fix {base}: CI red after {}", merged.title),
            "instructions": format!(
                "`{base}` went red right after the merge train merged {} (\"{}\", squash commit {}). CI: {detail}. \
                 Fix `{base}` minimally: the smallest change that makes the failing check pass again. Do not refactor, \
                 do not disable, skip or loosen tests, and do not revert unrelated work. If the failure is a genuine flake \
                 with nothing to fix, change nothing and say so in the pull request description.\n\n\
                 The failing job's log (tail):\n```\n{log}\n```",
                merged.pr_url,
                merged.title,
                merged.sha.as_deref().unwrap_or("unknown")
            ),
            "autopilot": true,
            "allow_duplicate": true,
            "origin": format!("{ORIGIN_FIX}{repo}"),
        }),
        Dispatch::Revert {
            repo,
            base,
            merged,
            detail,
        } => json!({
            "repo": repo,
            "title": format!("Revert \"{}\"", merged.title),
            "instructions": format!(
                "`{base}` went red right after the merge train merged {} (\"{}\"), and a re-run did not fix it ({detail}). \
                 Revert exactly that squash commit and nothing else: `git revert --no-edit {}`. Do not touch any other \
                 commit. Open the pull request with the revert only.",
                merged.pr_url,
                merged.title,
                merged.sha.as_deref().unwrap_or("unknown")
            ),
            "autopilot": true,
            "allow_duplicate": true,
            "origin": format!("{ORIGIN_FIX}{repo}"),
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// Running it: the scheduler, the kill switch, the history and the activity log.
// ---------------------------------------------------------------------------------------------

/// One real run at a time, per mothership.
static RUNNING: AtomicBool = AtomicBool::new(false);

struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// A dry run is asked for, or forced: the kill switch turns every run into one.
pub(crate) fn effective_dry(requested: bool, writes_blocked: bool) -> (bool, bool) {
    (requested || writes_blocked, !requested && writes_blocked)
}

/// Runs the loop once and records the report. `Err` when a real run is already going.
pub(crate) async fn execute(app: &Shared, dry_requested: bool) -> Result<Report, String> {
    let (dry, forced) = effective_dry(dry_requested, authority::external_writes_blocked());
    let _running = if dry {
        None
    } else {
        if RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("a merge-train loop run is already going".to_string());
        }
        Some(Running)
    };
    let dir = app.cfg.config_dir.clone();
    let state = load(&dir).await;
    let sessions = app.sessions.read().await.clone();
    let train = merge_train::train_settings(app).await;
    let mut memory = state.repos.clone();
    let ops = GhOps { app };
    let mut report = run_all(
        &ops,
        &state.settings,
        |repo| merge_train::effective_state(&train, repo) == TrainState::Denied,
        &sessions,
        &mut memory,
        dry,
    )
    .await;
    report.forced_dry_run = forced;
    report.lines = lines(&report);
    let saved = report.clone();
    if let Err(e) = update(&dir, move |s| {
        if !dry {
            // Merge rather than replace: only repositories this run read changed.
            for (repo, mem) in memory {
                s.repos.insert(repo, mem);
            }
        }
        s.history.push(saved);
        let over = s.history.len().saturating_sub(HISTORY);
        s.history.drain(..over);
    })
    .await
    {
        eprintln!("merge-train loop: could not save its report: {e:#}");
    }
    record(app, &report, &sessions).await;
    Ok(report)
}

/// The report into the activity log (one line per repository) and each colony's own log.
async fn record(app: &App, report: &Report, sessions: &[Session]) {
    let prefix = if report.dry_run {
        "merge-train loop (dry run)"
    } else {
        "merge-train loop"
    };
    for repo in &report.repos {
        // Repositories the loop does not drive stay out of the feed; they are in the report.
        if repo.main == "not read" && report.stopped.is_none() {
            continue;
        }
        let mut entry = Entry::new("publish.merge_train", "colony");
        entry.repo = Some(repo.repo.clone());
        entry.org = repo.repo.split('/').next().map(str::to_string);
        let one = Report {
            dry_run: report.dry_run,
            repos: vec![repo.clone()],
            stopped: report.stopped.clone(),
            ..Report::default()
        };
        entry.detail = Some(format!("{prefix}: {}", summary(&one)));
        crate::activity::record(app, entry).await;
        if report.dry_run {
            continue;
        }
        for item in &repo.items {
            if item.action == Action::Merged || !sessions.iter().any(|s| s.id == item.session) {
                continue;
            }
            let level = if matches!(item.action, Action::Red | Action::NeedsRedo) {
                "warn"
            } else {
                "info"
            };
            app.session_log(
                &item.session,
                level,
                format!("{prefix}, {}: {}", item.action.word(false), item.reason),
            )
            .await;
        }
    }
}

/// The scheduler: once a minute, a due loop starts its run in the background.
async fn tick(app: &Shared) {
    let dir = app.cfg.config_dir.clone();
    let state = load(&dir).await;
    if !state.settings.enabled || RUNNING.load(Ordering::SeqCst) {
        return;
    }
    let now = Utc::now();
    match state.next_run_at {
        Some(at) if at <= now => {}
        Some(_) => return,
        None => {
            let _ = update(&dir, |s| s.next_run_at = Some(next_run_after(&s.settings.cadence, now))).await;
            return;
        }
    }
    let _ = update(&dir, |s| s.next_run_at = Some(next_run_after(&s.settings.cadence, now))).await;
    let worker = app.clone();
    tokio::spawn(async move {
        let _ = execute(&worker, false).await;
    });
}

pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(60));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            every.tick().await;
            tick(&app).await;
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Routes.
// ---------------------------------------------------------------------------------------------

async fn view(app: &App) -> Value {
    let state = load(&app.cfg.config_dir).await;
    let mut history = state.history.clone();
    history.reverse();
    json!({
        "settings": state.settings,
        "next_run_at": state.next_run_at,
        "running": RUNNING.load(Ordering::SeqCst),
        "writes_blocked": authority::external_writes_blocked(),
        "repos": state.repos,
        "last_report": history.first(),
        "history": history,
    })
}

/// `GET /api/merge-train/loop`: the loop's settings, what it remembers per repository, and its
/// run history, newest first.
async fn get_loop(State(app): State<Shared>) -> Json<Value> {
    Json(view(&app).await)
}

/// `PUT /api/merge-train/loop`: replaces the settings. Switching it on books the next run one
/// cadence away; switching it off clears it.
async fn put_loop(State(app): State<Shared>, Json(body): Json<Settings>) -> ApiResult<Value> {
    let settings = normalize(body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let now = Utc::now();
    update(&app.cfg.config_dir, move |s| {
        let rebook = !s.settings.enabled || s.settings.cadence != settings.cadence || s.next_run_at.is_none();
        s.next_run_at = if !settings.enabled {
            None
        } else if rebook {
            Some(next_run_after(&settings.cadence, now))
        } else {
            s.next_run_at
        };
        s.settings = settings;
    })
    .await?;
    Ok(Json(view(&app).await))
}

#[derive(Deserialize)]
struct RunQuery {
    #[serde(default)]
    dry_run: bool,
}

/// `POST /api/merge-train/loop/run[?dry_run=true]`: a dry run answers its report; a real one
/// starts in the background (a 409 while one is going) — or, with external writes blocked, is a
/// dry run and answers its report.
async fn post_run(State(app): State<Shared>, Query(q): Query<RunQuery>) -> ApiResult<Value> {
    let (dry, _) = effective_dry(q.dry_run, authority::external_writes_blocked());
    if dry {
        let report = execute(&app, q.dry_run)
            .await
            .map_err(|e| client_error(StatusCode::CONFLICT, &e))?;
        return Ok(Json(json!({ "started": false, "report": report })));
    }
    if RUNNING.load(Ordering::SeqCst) {
        return Err(client_error(StatusCode::CONFLICT, "a merge-train loop run is already going"));
    }
    let worker = app.clone();
    tokio::spawn(async move {
        let _ = execute(&worker, false).await;
    });
    Ok(Json(json!({ "started": true })))
}

pub(crate) fn routes() -> axum::Router<Shared> {
    axum::Router::new()
        .route("/api/merge-train/loop", routing::get(get_loop).put(put_loop))
        .route("/api/merge-train/loop/run", routing::post(post_run))
}

#[cfg(test)]
mod tests;
